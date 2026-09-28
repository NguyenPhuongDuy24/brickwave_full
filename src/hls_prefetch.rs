//! Bounded loopback HLS prefetcher used by the StockOS MPlayer backend.
//!
//! The signed SoundCloud manifest and segment URLs never leave this module.
//! MPlayer receives a short-lived loopback URL whose playlist contains only
//! loopback segment paths. A small moving window is downloaded before and
//! during playback so HLS network jitter does not directly starve MPlayer.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use url::Url;

const MANIFEST_LIMIT_BYTES: usize = 512 * 1024;
const SEGMENT_LIMIT_BYTES: usize = 12 * 1024 * 1024;
const INITIAL_BUFFER_SECONDS: f32 = 5.0;
const INITIAL_MIN_SEGMENTS: usize = 3;
const LOOKAHEAD_SEGMENTS: usize = 5;
const RETAIN_BEHIND_SEGMENTS: usize = 2;
const MAX_RESIDENT_SEGMENTS: usize = LOOKAHEAD_SEGMENTS + RETAIN_BEHIND_SEGMENTS + 2;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15);
const PREFETCH_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: usize = 3;
const SEGMENT_DOWNLOAD_ATTEMPTS: usize = 3;

#[derive(Clone, Debug)]
struct SegmentSpec {
    url: Url,
    duration_seconds: f32,
    byte_range: Option<ByteRange>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ByteRange {
    start: u64,
    length: u64,
}

#[derive(Debug)]
struct SegmentSlot {
    spec: SegmentSpec,
    data: Option<Arc<Vec<u8>>>,
    error: Option<String>,
    discarded: bool,
}

#[derive(Debug)]
struct SharedState {
    segments: Vec<SegmentSlot>,
    wanted_through: usize,
    served_index: usize,
    stop: bool,
}

struct Shared {
    track_id: u64,
    state: Mutex<SharedState>,
    changed: Condvar,
}

/// Owns the loopback HLS server and bounded download worker for one track.
pub(crate) struct HlsPrefetchProxy {
    local_url: Url,
    stop: Arc<AtomicBool>,
    address: SocketAddr,
    shared: Arc<Shared>,
    server: Option<JoinHandle<()>>,
    downloader: Option<JoinHandle<()>>,
}

impl HlsPrefetchProxy {
    pub(crate) fn start(
        remote_manifest: Url,
        approved_hosts: &'static [&'static str],
        track_id: u64,
    ) -> Result<Self, String> {
        validate_remote_url(&remote_manifest, approved_hosts, true)?;
        println!("BRICKWAVE_PLAYBACK event=PREFETCH_BEGIN track_id={track_id}");
        let agent = Arc::new(
            ureq::AgentBuilder::new()
                .timeout(DOWNLOAD_TIMEOUT)
                .redirects(0)
                .build(),
        );
        let manifest_bytes = fetch_bytes(
            &agent,
            remote_manifest.clone(),
            None,
            MANIFEST_LIMIT_BYTES,
            approved_hosts,
        )?;
        let manifest =
            std::str::from_utf8(&manifest_bytes).map_err(|_| "hls-manifest-utf8".to_owned())?;
        let parsed = parse_media_playlist(manifest, &remote_manifest, approved_hosts)?;
        let initial_through = initial_prefetch_through(&parsed.segments);
        let segment_count = parsed.segments.len();
        let initial_seconds: f32 = parsed.segments[..=initial_through]
            .iter()
            .map(|segment| segment.duration_seconds)
            .sum();

        let shared = Arc::new(Shared {
            track_id,
            state: Mutex::new(SharedState {
                segments: parsed
                    .segments
                    .into_iter()
                    .map(|spec| SegmentSlot {
                        spec,
                        data: None,
                        error: None,
                        discarded: false,
                    })
                    .collect(),
                wanted_through: initial_through,
                served_index: 0,
                stop: false,
            }),
            changed: Condvar::new(),
        });
        let stop = Arc::new(AtomicBool::new(false));

        let downloader = spawn_downloader(
            Arc::clone(&shared),
            Arc::clone(&stop),
            Arc::clone(&agent),
            approved_hosts,
        )?;
        if let Err(error) = wait_for_initial_prefetch(&shared, initial_through) {
            stop.store(true, Ordering::Release);
            if let Ok(mut state) = shared.state.lock() {
                state.stop = true;
                shared.changed.notify_all();
            }
            let _ = downloader.join();
            return Err(error);
        }

        let listener =
            TcpListener::bind(("127.0.0.1", 0)).map_err(|_| "hls-loopback-bind".to_owned())?;
        listener
            .set_nonblocking(true)
            .map_err(|_| "hls-loopback-nonblocking".to_owned())?;
        let address = listener
            .local_addr()
            .map_err(|_| "hls-loopback-address".to_owned())?;
        let token = loopback_token(&remote_manifest, address);
        let local_url = Url::parse(&format!(
            "http://127.0.0.1:{}/{token}/playlist.m3u8",
            address.port()
        ))
        .map_err(|_| "hls-loopback-url".to_owned())?;
        let local_manifest = rewrite_playlist(&parsed.lines, &token);
        let server = match spawn_server(
            listener,
            token,
            local_manifest,
            Arc::clone(&shared),
            Arc::clone(&stop),
        ) {
            Ok(server) => server,
            Err(error) => {
                stop.store(true, Ordering::Release);
                if let Ok(mut state) = shared.state.lock() {
                    state.stop = true;
                    shared.changed.notify_all();
                }
                let _ = downloader.join();
                return Err(error);
            }
        };

        println!(
            "BRICKWAVE_PLAYBACK event=PREFETCH_READY track_id={track_id} segments={} buffered_seconds={:.3} total_segments={segment_count}",
            initial_through + 1,
            initial_seconds
        );

        Ok(Self {
            local_url,
            stop,
            address,
            shared,
            server: Some(server),
            downloader: Some(downloader),
        })
    }

    pub(crate) fn local_url(&self) -> &Url {
        &self.local_url
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Ok(mut state) = self.shared.state.lock() {
            state.stop = true;
            self.shared.changed.notify_all();
        }
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_millis(50));
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
        if let Some(downloader) = self.downloader.take() {
            let _ = downloader.join();
        }
    }
}

impl Drop for HlsPrefetchProxy {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Debug)]
struct ParsedPlaylist {
    lines: Vec<PlaylistLine>,
    segments: Vec<SegmentSpec>,
}

#[derive(Debug)]
enum PlaylistLine {
    Original(String),
    Segment(usize),
}

fn parse_media_playlist(
    manifest: &str,
    base: &Url,
    approved_hosts: &'static [&'static str],
) -> Result<ParsedPlaylist, String> {
    if !manifest
        .lines()
        .next()
        .is_some_and(|line| line.trim() == "#EXTM3U")
    {
        return Err("hls-manifest-header".to_owned());
    }
    if manifest.contains("#EXT-X-STREAM-INF") {
        return Err("hls-master-playlist-unsupported".to_owned());
    }
    if manifest.contains("#EXT-X-KEY") || manifest.contains("#EXT-X-MAP") {
        return Err("hls-encrypted-or-mapped-unsupported".to_owned());
    }

    let mut lines = Vec::new();
    let mut segments = Vec::new();
    let mut duration = None;
    let mut pending_range: Option<(u64, Option<u64>)> = None;
    let mut previous_range_end: Option<u64> = None;

    for raw_line in manifest.lines() {
        let line = raw_line.trim();
        if let Some(value) = line.strip_prefix("#EXTINF:") {
            let seconds = value
                .split(',')
                .next()
                .and_then(|value| value.trim().parse::<f32>().ok())
                .filter(|value| value.is_finite() && *value > 0.0)
                .ok_or_else(|| "hls-segment-duration".to_owned())?;
            duration = Some(seconds);
            lines.push(PlaylistLine::Original(line.to_owned()));
        } else if let Some(value) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            let (length, start) = match value.trim().split_once('@') {
                Some((length, start)) => (length.parse::<u64>().ok(), start.parse::<u64>().ok()),
                None => (value.trim().parse::<u64>().ok(), previous_range_end),
            };
            let length = length
                .filter(|length| *length > 0 && *length <= SEGMENT_LIMIT_BYTES as u64)
                .ok_or_else(|| "hls-byte-range".to_owned())?;
            pending_range = Some((length, start));
        } else if line.is_empty() || line.starts_with('#') {
            lines.push(PlaylistLine::Original(line.to_owned()));
        } else {
            let url = base.join(line).map_err(|_| "hls-segment-url".to_owned())?;
            validate_remote_url(&url, approved_hosts, false)?;
            let byte_range = if let Some((length, start)) = pending_range.take() {
                let start = start.ok_or_else(|| "hls-byte-range-offset".to_owned())?;
                previous_range_end = Some(start.saturating_add(length));
                Some(ByteRange { start, length })
            } else {
                previous_range_end = None;
                None
            };
            let index = segments.len();
            segments.push(SegmentSpec {
                url,
                duration_seconds: duration.take().unwrap_or(0.0),
                byte_range,
            });
            lines.push(PlaylistLine::Segment(index));
        }
    }
    if segments.is_empty() {
        return Err("hls-no-segments".to_owned());
    }
    Ok(ParsedPlaylist { lines, segments })
}

fn initial_prefetch_through(segments: &[SegmentSpec]) -> usize {
    let mut seconds = 0.0;
    for (index, segment) in segments.iter().enumerate() {
        seconds += segment.duration_seconds;
        if index + 1 >= INITIAL_MIN_SEGMENTS && seconds >= INITIAL_BUFFER_SECONDS {
            return index;
        }
    }
    segments.len().saturating_sub(1)
}

fn rewrite_playlist(lines: &[PlaylistLine], token: &str) -> Vec<u8> {
    let mut output = String::new();
    for line in lines {
        match line {
            PlaylistLine::Original(line) => output.push_str(line),
            PlaylistLine::Segment(index) => output.push_str(&format!("/{token}/segment/{index}")),
        }
        output.push('\n');
    }
    output.into_bytes()
}

fn spawn_downloader(
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    agent: Arc<ureq::Agent>,
    approved_hosts: &'static [&'static str],
) -> Result<JoinHandle<()>, String> {
    thread::Builder::new()
        .name("brickwave-hls-prefetch".to_owned())
        .spawn(move || {
            loop {
                let (index, spec) = {
                    let mut state = match shared.state.lock() {
                        Ok(state) => state,
                        Err(_) => return,
                    };
                    loop {
                        if state.stop || stop.load(Ordering::Acquire) {
                            return;
                        }
                        let through = state.wanted_through.min(state.segments.len() - 1);
                        if let Some(index) = (0..=through).find(|index| {
                            state.segments[*index].data.is_none()
                                && state.segments[*index].error.is_none()
                                && !state.segments[*index].discarded
                        }) {
                            break (index, state.segments[index].spec.clone());
                        }
                        state = match shared.changed.wait(state) {
                            Ok(state) => state,
                            Err(_) => return,
                        };
                    }
                };
                let mut result = Err("hls-download-failed".to_owned());
                for attempt in 1..=SEGMENT_DOWNLOAD_ATTEMPTS {
                    result = fetch_bytes(
                        &agent,
                        spec.url.clone(),
                        spec.byte_range,
                        SEGMENT_LIMIT_BYTES,
                        approved_hosts,
                    );
                    if result.is_ok() || stop.load(Ordering::Acquire) {
                        break;
                    }
                    if attempt < SEGMENT_DOWNLOAD_ATTEMPTS {
                        println!(
                            "BRICKWAVE_PLAYBACK event=PREFETCH_RETRY track_id={} index={index} attempt={attempt}",
                            shared.track_id
                        );
                        thread::sleep(Duration::from_millis(250 * attempt as u64));
                    }
                }
                let mut state = match shared.state.lock() {
                    Ok(state) => state,
                    Err(_) => return,
                };
                match result {
                    Ok(data) => state.segments[index].data = Some(Arc::new(data)),
                    Err(message) => {
                        println!(
                            "BRICKWAVE_PLAYBACK event=PREFETCH_ERROR track_id={} index={index} reason={message}",
                            shared.track_id
                        );
                        state.segments[index].error = Some(message);
                    }
                }
                evict_old_segments(&mut state);
                shared.changed.notify_all();
            }
        })
        .map_err(|_| "hls-prefetch-thread".to_owned())
}

fn wait_for_initial_prefetch(shared: &Arc<Shared>, through: usize) -> Result<(), String> {
    let deadline = std::time::Instant::now() + PREFETCH_TIMEOUT;
    let mut state = shared
        .state
        .lock()
        .map_err(|_| "hls-prefetch-state".to_owned())?;
    loop {
        if let Some(error) = state.segments[..=through]
            .iter()
            .find_map(|slot| slot.error.clone())
        {
            state.stop = true;
            shared.changed.notify_all();
            return Err(error);
        }
        if state.segments[..=through]
            .iter()
            .all(|slot| slot.data.is_some())
        {
            return Ok(());
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            state.stop = true;
            shared.changed.notify_all();
            return Err("hls-prefetch-timeout".to_owned());
        }
        let (next, _) = shared
            .changed
            .wait_timeout(state, deadline.saturating_duration_since(now))
            .map_err(|_| "hls-prefetch-state".to_owned())?;
        state = next;
    }
}

fn spawn_server(
    listener: TcpListener,
    token: String,
    manifest: Vec<u8>,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
) -> Result<JoinHandle<()>, String> {
    thread::Builder::new()
        .name("brickwave-hls-loopback".to_owned())
        .spawn(move || {
            while !stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(15)));
                        let _ = serve_request(&mut stream, &token, &manifest, &shared);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => return,
                }
            }
        })
        .map_err(|_| "hls-loopback-thread".to_owned())
}

fn serve_request(
    stream: &mut TcpStream,
    token: &str,
    manifest: &[u8],
    shared: &Arc<Shared>,
) -> Result<(), ()> {
    let mut request_bytes = [0_u8; 4096];
    let count = stream.read(&mut request_bytes).map_err(|_| ())?;
    let request = std::str::from_utf8(&request_bytes[..count]).map_err(|_| ())?;
    let first = request.lines().next().ok_or(())?;
    let mut parts = first.split_whitespace();
    let method = parts.next().ok_or(())?;
    let path = parts.next().ok_or(())?;
    if !matches!(method, "GET" | "HEAD") {
        return send_response(stream, 405, "text/plain", b"", method == "HEAD", None);
    }
    let playlist_path = format!("/{token}/playlist.m3u8");
    if path == playlist_path {
        return send_response(
            stream,
            200,
            "application/vnd.apple.mpegurl",
            manifest,
            method == "HEAD",
            None,
        );
    }
    let prefix = format!("/{token}/segment/");
    let Some(index) = path
        .strip_prefix(&prefix)
        .and_then(|value| value.split('?').next())
        .and_then(|value| value.parse::<usize>().ok())
    else {
        return send_response(stream, 404, "text/plain", b"", method == "HEAD", None);
    };
    let data = wait_for_segment(shared, index).map_err(|_| ())?;
    let requested_range = parse_http_range(request, data.len());
    let (status, body, content_range) = if let Some((start, end)) = requested_range {
        (
            206,
            &data[start..=end],
            Some(format!("bytes {start}-{end}/{}", data.len())),
        )
    } else {
        (200, data.as_slice(), None)
    };
    send_response(
        stream,
        status,
        "application/octet-stream",
        body,
        method == "HEAD",
        content_range.as_deref(),
    )
}

fn wait_for_segment(shared: &Arc<Shared>, index: usize) -> Result<Arc<Vec<u8>>, String> {
    let mut state = shared
        .state
        .lock()
        .map_err(|_| "hls-prefetch-state".to_owned())?;
    if index >= state.segments.len() {
        return Err("hls-segment-index".to_owned());
    }
    if state.segments[index].discarded {
        state.segments[index].discarded = false;
        state.segments[index].error = None;
    }
    state.served_index = index;
    state.wanted_through = state
        .wanted_through
        .max(index.saturating_add(LOOKAHEAD_SEGMENTS))
        .min(state.segments.len() - 1);
    shared.changed.notify_all();
    let mut logged_wait = false;
    loop {
        if let Some(data) = state.segments[index].data.clone() {
            evict_old_segments(&mut state);
            println!(
                "BRICKWAVE_PLAYBACK event=SEGMENT_SERVED track_id={} index={index} buffered_segments={}",
                shared.track_id,
                resident_ahead(&state)
            );
            return Ok(data);
        }
        if let Some(error) = state.segments[index].error.clone() {
            println!(
                "BRICKWAVE_PLAYBACK event=PREFETCH_ERROR track_id={} index={index} reason={error}",
                shared.track_id
            );
            return Err(error);
        }
        if state.stop {
            return Err("hls-prefetch-stopped".to_owned());
        }
        if !logged_wait {
            println!(
                "BRICKWAVE_PLAYBACK event=REFILL_WAIT track_id={} index={index}",
                shared.track_id
            );
            logged_wait = true;
        }
        state = shared
            .changed
            .wait(state)
            .map_err(|_| "hls-prefetch-state".to_owned())?;
    }
}

fn resident_ahead(state: &SharedState) -> usize {
    state.segments[state.served_index..]
        .iter()
        .take_while(|slot| slot.data.is_some())
        .count()
}

fn evict_old_segments(state: &mut SharedState) {
    let retain_from = state.served_index.saturating_sub(RETAIN_BEHIND_SEGMENTS);
    for slot in &mut state.segments[..retain_from] {
        slot.data = None;
        slot.error = None;
        slot.discarded = true;
    }
    let mut resident: VecDeque<usize> = state
        .segments
        .iter()
        .enumerate()
        .filter_map(|(index, slot)| slot.data.is_some().then_some(index))
        .collect();
    while resident.len() > MAX_RESIDENT_SEGMENTS {
        let Some(index) = resident.pop_front() else {
            break;
        };
        if index < state.served_index {
            state.segments[index].data = None;
            state.segments[index].discarded = true;
        } else {
            break;
        }
    }
}

fn send_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    head_only: bool,
    content_range: Option<&str>,
) -> Result<(), ()> {
    let reason = match status {
        200 => "OK",
        206 => "Partial Content",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let mut headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(content_range) = content_range {
        headers.push_str(&format!("Content-Range: {content_range}\r\n"));
    }
    headers.push_str("\r\n");
    stream.write_all(headers.as_bytes()).map_err(|_| ())?;
    if !head_only {
        stream.write_all(body).map_err(|_| ())?;
    }
    stream.flush().map_err(|_| ())
}

fn parse_http_range(request: &str, length: usize) -> Option<(usize, usize)> {
    let value = request.lines().find_map(|line| {
        line.split_once(':')
            .filter(|(name, _)| name.eq_ignore_ascii_case("range"))
            .map(|(_, value)| value.trim())
    })?;
    let bytes = value.strip_prefix("bytes=")?;
    let (start, end) = bytes.split_once('-')?;
    let start = start.parse::<usize>().ok()?;
    let end = if end.is_empty() {
        length.checked_sub(1)?
    } else {
        end.parse::<usize>().ok()?.min(length.checked_sub(1)?)
    };
    (start <= end && end < length).then_some((start, end))
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

fn loopback_token(remote: &Url, address: SocketAddr) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let mut digest = Sha256::new();
    digest.update(remote.as_str().as_bytes());
    digest.update(address.to_string().as_bytes());
    digest.update(nanos.to_le_bytes());
    hex::encode(digest.finalize())[..24].to_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        ByteRange, SegmentSpec, initial_prefetch_through, parse_http_range, parse_media_playlist,
        rewrite_playlist,
    };
    use url::Url;

    const HOSTS: &[&str] = &["media.example"];

    #[test]
    fn rewrites_segments_without_exposing_signed_urls() {
        let base = Url::parse("https://media.example/audio/playlist.m3u8?Policy=secret")
            .expect("base URL");
        let parsed = parse_media_playlist(
            "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2.0,\npart-0.mp3?sig=one\n#EXTINF:2.0,\nhttps://media.example/part-1.mp3?sig=two\n#EXTINF:2.0,\npart-2.mp3\n#EXT-X-ENDLIST\n",
            &base,
            HOSTS,
        )
        .expect("media playlist");
        let rewritten =
            String::from_utf8(rewrite_playlist(&parsed.lines, "opaque")).expect("rewritten UTF-8");
        assert_eq!(parsed.segments.len(), 3);
        assert!(rewritten.contains("/opaque/segment/0"));
        assert!(rewritten.contains("/opaque/segment/2"));
        assert!(!rewritten.contains("Policy=secret"));
        assert!(!rewritten.contains("sig=one"));
    }

    #[test]
    fn converts_hls_byte_ranges_to_independent_segment_requests() {
        let base = Url::parse("https://media.example/audio/playlist.m3u8").unwrap();
        let parsed = parse_media_playlist(
            "#EXTM3U\n#EXTINF:10,\n#EXT-X-BYTERANGE:100@0\nmedia.mp3\n#EXTINF:10,\n#EXT-X-BYTERANGE:120\nmedia.mp3\n#EXT-X-ENDLIST\n",
            &base,
            HOSTS,
        )
        .unwrap();
        assert_eq!(
            parsed.segments[0].byte_range,
            Some(ByteRange {
                start: 0,
                length: 100
            })
        );
        assert_eq!(
            parsed.segments[1].byte_range,
            Some(ByteRange {
                start: 100,
                length: 120
            })
        );
        let rewritten = String::from_utf8(rewrite_playlist(&parsed.lines, "token")).unwrap();
        assert!(!rewritten.contains("#EXT-X-BYTERANGE"));
    }

    #[test]
    fn initial_window_requires_three_segments_and_five_seconds() {
        let url = Url::parse("https://media.example/segment").unwrap();
        let segments: Vec<_> = [1.0, 1.0, 1.0, 2.5]
            .into_iter()
            .map(|duration_seconds| SegmentSpec {
                url: url.clone(),
                duration_seconds,
                byte_range: None,
            })
            .collect();
        assert_eq!(initial_prefetch_through(&segments), 3);
    }

    #[test]
    fn rejects_unapproved_segment_and_unsupported_playlist_features() {
        let base = Url::parse("https://media.example/audio/playlist.m3u8").unwrap();
        assert!(
            parse_media_playlist(
                "#EXTM3U\n#EXTINF:2,\nhttps://evil.example/a.mp3\n",
                &base,
                HOSTS
            )
            .is_err()
        );
        assert!(
            parse_media_playlist(
                "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXTINF:2,\na.ts\n",
                &base,
                HOSTS
            )
            .is_err()
        );
    }

    #[test]
    fn parses_bounded_loopback_range() {
        assert_eq!(
            parse_http_range("GET / HTTP/1.1\r\nRange: bytes=2-5\r\n\r\n", 10),
            Some((2, 5))
        );
        assert_eq!(
            parse_http_range("GET / HTTP/1.1\r\nRange: bytes=8-\r\n\r\n", 10),
            Some((8, 9))
        );
        assert_eq!(
            parse_http_range("GET / HTTP/1.1\r\nRange: bytes=12-\r\n\r\n", 10),
            None
        );
    }
}
