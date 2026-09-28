//! Artwork loading and caching independent from the UI layout.
//!
//! Network, disk IO and image decode run in one worker thread. `ArtworkManager`
//! is polled from the egui thread, where it alone creates and evicts textures.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use egui::{Color32, ColorImage, Context, TextureHandle, TextureOptions};
use image::ImageReader;
use sha2::{Digest, Sha256};
use url::Url;

use crate::state::ArtworkId;

const MAX_REDIRECTS: usize = 3;
const DEFAULT_MAX_DOWNLOAD_BYTES: usize = 768 * 1024;
const DEFAULT_MAX_DECODED_PIXELS: u64 = 1024 * 1024;
const DEFAULT_MAX_DISK_BYTES: u64 = 12 * 1024 * 1024;
// A 1024x768 Brick screen shows roughly eight result rows plus the persistent
// player. The pixel cap keeps GPU use bounded; the count is only a secondary
// guard and is no longer allowed to evict an image visible in this frame.
const DEFAULT_MAX_MEMORY_TEXTURES: usize = 12;
const DEFAULT_MAX_MEMORY_TEXTURE_PIXELS: u64 = 2_500_000;
const MAX_GPU_TEXTURE_EDGE: u32 = 384;
const DEFAULT_ERROR_RETRY: Duration = Duration::from_secs(15);
const MAX_IN_FLIGHT_REQUESTS: usize = 3;
const NETWORK_RESUME_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Debug)]
pub struct ArtworkConfig {
    pub allowed_hosts: BTreeSet<String>,
    pub cache_dir: PathBuf,
    pub max_download_bytes: usize,
    pub max_decoded_pixels: u64,
    pub max_disk_bytes: u64,
    pub max_memory_textures: usize,
    pub max_memory_texture_pixels: u64,
    pub error_retry: Duration,
    allow_http_for_tests: bool,
}

impl ArtworkConfig {
    pub fn preview() -> Self {
        let base_dir = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let cache_dir =
            resolve_cache_dir(std::env::var_os("BRICKWAVE_ARTWORK_CACHE_DIR"), base_dir);
        Self {
            // The mock preview has no approved CDN. Backend/catalog data never
            // changes this list: an integrator must provide exact hosts through
            // `with_approved_artwork_hosts` before any remote request can work.
            allowed_hosts: BTreeSet::new(),
            cache_dir,
            max_download_bytes: DEFAULT_MAX_DOWNLOAD_BYTES,
            max_decoded_pixels: DEFAULT_MAX_DECODED_PIXELS,
            max_disk_bytes: DEFAULT_MAX_DISK_BYTES,
            max_memory_textures: DEFAULT_MAX_MEMORY_TEXTURES,
            max_memory_texture_pixels: DEFAULT_MAX_MEMORY_TEXTURE_PIXELS,
            error_retry: DEFAULT_ERROR_RETRY,
            allow_http_for_tests: false,
        }
    }

    /// Applies a deployment-owned list of exact artwork CDN hostnames. This is
    /// deliberately configuration, not a response to catalog/API data.
    pub fn with_approved_artwork_hosts<I, S>(mut self, hosts: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.allowed_hosts = hosts
            .into_iter()
            .map(|host| normalize_approved_host(host.as_ref()))
            .collect::<Result<_, _>>()?;
        Ok(self)
    }

    #[cfg(test)]
    fn test_config(cache_dir: PathBuf, host: &str) -> Self {
        let mut config = Self::preview();
        config.cache_dir = cache_dir;
        config.allowed_hosts.insert(host.to_ascii_lowercase());
        config.allow_http_for_tests = true;
        config.error_retry = Duration::from_millis(20);
        config
    }
}

fn resolve_cache_dir(configured: Option<OsString>, base_dir: PathBuf) -> PathBuf {
    configured
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            base_dir
                .join("SoundCloudBrickPreview")
                .join("artwork-cache")
        })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArtworkEvent {
    ArtworkReady { url: String },
    ArtworkError { url: String, message: String },
}

#[derive(Clone, Debug)]
struct DecodedArtwork {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

#[derive(Debug)]
enum WorkerRequest {
    Fetch { url: String },
}

#[derive(Debug)]
enum WorkerEvent {
    Ready {
        url: String,
        image: DecodedArtwork,
        source: ArtworkSource,
    },
    Error {
        url: String,
        message: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArtworkSource {
    DiskCache,
    Network,
}

struct TextureSlot {
    texture: TextureHandle,
    last_used: u64,
    pixels: u64,
}

#[derive(Clone, Copy, Debug)]
struct FailureBackoff {
    retry_at: Instant,
    transient_network: bool,
}

/// Event counters are intentionally URL-free. They are useful when diagnosing
/// cache churn without writing private CDN query data to logs.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ArtworkMetrics {
    pub artwork_requests: u64,
    pub artwork_downloads: u64,
    pub artwork_cache_hits: u64,
    pub disk_cache_reads: u64,
    pub texture_creates: u64,
    pub texture_evictions: u64,
    pub texture_reuploads: u64,
}

/// UI-thread owner for deduped requests, GPU texture lifetime and observable
/// `ArtworkReady`/`ArtworkError` events.
pub struct ArtworkManager {
    config: ArtworkConfig,
    ui_ctx: Context,
    request_tx: Sender<WorkerRequest>,
    event_rx: Receiver<WorkerEvent>,
    pending: HashSet<String>,
    failures: HashMap<String, FailureBackoff>,
    decoded: HashMap<String, DecodedArtwork>,
    textures: HashMap<String, TextureSlot>,
    visible_this_frame: HashSet<String>,
    frame_active: bool,
    uploaded_once: HashSet<String>,
    fixture_textures: HashMap<ArtworkId, TextureHandle>,
    events: Vec<ArtworkEvent>,
    access_tick: u64,
    metrics: ArtworkMetrics,
    network_resume_not_before: Option<Instant>,
}

impl ArtworkManager {
    pub fn new(ui_ctx: &Context, config: ArtworkConfig) -> Self {
        let (request_tx, request_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let worker_config = config.clone();
        let repaint_ctx = ui_ctx.clone();
        thread::Builder::new()
            .name("artwork-worker".to_owned())
            .spawn(move || artwork_worker(worker_config, request_rx, event_tx, repaint_ctx))
            .expect("failed to start artwork worker");

        Self {
            config,
            ui_ctx: ui_ctx.clone(),
            request_tx,
            event_rx,
            pending: HashSet::new(),
            failures: HashMap::new(),
            decoded: HashMap::new(),
            textures: HashMap::new(),
            visible_this_frame: HashSet::new(),
            frame_active: false,
            uploaded_once: HashSet::new(),
            fixture_textures: HashMap::new(),
            events: Vec::new(),
            access_tick: 0,
            metrics: ArtworkMetrics::default(),
            network_resume_not_before: None,
        }
    }

    /// Keeps valid CPU/GPU cache entries while clearing only transient
    /// network failures after StockOS wakes Wi-Fi.
    pub fn on_network_resume(&mut self) {
        self.poll();
        let failures_before = self.failures.len();
        self.failures
            .retain(|_, failure| !failure.transient_network);
        let failures_cleared = failures_before.saturating_sub(self.failures.len());
        self.network_resume_not_before = Some(Instant::now() + NETWORK_RESUME_GRACE);
        println!(
            "BRICKWAVE_ARTWORK_WAKE_RECOVERY failures_cleared={} pending={} textures={}",
            failures_cleared,
            self.pending.len(),
            self.textures.len()
        );
    }

    /// Opens one UI frame. Textures used by visible artwork widgets are pinned
    /// until end_frame, preventing the old 8-entry LRU from evicting a row
    /// that is still being painted.
    pub fn begin_frame(&mut self) {
        if self.frame_active {
            self.end_frame();
        }
        self.frame_active = true;
        self.visible_this_frame.clear();
        self.poll();
    }

    /// Closes the UI frame and evicts only textures outside the visible set.
    pub fn end_frame(&mut self) {
        self.poll();
        self.evict_memory_textures();
        self.visible_this_frame.clear();
        self.frame_active = false;
    }

    /// Returns a cached texture if ready. This compatibility path treats the
    /// caller as visible; UI code should use texture_for_visible_url.
    pub fn texture_for_url(&mut self, url: Option<&str>) -> Option<&TextureHandle> {
        self.texture_for_visible_url(url, true)
    }

    /// Requests a URL only when its artwork slot is visible. A cached visible
    /// texture is pinned for the rest of the frame; an off-screen row neither
    /// refreshes its LRU position nor starts a new download.
    pub fn texture_for_visible_url(
        &mut self,
        url: Option<&str>,
        visible: bool,
    ) -> Option<&TextureHandle> {
        self.poll();
        let url = url?.to_owned();
        if visible {
            self.access_tick = self.access_tick.wrapping_add(1);
            self.visible_this_frame.insert(url.clone());
        }

        if self.textures.contains_key(&url) {
            if visible {
                if let Some(slot) = self.textures.get_mut(&url) {
                    slot.last_used = self.access_tick;
                }
            }
            self.metrics.artwork_cache_hits = self.metrics.artwork_cache_hits.saturating_add(1);
            return self.textures.get(&url).map(|slot| &slot.texture);
        }

        if self
            .failures
            .get(&url)
            .is_some_and(|failure| failure.retry_at > Instant::now())
        {
            return None;
        }

        if self
            .network_resume_not_before
            .is_some_and(|deadline| Instant::now() < deadline)
        {
            return None;
        }
        self.network_resume_not_before = None;

        if visible || !self.frame_active {
            self.request_url(url);
        }
        None
    }
    /// Existing locally generated preview covers are fixtures only. Remote
    /// artwork always uses `texture_for_url` and the worker path above.
    pub fn fixture_texture_for(&mut self, id: ArtworkId) -> &TextureHandle {
        self.fixture_textures.entry(id).or_insert_with(|| {
            self.ui_ctx.load_texture(
                format!("preview-artwork-{id:?}"),
                build_preview_image(id),
                TextureOptions::LINEAR,
            )
        })
    }

    pub fn take_events(&mut self) -> Vec<ArtworkEvent> {
        std::mem::take(&mut self.events)
    }

    pub fn memory_texture_count(&self) -> usize {
        self.textures.len()
    }

    pub fn memory_texture_pixels(&self) -> u64 {
        self.textures.values().map(|slot| slot.pixels).sum()
    }

    pub const fn metrics(&self) -> ArtworkMetrics {
        self.metrics
    }

    /// Indicates whether a URL can enter the artwork pipeline under the
    /// deployment-owned allowlist. It does not queue a network request.
    pub fn accepts_url(&self, raw_url: &str) -> bool {
        validate_url(&self.config, raw_url).is_ok()
    }

    pub fn request_url(&mut self, url: String) {
        if self.pending.contains(&url) || self.pending.len() >= MAX_IN_FLIGHT_REQUESTS {
            return;
        }
        if self
            .failures
            .get(&url)
            .is_some_and(|failure| failure.retry_at > Instant::now())
        {
            return;
        }
        if let Err(message) = validate_url(&self.config, &url) {
            self.record_error(url, message);
            return;
        }
        self.pending.insert(url.clone());
        self.metrics.artwork_requests = self.metrics.artwork_requests.saturating_add(1);
        debug_artwork("request", &url);
        if self
            .request_tx
            .send(WorkerRequest::Fetch { url: url.clone() })
            .is_err()
        {
            self.record_error(url, "Artwork worker is unavailable".to_owned());
        }
    }

    pub fn poll(&mut self) {
        self.collect_worker_events();
        self.upload_decoded();
    }

    fn collect_worker_events(&mut self) {
        loop {
            match self.event_rx.try_recv() {
                Ok(WorkerEvent::Ready { url, image, source }) => {
                    self.pending.remove(&url);
                    self.failures.remove(&url);
                    match source {
                        ArtworkSource::DiskCache => {
                            self.metrics.disk_cache_reads =
                                self.metrics.disk_cache_reads.saturating_add(1);
                            self.metrics.artwork_cache_hits =
                                self.metrics.artwork_cache_hits.saturating_add(1);
                            debug_artwork("disk-cache", &url);
                        }
                        ArtworkSource::Network => {
                            self.metrics.artwork_downloads =
                                self.metrics.artwork_downloads.saturating_add(1);
                            debug_artwork("download", &url);
                        }
                    }
                    self.decoded.insert(url.clone(), image);
                    self.events.push(ArtworkEvent::ArtworkReady { url });
                }
                Ok(WorkerEvent::Error { url, message }) => {
                    self.pending.remove(&url);
                    self.record_error(url, message);
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }

    fn upload_decoded(&mut self) {
        let decoded = std::mem::take(&mut self.decoded);
        for (url, image) in decoded {
            let color_image =
                ColorImage::from_rgba_unmultiplied([image.width, image.height], &image.rgba);
            self.access_tick = self.access_tick.wrapping_add(1);
            let texture = self.ui_ctx.load_texture(
                format!("remote-artwork-{}", cache_key(&url)),
                color_image,
                TextureOptions::LINEAR,
            );
            let pixels = (image.width as u64).saturating_mul(image.height as u64);
            let reupload = !self.uploaded_once.insert(url.clone());
            self.metrics.texture_creates = self.metrics.texture_creates.saturating_add(1);
            if reupload {
                self.metrics.texture_reuploads = self.metrics.texture_reuploads.saturating_add(1);
                debug_artwork("texture-reupload", &url);
            } else {
                debug_artwork("texture-create", &url);
            }
            self.textures.insert(
                url,
                TextureSlot {
                    texture,
                    last_used: self.access_tick,
                    pixels,
                },
            );
        }
        if !self.frame_active {
            self.evict_memory_textures();
        }
    }

    fn evict_memory_textures(&mut self) {
        while self.textures.len() > self.config.max_memory_textures
            || self.memory_texture_pixels() > self.config.max_memory_texture_pixels
        {
            let Some(key) = self
                .textures
                .iter()
                .filter(|(url, _)| !self.visible_this_frame.contains(*url))
                .min_by_key(|(_, slot)| slot.last_used)
                .map(|(url, _)| url.clone())
            else {
                // A frame can temporarily show more pixels than the budget.
                // Preserve visible artwork and trim it on a later frame.
                return;
            };
            self.textures.remove(&key);
            self.metrics.texture_evictions = self.metrics.texture_evictions.saturating_add(1);
            debug_artwork("texture-evict", &key);
        }
    }

    fn record_error(&mut self, url: String, message: String) {
        self.failures.insert(
            url.clone(),
            FailureBackoff {
                retry_at: Instant::now() + self.config.error_retry,
                transient_network: message == "Artwork download failed",
            },
        );
        self.events
            .push(ArtworkEvent::ArtworkError { url, message });
    }
}

fn normalize_approved_host(raw_host: &str) -> Result<String, String> {
    let host = raw_host.trim().to_ascii_lowercase();
    if host.is_empty()
        || raw_host != raw_host.trim()
        || host.contains('*')
        || host.contains(['/', '\\', '@', ':'])
    {
        return Err("Approved artwork host must be one exact hostname".to_owned());
    }
    let url = Url::parse(&format!("https://{host}/"))
        .map_err(|_| "Approved artwork host is invalid".to_owned())?;
    if url.host_str() != Some(host.as_str()) || url.port().is_some() {
        return Err("Approved artwork host must be one exact hostname".to_owned());
    }
    Ok(host)
}

fn artwork_worker(
    config: ArtworkConfig,
    request_rx: Receiver<WorkerRequest>,
    event_tx: Sender<WorkerEvent>,
    repaint_ctx: Context,
) {
    let _ = fs::create_dir_all(&config.cache_dir);
    cleanup_partial_files(&config.cache_dir);

    while let Ok(request) = request_rx.recv() {
        match request {
            WorkerRequest::Fetch { url } => {
                let result = load_artwork(&config, &url);
                let event = match result {
                    Ok((image, source)) => WorkerEvent::Ready { url, image, source },
                    Err(message) => WorkerEvent::Error { url, message },
                };
                if event_tx.send(event).is_err() {
                    return;
                }
                repaint_ctx.request_repaint();
            }
        }
    }
}

fn load_artwork(
    config: &ArtworkConfig,
    url: &str,
) -> Result<(DecodedArtwork, ArtworkSource), String> {
    validate_url(config, url)?;
    let cache_path = cache_path(&config.cache_dir, url);
    if let Ok(bytes) = fs::read(&cache_path) {
        match decode_image(&bytes, config.max_decoded_pixels) {
            Ok(image) => return Ok((image, ArtworkSource::DiskCache)),
            Err(_) => {
                let _ = fs::remove_file(&cache_path);
            }
        }
    }

    let bytes = fetch_bytes(config, url)?;
    let image = decode_image(&bytes, config.max_decoded_pixels)?;
    write_atomic(&cache_path, &bytes)?;
    evict_disk_cache(&config.cache_dir, config.max_disk_bytes);
    Ok((image, ArtworkSource::Network))
}

fn validate_url(config: &ArtworkConfig, raw_url: &str) -> Result<Url, String> {
    let url = Url::parse(raw_url).map_err(|_| "Artwork URL is invalid".to_owned())?;
    let secure = url.scheme() == "https";
    let test_http = cfg!(test) && config.allow_http_for_tests && url.scheme() == "http";
    if !secure && !test_http {
        return Err("Artwork URL must use HTTPS".to_owned());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Artwork URL must not contain credentials".to_owned());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "Artwork URL has no host".to_owned())?
        .to_ascii_lowercase();
    if !config.allowed_hosts.contains(&host) {
        return Err("Artwork host is not allowlisted".to_owned());
    }
    Ok(url)
}

fn fetch_bytes(config: &ArtworkConfig, raw_url: &str) -> Result<Vec<u8>, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(8))
        .timeout_write(Duration::from_secs(5))
        .redirects(0)
        .build();
    let mut current = validate_url(config, raw_url)?;

    for _ in 0..=MAX_REDIRECTS {
        let response = match agent.get(current.as_str()).call() {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(_) => return Err("Artwork download failed".to_owned()),
        };
        let status = response.status();
        if (300..400).contains(&status) {
            let location = response
                .header("Location")
                .ok_or_else(|| "Artwork redirect has no location".to_owned())?;
            current = current
                .join(location)
                .map_err(|_| "Artwork redirect URL is invalid".to_owned())?;
            validate_url(config, current.as_str())?;
            continue;
        }
        if status != 200 {
            return Err("Artwork server returned an error".to_owned());
        }
        if response
            .header("Content-Length")
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|size| size > config.max_download_bytes)
        {
            return Err("Artwork download exceeds the byte limit".to_owned());
        }
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take((config.max_download_bytes + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| "Artwork download could not be read".to_owned())?;
        if bytes.len() > config.max_download_bytes {
            return Err("Artwork download exceeds the byte limit".to_owned());
        }
        return Ok(bytes);
    }
    Err("Artwork redirect limit exceeded".to_owned())
}

fn decode_image(bytes: &[u8], max_pixels: u64) -> Result<DecodedArtwork, String> {
    let format = image::guess_format(bytes)
        .map_err(|_| "Artwork data is not a supported image".to_owned())?;
    let reader = ImageReader::with_format(Cursor::new(bytes), format);
    let (width, height) = reader
        .into_dimensions()
        .map_err(|_| "Artwork dimensions could not be read".to_owned())?;
    let pixels = u64::from(width) * u64::from(height);
    if pixels == 0 || pixels > max_pixels {
        return Err("Artwork dimensions exceed the pixel limit".to_owned());
    }
    let decoded = ImageReader::with_format(Cursor::new(bytes), format)
        .decode()
        .map_err(|_| "Artwork image could not be decoded".to_owned())?
        .to_rgba8();
    let decoded = if width.max(height) > MAX_GPU_TEXTURE_EDGE {
        let scale = MAX_GPU_TEXTURE_EDGE as f32 / width.max(height) as f32;
        let target_width = (width as f32 * scale).round().max(1.0) as u32;
        let target_height = (height as f32 * scale).round().max(1.0) as u32;
        image::imageops::resize(
            &decoded,
            target_width,
            target_height,
            image::imageops::FilterType::Triangle,
        )
    } else {
        decoded
    };
    Ok(DecodedArtwork {
        width: decoded.width() as usize,
        height: decoded.height() as usize,
        rgba: decoded.into_raw(),
    })
}

fn cache_key(url: &str) -> String {
    hex::encode(Sha256::digest(url.as_bytes()))
}

/// Opt-in event logging for cache diagnosis. URLs are represented only by a
/// short hash and logs are never emitted per frame.
fn debug_artwork(event: &str, url: &str) {
    if std::env::var_os("BRICKWAVE_ARTWORK_DEBUG").is_none() {
        return;
    }
    let key = cache_key(url);
    eprintln!("ARTWORK event={event} key={}", &key[..12]);
}

fn cache_path(cache_dir: &Path, url: &str) -> PathBuf {
    let key = cache_key(url);
    cache_dir.join(&key[..2]).join(format!("{key}.img"))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Artwork cache path has no parent".to_owned())?;
    fs::create_dir_all(parent)
        .map_err(|_| "Artwork cache directory could not be created".to_owned())?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = path.with_extension(format!("{nonce}.part"));
    let result = (|| {
        let mut file = File::create(&temporary)
            .map_err(|_| "Artwork cache file could not be created".to_owned())?;
        file.write_all(bytes)
            .map_err(|_| "Artwork cache file could not be written".to_owned())?;
        file.sync_all()
            .map_err(|_| "Artwork cache file could not be flushed".to_owned())?;
        fs::rename(&temporary, path)
            .map_err(|_| "Artwork cache file could not be finalized".to_owned())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn cleanup_partial_files(cache_dir: &Path) {
    let Ok(shards) = fs::read_dir(cache_dir) else {
        return;
    };
    for shard in shards.flatten() {
        let Ok(entries) = fs::read_dir(shard.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "part")
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

fn evict_disk_cache(cache_dir: &Path, max_bytes: u64) {
    let mut files = Vec::new();
    let mut total = 0_u64;
    let Ok(shards) = fs::read_dir(cache_dir) else {
        return;
    };
    for shard in shards.flatten() {
        let Ok(entries) = fs::read_dir(shard.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "img") {
                continue;
            }
            if let Ok(metadata) = entry.metadata() {
                total = total.saturating_add(metadata.len());
                files.push((
                    metadata.modified().unwrap_or(UNIX_EPOCH),
                    metadata.len(),
                    path,
                ));
            }
        }
    }
    files.sort_by_key(|(modified, _, _)| *modified);
    for (_, size, path) in files {
        if total <= max_bytes {
            break;
        }
        if fs::remove_file(path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

fn build_preview_image(id: ArtworkId) -> ColorImage {
    const SIZE: usize = 128;
    let (top, bottom, glow) = match id {
        ArtworkId::NightDrive => (
            (23_u8, 16_u8, 38_u8),
            (83_u8, 26_u8, 31_u8),
            Color32::from_rgb(255, 101, 0),
        ),
        ArtworkId::SoftFocus => (
            (13_u8, 36_u8, 47_u8),
            (24_u8, 102_u8, 106_u8),
            Color32::from_rgb(121, 224, 211),
        ),
    };
    let mut image = ColorImage::filled([SIZE, SIZE], Color32::BLACK);
    for y in 0..SIZE {
        let t = y as f32 / (SIZE - 1) as f32;
        for x in 0..SIZE {
            let vignette = (((x as f32 / (SIZE - 1) as f32) - 0.5).abs() * 0.30).min(0.20);
            let blend = (t + vignette).min(1.0);
            image[(x, y)] = Color32::from_rgb(
                lerp(top.0, bottom.0, blend),
                lerp(top.1, bottom.1, blend),
                lerp(top.2, bottom.2, blend),
            );
        }
    }
    let center = (SIZE as f32 * 0.52, SIZE as f32 * 0.47);
    let radius = SIZE as f32 * 0.30;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - center.0;
            let dy = y as f32 - center.1;
            let distance = (dx * dx + dy * dy).sqrt();
            if (distance - radius).abs() < 2.0 {
                image[(x, y)] = glow.gamma_multiply(0.85);
            } else if distance < radius * 0.15 {
                image[(x, y)] = glow;
            }
        }
    }
    image
}

fn lerp(start: u8, end: u8, t: f32) -> u8 {
    (start as f32 + (end as f32 - start as f32) * t).round() as u8
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};

    use super::*;

    struct TestServer {
        origin: String,
        calls: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl TestServer {
        fn start(routes: HashMap<&'static str, Vec<u8>>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            let calls = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let worker_calls = calls.clone();
            let worker_stop = stop.clone();
            let thread = thread::spawn(move || {
                while !worker_stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            worker_calls.fetch_add(1, Ordering::Relaxed);
                            let path = request_path(&mut stream);
                            let response = routes.get(path.as_str()).cloned().unwrap_or_else(|| {
                                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
                            });
                            let _ = stream.write_all(&response);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2))
                        }
                        Err(_) => return,
                    }
                }
            });
            Self {
                origin: format!("http://127.0.0.1:{port}"),
                calls,
                stop,
                thread: Some(thread),
            }
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn request_path(stream: &mut TcpStream) -> String {
        let mut request = [0_u8; 1024];
        let count = stream.read(&mut request).unwrap_or(0);
        String::from_utf8_lossy(&request[..count])
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_owned()
    }

    fn png_fixture(width: u32, height: u32) -> Vec<u8> {
        let image = RgbaImage::from_pixel(width, height, Rgba([255, 101, 0, 255]));
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    fn http_response(body: &[u8]) -> Vec<u8> {
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        response
    }

    fn temp_cache_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("soundcloud-artwork-{label}-{nonce}"))
    }

    fn wait_for_events(manager: &mut ArtworkManager) -> Vec<ArtworkEvent> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            manager.collect_worker_events();
            let events = manager.take_events();
            if !events.is_empty() || Instant::now() >= deadline {
                return events;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn test_manager(config: ArtworkConfig) -> ArtworkManager {
        ArtworkManager::new(&Context::default(), config)
    }

    #[test]
    fn url_validation_requires_https_and_a_configured_host() {
        let config = ArtworkConfig::preview();
        assert!(validate_url(&config, "http://example.com/cover.png").is_err());
        assert!(validate_url(&config, "https://example.com/cover.png").is_err());
    }

    #[test]
    fn deployment_can_select_a_persistent_artwork_cache_directory() {
        let fallback = PathBuf::from("fallback-root");
        assert_eq!(
            super::resolve_cache_dir(
                Some(std::ffi::OsString::from("nextui-artwork-cache")),
                fallback.clone(),
            ),
            PathBuf::from("nextui-artwork-cache")
        );
        assert_eq!(
            super::resolve_cache_dir(None, fallback.clone()),
            fallback
                .join("SoundCloudBrickPreview")
                .join("artwork-cache")
        );
    }

    #[test]
    fn approved_hosts_are_configured_independently_and_reject_wildcards() {
        let config = ArtworkConfig::preview()
            .with_approved_artwork_hosts(["i1.sndcdn.com"])
            .unwrap();
        assert!(validate_url(&config, "https://i1.sndcdn.com/artwork.jpg").is_ok());
        assert!(validate_url(&config, "https://other.sndcdn.com/artwork.jpg").is_err());
        assert!(
            ArtworkConfig::preview()
                .with_approved_artwork_hosts(["*.sndcdn.com"])
                .is_err()
        );
        assert!(
            ArtworkConfig::preview()
                .with_approved_artwork_hosts(["https://i1.sndcdn.com"])
                .is_err()
        );
    }

    #[test]
    fn catalog_url_from_an_unapproved_host_is_rejected_without_a_download() {
        let ctx = Context::default();
        let mut manager = ArtworkManager::new(&ctx, ArtworkConfig::preview());
        let catalog_url = "https://catalog-unapproved.example/cover.png";
        assert!(!manager.accepts_url(catalog_url));
        assert!(manager.texture_for_url(Some(catalog_url)).is_none());
        assert!(matches!(
            manager.take_events().as_slice(),
            [ArtworkEvent::ArtworkError { message, .. }]
                if message == "Artwork host is not allowlisted"
        ));
    }

    #[test]
    fn missing_or_rejected_url_leaves_the_placeholder_path_available() {
        let ctx = Context::default();
        let mut manager = ArtworkManager::new(&ctx, ArtworkConfig::preview());
        assert!(manager.texture_for_url(None).is_none());
        assert!(
            manager
                .texture_for_url(Some("https://untrusted.invalid/cover.png"))
                .is_none()
        );
        assert!(matches!(
            manager.take_events().as_slice(),
            [ArtworkEvent::ArtworkError { .. }]
        ));
    }

    #[test]
    fn worker_deduplicates_a_cache_miss_and_then_hits_disk_cache() {
        let png = png_fixture(2, 2);
        let server = TestServer::start(HashMap::from([("/cover.png", http_response(&png))]));
        let cache_dir = temp_cache_dir("hit");
        let config = ArtworkConfig::test_config(cache_dir.clone(), "127.0.0.1");
        let mut manager = test_manager(config);
        let url = format!("{}/cover.png", server.origin);
        manager.request_url(url.clone());
        manager.request_url(url.clone());
        assert!(matches!(
            wait_for_events(&mut manager).as_slice(),
            [ArtworkEvent::ArtworkReady { .. }]
        ));
        assert_eq!(server.calls.load(Ordering::Relaxed), 1);
        assert_eq!(manager.metrics().artwork_downloads, 1);

        manager.request_url(url);
        assert!(matches!(
            wait_for_events(&mut manager).as_slice(),
            [ArtworkEvent::ArtworkReady { .. }]
        ));
        assert_eq!(server.calls.load(Ordering::Relaxed), 1);
        assert_eq!(manager.metrics().artwork_downloads, 1);
        assert_eq!(manager.metrics().disk_cache_reads, 1);
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn invalid_redirect_and_corrupt_image_return_artwork_error() {
        let server = TestServer::start(HashMap::from([
            ("/redirect", b"HTTP/1.1 302 Found\r\nLocation: http://evil.invalid/cover.png\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()),
            ("/bad.png", b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nnotimage".to_vec()),
        ]));
        let cache_dir = temp_cache_dir("errors");
        let config = ArtworkConfig::test_config(cache_dir.clone(), "127.0.0.1");
        let mut manager = test_manager(config);
        manager.request_url(format!("{}/redirect", server.origin));
        assert!(matches!(
            wait_for_events(&mut manager).as_slice(),
            [ArtworkEvent::ArtworkError { .. }]
        ));
        manager.request_url(format!("{}/bad.png", server.origin));
        assert!(matches!(
            wait_for_events(&mut manager).as_slice(),
            [ArtworkEvent::ArtworkError { .. }]
        ));
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn temporary_error_cache_suppresses_repeated_requests() {
        let server = TestServer::start(HashMap::from([(
            "/bad.png",
            b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nnotimage".to_vec(),
        )]));
        let cache_dir = temp_cache_dir("retry");
        let config = ArtworkConfig::test_config(cache_dir.clone(), "127.0.0.1");
        let mut manager = test_manager(config);
        let url = format!("{}/bad.png", server.origin);
        manager.request_url(url.clone());
        assert!(matches!(
            wait_for_events(&mut manager).as_slice(),
            [ArtworkEvent::ArtworkError { .. }]
        ));
        manager.request_url(url);
        thread::sleep(Duration::from_millis(10));
        assert!(manager.take_events().is_empty());
        assert_eq!(server.calls.load(Ordering::Relaxed), 1);
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn wake_recovery_preserves_textures_and_releases_transient_failures() {
        let cache_dir = temp_cache_dir("wake-recovery");
        let mut manager = test_manager(ArtworkConfig::test_config(cache_dir.clone(), "127.0.0.1"));
        let failed = "http://127.0.0.1/failed.png".to_owned();
        manager.failures.insert(
            failed,
            FailureBackoff {
                retry_at: Instant::now() + Duration::from_secs(20),
                transient_network: true,
            },
        );
        manager.failures.insert(
            "http://127.0.0.1/permanent.png".to_owned(),
            FailureBackoff {
                retry_at: Instant::now() + Duration::from_secs(20),
                transient_network: false,
            },
        );
        manager.fixture_texture_for(ArtworkId::NightDrive);

        manager.on_network_resume();

        assert_eq!(manager.failures.len(), 1);
        assert!(
            manager
                .failures
                .contains_key("http://127.0.0.1/permanent.png")
        );
        assert_eq!(manager.fixture_textures.len(), 1);
        assert!(manager.network_resume_not_before.is_some());
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn ui_thread_keeps_a_bounded_number_of_uploaded_textures() {
        let png = png_fixture(2, 2);
        let server = TestServer::start(HashMap::from([
            ("/one.png", http_response(&png)),
            ("/two.png", http_response(&png)),
        ]));
        let cache_dir = temp_cache_dir("memory");
        let mut config = ArtworkConfig::test_config(cache_dir.clone(), "127.0.0.1");
        config.max_memory_textures = 1;
        let mut manager = test_manager(config);

        manager.request_url(format!("{}/one.png", server.origin));
        let _ = wait_for_events(&mut manager);
        manager.poll();
        assert_eq!(manager.memory_texture_count(), 1);

        manager.request_url(format!("{}/two.png", server.origin));
        let _ = wait_for_events(&mut manager);
        manager.poll();
        assert_eq!(manager.memory_texture_count(), 1);
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn visible_artwork_is_pinned_and_stationary_viewports_do_not_thrash() {
        let cache_dir = temp_cache_dir("viewport-pins");
        let mut config = ArtworkConfig::test_config(cache_dir.clone(), "127.0.0.1");
        config.max_memory_textures = 8;
        config.max_memory_texture_pixels = 1_000;
        let mut manager = test_manager(config);
        let urls: Vec<_> = (0..12)
            .map(|index| format!("http://127.0.0.1/cover-{index}.png"))
            .collect();

        // Simulate twelve completed worker decodes. The UI frame exposes only
        // the first eight rows; the old cache would evict in render order.
        manager.begin_frame();
        for url in &urls {
            manager.decoded.insert(
                url.clone(),
                DecodedArtwork {
                    width: 2,
                    height: 2,
                    rgba: vec![255, 101, 0, 255].repeat(4),
                },
            );
        }
        manager.poll();
        for url in &urls[..8] {
            assert!(manager.texture_for_visible_url(Some(url), true).is_some());
        }
        // Reusing the same cover for another component is a cache hit, not a
        // second upload or download.
        assert!(
            manager
                .texture_for_visible_url(Some(&urls[0]), true)
                .is_some()
        );
        manager.end_frame();

        let after_first_frame = manager.metrics();
        assert_eq!(manager.memory_texture_count(), 8);
        assert_eq!(after_first_frame.texture_creates, 12);
        assert_eq!(after_first_frame.texture_evictions, 4);
        assert_eq!(after_first_frame.texture_reuploads, 0);
        assert!(after_first_frame.artwork_cache_hits >= 9);

        // A stationary viewport must retain every visible texture indefinitely.
        for _ in 0..3 {
            manager.begin_frame();
            for url in &urls[..8] {
                assert!(manager.texture_for_visible_url(Some(url), true).is_some());
            }
            manager.end_frame();
        }
        assert_eq!(manager.memory_texture_count(), 8);
        assert_eq!(
            manager.metrics().texture_creates,
            after_first_frame.texture_creates
        );
        assert_eq!(
            manager.metrics().texture_reuploads,
            after_first_frame.texture_reuploads
        );
        assert_eq!(
            manager.metrics().texture_evictions,
            after_first_frame.texture_evictions
        );

        // Off-screen rows are neither pinned nor fetched merely because egui
        // builds their widgets while a ScrollArea is clipped.
        let requests_before = manager.metrics().artwork_requests;
        manager.begin_frame();
        assert!(
            manager
                .texture_for_visible_url(Some(&urls[8]), false)
                .is_none()
        );
        manager.end_frame();
        assert_eq!(manager.metrics().artwork_requests, requests_before);
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn content_length_limit_rejects_an_oversize_download_before_decode() {
        let png = png_fixture(2, 2);
        let server = TestServer::start(HashMap::from([("/cover.png", http_response(&png))]));
        let cache_dir = temp_cache_dir("download-limit");
        let mut config = ArtworkConfig::test_config(cache_dir.clone(), "127.0.0.1");
        config.max_download_bytes = png.len() - 1;
        let mut manager = test_manager(config);
        manager.request_url(format!("{}/cover.png", server.origin));
        assert!(matches!(
            wait_for_events(&mut manager).as_slice(),
            [ArtworkEvent::ArtworkError { .. }]
        ));
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn worker_cleans_partial_files_rejects_large_images_and_evicts_old_disk_entries() {
        let small = png_fixture(2, 2);
        let wide = png_fixture(2048, 1);
        let server = TestServer::start(HashMap::from([
            ("/one.png", http_response(&small)),
            ("/two.png", http_response(&small)),
            ("/wide.png", http_response(&wide)),
        ]));
        let cache_dir = temp_cache_dir("limits");
        fs::create_dir_all(cache_dir.join("aa")).unwrap();
        fs::write(cache_dir.join("aa").join("interrupted.part"), b"partial").unwrap();
        let mut config = ArtworkConfig::test_config(cache_dir.clone(), "127.0.0.1");
        config.max_decoded_pixels = 1024;
        config.max_disk_bytes = small.len() as u64 + 1;
        let mut manager = test_manager(config);
        manager.request_url(format!("{}/wide.png", server.origin));
        assert!(matches!(
            wait_for_events(&mut manager).as_slice(),
            [ArtworkEvent::ArtworkError { .. }]
        ));
        manager.request_url(format!("{}/one.png", server.origin));
        assert!(matches!(
            wait_for_events(&mut manager).as_slice(),
            [ArtworkEvent::ArtworkReady { .. }]
        ));
        manager.request_url(format!("{}/two.png", server.origin));
        assert!(matches!(
            wait_for_events(&mut manager).as_slice(),
            [ArtworkEvent::ArtworkReady { .. }]
        ));
        thread::sleep(Duration::from_millis(20));
        assert!(!cache_dir.join("aa").join("interrupted.part").exists());
        let disk_files = fs::read_dir(&cache_dir)
            .unwrap()
            .flatten()
            .flat_map(|entry| fs::read_dir(entry.path()).into_iter().flatten().flatten())
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "img")
            })
            .count();
        assert!(disk_files <= 1);
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[test]
    fn decoded_artwork_is_bounded_to_the_display_texture_edge() {
        let bytes = png_fixture(768, 384);
        let image = decode_image(&bytes, 768 * 384).unwrap();
        assert_eq!(image.width, MAX_GPU_TEXTURE_EDGE as usize);
        assert_eq!(image.height, (MAX_GPU_TEXTURE_EDGE / 2) as usize);
        assert!(
            (image.width as u64).saturating_mul(image.height as u64)
                <= (MAX_GPU_TEXTURE_EDGE as u64).pow(2)
        );
    }
}
