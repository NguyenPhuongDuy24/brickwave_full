//! Supervised StockOS MPlayer process for real SoundCloud HLS playback.
//!
//! Signed media URLs are validated twice (backend parsing and here), passed as
//! one argv value, and never written to logs. SoundCloud OAuth credentials are
//! not present in this process.

use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use egui::Context;
use url::Url;

use crate::hls_spool::SessionAudioCache;
use crate::player::QueueEntryId;
use crate::state::TrackId;

const STOCKOS_MPLAYER: &str = "/usr/trimui/bin/mplayer";
const APPROVED_MEDIA_HOSTS: &[&str] = &[
    "playback.media-streaming.soundcloud.cloud",
    "cf-hls-media.sndcdn.com",
];
// Diagnostic cadence for sub-second audio interruptions. These values do not
// alter playback; they only make a missing/sluggish slave response observable.
const POSITION_POLL_INTERVAL: Duration = Duration::from_millis(250);
const POSITION_RESPONSE_TIMEOUT: Duration = Duration::from_millis(750);
const STALL_LOG_COOLDOWN: Duration = Duration::from_secs(5);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
const MPLAYER_CACHE_KIB: &str = "8192";
const MPLAYER_CACHE_MIN_PERCENT: &str = "25";
const MPLAYER_SOFTVOL_MAX_PERCENT: &str = "100";
const MAX_MPLAYER_LINE_BYTES: usize = 16 * 1024;

pub enum PlaybackCommand {
    Load {
        track_id: TrackId,
        entry_id: QueueEntryId,
        format: String,
        media_url: String,
        volume: f32,
        liked: bool,
    },
    Pause,
    Resume,
    Seek(f32),
    SetVolume(f32),
    SetCachePriority {
        track_id: TrackId,
        liked: bool,
    },
    ClearSessionCache,
    Stop,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlaybackEngineEvent {
    Started {
        track_id: TrackId,
        entry_id: QueueEntryId,
    },
    Paused {
        track_id: TrackId,
        entry_id: QueueEntryId,
    },
    Position {
        track_id: TrackId,
        entry_id: QueueEntryId,
        seconds: f32,
    },
    Ended {
        track_id: TrackId,
        entry_id: QueueEntryId,
    },
    Error {
        track_id: Option<TrackId>,
        entry_id: Option<QueueEntryId>,
        message: String,
    },
}

enum WorkerCommand {
    Playback(PlaybackCommand),
    Shutdown,
}

struct OutputLine {
    generation: u64,
    text: String,
}

struct RunningPlayer {
    child: Child,
    stdin: ChildStdin,
    track_id: TrackId,
    entry_id: QueueEntryId,
    generation: u64,
    started: bool,
    paused: bool,
    error_reported: bool,
    load_started_at: Instant,
    startup_deadline: Instant,
    next_position_poll: Instant,
    position_request_pending_since: Option<Instant>,
    last_position_seconds: Option<f32>,
    last_progress_at: Instant,
    last_stall_log_at: Option<Instant>,
    alsa_buffer_size: Option<u64>,
    alsa_period_size: Option<u64>,
    /// Cleared by the first valid position response after a seek restart. An
    /// exit before that response is a failed seek, not a natural track end.
    seek_target_seconds: Option<f32>,
    seek_mismatch_logged: bool,
    local_file: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExitDisposition {
    Ended,
    Error,
    Ignore,
}

enum OutputSignal {
    Started,
    Position(f32),
    AudioError,
    BufferUnderrun,
    AlsaXrun(Option<f32>),
    AlsaBufferSize(u64),
    AlsaPeriodSize(u64),
    Ignore,
}

pub struct PlaybackEngine {
    command_tx: Option<Sender<WorkerCommand>>,
    event_rx: Receiver<PlaybackEngineEvent>,
    worker: Option<JoinHandle<()>>,
}

impl PlaybackEngine {
    pub fn for_current_platform(ui_ctx: &Context) -> Self {
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        {
            return Self::start(ui_ctx, PathBuf::from(STOCKOS_MPLAYER));
        }
        #[cfg(not(all(target_os = "linux", target_arch = "aarch64")))]
        {
            let _ = ui_ctx;
            Self::disabled()
        }
    }

    fn disabled() -> Self {
        let (_event_tx, event_rx) = mpsc::channel();
        Self {
            command_tx: None,
            event_rx,
            worker: None,
        }
    }

    fn start(ui_ctx: &Context, executable: PathBuf) -> Self {
        if !is_executable(&executable) {
            println!("BRICKWAVE_AUDIO_ENGINE state=unavailable reason=mplayer-missing");
            return Self::disabled();
        }
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let repaint = ui_ctx.clone();
        let worker = match thread::Builder::new()
            .name("brickwave-mplayer-worker".to_owned())
            .spawn(move || playback_worker(executable, command_rx, event_tx, repaint))
        {
            Ok(worker) => worker,
            Err(_) => {
                println!("BRICKWAVE_AUDIO_ENGINE state=unavailable reason=worker-start");
                return Self::disabled();
            }
        };
        println!(
            "BRICKWAVE_AUDIO_ENGINE state=ready backend=stockos-mplayer mixer=softvol cache_kib={MPLAYER_CACHE_KIB} cache_min_percent={MPLAYER_CACHE_MIN_PERCENT}"
        );
        Self {
            command_tx: Some(command_tx),
            event_rx,
            worker: Some(worker),
        }
    }

    pub fn is_available(&self) -> bool {
        self.command_tx.is_some()
    }

    pub fn send(&self, command: PlaybackCommand) -> Result<(), ()> {
        self.command_tx
            .as_ref()
            .ok_or(())?
            .send(WorkerCommand::Playback(command))
            .map_err(|_| ())
    }

    pub fn take_events(&self) -> Vec<PlaybackEngineEvent> {
        let mut events = Vec::new();
        loop {
            match self.event_rx.try_recv() {
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return events,
            }
        }
    }

    pub fn shutdown(&mut self) {
        if let Some(sender) = self.command_tx.take() {
            let _ = sender.send(WorkerCommand::Shutdown);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for PlaybackEngine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn playback_worker(
    executable: PathBuf,
    command_rx: Receiver<WorkerCommand>,
    event_tx: Sender<PlaybackEngineEvent>,
    repaint: Context,
) {
    let (line_tx, line_rx) = mpsc::channel::<OutputLine>();
    let mut running: Option<RunningPlayer> = None;
    let mut audio_cache = SessionAudioCache::new();
    let mut generation = 0_u64;
    loop {
        match command_rx.recv_timeout(Duration::from_millis(40)) {
            Ok(WorkerCommand::Playback(command)) => match command {
                PlaybackCommand::Load {
                    track_id,
                    entry_id,
                    format,
                    media_url,
                    volume,
                    liked,
                } => {
                    stop_running(&mut running);
                    generation = generation.wrapping_add(1).max(1);
                    let load_started_at = Instant::now();
                    println!(
                        "BRICKWAVE_PLAYBACK event=TRACK_LOADING track_id={} entry_id={}",
                        track_id.get(),
                        entry_id.get()
                    );
                    match validated_media_url(&media_url).and_then(|remote_url| {
                        let (player_source, local_file) = match audio_cache.prepare(
                            remote_url.clone(),
                            &format,
                            APPROVED_MEDIA_HOSTS,
                            track_id.get(),
                            liked,
                        ) {
                            Ok(path) => (path.to_string_lossy().into_owned(), true),
                            Err(reason) => {
                                println!(
                                    "BRICKWAVE_PLAYBACK event=SPOOL_FALLBACK track_id={} reason={reason}",
                                    track_id.get()
                                );
                                (remote_url.as_str().to_owned(), false)
                            }
                        };
                        spawn_player(
                            &executable,
                            player_source,
                            volume,
                            track_id,
                            entry_id,
                            generation,
                            &line_tx,
                            load_started_at,
                            local_file,
                        )
                    }) {
                        Ok(player) => {
                            running = Some(player);
                        }
                        Err(message) => {
                            println!(
                                "BRICKWAVE_PLAYBACK event=PLAYBACK_ERROR track_id={} reason={}",
                                track_id.get(),
                                message
                            );
                            publish(
                                &event_tx,
                                &repaint,
                                PlaybackEngineEvent::Error {
                                    track_id: Some(track_id),
                                    entry_id: Some(entry_id),
                                    message,
                                },
                            );
                        }
                    }
                }
                PlaybackCommand::Pause => {
                    if let Some(player) = running.as_mut() {
                        if !player.paused && write_slave(&mut player.stdin, "pause\n").is_ok() {
                            player.paused = true;
                            player.position_request_pending_since = None;
                            publish(
                                &event_tx,
                                &repaint,
                                PlaybackEngineEvent::Paused {
                                    track_id: player.track_id,
                                    entry_id: player.entry_id,
                                },
                            );
                        }
                    }
                }
                PlaybackCommand::Resume => {
                    if let Some(player) = running.as_mut() {
                        if player.paused && write_slave(&mut player.stdin, "pause\n").is_ok() {
                            player.paused = false;
                            reset_progress_observation(player);
                            publish(
                                &event_tx,
                                &repaint,
                                PlaybackEngineEvent::Started {
                                    track_id: player.track_id,
                                    entry_id: player.entry_id,
                                },
                            );
                        }
                    }
                }
                PlaybackCommand::Seek(seconds) => {
                    if let Some(player) = running.as_mut() {
                        let seconds = seconds.max(0.0);
                        println!(
                            "BRICKWAVE_PLAYBACK event=SEEK_REQUEST track_id={} entry_id={} target_seconds={seconds:.3}",
                            player.track_id.get(),
                            player.entry_id.get()
                        );
                        if !player.local_file {
                            println!(
                                "BRICKWAVE_PLAYBACK event=SEEK_UNAVAILABLE track_id={} entry_id={} reason=direct-hls-fallback",
                                player.track_id.get(),
                                player.entry_id.get()
                            );
                            if let Some(position) = player.last_position_seconds {
                                publish(
                                    &event_tx,
                                    &repaint,
                                    PlaybackEngineEvent::Position {
                                        track_id: player.track_id,
                                        entry_id: player.entry_id,
                                        seconds: position,
                                    },
                                );
                            }
                        } else if write_slave(&mut player.stdin, &format!("seek {seconds:.3} 2\n"))
                            .is_ok()
                        {
                            player.seek_target_seconds = Some(seconds);
                            player.seek_mismatch_logged = false;
                            reset_progress_observation(player);
                        } else {
                            println!(
                                "BRICKWAVE_PLAYBACK event=SEEK_ERROR track_id={} entry_id={} reason=slave-write",
                                player.track_id.get(),
                                player.entry_id.get()
                            );
                        }
                    }
                }
                PlaybackCommand::SetVolume(volume) => {
                    if let Some(player) = running.as_mut() {
                        let percent = volume_percent(volume);
                        if write_slave(&mut player.stdin, &format!("volume {percent} 1\n")).is_ok()
                        {
                            println!(
                                "BRICKWAVE_PLAYBACK event=VOLUME_SET track_id={} entry_id={} percent={percent}",
                                player.track_id.get(),
                                player.entry_id.get()
                            );
                        }
                    }
                }
                PlaybackCommand::SetCachePriority { track_id, liked } => {
                    audio_cache.set_liked(track_id.get(), liked);
                }
                PlaybackCommand::ClearSessionCache => {
                    stop_running(&mut running);
                    audio_cache.clear("logout");
                }
                PlaybackCommand::Stop => stop_running(&mut running),
            },
            Ok(WorkerCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                stop_running(&mut running);
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        while let Ok(line) = line_rx.try_recv() {
            let Some(player) = running.as_mut() else {
                continue;
            };
            if line.generation != player.generation {
                continue;
            }
            match parse_output_line(&line.text) {
                OutputSignal::Started if !player.started => {
                    player.started = true;
                    reset_progress_observation(player);
                    let startup_ms = player.load_started_at.elapsed().as_millis();
                    println!(
                        "BRICKWAVE_PLAYBACK event=AUDIO_OUTPUT_READY track_id={} startup_ms={startup_ms}",
                        player.track_id.get(),
                    );
                    println!(
                        "BRICKWAVE_PLAYBACK event=TRACK_PLAYING track_id={} entry_id={}",
                        player.track_id.get(),
                        player.entry_id.get()
                    );
                    publish(
                        &event_tx,
                        &repaint,
                        PlaybackEngineEvent::Started {
                            track_id: player.track_id,
                            entry_id: player.entry_id,
                        },
                    );
                }
                OutputSignal::Position(seconds) => {
                    if let Some(target) = player.seek_target_seconds {
                        if seek_position_matches(target, seconds) {
                            player.seek_target_seconds = None;
                            println!(
                                "BRICKWAVE_PLAYBACK event=SEEK_READY track_id={} entry_id={} target_seconds={target:.3} actual_seconds={seconds:.3}",
                                player.track_id.get(),
                                player.entry_id.get()
                            );
                        } else if !player.seek_mismatch_logged {
                            player.seek_mismatch_logged = true;
                            println!(
                                "BRICKWAVE_PLAYBACK event=SEEK_WAIT track_id={} entry_id={} target_seconds={target:.3} actual_seconds={seconds:.3}",
                                player.track_id.get(),
                                player.entry_id.get()
                            );
                        }
                    }
                    observe_position(player, seconds);
                    publish(
                        &event_tx,
                        &repaint,
                        PlaybackEngineEvent::Position {
                            track_id: player.track_id,
                            entry_id: player.entry_id,
                            seconds,
                        },
                    );
                }
                OutputSignal::AudioError if !player.error_reported => {
                    player.error_reported = true;
                    println!(
                        "BRICKWAVE_PLAYBACK event=PLAYBACK_ERROR track_id={} reason=alsa-output",
                        player.track_id.get()
                    );
                    publish(
                        &event_tx,
                        &repaint,
                        PlaybackEngineEvent::Error {
                            track_id: Some(player.track_id),
                            entry_id: Some(player.entry_id),
                            message: "MPlayer could not initialize ALSA audio output".to_owned(),
                        },
                    );
                }
                OutputSignal::BufferUnderrun => {
                    println!(
                        "BRICKWAVE_PLAYBACK event=BUFFER_UNDERRUN track_id={} entry_id={}",
                        player.track_id.get(),
                        player.entry_id.get()
                    );
                }
                OutputSignal::AlsaXrun(minimum_ms) => {
                    if let Some(minimum_ms) = minimum_ms {
                        println!(
                            "BRICKWAVE_PLAYBACK event=ALSA_XRUN track_id={} entry_id={} minimum_ms={minimum_ms:.3}",
                            player.track_id.get(),
                            player.entry_id.get()
                        );
                    } else {
                        println!(
                            "BRICKWAVE_PLAYBACK event=ALSA_XRUN track_id={} entry_id={} minimum_ms=unknown",
                            player.track_id.get(),
                            player.entry_id.get()
                        );
                    }
                }
                OutputSignal::AlsaBufferSize(value) => {
                    if player.alsa_buffer_size != Some(value) {
                        player.alsa_buffer_size = Some(value);
                        println!(
                            "BRICKWAVE_PLAYBACK event=ALSA_CONFIG track_id={} parameter=buffer_size value={value}",
                            player.track_id.get()
                        );
                    }
                }
                OutputSignal::AlsaPeriodSize(value) => {
                    if player.alsa_period_size != Some(value) {
                        player.alsa_period_size = Some(value);
                        println!(
                            "BRICKWAVE_PLAYBACK event=ALSA_CONFIG track_id={} parameter=period_size value={value}",
                            player.track_id.get()
                        );
                    }
                }
                OutputSignal::Started | OutputSignal::AudioError | OutputSignal::Ignore => {}
            }
        }

        let startup_timeout = running
            .as_ref()
            .filter(|player| !player.started && Instant::now() >= player.startup_deadline)
            .map(|player| (player.track_id, player.entry_id));
        if let Some((track_id, entry_id)) = startup_timeout {
            println!(
                "BRICKWAVE_PLAYBACK event=PLAYBACK_ERROR track_id={} reason=start-timeout",
                track_id.get()
            );
            stop_running(&mut running);
            publish(
                &event_tx,
                &repaint,
                PlaybackEngineEvent::Error {
                    track_id: Some(track_id),
                    entry_id: Some(entry_id),
                    message: "MPlayer did not start audio within 60 seconds".to_owned(),
                },
            );
        }

        if let Some(player) = running.as_mut() {
            if player.started && !player.paused {
                let now = Instant::now();
                if player
                    .position_request_pending_since
                    .is_some_and(|started| now.duration_since(started) >= POSITION_RESPONSE_TIMEOUT)
                {
                    maybe_log_stall(player, "no-position-response", now);
                    player.position_request_pending_since = None;
                }
                if player.position_request_pending_since.is_none()
                    && now >= player.next_position_poll
                {
                    if write_slave(&mut player.stdin, "get_time_pos\n").is_ok() {
                        player.position_request_pending_since = Some(now);
                    }
                    player.next_position_poll = now + POSITION_POLL_INTERVAL;
                }
            }
        }

        let exit = running
            .as_mut()
            .and_then(|player| player.child.try_wait().ok().flatten())
            .map(|status| status.success());
        if let Some(success) = exit {
            let player = running.take().expect("running player exists");
            if classify_player_exit(
                success,
                player.started,
                player.error_reported,
                player.seek_target_seconds.is_some(),
            ) == ExitDisposition::Ended
            {
                println!(
                    "BRICKWAVE_PLAYBACK event=TRACK_ENDED track_id={} entry_id={}",
                    player.track_id.get(),
                    player.entry_id.get()
                );
                publish(
                    &event_tx,
                    &repaint,
                    PlaybackEngineEvent::Ended {
                        track_id: player.track_id,
                        entry_id: player.entry_id,
                    },
                );
            } else if classify_player_exit(
                success,
                player.started,
                player.error_reported,
                player.seek_target_seconds.is_some(),
            ) == ExitDisposition::Error
            {
                let reason = if player.seek_target_seconds.is_some() {
                    "seek-exit"
                } else {
                    "mplayer-exit"
                };
                println!(
                    "BRICKWAVE_PLAYBACK event=PLAYBACK_ERROR track_id={} reason={reason}",
                    player.track_id.get(),
                );
                publish(
                    &event_tx,
                    &repaint,
                    PlaybackEngineEvent::Error {
                        track_id: Some(player.track_id),
                        entry_id: Some(player.entry_id),
                        message: if reason == "seek-exit" {
                            "MPlayer could not seek this HLS stream".to_owned()
                        } else {
                            "MPlayer exited before playback completed".to_owned()
                        },
                    },
                );
            }
        }
    }
}

fn spawn_player(
    executable: &Path,
    player_source: String,
    volume: f32,
    track_id: TrackId,
    entry_id: QueueEntryId,
    generation: u64,
    line_tx: &Sender<OutputLine>,
    load_started_at: Instant,
    local_file: bool,
) -> Result<RunningPlayer, String> {
    let mut command = Command::new(executable);
    command
        .args(mplayer_args(&player_source, volume, !local_file))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        use std::os::unix::process::CommandExt;

        let parent_pid = unsafe { libc::getpid() };
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent_pid {
                    libc::raise(libc::SIGTERM);
                }
                Ok(())
            });
        }
    }
    let mut child = command.spawn().map_err(|_| "mplayer-spawn".to_owned())?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "mplayer-stdin".to_owned())?;
    if let Some(stdout) = child.stdout.take() {
        spawn_reader(stdout, generation, line_tx.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_reader(stderr, generation, line_tx.clone());
    }
    let now = Instant::now();
    Ok(RunningPlayer {
        child,
        stdin,
        track_id,
        entry_id,
        generation,
        started: false,
        paused: false,
        error_reported: false,
        load_started_at,
        startup_deadline: load_started_at + STARTUP_TIMEOUT,
        next_position_poll: now + POSITION_POLL_INTERVAL,
        position_request_pending_since: None,
        last_position_seconds: None,
        last_progress_at: now,
        last_stall_log_at: None,
        alsa_buffer_size: None,
        alsa_period_size: None,
        seek_target_seconds: None,
        seek_mismatch_logged: false,
        local_file,
    })
}

fn reset_progress_observation(player: &mut RunningPlayer) {
    let now = Instant::now();
    player.position_request_pending_since = None;
    player.last_position_seconds = None;
    player.last_progress_at = now;
    player.last_stall_log_at = None;
    player.next_position_poll = now;
}

fn observe_position(player: &mut RunningPlayer, seconds: f32) {
    let now = Instant::now();
    player.position_request_pending_since = None;
    let progressed = player
        .last_position_seconds
        .is_none_or(|previous| seconds >= previous + 0.1 || seconds + 1.0 < previous);
    if progressed {
        player.last_position_seconds = Some(seconds);
        player.last_progress_at = now;
        return;
    }
    maybe_log_stall(player, "position-static", now);
}

fn classify_player_exit(
    success: bool,
    started: bool,
    error_reported: bool,
    seek_pending: bool,
) -> ExitDisposition {
    if error_reported {
        ExitDisposition::Ignore
    } else if success && started && !seek_pending {
        ExitDisposition::Ended
    } else {
        ExitDisposition::Error
    }
}

fn seek_position_matches(target: f32, actual: f32) -> bool {
    target.is_finite()
        && actual.is_finite()
        && target >= 0.0
        && actual >= 0.0
        && (target - actual).abs() <= 2.0
}

fn maybe_log_stall(player: &mut RunningPlayer, kind: &str, now: Instant) {
    let stalled_for = now.saturating_duration_since(player.last_progress_at);
    if stalled_for < POSITION_RESPONSE_TIMEOUT
        || player
            .last_stall_log_at
            .is_some_and(|last| now.saturating_duration_since(last) < STALL_LOG_COOLDOWN)
    {
        return;
    }
    player.last_stall_log_at = Some(now);
    println!(
        "BRICKWAVE_PLAYBACK event=PLAYBACK_STALL track_id={} entry_id={} kind={kind} stalled_ms={}",
        player.track_id.get(),
        player.entry_id.get(),
        stalled_for.as_millis()
    );
}

fn spawn_reader<R: std::io::Read + Send + 'static>(
    reader: R,
    generation: u64,
    sender: Sender<OutputLine>,
) {
    let _ = thread::Builder::new()
        .name("brickwave-mplayer-output".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(reader);
            let mut chunk = [0_u8; 4096];
            let mut line = Vec::with_capacity(256);
            let mut overflowed = false;
            loop {
                let count = match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => {
                        if !line.is_empty() && !overflowed {
                            let _ = send_output_line(&sender, generation, &line);
                        }
                        return;
                    }
                    Ok(count) => count,
                };
                for byte in &chunk[..count] {
                    if matches!(*byte, b'\r' | b'\n') {
                        if !line.is_empty()
                            && !overflowed
                            && send_output_line(&sender, generation, &line).is_err()
                        {
                            return;
                        }
                        line.clear();
                        overflowed = false;
                    } else if line.len() < MAX_MPLAYER_LINE_BYTES {
                        line.push(*byte);
                    } else {
                        overflowed = true;
                    }
                }
            }
        });
}

fn send_output_line(sender: &Sender<OutputLine>, generation: u64, bytes: &[u8]) -> Result<(), ()> {
    sender
        .send(OutputLine {
            generation,
            text: String::from_utf8_lossy(bytes).into_owned(),
        })
        .map_err(|_| ())
}

fn stop_running(running: &mut Option<RunningPlayer>) {
    let Some(mut player) = running.take() else {
        return;
    };
    let _ = write_slave(&mut player.stdin, "quit\n");
    for _ in 0..20 {
        if player.child.try_wait().ok().flatten().is_some() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let _ = player.child.kill();
    let _ = player.child.wait();
}

fn write_slave(stdin: &mut ChildStdin, command: &str) -> Result<(), ()> {
    stdin.write_all(command.as_bytes()).map_err(|_| ())?;
    stdin.flush().map_err(|_| ())
}

fn publish(sender: &Sender<PlaybackEngineEvent>, repaint: &Context, event: PlaybackEngineEvent) {
    if sender.send(event).is_ok() {
        repaint.request_repaint();
    }
}

fn validated_media_url(raw: &str) -> Result<Url, String> {
    if raw.len() > 8192 {
        return Err("stream-url-invalid".to_owned());
    }
    let url = Url::parse(raw).map_err(|_| "stream-url-invalid".to_owned())?;
    if url.scheme() != "https"
        || !url
            .host_str()
            .is_some_and(|host| APPROVED_MEDIA_HOSTS.contains(&host))
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !url.path().ends_with(".m3u8")
    {
        return Err("stream-url-rejected".to_owned());
    }
    Ok(url)
}

fn parse_output_line(line: &str) -> OutputSignal {
    let trimmed = line.trim();
    if trimmed.contains("Starting playback") || trimmed.starts_with("A:") {
        return OutputSignal::Started;
    }
    if let Some(value) = trimmed.strip_prefix("ANS_TIME_POSITION=")
        && let Ok(seconds) = value.trim().parse::<f32>()
        && seconds.is_finite()
        && seconds >= 0.0
    {
        return OutputSignal::Position(seconds);
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("could not open/initialize audio device")
        || lower.contains("failed to initialize audio driver")
        || lower.contains("audio: no sound")
    {
        return OutputSignal::AudioError;
    }
    if lower.contains("alsa xrun") {
        let minimum_ms = lower
            .split_once("at least ")
            .and_then(|(_, tail)| tail.split_once(" ms"))
            .and_then(|(value, _)| value.trim().parse::<f32>().ok())
            .filter(|value| value.is_finite() && *value >= 0.0);
        return OutputSignal::AlsaXrun(minimum_ms);
    }
    if let Some(value) = parse_unsigned_after(&lower, "alsa-init: got buffersize=") {
        return OutputSignal::AlsaBufferSize(value);
    }
    if let Some(value) = parse_unsigned_after(&lower, "alsa-init: got period size") {
        return OutputSignal::AlsaPeriodSize(value);
    }
    if lower.contains("cache empty")
        || lower.contains("buffer underrun")
        || lower.contains("underrun(")
    {
        return OutputSignal::BufferUnderrun;
    }
    OutputSignal::Ignore
}

fn parse_unsigned_after(line: &str, marker: &str) -> Option<u64> {
    let (_, tail) = line.split_once(marker)?;
    let digits: String = tail
        .trim_start_matches(|character: char| character == '=' || character.is_whitespace())
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

fn volume_percent(volume: f32) -> u8 {
    if volume.is_finite() {
        (volume.clamp(0.0, 1.0) * 100.0).round() as u8
    } else {
        0
    }
}

fn mplayer_args(media_source: &str, volume: f32, use_network_cache: bool) -> Vec<String> {
    let mut args = vec![
        "-slave".to_owned(),
        "-nolirc".to_owned(),
        "-noconsolecontrols".to_owned(),
        "-identify".to_owned(),
        // Keep MPlayer diagnostics enabled so the StockOS ALSA backend can
        // report xrun/buffer/period evidence. Output is consumed by the parser;
        // signed media URLs and arbitrary raw lines are never copied to logs.
        "-v".to_owned(),
        "-ao".to_owned(),
        "alsa".to_owned(),
        // The StockOS hardware mixer exposes an inverted response on Brick.
        // MPlayer's software mixer gives the UI a monotonic 0..100 scale.
        "-softvol".to_owned(),
        "-softvol-max".to_owned(),
        MPLAYER_SOFTVOL_MAX_PERCENT.to_owned(),
        "-volume".to_owned(),
        volume_percent(volume).to_string(),
    ];
    if use_network_cache {
        // Retained only for the direct-HLS fallback when local spooling fails.
        args.push("-cache".to_owned());
        args.push(MPLAYER_CACHE_KIB.to_owned());
        args.push("-cache-min".to_owned());
        args.push(MPLAYER_CACHE_MIN_PERCENT.to_owned());
    }
    args.push(media_source.to_owned());
    args
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ExitDisposition, OutputSignal, classify_player_exit, mplayer_args, parse_output_line,
        seek_position_matches, validated_media_url, volume_percent,
    };

    #[test]
    fn accepts_only_the_approved_signed_hls_hosts() {
        let aac = "https://playback.media-streaming.soundcloud.cloud/a/playlist.m3u8?Policy=signed";
        let mp3 = "https://cf-hls-media.sndcdn.com/playlist/a.128.mp3/playlist.m3u8?Policy=signed";
        assert!(validated_media_url(aac).is_ok());
        assert!(validated_media_url(mp3).is_ok());
        assert!(
            validated_media_url("http://playback.media-streaming.soundcloud.cloud/a/playlist.m3u8")
                .is_err()
        );
        assert!(validated_media_url("https://evil.example/a/playlist.m3u8").is_err());
        assert!(
            validated_media_url(
                "https://user:pass@playback.media-streaming.soundcloud.cloud/a/playlist.m3u8"
            )
            .is_err()
        );
        assert!(
            validated_media_url("https://playback.media-streaming.soundcloud.cloud/a/audio.mp3")
                .is_err()
        );
    }

    #[test]
    fn parses_only_bounded_mplayer_status_signals() {
        assert!(matches!(
            parse_output_line("Starting playback..."),
            OutputSignal::Started
        ));
        assert!(matches!(
            parse_output_line("A:  0.2 V: 0.0"),
            OutputSignal::Started
        ));
        assert!(
            matches!(parse_output_line("ANS_TIME_POSITION=12.500"), OutputSignal::Position(value) if value == 12.5)
        );
        assert!(matches!(
            parse_output_line("Failed to initialize audio driver 'alsa'"),
            OutputSignal::AudioError
        ));
        assert!(matches!(
            parse_output_line(
                "Cache empty, consider increasing -cache and/or -cache-min. [performance issue]"
            ),
            OutputSignal::BufferUnderrun
        ));
        assert!(matches!(
            parse_output_line("ALSA xrun!!! (at least 37.125 ms long)"),
            OutputSignal::AlsaXrun(Some(value)) if value == 37.125
        ));
        assert!(matches!(
            parse_output_line("ALSA xrun: prepare error: Broken pipe"),
            OutputSignal::AlsaXrun(None)
        ));
        assert!(matches!(
            parse_output_line("alsa-init: got buffersize=65536"),
            OutputSignal::AlsaBufferSize(65_536)
        ));
        assert!(matches!(
            parse_output_line("alsa-init: got period size 4096"),
            OutputSignal::AlsaPeriodSize(4_096)
        ));
        assert!(matches!(
            parse_output_line("https://secret.example"),
            OutputSignal::Ignore
        ));
    }

    #[test]
    fn volume_is_monotonic_and_mplayer_uses_softvol_with_a_larger_preload() {
        assert_eq!(volume_percent(0.0), 0);
        assert_eq!(volume_percent(0.25), 25);
        assert_eq!(volume_percent(0.75), 75);
        assert_eq!(volume_percent(1.0), 100);
        assert_eq!(volume_percent(f32::NAN), 0);

        let url = validated_media_url(
            "https://cf-hls-media.sndcdn.com/playlist/a.128.mp3/playlist.m3u8?Policy=signed",
        )
        .expect("approved test URL");
        let args = mplayer_args(url.as_str(), 0.75, true);
        assert!(args.windows(2).any(|pair| pair == ["-softvol-max", "100"]));
        assert!(args.windows(2).any(|pair| pair == ["-volume", "75"]));
        assert!(args.windows(2).any(|pair| pair == ["-cache", "8192"]));
        assert!(args.windows(2).any(|pair| pair == ["-cache-min", "25"]));
        assert!(args.iter().any(|arg| arg == "-v"));
        assert!(!args.iter().any(|arg| arg == "-quiet"));
        assert_eq!(args.last().map(String::as_str), Some(url.as_str()));
    }

    #[test]
    fn local_spool_disables_network_cache_and_startup_seek() {
        let args = mplayer_args("/tmp/brickwave/track.m4a", 0.5, false);
        assert!(!args.iter().any(|arg| arg == "-cache"));
        assert!(!args.iter().any(|arg| arg == "-ss"));
        assert_eq!(
            args.last().map(String::as_str),
            Some("/tmp/brickwave/track.m4a")
        );
    }

    #[test]
    fn exit_during_seek_is_not_reported_as_track_end() {
        assert_eq!(
            classify_player_exit(true, true, false, true),
            ExitDisposition::Error
        );
        assert_eq!(
            classify_player_exit(true, true, false, false),
            ExitDisposition::Ended
        );
        assert_eq!(
            classify_player_exit(false, false, false, false),
            ExitDisposition::Error
        );
        assert_eq!(
            classify_player_exit(false, false, true, false),
            ExitDisposition::Ignore
        );
    }

    #[test]
    fn seek_requires_a_position_near_the_requested_target() {
        assert!(seek_position_matches(42.0, 41.2));
        assert!(seek_position_matches(0.0, 0.1));
        assert!(!seek_position_matches(143.013, 0.1));
        assert!(!seek_position_matches(43.271, 0.0));
    }
}
