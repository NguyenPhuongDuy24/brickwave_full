//! Downloads one finite SoundCloud HLS VOD into a short-lived local media file.
//!
//! StockOS MPlayer can decode SoundCloud's remote HLS descriptors, but its HLS
//! cache stalls audibly and its absolute seek support returns position zero.
//! SoundCloud currently supplies finite, unencrypted VOD playlists, so the
//! bounded segments can be assembled into a local MP3 or fragmented MP4 file.
//! Signed URLs remain in memory and are never copied to logs or filenames.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use url::Url;

const MANIFEST_LIMIT_BYTES: usize = 512 * 1024;
const RESOURCE_LIMIT_BYTES: usize = 12 * 1024 * 1024;
const SPOOL_LIMIT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SEGMENTS: usize = 2048;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REDIRECTS: usize = 3;
const DOWNLOAD_ATTEMPTS: usize = 3;
const SPOOL_PREFIX: &str = "brickwave-stream-";
const DEFAULT_SESSION_CACHE_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_SESSION_CACHE_ENTRIES: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ByteRange {
    start: u64,
    length: u64,
}

#[derive(Clone, Debug)]
struct MediaResource {
    url: Url,
    byte_range: Option<ByteRange>,
}

#[derive(Debug)]
struct VodPlaylist {
    initialization: Option<MediaResource>,
    segments: Vec<MediaResource>,
    duration_seconds: f32,
}

/// Owns one fully downloaded media file. Dropping it removes the file.
struct HlsSpool {
    path: PathBuf,
}

impl HlsSpool {
    pub(crate) fn prepare(
        remote_manifest: Url,
        format: &str,
        approved_hosts: &'static [&'static str],
        track_id: u64,
    ) -> Result<Self, String> {
        validate_remote_url(&remote_manifest, approved_hosts, true)?;
        if !matches!(format, "hls_mp3_128" | "hls_aac_160") {
            return Err("spool-format-unsupported".to_owned());
        }

        println!("BRICKWAVE_PLAYBACK event=SPOOL_BEGIN track_id={track_id} format={format}");
        let agent = ureq::AgentBuilder::new()
            .timeout(DOWNLOAD_TIMEOUT)
            .redirects(0)
            .build();
        let manifest_bytes = fetch_bytes(
            &agent,
            remote_manifest.clone(),
            None,
            MANIFEST_LIMIT_BYTES,
            approved_hosts,
        )?;
        let manifest =
            std::str::from_utf8(&manifest_bytes).map_err(|_| "hls-manifest-utf8".to_owned())?;
        let playlist = parse_vod_playlist(manifest, &remote_manifest, approved_hosts)?;

        let runtime_dir = runtime_dir()?;
        fs::create_dir_all(&runtime_dir).map_err(|_| "spool-directory-create".to_owned())?;
        let extension = match (format, playlist.initialization.is_some()) {
            ("hls_aac_160", true) => "m4a",
            ("hls_aac_160", false) => "aac",
            _ => "mp3",
        };
        let key = spool_key(&remote_manifest);
        let final_path = runtime_dir.join(format!("{SPOOL_PREFIX}{key}.{extension}"));
        let partial_path = runtime_dir.join(format!("{SPOOL_PREFIX}{key}.{extension}.part"));

        let result = write_spool(&agent, &playlist, &partial_path, approved_hosts, track_id);
        if let Err(error) = result {
            let _ = fs::remove_file(&partial_path);
            return Err(error);
        }
        fs::rename(&partial_path, &final_path).map_err(|_| {
            let _ = fs::remove_file(&partial_path);
            "spool-commit".to_owned()
        })?;
        let bytes = fs::metadata(&final_path)
            .map_err(|_| "spool-metadata".to_owned())?
            .len();
        println!(
            "BRICKWAVE_PLAYBACK event=SPOOL_READY track_id={track_id} segments={} duration_seconds={:.3} bytes={bytes}",
            playlist.segments.len(),
            playlist.duration_seconds
        );
        Ok(Self { path: final_path })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for HlsSpool {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CacheKey {
    track_id: u64,
    format: String,
}

struct CacheEntry {
    spool: HlsSpool,
    bytes: u64,
    last_used: u64,
    liked: bool,
}

/// Bounded audio files reused only for the lifetime of this app session.
///
/// A stable track/format key avoids tying reuse to an expiring signed URL.
/// Liked entries are evicted after ordinary entries, but remain bounded and
/// are deleted on logout, shutdown, or the next startup.
pub(crate) struct SessionAudioCache {
    directory: Option<PathBuf>,
    entries: HashMap<CacheKey, CacheEntry>,
    access_clock: u64,
    max_bytes: u64,
    max_entries: usize,
}

impl SessionAudioCache {
    pub(crate) fn new() -> Self {
        let directory = runtime_dir().ok();
        if let Some(directory) = directory.as_deref()
            && fs::create_dir_all(directory).is_ok()
        {
            cleanup_stale_spools(directory);
        }
        Self {
            directory,
            entries: HashMap::new(),
            access_clock: 0,
            max_bytes: session_cache_bytes(),
            max_entries: DEFAULT_SESSION_CACHE_ENTRIES,
        }
    }

    pub(crate) fn prepare(
        &mut self,
        remote_manifest: Url,
        format: &str,
        approved_hosts: &'static [&'static str],
        track_id: u64,
        liked: bool,
    ) -> Result<PathBuf, String> {
        let key = CacheKey {
            track_id,
            format: format.to_owned(),
        };
        self.access_clock = self.access_clock.wrapping_add(1).max(1);
        if let Some(entry) = self.entries.get_mut(&key) {
            let valid = fs::metadata(entry.spool.path())
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() == entry.bytes);
            if valid {
                entry.last_used = self.access_clock;
                entry.liked = liked;
                println!(
                    "BRICKWAVE_PLAYBACK event=AUDIO_CACHE_HIT track_id={track_id} liked={liked} bytes={}",
                    entry.bytes
                );
                return Ok(entry.spool.path().to_path_buf());
            }
            println!(
                "BRICKWAVE_PLAYBACK event=AUDIO_CACHE_ERROR track_id={track_id} reason=missing-or-changed"
            );
            self.entries.remove(&key);
        }

        println!("BRICKWAVE_PLAYBACK event=AUDIO_CACHE_MISS track_id={track_id} liked={liked}");
        let spool = HlsSpool::prepare(remote_manifest, format, approved_hosts, track_id)?;
        let bytes = fs::metadata(spool.path())
            .map_err(|_| "spool-metadata".to_owned())?
            .len();
        let path = spool.path().to_path_buf();
        self.entries.insert(
            key.clone(),
            CacheEntry {
                spool,
                bytes,
                last_used: self.access_clock,
                liked,
            },
        );
        println!(
            "BRICKWAVE_PLAYBACK event=AUDIO_CACHE_READY track_id={track_id} liked={liked} bytes={bytes}"
        );
        self.evict_to_budget(Some(&key));
        Ok(path)
    }

    pub(crate) fn set_liked(&mut self, track_id: u64, liked: bool) {
        let mut updated = 0_usize;
        for (key, entry) in &mut self.entries {
            if key.track_id == track_id {
                entry.liked = liked;
                updated += 1;
            }
        }
        println!(
            "BRICKWAVE_PLAYBACK event=AUDIO_CACHE_PRIORITY track_id={track_id} liked={liked} entries={updated}"
        );
    }

    pub(crate) fn clear(&mut self, reason: &str) {
        let entries = self.entries.len();
        let bytes = self.total_bytes();
        self.entries.clear();
        if let Some(directory) = self.directory.as_deref() {
            cleanup_stale_spools(directory);
        }
        println!(
            "BRICKWAVE_PLAYBACK event=AUDIO_CACHE_CLEANUP reason={reason} entries={entries} bytes={bytes}"
        );
    }

    fn total_bytes(&self) -> u64 {
        self.entries.values().map(|entry| entry.bytes).sum()
    }

    fn over_budget(&self) -> bool {
        self.entries.len() > self.max_entries || self.total_bytes() > self.max_bytes
    }

    fn evict_to_budget(&mut self, protected: Option<&CacheKey>) {
        while self.over_budget() {
            let candidate = self
                .entries
                .iter()
                .filter(|(key, _)| protected != Some(*key))
                .min_by_key(|(_, entry)| (entry.liked, entry.last_used))
                .map(|(key, _)| key.clone());
            let Some(key) = candidate else {
                break;
            };
            if let Some(entry) = self.entries.remove(&key) {
                println!(
                    "BRICKWAVE_PLAYBACK event=AUDIO_CACHE_EVICT track_id={} liked={} bytes={}",
                    key.track_id, entry.liked, entry.bytes
                );
            }
        }
    }
}

impl Drop for SessionAudioCache {
    fn drop(&mut self) {
        self.clear("shutdown");
    }
}

fn session_cache_bytes() -> u64 {
    std::env::var("BRICKWAVE_AUDIO_CACHE_MIB")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|mib| (64..=1024).contains(mib))
        .and_then(|mib| mib.checked_mul(1024 * 1024))
        .unwrap_or(DEFAULT_SESSION_CACHE_BYTES)
}

fn runtime_dir() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("BRICKWAVE_RUNTIME_DIR")
        && !path.is_empty()
    {
        return Ok(PathBuf::from(path));
    }
    Ok(std::env::temp_dir().join("brickwave"))
}

fn cleanup_stale_spools(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(SPOOL_PREFIX))
        {
            let _ = fs::remove_file(path);
        }
    }
}

fn spool_key(remote: &Url) -> String {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let mut digest = Sha256::new();
    digest.update(remote.as_str().as_bytes());
    digest.update(std::process::id().to_le_bytes());
    digest.update(nonce.to_le_bytes());
    hex::encode(digest.finalize())[..24].to_owned()
}

fn write_spool(
    agent: &ureq::Agent,
    playlist: &VodPlaylist,
    path: &Path,
    approved_hosts: &'static [&'static str],
    track_id: u64,
) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|_| "spool-create".to_owned())?;
    let mut written = 0_u64;

    if let Some(initialization) = &playlist.initialization {
        let bytes = fetch_with_retry(agent, initialization, approved_hosts)?;
        write_bounded(&mut file, &bytes, &mut written)?;
    }
    for (index, segment) in playlist.segments.iter().enumerate() {
        let bytes = fetch_with_retry(agent, segment, approved_hosts)?;
        write_bounded(&mut file, &bytes, &mut written)?;
        let completed = index + 1;
        if completed == playlist.segments.len() || completed % 5 == 0 {
            println!(
                "BRICKWAVE_PLAYBACK event=SPOOL_PROGRESS track_id={track_id} completed={completed} total={} bytes={written}",
                playlist.segments.len()
            );
        }
    }
    file.flush().map_err(|_| "spool-flush".to_owned())?;
    file.sync_all().map_err(|_| "spool-sync".to_owned())
}

fn write_bounded(file: &mut File, bytes: &[u8], written: &mut u64) -> Result<(), String> {
    *written = written
        .checked_add(bytes.len() as u64)
        .filter(|total| *total <= SPOOL_LIMIT_BYTES)
        .ok_or_else(|| "spool-too-large".to_owned())?;
    file.write_all(bytes).map_err(|_| "spool-write".to_owned())
}

fn fetch_with_retry(
    agent: &ureq::Agent,
    resource: &MediaResource,
    approved_hosts: &'static [&'static str],
) -> Result<Vec<u8>, String> {
    let mut last_error = "hls-download-failed".to_owned();
    for _ in 0..DOWNLOAD_ATTEMPTS {
        match fetch_bytes(
            agent,
            resource.url.clone(),
            resource.byte_range,
            RESOURCE_LIMIT_BYTES,
            approved_hosts,
        ) {
            Ok(bytes) => return Ok(bytes),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

fn parse_vod_playlist(
    manifest: &str,
    base: &Url,
    approved_hosts: &'static [&'static str],
) -> Result<VodPlaylist, String> {
    if manifest.len() > MANIFEST_LIMIT_BYTES
        || !manifest
            .lines()
            .next()
            .is_some_and(|line| line.trim() == "#EXTM3U")
    {
        return Err("hls-manifest-header".to_owned());
    }
    if manifest.contains("#EXT-X-STREAM-INF")
        || manifest.contains("#EXT-X-DISCONTINUITY")
        || manifest.lines().any(|line| {
            line.trim()
                .strip_prefix("#EXT-X-KEY:")
                .is_some_and(|attributes| !attributes.contains("METHOD=NONE"))
        })
    {
        return Err("hls-vod-feature-unsupported".to_owned());
    }
    if !manifest.lines().any(|line| line.trim() == "#EXT-X-ENDLIST") {
        return Err("hls-vod-endlist-required".to_owned());
    }

    let mut initialization = None;
    let mut segments = Vec::new();
    let mut duration = None;
    let mut duration_seconds = 0.0_f32;
    let mut pending_range = None;
    let mut previous_range_end = None;

    for raw_line in manifest.lines() {
        let line = raw_line.trim();
        if let Some(attributes) = line.strip_prefix("#EXT-X-MAP:") {
            if initialization.is_some() {
                return Err("hls-map-duplicate".to_owned());
            }
            let uri =
                quoted_attribute(attributes, "URI").ok_or_else(|| "hls-map-uri".to_owned())?;
            let url = base.join(uri).map_err(|_| "hls-map-url".to_owned())?;
            validate_remote_url(&url, approved_hosts, false)?;
            let byte_range = quoted_attribute(attributes, "BYTERANGE")
                .map(parse_explicit_range)
                .transpose()?;
            initialization = Some(MediaResource { url, byte_range });
        } else if let Some(value) = line.strip_prefix("#EXTINF:") {
            let seconds = value
                .split(',')
                .next()
                .and_then(|value| value.trim().parse::<f32>().ok())
                .filter(|value| value.is_finite() && *value > 0.0)
                .ok_or_else(|| "hls-segment-duration".to_owned())?;
            duration = Some(seconds);
        } else if let Some(value) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            pending_range = Some(parse_range(value.trim(), previous_range_end)?);
        } else if line.is_empty() || line.starts_with('#') {
            continue;
        } else {
            let url = base.join(line).map_err(|_| "hls-segment-url".to_owned())?;
            validate_remote_url(&url, approved_hosts, false)?;
            let byte_range = pending_range.take();
            previous_range_end = byte_range.map(|range| range.start.saturating_add(range.length));
            duration_seconds += duration
                .take()
                .ok_or_else(|| "hls-missing-extinf".to_owned())?;
            segments.push(MediaResource { url, byte_range });
            if segments.len() > MAX_SEGMENTS {
                return Err("hls-too-many-segments".to_owned());
            }
        }
    }
    if segments.is_empty() {
        return Err("hls-no-segments".to_owned());
    }
    Ok(VodPlaylist {
        initialization,
        segments,
        duration_seconds,
    })
}

fn quoted_attribute<'a>(attributes: &'a str, name: &str) -> Option<&'a str> {
    let marker = format!("{name}=\"");
    let (_, tail) = attributes.split_once(&marker)?;
    let (value, _) = tail.split_once('"')?;
    (!value.is_empty()).then_some(value)
}

fn parse_explicit_range(value: &str) -> Result<ByteRange, String> {
    parse_range(value, None)
}

fn parse_range(value: &str, inherited_start: Option<u64>) -> Result<ByteRange, String> {
    let (length, start) = match value.split_once('@') {
        Some((length, start)) => (length.parse::<u64>().ok(), start.parse::<u64>().ok()),
        None => (value.parse::<u64>().ok(), inherited_start),
    };
    let length = length
        .filter(|length| *length > 0 && *length <= RESOURCE_LIMIT_BYTES as u64)
        .ok_or_else(|| "hls-byte-range".to_owned())?;
    let start = start.ok_or_else(|| "hls-byte-range-offset".to_owned())?;
    Ok(ByteRange { start, length })
}

fn fetch_bytes(
    agent: &ureq::Agent,
    mut current: Url,
    byte_range: Option<ByteRange>,
    limit: usize,
    approved_hosts: &'static [&'static str],
) -> Result<Vec<u8>, String> {
    for _ in 0..=MAX_REDIRECTS {
        validate_remote_url(&current, approved_hosts, false)?;
        let mut request = agent
            .get(current.as_str())
            .set("Accept-Encoding", "identity");
        if let Some(range) = byte_range {
            let end = range.start.saturating_add(range.length).saturating_sub(1);
            request = request.set("Range", &format!("bytes={}-{}", range.start, end));
        }
        let response = match request.call() {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(_) => return Err("hls-download-failed".to_owned()),
        };
        let status = response.status();
        if (300..400).contains(&status) {
            let location = response
                .header("Location")
                .ok_or_else(|| "hls-redirect-location".to_owned())?;
            current = current
                .join(location)
                .map_err(|_| "hls-redirect-url".to_owned())?;
            validate_remote_url(&current, approved_hosts, false)?;
            continue;
        }
        if !(200..300).contains(&status) {
            return Err(format!("hls-http-{status}"));
        }
        if byte_range.is_some() && status != 206 {
            return Err("hls-byte-range-ignored".to_owned());
        }
        if response
            .header("Content-Length")
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|length| length > limit)
        {
            return Err("hls-response-too-large".to_owned());
        }
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take((limit + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| "hls-response-read".to_owned())?;
        if bytes.len() > limit {
            return Err("hls-response-too-large".to_owned());
        }
        if let Some(range) = byte_range
            && bytes.len() != range.length as usize
        {
            return Err("hls-byte-range-length".to_owned());
        }
        return Ok(bytes);
    }
    Err("hls-too-many-redirects".to_owned())
}

fn validate_remote_url(
    url: &Url,
    approved_hosts: &'static [&'static str],
    require_manifest: bool,
) -> Result<(), String> {
    if url.scheme() != "https"
        || !url
            .host_str()
            .is_some_and(|host| approved_hosts.contains(&host))
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || (require_manifest && !url.path().ends_with(".m3u8"))
    {
        return Err("hls-url-rejected".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ByteRange, CacheEntry, CacheKey, HlsSpool, SessionAudioCache, parse_vod_playlist};
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};
    use url::Url;

    const HOSTS: &[&str] = &["media.example"];

    fn test_directory(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("brickwave-{name}-{nonce}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn empty_test_cache(directory: PathBuf, max_entries: usize) -> SessionAudioCache {
        SessionAudioCache {
            directory: Some(directory),
            entries: HashMap::new(),
            access_clock: 0,
            max_bytes: u64::MAX,
            max_entries,
        }
    }

    fn insert_test_entry(
        cache: &mut SessionAudioCache,
        track_id: u64,
        liked: bool,
        last_used: u64,
    ) -> PathBuf {
        let directory = cache.directory.as_ref().unwrap();
        let path = directory.join(format!("brickwave-stream-{track_id}.mp3"));
        fs::write(&path, [track_id as u8; 8]).unwrap();
        cache.entries.insert(
            CacheKey {
                track_id,
                format: "hls_mp3_128".to_owned(),
            },
            CacheEntry {
                spool: HlsSpool { path: path.clone() },
                bytes: 8,
                last_used,
                liked,
            },
        );
        path
    }

    #[test]
    fn session_cache_reuses_a_stable_track_key_without_downloading_again() {
        let directory = test_directory("cache-hit");
        let mut cache = empty_test_cache(directory.clone(), 4);
        let cached_path = insert_test_entry(&mut cache, 41, false, 1);

        let reused = cache
            .prepare(
                Url::parse("https://media.example/new-signed-url/playlist.m3u8?token=changed")
                    .unwrap(),
                "hls_mp3_128",
                HOSTS,
                41,
                true,
            )
            .unwrap();

        assert_eq!(reused, cached_path);
        assert!(cache.entries.values().next().unwrap().liked);
        cache.clear("test");
        assert!(!cached_path.exists());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn eviction_keeps_liked_tracks_before_newer_unliked_tracks() {
        let directory = test_directory("liked-priority");
        let mut cache = empty_test_cache(directory.clone(), 2);
        let liked_path = insert_test_entry(&mut cache, 1, true, 1);
        let ordinary_old_path = insert_test_entry(&mut cache, 2, false, 2);
        let ordinary_new_path = insert_test_entry(&mut cache, 3, false, 3);

        cache.evict_to_budget(None);

        assert!(liked_path.exists());
        assert!(!ordinary_old_path.exists());
        assert!(ordinary_new_path.exists());
        assert!(cache.entries.keys().any(|key| key.track_id == 1));
        assert!(cache.entries.keys().any(|key| key.track_id == 3));
        cache.clear("test");
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn parses_mp3_vod_segments_and_duration() {
        let base = Url::parse("https://media.example/audio/playlist.m3u8?Policy=secret").unwrap();
        let playlist = parse_vod_playlist(
            "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:2.5,\na.mp3\n#EXTINF:3.5,\nb.mp3\n#EXT-X-ENDLIST\n",
            &base,
            HOSTS,
        )
        .unwrap();
        assert!(playlist.initialization.is_none());
        assert_eq!(playlist.segments.len(), 2);
        assert_eq!(playlist.duration_seconds, 6.0);
    }

    #[test]
    fn parses_fmp4_initialization_without_exposing_it() {
        let base = Url::parse("https://media.example/audio/playlist.m3u8").unwrap();
        let playlist = parse_vod_playlist(
            "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4?token=secret\"\n#EXTINF:10.0,\npart-0.m4s\n#EXT-X-ENDLIST\n",
            &base,
            HOSTS,
        )
        .unwrap();
        assert!(playlist.initialization.is_some());
        assert_eq!(playlist.segments.len(), 1);
    }

    #[test]
    fn parses_segment_and_map_byte_ranges() {
        let base = Url::parse("https://media.example/audio/playlist.m3u8").unwrap();
        let playlist = parse_vod_playlist(
            "#EXTM3U\n#EXT-X-MAP:URI=\"media.mp4\",BYTERANGE=\"20@0\"\n#EXTINF:10,\n#EXT-X-BYTERANGE:100@20\nmedia.mp4\n#EXTINF:10,\n#EXT-X-BYTERANGE:120\nmedia.mp4\n#EXT-X-ENDLIST\n",
            &base,
            HOSTS,
        )
        .unwrap();
        assert_eq!(
            playlist.initialization.unwrap().byte_range,
            Some(ByteRange {
                start: 0,
                length: 20
            })
        );
        assert_eq!(
            playlist.segments[1].byte_range,
            Some(ByteRange {
                start: 120,
                length: 120
            })
        );
    }

    #[test]
    fn rejects_live_encrypted_and_unapproved_playlists() {
        let base = Url::parse("https://media.example/audio/playlist.m3u8").unwrap();
        assert!(parse_vod_playlist("#EXTM3U\n#EXTINF:2,\na.mp3\n", &base, HOSTS).is_err());
        assert!(
            parse_vod_playlist(
                "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXTINF:2,\na.mp3\n#EXT-X-ENDLIST\n",
                &base,
                HOSTS,
            )
            .is_err()
        );
        assert!(
            parse_vod_playlist(
                "#EXTM3U\n#EXTINF:2,\nhttps://evil.example/a.mp3\n#EXT-X-ENDLIST\n",
                &base,
                HOSTS,
            )
            .is_err()
        );
    }

    /// Manual network verification. The signed descriptor is supplied only
    /// through the process environment and is never printed or committed.
    #[test]
    #[ignore]
    fn live_descriptor_spools_without_exposing_its_url() {
        let raw = std::env::var("BRICKWAVE_TEST_MEDIA_URL").expect("test URL env");
        let format = std::env::var("BRICKWAVE_TEST_MEDIA_FORMAT").expect("test format env");
        let remote = Url::parse(&raw).expect("valid test URL");
        let spool = HlsSpool::prepare(
            remote,
            &format,
            &[
                "playback.media-streaming.soundcloud.cloud",
                "cf-hls-media.sndcdn.com",
            ],
            1,
        )
        .expect("live spool");
        assert!(std::fs::metadata(spool.path()).unwrap().len() > 1024);
    }
}
