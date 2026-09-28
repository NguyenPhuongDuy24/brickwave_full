//! Asynchronous SoundCloud waveform sample loading.
//!
//! SoundCloud currently returns either a `.json` sample URL directly or a
//! legacy `.png` URL whose sibling `.json` resource contains the samples. The
//! UI never performs HTTP or JSON parsing in an egui frame.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use egui::Context;
use serde::Deserialize;
use url::Url;

const WAVEFORM_HOST: &str = "wave.sndcdn.com";
const MAX_URL_BYTES: usize = 4096;
const MAX_DOWNLOAD_BYTES: usize = 256 * 1024;
const MAX_SAMPLES: usize = 20_000;
const MAX_REDIRECTS: u32 = 3;
const RETRY_AFTER: Duration = Duration::from_secs(20);
const NETWORK_RESUME_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq)]
pub struct WaveformSamples {
    /// Values normalized to `0.0..=1.0` using the height supplied by
    /// SoundCloud. Rendering mirrors these values around the centre line.
    pub values: Vec<f32>,
}

#[derive(Deserialize)]
struct WaveformWire {
    height: f32,
    samples: Vec<f32>,
}

enum WorkerRequest {
    Fetch { url: String },
    Shutdown,
}

enum WorkerEvent {
    Ready {
        url: String,
        samples: WaveformSamples,
    },
    Error {
        url: String,
    },
}

/// UI-thread owner for request de-duplication and the bounded in-memory sample
/// cache. A waveform is tiny compared with artwork; retaining 32 normalized
/// arrays costs well below one megabyte at SoundCloud's current 1800 samples.
pub struct WaveformManager {
    request_tx: Sender<WorkerRequest>,
    event_rx: Receiver<WorkerEvent>,
    pending: HashSet<String>,
    failures: HashMap<String, Instant>,
    samples: HashMap<String, WaveformSamples>,
    access_order: Vec<String>,
    network_resume_not_before: Option<Instant>,
}

impl WaveformManager {
    pub fn new(ui_ctx: &Context) -> Self {
        let (request_tx, request_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let repaint = ui_ctx.clone();
        thread::Builder::new()
            .name("waveform-worker".to_owned())
            .spawn(move || waveform_worker(request_rx, event_tx, repaint))
            .expect("failed to start waveform worker");
        Self {
            request_tx,
            event_rx,
            pending: HashSet::new(),
            failures: HashMap::new(),
            samples: HashMap::new(),
            access_order: Vec::new(),
            network_resume_not_before: None,
        }
    }

    /// Reopens the network-facing part of the waveform pipeline after
    /// StockOS wakes Wi-Fi. Successfully cached samples stay available; only
    /// temporary failures are forgotten. A short grace period prevents the
    /// first frame after wake from racing the network interface.
    pub fn on_network_resume(&mut self) {
        self.poll();
        let failures_cleared = self.failures.len();
        self.failures.clear();
        self.network_resume_not_before = Some(Instant::now() + NETWORK_RESUME_GRACE);
        println!(
            "BRICKWAVE_WAVEFORM_WAKE_RECOVERY failures_cleared={} pending={} cached={}",
            failures_cleared,
            self.pending.len(),
            self.samples.len()
        );
    }

    pub fn samples_for_url(&mut self, raw_url: Option<&str>) -> Option<&WaveformSamples> {
        self.poll();
        let url = normalize_waveform_url(raw_url?)?;
        if self.samples.contains_key(&url) {
            self.touch(&url);
            return self.samples.get(&url);
        }
        if self
            .network_resume_not_before
            .is_some_and(|deadline| Instant::now() < deadline)
        {
            return None;
        }
        self.network_resume_not_before = None;
        if self
            .failures
            .get(&url)
            .is_some_and(|retry_at| *retry_at > Instant::now())
        {
            return None;
        }
        if self.pending.insert(url.clone()) {
            if self
                .request_tx
                .send(WorkerRequest::Fetch { url: url.clone() })
                .is_err()
            {
                self.pending.remove(&url);
                self.failures.insert(url, Instant::now() + RETRY_AFTER);
                println!("BRICKWAVE_WAVEFORM event=worker-unavailable");
            } else {
                println!("BRICKWAVE_WAVEFORM event=request");
            }
        }
        None
    }

    fn poll(&mut self) {
        loop {
            match self.event_rx.try_recv() {
                Ok(WorkerEvent::Ready { url, samples }) => {
                    self.pending.remove(&url);
                    self.failures.remove(&url);
                    self.samples.insert(url.clone(), samples);
                    self.touch(&url);
                    while self.samples.len() > 32 {
                        if let Some(oldest) = self.access_order.first().cloned() {
                            self.access_order.remove(0);
                            self.samples.remove(&oldest);
                        }
                    }
                    println!(
                        "BRICKWAVE_WAVEFORM event=ready cached={} pending={}",
                        self.samples.len(),
                        self.pending.len()
                    );
                }
                Ok(WorkerEvent::Error { url }) => {
                    self.pending.remove(&url);
                    self.failures.insert(url, Instant::now() + RETRY_AFTER);
                    println!(
                        "BRICKWAVE_WAVEFORM event=error retry_after_seconds={} pending={}",
                        RETRY_AFTER.as_secs(),
                        self.pending.len()
                    );
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }

    fn touch(&mut self, url: &str) {
        self.access_order.retain(|existing| existing != url);
        self.access_order.push(url.to_owned());
    }
}

impl Drop for WaveformManager {
    fn drop(&mut self) {
        let _ = self.request_tx.send(WorkerRequest::Shutdown);
    }
}

fn waveform_worker(
    request_rx: Receiver<WorkerRequest>,
    event_tx: Sender<WorkerEvent>,
    repaint: Context,
) {
    while let Ok(request) = request_rx.recv() {
        let WorkerRequest::Fetch { url } = request else {
            break;
        };
        // Build a fresh agent for each rare waveform request. This avoids
        // reusing a pooled socket that StockOS invalidated while Wi-Fi slept.
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(Duration::from_secs(8))
            .timeout_write(Duration::from_secs(5))
            .redirects(MAX_REDIRECTS)
            .build();
        let event = match fetch_waveform(&agent, &url) {
            Ok(samples) => WorkerEvent::Ready {
                url: url.clone(),
                samples,
            },
            Err(()) => WorkerEvent::Error { url: url.clone() },
        };
        if event_tx.send(event).is_err() {
            break;
        }
        repaint.request_repaint();
    }
}

fn fetch_waveform(agent: &ureq::Agent, url: &str) -> Result<WaveformSamples, ()> {
    let response = agent.get(url).call().map_err(|_| ())?;
    let final_url = normalize_waveform_url(response.get_url()).ok_or(())?;
    if final_url != url {
        // Redirects are accepted only when the final canonical URL still uses
        // the exact reviewed SoundCloud waveform host.
        validate_waveform_url(&final_url).ok_or(())?;
    }
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take((MAX_DOWNLOAD_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() > MAX_DOWNLOAD_BYTES {
        return Err(());
    }
    parse_waveform(&bytes)
}

fn parse_waveform(bytes: &[u8]) -> Result<WaveformSamples, ()> {
    let wire: WaveformWire = serde_json::from_slice(bytes).map_err(|_| ())?;
    if !wire.height.is_finite()
        || wire.height <= 0.0
        || wire.height > 4096.0
        || wire.samples.is_empty()
        || wire.samples.len() > MAX_SAMPLES
        || wire.samples.iter().any(|sample| !sample.is_finite())
    {
        return Err(());
    }
    Ok(WaveformSamples {
        values: wire
            .samples
            .into_iter()
            .map(|sample| (sample / wire.height).clamp(0.0, 1.0))
            .collect(),
    })
}

fn normalize_waveform_url(raw_url: &str) -> Option<String> {
    let mut parsed = validate_waveform_url(raw_url)?;
    let path = parsed.path();
    if path.to_ascii_lowercase().ends_with(".png") {
        let new_path = format!("{}.json", &path[..path.len() - 4]);
        parsed.set_path(&new_path);
    } else if !path.to_ascii_lowercase().ends_with(".json") {
        return None;
    }
    parsed.set_fragment(None);
    Some(parsed.to_string())
}

fn validate_waveform_url(raw_url: &str) -> Option<Url> {
    if raw_url.len() > MAX_URL_BYTES {
        return None;
    }
    let parsed = Url::parse(raw_url).ok()?;
    (parsed.scheme() == "https"
        && parsed.host_str() == Some(WAVEFORM_HOST)
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.port().is_none())
    .then_some(parsed)
}

/// Energy-preserving downsampling for one visible bar.
///
/// Using the largest sample in every bucket makes dense, mastered tracks look
/// like a solid block: one short peak is enough to push the whole visible bar
/// to full height. RMS keeps transients visible while representing the energy
/// across the complete time slice.
pub fn bar_amplitude(samples: &[f32], index: usize, bar_count: usize) -> f32 {
    if samples.is_empty() || bar_count == 0 || index >= bar_count {
        return 0.0;
    }
    let start = index * samples.len() / bar_count;
    let end = ((index + 1) * samples.len() / bar_count)
        .max(start + 1)
        .min(samples.len());
    let bucket = &samples[start.min(samples.len() - 1)..end];
    let mean_square = bucket
        .iter()
        .copied()
        .map(|sample| sample * sample)
        .sum::<f32>()
        / bucket.len() as f32;
    mean_square.sqrt().clamp(0.0, 1.0)
}

/// Maps normalized SoundCloud samples to a useful visual range. SoundCloud
/// waveforms for mastered music spend much of their range near 1.0. The curve
/// restores visible contrast without changing the underlying samples.
pub fn display_amplitude(amplitude: f32) -> f32 {
    amplitude.clamp(0.0, 1.0).powf(1.8)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use egui::Context;

    use super::{
        WaveformManager, WaveformSamples, bar_amplitude, display_amplitude, normalize_waveform_url,
        parse_waveform,
    };

    #[test]
    fn accepts_exact_soundcloud_json_and_converts_png_sibling() {
        assert_eq!(
            normalize_waveform_url("https://wave.sndcdn.com/abc_m.json").as_deref(),
            Some("https://wave.sndcdn.com/abc_m.json")
        );
        assert_eq!(
            normalize_waveform_url("https://wave.sndcdn.com/abc_m.png?x=1").as_deref(),
            Some("https://wave.sndcdn.com/abc_m.json?x=1")
        );
    }

    #[test]
    fn rejects_unapproved_or_credentialed_waveform_urls() {
        assert!(normalize_waveform_url("https://evil.example/a.json").is_none());
        assert!(normalize_waveform_url("http://wave.sndcdn.com/a.json").is_none());
        assert!(normalize_waveform_url("https://u:p@wave.sndcdn.com/a.json").is_none());
        assert!(normalize_waveform_url("https://wave.sndcdn.com/a.gif").is_none());
    }

    #[test]
    fn parses_and_normalizes_real_sample_shape() {
        let parsed =
            parse_waveform(br#"{"width":1800,"height":140,"samples":[0,70,140]}"#).unwrap();
        assert_eq!(parsed.values, vec![0.0, 0.5, 1.0]);
    }

    #[test]
    fn rejects_invalid_or_oversized_sample_shapes() {
        assert!(parse_waveform(br#"{"height":0,"samples":[1]}"#).is_err());
        assert!(parse_waveform(br#"{"height":140,"samples":[]}"#).is_err());
        assert!(parse_waveform(b"not-json").is_err());
    }

    #[test]
    fn downsampling_uses_bucket_energy_and_preserves_bounds() {
        let samples = [0.1, 0.8, 0.2, 0.4];
        assert!((bar_amplitude(&samples, 0, 2) - 0.570_087_7).abs() < 0.0001);
        assert!((bar_amplitude(&samples, 1, 2) - 0.316_227_76).abs() < 0.0001);
        assert_eq!(bar_amplitude(&samples, 2, 2), 0.0);
        assert_eq!(bar_amplitude(&[], 0, 2), 0.0);
    }

    #[test]
    fn display_curve_separates_dense_waveform_heights() {
        assert_eq!(display_amplitude(0.0), 0.0);
        assert_eq!(display_amplitude(1.0), 1.0);
        assert!(display_amplitude(0.75) < 0.62);
        assert!(display_amplitude(0.9) < 0.84);
    }

    #[test]
    fn wake_recovery_preserves_samples_and_releases_transient_failures() {
        let mut manager = WaveformManager::new(&Context::default());
        let cached = "https://wave.sndcdn.com/cached.json".to_owned();
        let failed = "https://wave.sndcdn.com/failed.json".to_owned();
        manager.samples.insert(
            cached.clone(),
            WaveformSamples {
                values: vec![0.25, 0.75],
            },
        );
        manager
            .failures
            .insert(failed, Instant::now() + Duration::from_secs(20));

        manager.on_network_resume();

        assert!(manager.failures.is_empty());
        assert_eq!(manager.samples.get(&cached).unwrap().values, [0.25, 0.75]);
        assert!(manager.network_resume_not_before.is_some());
    }
}
