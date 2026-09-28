//! SoundCloud metadata adapter for the egui application.
//!
//! This module intentionally has no playback, SoundCloud token storage, or
//! SoundCloud-Desktop/sctui dependency. It sends metadata and device-pairing
//! commands to one Worker, maps documented JSON responses into app-neutral
//! models, and returns structured events to the UI thread. SoundCloud OAuth
//! tokens remain inside the Worker; this client holds only opaque Worker
//! session proof in process memory.

use std::collections::BTreeSet;
use std::io::Read;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use egui::Context;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::state::{PlaybackAvailability, TrackId};

const API_BASE: &str = "https://api.soundcloud.com";
const DEFAULT_APPROVED_ARTWORK_HOSTS: &[&str] = &["i1.sndcdn.com"];
const AAC_HLS_MEDIA_HOST: &str = "playback.media-streaming.soundcloud.cloud";
const MP3_HLS_MEDIA_HOST: &str = "cf-hls-media.sndcdn.com";

fn approved_stream_media_host(format: &str) -> Option<&'static str> {
    match format {
        "hls_aac_160" | "hls_aac_96" => Some(AAC_HLS_MEDIA_HOST),
        "hls_mp3_128" => Some(MP3_HLS_MEDIA_HOST),
        _ => None,
    }
}

fn valid_stream_media_url(format: &str, media_url: &str) -> bool {
    let Some(approved_media_host) = approved_stream_media_host(format) else {
        return false;
    };
    let Ok(parsed) = Url::parse(media_url) else {
        return false;
    };
    media_url.len() <= 8192
        && parsed.scheme() == "https"
        && parsed.host_str() == Some(approved_media_host)
        && parsed.port().is_none()
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.fragment().is_none()
        && parsed.path().ends_with(".m3u8")
}
const API_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_SEARCH_LIMIT: u8 = 20;
const MAX_SEARCH_LIMIT: u8 = 50;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PlaylistId(u64);

impl PlaylistId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProfileId(u64);

impl ProfileId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Opaque device proof returned once by the Worker when a QR pairing begins.
/// It is intentionally redacted from Debug output and is never persisted by
/// this Windows/TrimUI shared UI layer.
#[derive(Clone, Eq, PartialEq)]
pub struct QrLoginSession {
    pub pairing_id: String,
    pub pairing_url: String,
    pub device_secret: String,
    pub confirmation_code: String,
    pub expires_at_ms: u64,
}

impl std::fmt::Debug for QrLoginSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QrLoginSession")
            .field("pairing_id", &"<redacted>")
            .field("pairing_url", &"<redacted>")
            .field("device_secret", &"<redacted>")
            .field("confirmation_code", &"<redacted>")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

/// Opaque client-to-Worker session credentials. These are not SoundCloud
/// access/refresh tokens. They are redacted in diagnostics. Windows persists
/// them under DPAPI; StockOS uses the app's atomic session store on the SD card.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct UserSession {
    pub session_id: String,
    pub session_secret: String,
    pub expires_at_ms: u64,
}

impl std::fmt::Debug for UserSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UserSession")
            .field("session_id", &"<redacted>")
            .field("session_secret", &"<redacted>")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

/// Short-lived signed CDN descriptor returned by Brickwave's fixed stream
/// route. Debug intentionally redacts the bearer-like media URL.
#[derive(Clone, Eq, PartialEq)]
pub struct StreamDescriptor {
    pub request_id: u64,
    pub track_id: TrackId,
    pub format: String,
    media_url: String,
}

impl StreamDescriptor {
    pub(crate) fn new(
        request_id: u64,
        track_id: TrackId,
        format: String,
        media_url: String,
    ) -> Self {
        Self {
            request_id,
            track_id,
            format,
            media_url,
        }
    }

    pub fn into_media_url(self) -> String {
        self.media_url
    }
}

impl std::fmt::Debug for StreamDescriptor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StreamDescriptor")
            .field("request_id", &self.request_id)
            .field("track_id", &self.track_id)
            .field("format", &self.format)
            .field("media_url", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QrLoginPhase {
    WaitingForScan,
    WaitingForAuthorization,
}

/// Backend commands contain only identifiers and UI input. They have no egui,
/// playback, SoundCloud access token, or transport references.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackendCommand {
    SearchTracks {
        query: String,
        limit: u8,
        request_id: u64,
        cursor: Option<String>,
        append: bool,
    },
    GetTrack {
        track_id: TrackId,
    },
    GetPlaylist {
        playlist_id: PlaylistId,
    },
    /// Loads the ordered tracks for one playlist through the authenticated
    /// Worker session. The SoundCloud OAuth token never enters this process.
    GetPlaylistTracks {
        session: UserSession,
        playlist_id: PlaylistId,
        playlist_urn: String,
    },
    GetStreamDescriptor {
        session: UserSession,
        request_id: u64,
        track_id: TrackId,
        track_urn: String,
    },
    GetPublicProfile {
        profile_id: ProfileId,
    },
    StartQrLogin,
    PollQrLogin {
        login: QrLoginSession,
    },
    ConfirmPairing {
        login: QrLoginSession,
    },
    CancelQrLogin {
        login: QrLoginSession,
    },
    GetCurrentUser {
        session: UserSession,
    },
    GetUserLibrary {
        session: UserSession,
    },
    SetTrackLiked {
        session: UserSession,
        track_id: TrackId,
        track_urn: String,
        liked: bool,
    },
    AddTrackToPlaylist {
        session: UserSession,
        playlist_id: PlaylistId,
        playlist_urn: String,
        track_id: TrackId,
        track_urn: String,
    },
    RemoveTrackFromPlaylist {
        session: UserSession,
        playlist_id: PlaylistId,
        playlist_urn: String,
        track_id: TrackId,
        track_urn: String,
        track_index: usize,
    },
    CreatePlaylist {
        session: UserSession,
        title: String,
    },
    DeletePlaylist {
        session: UserSession,
        playlist_id: PlaylistId,
        playlist_urn: String,
    },
    Logout {
        session: UserSession,
    },
}

impl BackendCommand {
    pub fn search_tracks(query: impl Into<String>) -> Self {
        Self::search_tracks_with_request_id(query, 0)
    }

    /// A UI-owned generation identifies one visible search. The backend copies
    /// it into events so an old worker reply cannot replace newer results.
    pub fn search_tracks_with_request_id(query: impl Into<String>, request_id: u64) -> Self {
        Self::SearchTracks {
            query: query.into(),
            limit: DEFAULT_SEARCH_LIMIT,
            request_id,
            cursor: None,
            append: false,
        }
    }

    pub fn search_more_with_request_id(
        query: impl Into<String>,
        cursor: String,
        request_id: u64,
    ) -> Self {
        Self::SearchTracks {
            query: query.into(),
            limit: DEFAULT_SEARCH_LIMIT,
            request_id,
            cursor: Some(cursor),
            append: true,
        }
    }

    fn request_id(&self) -> Option<u64> {
        match self {
            Self::SearchTracks { request_id, .. } => Some(*request_id),
            Self::GetStreamDescriptor { request_id, .. } => Some(*request_id),
            Self::GetTrack { .. }
            | Self::GetPlaylist { .. }
            | Self::GetPlaylistTracks { .. }
            | Self::GetPublicProfile { .. }
            | Self::StartQrLogin
            | Self::PollQrLogin { .. }
            | Self::ConfirmPairing { .. }
            | Self::CancelQrLogin { .. }
            | Self::GetCurrentUser { .. }
            | Self::GetUserLibrary { .. }
            | Self::SetTrackLiked { .. }
            | Self::AddTrackToPlaylist { .. }
            | Self::RemoveTrackFromPlaylist { .. }
            | Self::CreatePlaylist { .. }
            | Self::DeletePlaylist { .. }
            | Self::Logout { .. } => None,
        }
    }

    fn is_device_auth_command(&self) -> bool {
        matches!(
            self,
            Self::StartQrLogin
                | Self::PollQrLogin { .. }
                | Self::ConfirmPairing { .. }
                | Self::CancelQrLogin { .. }
                | Self::GetCurrentUser { .. }
                | Self::GetUserLibrary { .. }
                | Self::SetTrackLiked { .. }
                | Self::AddTrackToPlaylist { .. }
                | Self::RemoveTrackFromPlaylist { .. }
                | Self::CreatePlaylist { .. }
                | Self::DeletePlaylist { .. }
                | Self::GetPlaylistTracks { .. }
                | Self::GetStreamDescriptor { .. }
                | Self::Logout { .. }
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackendOperation {
    SearchTracks,
    GetTrack,
    GetPlaylist,
    GetPlaylistTracks,
    GetStreamDescriptor,
    GetPublicProfile,
    StartQrLogin,
    PollQrLogin,
    ConfirmPairing,
    CancelQrLogin,
    GetCurrentUser,
    GetUserLibrary,
    SetTrackLiked,
    AddTrackToPlaylist,
    RemoveTrackFromPlaylist,
    CreatePlaylist,
    DeletePlaylist,
    Logout,
}

impl From<&BackendCommand> for BackendOperation {
    fn from(command: &BackendCommand) -> Self {
        match command {
            BackendCommand::SearchTracks { .. } => Self::SearchTracks,
            BackendCommand::GetTrack { .. } => Self::GetTrack,
            BackendCommand::GetPlaylist { .. } => Self::GetPlaylist,
            BackendCommand::GetPlaylistTracks { .. } => Self::GetPlaylistTracks,
            BackendCommand::GetStreamDescriptor { .. } => Self::GetStreamDescriptor,
            BackendCommand::GetPublicProfile { .. } => Self::GetPublicProfile,
            BackendCommand::StartQrLogin => Self::StartQrLogin,
            BackendCommand::PollQrLogin { .. } => Self::PollQrLogin,
            BackendCommand::ConfirmPairing { .. } => Self::ConfirmPairing,
            BackendCommand::CancelQrLogin { .. } => Self::CancelQrLogin,
            BackendCommand::GetCurrentUser { .. } => Self::GetCurrentUser,
            BackendCommand::GetUserLibrary { .. } => Self::GetUserLibrary,
            BackendCommand::SetTrackLiked { .. } => Self::SetTrackLiked,
            BackendCommand::AddTrackToPlaylist { .. } => Self::AddTrackToPlaylist,
            BackendCommand::RemoveTrackFromPlaylist { .. } => Self::RemoveTrackFromPlaylist,
            BackendCommand::CreatePlaylist { .. } => Self::CreatePlaylist,
            BackendCommand::DeletePlaylist { .. } => Self::DeletePlaylist,
            BackendCommand::Logout { .. } => Self::Logout,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackendFailure {
    CredentialsRequired,
    AuthorizationPending,
    TokenExpired,
    RefreshRequired,
    LoginFailed,
    LoggedOut,
    PairingExpired,
    PairingCancelled,
    Unauthorized,
    Forbidden,
    RateLimited,
    Timeout,
    Network,
    NotFound,
    InvalidResponse,
    Unsupported,
    PlaybackUnavailable,
    PlaylistUpdateUnsafe,
}

impl BackendFailure {
    pub const fn user_message(&self) -> &'static str {
        match self {
            Self::CredentialsRequired => "Chưa cấu hình thông tin kết nối SoundCloud",
            Self::AuthorizationPending => "Đang chờ SoundCloud cấp quyền",
            Self::TokenExpired => "Phiên SoundCloud đã hết hạn; cần làm mới",
            Self::RefreshRequired => "Cần làm mới phiên SoundCloud",
            Self::LoginFailed => "Đăng nhập SoundCloud thất bại",
            Self::LoggedOut => "Đã đăng xuất khỏi SoundCloud",
            Self::PairingExpired => "Mã ghép nối đã hết hạn; hãy tạo mã QR mới",
            Self::PairingCancelled => "Đã hủy ghép nối SoundCloud",
            Self::Unauthorized => "SoundCloud từ chối cấp quyền",
            Self::Forbidden => "SoundCloud không cho phép truy cập nội dung này",
            Self::RateLimited => "Đã đạt giới hạn yêu cầu SoundCloud; hãy thử lại sau",
            Self::Timeout => "Yêu cầu SoundCloud đã quá thời gian chờ",
            Self::Network => "Không thể kết nối mạng tới SoundCloud",
            Self::NotFound => "Không tìm thấy nội dung SoundCloud",
            Self::InvalidResponse => "SoundCloud trả về dữ liệu không hợp lệ",
            Self::Unsupported => "Brickwave chưa hỗ trợ nội dung SoundCloud này",
            Self::PlaybackUnavailable => "Không thể phát bài hát này trên Brickwave",
            Self::PlaylistUpdateUnsafe => {
                "Không thể xác minh đầy đủ danh sách phát nên Brickwave không thay đổi dữ liệu"
            }
        }
    }

    fn from_status(status: u16) -> Self {
        match status {
            401 => Self::Unauthorized,
            403 => Self::Forbidden,
            404 => Self::NotFound,
            429 => Self::RateLimited,
            408 | 504 => Self::Timeout,
            _ => Self::Network,
        }
    }
}

/// Authentication state is intentionally separate from egui and the metadata
/// worker. `Authorized` means an implementation has an opaque credential in a
/// protected store; no token value is carried in this state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthState {
    Unconfigured,
    AuthorizationPending,
    CreatingQr,
    WaitingForScan,
    WaitingForAuthorization,
    PairingConfirmation,
    /// A remote metadata service is reachable and owns the SoundCloud token.
    /// This deliberately does not mean the client holds an OAuth credential.
    ServiceReady,
    Authorized,
    TokenExpired,
    RefreshRequired,
    LoginFailed,
    LoggedOut,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SoundCloudTrack {
    pub id: TrackId,
    /// Exact URN returned by SoundCloud. It is retained for later official
    /// resource/stream requests and is never reconstructed from a Rust ID.
    pub urn: Option<String>,
    pub title: String,
    pub artist: String,
    pub duration_seconds: Option<u32>,
    pub artwork_url: Option<String>,
    /// Official SoundCloud waveform PNG. The UI still routes this URL through
    /// ArtworkManager so HTTPS, host, redirect and size checks remain shared.
    pub waveform_url: Option<String>,
    pub availability: PlaybackAvailability,
    pub genre: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SoundCloudPlaylist {
    pub id: PlaylistId,
    /// Exact URN returned by SoundCloud and required by the current playlist
    /// tracks endpoint.
    pub urn: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub artwork_url: Option<String>,
    pub track_count: u32,
    /// True only for playlists returned by `/me/playlists`. Search results and
    /// playlists liked by the user remain readable but cannot be modified.
    pub editable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SoundCloudProfile {
    pub id: ProfileId,
    pub username: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackendEvent {
    SearchResults {
        request_id: u64,
        query: String,
        tracks: Vec<SoundCloudTrack>,
        playlists: Vec<SoundCloudPlaylist>,
        next_cursor: Option<String>,
        append: bool,
    },
    TrackLoaded {
        track: SoundCloudTrack,
    },
    PlaylistLoaded {
        playlist: SoundCloudPlaylist,
        tracks: Vec<SoundCloudTrack>,
    },
    PlaylistTracksLoaded {
        playlist_id: PlaylistId,
        tracks: Vec<SoundCloudTrack>,
    },
    StreamDescriptorLoaded {
        descriptor: StreamDescriptor,
    },
    ProfileLoaded {
        profile: SoundCloudProfile,
    },
    QrLoginStarted {
        login: QrLoginSession,
    },
    QrLoginPending {
        phase: QrLoginPhase,
        expires_at_ms: u64,
        confirmation_code: String,
    },
    PairingConfirmationRequired {
        login: QrLoginSession,
    },
    QrLoginAuthorized {
        session: UserSession,
        profile: SoundCloudProfile,
    },
    QrLoginExpired,
    QrLoginError {
        failure: BackendFailure,
    },
    CurrentUserLoaded {
        profile: SoundCloudProfile,
    },
    UserLibraryLoaded {
        liked_tracks: Vec<SoundCloudTrack>,
        playlists: Vec<SoundCloudPlaylist>,
        home_tracks: Vec<SoundCloudTrack>,
        discover_tracks: Vec<SoundCloudTrack>,
    },
    TrackLikeUpdated {
        track_id: TrackId,
        liked: bool,
    },
    TrackAddedToPlaylist {
        playlist_id: PlaylistId,
        track_id: TrackId,
        track_count: u32,
    },
    TrackRemovedFromPlaylist {
        playlist_id: PlaylistId,
        track_id: TrackId,
        track_index: usize,
        track_count: u32,
    },
    PlaylistCreated {
        playlist: SoundCloudPlaylist,
    },
    PlaylistDeleted {
        playlist_id: PlaylistId,
    },
    LoggedOut,
    ArtworkAvailable {
        track_id: TrackId,
        url: String,
    },
    BackendError {
        operation: BackendOperation,
        request_id: Option<u64>,
        failure: BackendFailure,
    },
}

/// Supplies an opaque short-lived access token from a credential implementation
/// owned by the project. Implementations own OAuth, rotation and secure token
/// storage; this data client neither logs nor persists tokens.
pub trait AuthProvider: Send + Sync + 'static {
    fn state(&self) -> AuthState;
    /// `Some` is a token for direct SoundCloud transport. `None` means a
    /// trusted backend service supplies upstream auth and is never a fake token.
    fn access_token(&self) -> Result<Option<String>, BackendFailure>;
    fn refresh(&self) -> Result<(), BackendFailure>;
    fn logout(&self) -> Result<(), BackendFailure>;
}

#[derive(Default)]
pub struct NoCredentials;

impl AuthProvider for NoCredentials {
    fn state(&self) -> AuthState {
        AuthState::Unconfigured
    }

    fn access_token(&self) -> Result<Option<String>, BackendFailure> {
        Err(BackendFailure::CredentialsRequired)
    }

    fn refresh(&self) -> Result<(), BackendFailure> {
        Err(BackendFailure::CredentialsRequired)
    }

    fn logout(&self) -> Result<(), BackendFailure> {
        Ok(())
    }
}

/// Configuration for the **local-only** development token service from C3.
/// It is deliberately separate from the normal remote Worker configuration:
/// the distributed application must never require these values or carry a
/// SoundCloud confidential-client secret.
#[derive(Clone, Eq, PartialEq)]
pub struct LiveApiConfig {
    client_id: String,
    redirect_uri: Option<Url>,
    token_service_url: Url,
    local_service_key: String,
    approved_artwork_hosts: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LiveApiConfigError {
    Missing(&'static str),
    Invalid(&'static str),
}

impl LiveApiConfig {
    /// Loads deployment configuration only. It deliberately has no
    /// `client_secret` field. The optional redirect URI is retained for a
    /// future Authorization Code flow; Client Credentials does not use it.
    pub fn from_environment() -> Result<Self, LiveApiConfigError> {
        let client_id = std::env::var("SOUNDCLOUD_CLIENT_ID")
            .map_err(|_| LiveApiConfigError::Missing("SOUNDCLOUD_CLIENT_ID"))?;
        let redirect_uri = std::env::var("SOUNDCLOUD_REDIRECT_URI").ok();
        let token_service_url = std::env::var("SOUNDCLOUD_TOKEN_SERVICE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8787".to_owned());
        let local_service_key = std::env::var("SOUNDCLOUD_TOKEN_SERVICE_KEY")
            .map_err(|_| LiveApiConfigError::Missing("SOUNDCLOUD_TOKEN_SERVICE_KEY"))?;
        let approved_artwork_hosts = std::env::var("SOUNDCLOUD_ARTWORK_HOSTS").unwrap_or_default();
        Self::new(
            client_id,
            redirect_uri.as_deref(),
            token_service_url,
            local_service_key,
            approved_artwork_hosts.split(',').map(str::to_owned),
        )
    }

    pub fn new<I, S>(
        client_id: impl Into<String>,
        redirect_uri: Option<&str>,
        token_service_url: impl AsRef<str>,
        local_service_key: impl Into<String>,
        approved_artwork_hosts: I,
    ) -> Result<Self, LiveApiConfigError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let client_id = client_id.into();
        if client_id.trim().is_empty() {
            return Err(LiveApiConfigError::Invalid("client ID"));
        }
        let redirect_uri = redirect_uri
            .map(|uri| parse_https_url(uri, "redirect URI"))
            .transpose()?;
        let token_service_url = parse_token_service_url(token_service_url.as_ref())?;
        let local_service_key = local_service_key.into();
        if local_service_key.trim().is_empty() {
            return Err(LiveApiConfigError::Invalid("token service key"));
        }
        let approved_artwork_hosts = approved_artwork_hosts
            .into_iter()
            .map(|host| normalize_approved_artwork_host(host.as_ref()))
            .collect::<Result<BTreeSet<_>, _>>()?;
        Ok(Self {
            client_id,
            redirect_uri,
            token_service_url,
            local_service_key,
            approved_artwork_hosts,
        })
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }
    pub fn redirect_uri(&self) -> Option<&Url> {
        self.redirect_uri.as_ref()
    }
    pub fn token_service_url(&self) -> &Url {
        &self.token_service_url
    }
    pub fn local_service_key(&self) -> &str {
        &self.local_service_key
    }
    pub fn approved_artwork_hosts(&self) -> &BTreeSet<String> {
        &self.approved_artwork_hosts
    }
}

fn parse_token_service_url(raw: &str) -> Result<Url, LiveApiConfigError> {
    let url = Url::parse(raw).map_err(|_| LiveApiConfigError::Invalid("token service URL"))?;
    let secure_remote = url.scheme() == "https" && url.host_str().is_some();
    let local_loopback = url.scheme() == "http" && url.host_str() == Some("127.0.0.1");
    if (!secure_remote && !local_loopback) || !url.username().is_empty() || url.password().is_some()
    {
        return Err(LiveApiConfigError::Invalid("token service URL"));
    }
    Ok(url)
}

fn parse_https_url(raw: &str, field: &'static str) -> Result<Url, LiveApiConfigError> {
    let url = Url::parse(raw).map_err(|_| LiveApiConfigError::Invalid(field))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(LiveApiConfigError::Invalid(field));
    }
    Ok(url)
}

fn normalize_approved_artwork_host(raw: &str) -> Result<String, LiveApiConfigError> {
    let host = raw.trim().to_ascii_lowercase();
    if host.is_empty()
        || raw != raw.trim()
        || host.contains('*')
        || host.contains('/')
        || host.contains('\\')
        || host.contains('@')
        || host.contains(':')
    {
        return Err(LiveApiConfigError::Invalid("approved artwork host"));
    }
    let url = Url::parse(&format!("https://{host}/"))
        .map_err(|_| LiveApiConfigError::Invalid("approved artwork host"))?;
    if url.host_str() != Some(host.as_str()) || url.port().is_some() {
        return Err(LiveApiConfigError::Invalid("approved artwork host"));
    }
    Ok(host)
}

/// Configuration for the public Cloudflare metadata backend. It contains no
/// SoundCloud credential, token-service key, or client secret.
#[derive(Clone, Eq, PartialEq)]
pub struct RemoteWorkerConfig {
    backend_url: Url,
    approved_artwork_hosts: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemoteWorkerConfigError {
    Missing(&'static str),
    Invalid(&'static str),
}

impl RemoteWorkerConfig {
    pub fn from_environment() -> Result<Self, RemoteWorkerConfigError> {
        let backend_url = std::env::var("SOUNDCLOUD_BACKEND_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| option_env!("SOUNDCLOUD_BACKEND_URL").map(str::to_owned))
            .ok_or(RemoteWorkerConfigError::Missing("SOUNDCLOUD_BACKEND_URL"))?;
        let configured_hosts = std::env::var("SOUNDCLOUD_ARTWORK_HOSTS").ok();
        Self::new(
            backend_url,
            production_artwork_hosts(configured_hosts.as_deref()),
        )
    }

    pub fn new<I, S>(
        backend_url: impl AsRef<str>,
        approved_artwork_hosts: I,
    ) -> Result<Self, RemoteWorkerConfigError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut backend_url = Url::parse(backend_url.as_ref())
            .map_err(|_| RemoteWorkerConfigError::Invalid("SOUNDCLOUD_BACKEND_URL"))?;
        if backend_url.scheme() != "https"
            || backend_url.host_str().is_none()
            || !backend_url.username().is_empty()
            || backend_url.password().is_some()
        {
            return Err(RemoteWorkerConfigError::Invalid("SOUNDCLOUD_BACKEND_URL"));
        }
        backend_url.set_query(None);
        backend_url.set_fragment(None);
        let approved_artwork_hosts = approved_artwork_hosts
            .into_iter()
            .map(|host| normalize_approved_artwork_host(host.as_ref()))
            .collect::<Result<BTreeSet<_>, _>>()
            .map_err(|_| RemoteWorkerConfigError::Invalid("SOUNDCLOUD_ARTWORK_HOSTS"))?;
        Ok(Self {
            backend_url,
            approved_artwork_hosts,
        })
    }

    pub fn backend_url(&self) -> &Url {
        &self.backend_url
    }

    pub fn approved_artwork_hosts(&self) -> &BTreeSet<String> {
        &self.approved_artwork_hosts
    }
}

fn production_artwork_hosts(configured_hosts: Option<&str>) -> Vec<String> {
    let mut hosts: Vec<String> = DEFAULT_APPROVED_ARTWORK_HOSTS
        .iter()
        .map(|host| (*host).to_owned())
        .collect();
    if let Some(configured_hosts) = configured_hosts {
        hosts.extend(
            configured_hosts
                .split(',')
                .map(str::trim)
                .filter(|host| !host.is_empty())
                .map(str::to_owned),
        );
    }
    hosts
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

pub trait HttpTransport: Send + Sync + 'static {
    fn get_json(
        &self,
        url: &Url,
        access_token: Option<&str>,
    ) -> Result<HttpResponse, BackendFailure>;
}

/// Production transport. It never follows redirects, uses a bounded timeout,
/// and always sends the official `Authorization: OAuth` header.
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout(API_TIMEOUT)
                .redirects(0)
                .max_idle_connections(0)
                .build(),
        }
    }
}

impl HttpTransport for UreqTransport {
    fn get_json(
        &self,
        url: &Url,
        access_token: Option<&str>,
    ) -> Result<HttpResponse, BackendFailure> {
        let access_token = access_token.ok_or(BackendFailure::CredentialsRequired)?;
        let request = self
            .agent
            .get(url.as_str())
            .set("Accept", "application/json; charset=utf-8")
            .set("Authorization", &format!("OAuth {access_token}"));
        let response = match request.call() {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(ureq::Error::Transport(error)) => {
                return Err(
                    if error.to_string().to_ascii_lowercase().contains("timeout") {
                        BackendFailure::Timeout
                    } else {
                        BackendFailure::Network
                    },
                );
            }
        };
        let status = response.status();
        let mut body = Vec::new();
        response
            .into_reader()
            .take(2 * 1024 * 1024)
            .read_to_end(&mut body)
            .map_err(|_| BackendFailure::Network)?;
        Ok(HttpResponse { status, body })
    }
}

/// Worker transport accepts only the internal SoundCloud `/tracks` request
/// constructed by `SoundCloudApiClient` and converts it to the fixed remote
/// `/search` API. It never accepts arbitrary caller URLs and sends no token.
pub struct CloudflareWorkerTransport {
    endpoint: Url,
    agent: ureq::Agent,
}

impl CloudflareWorkerTransport {
    pub fn new(config: &RemoteWorkerConfig) -> Result<Self, BackendFailure> {
        let mut endpoint = config.backend_url().clone();
        let base = endpoint.path().trim_end_matches('/');
        endpoint.set_path(&format!("{base}/search"));
        endpoint.set_query(None);
        Ok(Self {
            endpoint,
            agent: ureq::AgentBuilder::new()
                .timeout(API_TIMEOUT)
                .redirects(0)
                .max_idle_connections(0)
                .build(),
        })
    }
}

impl HttpTransport for CloudflareWorkerTransport {
    fn get_json(
        &self,
        soundcloud_url: &Url,
        access_token: Option<&str>,
    ) -> Result<HttpResponse, BackendFailure> {
        if access_token.is_some()
            || soundcloud_url.host_str() != Some("api.soundcloud.com")
            || soundcloud_url.path() != "/tracks"
        {
            return Err(BackendFailure::Unsupported);
        }
        let query = soundcloud_url
            .query_pairs()
            .find(|(key, _)| key == "q")
            .map(|(_, value)| value.into_owned())
            .filter(|query| !query.trim().is_empty())
            .ok_or(BackendFailure::InvalidResponse)?;
        let cursor = soundcloud_url
            .query_pairs()
            .find(|(key, _)| key == "cursor")
            .map(|(_, value)| value.into_owned());
        if cursor
            .as_deref()
            .is_some_and(|value| validated_search_cursor(value).is_none())
        {
            return Err(BackendFailure::InvalidResponse);
        }
        let mut endpoint = self.endpoint.clone();
        endpoint.query_pairs_mut().append_pair("q", &query);
        if let Some(cursor) = cursor {
            endpoint.query_pairs_mut().append_pair("cursor", &cursor);
        }
        let response = match self
            .agent
            .get(endpoint.as_str())
            .set("Accept", "application/json; charset=utf-8")
            .call()
        {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(ureq::Error::Transport(error)) => {
                return Err(
                    if error.to_string().to_ascii_lowercase().contains("timeout") {
                        BackendFailure::Timeout
                    } else {
                        BackendFailure::Network
                    },
                );
            }
        };
        let status = response.status();
        let mut body = Vec::new();
        response
            .into_reader()
            .take(512 * 1024)
            .read_to_end(&mut body)
            .map_err(|_| BackendFailure::Network)?;
        Ok(HttpResponse { status, body })
    }
}

trait DeviceAuthTransport: Send + Sync + 'static {
    fn execute(&self, command: &BackendCommand) -> Result<Vec<BackendEvent>, BackendFailure>;
}

/// Strict client for the fixed Worker auth API. It accepts no arbitrary URL,
/// follows no redirects, and never sees a SoundCloud OAuth token. The two
/// opaque Worker session proofs are kept only in the in-memory UiState.
struct CloudflareDeviceAuthTransport {
    base_url: Url,
    agent: ureq::Agent,
}

impl CloudflareDeviceAuthTransport {
    fn new(config: &RemoteWorkerConfig) -> Self {
        Self {
            base_url: config.backend_url().clone(),
            agent: ureq::AgentBuilder::new()
                .timeout(API_TIMEOUT)
                .redirects(0)
                .max_idle_connections(0)
                .build(),
        }
    }

    fn endpoint(&self, path: &str) -> Result<Url, BackendFailure> {
        let mut endpoint = self.base_url.clone();
        let base = endpoint.path().trim_end_matches('/');
        endpoint.set_path(&format!("{base}{path}"));
        endpoint.set_query(None);
        Ok(endpoint)
    }

    fn playlist_tracks_endpoint(&self, playlist_urn: &str) -> Result<Url, BackendFailure> {
        if validated_resource_urn(playlist_urn, "playlists").is_none() {
            return Err(BackendFailure::Unsupported);
        }
        let mut endpoint = self.endpoint("/auth/playlists")?;
        endpoint
            .path_segments_mut()
            .map_err(|_| BackendFailure::Unsupported)?
            .push(playlist_urn)
            .push("tracks");
        Ok(endpoint)
    }

    fn stream_endpoint(&self, track_urn: &str) -> Result<Url, BackendFailure> {
        if validated_resource_urn(track_urn, "tracks").is_none() {
            return Err(BackendFailure::Unsupported);
        }
        let mut endpoint = self.endpoint("/auth/tracks")?;
        endpoint
            .path_segments_mut()
            .map_err(|_| BackendFailure::Unsupported)?
            .push(track_urn)
            .push("stream");
        Ok(endpoint)
    }

    fn track_like_endpoint(&self, track_urn: &str) -> Result<Url, BackendFailure> {
        if validated_resource_urn(track_urn, "tracks").is_none() {
            return Err(BackendFailure::Unsupported);
        }
        let mut endpoint = self.endpoint("/auth/tracks")?;
        endpoint
            .path_segments_mut()
            .map_err(|_| BackendFailure::Unsupported)?
            .push(track_urn)
            .push("like");
        Ok(endpoint)
    }

    fn playlist_add_endpoint(&self, playlist_urn: &str) -> Result<Url, BackendFailure> {
        if validated_resource_urn(playlist_urn, "playlists").is_none() {
            return Err(BackendFailure::Unsupported);
        }
        let mut endpoint = self.endpoint("/auth/playlists")?;
        endpoint
            .path_segments_mut()
            .map_err(|_| BackendFailure::Unsupported)?
            .push(playlist_urn)
            .push("tracks");
        Ok(endpoint)
    }

    fn playlist_delete_endpoint(&self, playlist_urn: &str) -> Result<Url, BackendFailure> {
        if validated_resource_urn(playlist_urn, "playlists").is_none() {
            return Err(BackendFailure::Unsupported);
        }
        let mut endpoint = self.endpoint("/auth/playlists")?;
        endpoint
            .path_segments_mut()
            .map_err(|_| BackendFailure::Unsupported)?
            .push(playlist_urn);
        Ok(endpoint)
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: Option<serde_json::Value>,
    ) -> Result<(u16, serde_json::Value), BackendFailure> {
        let endpoint = self.endpoint(path)?;
        self.request_url(method, endpoint, headers, body)
    }

    fn request_url(
        &self,
        method: &str,
        endpoint: Url,
        headers: &[(&str, String)],
        body: Option<serde_json::Value>,
    ) -> Result<(u16, serde_json::Value), BackendFailure> {
        let mut request = match method {
            "GET" => self.agent.get(endpoint.as_str()),
            "POST" => self.agent.post(endpoint.as_str()),
            "DELETE" => self.agent.delete(endpoint.as_str()),
            _ => return Err(BackendFailure::Unsupported),
        }
        .set("Accept", "application/json; charset=utf-8");
        for (name, value) in headers {
            request = request.set(name, value);
        }
        let response = match body {
            Some(body) => request.set("Content-Type", "application/json").send_string(
                &serde_json::to_string(&body).map_err(|_| BackendFailure::InvalidResponse)?,
            ),
            None => request.call(),
        };
        let response = match response {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(ureq::Error::Transport(error)) => {
                return Err(
                    if error.to_string().to_ascii_lowercase().contains("timeout") {
                        BackendFailure::Timeout
                    } else {
                        BackendFailure::Network
                    },
                );
            }
        };
        let status = response.status();
        let mut bytes = Vec::new();
        response
            .into_reader()
            // Playlist metadata is capped by the Worker at 512 KiB. Keep the
            // same hard bound here so a valid 100-track response is not
            // truncated while all auth responses remain bounded.
            .take(512 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|_| BackendFailure::Network)?;
        let value = serde_json::from_slice(&bytes).map_err(|_| BackendFailure::InvalidResponse)?;
        Ok((status, value))
    }

    fn failure(status: u16, value: &serde_json::Value) -> BackendFailure {
        let code = value
            .pointer("/error/code")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        match code {
            "pairing_expired" => BackendFailure::PairingExpired,
            "pairing_cancelled" => BackendFailure::PairingCancelled,
            "poll_too_soon" | "rate_limited" | "too_many_pending_pairings" => {
                BackendFailure::RateLimited
            }
            "stream_format_unavailable" | "stream_proxy_required" | "stream_not_found" => {
                BackendFailure::PlaybackUnavailable
            }
            "stream_url_rejected"
            | "stream_response_invalid"
            | "stream_metadata_redirect_rejected" => BackendFailure::InvalidResponse,
            "playlist_too_large" | "playlist_incomplete" => BackendFailure::PlaylistUpdateUnsafe,
            _ => match status {
                401 => BackendFailure::Unauthorized,
                403 => BackendFailure::Forbidden,
                404 => BackendFailure::NotFound,
                408 | 504 => BackendFailure::Timeout,
                410 => BackendFailure::PairingExpired,
                429 => BackendFailure::RateLimited,
                _ => BackendFailure::Network,
            },
        }
    }

    fn checked(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, BackendFailure> {
        let (status, value) = self.request(method, path, headers, body)?;
        if !(200..300).contains(&status) {
            return Err(Self::failure(status, &value));
        }
        Ok(value)
    }

    fn parse_profile(value: &serde_json::Value) -> Result<SoundCloudProfile, BackendFailure> {
        let value = value.get("profile").unwrap_or(value);
        let id = resource_id_from_value(value, "users").ok_or(BackendFailure::InvalidResponse)?;
        let username = value
            .get("username")
            .and_then(serde_json::Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .ok_or(BackendFailure::InvalidResponse)?;
        let display_name = value
            .get("displayName")
            .or_else(|| value.get("display_name"))
            .and_then(serde_json::Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .map(str::to_owned);
        let avatar_url = value
            .get("avatarUrl")
            .or_else(|| value.get("avatar_url"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .and_then(normalize_artwork_url);
        Ok(SoundCloudProfile {
            id: ProfileId::new(id),
            username: username.to_owned(),
            display_name,
            avatar_url,
        })
    }

    fn string(value: &serde_json::Value, name: &str) -> Result<String, BackendFailure> {
        value
            .get(name)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty() && value.len() <= 4096)
            .map(str::to_owned)
            .ok_or(BackendFailure::InvalidResponse)
    }

    fn timestamp(value: &serde_json::Value) -> Result<u64, BackendFailure> {
        value
            .get("expires_at_ms")
            .and_then(serde_json::Value::as_u64)
            .filter(|value| *value > 0)
            .ok_or(BackendFailure::InvalidResponse)
    }

    fn pairing_url(
        &self,
        value: &serde_json::Value,
        pairing_id: &str,
    ) -> Result<String, BackendFailure> {
        let raw = Self::string(value, "pairing_url")?;
        let url = Url::parse(&raw).map_err(|_| BackendFailure::InvalidResponse)?;
        let expected = self.endpoint("/auth/connect")?;
        let mut query = url.query_pairs();
        let matching_pair = matches!(
            (query.next(), query.next()),
            (Some((key, value)), None) if key == "pairing" && value == pairing_id
        );
        if url.scheme() != "https"
            || url.scheme() != expected.scheme()
            || url.host_str() != expected.host_str()
            || url.port_or_known_default() != expected.port_or_known_default()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != expected.path()
            || url.fragment().is_some()
            || !matching_pair
        {
            return Err(BackendFailure::InvalidResponse);
        }
        Ok(url.to_string())
    }
}

impl DeviceAuthTransport for CloudflareDeviceAuthTransport {
    fn execute(&self, command: &BackendCommand) -> Result<Vec<BackendEvent>, BackendFailure> {
        match command {
            BackendCommand::StartQrLogin => {
                let value = self.checked(
                    "POST",
                    "/auth/device/start",
                    &[],
                    Some(serde_json::json!({})),
                )?;
                let pairing_id = Self::string(&value, "pairing_id")?;
                let login = QrLoginSession {
                    pairing_url: self.pairing_url(&value, &pairing_id)?,
                    pairing_id,
                    device_secret: Self::string(&value, "device_secret")?,
                    confirmation_code: Self::string(&value, "confirmation_code")?,
                    expires_at_ms: Self::timestamp(&value)?,
                };
                Ok(vec![BackendEvent::QrLoginStarted { login }])
            }
            BackendCommand::PollQrLogin { login } => {
                let mut endpoint = self.endpoint("/auth/device/status")?;
                endpoint
                    .query_pairs_mut()
                    .append_pair("pairing_id", &login.pairing_id);
                let (status, value) = self.request_url(
                    "GET",
                    endpoint,
                    &[("Authorization", format!("Bearer {}", login.device_secret))],
                    None,
                )?;
                // `request` accepts a fixed path. The query is added here after
                // validating the generated pairing ID, never from free UI URL input.
                let value = if status >= 200 && status < 300 {
                    value
                } else {
                    return Err(Self::failure(status, &value));
                };
                let phase = Self::string(&value, "phase")?;
                let expires_at_ms = Self::timestamp(&value)?;
                let confirmation_code = Self::string(&value, "confirmation_code")?;
                match phase.as_str() {
                    "waiting_for_scan" => Ok(vec![BackendEvent::QrLoginPending {
                        phase: QrLoginPhase::WaitingForScan,
                        expires_at_ms,
                        confirmation_code,
                    }]),
                    "waiting_for_authorization" => Ok(vec![BackendEvent::QrLoginPending {
                        phase: QrLoginPhase::WaitingForAuthorization,
                        expires_at_ms,
                        confirmation_code,
                    }]),
                    "authorization_complete" => {
                        Ok(vec![BackendEvent::PairingConfirmationRequired {
                            login: login.clone(),
                        }])
                    }
                    "authorized" => Err(BackendFailure::Unauthorized),
                    "cancelled" => Ok(vec![BackendEvent::QrLoginError {
                        failure: BackendFailure::PairingCancelled,
                    }]),
                    _ => Err(BackendFailure::InvalidResponse),
                }
            }
            BackendCommand::ConfirmPairing { login } => {
                let value = self.checked(
                    "POST",
                    "/auth/device/complete",
                    &[("Authorization", format!("Bearer {}", login.device_secret))],
                    Some(serde_json::json!({
                        "pairing_id": login.pairing_id,
                        "confirmation_code": login.confirmation_code,
                    })),
                )?;
                let session = UserSession {
                    session_id: Self::string(&value, "session_id")?,
                    session_secret: Self::string(&value, "session_secret")?,
                    expires_at_ms: Self::timestamp(&value)?,
                };
                Ok(vec![BackendEvent::QrLoginAuthorized {
                    session,
                    profile: Self::parse_profile(&value)?,
                }])
            }
            BackendCommand::CancelQrLogin { login } => {
                self.checked(
                    "POST",
                    "/auth/device/cancel",
                    &[("Authorization", format!("Bearer {}", login.device_secret))],
                    Some(serde_json::json!({ "pairing_id": login.pairing_id })),
                )?;
                Ok(vec![BackendEvent::QrLoginError {
                    failure: BackendFailure::PairingCancelled,
                }])
            }
            BackendCommand::GetCurrentUser { session } => {
                let value = self.checked(
                    "GET",
                    "/auth/me",
                    &[
                        (
                            "Authorization",
                            format!("Bearer {}", session.session_secret),
                        ),
                        ("X-Brickwave-Session", session.session_id.clone()),
                    ],
                    None,
                )?;
                Ok(vec![BackendEvent::CurrentUserLoaded {
                    profile: Self::parse_profile(&value)?,
                }])
            }
            BackendCommand::GetUserLibrary { session } => {
                let value = self.checked(
                    "GET",
                    "/auth/library",
                    &[
                        (
                            "Authorization",
                            format!("Bearer {}", session.session_secret),
                        ),
                        ("X-Brickwave-Session", session.session_id.clone()),
                    ],
                    None,
                )?;
                let liked_tracks = parse_track_collection(
                    value
                        .get("liked_tracks")
                        .ok_or(BackendFailure::InvalidResponse)?,
                )?;
                let playlists = parse_playlist_collection(
                    value
                        .get("playlists")
                        .ok_or(BackendFailure::InvalidResponse)?,
                )?;
                let home_tracks = parse_track_collection(
                    value
                        .get("home_tracks")
                        .ok_or(BackendFailure::InvalidResponse)?,
                )?;
                let discover_tracks = parse_track_collection(
                    value
                        .get("discover_tracks")
                        .ok_or(BackendFailure::InvalidResponse)?,
                )?;
                let mut events = vec![BackendEvent::UserLibraryLoaded {
                    liked_tracks: liked_tracks.clone(),
                    playlists,
                    home_tracks: home_tracks.clone(),
                    discover_tracks: discover_tracks.clone(),
                }];
                events.extend(artwork_events(&liked_tracks));
                events.extend(artwork_events(&home_tracks));
                events.extend(artwork_events(&discover_tracks));
                Ok(events)
            }
            BackendCommand::SetTrackLiked {
                session,
                track_id,
                track_urn,
                liked,
            } => {
                let endpoint = self.track_like_endpoint(track_urn)?;
                let (status, value) = self.request_url(
                    if *liked { "POST" } else { "DELETE" },
                    endpoint,
                    &[
                        (
                            "Authorization",
                            format!("Bearer {}", session.session_secret),
                        ),
                        ("X-Brickwave-Session", session.session_id.clone()),
                    ],
                    None,
                )?;
                if !(200..300).contains(&status) {
                    return Err(Self::failure(status, &value));
                }
                Ok(vec![BackendEvent::TrackLikeUpdated {
                    track_id: *track_id,
                    liked: *liked,
                }])
            }
            BackendCommand::AddTrackToPlaylist {
                session,
                playlist_id,
                playlist_urn,
                track_id,
                track_urn,
            } => {
                let endpoint = self.playlist_add_endpoint(playlist_urn)?;
                let (status, value) = self.request_url(
                    "POST",
                    endpoint,
                    &[
                        (
                            "Authorization",
                            format!("Bearer {}", session.session_secret),
                        ),
                        ("X-Brickwave-Session", session.session_id.clone()),
                    ],
                    Some(serde_json::json!({ "track_urn": track_urn })),
                )?;
                if !(200..300).contains(&status) {
                    return Err(Self::failure(status, &value));
                }
                let track_count = value
                    .get("track_count")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or(BackendFailure::InvalidResponse)?;
                Ok(vec![BackendEvent::TrackAddedToPlaylist {
                    playlist_id: *playlist_id,
                    track_id: *track_id,
                    track_count,
                }])
            }
            BackendCommand::RemoveTrackFromPlaylist {
                session,
                playlist_id,
                playlist_urn,
                track_id,
                track_urn,
                track_index,
            } => {
                let endpoint = self.playlist_add_endpoint(playlist_urn)?;
                let (status, value) = self.request_url(
                    "DELETE",
                    endpoint,
                    &[
                        (
                            "Authorization",
                            format!("Bearer {}", session.session_secret),
                        ),
                        ("X-Brickwave-Session", session.session_id.clone()),
                    ],
                    Some(serde_json::json!({
                        "track_urn": track_urn,
                        "track_index": track_index,
                    })),
                )?;
                if !(200..300).contains(&status) {
                    return Err(Self::failure(status, &value));
                }
                let track_count = value
                    .get("track_count")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or(BackendFailure::InvalidResponse)?;
                Ok(vec![BackendEvent::TrackRemovedFromPlaylist {
                    playlist_id: *playlist_id,
                    track_id: *track_id,
                    track_index: *track_index,
                    track_count,
                }])
            }
            BackendCommand::CreatePlaylist { session, title } => {
                let value = self.checked(
                    "POST",
                    "/auth/playlists",
                    &[
                        (
                            "Authorization",
                            format!("Bearer {}", session.session_secret),
                        ),
                        ("X-Brickwave-Session", session.session_id.clone()),
                    ],
                    Some(serde_json::json!({ "title": title })),
                )?;
                let playlist_value = value.get("playlist").unwrap_or(&value);
                let (mut playlist, _) = parse_playlist(playlist_value)?;
                // This endpoint is user-scoped and the Worker only returns the
                // playlist it has just created for the authenticated account.
                playlist.editable = true;
                Ok(vec![BackendEvent::PlaylistCreated { playlist }])
            }
            BackendCommand::DeletePlaylist {
                session,
                playlist_id,
                playlist_urn,
            } => {
                let endpoint = self.playlist_delete_endpoint(playlist_urn)?;
                let (status, value) = self.request_url(
                    "DELETE",
                    endpoint,
                    &[
                        (
                            "Authorization",
                            format!("Bearer {}", session.session_secret),
                        ),
                        ("X-Brickwave-Session", session.session_id.clone()),
                    ],
                    None,
                )?;
                if !(200..300).contains(&status) {
                    return Err(Self::failure(status, &value));
                }
                Ok(vec![BackendEvent::PlaylistDeleted {
                    playlist_id: *playlist_id,
                }])
            }
            BackendCommand::GetPlaylistTracks {
                session,
                playlist_id,
                playlist_urn,
            } => {
                let endpoint = self.playlist_tracks_endpoint(playlist_urn)?;
                let (status, value) = self.request_url(
                    "GET",
                    endpoint,
                    &[
                        (
                            "Authorization",
                            format!("Bearer {}", session.session_secret),
                        ),
                        ("X-Brickwave-Session", session.session_id.clone()),
                    ],
                    None,
                )?;
                if !(200..300).contains(&status) {
                    return Err(Self::failure(status, &value));
                }
                if value
                    .get("playlist_urn")
                    .and_then(serde_json::Value::as_str)
                    != Some(playlist_urn.as_str())
                {
                    return Err(BackendFailure::InvalidResponse);
                }
                let tracks = parse_track_collection(
                    value.get("tracks").ok_or(BackendFailure::InvalidResponse)?,
                )?;
                let mut events = vec![BackendEvent::PlaylistTracksLoaded {
                    playlist_id: *playlist_id,
                    tracks: tracks.clone(),
                }];
                events.extend(artwork_events(&tracks));
                Ok(events)
            }
            BackendCommand::GetStreamDescriptor {
                session,
                request_id,
                track_id,
                track_urn,
            } => {
                let endpoint = self.stream_endpoint(track_urn)?;
                let (status, value) = self.request_url(
                    "GET",
                    endpoint,
                    &[
                        (
                            "Authorization",
                            format!("Bearer {}", session.session_secret),
                        ),
                        ("X-Brickwave-Session", session.session_id.clone()),
                    ],
                    None,
                )?;
                if !(200..300).contains(&status) {
                    return Err(Self::failure(status, &value));
                }
                if value.get("track_urn").and_then(serde_json::Value::as_str)
                    != Some(track_urn.as_str())
                {
                    return Err(BackendFailure::InvalidResponse);
                }
                let format = Self::string(&value, "format")?;
                let media_url = Self::string(&value, "media_url")?;
                if !valid_stream_media_url(&format, &media_url) {
                    return Err(BackendFailure::InvalidResponse);
                }
                Ok(vec![BackendEvent::StreamDescriptorLoaded {
                    descriptor: StreamDescriptor::new(*request_id, *track_id, format, media_url),
                }])
            }
            BackendCommand::Logout { session } => {
                self.checked(
                    "POST",
                    "/auth/logout",
                    &[
                        (
                            "Authorization",
                            format!("Bearer {}", session.session_secret),
                        ),
                        ("X-Brickwave-Session", session.session_id.clone()),
                    ],
                    Some(serde_json::json!({})),
                )?;
                Ok(vec![BackendEvent::LoggedOut])
            }
            BackendCommand::SearchTracks { .. }
            | BackendCommand::GetTrack { .. }
            | BackendCommand::GetPlaylist { .. }
            | BackendCommand::GetPublicProfile { .. } => Err(BackendFailure::Unsupported),
        }
    }
}

#[derive(Default)]
pub struct CloudflareWorkerAuth;

impl AuthProvider for CloudflareWorkerAuth {
    fn state(&self) -> AuthState {
        AuthState::ServiceReady
    }

    fn access_token(&self) -> Result<Option<String>, BackendFailure> {
        // The Worker owns upstream credentials. No token is synthesized or
        // retained in this Windows/Brick client.
        Ok(None)
    }

    fn refresh(&self) -> Result<(), BackendFailure> {
        Ok(())
    }

    fn logout(&self) -> Result<(), BackendFailure> {
        Ok(())
    }
}

#[derive(Clone)]
struct LocalAccessToken {
    value: String,
    expires_at_unix: u64,
}

#[derive(Deserialize)]
struct LocalTokenLeaseWire {
    access_token: String,
    expires_at_unix: u64,
}

/// Auth adapter for the loopback-only local token service. It has no knowledge
/// of the SoundCloud client secret and keeps its short-lived lease in memory.
pub struct LocalTokenServiceAuth {
    endpoint: Url,
    local_service_key: String,
    agent: ureq::Agent,
    cache: Mutex<Option<LocalAccessToken>>,
    state: Mutex<AuthState>,
}

impl LocalTokenServiceAuth {
    pub fn new(config: &LiveApiConfig) -> Result<Self, BackendFailure> {
        if config.token_service_url().scheme() != "http"
            || config.token_service_url().host_str() != Some("127.0.0.1")
        {
            return Err(BackendFailure::Unsupported);
        }
        let mut endpoint = config.token_service_url().clone();
        endpoint.set_path("/v1/token");
        endpoint.set_query(None);
        Ok(Self {
            endpoint,
            local_service_key: config.local_service_key().to_owned(),
            agent: ureq::AgentBuilder::new()
                .timeout(API_TIMEOUT)
                .redirects(0)
                .max_idle_connections(0)
                .build(),
            cache: Mutex::new(None),
            state: Mutex::new(AuthState::AuthorizationPending),
        })
    }

    fn lease_is_valid(token: &LocalAccessToken) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(u64::MAX);
        token.expires_at_unix.saturating_sub(now) > 30
    }

    fn request_lease(&self) -> Result<LocalAccessToken, BackendFailure> {
        let response = match self
            .agent
            .get(self.endpoint.as_str())
            .set("Accept", "application/json; charset=utf-8")
            .set("X-SoundCloud-Local-Key", &self.local_service_key)
            .call()
        {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => {
                return Err(match response.status() {
                    401 => BackendFailure::Unauthorized,
                    403 => BackendFailure::Forbidden,
                    429 => BackendFailure::RateLimited,
                    503 => BackendFailure::CredentialsRequired,
                    504 => BackendFailure::Timeout,
                    _ => BackendFailure::Network,
                });
            }
            Err(ureq::Error::Transport(error)) => {
                return Err(
                    if error.to_string().to_ascii_lowercase().contains("timeout") {
                        BackendFailure::Timeout
                    } else {
                        BackendFailure::Network
                    },
                );
            }
        };
        let mut body = Vec::new();
        response
            .into_reader()
            .take(64 * 1024)
            .read_to_end(&mut body)
            .map_err(|_| BackendFailure::Network)?;
        let lease: LocalTokenLeaseWire =
            serde_json::from_slice(&body).map_err(|_| BackendFailure::InvalidResponse)?;
        if lease.access_token.trim().is_empty() || lease.expires_at_unix == 0 {
            return Err(BackendFailure::InvalidResponse);
        }
        Ok(LocalAccessToken {
            value: lease.access_token,
            expires_at_unix: lease.expires_at_unix,
        })
    }
}

impl AuthProvider for LocalTokenServiceAuth {
    fn state(&self) -> AuthState {
        self.state
            .lock()
            .map(|state| *state)
            .unwrap_or(AuthState::LoginFailed)
    }

    fn access_token(&self) -> Result<Option<String>, BackendFailure> {
        if self.state() == AuthState::LoggedOut {
            return Err(BackendFailure::LoggedOut);
        }
        let mut cache = self.cache.lock().map_err(|_| BackendFailure::LoginFailed)?;
        if let Some(token) = cache.as_ref().filter(|token| Self::lease_is_valid(token)) {
            *self.state.lock().map_err(|_| BackendFailure::LoginFailed)? = AuthState::Authorized;
            return Ok(Some(token.value.clone()));
        }
        *self.state.lock().map_err(|_| BackendFailure::LoginFailed)? = AuthState::RefreshRequired;
        match self.request_lease() {
            Ok(token) => {
                let value = token.value.clone();
                *cache = Some(token);
                *self.state.lock().map_err(|_| BackendFailure::LoginFailed)? =
                    AuthState::Authorized;
                Ok(Some(value))
            }
            Err(error) => {
                *self.state.lock().map_err(|_| BackendFailure::LoginFailed)? = match error {
                    BackendFailure::Unauthorized => AuthState::TokenExpired,
                    BackendFailure::CredentialsRequired => AuthState::Unconfigured,
                    _ => AuthState::LoginFailed,
                };
                Err(error)
            }
        }
    }

    fn refresh(&self) -> Result<(), BackendFailure> {
        *self.cache.lock().map_err(|_| BackendFailure::LoginFailed)? = None;
        self.access_token().map(|_| ())
    }

    fn logout(&self) -> Result<(), BackendFailure> {
        *self.cache.lock().map_err(|_| BackendFailure::LoginFailed)? = None;
        *self.state.lock().map_err(|_| BackendFailure::LoginFailed)? = AuthState::LoggedOut;
        Ok(())
    }
}

enum WorkerCommand {
    Execute(BackendCommand),
    Shutdown,
}

/// One metadata worker. All HTTP and JSON parsing remain outside the egui
/// frame. The worker requests a repaint after publishing an event.
pub struct SoundCloudBackend {
    command_tx: Sender<WorkerCommand>,
    event_rx: Receiver<BackendEvent>,
    worker: Option<JoinHandle<()>>,
}

impl SoundCloudBackend {
    pub fn unconfigured(ui_ctx: &Context) -> Self {
        Self::start(
            ui_ctx,
            Arc::new(NoCredentials),
            Arc::new(UreqTransport::default()),
            None,
        )
    }

    /// Explicit local-development backend. Normal `LIVE` mode uses `remote`.
    pub fn local(ui_ctx: &Context, config: &LiveApiConfig) -> Result<Self, BackendFailure> {
        Ok(Self::start(
            ui_ctx,
            Arc::new(LocalTokenServiceAuth::new(config)?),
            Arc::new(UreqTransport::default()),
            None,
        ))
    }

    /// Normal LIVE mode. The Cloudflare Worker owns the upstream confidential
    /// client and token lease; this client sends only a bounded search query.
    pub fn remote(ui_ctx: &Context, config: &RemoteWorkerConfig) -> Result<Self, BackendFailure> {
        Ok(Self::start(
            ui_ctx,
            Arc::new(CloudflareWorkerAuth),
            Arc::new(CloudflareWorkerTransport::new(config)?),
            Some(Arc::new(CloudflareDeviceAuthTransport::new(config))),
        ))
    }

    fn start(
        ui_ctx: &Context,
        auth: Arc<dyn AuthProvider>,
        transport: Arc<dyn HttpTransport>,
        device_auth: Option<Arc<dyn DeviceAuthTransport>>,
    ) -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let repaint_ctx = ui_ctx.clone();
        let worker = thread::Builder::new()
            .name("soundcloud-data-worker".to_owned())
            .spawn(move || {
                let client = SoundCloudApiClient { auth, transport };
                while let Ok(command) = command_rx.recv() {
                    match command {
                        WorkerCommand::Execute(command) => {
                            let events = if command.is_device_auth_command() {
                                let result = device_auth
                                    .as_ref()
                                    .map(|transport| transport.execute(&command))
                                    .unwrap_or_else(|| Err(BackendFailure::Unsupported));
                                match result {
                                    Ok(events) => events,
                                    Err(BackendFailure::PairingExpired) => {
                                        vec![BackendEvent::QrLoginExpired]
                                    }
                                    Err(failure) => {
                                        vec![BackendEvent::BackendError {
                                            operation: BackendOperation::from(&command),
                                            request_id: command.request_id(),
                                            failure,
                                        }]
                                    }
                                }
                            } else {
                                client.execute(command)
                            };
                            for event in events {
                                if event_tx.send(event).is_err() {
                                    return;
                                }
                                repaint_ctx.request_repaint();
                            }
                        }
                        WorkerCommand::Shutdown => return,
                    }
                }
            })
            .expect("failed to start SoundCloud data worker");
        Self {
            command_tx,
            event_rx,
            worker: Some(worker),
        }
    }

    pub fn send(&self, command: BackendCommand) -> Result<(), BackendFailure> {
        self.command_tx
            .send(WorkerCommand::Execute(command))
            .map_err(|_| BackendFailure::Network)
    }

    pub fn take_events(&self) -> Vec<BackendEvent> {
        let mut events = Vec::new();
        loop {
            match self.event_rx.try_recv() {
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return events,
            }
        }
    }

    pub fn shutdown(&mut self) {
        let _ = self.command_tx.send(WorkerCommand::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for SoundCloudBackend {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct SoundCloudApiClient {
    auth: Arc<dyn AuthProvider>,
    transport: Arc<dyn HttpTransport>,
}

impl SoundCloudApiClient {
    fn execute(&self, command: BackendCommand) -> Vec<BackendEvent> {
        let operation = BackendOperation::from(&command);
        let request_id = command.request_id();
        let result = self.execute_inner(&command);
        match result {
            Ok(events) => events,
            Err(failure) => vec![BackendEvent::BackendError {
                operation,
                request_id,
                failure,
            }],
        }
    }

    fn execute_inner(&self, command: &BackendCommand) -> Result<Vec<BackendEvent>, BackendFailure> {
        if command.is_device_auth_command() {
            return Err(BackendFailure::Unsupported);
        }
        let access_token = self.auth.access_token()?;
        match command {
            BackendCommand::SearchTracks {
                query,
                limit,
                request_id,
                cursor,
                append,
            } => {
                let query = query.trim();
                if query.is_empty() {
                    return Ok(vec![BackendEvent::SearchResults {
                        request_id: *request_id,
                        query: String::new(),
                        tracks: Vec::new(),
                        playlists: Vec::new(),
                        next_cursor: None,
                        append: *append,
                    }]);
                }
                let mut url = api_url("/tracks")?;
                url.query_pairs_mut()
                    .append_pair("q", query)
                    .append_pair("access", "playable,preview,blocked")
                    .append_pair("linked_partitioning", "true")
                    .append_pair("limit", &(*limit).clamp(1, MAX_SEARCH_LIMIT).to_string());
                if let Some(cursor) = cursor {
                    let cursor =
                        validated_search_cursor(cursor).ok_or(BackendFailure::InvalidResponse)?;
                    url.query_pairs_mut().append_pair("cursor", cursor);
                }
                let value = self.get_value(&url, access_token.as_deref())?;
                let tracks = parse_track_collection(&value)?;
                let playlists = value
                    .get("playlists")
                    .map(parse_playlist_collection)
                    .transpose()?
                    .unwrap_or_default();
                let next_cursor = value
                    .get("next_cursor")
                    .and_then(serde_json::Value::as_str)
                    .map(|cursor| {
                        validated_search_cursor(cursor)
                            .map(str::to_owned)
                            .ok_or(BackendFailure::InvalidResponse)
                    })
                    .transpose()?;
                let mut events = vec![BackendEvent::SearchResults {
                    request_id: *request_id,
                    query: query.to_owned(),
                    tracks: tracks.clone(),
                    playlists: playlists.clone(),
                    next_cursor,
                    append: *append,
                }];
                events.extend(artwork_events(&tracks));
                Ok(events)
            }
            BackendCommand::GetTrack { track_id } => {
                let url = api_url(&format!("/tracks/{}", track_id.get()))?;
                let track = parse_track(&self.get_value(&url, access_token.as_deref())?)?;
                let mut events = vec![BackendEvent::TrackLoaded {
                    track: track.clone(),
                }];
                events.extend(artwork_events(std::slice::from_ref(&track)));
                Ok(events)
            }
            BackendCommand::GetPlaylist { playlist_id } => {
                let mut url = api_url(&format!("/playlists/{}", playlist_id.get()))?;
                url.query_pairs_mut().append_pair("show_tracks", "true");
                let value = self.get_value(&url, access_token.as_deref())?;
                let (playlist, tracks) = parse_playlist(&value)?;
                let mut events = vec![BackendEvent::PlaylistLoaded {
                    playlist,
                    tracks: tracks.clone(),
                }];
                events.extend(artwork_events(&tracks));
                Ok(events)
            }
            BackendCommand::GetPublicProfile { profile_id } => {
                let url = api_url(&format!("/users/{}", profile_id.get()))?;
                let profile = parse_profile(&self.get_value(&url, access_token.as_deref())?)?;
                Ok(vec![BackendEvent::ProfileLoaded { profile }])
            }
            BackendCommand::StartQrLogin
            | BackendCommand::PollQrLogin { .. }
            | BackendCommand::ConfirmPairing { .. }
            | BackendCommand::CancelQrLogin { .. }
            | BackendCommand::GetCurrentUser { .. }
            | BackendCommand::GetUserLibrary { .. }
            | BackendCommand::SetTrackLiked { .. }
            | BackendCommand::AddTrackToPlaylist { .. }
            | BackendCommand::RemoveTrackFromPlaylist { .. }
            | BackendCommand::CreatePlaylist { .. }
            | BackendCommand::DeletePlaylist { .. }
            | BackendCommand::GetPlaylistTracks { .. }
            | BackendCommand::GetStreamDescriptor { .. }
            | BackendCommand::Logout { .. } => Err(BackendFailure::Unsupported),
        }
    }

    fn get_value(
        &self,
        url: &Url,
        access_token: Option<&str>,
    ) -> Result<serde_json::Value, BackendFailure> {
        let response = self.transport.get_json(url, access_token)?;
        if !(200..300).contains(&response.status) {
            return Err(BackendFailure::from_status(response.status));
        }
        serde_json::from_slice(&response.body).map_err(|_| BackendFailure::InvalidResponse)
    }
}

fn api_url(path: &str) -> Result<Url, BackendFailure> {
    Url::parse(&format!("{API_BASE}{path}")).map_err(|_| BackendFailure::Unsupported)
}

fn artwork_events(tracks: &[SoundCloudTrack]) -> impl Iterator<Item = BackendEvent> + '_ {
    tracks.iter().filter_map(|track| {
        track
            .artwork_url
            .as_ref()
            .map(|url| BackendEvent::ArtworkAvailable {
                track_id: track.id,
                url: url.clone(),
            })
    })
}

#[derive(Deserialize)]
struct TrackWire {
    id: Option<u64>,
    urn: Option<String>,
    title: String,
    duration: Option<u64>,
    metadata_artist: Option<String>,
    user: Option<UserWire>,
    artwork_url: Option<String>,
    waveform_url: Option<String>,
    access: Option<String>,
    genre: Option<String>,
}

#[derive(Deserialize)]
struct PlaylistWire {
    id: Option<u64>,
    urn: Option<String>,
    title: String,
    description: Option<String>,
    artwork_url: Option<String>,
    track_count: Option<u32>,
    #[serde(default)]
    editable: bool,
    tracks: Option<Vec<TrackWire>>,
}

#[derive(Deserialize)]
struct UserWire {
    id: Option<u64>,
    urn: Option<String>,
    username: String,
    full_name: Option<String>,
    avatar_url: Option<String>,
}

fn parse_track_collection(
    value: &serde_json::Value,
) -> Result<Vec<SoundCloudTrack>, BackendFailure> {
    let items = value
        .get("collection")
        .and_then(serde_json::Value::as_array)
        .or_else(|| value.as_array())
        .ok_or(BackendFailure::InvalidResponse)?;
    items.iter().map(parse_track).collect()
}

fn parse_playlist_collection(
    value: &serde_json::Value,
) -> Result<Vec<SoundCloudPlaylist>, BackendFailure> {
    let items = value
        .get("collection")
        .and_then(serde_json::Value::as_array)
        .or_else(|| value.as_array())
        .ok_or(BackendFailure::InvalidResponse)?;
    items
        .iter()
        .map(|item| parse_playlist(item).map(|(playlist, _)| playlist))
        .collect()
}

fn parse_track(value: &serde_json::Value) -> Result<SoundCloudTrack, BackendFailure> {
    let wire: TrackWire =
        serde_json::from_value(value.clone()).map_err(|_| BackendFailure::InvalidResponse)?;
    map_track_wire(wire)
}

fn map_track_wire(wire: TrackWire) -> Result<SoundCloudTrack, BackendFailure> {
    let urn = wire
        .urn
        .as_deref()
        .and_then(|value| validated_resource_urn(value, "tracks"))
        .map(str::to_owned);
    let id = resource_id(wire.id, wire.urn.as_deref(), "tracks")
        .ok_or(BackendFailure::InvalidResponse)?;
    if wire.title.trim().is_empty() {
        return Err(BackendFailure::InvalidResponse);
    }
    let artist = wire
        .metadata_artist
        .filter(|artist| !artist.trim().is_empty())
        .or_else(|| wire.user.map(|user| user.username))
        .unwrap_or_else(|| "Không rõ nghệ sĩ".to_owned());
    let artwork_url = wire.artwork_url.and_then(normalize_artwork_url);
    let waveform_url = wire.waveform_url.and_then(normalize_waveform_url);
    let availability = match wire.access.as_deref() {
        None | Some("playable") => PlaybackAvailability::Available,
        Some(_) => PlaybackAvailability::Unavailable,
    };
    Ok(SoundCloudTrack {
        id: TrackId::new(id),
        urn,
        title: wire.title,
        artist,
        duration_seconds: wire
            .duration
            .map(|milliseconds| (milliseconds / 1000) as u32),
        artwork_url,
        waveform_url,
        availability,
        genre: wire.genre.unwrap_or_else(|| "SoundCloud".to_owned()),
    })
}

fn parse_playlist(
    value: &serde_json::Value,
) -> Result<(SoundCloudPlaylist, Vec<SoundCloudTrack>), BackendFailure> {
    let wire: PlaylistWire =
        serde_json::from_value(value.clone()).map_err(|_| BackendFailure::InvalidResponse)?;
    let urn = wire
        .urn
        .as_deref()
        .and_then(|value| validated_resource_urn(value, "playlists"))
        .map(str::to_owned);
    let id = resource_id(wire.id, wire.urn.as_deref(), "playlists")
        .ok_or(BackendFailure::InvalidResponse)?;
    if wire.title.trim().is_empty() {
        return Err(BackendFailure::InvalidResponse);
    }
    let tracks: Result<Vec<_>, _> = wire
        .tracks
        .unwrap_or_default()
        .into_iter()
        .map(map_track_wire)
        .collect();
    let tracks = tracks?;
    let playlist = SoundCloudPlaylist {
        id: PlaylistId::new(id),
        urn,
        title: wire.title,
        description: wire.description.filter(|text| !text.trim().is_empty()),
        artwork_url: wire.artwork_url.and_then(normalize_artwork_url),
        track_count: wire.track_count.unwrap_or(tracks.len() as u32),
        editable: wire.editable,
    };
    Ok((playlist, tracks))
}

fn parse_profile(value: &serde_json::Value) -> Result<SoundCloudProfile, BackendFailure> {
    let wire: UserWire =
        serde_json::from_value(value.clone()).map_err(|_| BackendFailure::InvalidResponse)?;
    let id = resource_id(wire.id, wire.urn.as_deref(), "users")
        .ok_or(BackendFailure::InvalidResponse)?;
    if wire.username.trim().is_empty() {
        return Err(BackendFailure::InvalidResponse);
    }
    Ok(SoundCloudProfile {
        id: ProfileId::new(id),
        username: wire.username,
        display_name: wire.full_name.filter(|name| !name.trim().is_empty()),
        avatar_url: wire.avatar_url.and_then(normalize_artwork_url),
    })
}

fn resource_id_from_value(value: &serde_json::Value, kind: &str) -> Option<u64> {
    resource_id(
        value.get("id").and_then(serde_json::Value::as_u64),
        value.get("urn").and_then(serde_json::Value::as_str),
        kind,
    )
}

fn resource_id(legacy_id: Option<u64>, urn: Option<&str>, kind: &str) -> Option<u64> {
    let prefix = format!("soundcloud:{kind}:");
    urn.and_then(|value| value.strip_prefix(&prefix))
        .filter(|suffix| !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|suffix| suffix.parse::<u64>().ok())
        .filter(|id| *id > 0)
        .or_else(|| legacy_id.filter(|id| *id > 0))
}

fn validated_resource_urn<'a>(urn: &'a str, kind: &str) -> Option<&'a str> {
    let prefix = format!("soundcloud:{kind}:");
    let suffix = urn.strip_prefix(&prefix)?;
    (!suffix.is_empty()
        && suffix.bytes().all(|byte| byte.is_ascii_digit())
        && suffix.as_bytes()[0] != b'0')
        .then_some(urn)
}

fn validated_search_cursor(cursor: &str) -> Option<&str> {
    (!cursor.is_empty()
        && cursor.len() <= 4096
        && cursor
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
    .then_some(cursor)
}

fn normalize_artwork_url(raw_url: String) -> Option<String> {
    let url = Url::parse(&raw_url).ok()?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    Some(url.into())
}

fn normalize_waveform_url(raw_url: String) -> Option<String> {
    let parsed = url::Url::parse(&raw_url).ok()?;
    (parsed.scheme() == "https"
        && parsed.host_str() == Some("wave.sndcdn.com")
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.port().is_none())
    .then_some(raw_url)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;
    use std::time::Instant;

    use super::*;

    #[test]
    fn stream_formats_are_bound_to_their_exact_media_hosts() {
        let aac = "https://playback.media-streaming.soundcloud.cloud/a/playlist.m3u8?Policy=signed";
        let mp3 = "https://cf-hls-media.sndcdn.com/playlist/a.128.mp3/playlist.m3u8?Policy=signed";

        assert!(valid_stream_media_url("hls_aac_160", aac));
        assert!(valid_stream_media_url("hls_aac_96", aac));
        assert!(valid_stream_media_url("hls_mp3_128", mp3));
        assert!(!valid_stream_media_url("hls_mp3_128", aac));
        assert!(!valid_stream_media_url("hls_aac_160", mp3));
        assert!(!valid_stream_media_url("preview_mp3_128", mp3));
        assert!(!valid_stream_media_url(
            "hls_mp3_128",
            "https://evil.example/playlist.m3u8"
        ));
    }

    #[derive(Clone)]
    struct FixedAuth;

    impl AuthProvider for FixedAuth {
        fn state(&self) -> AuthState {
            AuthState::Authorized
        }

        fn access_token(&self) -> Result<Option<String>, BackendFailure> {
            Ok(Some("fixture-token".to_owned()))
        }

        fn refresh(&self) -> Result<(), BackendFailure> {
            Ok(())
        }

        fn logout(&self) -> Result<(), BackendFailure> {
            Ok(())
        }
    }

    struct RotatingFixtureAuth {
        state: Mutex<AuthState>,
        generation: Mutex<u8>,
    }

    impl RotatingFixtureAuth {
        fn new(state: AuthState) -> Self {
            Self {
                state: Mutex::new(state),
                generation: Mutex::new(0),
            }
        }

        fn set_state(&self, state: AuthState) {
            *self.state.lock().unwrap() = state;
        }
    }

    impl AuthProvider for RotatingFixtureAuth {
        fn state(&self) -> AuthState {
            *self.state.lock().unwrap()
        }

        fn access_token(&self) -> Result<Option<String>, BackendFailure> {
            match self.state() {
                AuthState::Authorized => Ok(Some(format!(
                    "test-only-opaque-credential-generation-{}",
                    *self.generation.lock().unwrap()
                ))),
                AuthState::ServiceReady => Ok(None),
                AuthState::AuthorizationPending => Err(BackendFailure::AuthorizationPending),
                AuthState::TokenExpired => Err(BackendFailure::TokenExpired),
                AuthState::RefreshRequired => Err(BackendFailure::RefreshRequired),
                AuthState::LoginFailed => Err(BackendFailure::LoginFailed),
                AuthState::LoggedOut => Err(BackendFailure::LoggedOut),
                AuthState::CreatingQr
                | AuthState::WaitingForScan
                | AuthState::WaitingForAuthorization
                | AuthState::PairingConfirmation => Err(BackendFailure::AuthorizationPending),
                AuthState::Cancelled => Err(BackendFailure::PairingCancelled),
                AuthState::Unconfigured => Err(BackendFailure::CredentialsRequired),
            }
        }

        fn refresh(&self) -> Result<(), BackendFailure> {
            match self.state() {
                AuthState::TokenExpired | AuthState::RefreshRequired => {
                    *self.generation.lock().unwrap() += 1;
                    self.set_state(AuthState::Authorized);
                    Ok(())
                }
                AuthState::AuthorizationPending => Err(BackendFailure::AuthorizationPending),
                AuthState::LoginFailed => Err(BackendFailure::LoginFailed),
                AuthState::LoggedOut => Err(BackendFailure::LoggedOut),
                AuthState::CreatingQr
                | AuthState::WaitingForScan
                | AuthState::WaitingForAuthorization
                | AuthState::PairingConfirmation => Err(BackendFailure::AuthorizationPending),
                AuthState::Cancelled => Err(BackendFailure::PairingCancelled),
                AuthState::Unconfigured => Err(BackendFailure::CredentialsRequired),
                AuthState::Authorized | AuthState::ServiceReady => Ok(()),
            }
        }

        fn logout(&self) -> Result<(), BackendFailure> {
            self.set_state(AuthState::LoggedOut);
            Ok(())
        }
    }

    struct FixtureTransport {
        responses: Mutex<VecDeque<Result<HttpResponse, BackendFailure>>>,
    }

    impl FixtureTransport {
        fn new(responses: Vec<Result<HttpResponse, BackendFailure>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
            }
        }
    }

    impl HttpTransport for FixtureTransport {
        fn get_json(&self, _: &Url, _: Option<&str>) -> Result<HttpResponse, BackendFailure> {
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(BackendFailure::Network))
        }
    }

    fn response(json: &str) -> Result<HttpResponse, BackendFailure> {
        Ok(HttpResponse {
            status: 200,
            body: json.as_bytes().to_vec(),
        })
    }

    fn fixture_client(json: &str) -> SoundCloudApiClient {
        SoundCloudApiClient {
            auth: Arc::new(FixedAuth),
            transport: Arc::new(FixtureTransport::new(vec![response(json)])),
        }
    }

    const TRACK_JSON: &str = r#"{
        "id": 987654, "title": "Real Track", "duration": 222000,
        "metadata_artist": "Real Artist", "artwork_url": "https://i1.sndcdn.com/artworks-example-large.jpg",
        "waveform_url": "https://wave.sndcdn.com/example.png",
        "access": "playable", "genre": "Electronic"
    }"#;

    #[test]
    fn search_fixture_maps_stable_track_ids_and_artwork() {
        let client = fixture_client(&format!(r#"{{"collection":[{TRACK_JSON}]}}"#));
        let events = client.execute(BackendCommand::search_tracks("real"));
        assert!(matches!(
            &events[0],
            BackendEvent::SearchResults { query, tracks, .. }
                if query == "real" && tracks[0].id == TrackId::new(987654)
                    && tracks[0].artwork_url.as_deref() == Some("https://i1.sndcdn.com/artworks-example-large.jpg")
                    && tracks[0].waveform_url.as_deref() == Some("https://wave.sndcdn.com/example.png")
        ));
        assert!(matches!(events[1], BackendEvent::ArtworkAvailable { .. }));
    }

    #[test]
    fn user_library_payload_maps_likes_and_playlist_summaries() {
        let value = serde_json::json!({
            "liked_tracks": [{
                "id": 8001,
                "title": "Account like",
                "duration": 181000,
                "user": { "id": 12, "username": "Artist" },
                "access": "playable"
            }],
            "playlists": [{
                "id": 9001,
                "title": "Account playlist",
                "description": "Saved online",
                "artwork_url": "https://i1.sndcdn.com/list.jpg",
                "track_count": 7
            }]
        });
        let tracks = parse_track_collection(value.get("liked_tracks").unwrap()).unwrap();
        let playlists = parse_playlist_collection(value.get("playlists").unwrap()).unwrap();
        assert_eq!(tracks[0].id, TrackId::new(8001));
        assert_eq!(playlists[0].id, PlaylistId::new(9001));
        assert_eq!(playlists[0].track_count, 7);
    }

    #[test]
    fn urn_only_resources_map_without_the_deprecated_numeric_id_field() {
        let track = parse_track(&serde_json::json!({
            "urn": "soundcloud:tracks:88001",
            "title": "URN track",
            "user": { "urn": "soundcloud:users:77", "username": "Artist" },
            "access": "playable"
        }))
        .unwrap();
        let (playlist, _) = parse_playlist(&serde_json::json!({
            "urn": "soundcloud:playlists:99001",
            "title": "URN playlist"
        }))
        .unwrap();
        let profile = parse_profile(&serde_json::json!({
            "urn": "soundcloud:users:77001",
            "username": "urn-user"
        }))
        .unwrap();
        assert_eq!(track.id, TrackId::new(88_001));
        assert_eq!(track.urn.as_deref(), Some("soundcloud:tracks:88001"));
        assert_eq!(playlist.id, PlaylistId::new(99_001));
        assert_eq!(playlist.urn.as_deref(), Some("soundcloud:playlists:99001"));
        assert_eq!(profile.id, ProfileId::new(77_001));
    }

    #[test]
    fn track_without_artwork_maps_to_placeholder_path() {
        let client = fixture_client(
            r#"{"id": 4, "title": "No cover", "duration": 0, "user":{"id":7,"username":"Uploader"}, "access":"preview"}"#,
        );
        let events = client.execute(BackendCommand::GetTrack {
            track_id: TrackId::new(4),
        });
        assert!(matches!(
            events.as_slice(),
            [BackendEvent::TrackLoaded { track }]
                if track.id == TrackId::new(4)
                    && track.artwork_url.is_none()
                    && track.availability == PlaybackAvailability::Unavailable
        ));
    }

    #[test]
    fn playlist_fixture_maps_tracks_without_a_second_ui_model() {
        let client = fixture_client(&format!(
            r#"{{"id":123,"title":"Real Playlist","track_count":1,"tracks":[{TRACK_JSON}]}}"#
        ));
        let events = client.execute(BackendCommand::GetPlaylist {
            playlist_id: PlaylistId::new(123),
        });
        assert!(matches!(
            &events[0],
            BackendEvent::PlaylistLoaded { playlist, tracks }
                if playlist.id == PlaylistId::new(123) && tracks[0].id == TrackId::new(987654)
        ));
    }

    #[test]
    fn public_profile_fixture_maps_a_stable_profile_id() {
        let client = fixture_client(
            r#"{"id":42,"username":"Public Artist","avatar_url":"https://i1.sndcdn.com/avatar.jpg"}"#,
        );
        assert!(matches!(
            client
                .execute(BackendCommand::GetPublicProfile {
                    profile_id: ProfileId::new(42),
                })
                .as_slice(),
            [BackendEvent::ProfileLoaded { profile }]
                if profile.id == ProfileId::new(42)
                    && profile.avatar_url.as_deref() == Some("https://i1.sndcdn.com/avatar.jpg")
        ));
    }

    #[test]
    fn status_failures_and_invalid_json_are_structured() {
        for (status, expected) in [
            (401, BackendFailure::Unauthorized),
            (403, BackendFailure::Forbidden),
            (429, BackendFailure::RateLimited),
        ] {
            let client = SoundCloudApiClient {
                auth: Arc::new(FixedAuth),
                transport: Arc::new(FixtureTransport::new(vec![Ok(HttpResponse {
                    status,
                    body: Vec::new(),
                })])),
            };
            assert!(matches!(
                client.execute(BackendCommand::search_tracks("x")).as_slice(),
                [BackendEvent::BackendError { failure, .. }] if *failure == expected
            ));
        }
        let client = fixture_client("not-json");
        assert!(matches!(
            client
                .execute(BackendCommand::search_tracks("x"))
                .as_slice(),
            [BackendEvent::BackendError {
                failure: BackendFailure::InvalidResponse,
                ..
            }]
        ));
    }

    #[test]
    fn timeout_and_unconfigured_auth_never_become_success() {
        let timeout_client = SoundCloudApiClient {
            auth: Arc::new(FixedAuth),
            transport: Arc::new(FixtureTransport::new(vec![Err(BackendFailure::Timeout)])),
        };
        assert!(matches!(
            timeout_client
                .execute(BackendCommand::search_tracks("x"))
                .as_slice(),
            [BackendEvent::BackendError {
                failure: BackendFailure::Timeout,
                ..
            }]
        ));
        let no_auth = SoundCloudApiClient {
            auth: Arc::new(NoCredentials),
            transport: Arc::new(FixtureTransport::new(Vec::new())),
        };
        assert!(matches!(
            no_auth
                .execute(BackendCommand::search_tracks("x"))
                .as_slice(),
            [BackendEvent::BackendError {
                failure: BackendFailure::CredentialsRequired,
                ..
            }]
        ));
    }

    #[test]
    fn backend_worker_shuts_down_cleanly() {
        let ctx = Context::default();
        let mut backend = SoundCloudBackend::start(
            &ctx,
            Arc::new(FixedAuth),
            Arc::new(FixtureTransport::new(Vec::new())),
            None,
        );
        backend.send(BackendCommand::search_tracks("x")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while backend.take_events().is_empty() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        backend.shutdown();
    }

    #[test]
    fn auth_contract_handles_pending_expiry_refresh_rotation_failure_and_logout() {
        let auth = RotatingFixtureAuth::new(AuthState::AuthorizationPending);
        assert_eq!(auth.state(), AuthState::AuthorizationPending);
        assert_eq!(
            auth.access_token(),
            Err(BackendFailure::AuthorizationPending)
        );

        auth.set_state(AuthState::Authorized);
        let before_refresh = auth.access_token().unwrap();
        auth.set_state(AuthState::TokenExpired);
        assert_eq!(auth.access_token(), Err(BackendFailure::TokenExpired));
        auth.set_state(AuthState::RefreshRequired);
        auth.refresh().unwrap();
        assert_eq!(auth.state(), AuthState::Authorized);
        assert_ne!(before_refresh, auth.access_token().unwrap());

        auth.set_state(AuthState::LoginFailed);
        assert_eq!(auth.access_token(), Err(BackendFailure::LoginFailed));
        auth.logout().unwrap();
        assert_eq!(auth.state(), AuthState::LoggedOut);
        assert_eq!(auth.access_token(), Err(BackendFailure::LoggedOut));
    }

    #[test]
    fn live_api_configuration_contains_no_client_secret_and_validates_exact_hosts() {
        let config = LiveApiConfig::new(
            "public-client-id",
            Some("https://brick.example/callback"),
            "http://127.0.0.1:8787",
            "test-local-key",
            ["i1.sndcdn.com"],
        )
        .unwrap();
        assert_eq!(config.client_id(), "public-client-id");
        assert_eq!(config.redirect_uri().unwrap().scheme(), "https");
        assert_eq!(config.token_service_url().host_str(), Some("127.0.0.1"));
        assert!(config.approved_artwork_hosts().contains("i1.sndcdn.com"));
        assert!(
            LiveApiConfig::new(
                "public-client-id",
                None,
                "http://127.0.0.1:8787",
                "test-local-key",
                ["*.sndcdn.com"],
            )
            .is_err()
        );
        assert!(
            LiveApiConfig::new(
                "public-client-id",
                Some("http://brick.example/callback"),
                "http://127.0.0.1:8787",
                "test-local-key",
                ["i1.sndcdn.com"],
            )
            .is_err()
        );
        assert!(
            LiveApiConfig::new(
                "public-client-id",
                None,
                "http://localhost:8787",
                "test-local-key",
                std::iter::empty::<&str>(),
            )
            .is_err()
        );
    }

    #[test]
    fn loopback_auth_caches_a_token_and_sends_the_local_service_key() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.contains("X-SoundCloud-Local-Key: fixture-local-key"));
            let expires_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 3600;
            let body = format!(r#"{{"access_token":"test-lease","expires_at_unix":{expires_at}}}"#);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(), body
            )
            .unwrap();
        });
        let config = LiveApiConfig::new(
            "fixture-client-id",
            None,
            format!("http://{address}"),
            "fixture-local-key",
            std::iter::empty::<&str>(),
        )
        .unwrap();
        let auth = LocalTokenServiceAuth::new(&config).unwrap();
        assert_eq!(auth.access_token().unwrap().as_deref(), Some("test-lease"));
        assert_eq!(auth.access_token().unwrap().as_deref(), Some("test-lease"));
        assert_eq!(auth.state(), AuthState::Authorized);
        server.join().unwrap();
    }

    #[test]
    fn remote_worker_configuration_is_https_only_and_never_needs_credentials() {
        let config = RemoteWorkerConfig::new("https://worker.example", ["i1.sndcdn.com"]).unwrap();
        assert_eq!(config.backend_url().as_str(), "https://worker.example/");
        assert!(config.approved_artwork_hosts().contains("i1.sndcdn.com"));
        assert!(
            RemoteWorkerConfig::new("http://127.0.0.1:8787", std::iter::empty::<&str>()).is_err()
        );
        assert!(
            RemoteWorkerConfig::new(
                "https://credential@worker.example",
                std::iter::empty::<&str>()
            )
            .is_err()
        );
    }

    #[test]
    fn production_artwork_allowlist_keeps_the_reviewed_cdn_without_environment_setup() {
        let defaults = production_artwork_hosts(None);
        assert_eq!(defaults, ["i1.sndcdn.com"]);

        let extended = production_artwork_hosts(Some(" images.example.test, i1.sndcdn.com "));
        assert!(extended.iter().any(|host| host == "i1.sndcdn.com"));
        assert!(extended.iter().any(|host| host == "images.example.test"));
    }

    #[test]
    fn qr_pairing_url_is_fixed_to_the_configured_worker_origin_and_pairing_id() {
        let config =
            RemoteWorkerConfig::new("https://worker.example", std::iter::empty::<&str>()).unwrap();
        let transport = CloudflareDeviceAuthTransport::new(&config);
        let pairing_id = "pairing_id_abcdefghijklmnop";
        let valid = serde_json::json!({
            "pairing_url": format!(
                "https://worker.example/auth/connect?pairing={pairing_id}"
            )
        });
        assert_eq!(
            transport.pairing_url(&valid, pairing_id).unwrap(),
            format!("https://worker.example/auth/connect?pairing={pairing_id}")
        );

        for invalid in [
            serde_json::json!({
                "pairing_url": format!("https://phishing.example/auth/connect?pairing={pairing_id}")
            }),
            serde_json::json!({
                "pairing_url": "https://worker.example/auth/connect?pairing=another_pairing_id_123456"
            }),
            serde_json::json!({
                "pairing_url": format!(
                    "https://worker.example/auth/connect?pairing={pairing_id}&next=https://phishing.example"
                )
            }),
        ] {
            assert_eq!(
                transport.pairing_url(&invalid, pairing_id),
                Err(BackendFailure::InvalidResponse)
            );
        }
    }

    #[test]
    fn playlist_tracks_url_is_fixed_to_the_worker_and_keeps_the_exact_urn_segment() {
        let config =
            RemoteWorkerConfig::new("https://worker.example", std::iter::empty::<&str>()).unwrap();
        let transport = CloudflareDeviceAuthTransport::new(&config);
        assert_eq!(
            transport
                .playlist_tracks_endpoint("soundcloud:playlists:99001")
                .unwrap()
                .as_str(),
            "https://worker.example/auth/playlists/soundcloud:playlists:99001/tracks"
        );
        assert_eq!(
            transport.playlist_tracks_endpoint("https://evil.example/playlist"),
            Err(BackendFailure::Unsupported)
        );
    }

    #[test]
    fn stream_descriptor_url_is_fixed_to_the_worker_and_keeps_the_exact_urn_segment() {
        let config =
            RemoteWorkerConfig::new("https://worker.example", std::iter::empty::<&str>()).unwrap();
        let transport = CloudflareDeviceAuthTransport::new(&config);
        assert_eq!(
            transport
                .stream_endpoint("soundcloud:tracks:99001")
                .unwrap()
                .as_str(),
            "https://worker.example/auth/tracks/soundcloud:tracks:99001/stream"
        );
        for invalid in [
            "https://evil.example/track",
            "soundcloud:playlists:99001",
            "soundcloud:tracks:99001/../../auth/me",
        ] {
            assert_eq!(
                transport.stream_endpoint(invalid),
                Err(BackendFailure::Unsupported)
            );
        }
    }

    #[test]
    fn remote_worker_auth_uses_no_client_token_and_maps_worker_search_response() {
        let auth = CloudflareWorkerAuth;
        assert_eq!(auth.state(), AuthState::ServiceReady);
        assert_eq!(auth.access_token().unwrap(), None);

        let client = SoundCloudApiClient {
            auth: Arc::new(auth),
            transport: Arc::new(FixtureTransport::new(vec![response(&format!(
                r#"{{"collection":[{TRACK_JSON}],"playlists":[{{"urn":"soundcloud:playlists:88001","title":"Remote playlist","track_count":8}}]}}"#
            ))])),
        };
        assert!(matches!(
            client.execute(BackendCommand::search_tracks("remote")).as_slice(),
            [BackendEvent::SearchResults { query, tracks, playlists, .. }, BackendEvent::ArtworkAvailable { .. }]
                if query == "remote"
                    && tracks[0].id == TrackId::new(987654)
                    && playlists[0].id == PlaylistId::new(88001)
        ));
    }

    fn wait_for_events(backend: &SoundCloudBackend, minimum: usize) -> Vec<BackendEvent> {
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut events = Vec::new();
        while Instant::now() < deadline {
            events.extend(backend.take_events());
            if events.len() >= minimum {
                return events;
            }
            thread::sleep(Duration::from_millis(2));
        }
        events
    }

    #[test]
    fn offline_fixture_exercises_backend_catalog_player_and_artwork_policy_end_to_end() {
        use crate::artwork::{ArtworkConfig, ArtworkEvent, ArtworkManager};
        use crate::state::{DataMode, SearchStatus, UiState};

        const SEARCH: &str = r#"{
          "collection": [
            {"id":501,"title":"Approved","duration":120000,"user":{"id":1,"username":"One"},"artwork_url":"https://i1.sndcdn.com/approved.jpg","access":"playable","genre":"Test"},
            {"id":502,"title":"Unapproved","duration":121000,"user":{"id":2,"username":"Two"},"artwork_url":"https://catalog-unapproved.example/cover.jpg","access":"playable","genre":"Test"}
          ]
        }"#;
        const PLAYLIST: &str = r#"{
          "id":700,"title":"Fixture Playlist","track_count":2,"tracks":[
            {"id":502,"title":"Unapproved","duration":121000,"user":{"id":2,"username":"Two"},"artwork_url":"https://catalog-unapproved.example/cover.jpg","access":"playable","genre":"Test"},
            {"id":503,"title":"No artwork","duration":122000,"user":{"id":3,"username":"Three"},"access":"playable","genre":"Test"}
          ]
        }"#;

        let ctx = Context::default();
        let mut backend = SoundCloudBackend::start(
            &ctx,
            Arc::new(FixedAuth),
            Arc::new(FixtureTransport::new(vec![
                response(SEARCH),
                response(PLAYLIST),
                Ok(HttpResponse {
                    status: 429,
                    body: Vec::new(),
                }),
            ])),
            None,
        );
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        let mut artwork = ArtworkManager::new(
            &ctx,
            ArtworkConfig::preview()
                .with_approved_artwork_hosts(["i1.sndcdn.com"])
                .unwrap(),
        );

        state.query = "fixture".to_owned();
        state.submit_search();
        for command in state.take_backend_commands() {
            backend.send(command).unwrap();
        }
        let search_events = wait_for_events(&backend, 3);
        assert_eq!(search_events.len(), 3);
        for event in search_events {
            state.apply_backend_event(event);
        }
        assert_eq!(
            state.search_results(),
            vec![TrackId::new(501), TrackId::new(502)]
        );
        assert_eq!(state.search_status(), SearchStatus::Results { count: 2 });
        assert_eq!(
            state
                .catalog
                .all_track_ids()
                .into_iter()
                .filter(|id| *id == TrackId::new(501))
                .count(),
            1
        );
        assert!(
            artwork.accepts_url(
                state
                    .track(TrackId::new(501))
                    .unwrap()
                    .artwork_url
                    .as_deref()
                    .unwrap()
            )
        );
        let rejected_artwork = state.track(TrackId::new(502)).unwrap().artwork_url.unwrap();
        assert!(!artwork.accepts_url(&rejected_artwork));
        assert!(artwork.texture_for_url(Some(&rejected_artwork)).is_none());
        assert!(matches!(
            artwork.take_events().as_slice(),
            [ArtworkEvent::ArtworkError { .. }]
        ));

        state.select_track(TrackId::new(501));
        assert_eq!(state.current_track().unwrap().id, TrackId::new(501));
        assert!(!state.is_playing());
        state.next();
        assert_eq!(state.current_track().unwrap().id, TrackId::new(502));

        backend
            .send(BackendCommand::GetPlaylist {
                playlist_id: PlaylistId::new(700),
            })
            .unwrap();
        let playlist_events = wait_for_events(&backend, 2);
        assert_eq!(playlist_events.len(), 2);
        for event in playlist_events {
            state.apply_backend_event(event);
        }
        assert_eq!(
            state
                .catalog
                .playlist(PlaylistId::new(700))
                .unwrap()
                .track_ids,
            vec![TrackId::new(502), TrackId::new(503)]
        );
        assert!(
            state
                .track(TrackId::new(503))
                .unwrap()
                .artwork_url
                .is_none()
        );
        assert!(artwork.texture_for_url(None).is_none());

        state.query = "rate".to_owned();
        state.submit_search();
        for command in state.take_backend_commands() {
            backend.send(command).unwrap();
        }
        let error_events = wait_for_events(&backend, 1);
        assert!(matches!(
            error_events.as_slice(),
            [BackendEvent::BackendError {
                failure: BackendFailure::RateLimited,
                ..
            }]
        ));
        for event in error_events {
            state.apply_backend_event(event);
        }
        assert_eq!(state.search_status(), SearchStatus::Error);
        assert!(state.search_results().is_empty());
        assert!(state.track(TrackId::new(501)).is_some());
        assert_eq!(
            state.toast.as_deref(),
            Some("Đã đạt giới hạn yêu cầu SoundCloud; hãy thử lại sau")
        );
        backend.shutdown();
    }
}
