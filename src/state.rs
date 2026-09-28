use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use crate::backend::{
    AuthState, BackendCommand, BackendEvent, BackendOperation, PlaylistId, ProfileId, QrLoginPhase,
    QrLoginSession, SoundCloudPlaylist, SoundCloudProfile, SoundCloudTrack, UserSession,
};
use crate::mock_engine::MockPlaybackEngine;
use crate::playback_engine::{PlaybackCommand as AudioCommand, PlaybackEngineEvent};
use crate::player::{
    PlaybackStatus, PlayerCommand, PlayerController, PlayerEvent, QueueEntry, QueueEntryId,
    TrackPlaybackMetadata,
};

pub use crate::player::RepeatMode;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Page {
    Home,
    Discover,
    Search,
    Library,
    Likes,
    Playlist,
    NowPlaying,
    Profile,
    Stations,
    Following,
    Settings,
}

impl Page {
    pub const fn title(self) -> &'static str {
        match self {
            Self::Home => "Trang chủ",
            Self::Discover => "Khám phá",
            Self::Search => "Tìm kiếm",
            Self::Library => "Thư viện",
            Self::Likes => "Bài hát đã thích",
            Self::Playlist => "Danh sách phát",
            Self::NowPlaying => "Đang phát",
            Self::Profile => "Hồ sơ",
            Self::Stations => "Trạm nhạc",
            Self::Following => "Đang theo dõi",
            Self::Settings => "Cài đặt",
        }
    }
}

/// The application has exactly one active top-level layout. `Main` owns the
/// regular navigation shell, `Restoring` validates a saved session without
/// exposing the pairing UI, and `Login` owns the QR authentication viewport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppRoute {
    Main(Page),
    Restoring { destination: Page },
    Login { return_page: Page },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LibraryTab {
    Tracks,
    Playlists,
}

/// Preview is local fixture data. Live never falls back to fixture search
/// results after a backend error, so the UI cannot mislabel mock results as API
/// results.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataMode {
    Preview,
    Live,
}

/// Visible state for the one search surface. Search metadata itself remains in
/// `Catalog`, so the UI cannot accidentally grow a second track store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchStatus {
    Idle,
    Loading,
    Results { count: usize },
    Empty,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LibraryStatus {
    Idle,
    Loading,
    Loaded,
    Empty,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaylistStatus {
    Idle,
    Loading,
    Loaded,
    Empty,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub struct TrackId(u64);

impl TrackId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaybackAvailability {
    Available,
    Unavailable,
}

/// A local fixture cover. Remote artwork is always supplied in `artwork_url`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ArtworkId {
    NightDrive,
    SoftFocus,
}

/// One application model, identified by a stable SoundCloud or fixture ID.
/// Every screen reads it from `Catalog`; no screen owns a second track store.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Track {
    pub id: TrackId,
    pub urn: Option<String>,
    pub title: String,
    pub artist: String,
    pub duration_seconds: Option<u32>,
    pub artwork_url: Option<String>,
    pub waveform_url: Option<String>,
    pub availability: PlaybackAvailability,
    pub mood: String,
    pub tint: [u8; 3],
    pub artwork: Option<ArtworkId>,
}

impl Track {
    pub fn duration_label(&self) -> String {
        format_duration(self.duration_seconds)
    }

    fn playback_metadata(&self) -> TrackPlaybackMetadata {
        TrackPlaybackMetadata {
            duration_seconds: self.duration_seconds,
            availability: self.availability,
        }
    }
}

impl From<SoundCloudTrack> for Track {
    fn from(track: SoundCloudTrack) -> Self {
        Self {
            id: track.id,
            urn: track.urn,
            title: track.title,
            artist: track.artist,
            duration_seconds: track.duration_seconds,
            artwork_url: track.artwork_url,
            waveform_url: track.waveform_url,
            availability: track.availability,
            mood: track.genre,
            tint: tint_from_id(track.id),
            artwork: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Playlist {
    pub id: PlaylistId,
    pub urn: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub artwork_url: Option<String>,
    pub track_count: u32,
    pub editable: bool,
    pub track_ids: Vec<TrackId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicProfile {
    pub id: ProfileId,
    pub username: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

impl From<SoundCloudProfile> for PublicProfile {
    fn from(profile: SoundCloudProfile) -> Self {
        Self {
            id: profile.id,
            username: profile.username,
            display_name: profile.display_name,
            avatar_url: profile.avatar_url,
        }
    }
}

/// The sole full metadata store. Lists retain only IDs into `tracks`.
#[derive(Clone, Debug)]
pub struct Catalog {
    tracks: BTreeMap<TrackId, Track>,
    home_track_ids: Vec<TrackId>,
    live_home_track_ids: Vec<TrackId>,
    discover_track_ids: Vec<TrackId>,
    search_track_ids: Vec<TrackId>,
    search_playlist_ids: Vec<PlaylistId>,
    liked_track_ids: Vec<TrackId>,
    library_playlist_ids: Vec<PlaylistId>,
    playlists: BTreeMap<PlaylistId, Playlist>,
    profile: Option<PublicProfile>,
}

impl Catalog {
    fn preview() -> Self {
        let preview = preview_tracks();
        let home_track_ids = preview.iter().map(|track| track.id).collect();
        let tracks = preview.into_iter().map(|track| (track.id, track)).collect();
        Self {
            tracks,
            home_track_ids,
            live_home_track_ids: Vec::new(),
            discover_track_ids: Vec::new(),
            search_track_ids: Vec::new(),
            search_playlist_ids: Vec::new(),
            liked_track_ids: Vec::new(),
            library_playlist_ids: Vec::new(),
            playlists: BTreeMap::new(),
            profile: None,
        }
    }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.get(&id)
    }
    pub fn track_cloned(&self, id: TrackId) -> Option<Track> {
        self.track(id).cloned()
    }
    pub fn home_track_ids(&self) -> &[TrackId] {
        &self.home_track_ids
    }
    pub fn live_home_track_ids(&self) -> &[TrackId] {
        &self.live_home_track_ids
    }
    pub fn discover_track_ids(&self) -> &[TrackId] {
        &self.discover_track_ids
    }
    pub fn preview_track(&self, index: usize) -> Track {
        self.track_cloned(self.home_track_ids[index])
            .expect("preview catalog is complete")
    }
    pub fn all_track_ids(&self) -> Vec<TrackId> {
        self.tracks.keys().copied().collect()
    }
    pub fn playlist(&self, id: PlaylistId) -> Option<&Playlist> {
        self.playlists.get(&id)
    }
    pub fn liked_track_ids(&self) -> &[TrackId] {
        &self.liked_track_ids
    }
    pub fn library_playlists(&self) -> Vec<Playlist> {
        self.library_playlist_ids
            .iter()
            .filter_map(|id| self.playlists.get(id).cloned())
            .collect()
    }

    pub fn upsert_tracks<I>(&mut self, tracks: I)
    where
        I: IntoIterator<Item = Track>,
    {
        for track in tracks {
            self.tracks.insert(track.id, track);
        }
    }

    fn replace_search(&mut self, tracks: Vec<Track>, playlists: Vec<SoundCloudPlaylist>) {
        self.search_track_ids = tracks.iter().map(|track| track.id).collect();
        self.upsert_tracks(tracks);
        self.search_playlist_ids = playlists.iter().map(|playlist| playlist.id).collect();
        for playlist in playlists {
            self.upsert_playlist(playlist, Vec::new());
        }
    }

    fn append_search(&mut self, tracks: Vec<Track>, playlists: Vec<SoundCloudPlaylist>) {
        for track in &tracks {
            if !self.search_track_ids.contains(&track.id) {
                self.search_track_ids.push(track.id);
            }
        }
        self.upsert_tracks(tracks);
        for playlist in playlists {
            if !self.search_playlist_ids.contains(&playlist.id) {
                self.search_playlist_ids.push(playlist.id);
            }
            self.upsert_playlist(playlist, Vec::new());
        }
    }

    fn clear_search(&mut self) {
        self.search_track_ids.clear();
        self.search_playlist_ids.clear();
    }

    fn upsert_playlist(&mut self, playlist: SoundCloudPlaylist, tracks: Vec<Track>) {
        let track_ids = tracks.iter().map(|track| track.id).collect();
        self.upsert_tracks(tracks);
        self.playlists.insert(
            playlist.id,
            Playlist {
                id: playlist.id,
                urn: playlist.urn,
                title: playlist.title,
                description: playlist.description,
                artwork_url: playlist.artwork_url,
                track_count: playlist.track_count,
                editable: playlist.editable,
                track_ids,
            },
        );
    }

    fn replace_playlist_tracks(&mut self, playlist_id: PlaylistId, tracks: Vec<Track>) {
        let track_ids = tracks.iter().map(|track| track.id).collect();
        self.upsert_tracks(tracks);
        if let Some(playlist) = self.playlists.get_mut(&playlist_id) {
            playlist.track_ids = track_ids;
        }
    }

    fn set_liked(&mut self, track_id: TrackId, liked: bool) {
        if liked {
            if !self.liked_track_ids.contains(&track_id) {
                self.liked_track_ids.push(track_id);
            }
        } else {
            self.liked_track_ids.retain(|id| *id != track_id);
        }
    }

    fn add_track_to_playlist(
        &mut self,
        playlist_id: PlaylistId,
        track_id: TrackId,
        track_count: u32,
    ) {
        if let Some(playlist) = self.playlists.get_mut(&playlist_id) {
            if !playlist.track_ids.contains(&track_id) {
                playlist.track_ids.push(track_id);
            }
            playlist.track_count = track_count;
        }
    }

    fn remove_track_from_playlist(
        &mut self,
        playlist_id: PlaylistId,
        track_id: TrackId,
        track_index: usize,
        track_count: u32,
    ) {
        if let Some(playlist) = self.playlists.get_mut(&playlist_id) {
            if playlist.track_ids.get(track_index) == Some(&track_id) {
                playlist.track_ids.remove(track_index);
            } else if let Some(index) = playlist.track_ids.iter().position(|id| *id == track_id) {
                // The Worker already verified the exact server-side index and
                // URN. This fallback only reconciles a locally stale ordering.
                playlist.track_ids.remove(index);
            }
            playlist.track_count = track_count;
        }
    }

    fn add_created_playlist(&mut self, playlist: SoundCloudPlaylist) {
        let playlist_id = playlist.id;
        self.upsert_playlist(playlist, Vec::new());
        self.library_playlist_ids.retain(|id| *id != playlist_id);
        self.library_playlist_ids.insert(0, playlist_id);
    }

    fn remove_playlist(&mut self, playlist_id: PlaylistId) {
        self.library_playlist_ids.retain(|id| *id != playlist_id);
        self.search_playlist_ids.retain(|id| *id != playlist_id);
        self.playlists.remove(&playlist_id);
    }

    fn replace_user_library(
        &mut self,
        liked_tracks: Vec<Track>,
        playlists: Vec<SoundCloudPlaylist>,
        home_tracks: Vec<Track>,
        discover_tracks: Vec<Track>,
    ) {
        self.liked_track_ids = liked_tracks.iter().map(|track| track.id).collect();
        self.upsert_tracks(liked_tracks);
        self.live_home_track_ids = home_tracks.iter().map(|track| track.id).collect();
        self.upsert_tracks(home_tracks);
        self.discover_track_ids = discover_tracks.iter().map(|track| track.id).collect();
        self.upsert_tracks(discover_tracks);
        self.library_playlist_ids = playlists.iter().map(|playlist| playlist.id).collect();
        for playlist in playlists {
            self.upsert_playlist(playlist, Vec::new());
        }
    }

    fn clear_user_library(&mut self) {
        self.liked_track_ids.clear();
        self.library_playlist_ids.clear();
        self.playlists.clear();
        self.live_home_track_ids.clear();
        self.discover_track_ids.clear();
    }

    fn search_matches(&self, query: &str) -> Vec<TrackId> {
        let query = query.trim().to_ascii_lowercase();
        self.tracks
            .values()
            .filter(|track| {
                query.is_empty()
                    || track.title.to_ascii_lowercase().contains(&query)
                    || track.artist.to_ascii_lowercase().contains(&query)
                    || track.mood.to_ascii_lowercase().contains(&query)
            })
            .map(|track| track.id)
            .collect()
    }

    fn context_for(&self, track_id: TrackId) -> Vec<TrackId> {
        if self.search_track_ids.contains(&track_id) {
            return self.search_track_ids.clone();
        }
        if self.live_home_track_ids.contains(&track_id) {
            return self.live_home_track_ids.clone();
        }
        if self.discover_track_ids.contains(&track_id) {
            return self.discover_track_ids.clone();
        }
        if self.liked_track_ids.contains(&track_id) {
            return self.liked_track_ids.clone();
        }
        if let Some(playlist) = self
            .playlists
            .values()
            .find(|playlist| playlist.track_ids.contains(&track_id))
        {
            return playlist.track_ids.clone();
        }
        self.home_track_ids.clone()
    }

    fn sync_player(&self, player: &mut PlayerController) {
        player.sync_track_metadata(
            self.tracks
                .values()
                .map(|track| (track.id, track.playback_metadata())),
        );
    }
}

pub fn format_duration(seconds: Option<u32>) -> String {
    match seconds {
        Some(seconds) => format!("{}:{:02}", seconds / 60, seconds % 60),
        None => "--:--".to_owned(),
    }
}

pub fn format_position(seconds: f32) -> String {
    let seconds = seconds.max(0.0).round() as u32;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

pub fn format_timer_duration(duration: Duration) -> String {
    let total_seconds = duration.as_secs();
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

fn sanitize_timer_field(value: &mut String, max_chars: usize) {
    value.retain(|character| character.is_ascii_digit());
    value.truncate(max_chars);
}

pub struct UiState {
    pub data_mode: DataMode,
    route: AppRoute,
    navigation_stack: Vec<Page>,
    pub library_tab: LibraryTab,
    pub sidebar_collapsed: bool,
    pub query: String,
    pub submitted_query: String,
    search_status: SearchStatus,
    library_status: LibraryStatus,
    playlist_status: PlaylistStatus,
    selected_playlist_id: Option<PlaylistId>,
    next_search_request_id: u64,
    active_search_request_id: Option<u64>,
    search_next_cursor: Option<String>,
    search_loading_more: bool,
    pub player: PlayerController,
    pub catalog: Catalog,
    mock_engine: MockPlaybackEngine,
    liked: BTreeSet<TrackId>,
    pending_likes: BTreeSet<TrackId>,
    add_to_playlist_track: Option<TrackId>,
    pending_playlist_add: Option<(TrackId, PlaylistId)>,
    playlist_track_remove_mode: Option<PlaylistId>,
    remove_playlist_track: Option<(PlaylistId, TrackId, usize)>,
    pending_playlist_track_remove: Option<(PlaylistId, TrackId, usize)>,
    create_playlist_open: bool,
    new_playlist_title: String,
    playlist_create_pending: bool,
    delete_playlist_id: Option<PlaylistId>,
    pending_playlist_delete: Option<PlaylistId>,
    pub toast: Option<String>,
    pub compact_effects: bool,
    pub show_battery_percentage: bool,
    pub always_keep_screen_on: bool,
    pub minimal_interface: bool,
    battery_percentage: Option<u8>,
    stop_timer_hours: String,
    stop_timer_minutes: String,
    stop_timer_deadline: Option<Instant>,
    pub queue_open: bool,
    auth_state: AuthState,
    qr_login: Option<QrLoginSession>,
    auth_poll_in_flight: bool,
    user_session: Option<UserSession>,
    auth_error: Option<String>,
    pending_backend_commands: Vec<BackendCommand>,
    live_playback_enabled: bool,
    next_stream_request_id: u64,
    active_stream_request: Option<(u64, TrackId, QueueEntryId)>,
    pending_audio_commands: Vec<AudioCommand>,
    audio_suspended_for_idle: bool,
    resume_after_start_seconds: Option<f32>,
}

impl Default for UiState {
    fn default() -> Self {
        let catalog = Catalog::preview();
        let mut player = PlayerController::new();
        catalog.sync_player(&mut player);
        let liked = [
            catalog.preview_track(0).id,
            catalog.preview_track(3).id,
            catalog.preview_track(5).id,
        ]
        .into_iter()
        .collect();
        Self {
            data_mode: DataMode::Preview,
            route: AppRoute::Main(Page::Home),
            navigation_stack: Vec::new(),
            library_tab: LibraryTab::Tracks,
            sidebar_collapsed: false,
            query: String::new(),
            submitted_query: String::new(),
            search_status: SearchStatus::Idle,
            library_status: LibraryStatus::Idle,
            playlist_status: PlaylistStatus::Idle,
            selected_playlist_id: None,
            next_search_request_id: 0,
            active_search_request_id: None,
            search_next_cursor: None,
            search_loading_more: false,
            player,
            catalog,
            mock_engine: MockPlaybackEngine,
            liked,
            pending_likes: BTreeSet::new(),
            add_to_playlist_track: None,
            pending_playlist_add: None,
            playlist_track_remove_mode: None,
            remove_playlist_track: None,
            pending_playlist_track_remove: None,
            create_playlist_open: false,
            new_playlist_title: String::new(),
            playlist_create_pending: false,
            delete_playlist_id: None,
            pending_playlist_delete: None,
            toast: None,
            compact_effects: true,
            show_battery_percentage: true,
            always_keep_screen_on: false,
            minimal_interface: false,
            battery_percentage: None,
            stop_timer_hours: "0".to_owned(),
            stop_timer_minutes: "30".to_owned(),
            stop_timer_deadline: None,
            queue_open: false,
            auth_state: AuthState::Unconfigured,
            qr_login: None,
            auth_poll_in_flight: false,
            user_session: None,
            auth_error: None,
            pending_backend_commands: Vec::new(),
            live_playback_enabled: false,
            next_stream_request_id: 0,
            active_stream_request: None,
            pending_audio_commands: Vec::new(),
            audio_suspended_for_idle: false,
            resume_after_start_seconds: None,
        }
    }
}

impl UiState {
    pub fn set_show_battery_percentage(&mut self, enabled: bool) {
        self.show_battery_percentage = enabled;
    }

    pub fn set_always_keep_screen_on(&mut self, enabled: bool) {
        self.always_keep_screen_on = enabled;
    }

    pub fn set_minimal_interface(&mut self, enabled: bool) {
        self.minimal_interface = enabled;
    }

    pub fn set_battery_percentage(&mut self, percentage: Option<u8>) {
        self.battery_percentage = percentage.filter(|value| *value <= 100);
    }

    pub const fn battery_percentage(&self) -> Option<u8> {
        self.battery_percentage
    }

    pub fn stop_timer_hours_mut(&mut self) -> &mut String {
        &mut self.stop_timer_hours
    }

    pub fn stop_timer_minutes_mut(&mut self) -> &mut String {
        &mut self.stop_timer_minutes
    }

    pub fn clear_stop_timer_hours(&mut self) {
        self.stop_timer_hours.clear();
    }

    pub fn clear_stop_timer_minutes(&mut self) {
        self.stop_timer_minutes.clear();
    }

    pub fn sanitize_stop_timer_inputs(&mut self) {
        sanitize_timer_field(&mut self.stop_timer_hours, 2);
        sanitize_timer_field(&mut self.stop_timer_minutes, 2);
    }

    pub fn set_stop_timer(&mut self) -> bool {
        self.set_stop_timer_at(Instant::now())
    }

    fn set_stop_timer_at(&mut self, now: Instant) -> bool {
        self.sanitize_stop_timer_inputs();
        let hours = self.stop_timer_hours.parse::<u64>().unwrap_or(0);
        let minutes = self.stop_timer_minutes.parse::<u64>().unwrap_or(0);
        if hours > 23 || minutes > 59 || (hours == 0 && minutes == 0) {
            self.toast = Some("Nhập thời gian hẹn giờ từ 00:01 đến 23:59".to_owned());
            println!("BRICKWAVE_SLEEP_TIMER event=ERROR reason=invalid-duration");
            return false;
        }
        let seconds = hours
            .saturating_mul(60 * 60)
            .saturating_add(minutes.saturating_mul(60));
        self.stop_timer_deadline = now.checked_add(Duration::from_secs(seconds));
        if self.stop_timer_deadline.is_none() {
            self.toast = Some("Không thể bắt đầu hẹn giờ".to_owned());
            println!("BRICKWAVE_SLEEP_TIMER event=ERROR reason=deadline-overflow");
            return false;
        }
        self.toast = Some(format!(
            "Nhạc sẽ tạm dừng sau {}",
            format_timer_duration(Duration::from_secs(seconds))
        ));
        println!(
            "BRICKWAVE_SLEEP_TIMER event=SET hours={hours} minutes={minutes} seconds={seconds}"
        );
        true
    }

    pub fn cancel_stop_timer(&mut self) {
        if self.stop_timer_deadline.take().is_some() {
            self.toast = Some("Đã hủy hẹn giờ".to_owned());
            println!("BRICKWAVE_SLEEP_TIMER event=CANCEL");
        }
    }

    pub fn stop_timer_remaining(&self) -> Option<Duration> {
        self.stop_timer_remaining_at(Instant::now())
    }

    fn stop_timer_remaining_at(&self, now: Instant) -> Option<Duration> {
        self.stop_timer_deadline
            .map(|deadline| deadline.saturating_duration_since(now))
    }

    pub fn service_stop_timer(&mut self) {
        self.service_stop_timer_at(Instant::now());
    }

    fn service_stop_timer_at(&mut self, now: Instant) {
        let Some(deadline) = self.stop_timer_deadline else {
            return;
        };
        if now < deadline {
            return;
        }
        self.stop_timer_deadline = None;
        let playback_status = self.player_state().playback_status.clone();
        match playback_status {
            PlaybackStatus::Playing => self.pause_playback(),
            PlaybackStatus::Loading => {
                self.active_stream_request = None;
                if self.data_mode == DataMode::Live && self.live_playback_enabled {
                    self.pending_audio_commands.push(AudioCommand::Stop);
                }
                self.dispatch_player(PlayerCommand::Stop);
            }
            PlaybackStatus::Idle
            | PlaybackStatus::Paused
            | PlaybackStatus::Ended
            | PlaybackStatus::Error(_) => {}
        }
        self.toast = Some("Hẹn giờ đã kết thúc; nhạc đã tạm dừng".to_owned());
        println!("BRICKWAVE_SLEEP_TIMER event=EXPIRED action=pause");
    }

    pub const fn route(&self) -> AppRoute {
        self.route
    }

    pub const fn main_page(&self) -> Page {
        match self.route {
            AppRoute::Main(page) => page,
            AppRoute::Restoring { destination } => destination,
            AppRoute::Login { return_page } => return_page,
        }
    }

    pub const fn is_login_route(&self) -> bool {
        matches!(self.route, AppRoute::Login { .. })
    }

    pub const fn is_restoring_route(&self) -> bool {
        matches!(self.route, AppRoute::Restoring { .. })
    }

    /// LIVE mode is account-gated. A restored session is not trusted until
    /// `/auth/me` has validated it and moved `auth_state` to `Authorized`.
    /// Preview mode remains available for local UI development.
    pub fn can_enter_main_app(&self) -> bool {
        self.data_mode == DataMode::Preview
            || (self.auth_state == AuthState::Authorized && self.user_session.is_some())
    }

    pub fn can_leave_login(&self) -> bool {
        self.can_enter_main_app()
    }

    pub fn navigate_main(&mut self, page: Page) {
        if page != Page::Playlist {
            self.playlist_track_remove_mode = None;
        }
        if page == Page::Home {
            self.navigation_stack.clear();
        } else if let AppRoute::Main(current) = self.route
            && current != page
        {
            if self.navigation_stack.last().copied() != Some(current) {
                self.navigation_stack.push(current);
                if self.navigation_stack.len() > 32 {
                    self.navigation_stack.remove(0);
                }
            }
        }
        self.route = if self.can_enter_main_app() {
            AppRoute::Main(page)
        } else {
            AppRoute::Login { return_page: page }
        };
        self.queue_open = false;
    }

    pub fn can_navigate_back(&self) -> bool {
        self.main_page() != Page::Home
    }

    pub fn navigate_back(&mut self) {
        if !self.can_enter_main_app() {
            return;
        }
        let destination = self.navigation_stack.pop().unwrap_or(Page::Home);
        self.route = AppRoute::Main(destination);
        self.queue_open = false;
        self.playlist_track_remove_mode = None;
    }

    pub fn open_login(&mut self) {
        if self.auth_state == AuthState::Authorized {
            self.navigate_main(Page::Profile);
            return;
        }
        let return_page = self.main_page();
        self.route = AppRoute::Login { return_page };
        self.queue_open = false;
    }

    pub fn back_from_login(&mut self) {
        let AppRoute::Login { return_page } = self.route else {
            return;
        };
        if !self.can_enter_main_app() {
            return;
        }
        self.stop_pairing_on_leave();
        self.route = AppRoute::Main(return_page);
    }

    pub fn cancel_login_and_return(&mut self) {
        let return_page = match self.route {
            AppRoute::Login { return_page } => return_page,
            AppRoute::Restoring { destination } => destination,
            AppRoute::Main(page) => page,
        };
        let can_return = self.can_enter_main_app();
        self.stop_pairing_on_leave();
        self.auth_error = None;
        if can_return {
            self.route = AppRoute::Main(return_page);
        } else {
            self.auth_state = AuthState::Cancelled;
            self.route = AppRoute::Login { return_page };
        }
    }

    fn stop_pairing_on_leave(&mut self) {
        self.auth_poll_in_flight = false;
        if let Some(login) = self.qr_login.take() {
            self.pending_backend_commands
                .push(BackendCommand::CancelQrLogin { login });
            self.auth_state = AuthState::Cancelled;
            self.auth_error = None;
        } else if self.auth_state == AuthState::CreatingQr {
            // A late StartQrLogin response is rejected in apply_backend_event.
            self.auth_state = AuthState::Cancelled;
            self.auth_error = None;
        }
    }

    pub fn set_data_mode(&mut self, data_mode: DataMode) {
        if self.data_mode != data_mode && self.live_playback_enabled {
            self.active_stream_request = None;
            self.pending_audio_commands.push(AudioCommand::Stop);
        }
        self.data_mode = data_mode;
        self.search_status = SearchStatus::Idle;
        self.active_search_request_id = None;
        self.search_next_cursor = None;
        self.search_loading_more = false;
        self.catalog.clear_search();
        if data_mode == DataMode::Live && self.auth_state == AuthState::Unconfigured {
            self.auth_state = AuthState::LoggedOut;
        }
        if data_mode == DataMode::Live && !self.can_enter_main_app() {
            self.navigation_stack.clear();
            self.route = AppRoute::Login {
                return_page: self.main_page(),
            };
            self.queue_open = false;
        }
    }
    pub const fn search_status(&self) -> SearchStatus {
        self.search_status
    }
    pub const fn library_status(&self) -> LibraryStatus {
        self.library_status
    }
    pub const fn playlist_status(&self) -> PlaylistStatus {
        self.playlist_status
    }
    pub fn playback_is_unavailable(&self) -> bool {
        self.data_mode == DataMode::Live && !self.live_playback_enabled
    }

    pub fn set_live_playback_enabled(&mut self, enabled: bool) {
        let was_enabled = self.live_playback_enabled;
        self.live_playback_enabled = enabled;
        if was_enabled && !enabled {
            self.active_stream_request = None;
            self.pending_audio_commands.push(AudioCommand::Stop);
        }
    }

    pub const fn auth_state(&self) -> AuthState {
        self.auth_state
    }

    pub fn qr_login(&self) -> Option<&QrLoginSession> {
        self.qr_login.as_ref()
    }

    pub fn current_profile(&self) -> Option<&PublicProfile> {
        self.catalog.profile.as_ref()
    }

    pub fn user_session(&self) -> Option<&UserSession> {
        self.user_session.as_ref()
    }

    pub fn restore_user_session(&mut self, session: UserSession) {
        self.navigation_stack.clear();
        self.route = AppRoute::Restoring {
            destination: Page::Home,
        };
        self.queue_open = false;
        self.auth_state = AuthState::AuthorizationPending;
        self.auth_error = None;
        self.user_session = Some(session.clone());
        self.pending_backend_commands
            .push(BackendCommand::GetCurrentUser { session });
    }

    /// Retry validation of the saved Worker session without creating a new QR
    /// pairing request. This is used only by the dedicated restore screen.
    pub fn retry_restore_session(&mut self) {
        if !self.is_restoring_route() {
            return;
        }
        let Some(session) = self.user_session.clone() else {
            self.abandon_restore_and_login();
            return;
        };
        self.auth_state = AuthState::AuthorizationPending;
        self.auth_error = None;
        self.pending_backend_commands
            .push(BackendCommand::GetCurrentUser { session });
    }

    /// Forget an invalid or unreachable saved session and enter the normal QR
    /// login flow. The persistence layer observes `user_session == None` and
    /// removes the encrypted local session on the following frame.
    pub fn abandon_restore_and_login(&mut self) {
        self.active_stream_request = None;
        self.pending_audio_commands.push(AudioCommand::Stop);
        self.pending_audio_commands
            .push(AudioCommand::ClearSessionCache);
        self.auth_poll_in_flight = false;
        self.user_session = None;
        self.qr_login = None;
        self.catalog.profile = None;
        self.catalog.clear_user_library();
        self.library_status = LibraryStatus::Idle;
        self.playlist_status = PlaylistStatus::Idle;
        self.selected_playlist_id = None;
        self.auth_state = AuthState::LoggedOut;
        self.auth_error = None;
        self.navigation_stack.clear();
        self.route = AppRoute::Login {
            return_page: Page::Home,
        };
        self.queue_open = false;
    }

    pub fn liked_track_ids(&self) -> Vec<TrackId> {
        if self.data_mode == DataMode::Live {
            self.catalog.liked_track_ids().to_vec()
        } else {
            self.catalog
                .all_track_ids()
                .into_iter()
                .filter(|track_id| self.liked.contains(track_id))
                .collect()
        }
    }

    pub fn library_playlists(&self) -> Vec<Playlist> {
        self.catalog.library_playlists()
    }

    pub fn current_playlist(&self) -> Option<Playlist> {
        self.selected_playlist_id
            .and_then(|id| self.catalog.playlist(id).cloned())
    }

    pub fn open_playlist(&mut self, playlist_id: PlaylistId) {
        self.playlist_track_remove_mode = None;
        self.selected_playlist_id = Some(playlist_id);
        self.navigate_main(Page::Playlist);
        if self.data_mode == DataMode::Preview {
            self.playlist_status = PlaylistStatus::Loaded;
            return;
        }
        let Some(playlist) = self.catalog.playlist(playlist_id).cloned() else {
            self.playlist_status = PlaylistStatus::Error;
            self.toast = Some("Không có thông tin danh sách phát".to_owned());
            return;
        };
        if !playlist.track_ids.is_empty() {
            self.playlist_status = PlaylistStatus::Loaded;
            return;
        }
        if playlist.track_count == 0 {
            self.playlist_status = PlaylistStatus::Empty;
            return;
        }
        let Some(playlist_urn) = playlist.urn else {
            self.playlist_status = PlaylistStatus::Error;
            self.toast = Some("SoundCloud không trả về mã danh sách phát".to_owned());
            return;
        };
        let Some(session) = self.user_session.clone() else {
            self.playlist_status = PlaylistStatus::Error;
            self.toast = Some("Hãy kết nối lại SoundCloud để mở danh sách này".to_owned());
            return;
        };
        self.playlist_status = PlaylistStatus::Loading;
        self.pending_backend_commands
            .push(BackendCommand::GetPlaylistTracks {
                session,
                playlist_id,
                playlist_urn,
            });
    }

    pub fn retry_current_playlist(&mut self) {
        if let Some(playlist_id) = self.selected_playlist_id {
            self.open_playlist(playlist_id);
        }
    }

    pub fn refresh_user_library(&mut self) {
        let Some(session) = self.user_session.clone() else {
            self.library_status = LibraryStatus::Error;
            self.toast = Some("Hãy kết nối SoundCloud để tải dữ liệu tài khoản".to_owned());
            return;
        };
        self.library_status = LibraryStatus::Loading;
        self.pending_backend_commands
            .push(BackendCommand::GetUserLibrary { session });
    }

    /// Retries only account data that was incomplete when StockOS suspended
    /// networking. Loaded catalog data is preserved and never refreshed just
    /// because the screen woke.
    pub fn recover_after_network_resume(&mut self) {
        if self.data_mode != DataMode::Live
            || self.auth_state != AuthState::Authorized
            || self.user_session.is_none()
        {
            println!(
                "BRICKWAVE_DATA_WAKE_RECOVERY action=skip mode={:?} auth={:?}",
                self.data_mode, self.auth_state
            );
            return;
        }

        let retry_library = matches!(
            self.library_status,
            LibraryStatus::Idle | LibraryStatus::Error
        );
        let retry_playlist = self.main_page() == Page::Playlist
            && self.selected_playlist_id.is_some()
            && matches!(
                self.playlist_status,
                PlaylistStatus::Idle | PlaylistStatus::Error
            );

        if retry_library {
            self.refresh_user_library();
        }
        if retry_playlist {
            self.retry_current_playlist();
        }
        println!(
            "BRICKWAVE_DATA_WAKE_RECOVERY library_retry={} playlist_retry={} catalog_preserved=true",
            retry_library, retry_playlist
        );
    }

    pub fn auth_error(&self) -> Option<&str> {
        self.auth_error.as_deref()
    }

    pub fn auth_polling_active(&self) -> bool {
        self.is_login_route()
            && self.qr_login.is_some()
            && matches!(
                self.auth_state,
                AuthState::WaitingForScan | AuthState::WaitingForAuthorization
            )
    }

    pub fn start_qr_login(&mut self) {
        if self.data_mode != DataMode::Live {
            self.toast = Some("Chuyển sang chế độ trực tuyến để kết nối SoundCloud".to_owned());
            return;
        }
        if self.auth_state == AuthState::Authorized {
            self.navigate_main(Page::Profile);
            return;
        }
        if self.auth_state == AuthState::CreatingQr
            || (self.qr_login.is_some()
                && matches!(
                    self.auth_state,
                    AuthState::WaitingForScan
                        | AuthState::WaitingForAuthorization
                        | AuthState::PairingConfirmation
                ))
        {
            return;
        }
        if !self.is_login_route() {
            self.open_login();
        }
        self.auth_state = AuthState::CreatingQr;
        self.auth_error = None;
        self.qr_login = None;
        self.auth_poll_in_flight = false;
        self.pending_backend_commands
            .push(BackendCommand::StartQrLogin);
    }

    pub fn poll_qr_login(&mut self) {
        if self.is_login_route() && !self.auth_poll_in_flight {
            let Some(login) = self.qr_login.clone() else {
                return;
            };
            self.auth_poll_in_flight = true;
            self.pending_backend_commands
                .push(BackendCommand::PollQrLogin { login });
        }
    }

    pub fn confirm_pairing(&mut self) {
        if self.auth_state != AuthState::PairingConfirmation {
            return;
        }
        if let Some(login) = self.qr_login.clone() {
            self.pending_backend_commands
                .push(BackendCommand::ConfirmPairing { login });
        }
    }

    pub fn cancel_qr_login(&mut self) {
        self.auth_poll_in_flight = false;
        if let Some(login) = self.qr_login.take() {
            self.pending_backend_commands
                .push(BackendCommand::CancelQrLogin { login });
        }
        self.auth_state = AuthState::Cancelled;
        self.auth_error = None;
    }

    pub fn logout(&mut self) {
        if let Some(session) = self.user_session.clone() {
            self.pending_backend_commands
                .push(BackendCommand::Logout { session });
        }
    }
    pub fn player_state(&self) -> &crate::player::PlayerState {
        self.player.state()
    }
    pub fn current_track(&self) -> Option<Track> {
        self.player_state()
            .current_track_id
            .and_then(|id| self.catalog.track_cloned(id))
    }
    pub fn preview_track(&self, index: usize) -> Track {
        self.catalog.preview_track(index)
    }
    pub fn home_track_ids(&self) -> Vec<TrackId> {
        if self.data_mode == DataMode::Live {
            self.catalog.live_home_track_ids().to_vec()
        } else {
            self.catalog.home_track_ids().to_vec()
        }
    }
    pub fn discover_track_ids(&self) -> Vec<TrackId> {
        self.catalog.discover_track_ids().to_vec()
    }
    pub fn track(&self, id: TrackId) -> Option<Track> {
        self.catalog.track_cloned(id)
    }
    pub fn is_playing(&self) -> bool {
        self.player_state().playback_status.is_playing()
    }
    pub fn is_current_track(&self, track_id: TrackId) -> bool {
        self.player_state().current_track_id == Some(track_id)
    }
    pub fn is_current_queue_entry(&self, entry_id: QueueEntryId) -> bool {
        self.player_state().current_queue_entry_id == Some(entry_id)
    }
    pub fn is_liked(&self, track_id: TrackId) -> bool {
        if self.data_mode == DataMode::Live {
            self.catalog.liked_track_ids().contains(&track_id)
        } else {
            self.liked.contains(&track_id)
        }
    }

    pub fn toggle_like(&mut self, track_id: TrackId) {
        if self.data_mode == DataMode::Live {
            if self.pending_likes.contains(&track_id) {
                return;
            }
            let Some(session) = self.user_session.clone() else {
                self.toast = Some("Hãy kết nối lại SoundCloud để thay đổi bài đã thích".to_owned());
                return;
            };
            let Some(track_urn) = self
                .catalog
                .track(track_id)
                .and_then(|track| track.urn.clone())
            else {
                self.toast = Some("SoundCloud không trả về mã bài hát".to_owned());
                return;
            };
            let liked = !self.catalog.liked_track_ids().contains(&track_id);
            self.pending_likes.insert(track_id);
            self.pending_backend_commands
                .push(BackendCommand::SetTrackLiked {
                    session,
                    track_id,
                    track_urn,
                    liked,
                });
            self.toast = Some(if liked {
                "Đang thêm vào bài đã thích...".to_owned()
            } else {
                "Đang bỏ khỏi bài đã thích...".to_owned()
            });
            return;
        }
        let added = self.liked.insert(track_id);
        if !added {
            self.liked.remove(&track_id);
        }
        self.toast = Some(if added {
            "Đã thêm vào bài đã thích".to_owned()
        } else {
            "Đã bỏ khỏi bài đã thích".to_owned()
        });
    }

    pub fn like_pending(&self, track_id: TrackId) -> bool {
        self.pending_likes.contains(&track_id)
    }

    pub fn open_add_to_playlist(&mut self, track_id: TrackId) {
        if self.data_mode != DataMode::Live {
            self.toast = Some("Cần kết nối tài khoản để thêm vào danh sách phát".to_owned());
            return;
        }
        if self.user_session.is_none() {
            self.toast = Some("Hãy kết nối lại SoundCloud để sửa danh sách phát".to_owned());
            return;
        }
        if self.catalog.track(track_id).is_none() {
            self.toast = Some("Không có thông tin bài hát".to_owned());
            return;
        }
        if !self
            .library_playlists()
            .iter()
            .any(|playlist| playlist.editable)
        {
            self.toast = Some("Tài khoản này chưa có danh sách phát có thể chỉnh sửa".to_owned());
            return;
        }
        self.add_to_playlist_track = Some(track_id);
    }

    pub const fn add_to_playlist_track(&self) -> Option<TrackId> {
        self.add_to_playlist_track
    }

    pub fn close_add_to_playlist(&mut self) {
        self.add_to_playlist_track = None;
    }

    pub fn add_track_to_playlist(&mut self, playlist_id: PlaylistId) {
        let Some(track_id) = self.add_to_playlist_track else {
            return;
        };
        if self.pending_playlist_add.is_some() {
            return;
        }
        let Some(session) = self.user_session.clone() else {
            self.toast = Some("Hãy kết nối lại SoundCloud để sửa danh sách phát".to_owned());
            return;
        };
        let Some(track_urn) = self
            .catalog
            .track(track_id)
            .and_then(|track| track.urn.clone())
        else {
            self.toast = Some("SoundCloud không trả về mã bài hát".to_owned());
            return;
        };
        let Some(playlist) = self.catalog.playlist(playlist_id) else {
            self.toast = Some("Không có thông tin danh sách phát".to_owned());
            return;
        };
        if !playlist.editable {
            self.toast = Some("Chỉ có thể sửa danh sách phát của chính bạn".to_owned());
            return;
        }
        let Some(playlist_urn) = playlist.urn.clone() else {
            self.toast = Some("SoundCloud không trả về mã danh sách phát".to_owned());
            return;
        };
        self.pending_playlist_add = Some((track_id, playlist_id));
        self.add_to_playlist_track = None;
        self.pending_backend_commands
            .push(BackendCommand::AddTrackToPlaylist {
                session,
                playlist_id,
                playlist_urn,
                track_id,
                track_urn,
            });
        self.toast = Some("Đang thêm bài hát vào danh sách...".to_owned());
    }

    pub fn playlist_add_pending(&self) -> bool {
        self.pending_playlist_add.is_some()
    }

    pub fn toggle_playlist_track_remove_mode(&mut self, playlist_id: PlaylistId) {
        if self.pending_playlist_track_remove.is_some() {
            return;
        }
        let editable = self
            .catalog
            .playlist(playlist_id)
            .is_some_and(|playlist| playlist.editable);
        if !editable {
            self.toast = Some("Chỉ có thể sửa danh sách phát của chính bạn".to_owned());
            return;
        }
        if self.playlist_track_remove_mode == Some(playlist_id) {
            self.playlist_track_remove_mode = None;
        } else {
            self.playlist_track_remove_mode = Some(playlist_id);
            self.toast = Some("Chọn bài hát cần xóa khỏi danh sách".to_owned());
        }
    }

    pub fn playlist_track_remove_mode(&self, playlist_id: PlaylistId) -> bool {
        matches!(self.playlist_track_remove_mode, Some(active) if active == playlist_id)
    }

    pub fn request_remove_track_from_playlist(
        &mut self,
        playlist_id: PlaylistId,
        track_id: TrackId,
        track_index: usize,
    ) {
        if self.pending_playlist_track_remove.is_some() {
            return;
        }
        let Some(playlist) = self.catalog.playlist(playlist_id) else {
            self.toast = Some("Không có thông tin danh sách phát".to_owned());
            return;
        };
        if !playlist.editable {
            self.toast = Some("Chỉ có thể sửa danh sách phát của chính bạn".to_owned());
            return;
        }
        if playlist.track_ids.get(track_index) != Some(&track_id) {
            self.toast = Some("Bài hát đã chọn không còn trong danh sách".to_owned());
            return;
        }
        self.remove_playlist_track = Some((playlist_id, track_id, track_index));
        self.add_to_playlist_track = None;
        self.delete_playlist_id = None;
    }

    pub const fn remove_playlist_track(&self) -> Option<(PlaylistId, TrackId, usize)> {
        self.remove_playlist_track
    }

    pub fn close_remove_track_from_playlist(&mut self) {
        if self.pending_playlist_track_remove.is_none() {
            self.remove_playlist_track = None;
        }
    }

    pub fn confirm_remove_track_from_playlist(&mut self) {
        let Some((playlist_id, track_id, track_index)) = self.remove_playlist_track else {
            return;
        };
        if self.pending_playlist_track_remove.is_some() {
            return;
        }
        let Some(session) = self.user_session.clone() else {
            self.toast = Some("Hãy kết nối lại SoundCloud để sửa danh sách phát".to_owned());
            return;
        };
        let Some(playlist) = self.catalog.playlist(playlist_id) else {
            self.toast = Some("Không có thông tin danh sách phát".to_owned());
            return;
        };
        if !playlist.editable {
            self.toast = Some("Chỉ có thể sửa danh sách phát của chính bạn".to_owned());
            return;
        }
        if playlist.track_ids.get(track_index) != Some(&track_id) {
            self.toast = Some("Danh sách đã thay đổi; hãy mở lại danh sách phát".to_owned());
            return;
        }
        let Some(playlist_urn) = playlist.urn.clone() else {
            self.toast = Some("SoundCloud không trả về mã danh sách phát".to_owned());
            return;
        };
        let Some(track_urn) = self
            .catalog
            .track(track_id)
            .and_then(|track| track.urn.clone())
        else {
            self.toast = Some("SoundCloud không trả về mã bài hát".to_owned());
            return;
        };
        self.pending_playlist_track_remove = Some((playlist_id, track_id, track_index));
        self.pending_backend_commands
            .push(BackendCommand::RemoveTrackFromPlaylist {
                session,
                playlist_id,
                playlist_urn,
                track_id,
                track_urn,
                track_index,
            });
        self.toast = Some("Đang xóa bài hát khỏi danh sách...".to_owned());
    }

    pub fn playlist_track_remove_pending(&self) -> bool {
        self.pending_playlist_track_remove.is_some()
    }

    pub fn open_create_playlist(&mut self) {
        if self.data_mode != DataMode::Live || self.user_session.is_none() {
            self.toast = Some("Hãy kết nối lại SoundCloud để tạo danh sách phát".to_owned());
            return;
        }
        self.new_playlist_title.clear();
        self.create_playlist_open = true;
        self.playlist_track_remove_mode = None;
        self.delete_playlist_id = None;
        self.remove_playlist_track = None;
    }

    pub const fn create_playlist_dialog_open(&self) -> bool {
        self.create_playlist_open
    }

    pub fn new_playlist_title_mut(&mut self) -> &mut String {
        &mut self.new_playlist_title
    }

    pub fn clear_new_playlist_title(&mut self) {
        self.new_playlist_title.clear();
    }

    pub fn close_create_playlist(&mut self) {
        if !self.playlist_create_pending {
            self.create_playlist_open = false;
            self.new_playlist_title.clear();
        }
    }

    pub fn submit_create_playlist(&mut self) {
        if self.playlist_create_pending {
            return;
        }
        let title = self.new_playlist_title.trim().to_owned();
        let title_length = title.chars().count();
        if title_length == 0 {
            self.toast = Some("Hãy nhập tên danh sách phát".to_owned());
            return;
        }
        if title_length > 100 {
            self.toast = Some("Tên danh sách phát không được dài quá 100 ký tự".to_owned());
            return;
        }
        let Some(session) = self.user_session.clone() else {
            self.toast = Some("Hãy kết nối lại SoundCloud để tạo danh sách phát".to_owned());
            return;
        };
        self.playlist_create_pending = true;
        self.pending_backend_commands
            .push(BackendCommand::CreatePlaylist { session, title });
        self.toast = Some("Đang tạo danh sách phát riêng tư...".to_owned());
    }

    pub const fn playlist_create_pending(&self) -> bool {
        self.playlist_create_pending
    }

    pub fn request_delete_playlist(&mut self, playlist_id: PlaylistId) {
        let Some(playlist) = self.catalog.playlist(playlist_id) else {
            self.toast = Some("Không có thông tin danh sách phát".to_owned());
            return;
        };
        if !playlist.editable {
            self.toast = Some("Chỉ có thể xóa danh sách phát của chính bạn".to_owned());
            return;
        }
        self.delete_playlist_id = Some(playlist_id);
        self.create_playlist_open = false;
        self.remove_playlist_track = None;
    }

    pub const fn delete_playlist_id(&self) -> Option<PlaylistId> {
        self.delete_playlist_id
    }

    pub fn close_delete_playlist(&mut self) {
        if self.pending_playlist_delete.is_none() {
            self.delete_playlist_id = None;
        }
    }

    pub fn confirm_delete_playlist(&mut self) {
        let Some(playlist_id) = self.delete_playlist_id else {
            return;
        };
        if self.pending_playlist_delete.is_some() {
            return;
        }
        let Some(session) = self.user_session.clone() else {
            self.toast = Some("Hãy kết nối lại SoundCloud để xóa danh sách phát".to_owned());
            return;
        };
        let Some(playlist) = self.catalog.playlist(playlist_id) else {
            self.toast = Some("Không có thông tin danh sách phát".to_owned());
            return;
        };
        if !playlist.editable {
            self.toast = Some("Chỉ có thể xóa danh sách phát của chính bạn".to_owned());
            return;
        }
        let Some(playlist_urn) = playlist.urn.clone() else {
            self.toast = Some("SoundCloud không trả về mã danh sách phát".to_owned());
            return;
        };
        self.pending_playlist_delete = Some(playlist_id);
        self.pending_backend_commands
            .push(BackendCommand::DeletePlaylist {
                session,
                playlist_id,
                playlist_urn,
            });
        self.toast = Some("Đang xóa danh sách phát...".to_owned());
    }

    pub fn playlist_delete_pending(&self) -> bool {
        self.pending_playlist_delete.is_some()
    }

    /// Closes the topmost account-edit overlay. The TrimUI B button calls this
    /// before page navigation so a confirmation dialog cannot be left behind.
    pub fn dismiss_top_modal(&mut self) -> bool {
        if self.add_to_playlist_track.is_some() {
            self.close_add_to_playlist();
            return true;
        }
        if self.remove_playlist_track.is_some() && self.pending_playlist_track_remove.is_none() {
            self.remove_playlist_track = None;
            return true;
        }
        if self.delete_playlist_id.is_some() && self.pending_playlist_delete.is_none() {
            self.delete_playlist_id = None;
            return true;
        }
        if self.create_playlist_open && !self.playlist_create_pending {
            self.close_create_playlist();
            return true;
        }
        if self.playlist_track_remove_mode.is_some() && self.pending_playlist_track_remove.is_none()
        {
            self.playlist_track_remove_mode = None;
            return true;
        }
        false
    }

    pub fn play_track_from_preview(&mut self, track_id: TrackId) {
        self.dispatch_player(PlayerCommand::PlayTrack {
            track_id,
            context: self.catalog.context_for(track_id),
        });
    }
    /// LIVE selection builds the real queue and starts the supervised audio
    /// backend when the StockOS runtime is available. Preview remains mock.
    pub fn select_track(&mut self, track_id: TrackId) {
        let context = self.catalog.context_for(track_id);
        let start_at = context.iter().position(|id| *id == track_id).unwrap_or(0);
        self.select_track_from_context(context, start_at);
    }

    pub fn select_track_from_context(&mut self, context: Vec<TrackId>, start_at: usize) {
        let Some(&track_id) = context.get(start_at) else {
            self.toast = Some("Bài hát đã chọn không nằm trong bộ sưu tập".to_owned());
            return;
        };
        if self.data_mode == DataMode::Preview {
            self.dispatch_player(PlayerCommand::PlayCollection {
                tracks: context,
                start_at,
            });
            return;
        }
        let selected = self.dispatch_player(PlayerCommand::SelectCollection {
            tracks: context,
            start_at,
        });
        if self.live_playback_enabled {
            if selected {
                self.begin_live_playback();
            }
            return;
        }
        if let Some(track) = self.catalog.track(track_id) {
            self.toast = Some(format!(
                "Đã chọn {}. Hiện chưa thể phát âm thanh.",
                track.title
            ));
        }
    }

    pub fn select_queue_entry(&mut self, entry_id: QueueEntryId) {
        let selected = self.dispatch_player(if self.data_mode == DataMode::Live {
            PlayerCommand::SelectQueueEntry { entry_id }
        } else {
            PlayerCommand::PlayQueueEntry { entry_id }
        });
        if selected && self.data_mode == DataMode::Live && self.live_playback_enabled {
            self.begin_live_playback();
        }
    }
    pub fn play_collection_from_preview(&mut self, start_at: usize) {
        self.dispatch_player(PlayerCommand::PlayCollection {
            tracks: self.home_track_ids(),
            start_at,
        });
    }
    pub fn toggle_playback(&mut self) {
        if self.playback_is_unavailable() {
            self.toast = Some("Hiện chưa thể phát bài hát.".to_owned());
            return;
        }
        if self.player_state().current_track_id.is_none() {
            if self.data_mode == DataMode::Live {
                let tracks = self.home_track_ids();
                if tracks.is_empty() {
                    self.toast = Some("Hãy chọn một bài hát trước".to_owned());
                } else {
                    self.select_track_from_context(tracks, 0);
                }
            } else {
                self.play_collection_from_preview(0);
            }
        } else if self.data_mode == DataMode::Live {
            match self.player_state().playback_status {
                PlaybackStatus::Playing => self.pending_audio_commands.push(AudioCommand::Pause),
                PlaybackStatus::Paused if self.audio_suspended_for_idle => {
                    self.resume_live_playback_after_idle()
                }
                PlaybackStatus::Paused => self.pending_audio_commands.push(AudioCommand::Resume),
                PlaybackStatus::Loading => {}
                PlaybackStatus::Idle | PlaybackStatus::Ended | PlaybackStatus::Error(_) => {
                    self.begin_live_playback();
                }
            }
        } else {
            self.dispatch_player(PlayerCommand::TogglePlayPause);
        }
    }
    /// Starts or resumes playback without pausing an already-playing track.
    /// This is the semantic used by the physical START button.
    pub fn start_playback(&mut self) {
        if self.playback_is_unavailable() {
            self.toast = Some("Hiện chưa thể phát bài hát.".to_owned());
            return;
        }
        if self.player_state().current_track_id.is_none() {
            if self.data_mode == DataMode::Live {
                let tracks = self.home_track_ids();
                if tracks.is_empty() {
                    self.toast = Some("Hãy chọn một bài hát trước".to_owned());
                } else {
                    self.select_track_from_context(tracks, 0);
                }
            } else {
                self.play_collection_from_preview(0);
            }
            return;
        }
        if self.data_mode == DataMode::Live {
            match self.player_state().playback_status {
                PlaybackStatus::Playing | PlaybackStatus::Loading => {}
                PlaybackStatus::Paused if self.audio_suspended_for_idle => {
                    self.resume_live_playback_after_idle()
                }
                PlaybackStatus::Paused => self.pending_audio_commands.push(AudioCommand::Resume),
                PlaybackStatus::Idle | PlaybackStatus::Ended | PlaybackStatus::Error(_) => {
                    self.begin_live_playback();
                }
            }
        } else if !matches!(
            self.player_state().playback_status,
            PlaybackStatus::Playing | PlaybackStatus::Loading
        ) {
            self.dispatch_player(PlayerCommand::TogglePlayPause);
        }
    }
    /// Pauses the current transport without discarding the decoder or current
    /// position. The physical Y/SELECT button uses this so START can resume
    /// immediately without resolving or downloading the track again.
    pub fn pause_playback(&mut self) {
        if self.playback_is_unavailable() {
            self.toast = Some("Hiện chưa thể phát bài hát.".to_owned());
            return;
        }
        if self.player_state().current_track_id.is_none() {
            self.toast = Some("Hãy chọn một bài hát trước".to_owned());
            return;
        }
        if self.data_mode == DataMode::Live {
            if matches!(self.player_state().playback_status, PlaybackStatus::Playing) {
                self.pending_audio_commands.push(AudioCommand::Pause);
                println!("BRICKWAVE_PLAYBACK event=PAUSE_REQUEST source=physical-button");
            }
        } else if matches!(self.player_state().playback_status, PlaybackStatus::Playing) {
            self.dispatch_player(PlayerCommand::TogglePlayPause);
        }
    }
    /// Stops the decoder and resets position while retaining the selected
    /// track and queue. This is reserved for confirmed app exit.
    pub fn stop_playback(&mut self) {
        self.active_stream_request = None;
        self.audio_suspended_for_idle = false;
        self.resume_after_start_seconds = None;
        if self.data_mode == DataMode::Live && self.live_playback_enabled {
            self.pending_audio_commands.push(AudioCommand::Stop);
        }
        self.dispatch_player(PlayerCommand::Stop);
        self.toast = Some("Đã dừng phát nhạc".to_owned());
        println!("BRICKWAVE_PLAYBACK event=STOP_REQUEST");
    }

    /// Releases the StockOS MPlayer process before native display sleep while
    /// retaining the current queue entry and position for a later START.
    pub fn suspend_audio_for_idle(&mut self) -> bool {
        if self.data_mode != DataMode::Live || !self.live_playback_enabled {
            return false;
        }
        let (Some(track_id), Some(entry_id)) = (
            self.player_state().current_track_id,
            self.player_state().current_queue_entry_id,
        ) else {
            return false;
        };
        if !matches!(
            self.player_state().playback_status,
            PlaybackStatus::Playing | PlaybackStatus::Loading | PlaybackStatus::Paused
        ) {
            return false;
        }

        let position = self.player_state().position_seconds;
        self.active_stream_request = None;
        self.pending_audio_commands.push(AudioCommand::Stop);
        let events = self.player.confirm_paused(track_id, entry_id);
        self.observe_events(events);
        self.audio_suspended_for_idle = true;
        self.resume_after_start_seconds = None;
        println!(
            "BRICKWAVE_POWER_AUDIO state=suspended track_id={} entry_id={} position_seconds={position:.3}",
            track_id.get(),
            entry_id.get()
        );
        true
    }
    pub fn next(&mut self) {
        if self.playback_is_unavailable() {
            self.dispatch_player(PlayerCommand::SelectNext);
            self.toast = self
                .current_track()
                .map(|track| format!("Đã chọn {}. Hiện chưa thể phát âm thanh.", track.title));
            return;
        }
        if self.data_mode == DataMode::Live {
            if self.dispatch_player(PlayerCommand::SelectNext) {
                self.begin_live_playback();
            }
        } else {
            self.dispatch_player(PlayerCommand::Next);
        }
    }
    pub fn previous(&mut self) {
        if self.playback_is_unavailable() {
            self.dispatch_player(PlayerCommand::SelectPrevious);
            self.toast = self
                .current_track()
                .map(|track| format!("Đã chọn {}. Hiện chưa thể phát âm thanh.", track.title));
            return;
        }
        if self.data_mode == DataMode::Live {
            if self.dispatch_player(PlayerCommand::SelectPrevious) {
                self.begin_live_playback();
            }
        } else {
            self.dispatch_player(PlayerCommand::Previous);
        }
    }
    pub fn set_shuffle(&mut self, enabled: bool) {
        self.dispatch_player(PlayerCommand::SetShuffle(enabled));
        self.toast = Some(if enabled {
            "Đã bật phát ngẫu nhiên".to_owned()
        } else {
            "Đã tắt phát ngẫu nhiên".to_owned()
        });
    }
    pub fn cycle_repeat(&mut self) {
        self.dispatch_player(PlayerCommand::CycleRepeat);
        self.toast = Some(match self.player_state().repeat_mode {
            RepeatMode::Off => "Đã tắt lặp lại".to_owned(),
            RepeatMode::All => "Đã bật lặp lại tất cả".to_owned(),
            RepeatMode::One => "Đã bật lặp lại một bài".to_owned(),
        });
    }
    pub fn set_volume(&mut self, volume: f32) {
        self.dispatch_player(PlayerCommand::SetVolume(volume));
        if self.data_mode == DataMode::Live && self.live_playback_enabled {
            self.pending_audio_commands
                .push(AudioCommand::SetVolume(volume));
        }
    }
    pub fn seek_progress(&mut self, progress: f32) {
        if self.playback_is_unavailable() {
            self.toast = Some("Hiện chưa thể phát bài hát.".to_owned());
            return;
        }
        let duration = self.player_state().duration_seconds.unwrap_or(0.0);
        let seconds = duration * progress.clamp(0.0, 1.0);
        self.dispatch_player(PlayerCommand::Seek { seconds });
        if self.data_mode == DataMode::Live {
            self.pending_audio_commands
                .push(AudioCommand::Seek(seconds));
        }
    }
    pub fn progress(&self) -> f32 {
        let state = self.player_state();
        let duration = state.duration_seconds.unwrap_or(0.0);
        if duration > 0.0 {
            (state.position_seconds / duration).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
    pub fn queue(&self) -> &[QueueEntry] {
        &self.player_state().queue
    }
    pub fn advance_preview(&mut self, elapsed_seconds: f32) {
        if self.data_mode != DataMode::Preview {
            return;
        }
        let events = self.mock_engine.advance(&mut self.player, elapsed_seconds);
        self.observe_events(events);
    }

    pub fn take_audio_commands(&mut self) -> Vec<AudioCommand> {
        std::mem::take(&mut self.pending_audio_commands)
    }

    pub fn apply_playback_engine_event(&mut self, event: PlaybackEngineEvent) {
        match event {
            PlaybackEngineEvent::Started { track_id, entry_id } => {
                if self.audio_suspended_for_idle {
                    self.pending_audio_commands.push(AudioCommand::Stop);
                    println!(
                        "BRICKWAVE_POWER_AUDIO state=late-start-ignored track_id={} entry_id={}",
                        track_id.get(),
                        entry_id.get()
                    );
                    return;
                }
                let events = self.player.confirm_playing(track_id, entry_id);
                self.observe_events(events);
                if let Some(seconds) = self.resume_after_start_seconds.take() {
                    let events = self.player.update_position(track_id, entry_id, seconds);
                    self.observe_events(events);
                    self.pending_audio_commands
                        .push(AudioCommand::Seek(seconds));
                    println!(
                        "BRICKWAVE_POWER_AUDIO state=resume-seek track_id={} position_seconds={seconds:.3}",
                        track_id.get()
                    );
                }
                if self.player_state().current_queue_entry_id == Some(entry_id) {
                    self.toast = self
                        .catalog
                        .track(track_id)
                        .map(|track| format!("Đang phát {}", track.title));
                }
            }
            PlaybackEngineEvent::Paused { track_id, entry_id } => {
                let events = self.player.confirm_paused(track_id, entry_id);
                self.observe_events(events);
                if self.player_state().current_queue_entry_id == Some(entry_id) {
                    self.toast = Some("Đã tạm dừng".to_owned());
                }
            }
            PlaybackEngineEvent::Position {
                track_id,
                entry_id,
                seconds,
            } => {
                if self.audio_suspended_for_idle {
                    return;
                }
                let events = self.player.update_position(track_id, entry_id, seconds);
                self.observe_events(events);
            }
            PlaybackEngineEvent::Ended { track_id, entry_id } => {
                if self.audio_suspended_for_idle {
                    return;
                }
                if self.player_state().current_track_id != Some(track_id)
                    || self.player_state().current_queue_entry_id != Some(entry_id)
                {
                    return;
                }
                let events = self.player.dispatch(PlayerCommand::PlaybackFinished);
                self.observe_events(events);
                if matches!(self.player_state().playback_status, PlaybackStatus::Playing) {
                    self.begin_live_playback();
                }
            }
            PlaybackEngineEvent::Error {
                track_id,
                entry_id,
                message,
            } => {
                if track_id.is_some() && track_id != self.player_state().current_track_id {
                    return;
                }
                if entry_id.is_some() && entry_id != self.player_state().current_queue_entry_id {
                    return;
                }
                self.active_stream_request = None;
                self.audio_suspended_for_idle = false;
                self.resume_after_start_seconds = None;
                self.pending_audio_commands.push(AudioCommand::Stop);
                let events = self.player.playback_error(message);
                self.observe_events(events);
            }
        }
    }

    fn begin_live_playback(&mut self) {
        self.begin_live_playback_at(None);
    }

    fn resume_live_playback_after_idle(&mut self) {
        let position = self.player_state().position_seconds;
        self.audio_suspended_for_idle = false;
        self.begin_live_playback_at(Some(position));
    }

    fn begin_live_playback_at(&mut self, resume_position: Option<f32>) {
        if self.data_mode != DataMode::Live || !self.live_playback_enabled {
            return;
        }
        let Some(session) = self.user_session.clone() else {
            let events = self
                .player
                .playback_error("Hãy đăng nhập trước khi phát nhạc");
            self.observe_events(events);
            return;
        };
        let (Some(track_id), Some(entry_id)) = (
            self.player_state().current_track_id,
            self.player_state().current_queue_entry_id,
        ) else {
            return;
        };
        let Some(track) = self.catalog.track(track_id) else {
            let events = self.player.playback_error("Không có thông tin bài hát");
            self.observe_events(events);
            return;
        };
        let Some(track_urn) = track.urn.clone() else {
            let events = self
                .player
                .playback_error("Bài hát không có mã phát nhạc SoundCloud");
            self.observe_events(events);
            return;
        };
        if track.availability != PlaybackAvailability::Available {
            let events = self.player.playback_error("Không thể phát bài hát này");
            self.observe_events(events);
            return;
        }
        self.audio_suspended_for_idle = false;
        self.resume_after_start_seconds = resume_position.filter(|seconds| *seconds > 0.05);
        self.pending_audio_commands.push(AudioCommand::Stop);
        let events = self.player.begin_loading();
        self.observe_events(events);
        if let Some(seconds) = self.resume_after_start_seconds {
            let events = self.player.update_position(track_id, entry_id, seconds);
            self.observe_events(events);
        }
        self.next_stream_request_id = self.next_stream_request_id.wrapping_add(1).max(1);
        let request_id = self.next_stream_request_id;
        self.active_stream_request = Some((request_id, track_id, entry_id));
        println!(
            "BRICKWAVE_PLAYBACK event=PLAY_REQUEST track_id={} entry_id={} request_id={}",
            track_id.get(),
            entry_id.get(),
            request_id
        );
        self.pending_backend_commands
            .push(BackendCommand::GetStreamDescriptor {
                session,
                request_id,
                track_id,
                track_urn,
            });
        self.toast = Some("Đang tải âm thanh...".to_owned());
    }
    pub fn open_now_playing(&mut self) {
        self.navigate_main(Page::NowPlaying);
    }
    pub fn close_now_playing(&mut self) {
        self.navigate_back();
    }

    pub fn submit_search(&mut self) {
        let query = self.query.trim().to_owned();
        self.submitted_query = query.clone();
        self.navigate_main(Page::Search);
        if query.is_empty() {
            self.active_search_request_id = None;
            self.search_next_cursor = None;
            self.search_loading_more = false;
            self.catalog.clear_search();
            self.search_status = SearchStatus::Idle;
            self.toast = Some("Hãy nhập nội dung cần tìm".into());
        } else if self.data_mode == DataMode::Preview {
            self.active_search_request_id = None;
            self.search_next_cursor = None;
            self.search_loading_more = false;
            let count = self.catalog.search_matches(&self.submitted_query).len();
            self.search_status = if count == 0 {
                SearchStatus::Empty
            } else {
                SearchStatus::Results { count }
            };
            self.toast = Some("Đang tìm trong dữ liệu xem trước".into());
        } else {
            self.next_search_request_id = self.next_search_request_id.wrapping_add(1);
            if self.next_search_request_id == 0 {
                self.next_search_request_id = 1;
            }
            let request_id = self.next_search_request_id;
            self.active_search_request_id = Some(request_id);
            self.search_next_cursor = None;
            self.search_loading_more = false;
            self.catalog.clear_search();
            self.search_status = SearchStatus::Loading;
            self.pending_backend_commands
                .push(BackendCommand::search_tracks_with_request_id(
                    query, request_id,
                ));
            self.toast = None;
        }
    }

    pub fn load_more_search_results(&mut self) {
        if self.data_mode != DataMode::Live
            || !matches!(self.search_status, SearchStatus::Results { .. })
            || self.search_loading_more
            || self.active_search_request_id.is_some()
        {
            return;
        }
        let Some(cursor) = self.search_next_cursor.clone() else {
            return;
        };
        self.next_search_request_id = self.next_search_request_id.wrapping_add(1);
        if self.next_search_request_id == 0 {
            self.next_search_request_id = 1;
        }
        let request_id = self.next_search_request_id;
        self.active_search_request_id = Some(request_id);
        self.search_loading_more = true;
        self.pending_backend_commands
            .push(BackendCommand::search_more_with_request_id(
                self.submitted_query.clone(),
                cursor,
                request_id,
            ));
    }

    pub const fn search_loading_more(&self) -> bool {
        self.search_loading_more
    }

    pub fn search_has_more(&self) -> bool {
        self.search_next_cursor.is_some()
    }
    pub fn clear_search_input(&mut self) {
        self.query.clear();
    }
    pub fn take_backend_commands(&mut self) -> Vec<BackendCommand> {
        std::mem::take(&mut self.pending_backend_commands)
    }

    /// Applies backend output to the one shared catalog. Artwork URLs remain
    /// metadata only; this method never changes artwork policy.
    pub fn apply_backend_event(&mut self, event: BackendEvent) {
        match event {
            BackendEvent::SearchResults {
                request_id,
                query,
                tracks,
                playlists,
                next_cursor,
                append,
            } => {
                let tracks: Vec<_> = tracks.into_iter().map(Track::from).collect();
                let active = self.data_mode != DataMode::Live
                    || self.active_search_request_id == Some(request_id);
                if !active {
                    // Retain valid metadata without allowing an old result to
                    // replace the query, result list, loading, or error state.
                    self.catalog.upsert_tracks(tracks);
                    for playlist in playlists {
                        self.catalog.upsert_playlist(playlist, Vec::new());
                    }
                    self.catalog.sync_player(&mut self.player);
                    return;
                }

                self.submitted_query = query;
                if append {
                    self.catalog.append_search(tracks, playlists);
                } else {
                    self.catalog.replace_search(tracks, playlists);
                }
                self.catalog.sync_player(&mut self.player);
                let count =
                    self.catalog.search_track_ids.len() + self.catalog.search_playlist_ids.len();
                self.search_status = if count == 0 {
                    SearchStatus::Empty
                } else {
                    SearchStatus::Results { count }
                };
                self.active_search_request_id = None;
                self.search_next_cursor = next_cursor;
                self.search_loading_more = false;
            }
            BackendEvent::TrackLoaded { track } => {
                self.catalog.upsert_tracks([Track::from(track)]);
                self.catalog.sync_player(&mut self.player);
            }
            BackendEvent::PlaylistLoaded { playlist, tracks } => {
                self.catalog
                    .upsert_playlist(playlist, tracks.into_iter().map(Track::from).collect());
                self.catalog.sync_player(&mut self.player);
            }
            BackendEvent::PlaylistTracksLoaded {
                playlist_id,
                tracks,
            } => {
                let empty = tracks.is_empty();
                self.catalog.replace_playlist_tracks(
                    playlist_id,
                    tracks.into_iter().map(Track::from).collect(),
                );
                self.catalog.sync_player(&mut self.player);
                if self.selected_playlist_id == Some(playlist_id) {
                    self.playlist_status = if empty {
                        PlaylistStatus::Empty
                    } else {
                        PlaylistStatus::Loaded
                    };
                }
            }
            BackendEvent::StreamDescriptorLoaded { descriptor } => {
                let Some((request_id, track_id, entry_id)) = self.active_stream_request else {
                    return;
                };
                if descriptor.request_id != request_id
                    || descriptor.track_id != track_id
                    || self.player_state().current_track_id != Some(track_id)
                    || self.player_state().current_queue_entry_id != Some(entry_id)
                {
                    return;
                }
                let format = descriptor.format.clone();
                let media_url = descriptor.into_media_url();
                self.active_stream_request = None;
                println!(
                    "BRICKWAVE_PLAYBACK event=STREAM_DESCRIPTOR_READY track_id={} format={}",
                    track_id.get(),
                    format
                );
                self.pending_audio_commands.push(AudioCommand::Load {
                    track_id,
                    entry_id,
                    format,
                    media_url,
                    volume: self.player_state().volume,
                    liked: self.is_liked(track_id),
                });
            }
            BackendEvent::ProfileLoaded { profile } => {
                self.catalog.profile = Some(profile.into());
            }
            BackendEvent::QrLoginStarted { login } => {
                self.auth_poll_in_flight = false;
                if self.is_login_route() && self.auth_state == AuthState::CreatingQr {
                    self.auth_state = AuthState::WaitingForScan;
                    self.auth_error = None;
                    self.qr_login = Some(login);
                } else {
                    // The user left LoginScreen before the worker completed
                    // creation. Revoke the late session and never start polling.
                    self.pending_backend_commands
                        .push(BackendCommand::CancelQrLogin { login });
                }
            }
            BackendEvent::QrLoginPending {
                phase,
                expires_at_ms,
                confirmation_code,
            } => {
                self.auth_poll_in_flight = false;
                if self.is_login_route() {
                    if let Some(login) = self.qr_login.as_mut() {
                        login.expires_at_ms = expires_at_ms;
                        login.confirmation_code = confirmation_code;
                    }
                    self.auth_state = match phase {
                        QrLoginPhase::WaitingForScan => AuthState::WaitingForScan,
                        QrLoginPhase::WaitingForAuthorization => AuthState::WaitingForAuthorization,
                    };
                    self.auth_error = None;
                }
            }
            BackendEvent::PairingConfirmationRequired { login } => {
                self.auth_poll_in_flight = false;
                if self.is_login_route() {
                    self.qr_login = Some(login);
                    self.auth_state = AuthState::PairingConfirmation;
                    self.auth_error = None;
                } else {
                    self.pending_backend_commands
                        .push(BackendCommand::CancelQrLogin { login });
                }
            }
            BackendEvent::QrLoginAuthorized { session, profile } => {
                self.auth_poll_in_flight = false;
                self.pending_backend_commands
                    .push(BackendCommand::GetCurrentUser {
                        session: session.clone(),
                    });
                self.pending_backend_commands
                    .push(BackendCommand::GetUserLibrary {
                        session: session.clone(),
                    });
                self.library_status = LibraryStatus::Loading;
                self.user_session = Some(session);
                self.catalog.profile = Some(profile.into());
                // Dropping this value clears the device proof and pairing URL
                // from client memory once the app session exists.
                self.qr_login = None;
                self.auth_state = AuthState::Authorized;
                self.auth_error = None;
                self.navigate_main(Page::Home);
                self.toast = Some("Đã kết nối tài khoản SoundCloud".to_owned());
            }
            BackendEvent::QrLoginExpired => {
                self.auth_poll_in_flight = false;
                self.qr_login = None;
                self.auth_state = AuthState::TokenExpired;
                self.auth_error = Some("Mã ghép nối đã hết hạn".to_owned());
            }
            BackendEvent::QrLoginError { failure } => {
                self.auth_poll_in_flight = false;
                self.qr_login = None;
                self.auth_state = if failure == crate::backend::BackendFailure::PairingCancelled {
                    AuthState::Cancelled
                } else if failure == crate::backend::BackendFailure::PairingExpired {
                    AuthState::TokenExpired
                } else {
                    AuthState::LoginFailed
                };
                self.auth_error = Some(failure.user_message().to_owned());
            }
            BackendEvent::CurrentUserLoaded { profile } => {
                self.catalog.profile = Some(profile.into());
                self.auth_state = AuthState::Authorized;
                self.auth_error = None;
                if self.library_status == LibraryStatus::Idle {
                    self.refresh_user_library();
                }
                self.navigate_main(Page::Home);
            }
            BackendEvent::UserLibraryLoaded {
                liked_tracks,
                playlists,
                home_tracks,
                discover_tracks,
            } => {
                let liked_tracks: Vec<_> = liked_tracks.into_iter().map(Track::from).collect();
                let home_tracks: Vec<_> = home_tracks.into_iter().map(Track::from).collect();
                let discover_tracks: Vec<_> =
                    discover_tracks.into_iter().map(Track::from).collect();
                let empty = liked_tracks.is_empty()
                    && playlists.is_empty()
                    && home_tracks.is_empty()
                    && discover_tracks.is_empty();
                self.catalog.replace_user_library(
                    liked_tracks,
                    playlists,
                    home_tracks,
                    discover_tracks,
                );
                self.catalog.sync_player(&mut self.player);
                self.library_status = if empty {
                    LibraryStatus::Empty
                } else {
                    LibraryStatus::Loaded
                };
            }
            BackendEvent::TrackLikeUpdated { track_id, liked } => {
                self.pending_likes.remove(&track_id);
                self.catalog.set_liked(track_id, liked);
                if self.live_playback_enabled {
                    self.pending_audio_commands
                        .push(AudioCommand::SetCachePriority { track_id, liked });
                }
                self.toast = Some(if liked {
                    "Đã thêm vào bài đã thích".to_owned()
                } else {
                    "Đã bỏ khỏi bài đã thích".to_owned()
                });
            }
            BackendEvent::TrackAddedToPlaylist {
                playlist_id,
                track_id,
                track_count,
            } => {
                self.pending_playlist_add = None;
                self.catalog
                    .add_track_to_playlist(playlist_id, track_id, track_count);
                self.toast = Some("Đã thêm bài hát vào danh sách phát".to_owned());
            }
            BackendEvent::TrackRemovedFromPlaylist {
                playlist_id,
                track_id,
                track_index,
                track_count,
            } => {
                self.pending_playlist_track_remove = None;
                self.remove_playlist_track = None;
                self.catalog.remove_track_from_playlist(
                    playlist_id,
                    track_id,
                    track_index,
                    track_count,
                );
                if self.selected_playlist_id == Some(playlist_id) {
                    self.playlist_status = if track_count == 0 {
                        PlaylistStatus::Empty
                    } else {
                        PlaylistStatus::Loaded
                    };
                }
                if track_count == 0 {
                    self.playlist_track_remove_mode = None;
                }
                self.toast = Some("Đã xóa bài hát khỏi danh sách phát".to_owned());
            }
            BackendEvent::PlaylistCreated { playlist } => {
                self.playlist_create_pending = false;
                self.create_playlist_open = false;
                self.new_playlist_title.clear();
                self.catalog.add_created_playlist(playlist);
                self.library_tab = LibraryTab::Playlists;
                self.library_status = LibraryStatus::Loaded;
                self.toast = Some("Đã tạo danh sách phát riêng tư".to_owned());
            }
            BackendEvent::PlaylistDeleted { playlist_id } => {
                self.pending_playlist_delete = None;
                self.delete_playlist_id = None;
                self.catalog.remove_playlist(playlist_id);
                if self.selected_playlist_id == Some(playlist_id) {
                    self.selected_playlist_id = None;
                    self.playlist_track_remove_mode = None;
                    self.playlist_status = PlaylistStatus::Idle;
                    self.navigation_stack.retain(|page| *page != Page::Playlist);
                    self.route = AppRoute::Main(Page::Library);
                    self.library_tab = LibraryTab::Playlists;
                    self.queue_open = false;
                }
                self.toast = Some("Đã xóa danh sách phát".to_owned());
            }
            BackendEvent::LoggedOut => {
                self.active_stream_request = None;
                self.pending_audio_commands.push(AudioCommand::Stop);
                self.pending_audio_commands
                    .push(AudioCommand::ClearSessionCache);
                self.auth_poll_in_flight = false;
                self.user_session = None;
                self.qr_login = None;
                self.catalog.profile = None;
                self.catalog.clear_user_library();
                self.library_status = LibraryStatus::Idle;
                self.playlist_status = PlaylistStatus::Idle;
                self.selected_playlist_id = None;
                self.playlist_track_remove_mode = None;
                self.pending_likes.clear();
                self.pending_playlist_add = None;
                self.add_to_playlist_track = None;
                self.remove_playlist_track = None;
                self.pending_playlist_track_remove = None;
                self.create_playlist_open = false;
                self.new_playlist_title.clear();
                self.playlist_create_pending = false;
                self.delete_playlist_id = None;
                self.pending_playlist_delete = None;
                self.auth_state = AuthState::LoggedOut;
                self.auth_error = None;
                self.navigation_stack.clear();
                self.route = AppRoute::Login {
                    return_page: Page::Home,
                };
                self.queue_open = false;
                self.toast = Some("Đã ngắt kết nối tài khoản SoundCloud".to_owned());
            }
            BackendEvent::ArtworkAvailable { .. } => {}
            BackendEvent::BackendError {
                operation,
                request_id,
                failure,
            } => {
                let restore_validation_failed =
                    operation == BackendOperation::GetCurrentUser && self.is_restoring_route();
                if operation == crate::backend::BackendOperation::SearchTracks
                    && self.data_mode == DataMode::Live
                {
                    if request_id != self.active_search_request_id {
                        return;
                    }
                    if !self.search_loading_more {
                        self.search_status = SearchStatus::Error;
                    }
                    self.active_search_request_id = None;
                    self.search_loading_more = false;
                }
                match operation {
                    BackendOperation::StartQrLogin => {
                        self.auth_poll_in_flight = false;
                        self.qr_login = None;
                        self.auth_state = AuthState::LoginFailed;
                        self.auth_error = Some(failure.user_message().to_owned());
                    }
                    BackendOperation::PollQrLogin => {
                        self.auth_poll_in_flight = false;
                        if failure == crate::backend::BackendFailure::PairingExpired {
                            self.qr_login = None;
                            self.auth_state = AuthState::TokenExpired;
                        } else if failure == crate::backend::BackendFailure::PairingCancelled {
                            self.qr_login = None;
                            self.auth_state = AuthState::Cancelled;
                        } else if !matches!(
                            failure,
                            crate::backend::BackendFailure::RateLimited
                                | crate::backend::BackendFailure::Timeout
                                | crate::backend::BackendFailure::Network
                        ) {
                            self.qr_login = None;
                            self.auth_state = AuthState::LoginFailed;
                        }
                        // A transient status failure keeps the active QR and its
                        // polling state. The next scheduled poll can recover.
                        self.auth_error = Some(failure.user_message().to_owned());
                    }
                    BackendOperation::ConfirmPairing => {
                        if matches!(
                            failure,
                            crate::backend::BackendFailure::RateLimited
                                | crate::backend::BackendFailure::Timeout
                                | crate::backend::BackendFailure::Network
                        ) {
                            self.auth_state = AuthState::PairingConfirmation;
                        } else {
                            self.qr_login = None;
                            self.auth_state = AuthState::LoginFailed;
                        }
                        self.auth_error = Some(failure.user_message().to_owned());
                    }
                    BackendOperation::CancelQrLogin => {
                        // The server session will still expire after its short
                        // TTL if cancellation could not reach the Worker. Drop
                        // the local device proof and stop polling immediately.
                        self.qr_login = None;
                        self.auth_state = AuthState::Cancelled;
                        self.auth_error = Some(failure.user_message().to_owned());
                    }
                    BackendOperation::GetCurrentUser => {
                        if matches!(
                            failure,
                            crate::backend::BackendFailure::Unauthorized
                                | crate::backend::BackendFailure::Forbidden
                                | crate::backend::BackendFailure::TokenExpired
                                | crate::backend::BackendFailure::RefreshRequired
                        ) {
                            self.active_stream_request = None;
                            self.pending_audio_commands.push(AudioCommand::Stop);
                            self.pending_audio_commands
                                .push(AudioCommand::ClearSessionCache);
                            self.user_session = None;
                            self.catalog.profile = None;
                            self.catalog.clear_user_library();
                            self.library_status = LibraryStatus::Idle;
                            self.playlist_status = PlaylistStatus::Idle;
                            self.selected_playlist_id = None;
                            self.auth_state = AuthState::TokenExpired;
                            self.navigation_stack.clear();
                            self.route = AppRoute::Login {
                                return_page: Page::Home,
                            };
                            self.queue_open = false;
                        } else {
                            // A saved session may enter Main only after a
                            // successful `/auth/me` response. An already
                            // verified in-memory session can remain active on a
                            // transient refresh failure.
                            if self.auth_state != AuthState::Authorized {
                                self.auth_state = AuthState::LoginFailed;
                                if !self.is_restoring_route() {
                                    self.route = AppRoute::Login {
                                        return_page: Page::Home,
                                    };
                                }
                                self.queue_open = false;
                            }
                        }
                        self.auth_error = Some(failure.user_message().to_owned());
                    }
                    BackendOperation::GetUserLibrary => {
                        self.library_status = LibraryStatus::Error;
                        if matches!(
                            failure,
                            crate::backend::BackendFailure::Unauthorized
                                | crate::backend::BackendFailure::Forbidden
                                | crate::backend::BackendFailure::TokenExpired
                                | crate::backend::BackendFailure::RefreshRequired
                        ) {
                            self.active_stream_request = None;
                            self.pending_audio_commands.push(AudioCommand::Stop);
                            self.pending_audio_commands
                                .push(AudioCommand::ClearSessionCache);
                            self.user_session = None;
                            self.catalog.profile = None;
                            self.catalog.clear_user_library();
                            self.playlist_status = PlaylistStatus::Idle;
                            self.selected_playlist_id = None;
                            self.auth_state = AuthState::TokenExpired;
                            self.auth_error = Some(failure.user_message().to_owned());
                            self.navigation_stack.clear();
                            self.route = AppRoute::Login {
                                return_page: Page::Home,
                            };
                            self.queue_open = false;
                        }
                    }
                    BackendOperation::SetTrackLiked => {
                        self.pending_likes.clear();
                    }
                    BackendOperation::AddTrackToPlaylist => {
                        self.pending_playlist_add = None;
                    }
                    BackendOperation::RemoveTrackFromPlaylist => {
                        self.pending_playlist_track_remove = None;
                    }
                    BackendOperation::CreatePlaylist => {
                        self.playlist_create_pending = false;
                    }
                    BackendOperation::DeletePlaylist => {
                        self.pending_playlist_delete = None;
                    }
                    BackendOperation::GetPlaylistTracks => {
                        if self.main_page() == Page::Playlist {
                            self.playlist_status = PlaylistStatus::Error;
                        }
                    }
                    BackendOperation::GetStreamDescriptor => {
                        if request_id
                            == self
                                .active_stream_request
                                .map(|(active_request_id, _, _)| active_request_id)
                        {
                            if let Some((active_request_id, track_id, entry_id)) =
                                self.active_stream_request
                            {
                                println!(
                                    "BRICKWAVE_PLAYBACK event=STREAM_DESCRIPTOR_ERROR track_id={} entry_id={} request_id={} reason={:?}",
                                    track_id.get(),
                                    entry_id.get(),
                                    active_request_id,
                                    failure
                                );
                            }
                            self.active_stream_request = None;
                            self.pending_audio_commands.push(AudioCommand::Stop);
                            let events = self
                                .player
                                .playback_error(failure.user_message().to_owned());
                            self.observe_events(events);
                        }
                    }
                    BackendOperation::Logout => {
                        // Do not discard a valid proof until server-side
                        // invalidation succeeds. The user can retry Logout.
                        self.auth_state = AuthState::Authorized;
                        self.auth_error = Some(failure.user_message().to_owned());
                    }
                    BackendOperation::SearchTracks
                    | BackendOperation::GetTrack
                    | BackendOperation::GetPlaylist
                    | BackendOperation::GetPublicProfile => {}
                }
                self.toast = if restore_validation_failed {
                    None
                } else {
                    Some(failure.user_message().to_owned())
                };
            }
        }
    }
    pub fn search_results(&self) -> Vec<TrackId> {
        if self.data_mode == DataMode::Live {
            return match self.search_status {
                SearchStatus::Results { .. } | SearchStatus::Empty => {
                    self.catalog.search_track_ids.clone()
                }
                SearchStatus::Idle | SearchStatus::Loading | SearchStatus::Error => Vec::new(),
            };
        }
        if self.submitted_query.is_empty() || self.catalog.search_track_ids.is_empty() {
            self.catalog.search_matches(&self.submitted_query)
        } else {
            self.catalog.search_track_ids.clone()
        }
    }

    pub fn search_playlists(&self) -> Vec<Playlist> {
        if self.data_mode != DataMode::Live
            || !matches!(
                self.search_status,
                SearchStatus::Results { .. } | SearchStatus::Empty
            )
        {
            return Vec::new();
        }
        self.catalog
            .search_playlist_ids
            .iter()
            .filter_map(|id| self.catalog.playlist(*id).cloned())
            .collect()
    }

    fn dispatch_player(&mut self, command: PlayerCommand) -> bool {
        let events = self.player.dispatch(command);
        let handled = !events.is_empty();
        self.observe_events(events);
        handled
    }
    fn observe_events(&mut self, events: Vec<PlayerEvent>) {
        for event in events {
            if self.data_mode == DataMode::Live {
                match event {
                    PlayerEvent::PlaybackError { message } => self.toast = Some(message),
                    PlayerEvent::TrackLoading { .. }
                    | PlayerEvent::TrackPlaying { .. }
                    | PlayerEvent::TrackPaused { .. }
                    | PlayerEvent::TrackStopped { .. }
                    | PlayerEvent::TrackEnded { .. }
                    | PlayerEvent::PositionChanged { .. }
                    | PlayerEvent::QueueChanged => {}
                }
                continue;
            }
            match event {
                PlayerEvent::TrackPlaying { track_id, .. } => {
                    if let Some(track) = self.catalog.track(track_id) {
                        self.toast = Some(format!("Đang phát thử {}", track.title));
                    }
                }
                PlayerEvent::TrackPaused { .. } => {
                    self.toast = Some("Đã tạm dừng bản phát thử".to_owned())
                }
                PlayerEvent::TrackStopped { .. } => {
                    self.toast = Some("Đã dừng bản phát thử".to_owned())
                }
                PlayerEvent::TrackEnded { .. } => {
                    self.toast = Some("Bản phát thử đã kết thúc".to_owned())
                }
                PlayerEvent::PlaybackError { message } => self.toast = Some(message),
                PlayerEvent::TrackLoading { .. }
                | PlayerEvent::PositionChanged { .. }
                | PlayerEvent::QueueChanged => {}
            }
        }
    }
}

fn preview_tracks() -> Vec<Track> {
    [
        (
            "Afterglow",
            "Mira Lane",
            222,
            "Ambient pop",
            [255, 85, 0],
            Some(ArtworkId::NightDrive),
        ),
        (
            "Neon Weather",
            "Orchid State",
            248,
            "Electronic",
            [115, 111, 255],
            None,
        ),
        (
            "Slow Current",
            "Nico Sato",
            178,
            "Lo-fi",
            [0, 191, 166],
            None,
        ),
        (
            "Satellite Hearts",
            "Velvet Echo",
            206,
            "Indie dance",
            [238, 90, 132],
            None,
        ),
        (
            "Morning Tapes",
            "June Assembly",
            271,
            "Downtempo",
            [244, 190, 70],
            None,
        ),
        ("Blue Hour", "Vela", 195, "Dream pop", [68, 146, 255], None),
        (
            "Still Moving",
            "Kite Theory",
            231,
            "House",
            [176, 103, 255],
            None,
        ),
        (
            "Window Seat",
            "Lantern Club",
            167,
            "Alternative",
            [86, 197, 113],
            None,
        ),
        (
            "Last Signal",
            "Nora Bloom",
            243,
            "Synthwave",
            [255, 108, 77],
            None,
        ),
        (
            "Soft Focus",
            "Low Mercury",
            213,
            "Chill",
            [43, 180, 196],
            Some(ArtworkId::SoftFocus),
        ),
    ]
    .into_iter()
    .enumerate()
    .map(
        |(index, (title, artist, duration_seconds, mood, tint, artwork))| Track {
            id: TrackId::new(10_001 + index as u64),
            urn: None,
            title: title.to_owned(),
            artist: artist.to_owned(),
            duration_seconds: Some(duration_seconds),
            artwork_url: None,
            waveform_url: None,
            availability: PlaybackAvailability::Available,
            mood: mood.to_owned(),
            tint,
            artwork,
        },
    )
    .collect()
}

fn tint_from_id(id: TrackId) -> [u8; 3] {
    const TINTS: [[u8; 3]; 6] = [
        [255, 85, 0],
        [115, 111, 255],
        [0, 191, 166],
        [238, 90, 132],
        [244, 190, 70],
        [68, 146, 255],
    ];
    TINTS[id.get() as usize % TINTS.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleep_timer_pauses_without_resetting_the_selected_track() {
        let mut state = UiState::default();
        state.play_collection_from_preview(0);
        let track_id = state.player_state().current_track_id;
        let queue_entry = state.player_state().current_queue_entry_id;
        let now = Instant::now();
        *state.stop_timer_hours_mut() = "0".to_owned();
        *state.stop_timer_minutes_mut() = "1".to_owned();

        assert!(state.set_stop_timer_at(now));
        state.service_stop_timer_at(now + Duration::from_secs(59));
        assert_eq!(
            state.player_state().playback_status,
            PlaybackStatus::Playing
        );
        state.service_stop_timer_at(now + Duration::from_secs(60));

        assert_eq!(state.player_state().playback_status, PlaybackStatus::Paused);
        assert_eq!(state.player_state().current_track_id, track_id);
        assert_eq!(state.player_state().current_queue_entry_id, queue_entry);
        assert!(
            state
                .stop_timer_remaining_at(now + Duration::from_secs(60))
                .is_none()
        );
    }

    #[test]
    fn sleep_timer_rejects_zero_and_out_of_range_values() {
        let mut state = UiState::default();
        *state.stop_timer_hours_mut() = "0".to_owned();
        *state.stop_timer_minutes_mut() = "0".to_owned();
        assert!(!state.set_stop_timer_at(Instant::now()));

        *state.stop_timer_hours_mut() = "24".to_owned();
        *state.stop_timer_minutes_mut() = "00".to_owned();
        assert!(!state.set_stop_timer_at(Instant::now()));

        *state.stop_timer_hours_mut() = "ab1".to_owned();
        *state.stop_timer_minutes_mut() = "2x".to_owned();
        state.sanitize_stop_timer_inputs();
        assert_eq!(state.stop_timer_hours, "1");
        assert_eq!(state.stop_timer_minutes, "2");
    }

    #[test]
    fn preview_metadata_has_stable_ids_and_optional_fields() {
        let state = UiState::default();
        assert_ne!(state.preview_track(0).id, state.preview_track(1).id);
        assert_eq!(state.preview_track(0).duration_seconds, Some(222));
        assert!(state.preview_track(1).artwork_url.is_none());
        assert_eq!(state.preview_track(0).artwork, Some(ArtworkId::NightDrive));
    }
    #[test]
    fn every_screen_reads_the_same_player_controller() {
        let mut state = UiState::default();
        state.play_collection_from_preview(4);
        assert_eq!(state.current_track().unwrap().id, state.preview_track(4).id);
        state.navigate_main(Page::NowPlaying);
        state.next();
        assert_eq!(state.current_track().unwrap().id, state.preview_track(5).id);
        state.navigate_main(Page::Home);
        assert!(state.is_current_track(state.preview_track(5).id));
        assert!(state.is_playing());
    }

    #[test]
    fn physical_pause_and_start_resume_the_same_position_and_queue() {
        let mut state = UiState::default();
        state.play_collection_from_preview(0);
        let selected = state.player_state().current_queue_entry_id;
        let queue_len = state.queue().len();
        state.dispatch_player(PlayerCommand::Seek { seconds: 42.0 });

        state.start_playback();
        assert_eq!(
            state.player_state().playback_status,
            PlaybackStatus::Playing
        );

        state.pause_playback();
        assert_eq!(state.player_state().playback_status, PlaybackStatus::Paused);
        assert_eq!(state.player_state().position_seconds, 42.0);
        assert_eq!(state.player_state().current_queue_entry_id, selected);
        assert_eq!(state.queue().len(), queue_len);

        state.start_playback();
        assert_eq!(
            state.player_state().playback_status,
            PlaybackStatus::Playing
        );
        assert_eq!(state.player_state().position_seconds, 42.0);
        assert_eq!(state.player_state().current_queue_entry_id, selected);
    }
    #[test]
    fn search_results_use_the_shared_catalog() {
        let state = UiState {
            submitted_query: "neon".to_owned(),
            ..UiState::default()
        };
        assert_eq!(state.search_results(), vec![state.preview_track(1).id]);
    }
    #[test]
    fn preview_search_stays_local_but_live_search_never_falls_back_to_fixture_matches() {
        let mut preview = UiState {
            query: "neon".to_owned(),
            ..UiState::default()
        };
        preview.submit_search();
        assert!(preview.take_backend_commands().is_empty());
        assert_eq!(preview.search_results(), vec![preview.preview_track(1).id]);

        preview.set_data_mode(DataMode::Live);
        preview.submitted_query = "neon".to_owned();
        assert!(preview.search_results().is_empty());
    }
    #[test]
    fn backend_results_upsert_the_catalog_without_changing_artwork_policy() {
        let mut state = UiState::default();
        let id = TrackId::new(765);
        state.apply_backend_event(BackendEvent::SearchResults {
            request_id: 0,
            query: "real".to_owned(),
            tracks: vec![SoundCloudTrack {
                id,
                urn: Some("soundcloud:tracks:765".to_owned()),
                title: "Real".to_owned(),
                artist: "Artist".to_owned(),
                duration_seconds: Some(60),
                artwork_url: Some("https://i1.sndcdn.com/real.jpg".to_owned()),
                waveform_url: Some("https://wave.sndcdn.com/real.png".to_owned()),
                availability: PlaybackAvailability::Available,
                genre: "Test".to_owned(),
            }],
            playlists: Vec::new(),
            next_cursor: None,
            append: false,
        });
        assert_eq!(state.search_results(), vec![id]);
        state.apply_backend_event(BackendEvent::ArtworkAvailable {
            track_id: id,
            url: "https://i1.sndcdn.com/real.jpg".to_owned(),
        });
        assert_eq!(
            state.track(id).and_then(|track| track.artwork_url),
            Some("https://i1.sndcdn.com/real.jpg".to_owned())
        );
    }
    #[test]
    fn backend_error_never_reveals_credentials() {
        let mut state = UiState::default();
        state.apply_backend_event(BackendEvent::BackendError {
            operation: crate::backend::BackendOperation::SearchTracks,
            request_id: None,
            failure: crate::backend::BackendFailure::CredentialsRequired,
        });
        assert_eq!(
            state.toast.as_deref(),
            Some("Chưa cấu hình thông tin kết nối SoundCloud")
        );
    }

    fn live_track(id: u64, title: &str) -> SoundCloudTrack {
        SoundCloudTrack {
            id: TrackId::new(id),
            urn: Some(format!("soundcloud:tracks:{id}")),
            title: title.to_owned(),
            artist: "Live artist".to_owned(),
            duration_seconds: Some(180),
            artwork_url: Some("https://i1.sndcdn.com/live.jpg".to_owned()),
            waveform_url: Some(format!("https://wave.sndcdn.com/{id}.png")),
            availability: PlaybackAvailability::Available,
            genre: "Live metadata".to_owned(),
        }
    }

    fn begin_live_search(state: &mut UiState, query: &str) -> u64 {
        state.query = query.to_owned();
        state.submit_search();
        let commands = state.take_backend_commands();
        assert_eq!(commands.len(), 1);
        match &commands[0] {
            BackendCommand::SearchTracks {
                query: submitted,
                request_id,
                ..
            } => {
                assert_eq!(submitted, query);
                assert_ne!(*request_id, 0);
                *request_id
            }
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[test]
    fn live_search_command_results_and_catalog_share_one_state_path() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        let request_id = begin_live_search(&mut state, "ambient");
        assert_eq!(state.search_status(), SearchStatus::Loading);
        assert!(state.search_results().is_empty());

        state.apply_backend_event(BackendEvent::SearchResults {
            request_id,
            query: "ambient".to_owned(),
            tracks: vec![live_track(90_001, "Live ambient")],
            playlists: vec![SoundCloudPlaylist {
                id: PlaylistId::new(91_001),
                urn: Some("soundcloud:playlists:91001".to_owned()),
                title: "Live ambient playlists".to_owned(),
                description: Some("Search result".to_owned()),
                artwork_url: Some("https://i1.sndcdn.com/playlist.jpg".to_owned()),
                track_count: 18,
                editable: false,
            }],
            next_cursor: None,
            append: false,
        });
        assert_eq!(state.search_status(), SearchStatus::Results { count: 2 });
        assert_eq!(state.search_results(), vec![TrackId::new(90_001)]);
        assert_eq!(state.search_playlists()[0].id, PlaylistId::new(91_001));
        assert_eq!(
            state.track(TrackId::new(90_001)).unwrap().title,
            "Live ambient"
        );
    }

    #[test]
    fn live_search_pagination_appends_once_and_keeps_existing_results_on_error() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        let first_request = begin_live_search(&mut state, "Ambient Case");
        state.apply_backend_event(BackendEvent::SearchResults {
            request_id: first_request,
            query: "Ambient Case".to_owned(),
            tracks: vec![live_track(91_001, "First page")],
            playlists: Vec::new(),
            next_cursor: Some("cursor_page_two".to_owned()),
            append: false,
        });

        assert!(state.search_has_more());
        state.load_more_search_results();
        assert!(state.search_loading_more());
        let commands = state.take_backend_commands();
        let next_request = match commands.as_slice() {
            [
                BackendCommand::SearchTracks {
                    query,
                    cursor: Some(cursor),
                    append: true,
                    request_id,
                    ..
                },
            ] => {
                assert_eq!(query, "Ambient Case");
                assert_eq!(cursor, "cursor_page_two");
                *request_id
            }
            command => panic!("unexpected pagination command: {command:?}"),
        };

        state.apply_backend_event(BackendEvent::BackendError {
            operation: BackendOperation::SearchTracks,
            request_id: Some(next_request),
            failure: crate::backend::BackendFailure::Network,
        });
        assert_eq!(state.search_status(), SearchStatus::Results { count: 1 });
        assert_eq!(state.search_results(), vec![TrackId::new(91_001)]);
        assert!(state.search_has_more());

        state.load_more_search_results();
        let retry_request = match state.take_backend_commands().as_slice() {
            [
                BackendCommand::SearchTracks {
                    request_id,
                    append: true,
                    ..
                },
            ] => *request_id,
            command => panic!("unexpected retry command: {command:?}"),
        };
        state.apply_backend_event(BackendEvent::SearchResults {
            request_id: retry_request,
            query: "Ambient Case".to_owned(),
            tracks: vec![
                live_track(91_001, "Updated first page metadata"),
                live_track(91_002, "Second page"),
            ],
            playlists: Vec::new(),
            next_cursor: None,
            append: true,
        });

        assert_eq!(
            state.search_results(),
            vec![TrackId::new(91_001), TrackId::new(91_002)]
        );
        assert_eq!(
            state.track(TrackId::new(91_001)).unwrap().title,
            "Updated first page metadata"
        );
        assert!(!state.search_has_more());
        assert!(!state.search_loading_more());
    }

    #[test]
    fn back_navigation_returns_through_detail_flow_then_home() {
        let mut state = UiState::default();
        state.navigate_main(Page::Search);
        state.navigate_main(Page::Playlist);
        state.navigate_main(Page::NowPlaying);

        state.navigate_back();
        assert_eq!(state.route(), AppRoute::Main(Page::Playlist));
        state.navigate_back();
        assert_eq!(state.route(), AppRoute::Main(Page::Search));
        state.navigate_back();
        assert_eq!(state.route(), AppRoute::Main(Page::Home));
        assert!(!state.can_navigate_back());
    }

    #[test]
    fn stale_live_results_cannot_replace_newer_query_or_status() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        let old_request = begin_live_search(&mut state, "ambient");
        let new_request = begin_live_search(&mut state, "jazz");

        state.apply_backend_event(BackendEvent::SearchResults {
            request_id: old_request,
            query: "ambient".to_owned(),
            tracks: vec![live_track(90_002, "Old result")],
            playlists: Vec::new(),
            next_cursor: None,
            append: false,
        });
        assert_eq!(state.submitted_query, "jazz");
        assert_eq!(state.search_status(), SearchStatus::Loading);
        assert!(state.search_results().is_empty());
        assert!(state.track(TrackId::new(90_002)).is_some());

        state.apply_backend_event(BackendEvent::BackendError {
            operation: crate::backend::BackendOperation::SearchTracks,
            request_id: Some(old_request),
            failure: crate::backend::BackendFailure::Timeout,
        });
        assert_eq!(state.search_status(), SearchStatus::Loading);
        assert_eq!(state.submitted_query, "jazz");

        state.apply_backend_event(BackendEvent::SearchResults {
            request_id: new_request,
            query: "jazz".to_owned(),
            tracks: vec![live_track(90_003, "New result")],
            playlists: Vec::new(),
            next_cursor: None,
            append: false,
        });
        assert_eq!(state.search_results(), vec![TrackId::new(90_003)]);
        assert_eq!(state.submitted_query, "jazz");
    }

    #[test]
    fn live_empty_timeout_and_error_states_do_not_fall_back_to_preview() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        let empty_request = begin_live_search(&mut state, "no-result");
        state.apply_backend_event(BackendEvent::SearchResults {
            request_id: empty_request,
            query: "no-result".to_owned(),
            tracks: Vec::new(),
            playlists: Vec::new(),
            next_cursor: None,
            append: false,
        });
        assert_eq!(state.search_status(), SearchStatus::Empty);
        assert!(state.search_results().is_empty());

        let error_request = begin_live_search(&mut state, "timeout");
        state.apply_backend_event(BackendEvent::BackendError {
            operation: crate::backend::BackendOperation::SearchTracks,
            request_id: Some(error_request),
            failure: crate::backend::BackendFailure::Timeout,
        });
        assert_eq!(state.search_status(), SearchStatus::Error);
        assert!(state.search_results().is_empty());
        assert_eq!(
            state.toast.as_deref(),
            Some("Yêu cầu SoundCloud đã quá thời gian chờ")
        );
    }

    #[test]
    fn empty_live_query_does_not_start_a_worker_request() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        state.query = "   ".to_owned();
        state.submit_search();
        assert_eq!(state.search_status(), SearchStatus::Idle);
        assert!(state.take_backend_commands().is_empty());
        assert!(state.search_results().is_empty());
    }

    #[test]
    fn clearing_search_input_cannot_submit_a_request() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        state.query = "ambient".to_owned();
        state.clear_search_input();
        assert!(state.query.is_empty());
        state.submit_search();
        assert_eq!(state.search_status(), SearchStatus::Idle);
        assert!(state.take_backend_commands().is_empty());
    }

    #[test]
    fn live_track_selection_builds_context_without_fake_playback() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        let request_id = begin_live_search(&mut state, "live");
        let first = TrackId::new(90_004);
        let second = TrackId::new(90_005);
        state.apply_backend_event(BackendEvent::SearchResults {
            request_id,
            query: "live".to_owned(),
            tracks: vec![
                live_track(first.get(), "First"),
                live_track(second.get(), "Second"),
            ],
            playlists: Vec::new(),
            next_cursor: None,
            append: false,
        });

        state.select_track(first);
        assert_eq!(state.current_track().unwrap().id, first);
        assert_eq!(state.queue().len(), 2);
        assert_eq!(
            state.player_state().playback_status,
            crate::player::PlaybackStatus::Paused
        );
        assert!(!state.is_playing());
        assert_eq!(state.progress(), 0.0);

        state.advance_preview(60.0);
        state.toggle_playback();
        state.next();
        assert_eq!(state.current_track().unwrap().id, second);
        assert!(!state.is_playing());
        assert_eq!(state.progress(), 0.0);
        assert!(
            state
                .toast
                .as_deref()
                .is_some_and(|message| message.contains("Đã chọn Second"))
        );
    }

    #[test]
    fn enabled_live_playback_requests_a_descriptor_and_keeps_queue_order() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        state.set_live_playback_enabled(true);
        state.auth_state = AuthState::Authorized;
        state.user_session = Some(UserSession {
            session_id: "session_id_abcdefghijklmnop".to_owned(),
            session_secret: "session_secret_abcdefghijklmnopqrstuvwxyz".to_owned(),
            expires_at_ms: 4_000_000_000_000,
        });
        let search_request = begin_live_search(&mut state, "playable");
        let first = TrackId::new(96_001);
        let second = TrackId::new(96_002);
        state.apply_backend_event(BackendEvent::SearchResults {
            request_id: search_request,
            query: "playable".to_owned(),
            tracks: vec![
                live_track(first.get(), "First"),
                live_track(second.get(), "Second"),
            ],
            playlists: Vec::new(),
            next_cursor: None,
            append: false,
        });

        state.select_track(first);
        assert_eq!(
            state.player_state().playback_status,
            PlaybackStatus::Loading
        );
        assert!(matches!(
            state.take_audio_commands().as_slice(),
            [AudioCommand::Stop]
        ));
        let (descriptor_request, first_entry) = match state.take_backend_commands().as_slice() {
            [
                BackendCommand::GetStreamDescriptor {
                    request_id,
                    track_id,
                    track_urn,
                    ..
                },
            ] => {
                assert_eq!(*track_id, first);
                assert_eq!(track_urn, "soundcloud:tracks:96001");
                (
                    *request_id,
                    state.player_state().current_queue_entry_id.unwrap(),
                )
            }
            commands => panic!("unexpected playback commands: {commands:?}"),
        };
        let signed_url =
            "https://playback.media-streaming.soundcloud.cloud/a/playlist.m3u8?Policy=signed";
        let descriptor = crate::backend::StreamDescriptor::new(
            descriptor_request,
            first,
            "hls_aac_160".to_owned(),
            signed_url.to_owned(),
        );
        assert!(!format!("{descriptor:?}").contains(signed_url));
        state.apply_backend_event(BackendEvent::StreamDescriptorLoaded { descriptor });
        assert!(matches!(
            state.take_audio_commands().as_slice(),
            [AudioCommand::Load { track_id, entry_id, .. }]
                if *track_id == first && *entry_id == first_entry
        ));

        state.apply_playback_engine_event(PlaybackEngineEvent::Started {
            track_id: first,
            entry_id: first_entry,
        });
        assert!(state.is_playing());
        state.apply_playback_engine_event(PlaybackEngineEvent::Position {
            track_id: first,
            entry_id: first_entry,
            seconds: 37.0,
        });
        state.pause_playback();
        assert!(matches!(
            state.take_audio_commands().as_slice(),
            [AudioCommand::Pause]
        ));
        state.apply_playback_engine_event(PlaybackEngineEvent::Paused {
            track_id: first,
            entry_id: first_entry,
        });
        assert_eq!(state.player_state().playback_status, PlaybackStatus::Paused);
        assert_eq!(state.player_state().position_seconds, 37.0);
        state.start_playback();
        assert!(matches!(
            state.take_audio_commands().as_slice(),
            [AudioCommand::Resume]
        ));
        assert!(state.take_backend_commands().is_empty());
        state.apply_playback_engine_event(PlaybackEngineEvent::Started {
            track_id: first,
            entry_id: first_entry,
        });
        assert_eq!(state.player_state().position_seconds, 37.0);
        state.next();
        assert_eq!(state.current_track().unwrap().id, second);
        assert_eq!(
            state.player_state().playback_status,
            PlaybackStatus::Loading
        );
        assert!(matches!(
            state.take_audio_commands().as_slice(),
            [AudioCommand::Stop]
        ));
        assert!(matches!(
            state.take_backend_commands().as_slice(),
            [BackendCommand::GetStreamDescriptor { track_id, .. }] if *track_id == second
        ));
    }

    #[test]
    fn stockos_idle_suspend_stops_decoder_and_start_reloads_at_saved_position() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        state.set_live_playback_enabled(true);
        state.auth_state = AuthState::Authorized;
        state.user_session = Some(UserSession {
            session_id: "session_id_idle_resume_abcdefghijkl".to_owned(),
            session_secret: "session_secret_idle_resume_abcdefghijklmnopqrstuvwxyz".to_owned(),
            expires_at_ms: 4_000_000_000_000,
        });
        let search_request = begin_live_search(&mut state, "idle resume");
        let track_id = TrackId::new(96_101);
        state.apply_backend_event(BackendEvent::SearchResults {
            request_id: search_request,
            query: "idle resume".to_owned(),
            tracks: vec![live_track(track_id.get(), "Idle resume")],
            playlists: Vec::new(),
            next_cursor: None,
            append: false,
        });

        state.select_track(track_id);
        let entry_id = state.player_state().current_queue_entry_id.unwrap();
        state.take_audio_commands();
        let first_request = match state.take_backend_commands().as_slice() {
            [BackendCommand::GetStreamDescriptor { request_id, .. }] => *request_id,
            commands => panic!("unexpected initial commands: {commands:?}"),
        };
        state.apply_backend_event(BackendEvent::StreamDescriptorLoaded {
            descriptor: crate::backend::StreamDescriptor::new(
                first_request,
                track_id,
                "hls_aac_160".to_owned(),
                "https://playback.media-streaming.soundcloud.cloud/a/idle.m3u8?Policy=signed"
                    .to_owned(),
            ),
        });
        assert!(matches!(
            state.take_audio_commands().as_slice(),
            [AudioCommand::Load { .. }]
        ));
        state.apply_playback_engine_event(PlaybackEngineEvent::Started { track_id, entry_id });
        state.apply_playback_engine_event(PlaybackEngineEvent::Position {
            track_id,
            entry_id,
            seconds: 42.5,
        });

        assert!(state.suspend_audio_for_idle());
        assert_eq!(state.player_state().playback_status, PlaybackStatus::Paused);
        assert_eq!(state.player_state().position_seconds, 42.5);
        assert!(matches!(
            state.take_audio_commands().as_slice(),
            [AudioCommand::Stop]
        ));

        state.start_playback();
        assert!(matches!(
            state.take_audio_commands().as_slice(),
            [AudioCommand::Stop]
        ));
        let resume_request = match state.take_backend_commands().as_slice() {
            [BackendCommand::GetStreamDescriptor { request_id, .. }] => *request_id,
            commands => panic!("unexpected resume commands: {commands:?}"),
        };
        assert_eq!(state.player_state().position_seconds, 42.5);
        state.apply_backend_event(BackendEvent::StreamDescriptorLoaded {
            descriptor: crate::backend::StreamDescriptor::new(
                resume_request,
                track_id,
                "hls_aac_160".to_owned(),
                "https://playback.media-streaming.soundcloud.cloud/a/idle.m3u8?Policy=signed"
                    .to_owned(),
            ),
        });
        assert!(matches!(
            state.take_audio_commands().as_slice(),
            [AudioCommand::Load { .. }]
        ));
        state.apply_playback_engine_event(PlaybackEngineEvent::Started { track_id, entry_id });
        assert!(matches!(
            state.take_audio_commands().as_slice(),
            [AudioCommand::Seek(seconds)] if (*seconds - 42.5).abs() < f32::EPSILON
        ));
        assert_eq!(state.player_state().position_seconds, 42.5);
    }

    #[test]
    fn unavailable_playback_engine_does_not_create_a_stop_retry_loop() {
        let mut state = UiState::default();
        state.set_live_playback_enabled(false);
        assert!(state.take_audio_commands().is_empty());

        state.set_live_playback_enabled(true);
        state.set_live_playback_enabled(false);
        assert!(matches!(
            state.take_audio_commands().as_slice(),
            [AudioCommand::Stop]
        ));
        state.set_live_playback_enabled(false);
        assert!(state.take_audio_commands().is_empty());
    }

    #[test]
    fn qr_login_uses_one_app_state_and_clears_all_client_proofs_on_logout() {
        use crate::backend::{AuthState, ProfileId, QrLoginSession, UserSession};

        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        state.open_login();
        state.start_qr_login();
        assert_eq!(state.auth_state(), AuthState::CreatingQr);
        assert!(matches!(
            state.take_backend_commands().as_slice(),
            [BackendCommand::StartQrLogin]
        ));

        let login = QrLoginSession {
            pairing_id: "pairing_id_abcdefghijklmnop".to_owned(),
            pairing_url: "https://brickwave.example/auth/connect?pairing=safe".to_owned(),
            device_secret: "device_secret_abcdefghijklmnopqrstuvwxyz".to_owned(),
            confirmation_code: "123456".to_owned(),
            expires_at_ms: 4_000_000_000_000,
        };
        let redacted = format!("{login:?}");
        assert!(!redacted.contains(&login.device_secret));
        assert!(!redacted.contains(&login.pairing_url));

        state.apply_backend_event(BackendEvent::QrLoginStarted {
            login: login.clone(),
        });
        assert_eq!(state.auth_state(), AuthState::WaitingForScan);
        state.poll_qr_login();
        assert!(matches!(
            state.take_backend_commands().as_slice(),
            [BackendCommand::PollQrLogin { login: queued }] if queued == &login
        ));

        state.apply_backend_event(BackendEvent::PairingConfirmationRequired {
            login: login.clone(),
        });
        assert_eq!(state.auth_state(), AuthState::PairingConfirmation);
        state.confirm_pairing();
        assert!(matches!(
            state.take_backend_commands().as_slice(),
            [BackendCommand::ConfirmPairing { login: queued }] if queued == &login
        ));

        let app_session = UserSession {
            session_id: "session_id_abcdefghijklmnop".to_owned(),
            session_secret: "session_secret_abcdefghijklmnopqrstuvwxyz".to_owned(),
            expires_at_ms: 4_000_000_000_000,
        };
        state.apply_backend_event(BackendEvent::QrLoginAuthorized {
            session: app_session.clone(),
            profile: SoundCloudProfile {
                id: ProfileId::new(77),
                username: "real-listener".to_owned(),
                display_name: Some("Real Listener".to_owned()),
                avatar_url: Some("https://i1.sndcdn.com/avatar.jpg".to_owned()),
            },
        });
        assert_eq!(state.auth_state(), AuthState::Authorized);
        assert!(state.qr_login().is_none());
        assert_eq!(state.current_profile().unwrap().username, "real-listener");
        assert!(matches!(
            state.take_backend_commands().as_slice(),
            [
                BackendCommand::GetCurrentUser { session: profile_session },
                BackendCommand::GetUserLibrary { session: library_session }
            ] if profile_session == &app_session && library_session == &app_session
        ));

        state.logout();
        assert!(matches!(
            state.take_backend_commands().as_slice(),
            [BackendCommand::Logout { session }] if session == &app_session
        ));
        state.apply_backend_event(BackendEvent::LoggedOut);
        assert_eq!(state.auth_state(), AuthState::LoggedOut);
        assert!(state.current_profile().is_none());
        assert!(state.qr_login().is_none());

        // Public metadata search remains independent of the user session.
        let request_id = begin_live_search(&mut state, "ambient");
        assert_ne!(request_id, 0);
    }

    #[test]
    fn transient_qr_poll_and_profile_errors_preserve_recoverable_state() {
        use crate::backend::{AuthState, ProfileId, QrLoginSession, UserSession};

        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        state.open_login();
        state.start_qr_login();
        let _ = state.take_backend_commands();
        let login = QrLoginSession {
            pairing_id: "pairing_id_abcdefghijklmnop".to_owned(),
            pairing_url: "https://brickwave.example/auth/connect?pairing=safe".to_owned(),
            device_secret: "device_secret_abcdefghijklmnopqrstuvwxyz".to_owned(),
            confirmation_code: "123456".to_owned(),
            expires_at_ms: 4_000_000_000_000,
        };
        state.apply_backend_event(BackendEvent::QrLoginStarted {
            login: login.clone(),
        });
        state.apply_backend_event(BackendEvent::BackendError {
            operation: BackendOperation::PollQrLogin,
            request_id: None,
            failure: crate::backend::BackendFailure::Timeout,
        });
        assert_eq!(state.auth_state(), AuthState::WaitingForScan);
        assert_eq!(state.qr_login(), Some(&login));
        assert!(state.auth_polling_active());

        let app_session = UserSession {
            session_id: "session_id_abcdefghijklmnop".to_owned(),
            session_secret: "session_secret_abcdefghijklmnopqrstuvwxyz".to_owned(),
            expires_at_ms: 4_000_000_000_000,
        };
        state.apply_backend_event(BackendEvent::QrLoginAuthorized {
            session: app_session,
            profile: SoundCloudProfile {
                id: ProfileId::new(77),
                username: "real-listener".to_owned(),
                display_name: None,
                avatar_url: None,
            },
        });
        state.take_backend_commands();
        state.apply_backend_event(BackendEvent::BackendError {
            operation: BackendOperation::GetCurrentUser,
            request_id: None,
            failure: crate::backend::BackendFailure::Network,
        });
        assert_eq!(state.auth_state(), AuthState::Authorized);
        assert_eq!(state.current_profile().unwrap().username, "real-listener");
    }

    #[test]
    fn authenticated_library_replaces_live_likes_and_playlist_summaries() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        state.apply_backend_event(BackendEvent::UserLibraryLoaded {
            liked_tracks: vec![live_track(91_001, "Liked from account")],
            playlists: vec![SoundCloudPlaylist {
                id: PlaylistId::new(92_001),
                urn: Some("soundcloud:playlists:92001".to_owned()),
                title: "Account playlist".to_owned(),
                description: Some("Loaded through the user session".to_owned()),
                artwork_url: Some("https://i1.sndcdn.com/playlist.jpg".to_owned()),
                track_count: 12,
                editable: true,
            }],
            home_tracks: vec![live_track(93_001, "Feed track")],
            discover_tracks: vec![live_track(94_001, "Related track")],
        });

        assert_eq!(state.library_status(), LibraryStatus::Loaded);
        assert_eq!(state.liked_track_ids(), vec![TrackId::new(91_001)]);
        assert!(state.is_liked(TrackId::new(91_001)));
        let playlists = state.library_playlists();
        assert_eq!(playlists.len(), 1);
        assert_eq!(playlists[0].title, "Account playlist");
        assert_eq!(playlists[0].track_count, 12);
        assert_eq!(state.home_track_ids(), vec![TrackId::new(93_001)]);
        assert_eq!(state.discover_track_ids(), vec![TrackId::new(94_001)]);
        state.select_track(TrackId::new(93_001));
        assert_eq!(state.current_track().unwrap().id, TrackId::new(93_001));
        assert_eq!(state.player_state().queue.len(), 1);
    }

    #[test]
    fn live_like_waits_for_backend_confirmation_and_updates_one_catalog() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        state.auth_state = AuthState::Authorized;
        state.user_session = Some(UserSession {
            session_id: "session_id_abcdefghijklmnop".to_owned(),
            session_secret: "session_secret_abcdefghijklmnopqrstuvwxyz".to_owned(),
            expires_at_ms: 4_000_000_000_000,
        });
        let track = live_track(95_001, "Like me");
        let track_id = track.id;
        state.apply_backend_event(BackendEvent::UserLibraryLoaded {
            liked_tracks: Vec::new(),
            playlists: Vec::new(),
            home_tracks: vec![track],
            discover_tracks: Vec::new(),
        });
        state.set_live_playback_enabled(true);

        state.toggle_like(track_id);
        assert!(state.like_pending(track_id));
        assert!(!state.is_liked(track_id));
        assert!(matches!(
            state.take_backend_commands().as_slice(),
            [BackendCommand::SetTrackLiked {
                track_id: requested,
                track_urn,
                liked: true,
                ..
            }] if *requested == track_id && track_urn == "soundcloud:tracks:95001"
        ));

        state.apply_backend_event(BackendEvent::TrackLikeUpdated {
            track_id,
            liked: true,
        });
        assert!(!state.like_pending(track_id));
        assert!(state.is_liked(track_id));
        assert_eq!(state.liked_track_ids(), vec![track_id]);
        assert!(matches!(
            state.take_audio_commands().as_slice(),
            [AudioCommand::SetCachePriority {
                track_id: cached_track,
                liked: true
            }] if *cached_track == track_id
        ));
    }

    #[test]
    fn add_to_playlist_targets_only_editable_account_playlists() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        state.auth_state = AuthState::Authorized;
        state.user_session = Some(UserSession {
            session_id: "session_id_abcdefghijklmnop".to_owned(),
            session_secret: "session_secret_abcdefghijklmnopqrstuvwxyz".to_owned(),
            expires_at_ms: 4_000_000_000_000,
        });
        let track = live_track(95_002, "Save me");
        let track_id = track.id;
        let playlist_id = PlaylistId::new(95_101);
        state.apply_backend_event(BackendEvent::UserLibraryLoaded {
            liked_tracks: Vec::new(),
            playlists: vec![SoundCloudPlaylist {
                id: playlist_id,
                urn: Some("soundcloud:playlists:95101".to_owned()),
                title: "Owned playlist".to_owned(),
                description: None,
                artwork_url: None,
                track_count: 2,
                editable: true,
            }],
            home_tracks: vec![track],
            discover_tracks: Vec::new(),
        });

        state.open_add_to_playlist(track_id);
        assert_eq!(state.add_to_playlist_track(), Some(track_id));
        state.add_track_to_playlist(playlist_id);
        assert!(state.playlist_add_pending());
        assert!(matches!(
            state.take_backend_commands().as_slice(),
            [BackendCommand::AddTrackToPlaylist {
                playlist_id: requested_playlist,
                playlist_urn,
                track_id: requested_track,
                track_urn,
                ..
            }] if *requested_playlist == playlist_id
                && playlist_urn == "soundcloud:playlists:95101"
                && *requested_track == track_id
                && track_urn == "soundcloud:tracks:95002"
        ));

        state.apply_backend_event(BackendEvent::TrackAddedToPlaylist {
            playlist_id,
            track_id,
            track_count: 3,
        });
        assert!(!state.playlist_add_pending());
        let playlist = state.catalog.playlist(playlist_id).unwrap();
        assert_eq!(playlist.track_ids, vec![track_id]);
        assert_eq!(playlist.track_count, 3);
    }

    #[test]
    fn remove_selected_playlist_entry_waits_for_confirmation_and_keeps_order() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        state.auth_state = AuthState::Authorized;
        state.user_session = Some(UserSession {
            session_id: "session_id_abcdefghijklmnop".to_owned(),
            session_secret: "session_secret_abcdefghijklmnopqrstuvwxyz".to_owned(),
            expires_at_ms: 4_000_000_000_000,
        });
        let playlist_id = PlaylistId::new(95_102);
        let first = live_track(95_011, "First occurrence");
        let second = live_track(95_012, "Middle track");
        let repeated = first.clone();
        let first_id = first.id;
        let second_id = second.id;
        state.apply_backend_event(BackendEvent::UserLibraryLoaded {
            liked_tracks: Vec::new(),
            playlists: vec![SoundCloudPlaylist {
                id: playlist_id,
                urn: Some("soundcloud:playlists:95102".to_owned()),
                title: "Editable order".to_owned(),
                description: None,
                artwork_url: None,
                track_count: 3,
                editable: true,
            }],
            home_tracks: Vec::new(),
            discover_tracks: Vec::new(),
        });
        state.open_playlist(playlist_id);
        let _ = state.take_backend_commands();
        state.apply_backend_event(BackendEvent::PlaylistTracksLoaded {
            playlist_id,
            tracks: vec![first, second, repeated],
        });

        state.toggle_playlist_track_remove_mode(playlist_id);
        assert!(state.playlist_track_remove_mode(playlist_id));
        assert!(state.dismiss_top_modal());
        assert!(!state.playlist_track_remove_mode(playlist_id));
        state.toggle_playlist_track_remove_mode(playlist_id);

        state.request_remove_track_from_playlist(playlist_id, first_id, 2);
        assert_eq!(
            state.remove_playlist_track(),
            Some((playlist_id, first_id, 2))
        );
        state.confirm_remove_track_from_playlist();
        assert!(state.playlist_track_remove_pending());
        assert_eq!(
            state.current_playlist().unwrap().track_ids,
            vec![first_id, second_id, first_id]
        );
        assert!(matches!(
            state.take_backend_commands().as_slice(),
            [BackendCommand::RemoveTrackFromPlaylist {
                playlist_id: requested_playlist,
                playlist_urn,
                track_id: requested_track,
                track_urn,
                track_index: 2,
                ..
            }] if *requested_playlist == playlist_id
                && playlist_urn == "soundcloud:playlists:95102"
                && *requested_track == first_id
                && track_urn == "soundcloud:tracks:95011"
        ));

        state.apply_backend_event(BackendEvent::TrackRemovedFromPlaylist {
            playlist_id,
            track_id: first_id,
            track_index: 2,
            track_count: 2,
        });
        assert!(!state.playlist_track_remove_pending());
        let playlist = state.current_playlist().unwrap();
        assert_eq!(playlist.track_ids, vec![first_id, second_id]);
        assert_eq!(playlist.track_count, 2);
        assert_eq!(state.playlist_status(), PlaylistStatus::Loaded);
        assert!(state.playlist_track_remove_mode(playlist_id));
    }

    #[test]
    fn create_and_delete_playlist_update_the_single_catalog_after_confirmation() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        state.auth_state = AuthState::Authorized;
        state.user_session = Some(UserSession {
            session_id: "session_id_abcdefghijklmnop".to_owned(),
            session_secret: "session_secret_abcdefghijklmnopqrstuvwxyz".to_owned(),
            expires_at_ms: 4_000_000_000_000,
        });

        state.open_create_playlist();
        state
            .new_playlist_title_mut()
            .push_str("  Brick favourites  ");
        state.submit_create_playlist();
        assert!(state.playlist_create_pending());
        assert!(matches!(
            state.take_backend_commands().as_slice(),
            [BackendCommand::CreatePlaylist { title, .. }] if title == "Brick favourites"
        ));

        let playlist_id = PlaylistId::new(95_201);
        state.apply_backend_event(BackendEvent::PlaylistCreated {
            playlist: SoundCloudPlaylist {
                id: playlist_id,
                urn: Some("soundcloud:playlists:95201".to_owned()),
                title: "Brick favourites".to_owned(),
                description: None,
                artwork_url: None,
                track_count: 0,
                editable: true,
            },
        });
        assert!(!state.playlist_create_pending());
        assert!(!state.create_playlist_dialog_open());
        assert_eq!(state.library_playlists()[0].id, playlist_id);

        state.open_playlist(playlist_id);
        state.request_delete_playlist(playlist_id);
        state.confirm_delete_playlist();
        assert!(state.playlist_delete_pending());
        assert!(matches!(
            state.take_backend_commands().as_slice(),
            [BackendCommand::DeletePlaylist {
                playlist_id: requested,
                playlist_urn,
                ..
            }] if *requested == playlist_id && playlist_urn == "soundcloud:playlists:95201"
        ));
        state.apply_backend_event(BackendEvent::PlaylistDeleted { playlist_id });
        assert!(!state.playlist_delete_pending());
        assert!(state.library_playlists().is_empty());
        assert_eq!(state.main_page(), Page::Library);
        assert_eq!(state.library_tab, LibraryTab::Playlists);
    }

    #[test]
    fn playlist_mutations_reject_invalid_title_and_non_owned_playlist() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        state.auth_state = AuthState::Authorized;
        state.user_session = Some(UserSession {
            session_id: "session_id_abcdefghijklmnop".to_owned(),
            session_secret: "session_secret_abcdefghijklmnopqrstuvwxyz".to_owned(),
            expires_at_ms: 4_000_000_000_000,
        });
        state.open_create_playlist();
        state.submit_create_playlist();
        assert!(!state.playlist_create_pending());
        assert!(state.take_backend_commands().is_empty());

        let playlist_id = PlaylistId::new(95_202);
        state.apply_backend_event(BackendEvent::UserLibraryLoaded {
            liked_tracks: Vec::new(),
            playlists: vec![SoundCloudPlaylist {
                id: playlist_id,
                urn: Some("soundcloud:playlists:95202".to_owned()),
                title: "Liked, not owned".to_owned(),
                description: None,
                artwork_url: None,
                track_count: 2,
                editable: false,
            }],
            home_tracks: Vec::new(),
            discover_tracks: Vec::new(),
        });
        state.request_delete_playlist(playlist_id);
        assert_eq!(state.delete_playlist_id(), None);
        assert!(state.take_backend_commands().is_empty());
    }

    #[test]
    fn live_playlist_loads_ordered_tracks_and_metadata_next_advances() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        state.auth_state = AuthState::Authorized;
        state.user_session = Some(UserSession {
            session_id: "session_id_abcdefghijklmnop".to_owned(),
            session_secret: "session_secret_abcdefghijklmnopqrstuvwxyz".to_owned(),
            expires_at_ms: 4_000_000_000_000,
        });
        let playlist_id = PlaylistId::new(92_002);
        state.apply_backend_event(BackendEvent::UserLibraryLoaded {
            liked_tracks: Vec::new(),
            playlists: vec![SoundCloudPlaylist {
                id: playlist_id,
                urn: Some("soundcloud:playlists:92002".to_owned()),
                title: "Ordered playlist".to_owned(),
                description: None,
                artwork_url: None,
                track_count: 10,
                editable: true,
            }],
            home_tracks: Vec::new(),
            discover_tracks: Vec::new(),
        });

        state.open_playlist(playlist_id);
        assert_eq!(state.main_page(), Page::Playlist);
        assert_eq!(state.playlist_status(), PlaylistStatus::Loading);
        assert!(matches!(
            state.take_backend_commands().as_slice(),
            [BackendCommand::GetPlaylistTracks {
                playlist_id: requested,
                playlist_urn,
                ..
            }] if *requested == playlist_id && playlist_urn == "soundcloud:playlists:92002"
        ));

        let tracks: Vec<_> = (0..10)
            .map(|index| live_track(95_000 + index, &format!("Track {}", index + 1)))
            .collect();
        state.apply_backend_event(BackendEvent::PlaylistTracksLoaded {
            playlist_id,
            tracks,
        });
        assert_eq!(state.playlist_status(), PlaylistStatus::Loaded);
        let context = state.current_playlist().unwrap().track_ids;
        assert_eq!(context.len(), 10);
        state.select_track_from_context(context, 4);
        assert_eq!(state.current_track().unwrap().title, "Track 5");
        state.next();
        assert_eq!(state.current_track().unwrap().title, "Track 6");
        assert_eq!(
            state.player_state().playback_status,
            crate::player::PlaybackStatus::Paused
        );
    }

    #[test]
    fn live_library_error_preserves_previous_account_data_for_retry() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        state.apply_backend_event(BackendEvent::UserLibraryLoaded {
            liked_tracks: vec![live_track(91_002, "Still visible")],
            playlists: Vec::new(),
            home_tracks: Vec::new(),
            discover_tracks: Vec::new(),
        });
        state.apply_backend_event(BackendEvent::BackendError {
            operation: BackendOperation::GetUserLibrary,
            request_id: None,
            failure: crate::backend::BackendFailure::Timeout,
        });

        assert_eq!(state.library_status(), LibraryStatus::Error);
        assert_eq!(state.liked_track_ids(), vec![TrackId::new(91_002)]);
        assert_eq!(state.auth_state(), AuthState::LoggedOut);
    }

    #[test]
    fn wake_recovery_retries_only_incomplete_live_account_data() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        state.auth_state = AuthState::Authorized;
        state.user_session = Some(UserSession {
            session_id: "session_id_abcdefghijklmnop".to_owned(),
            session_secret: "session_secret_abcdefghijklmnopqrstuvwxyz".to_owned(),
            expires_at_ms: 4_000_000_000_000,
        });
        state.library_status = LibraryStatus::Error;
        state.take_backend_commands();

        state.recover_after_network_resume();

        assert_eq!(state.library_status(), LibraryStatus::Loading);
        assert!(matches!(
            state.take_backend_commands().as_slice(),
            [BackendCommand::GetUserLibrary { .. }]
        ));

        state.library_status = LibraryStatus::Loaded;
        state.recover_after_network_resume();
        assert!(state.take_backend_commands().is_empty());
    }

    #[test]
    fn restored_session_uses_restore_gate_then_opens_home() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        let session = UserSession {
            session_id: "session_id_abcdefghijklmnop".to_owned(),
            session_secret: "session_secret_abcdefghijklmnopqrstuvwxyz".to_owned(),
            expires_at_ms: 4_000_000_000_000,
        };
        state.restore_user_session(session.clone());
        assert_eq!(
            state.route(),
            AppRoute::Restoring {
                destination: Page::Home
            }
        );
        assert!(state.is_restoring_route());
        assert!(!state.is_login_route());
        assert_eq!(state.auth_state(), AuthState::AuthorizationPending);
        assert!(!state.can_enter_main_app());
        assert!(matches!(
            state.take_backend_commands().as_slice(),
            [BackendCommand::GetCurrentUser { session: restored }] if restored == &session
        ));

        state.apply_backend_event(BackendEvent::CurrentUserLoaded {
            profile: SoundCloudProfile {
                id: ProfileId::new(42),
                username: "restored-user".to_owned(),
                display_name: None,
                avatar_url: None,
            },
        });
        assert_eq!(state.route(), AppRoute::Main(Page::Home));
        assert!(state.can_enter_main_app());
        assert!(matches!(
            state.take_backend_commands().as_slice(),
            [BackendCommand::GetUserLibrary { session: restored }] if restored == &session
        ));
    }

    #[test]
    fn transient_restore_failure_stays_off_qr_and_can_retry() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        let session = UserSession {
            session_id: "session_id_abcdefghijklmnop".to_owned(),
            session_secret: "session_secret_abcdefghijklmnopqrstuvwxyz".to_owned(),
            expires_at_ms: 4_000_000_000_000,
        };
        state.restore_user_session(session.clone());
        let _ = state.take_backend_commands();

        state.apply_backend_event(BackendEvent::BackendError {
            operation: BackendOperation::GetCurrentUser,
            request_id: None,
            failure: crate::backend::BackendFailure::Network,
        });

        assert_eq!(
            state.route(),
            AppRoute::Restoring {
                destination: Page::Home
            }
        );
        assert_eq!(state.auth_state(), AuthState::LoginFailed);
        assert_eq!(state.user_session(), Some(&session));
        assert_eq!(
            state.auth_error(),
            Some("Không thể kết nối mạng tới SoundCloud")
        );
        assert!(state.toast.is_none());

        state.retry_restore_session();
        assert_eq!(state.auth_state(), AuthState::AuthorizationPending);
        assert!(state.auth_error().is_none());
        assert!(matches!(
            state.take_backend_commands().as_slice(),
            [BackendCommand::GetCurrentUser { session: restored }] if restored == &session
        ));
    }

    #[test]
    fn sign_in_again_clears_saved_identity_and_enters_qr_route() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        state.catalog.profile = Some(PublicProfile {
            id: ProfileId::new(42),
            username: "old-user".to_owned(),
            display_name: None,
            avatar_url: None,
        });
        let session = UserSession {
            session_id: "session_id_abcdefghijklmnop".to_owned(),
            session_secret: "session_secret_abcdefghijklmnopqrstuvwxyz".to_owned(),
            expires_at_ms: 4_000_000_000_000,
        };
        state.restore_user_session(session);
        let _ = state.take_backend_commands();

        state.abandon_restore_and_login();

        assert_eq!(
            state.route(),
            AppRoute::Login {
                return_page: Page::Home
            }
        );
        assert_eq!(state.auth_state(), AuthState::LoggedOut);
        assert!(state.user_session().is_none());
        assert!(state.current_profile().is_none());
        assert_eq!(state.library_status(), LibraryStatus::Idle);
        assert!(state.auth_error().is_none());
    }

    #[test]
    fn logged_out_account_cannot_back_into_the_main_app() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        state.navigate_main(Page::Search);
        state.open_login();
        assert_eq!(
            state.route(),
            AppRoute::Login {
                return_page: Page::Search
            }
        );
        assert!(!state.can_leave_login());
        state.back_from_login();
        assert_eq!(
            state.route(),
            AppRoute::Login {
                return_page: Page::Search
            }
        );
    }

    #[test]
    fn cancel_login_revokes_pairing_and_remains_on_required_login() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        state.navigate_main(Page::Discover);
        state.open_login();
        state.start_qr_login();
        let _ = state.take_backend_commands();
        let login = login_fixture();
        state.apply_backend_event(BackendEvent::QrLoginStarted {
            login: login.clone(),
        });
        state.poll_qr_login();
        let _ = state.take_backend_commands();

        state.cancel_login_and_return();

        assert_eq!(
            state.route(),
            AppRoute::Login {
                return_page: Page::Discover
            }
        );
        assert_eq!(state.auth_state(), AuthState::Cancelled);
        assert!(!state.auth_polling_active());
        assert!(matches!(
            state.take_backend_commands().as_slice(),
            [BackendCommand::CancelQrLogin { login: queued }] if queued == &login
        ));
    }

    #[test]
    fn successful_login_closes_login_screen_and_opens_home() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        state.open_login();
        state.start_qr_login();
        let _ = state.take_backend_commands();
        state.apply_backend_event(BackendEvent::QrLoginStarted {
            login: login_fixture(),
        });
        let session = UserSession {
            session_id: "session_id_abcdefghijklmnop".to_owned(),
            session_secret: "session_secret_abcdefghijklmnopqrstuvwxyz".to_owned(),
            expires_at_ms: 4_000_000_000_000,
        };
        state.apply_backend_event(BackendEvent::QrLoginAuthorized {
            session,
            profile: SoundCloudProfile {
                id: ProfileId::new(57),
                username: "brick-listener".to_owned(),
                display_name: Some("Brick Listener".to_owned()),
                avatar_url: Some("https://i1.sndcdn.com/avatar.jpg".to_owned()),
            },
        });

        assert_eq!(state.route(), AppRoute::Main(Page::Home));
        assert_eq!(state.auth_state(), AuthState::Authorized);
        assert_eq!(state.current_profile().unwrap().username, "brick-listener");
        assert!(!state.auth_polling_active());
    }

    #[test]
    fn authorized_account_never_starts_a_new_qr_session() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        state.auth_state = AuthState::Authorized;
        state.user_session = Some(UserSession {
            session_id: "session_id_abcdefghijklmnop".to_owned(),
            session_secret: "session_secret_abcdefghijklmnopqrstuvwxyz".to_owned(),
            expires_at_ms: 4_000_000_000_000,
        });
        state.navigate_main(Page::Home);
        state.open_login();
        assert_eq!(state.route(), AppRoute::Main(Page::Profile));
        assert!(state.take_backend_commands().is_empty());
    }

    #[test]
    fn expired_session_can_navigate_to_reconnect() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        state.auth_state = AuthState::TokenExpired;
        state.navigate_main(Page::Profile);
        state.open_login();
        assert_eq!(
            state.route(),
            AppRoute::Login {
                return_page: Page::Profile
            }
        );
        assert!(state.take_backend_commands().is_empty());
    }

    #[test]
    fn polling_has_only_one_in_flight_request() {
        let mut state = UiState::default();
        state.set_data_mode(DataMode::Live);
        state.open_login();
        state.start_qr_login();
        let _ = state.take_backend_commands();
        let login = login_fixture();
        state.apply_backend_event(BackendEvent::QrLoginStarted {
            login: login.clone(),
        });
        state.poll_qr_login();
        state.poll_qr_login();
        assert!(matches!(
            state.take_backend_commands().as_slice(),
            [BackendCommand::PollQrLogin { login: queued }] if queued == &login
        ));
        state.apply_backend_event(BackendEvent::QrLoginPending {
            phase: QrLoginPhase::WaitingForScan,
            expires_at_ms: login.expires_at_ms,
            confirmation_code: login.confirmation_code.clone(),
        });
        state.poll_qr_login();
        assert_eq!(state.take_backend_commands().len(), 1);
    }

    #[test]
    fn routing_preserves_state_but_live_main_stays_locked_until_login() {
        let mut state = UiState::default();
        let selected = state.preview_track(4).id;
        state.play_collection_from_preview(4);
        let queue_len = state.queue().len();
        state.open_login();
        state.back_from_login();
        assert_eq!(state.current_track().unwrap().id, selected);
        assert_eq!(state.queue().len(), queue_len);

        state.set_data_mode(DataMode::Live);
        assert_eq!(
            state.route(),
            AppRoute::Login {
                return_page: Page::Home
            }
        );
        state.query = "ambient".to_owned();
        state.submit_search();
        assert_eq!(
            state.route(),
            AppRoute::Login {
                return_page: Page::Search
            }
        );
        assert!(matches!(
            state.take_backend_commands().as_slice(),
            [BackendCommand::SearchTracks { query, .. }] if query == "ambient"
        ));
    }

    fn login_fixture() -> QrLoginSession {
        QrLoginSession {
            pairing_id: "pairing_id_abcdefghijklmnop".to_owned(),
            pairing_url: "https://brickwave.example/auth/connect?pairing=safe".to_owned(),
            device_secret: "device_secret_abcdefghijklmnopqrstuvwxyz".to_owned(),
            confirmation_code: "123456".to_owned(),
            expires_at_ms: 4_000_000_000_000,
        }
    }
}
