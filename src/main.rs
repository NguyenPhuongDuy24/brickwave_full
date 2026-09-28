mod artwork;
mod auth_view;
mod backend;
mod battery;
mod hls_spool;
mod icons;
mod mock_engine;
mod playback_engine;
mod player;
mod power_policy;
mod preferences;
mod restore_view;
mod session_store;
mod state;
mod theme;
mod token_service;
#[cfg(all(feature = "trimui-sdl2", target_os = "linux", target_arch = "aarch64"))]
mod trimui_host;
#[cfg(all(feature = "trimui-sdl2", target_os = "linux", target_arch = "aarch64"))]
mod trimui_input;
mod waveform;

#[cfg(all(
    target_os = "linux",
    target_arch = "aarch64",
    not(feature = "trimui-sdl2")
))]
compile_error!("the aarch64 Brick build requires --features trimui-sdl2");

use std::time::{Duration, Instant};

use artwork::{ArtworkConfig, ArtworkManager};
use backend::{AuthState, LiveApiConfig, RemoteWorkerConfig, SoundCloudBackend, UserSession};
use battery::BatteryMonitor;
use egui::{
    self, Align, Align2, Color32, CornerRadius, Frame, Id, Layout, Margin, Rect, Response,
    RichText, Sense, Stroke, Ui, pos2, vec2,
};
use icons::{
    CONTROL_GAP, CONTROL_HITBOX, Glyph, ICON_SIZE_LARGE, ICON_SIZE_MEDIUM, ICON_SIZE_SMALL,
    IconRegistry,
};
use playback_engine::{PlaybackEngine, PlaybackEngineEvent};
use preferences::Preferences;
use state::{
    AppRoute, DataMode, LibraryStatus, LibraryTab, Page, Playlist, PlaylistStatus, RepeatMode,
    SearchStatus, Track, TrackId, UiState, format_position, format_timer_duration,
};
use theme::*;
#[cfg(all(feature = "trimui-sdl2", target_os = "linux", target_arch = "aarch64"))]
use trimui_ui_kit::keyboard::KeyboardControl;
use trimui_ui_kit::keyboard::{KeyboardAction, KeyboardConfig, KeyboardTheme, VirtualKeyboard};
use waveform::{WaveformManager, bar_amplitude, display_amplitude};

const SIDEBAR_WIDTH: f32 = 218.0;
const PLAYER_HEIGHT: f32 = 112.0;
const TOP_CONTROL_HEIGHT: f32 = 38.0;
const TOP_CONTROL_RADIUS: u8 = SMALL_RADIUS;
const HOME_SHELF_COLUMNS: usize = 5;
const HOME_SHELF_GAP: f32 = 10.0;
const TOAST_VISIBLE_DURATION: Duration = Duration::from_secs(5);

#[derive(Default)]
struct ToastLifetime {
    message: Option<String>,
    expires_at: Option<Instant>,
}

impl ToastLifetime {
    fn observe(&mut self, message: Option<&str>, now: Instant) -> bool {
        let Some(message) = message else {
            self.message = None;
            self.expires_at = None;
            return false;
        };
        if self.message.as_deref() != Some(message) {
            self.message = Some(message.to_owned());
            self.expires_at = Some(now + TOAST_VISIBLE_DURATION);
            return false;
        }
        self.expires_at.is_some_and(|deadline| now >= deadline)
    }

    fn remaining(&self, now: Instant) -> Option<Duration> {
        self.expires_at
            .and_then(|deadline| deadline.checked_duration_since(now))
    }

    fn clear(&mut self) {
        self.message = None;
        self.expires_at = None;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StartupMode {
    Live,
    Local,
    Preview,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum KeyboardTarget {
    GlobalSearch,
    PlaylistTitle,
    StopTimerHours,
    StopTimerMinutes,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MinimalPage {
    Player,
    Search,
    Playlists,
    Settings,
}

impl MinimalPage {
    const ALL: [Self; 4] = [Self::Player, Self::Search, Self::Playlists, Self::Settings];

    const fn index(self) -> usize {
        match self {
            Self::Player => 0,
            Self::Search => 1,
            Self::Playlists => 2,
            Self::Settings => 3,
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Player => "Trình phát",
            Self::Search => "Tìm kiếm",
            Self::Playlists => "Danh sách phát",
            Self::Settings => "Cài đặt",
        }
    }

    const fn icon(self) -> Icon {
        match self {
            Self::Player => Icon::Play,
            Self::Search => Icon::Search,
            Self::Playlists => Icon::Playlist,
            Self::Settings => Icon::Settings,
        }
    }
}

#[derive(Debug)]
struct MinimalNavigation {
    page: MinimalPage,
    focus: usize,
}

impl Default for MinimalNavigation {
    fn default() -> Self {
        Self {
            page: MinimalPage::Player,
            focus: 1,
        }
    }
}

impl KeyboardTarget {
    fn field_id(self) -> Id {
        match self {
            Self::GlobalSearch => search_field_id(),
            Self::PlaylistTitle => playlist_title_field_id(),
            Self::StopTimerHours => stop_timer_hours_field_id(),
            Self::StopTimerMinutes => stop_timer_minutes_field_id(),
        }
    }

    const fn log_name(self) -> &'static str {
        match self {
            Self::GlobalSearch => "global-search",
            Self::PlaylistTitle => "playlist-title",
            Self::StopTimerHours => "stop-timer-hours",
            Self::StopTimerMinutes => "stop-timer-minutes",
        }
    }

    fn config(self) -> KeyboardConfig {
        match self {
            Self::GlobalSearch => KeyboardConfig::search(),
            Self::PlaylistTitle => KeyboardConfig {
                submit_label: "Tạo".to_owned(),
            },
            Self::StopTimerHours | Self::StopTimerMinutes => KeyboardConfig {
                submit_label: "Xong".to_owned(),
            },
        }
    }
}

fn startup_mode(value: Option<&str>) -> StartupMode {
    match value {
        Some(value) if value.eq_ignore_ascii_case("preview") => StartupMode::Preview,
        Some(value) if value.eq_ignore_ascii_case("local") => StartupMode::Local,
        // The distributed application is always LIVE unless development mode
        // is requested explicitly. Unknown or missing values cannot expose
        // fixture data by accident.
        _ => StartupMode::Live,
    }
}

#[cfg(test)]
mod startup_mode_tests {
    use super::{StartupMode, startup_mode};

    #[test]
    fn live_is_the_safe_default_and_preview_is_explicit() {
        assert_eq!(startup_mode(None), StartupMode::Live);
        assert_eq!(startup_mode(Some("unexpected")), StartupMode::Live);
        assert_eq!(startup_mode(Some("live")), StartupMode::Live);
        assert_eq!(startup_mode(Some("preview")), StartupMode::Preview);
        assert_eq!(startup_mode(Some("local")), StartupMode::Local);
    }
}

fn main() {
    if let Err(error) = run_application() {
        eprintln!("BRICKWAVE_FATAL {error}");
        std::process::exit(1);
    }
}

fn run_application() -> Result<(), String> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        println!(
            "Brickwave {}\n\
             Usage:\n\
             brickwave                     Start the graphical application\n\
             brickwave --live-search QUERY Search through the configured Worker\n\
             brickwave --help              Show this help\n\
             TrimUI controls: analog=pointer, A=click/drag, D-pad=scroll/keyboard navigation, B=back, MENU=exit confirmation, Y/SELECT=pause, START=play/resume, L1/R1=previous/next",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }
    if arguments
        .iter()
        .any(|argument| argument == "--token-service")
    {
        if let Err(error) = token_service::run_from_environment() {
            return Err(format!("SoundCloud token service failed: {error:?}"));
        }
        return Ok(());
    }
    if let Some(position) = arguments
        .iter()
        .position(|argument| argument == "--live-search")
    {
        let query = arguments.get(position + 1).cloned().unwrap_or_default();
        run_live_search(&query).map_err(|message| format!("LIVE_SEARCH_FAILED: {message}"))?;
        return Ok(());
    }
    if let Some(position) = arguments
        .iter()
        .position(|argument| argument == "--local-live-search")
    {
        let query = arguments.get(position + 1).cloned().unwrap_or_default();
        run_local_live_search(&query)
            .map_err(|message| format!("LOCAL_LIVE_SEARCH_FAILED: {message}"))?;
        return Ok(());
    }

    #[cfg(all(feature = "trimui-sdl2", target_os = "linux", target_arch = "aarch64"))]
    {
        return trimui_host::run();
    }

    #[cfg(not(all(target_os = "linux", target_arch = "aarch64")))]
    {
        let viewport = egui::ViewportBuilder::default()
            .with_inner_size([1024.0, 768.0])
            .with_min_inner_size([1024.0, 768.0])
            .with_max_inner_size([1024.0, 768.0])
            .with_resizable(false)
            .with_title("Brickwave");
        eframe::run_native(
            "Brickwave",
            eframe::NativeOptions {
                viewport,
                ..Default::default()
            },
            Box::new(|cc| Ok(Box::new(BrickwaveApp::new(cc)))),
        )
        .map_err(|error| error.to_string())
    }
}

/// CLI validation for the exact live route used by the UI: BackendCommand →
/// worker → BackendEvent → Catalog → UiState. It prints only count/status,
/// never credentials, tokens, or track metadata.
fn run_live_search(query: &str) -> Result<(), String> {
    let config = RemoteWorkerConfig::from_environment()
        .map_err(|error| format!("remote Worker configuration error: {error:?}"))?;
    let ctx = egui::Context::default();
    let backend = SoundCloudBackend::remote(&ctx, &config)
        .map_err(|error| format!("remote Worker unavailable: {}", error.user_message()))?;
    run_search_with_backend(query, backend)
}

/// The loopback token-service route is retained only for explicit local
/// development. Normal LIVE mode never selects it.
fn run_local_live_search(query: &str) -> Result<(), String> {
    let config = LiveApiConfig::from_environment()
        .map_err(|error| format!("local configuration error: {error:?}"))?;
    let ctx = egui::Context::default();
    let backend = SoundCloudBackend::local(&ctx, &config)
        .map_err(|error| format!("local token service unavailable: {}", error.user_message()))?;
    run_search_with_backend(query, backend)
}

fn run_search_with_backend(query: &str, mut backend: SoundCloudBackend) -> Result<(), String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("provide a non-empty query after --live-search".to_owned());
    }
    let mut state = UiState::default();
    state.set_data_mode(DataMode::Live);
    state.query = query.to_owned();
    state.submit_search();
    for command in state.take_backend_commands() {
        backend
            .send(command)
            .map_err(|error| error.user_message().to_owned())?;
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut received_search_results = false;
    while std::time::Instant::now() < deadline {
        let events = backend.take_events();
        if !events.is_empty() {
            for event in events {
                received_search_results |=
                    matches!(&event, backend::BackendEvent::SearchResults { .. });
                state.apply_backend_event(event);
            }
            if received_search_results {
                println!(
                    "LIVE_SEARCH_OK tracks={} playlists={}",
                    state.search_results().len(),
                    state.search_playlists().len()
                );
                backend.shutdown();
                return Ok(());
            }
            if let Some(error) = state.toast.as_deref() {
                backend.shutdown();
                return Err(error.to_owned());
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    backend.shutdown();
    Err("SoundCloud search timed out".to_owned())
}

struct BrickwaveApp {
    state: UiState,
    app_logo: egui::TextureHandle,
    artwork_manager: ArtworkManager,
    waveform_manager: WaveformManager,
    backend: SoundCloudBackend,
    playback_engine: PlaybackEngine,
    battery_monitor: BatteryMonitor,
    next_auth_poll: Instant,
    persisted_session: Option<UserSession>,
    next_session_store_retry: Instant,
    virtual_keyboard: VirtualKeyboard,
    keyboard_target: Option<KeyboardTarget>,
    keyboard_focus_was: Option<KeyboardTarget>,
    persisted_preferences: Preferences,
    next_preferences_store_retry: Instant,
    exit_confirmation_open: bool,
    exit_requested: bool,
    wake_data_recovery_at: Option<Instant>,
    toast_lifetime: ToastLifetime,
    minimal_navigation: MinimalNavigation,
}

impl BrickwaveApp {
    #[cfg(not(all(target_os = "linux", target_arch = "aarch64")))]
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self::new_with_context(&cc.egui_ctx)
    }

    fn new_with_context(ui_ctx: &egui::Context) -> Self {
        IconRegistry::install_fonts(ui_ctx);
        let mut style = (*ui_ctx.global_style()).clone();
        theme::apply(&mut style);
        ui_ctx.set_global_style(style);
        let mut state = UiState::default();
        let requested_mode = std::env::var("SOUNDCLOUD_MODE").ok();
        let (artwork_config, backend) = match startup_mode(requested_mode.as_deref()) {
            StartupMode::Live => {
                state.set_data_mode(DataMode::Live);
                match RemoteWorkerConfig::from_environment() {
                    Ok(config) => {
                        let artwork_config = ArtworkConfig::preview()
                            .with_approved_artwork_hosts(config.approved_artwork_hosts())
                            .unwrap_or_else(|_| ArtworkConfig::preview());
                        match SoundCloudBackend::remote(ui_ctx, &config) {
                            Ok(backend) => {
                                state.toast = Some("Hãy đăng nhập để tiếp tục".to_owned());
                                (artwork_config, backend)
                            }
                            Err(error) => {
                                state.toast = Some(format!(
                                    "Không thể mở chế độ trực tuyến: {}",
                                    error.user_message()
                                ));
                                (
                                    ArtworkConfig::preview(),
                                    SoundCloudBackend::unconfigured(ui_ctx),
                                )
                            }
                        }
                    }
                    Err(error) => {
                        state.toast = Some(format!("Lỗi cấu hình máy chủ: {error:?}"));
                        (
                            ArtworkConfig::preview(),
                            SoundCloudBackend::unconfigured(ui_ctx),
                        )
                    }
                }
            }
            StartupMode::Local => {
                state.set_data_mode(DataMode::Live);
                match LiveApiConfig::from_environment() {
                    Ok(config) => {
                        let artwork_config = ArtworkConfig::preview()
                            .with_approved_artwork_hosts(config.approved_artwork_hosts())
                            .unwrap_or_else(|_| ArtworkConfig::preview());
                        match SoundCloudBackend::local(ui_ctx, &config) {
                            Ok(backend) => {
                                state.toast = Some(
                                    "Chế độ cục bộ: dịch vụ nội bộ cung cấp dữ liệu tìm kiếm"
                                        .to_owned(),
                                );
                                (artwork_config, backend)
                            }
                            Err(error) => {
                                state.toast = Some(format!(
                                    "Không thể mở chế độ cục bộ: {}",
                                    error.user_message()
                                ));
                                (
                                    ArtworkConfig::preview(),
                                    SoundCloudBackend::unconfigured(ui_ctx),
                                )
                            }
                        }
                    }
                    Err(error) => {
                        state.toast = Some(format!("Lỗi cấu hình cục bộ: {error:?}"));
                        (
                            ArtworkConfig::preview(),
                            SoundCloudBackend::unconfigured(ui_ctx),
                        )
                    }
                }
            }
            StartupMode::Preview => {
                state.toast = Some("Chế độ xem trước: dữ liệu cục bộ".to_owned());
                (
                    ArtworkConfig::preview(),
                    SoundCloudBackend::unconfigured(ui_ctx),
                )
            }
        };
        let persisted_session = if state.data_mode == DataMode::Live {
            match session_store::load() {
                Ok(Some(session)) => {
                    state.restore_user_session(session.clone());
                    // The dedicated restore route owns this state. A toast
                    // would duplicate it and briefly expose the main shell.
                    state.toast = None;
                    Some(session)
                }
                Ok(None) => None,
                Err(_) => {
                    state.toast = Some("Không thể đọc phiên tài khoản đã lưu".to_owned());
                    None
                }
            }
        } else {
            None
        };
        let playback_engine = PlaybackEngine::for_current_platform(ui_ctx);
        state.set_live_playback_enabled(playback_engine.is_available());
        let persisted_preferences = match preferences::load() {
            Ok(preferences) => preferences,
            Err(_) => {
                println!("BRICKWAVE_PREFERENCES_LOAD_ERROR fallback=defaults");
                Preferences::default()
            }
        };
        state.show_battery_percentage = persisted_preferences.show_battery_percentage;
        state.always_keep_screen_on = persisted_preferences.always_keep_screen_on;
        state.minimal_interface = persisted_preferences.minimal_interface;
        let keyboard_theme = KeyboardTheme {
            panel: SIDEBAR_BG,
            panel_border: BORDER,
            key: SURFACE,
            key_hover: WARM,
            key_pressed: ACCENT_HOVER,
            action_key: RAISED,
            selected_key: ACCENT_HOVER,
            selected_border: TEXT,
            confirm_key: ACCENT,
            text: TEXT,
            selected_text: APP_BG,
        };
        Self {
            state,
            app_logo: load_app_logo(ui_ctx),
            artwork_manager: ArtworkManager::new(ui_ctx, artwork_config),
            waveform_manager: WaveformManager::new(ui_ctx),
            backend,
            playback_engine,
            battery_monitor: BatteryMonitor::new(),
            next_auth_poll: Instant::now(),
            persisted_session,
            next_session_store_retry: Instant::now(),
            virtual_keyboard: VirtualKeyboard::new(keyboard_theme),
            keyboard_target: None,
            keyboard_focus_was: None,
            persisted_preferences,
            next_preferences_store_retry: Instant::now(),
            exit_confirmation_open: false,
            exit_requested: false,
            wake_data_recovery_at: None,
            toast_lifetime: ToastLifetime::default(),
            minimal_navigation: MinimalNavigation::default(),
        }
    }

    fn service_toast_lifetime(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        if self
            .toast_lifetime
            .observe(self.state.toast.as_deref(), now)
        {
            self.state.toast = None;
            self.toast_lifetime.clear();
            println!("BRICKWAVE_TOAST state=expired visible_seconds=5");
            return;
        }
        if let Some(remaining) = self.toast_lifetime.remaining(now) {
            ctx.request_repaint_after(remaining);
        }
    }

    #[cfg(all(feature = "trimui-sdl2", target_os = "linux", target_arch = "aarch64"))]
    fn on_stockos_wake(&mut self, ctx: &egui::Context) {
        // First consume results that completed while rendering was suspended,
        // then release only transient media failures. Account requests wait
        // for the same two-second Wi-Fi grace period.
        self.service_workers();
        self.waveform_manager.on_network_resume();
        self.artwork_manager.on_network_resume();
        self.wake_data_recovery_at = Some(Instant::now() + Duration::from_secs(2));
        ctx.request_repaint();
        println!("BRICKWAVE_WAKE_RECOVERY state=scheduled delay_ms=2000");
    }

    #[cfg(all(feature = "trimui-sdl2", target_os = "linux", target_arch = "aarch64"))]
    fn keyboard_is_open(&self) -> bool {
        self.virtual_keyboard.is_open()
    }

    #[cfg(all(feature = "trimui-sdl2", target_os = "linux", target_arch = "aarch64"))]
    fn handle_keyboard_control(&mut self, control: KeyboardControl) -> bool {
        let consumed = self.virtual_keyboard.handle_control(control);
        if consumed {
            match control {
                KeyboardControl::DpadX(_) | KeyboardControl::DpadY(_) => {
                    let (row, column) = self.virtual_keyboard.selected_key();
                    println!("BRICKWAVE_KEYBOARD_NAV row={row} column={column}");
                }
                KeyboardControl::Back => {
                    println!("BRICKWAVE_KEYBOARD_CLOSE reason=button-b")
                }
                KeyboardControl::Primary(_) => {}
            }
        }
        consumed
    }

    fn apply_keyboard_actions(&mut self, ctx: &egui::Context) {
        let target = self.keyboard_target.unwrap_or(KeyboardTarget::GlobalSearch);
        let field_id = target.field_id();
        for action in self.virtual_keyboard.take_actions() {
            match action {
                KeyboardAction::Insert(text) => {
                    ctx.memory_mut(|memory| memory.request_focus(field_id));
                    ctx.input_mut(|input| input.events.push(egui::Event::Text(text)));
                    println!("BRICKWAVE_KEYBOARD_INPUT target={}", target.log_name());
                }
                KeyboardAction::Backspace => {
                    ctx.memory_mut(|memory| memory.request_focus(field_id));
                    ctx.input_mut(|input| {
                        input.events.push(egui::Event::Key {
                            key: egui::Key::Backspace,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::NONE,
                        });
                    });
                    println!("BRICKWAVE_KEYBOARD_BACKSPACE target={}", target.log_name());
                }
                KeyboardAction::Clear => {
                    match target {
                        KeyboardTarget::GlobalSearch => self.state.clear_search_input(),
                        KeyboardTarget::PlaylistTitle => self.state.clear_new_playlist_title(),
                        KeyboardTarget::StopTimerHours => self.state.clear_stop_timer_hours(),
                        KeyboardTarget::StopTimerMinutes => self.state.clear_stop_timer_minutes(),
                    }
                    ctx.memory_mut(|memory| memory.request_focus(field_id));
                    println!(
                        "BRICKWAVE_KEYBOARD_INPUT target={} kind=clear",
                        target.log_name()
                    );
                }
                KeyboardAction::Submit => {
                    ctx.memory_mut(|memory| memory.surrender_focus(field_id));
                    match target {
                        KeyboardTarget::GlobalSearch => {
                            if self.state.query.trim().is_empty() {
                                self.state.toast = Some("Hãy nhập nội dung cần tìm".to_owned());
                                println!(
                                    "BRICKWAVE_KEYBOARD_ERROR target=global-search reason=empty-submit"
                                );
                            } else {
                                self.state.submit_search();
                                println!("BRICKWAVE_KEYBOARD_SUBMIT target=global-search");
                            }
                        }
                        KeyboardTarget::PlaylistTitle => {
                            self.state.submit_create_playlist();
                            println!("BRICKWAVE_KEYBOARD_SUBMIT target=playlist-title");
                        }
                        KeyboardTarget::StopTimerHours | KeyboardTarget::StopTimerMinutes => {
                            self.state.sanitize_stop_timer_inputs();
                            println!(
                                "BRICKWAVE_KEYBOARD_SUBMIT target={} kind=done",
                                target.log_name()
                            );
                        }
                    }
                }
                KeyboardAction::Close => {
                    ctx.memory_mut(|memory| memory.surrender_focus(field_id));
                    println!("BRICKWAVE_KEYBOARD_CLOSE reason=keyboard-action");
                }
            }
        }
    }

    fn sync_persisted_session(&mut self) {
        let current = self.state.user_session().cloned();
        if current == self.persisted_session {
            return;
        }
        if Instant::now() < self.next_session_store_retry {
            return;
        }
        match current {
            Some(session) => match session_store::save(&session) {
                Ok(()) => {
                    self.persisted_session = Some(session);
                    self.next_session_store_retry = Instant::now();
                }
                Err(_) => {
                    self.next_session_store_retry = Instant::now() + Duration::from_secs(5);
                    self.state.toast = Some("Không thể lưu phiên tài khoản".to_owned());
                }
            },
            None => match session_store::clear() {
                Ok(()) => {
                    self.persisted_session = None;
                    self.next_session_store_retry = Instant::now();
                }
                Err(_) => {
                    self.next_session_store_retry = Instant::now() + Duration::from_secs(5);
                    self.state.toast = Some("Không thể xóa phiên tài khoản đã lưu".to_owned());
                }
            },
        }
    }

    fn sync_preferences(&mut self) {
        let current = Preferences {
            show_battery_percentage: self.state.show_battery_percentage,
            always_keep_screen_on: self.state.always_keep_screen_on,
            minimal_interface: self.state.minimal_interface,
        };
        if current == self.persisted_preferences
            || Instant::now() < self.next_preferences_store_retry
        {
            return;
        }
        match preferences::save(current) {
            Ok(()) => {
                self.persisted_preferences = current;
                self.next_preferences_store_retry = Instant::now();
                println!(
                    "BRICKWAVE_PREFERENCES_SAVED show_battery_percentage={} always_keep_screen_on={} minimal_interface={}",
                    current.show_battery_percentage,
                    current.always_keep_screen_on,
                    current.minimal_interface
                );
            }
            Err(_) => {
                self.next_preferences_store_retry = Instant::now() + Duration::from_secs(5);
                self.state.toast = Some("Không thể lưu cài đặt hiển thị".to_owned());
                println!("BRICKWAVE_PREFERENCES_SAVE_ERROR");
            }
        }
    }

    fn service_workers(&mut self) {
        self.state.service_stop_timer();
        let percentage = self
            .battery_monitor
            .poll(self.state.show_battery_percentage);
        self.state.set_battery_percentage(percentage);
        for event in self.playback_engine.take_events() {
            self.state.apply_playback_engine_event(event);
        }
        for event in self.backend.take_events() {
            self.state.apply_backend_event(event);
        }
        if self
            .wake_data_recovery_at
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.wake_data_recovery_at = None;
            self.state.recover_after_network_resume();
            println!("BRICKWAVE_WAKE_RECOVERY state=running");
        }
        self.sync_persisted_session();
        self.sync_preferences();
        if self.state.auth_polling_active() && Instant::now() >= self.next_auth_poll {
            self.state.poll_qr_login();
            self.next_auth_poll = Instant::now() + Duration::from_secs(5);
        } else if !self.state.auth_polling_active() {
            self.next_auth_poll = Instant::now();
        }

        for command in self.state.take_backend_commands() {
            if self.backend.send(command).is_err() {
                self.state.toast = Some("Dịch vụ dữ liệu SoundCloud không khả dụng".to_owned());
            }
        }
        for command in self.state.take_audio_commands() {
            if self.playback_engine.send(command).is_err() {
                self.state
                    .apply_playback_engine_event(PlaybackEngineEvent::Error {
                        track_id: None,
                        entry_id: None,
                        message: "Trình phát âm thanh StockOS không khả dụng".to_owned(),
                    });
                self.state.set_live_playback_enabled(false);
                // The failed channel cannot consume the recovery Stop queued
                // by the state transition. Drop it to avoid a retry loop.
                self.state.take_audio_commands();
                break;
            }
        }
    }

    #[cfg(all(feature = "trimui-sdl2", target_os = "linux", target_arch = "aarch64"))]
    fn request_exit_confirmation(&mut self) {
        if self.virtual_keyboard.is_open() {
            self.virtual_keyboard.request_close();
            println!("BRICKWAVE_KEYBOARD_CLOSE reason=exit-confirmation");
        }
        self.exit_confirmation_open = true;
        println!("BRICKWAVE_EXIT_CONFIRM state=open");
    }

    #[cfg(all(feature = "trimui-sdl2", target_os = "linux", target_arch = "aarch64"))]
    fn dismiss_exit_confirmation(&mut self) -> bool {
        if !self.exit_confirmation_open {
            return false;
        }
        self.exit_confirmation_open = false;
        println!("BRICKWAVE_EXIT_CONFIRM state=cancelled source=button-b");
        true
    }

    #[cfg(all(feature = "trimui-sdl2", target_os = "linux", target_arch = "aarch64"))]
    fn exit_confirmation_is_open(&self) -> bool {
        self.exit_confirmation_open
    }

    #[cfg(all(feature = "trimui-sdl2", target_os = "linux", target_arch = "aarch64"))]
    fn confirm_exit_with_primary(&mut self) -> bool {
        if !self.exit_confirmation_open {
            return false;
        }
        self.exit_confirmation_open = false;
        self.exit_requested = true;
        println!("BRICKWAVE_EXIT_CONFIRM action=exit source=button-a");
        true
    }

    #[cfg(all(feature = "trimui-sdl2", target_os = "linux", target_arch = "aarch64"))]
    fn take_exit_request(&mut self) -> bool {
        std::mem::take(&mut self.exit_requested)
    }

    #[cfg(all(feature = "trimui-sdl2", target_os = "linux", target_arch = "aarch64"))]
    fn stop_audio_for_exit(&mut self) {
        self.state.stop_playback();
        self.service_workers();
        println!("BRICKWAVE_EXIT_CONFIRM state=confirmed audio=stop");
    }

    fn minimal_ui_enabled(&self) -> bool {
        self.state.minimal_interface && matches!(self.state.route(), AppRoute::Main(_))
    }

    fn minimal_focus_count(&self) -> usize {
        match self.minimal_navigation.page {
            MinimalPage::Player => 6,
            MinimalPage::Search => {
                1 + self.state.search_results().len() + self.state.search_playlists().len()
            }
            MinimalPage::Playlists if self.state.main_page() == Page::Playlist => self
                .state
                .current_playlist()
                .map(|playlist| playlist.track_ids.len())
                .unwrap_or(0)
                .max(1),
            MinimalPage::Playlists if self.state.main_page() == Page::Likes => {
                self.state.liked_track_ids().len().max(1)
            }
            MinimalPage::Playlists => 3 + self.state.library_playlists().len(),
            MinimalPage::Settings => 4,
        }
    }

    fn select_minimal_page(&mut self, page: MinimalPage) {
        self.minimal_navigation.page = page;
        self.minimal_navigation.focus = if page == MinimalPage::Player { 1 } else { 0 };
        match page {
            MinimalPage::Player => self.state.open_now_playing(),
            MinimalPage::Search => self.state.navigate_main(Page::Search),
            MinimalPage::Playlists => {
                self.state.library_tab = LibraryTab::Playlists;
                self.state.navigate_main(Page::Library);
            }
            MinimalPage::Settings => self.state.navigate_main(Page::Settings),
        }
    }

    fn switch_minimal_page(&mut self, direction: i8) {
        let current = self.minimal_navigation.page.index() as i32;
        let next = (current + direction.signum() as i32).rem_euclid(MinimalPage::ALL.len() as i32);
        self.select_minimal_page(MinimalPage::ALL[next as usize]);
    }

    fn move_minimal_focus(&mut self, direction: i8) {
        let count = self.minimal_focus_count();
        if count == 0 {
            self.minimal_navigation.focus = 0;
            return;
        }
        let current = self.minimal_navigation.focus.min(count - 1) as i32;
        self.minimal_navigation.focus =
            (current + direction.signum() as i32).rem_euclid(count as i32) as usize;
    }

    fn activate_minimal_focus(&mut self) {
        let focus = self
            .minimal_navigation
            .focus
            .min(self.minimal_focus_count().saturating_sub(1));
        match self.minimal_navigation.page {
            MinimalPage::Player => match focus {
                0 => self.state.previous(),
                1 => self.state.toggle_playback(),
                2 => self.state.next(),
                3 => {
                    if let Some(track) = self.state.current_track() {
                        self.state.toggle_like(track.id);
                    }
                }
                4 => self.adjust_minimal_volume(-0.05),
                5 => self.adjust_minimal_volume(0.05),
                _ => {}
            },
            MinimalPage::Search => {
                if focus == 0 {
                    self.keyboard_target = Some(KeyboardTarget::GlobalSearch);
                    self.virtual_keyboard
                        .open(KeyboardTarget::GlobalSearch.config());
                    return;
                }
                let tracks = self.state.search_results();
                let track_index = focus - 1;
                if tracks.get(track_index).is_some() {
                    self.state
                        .select_track_from_context(tracks.clone(), track_index);
                    self.select_minimal_page(MinimalPage::Player);
                    return;
                }
                let playlist_index = track_index.saturating_sub(tracks.len());
                if let Some(playlist) = self.state.search_playlists().get(playlist_index) {
                    self.state.open_playlist(playlist.id);
                    self.minimal_navigation.page = MinimalPage::Playlists;
                    self.minimal_navigation.focus = 0;
                }
            }
            MinimalPage::Playlists if self.state.main_page() == Page::Playlist => {
                if let Some(playlist) = self.state.current_playlist()
                    && let Some(track_id) = playlist.track_ids.get(focus).copied()
                {
                    self.state
                        .select_track_from_context(playlist.track_ids.clone(), focus);
                    self.state.toast = self
                        .state
                        .track(track_id)
                        .map(|track| format!("Đang phát {}", track.title));
                }
            }
            MinimalPage::Playlists if self.state.main_page() == Page::Likes => {
                let ids = self.state.liked_track_ids();
                if ids.get(focus).is_some() {
                    self.state.select_track_from_context(ids, focus);
                }
            }
            MinimalPage::Playlists => {
                if focus == 0 {
                    self.state.refresh_user_library();
                } else if focus == 1 {
                    self.state.open_create_playlist();
                } else if focus == 2 {
                    self.state.navigate_main(Page::Likes);
                    self.minimal_navigation.focus = 0;
                } else if let Some(playlist) = self.state.library_playlists().get(focus - 3) {
                    self.state.open_playlist(playlist.id);
                    self.minimal_navigation.focus = 0;
                }
            }
            MinimalPage::Settings => match focus {
                0 => self.state.set_minimal_interface(false),
                1 => self.state.set_minimal_interface(true),
                2 => self
                    .state
                    .set_always_keep_screen_on(!self.state.always_keep_screen_on),
                3 => self
                    .state
                    .set_show_battery_percentage(!self.state.show_battery_percentage),
                _ => {}
            },
        }
    }

    fn adjust_minimal_volume(&mut self, delta: f32) {
        let volume = (self.state.player_state().volume + delta).clamp(0.0, 1.0);
        self.state.set_volume(volume);
    }

    fn handle_minimal_back(&mut self) -> bool {
        if !self.minimal_ui_enabled() {
            return false;
        }
        if self.minimal_navigation.page == MinimalPage::Playlists
            && matches!(self.state.main_page(), Page::Playlist | Page::Likes)
        {
            self.state.navigate_back();
            self.minimal_navigation.focus = 0;
            return true;
        }
        if self.minimal_navigation.page != MinimalPage::Player {
            self.select_minimal_page(MinimalPage::Player);
        }
        true
    }

    fn handle_minimal_dpad_x(&mut self, value: i8) -> bool {
        if !self.minimal_ui_enabled() {
            return false;
        }
        if value != 0 {
            self.switch_minimal_page(value);
        }
        true
    }

    fn handle_minimal_dpad_y(&mut self, value: i8) -> bool {
        if !self.minimal_ui_enabled() {
            return false;
        }
        if value != 0 {
            self.move_minimal_focus(value);
        }
        true
    }

    fn handle_minimal_primary(&mut self, pressed: bool) -> bool {
        if !self.minimal_ui_enabled() {
            return false;
        }
        if pressed {
            self.activate_minimal_focus();
        }
        true
    }

    fn handle_minimal_keyboard(&mut self, ctx: &egui::Context) {
        if !self.minimal_ui_enabled() || self.virtual_keyboard.is_open() {
            return;
        }
        let (up, down, left, right, activate, back, play, pause) = ctx.input(|input| {
            (
                input.key_pressed(egui::Key::ArrowUp),
                input.key_pressed(egui::Key::ArrowDown),
                input.key_pressed(egui::Key::ArrowLeft),
                input.key_pressed(egui::Key::ArrowRight),
                input.key_pressed(egui::Key::Enter) || input.key_pressed(egui::Key::Space),
                input.key_pressed(egui::Key::Escape) || input.key_pressed(egui::Key::Backspace),
                input.key_pressed(egui::Key::P),
                input.key_pressed(egui::Key::Y),
            )
        });
        if left {
            self.switch_minimal_page(-1);
        } else if right {
            self.switch_minimal_page(1);
        }
        if up {
            self.move_minimal_focus(-1);
        } else if down {
            self.move_minimal_focus(1);
        }
        if activate {
            self.activate_minimal_focus();
        }
        if back {
            self.handle_minimal_back();
        }
        if play {
            self.state.start_playback();
        }
        if pause {
            self.state.pause_playback();
        }
    }

    fn render_ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let create_playlist_was_open = self.state.create_playlist_dialog_open();
        self.service_workers();
        self.virtual_keyboard.handle_egui_controls(&ctx);
        let keyboard_delta = ctx.input(|input| input.stable_dt).clamp(0.0, 0.25);
        self.virtual_keyboard
            .advance(Duration::from_secs_f32(keyboard_delta));
        self.apply_keyboard_actions(&ctx);
        self.handle_minimal_keyboard(&ctx);
        if self.virtual_keyboard.is_open()
            && let Some(target) = self.keyboard_target
        {
            ctx.memory_mut(|memory| memory.request_focus(target.field_id()));
        }
        ctx.layer_painter(egui::LayerId::background()).rect_filled(
            ctx.content_rect(),
            CornerRadius::ZERO,
            APP_BG,
        );

        self.artwork_manager.begin_frame();
        match self.state.route() {
            AppRoute::Restoring { .. } => {
                egui::CentralPanel::default()
                    .frame(Frame::new().fill(APP_BG).inner_margin(Margin::ZERO))
                    .show(ui, |ui| {
                        restore_view::render(ui, &mut self.state, &self.app_logo)
                    });
            }
            AppRoute::Login { .. } => {
                egui::CentralPanel::default()
                    .frame(Frame::new().fill(APP_BG).inner_margin(Margin::ZERO))
                    .show(ui, |ui| {
                        auth_view::render(ui, &mut self.state, &self.app_logo)
                    });
            }
            AppRoute::Main(_) => {
                if self.state.minimal_interface {
                    minimal_shell(
                        ui,
                        &mut self.state,
                        &self.app_logo,
                        &mut self.artwork_manager,
                        &mut self.waveform_manager,
                        &mut self.minimal_navigation,
                    );
                } else {
                    player_bar(ui, &mut self.state, &mut self.artwork_manager);
                    sidebar(ui, &mut self.state, &self.app_logo);
                    egui::CentralPanel::default()
                        .frame(
                            Frame::new()
                                .fill(APP_BG)
                                .inner_margin(Margin::symmetric(22, 17)),
                        )
                        .show(ui, |ui| {
                            page(
                                ui,
                                &mut self.state,
                                &mut self.artwork_manager,
                                &mut self.waveform_manager,
                            )
                        });
                }
            }
        }
        self.service_toast_lifetime(&ctx);
        toast(&ctx, &mut self.state);
        add_to_playlist_dialog(&ctx, &mut self.state);
        create_playlist_dialog(&ctx, &mut self.state);
        remove_track_from_playlist_dialog(&ctx, &mut self.state);
        delete_playlist_dialog(&ctx, &mut self.state);
        if exit_confirmation_dialog(&ctx, &mut self.exit_confirmation_open) {
            self.exit_requested = true;
        }

        let create_playlist_just_opened =
            !create_playlist_was_open && self.state.create_playlist_dialog_open();
        if create_playlist_just_opened {
            ctx.memory_mut(|memory| memory.request_focus(playlist_title_field_id()));
        }
        let focused_target = ctx.memory(|memory| {
            if memory.has_focus(playlist_title_field_id()) {
                Some(KeyboardTarget::PlaylistTitle)
            } else if memory.has_focus(stop_timer_hours_field_id()) {
                Some(KeyboardTarget::StopTimerHours)
            } else if memory.has_focus(stop_timer_minutes_field_id()) {
                Some(KeyboardTarget::StopTimerMinutes)
            } else if memory.has_focus(search_field_id()) {
                Some(KeyboardTarget::GlobalSearch)
            } else {
                None
            }
        });
        if matches!(self.state.route(), AppRoute::Main(_)) {
            if !self.virtual_keyboard.is_open()
                && let Some(target) = focused_target
                && (self.keyboard_focus_was != Some(target) || create_playlist_just_opened)
                && self.virtual_keyboard.open(target.config())
            {
                self.keyboard_target = Some(target);
                println!(
                    "BRICKWAVE_KEYBOARD_OPEN target={} input=dpad-a",
                    target.log_name()
                );
                println!(
                    "BRICKWAVE_KEYBOARD_FOCUS_READY target={}",
                    target.log_name()
                );
            }
        } else if self.virtual_keyboard.is_open() {
            self.virtual_keyboard.request_close();
        }
        if self.virtual_keyboard.is_open() {
            if let Some(target) = self.keyboard_target {
                ctx.memory_mut(|memory| memory.request_focus(target.field_id()));
            }
            if let Some(layout) = self.virtual_keyboard.show(&ctx) {
                println!(
                    "BRICKWAVE_KEYBOARD_LAYOUT screen_width={:.0} screen_height={:.0} pixels_per_point={:.3} panel_x={:.0} panel_y={:.0} panel_width={:.0} panel_height={:.0} row_count={}",
                    layout.screen_width,
                    layout.screen_height,
                    layout.pixels_per_point,
                    layout.panel_x,
                    layout.panel_y,
                    layout.panel_width,
                    layout.panel_height,
                    layout.row_count,
                );
            }
        } else if focused_target.is_none() {
            self.keyboard_target = None;
        }
        self.keyboard_focus_was = focused_target;
        self.artwork_manager.end_frame();

        let elapsed = ctx.input(|input| input.stable_dt).clamp(0.0, 0.25);
        self.state.advance_preview(elapsed);
        if self.state.is_playing() {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        if self.state.auth_polling_active() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        if self.state.stop_timer_remaining().is_some() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        if self.virtual_keyboard.has_held_direction() {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
}

fn load_app_logo(ctx: &egui::Context) -> egui::TextureHandle {
    let decoded = image::load_from_memory(include_bytes!("../assets/brickwave.png"))
        .expect("embedded Brickwave logo must be a valid PNG")
        .into_rgba8();
    let size = [decoded.width() as usize, decoded.height() as usize];
    let pixels = decoded.into_raw();
    ctx.load_texture(
        "brickwave-app-logo",
        egui::ColorImage::from_rgba_unmultiplied(size, &pixels),
        egui::TextureOptions::LINEAR,
    )
}

#[cfg(not(all(target_os = "linux", target_arch = "aarch64")))]
impl eframe::App for BrickwaveApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.render_ui(ui);
    }
}

#[derive(Clone, Copy)]
enum Icon {
    Brand,
    Home,
    Discover,
    Search,
    Library,
    Like,
    LikeFilled,
    Playlist,
    Settings,
    User,
    Play,
    Pause,
    Previous,
    Next,
    Shuffle,
    Repeat,
    RepeatOne,
    Volume,
    Queue,
    ChevronDown,
    ChevronLeft,
    ChevronRight,
    Back,
    Close,
    More,
    Save,
}

#[allow(dead_code)]
fn legacy_paint_icon(painter: &egui::Painter, icon: Icon, rect: Rect, color: Color32) {
    let c = rect.center();
    let s = rect.width().min(rect.height()) * 0.38;
    let line = Stroke::new(1.6_f32, color);
    match icon {
        Icon::Brand => {
            painter.circle_stroke(c, s, Stroke::new(2.0_f32, ACCENT));
            painter.circle_filled(c, s * 0.22, ACCENT);
            painter.line_segment([pos2(c.x - s * 0.7, c.y), pos2(c.x + s * 0.7, c.y)], line);
        }
        Icon::Home => {
            painter.line_segment([pos2(c.x - s, c.y), pos2(c.x, c.y - s)], line);
            painter.line_segment([pos2(c.x, c.y - s), pos2(c.x + s, c.y)], line);
            painter.rect_stroke(
                Rect::from_center_size(pos2(c.x, c.y + s * 0.43), vec2(s * 1.25, s * 0.95)),
                CornerRadius::same(2),
                line,
                egui::StrokeKind::Inside,
            );
        }
        Icon::Discover => {
            painter.circle_stroke(c, s * 0.88, line);
            painter.line_segment([pos2(c.x, c.y - s * 0.62), pos2(c.x + s * 0.62, c.y)], line);
            painter.circle_filled(c, s * 0.15, color);
        }
        Icon::Search => {
            painter.circle_stroke(pos2(c.x - s * 0.2, c.y - s * 0.2), s * 0.55, line);
            painter.line_segment(
                [
                    pos2(c.x + s * 0.2, c.y + s * 0.2),
                    pos2(c.x + s * 0.85, c.y + s * 0.85),
                ],
                line,
            );
        }
        Icon::Library | Icon::Playlist | Icon::Queue => {
            for y in [-0.55_f32, 0.0_f32, 0.55_f32] {
                painter.line_segment(
                    [pos2(c.x - s, c.y + y * s), pos2(c.x + s, c.y + y * s)],
                    line,
                );
            }
            if matches!(icon, Icon::Library) {
                painter.line_segment([pos2(c.x - s, c.y - s), pos2(c.x - s, c.y + s)], line);
            }
            if matches!(icon, Icon::Queue) {
                painter.circle_filled(pos2(c.x + s * 0.78, c.y + s * 0.55), s * 0.15, color);
            }
        }
        Icon::Like | Icon::LikeFilled => {
            let points = vec![
                pos2(c.x, c.y + s),
                pos2(c.x - s, c.y + s * 0.08),
                pos2(c.x - s * 0.85, c.y - s * 0.52),
                pos2(c.x - s * 0.38, c.y - s * 0.82),
                pos2(c.x, c.y - s * 0.28),
                pos2(c.x + s * 0.38, c.y - s * 0.82),
                pos2(c.x + s * 0.85, c.y - s * 0.52),
                pos2(c.x + s, c.y + s * 0.08),
            ];
            if matches!(icon, Icon::LikeFilled) {
                painter.add(egui::Shape::convex_polygon(points, ACCENT, Stroke::NONE));
            } else {
                painter.add(egui::Shape::line(points.clone(), line));
                painter.line_segment([points[7], points[0]], line);
            }
        }
        Icon::Settings => {
            painter.circle_stroke(c, s * 0.4, line);
            for n in 0..8 {
                let a = n as f32 * std::f32::consts::TAU / 8.0;
                let p1 = c + vec2(a.cos(), a.sin()) * s * 0.58;
                let p2 = c + vec2(a.cos(), a.sin()) * s;
                painter.line_segment([p1, p2], line);
            }
        }
        Icon::User => {
            painter.circle_stroke(pos2(c.x, c.y - s * 0.4), s * 0.35, line);
            painter.line_segment(
                [
                    pos2(c.x - s * 0.82, c.y + s * 0.8),
                    pos2(c.x - s * 0.5, c.y + s * 0.2),
                ],
                line,
            );
            painter.line_segment(
                [
                    pos2(c.x - s * 0.5, c.y + s * 0.2),
                    pos2(c.x + s * 0.5, c.y + s * 0.2),
                ],
                line,
            );
            painter.line_segment(
                [
                    pos2(c.x + s * 0.5, c.y + s * 0.2),
                    pos2(c.x + s * 0.82, c.y + s * 0.8),
                ],
                line,
            );
        }
        Icon::Play => {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    pos2(c.x - s * 0.42, c.y - s),
                    pos2(c.x - s * 0.42, c.y + s),
                    pos2(c.x + s, c.y),
                ],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Pause => {
            painter.rect_filled(
                Rect::from_center_size(pos2(c.x - s * 0.38, c.y), vec2(s * 0.38, s * 1.7)),
                CornerRadius::same(1),
                color,
            );
            painter.rect_filled(
                Rect::from_center_size(pos2(c.x + s * 0.38, c.y), vec2(s * 0.38, s * 1.7)),
                CornerRadius::same(1),
                color,
            );
        }
        Icon::Previous | Icon::Next => {
            let flip = if matches!(icon, Icon::Previous) {
                -1.0_f32
            } else {
                1.0_f32
            };
            let x = c.x + flip * s * 0.55;
            painter.line_segment([pos2(x, c.y - s), pos2(x, c.y + s)], line);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    pos2(x - flip * s * 0.15, c.y),
                    pos2(x - flip * s, c.y - s),
                    pos2(x - flip * s, c.y + s),
                ],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Shuffle => {
            painter.line_segment(
                [
                    pos2(c.x - s, c.y - s * 0.55),
                    pos2(c.x - s * 0.4, c.y - s * 0.55),
                ],
                line,
            );
            painter.line_segment(
                [
                    pos2(c.x - s * 0.4, c.y - s * 0.55),
                    pos2(c.x + s * 0.55, c.y + s * 0.55),
                ],
                line,
            );
            painter.line_segment(
                [
                    pos2(c.x - s, c.y + s * 0.55),
                    pos2(c.x - s * 0.4, c.y + s * 0.55),
                ],
                line,
            );
            painter.line_segment(
                [
                    pos2(c.x - s * 0.4, c.y + s * 0.55),
                    pos2(c.x - s * 0.05, c.y + s * 0.2),
                ],
                line,
            );
            painter.line_segment(
                [
                    pos2(c.x + s * 0.55, c.y - s * 0.55),
                    pos2(c.x + s * 0.88, c.y - s * 0.55),
                ],
                line,
            );
            painter.add(egui::Shape::convex_polygon(
                vec![
                    pos2(c.x + s, c.y - s * 0.55),
                    pos2(c.x + s * 0.62, c.y - s * 0.82),
                    pos2(c.x + s * 0.62, c.y - s * 0.28),
                ],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Repeat | Icon::RepeatOne => {
            painter.line_segment(
                [
                    pos2(c.x - s, c.y - s * 0.55),
                    pos2(c.x + s * 0.7, c.y - s * 0.55),
                ],
                line,
            );
            painter.line_segment(
                [
                    pos2(c.x + s * 0.7, c.y - s * 0.55),
                    pos2(c.x + s * 0.7, c.y + s * 0.55),
                ],
                line,
            );
            painter.line_segment(
                [
                    pos2(c.x + s * 0.7, c.y + s * 0.55),
                    pos2(c.x - s, c.y + s * 0.55),
                ],
                line,
            );
            painter.add(egui::Shape::convex_polygon(
                vec![
                    pos2(c.x + s, c.y - s * 0.55),
                    pos2(c.x + s * 0.58, c.y - s * 0.83),
                    pos2(c.x + s * 0.58, c.y - s * 0.27),
                ],
                color,
                Stroke::NONE,
            ));
            if matches!(icon, Icon::RepeatOne) {
                painter.text(
                    c,
                    Align2::CENTER_CENTER,
                    "1",
                    egui::FontId::proportional(s),
                    color,
                );
            }
        }
        Icon::Volume => {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    pos2(c.x - s, c.y - s * 0.35),
                    pos2(c.x - s * 0.4, c.y - s * 0.35),
                    pos2(c.x + s * 0.05, c.y - s * 0.8),
                    pos2(c.x + s * 0.05, c.y + s * 0.8),
                    pos2(c.x - s * 0.4, c.y + s * 0.35),
                    pos2(c.x - s, c.y + s * 0.35),
                ],
                color,
                Stroke::NONE,
            ));
            painter.line_segment(
                [
                    pos2(c.x + s * 0.35, c.y - s * 0.58),
                    pos2(c.x + s * 0.7, c.y),
                ],
                line,
            );
            painter.line_segment(
                [
                    pos2(c.x + s * 0.7, c.y),
                    pos2(c.x + s * 0.35, c.y + s * 0.58),
                ],
                line,
            );
        }
        Icon::ChevronDown | Icon::ChevronLeft | Icon::ChevronRight => {
            let points = match icon {
                Icon::ChevronDown => [
                    pos2(c.x - s * 0.7, c.y - s * 0.3),
                    pos2(c.x, c.y + s * 0.35),
                    pos2(c.x + s * 0.7, c.y - s * 0.3),
                ],
                Icon::ChevronLeft => [
                    pos2(c.x + s * 0.3, c.y - s * 0.7),
                    pos2(c.x - s * 0.35, c.y),
                    pos2(c.x + s * 0.3, c.y + s * 0.7),
                ],
                _ => [
                    pos2(c.x - s * 0.3, c.y - s * 0.7),
                    pos2(c.x + s * 0.35, c.y),
                    pos2(c.x - s * 0.3, c.y + s * 0.7),
                ],
            };
            painter.line_segment([points[0], points[1]], line);
            painter.line_segment([points[1], points[2]], line);
        }
        Icon::Close => {
            painter.line_segment(
                [
                    pos2(c.x - s * 0.7, c.y - s * 0.7),
                    pos2(c.x + s * 0.7, c.y + s * 0.7),
                ],
                line,
            );
            painter.line_segment(
                [
                    pos2(c.x + s * 0.7, c.y - s * 0.7),
                    pos2(c.x - s * 0.7, c.y + s * 0.7),
                ],
                line,
            );
        }
        Icon::More => {
            for x in [-0.6_f32, 0.0_f32, 0.6_f32] {
                painter.circle_filled(pos2(c.x + x * s, c.y), s * 0.14, color);
            }
        }
        Icon::Save => {
            painter.rect_stroke(
                Rect::from_center_size(c, vec2(s * 1.4, s * 1.6)),
                CornerRadius::same(1),
                line,
                egui::StrokeKind::Inside,
            );
            painter.rect_filled(
                Rect::from_center_size(pos2(c.x, c.y - s * 0.35), vec2(s * 0.64, s * 0.4)),
                CornerRadius::same(1),
                color,
            );
        }
        Icon::Back => {
            painter.line_segment([pos2(c.x + s * 0.8, c.y), pos2(c.x - s * 0.7, c.y)], line);
            painter.line_segment(
                [
                    pos2(c.x - s * 0.7, c.y),
                    pos2(c.x - s * 0.15, c.y - s * 0.55),
                ],
                line,
            );
            painter.line_segment(
                [
                    pos2(c.x - s * 0.7, c.y),
                    pos2(c.x - s * 0.15, c.y + s * 0.55),
                ],
                line,
            );
        }
    }
}

fn paint_icon(painter: &egui::Painter, icon: Icon, rect: Rect, color: Color32) {
    let glyph = match icon {
        Icon::Brand => Glyph::Brand,
        Icon::Home => Glyph::Home,
        Icon::Discover => Glyph::Discover,
        Icon::Search => Glyph::Search,
        Icon::Library => Glyph::Library,
        Icon::Like => Glyph::Like,
        Icon::LikeFilled => Glyph::LikeFilled,
        Icon::Playlist => Glyph::Playlist,
        Icon::Settings => Glyph::Settings,
        Icon::User => Glyph::User,
        Icon::Play => Glyph::Play,
        Icon::Pause => Glyph::Pause,
        Icon::Previous => Glyph::Previous,
        Icon::Next => Glyph::Next,
        Icon::Shuffle => Glyph::Shuffle,
        Icon::Repeat => Glyph::Repeat,
        Icon::RepeatOne => Glyph::RepeatOne,
        Icon::Volume => Glyph::Volume,
        Icon::Queue => Glyph::Queue,
        Icon::ChevronDown => Glyph::ChevronDown,
        Icon::ChevronLeft => Glyph::ChevronLeft,
        Icon::ChevronRight => Glyph::ChevronRight,
        Icon::Back => Glyph::Back,
        Icon::Close => Glyph::Close,
        Icon::More => Glyph::More,
        Icon::Save => Glyph::Save,
    };
    IconRegistry::paint(painter, glyph, rect, color);
}

fn text(ui: &mut Ui, value: &str, size: f32, color: Color32, strong: bool) {
    let mut value = RichText::new(value).size(font_size(size)).color(color);
    if strong {
        value = value.strong();
    }
    ui.label(value);
}

fn micro(ui: &mut Ui, value: &str) {
    text(ui, value, 10.0, SECONDARY, true);
}

fn icon_button(
    ui: &mut Ui,
    id: impl std::hash::Hash,
    icon: Icon,
    selected: bool,
    enabled: bool,
    hint: &str,
) -> Response {
    icon_button_sized(ui, id, icon, selected, enabled, CONTROL_HITBOX, hint)
}

fn icon_button_sized(
    ui: &mut Ui,
    id: impl std::hash::Hash,
    icon: Icon,
    selected: bool,
    enabled: bool,
    hitbox: f32,
    hint: &str,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(
        vec2(hitbox, hitbox),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let fill = if selected {
        WARM
    } else if response.hovered() && enabled {
        RAISED
    } else {
        Color32::TRANSPARENT
    };
    let border = if response.has_focus() {
        ACCENT
    } else if selected {
        ACCENT
    } else if response.hovered() && enabled {
        BORDER
    } else {
        Color32::TRANSPARENT
    };
    let color = if !enabled {
        DISABLED
    } else if selected {
        ACCENT
    } else if response.hovered() {
        ACCENT_HOVER
    } else {
        SECONDARY
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(SMALL_RADIUS),
        fill,
        Stroke::new(1.0_f32, border),
        egui::StrokeKind::Inside,
    );
    let icon_size = if hitbox <= 26.0 {
        ICON_SIZE_SMALL
    } else {
        ICON_SIZE_MEDIUM
    };
    let icon_rect = Rect::from_center_size(rect.center(), vec2(icon_size, icon_size));
    paint_icon(ui.painter(), icon, icon_rect, color);
    let _ = id;
    response.on_hover_text(hint)
}

fn hifi_button(ui: &mut Ui, label: &str, primary: bool, enabled: bool) -> Response {
    let width = (label.len() as f32 * 7.6 + 28.0).max(68.0);
    let (rect, response) = ui.allocate_exact_size(
        vec2(width, 34.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let fill = if !enabled {
        SURFACE
    } else if primary {
        if response.hovered() {
            ACCENT_HOVER
        } else {
            ACCENT
        }
    } else if response.hovered() {
        RAISED
    } else {
        SURFACE
    };
    let border = if response.has_focus() || primary {
        ACCENT
    } else if response.hovered() {
        ACCENT_HOVER
    } else {
        BORDER
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(SMALL_RADIUS),
        fill,
        Stroke::new(1.0_f32, border),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(font_size(12.0)),
        if !enabled {
            DISABLED
        } else if primary {
            APP_BG
        } else {
            TEXT
        },
    );
    response
}

/// Paints a themed button over an already allocated card. This keeps the
/// card itself clickable while giving its action a later, higher-priority
/// interaction region.
fn hifi_button_at(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    rect: Rect,
    label: &str,
    primary: bool,
    enabled: bool,
) -> Response {
    let response = ui.interact(
        rect,
        ui.id().with(id),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let fill = if !enabled {
        SURFACE
    } else if primary {
        if response.hovered() {
            ACCENT_HOVER
        } else {
            ACCENT
        }
    } else if response.hovered() {
        RAISED
    } else {
        SURFACE
    };
    let border = if response.has_focus() || primary {
        ACCENT
    } else if response.hovered() {
        ACCENT_HOVER
    } else {
        BORDER
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(SMALL_RADIUS),
        fill,
        Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(font_size(12.0)),
        if !enabled {
            DISABLED
        } else if primary {
            APP_BG
        } else {
            TEXT
        },
    );
    response
}

fn minimal_shell(
    ui: &mut Ui,
    state: &mut UiState,
    app_logo: &egui::TextureHandle,
    artwork_manager: &mut ArtworkManager,
    waveform_manager: &mut WaveformManager,
    navigation: &mut MinimalNavigation,
) {
    egui::Panel::top("minimal-main-top")
        .exact_size(72.0)
        .frame(
            Frame::new()
                .fill(APP_BG)
                .stroke(Stroke::new(1.0, SUBTLE))
                .inner_margin(Margin::symmetric(28, 13)),
        )
        .show(ui, |ui| minimal_top_bar(ui, state, app_logo));

    egui::Panel::bottom("minimal-main-bottom")
        .exact_size(112.0)
        .frame(
            Frame::new()
                .fill(SURFACE)
                .stroke(Stroke::new(1.0, BORDER))
                .inner_margin(Margin::symmetric(22, 10)),
        )
        .show(ui, |ui| minimal_bottom_bar(ui, state, navigation));

    egui::CentralPanel::default()
        .frame(
            Frame::new()
                .fill(APP_BG)
                .inner_margin(Margin::symmetric(28, 18)),
        )
        .show(ui, |ui| match navigation.page {
            MinimalPage::Player => {
                minimal_player(ui, state, artwork_manager, waveform_manager, navigation)
            }
            MinimalPage::Search => minimal_search(ui, state, artwork_manager, navigation),
            MinimalPage::Playlists => minimal_playlists(ui, state, artwork_manager, navigation),
            MinimalPage::Settings => minimal_settings(ui, state, navigation),
        });
}

fn minimal_top_bar(ui: &mut Ui, state: &UiState, app_logo: &egui::TextureHandle) {
    ui.horizontal(|ui| {
        let (logo_rect, _) = ui.allocate_exact_size(vec2(42.0, 42.0), Sense::hover());
        ui.painter().image(
            app_logo.id(),
            logo_rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        ui.add_space(5.0);
        text(ui, "BRICKWAVE", 21.0, TEXT, true);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if state.show_battery_percentage {
                battery_indicator(ui, state.battery_percentage());
            }
            if let Some(profile) = state.current_profile() {
                let account = profile.display_name.as_deref().unwrap_or(&profile.username);
                ui.add_sized(
                    [210.0, TOP_CONTROL_HEIGHT],
                    egui::Label::new(
                        RichText::new(account)
                            .size(font_size(12.5))
                            .color(SECONDARY),
                    )
                    .truncate()
                    .halign(Align::Max),
                );
            }
        });
    });
}

fn apply_minimal_page(state: &mut UiState, navigation: &mut MinimalNavigation, page: MinimalPage) {
    navigation.page = page;
    navigation.focus = if page == MinimalPage::Player { 1 } else { 0 };
    match page {
        MinimalPage::Player => state.open_now_playing(),
        MinimalPage::Search => state.navigate_main(Page::Search),
        MinimalPage::Playlists => {
            state.library_tab = LibraryTab::Playlists;
            state.navigate_main(Page::Library);
        }
        MinimalPage::Settings => state.navigate_main(Page::Settings),
    }
}

fn minimal_bottom_bar(ui: &mut Ui, state: &mut UiState, navigation: &mut MinimalNavigation) {
    ui.label(
        RichText::new(
            "D-pad: di chuyển  ·  A: chọn  ·  B: quay lại  ·  START: phát  ·  Y: tạm dừng  ·  L1/R1: chuyển bài",
        )
        .size(font_size(11.5))
        .color(SECONDARY),
    );
    ui.add_space(6.0);
    ui.columns(MinimalPage::ALL.len(), |columns| {
        for (index, page) in MinimalPage::ALL.into_iter().enumerate() {
            let selected = navigation.page == page;
            if minimal_nav_tile(&mut columns[index], page, selected).clicked() {
                apply_minimal_page(state, navigation, page);
            }
        }
    });
}

fn minimal_nav_tile(ui: &mut Ui, page: MinimalPage, selected: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 58.0), Sense::click());
    ui.painter().rect(
        rect,
        CornerRadius::same(10),
        if selected { WARM } else { Color32::TRANSPARENT },
        Stroke::new(
            1.0,
            if selected {
                ACCENT
            } else {
                Color32::TRANSPARENT
            },
        ),
        egui::StrokeKind::Inside,
    );
    let icon_rect = Rect::from_center_size(
        pos2(rect.center().x, rect.center().y - 9.0),
        vec2(25.0, 25.0),
    );
    paint_icon(
        ui.painter(),
        page.icon(),
        icon_rect,
        if selected { ACCENT } else { SECONDARY },
    );
    ui.painter().text(
        pos2(rect.center().x, rect.bottom() - 8.0),
        Align2::CENTER_BOTTOM,
        page.title(),
        egui::FontId::proportional(font_size(12.5)),
        if selected { TEXT } else { SECONDARY },
    );
    response
}

fn minimal_player(
    ui: &mut Ui,
    state: &mut UiState,
    artwork_manager: &mut ArtworkManager,
    waveform_manager: &mut WaveformManager,
    navigation: &mut MinimalNavigation,
) {
    const ARTWORK_SIZE: f32 = 350.0;
    const PLAYER_ROW_HEIGHT: f32 = ARTWORK_SIZE + 28.0;

    let Some(track) = state.current_track() else {
        ui.vertical_centered(|ui| {
            ui.add_space(150.0);
            paint_icon(
                ui.painter(),
                Icon::Play,
                Rect::from_center_size(ui.cursor().center_top(), vec2(54.0, 54.0)),
                ACCENT,
            );
            ui.add_space(62.0);
            text(ui, "Chưa chọn bài hát", 24.0, TEXT, true);
            text(
                ui,
                "Mở Tìm kiếm hoặc Playlist rồi chọn một bài để phát.",
                14.0,
                SECONDARY,
                false,
            );
        });
        return;
    };

    ui.horizontal_top(|ui| {
        Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(CornerRadius::same(16))
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                artwork(ui, artwork_manager, &track, ARTWORK_SIZE);
            });
        ui.add_space(16.0);
        let width = ui.available_width().max(420.0);
        ui.allocate_ui_with_layout(
            vec2(width, PLAYER_ROW_HEIGHT),
            Layout::top_down(Align::Min),
            |ui| {
                // Keep this column deterministic: egui's default item spacing used
                // to make it taller than the framed artwork and pushed the city down.
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.set_width(width);
                ui.set_height(PLAYER_ROW_HEIGHT);
                ui.add_sized(
                    [width, 18.0],
                    egui::Label::new(
                        RichText::new(if track.mood.trim().is_empty() {
                            "BRICKWAVE"
                        } else {
                            &track.mood
                        })
                        .size(font_size(10.0))
                        .strong()
                        .color(SECONDARY),
                    ),
                );
                ui.add_sized(
                    [width, 52.0],
                    egui::Label::new(
                        RichText::new(&track.title)
                            .size(font_size(31.0))
                            .strong()
                            .color(TEXT),
                    )
                    .truncate(),
                );
                ui.add_sized(
                    [width, 30.0],
                    egui::Label::new(
                        RichText::new(&track.artist)
                            .size(font_size(18.0))
                            .color(SECONDARY),
                    )
                    .truncate(),
                );
                ui.add_space(13.0);
                let position = format_position(state.player_state().position_seconds);
                now_playing_waveform(
                    ui,
                    state,
                    waveform_manager,
                    width,
                    state.playback_is_unavailable(),
                    track.waveform_url.as_deref(),
                    &position,
                    &track.duration_label(),
                );
                ui.add_space(8.0);
                minimal_player_controls(ui, state, navigation, width, track.id);
            },
        );
    });
    ui.add_space(12.0);
    minimal_retro_cityscape(ui, state, ui.available_width());
}

fn minimal_retro_cityscape(ui: &mut Ui, state: &UiState, width: f32) {
    const HEIGHT: f32 = 158.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, HEIGHT), Sense::hover());
    let painter = ui.painter().with_clip_rect(rect);

    painter.rect_filled(rect, CornerRadius::same(9), Color32::from_rgb(20, 15, 16));
    let sky = [
        Color32::from_rgb(29, 20, 21),
        Color32::from_rgb(42, 25, 23),
        Color32::from_rgb(57, 29, 23),
        Color32::from_rgb(72, 35, 24),
        Color32::from_rgb(80, 39, 24),
    ];
    let sky_height = rect.height() * 0.72;
    let band_height = sky_height / sky.len() as f32;
    for (index, color) in sky.into_iter().enumerate() {
        let top = rect.top() + index as f32 * band_height;
        painter.rect_filled(
            Rect::from_min_max(
                pos2(rect.left(), top),
                pos2(rect.right(), top + band_height + 1.0),
            ),
            CornerRadius::ZERO,
            color,
        );
    }

    paint_pixel_sun(
        &painter,
        pos2(rect.left() + rect.width() * 0.945, rect.top() + 34.0),
        19.0,
    );

    // Each cloud is an independent layer. Their unequal speeds keep the sky
    // moving without turning the whole city into a texture animation.
    let phase = state.player_state().position_seconds.max(0.0);
    let clouds = [
        (0.01, 39.0, 0.74, 2.0, Color32::from_rgb(104, 54, 31)),
        (0.21, 31.0, 0.86, 3.4, Color32::from_rgb(104, 69, 54)),
        (0.52, 39.0, 0.92, 2.6, Color32::from_rgb(119, 59, 32)),
        (0.71, 30.0, 0.82, 3.1, Color32::from_rgb(101, 67, 53)),
        (0.88, 56.0, 0.62, 2.2, Color32::from_rgb(91, 54, 40)),
        (0.16, 69.0, 0.48, 1.5, Color32::from_rgb(97, 48, 29)),
    ];
    for (offset, y, scale, speed, color) in clouds {
        paint_pixel_cloud(
            &painter,
            rect,
            wrapped_scene_x(rect, rect.width() * offset + phase * speed, 94.0 * scale),
            rect.top() + y,
            scale,
            color,
        );
    }

    let ground = rect.bottom() - 1.0;
    paint_far_city_layer(&painter, rect, ground);

    // x, width and height are normalized for the 6:1 panel. The roof kind
    // varies parapets, stepped roofs, pitched roofs and rooftop tanks.
    let buildings = [
        (0.000, 0.066, 55.0, 0_u8),
        (0.055, 0.047, 84.0, 1),
        (0.101, 0.050, 111.0, 0),
        (0.151, 0.066, 57.0, 1),
        (0.211, 0.057, 80.0, 1),
        (0.254, 0.045, 104.0, 1),
        (0.294, 0.057, 64.0, 0),
        (0.337, 0.050, 118.0, 0),
        (0.386, 0.055, 86.0, 1),
        (0.428, 0.060, 58.0, 0),
        (0.458, 0.050, 102.0, 2),
        (0.501, 0.088, 57.0, 3),
        (0.585, 0.068, 86.0, 2),
        (0.648, 0.061, 67.0, 3),
        (0.699, 0.050, 48.0, 0),
        (0.744, 0.052, 94.0, 1),
        (0.786, 0.058, 56.0, 0),
        (0.821, 0.052, 72.0, 1),
        (0.855, 0.056, 118.0, 0),
        (0.903, 0.062, 65.0, 3),
        (0.946, 0.054, 46.0, 0),
        (0.981, 0.040, 87.0, 2),
    ];
    for (index, (x, width, height, roof_kind)) in buildings.into_iter().enumerate() {
        paint_city_building(&painter, rect, ground, x, width, height, roof_kind, index);
    }

    for (offset, height, faces_right) in [
        (0.095, 46.0, true),
        (0.322, 43.0, true),
        (0.551, 45.0, false),
        (0.611, 46.0, true),
        (0.811, 45.0, true),
    ] {
        paint_retro_street_light(
            &painter,
            pos2(rect.left() + rect.width() * offset, rect.bottom() - 1.0),
            height,
            faces_right,
        );
    }

    // There is deliberately no orange horizon line: individual rooftop and
    // window accents provide the orange rhythm without splitting the scene.
    painter.rect_stroke(
        rect,
        CornerRadius::same(9),
        Stroke::new(1.0, BORDER),
        egui::StrokeKind::Inside,
    );
}

fn paint_far_city_layer(painter: &egui::Painter, rect: Rect, ground: f32) {
    let heights = [
        30.0, 42.0, 27.0, 47.0, 35.0, 51.0, 29.0, 44.0, 33.0, 48.0, 37.0,
    ];
    let width = rect.width() / heights.len() as f32;
    for (index, height) in heights.into_iter().enumerate() {
        let left = rect.left() + index as f32 * width - 2.0;
        let building = Rect::from_min_max(
            pos2(left, ground - height),
            pos2(left + width + 4.0, ground),
        );
        painter.rect_filled(
            building,
            CornerRadius::ZERO,
            if index % 2 == 0 {
                Color32::from_rgb(31, 25, 23)
            } else {
                Color32::from_rgb(35, 27, 24)
            },
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_city_building(
    painter: &egui::Painter,
    scene: Rect,
    ground: f32,
    x_fraction: f32,
    width_fraction: f32,
    height: f32,
    roof_kind: u8,
    seed: usize,
) {
    let left = scene.left() + scene.width() * x_fraction;
    let width = (scene.width() * width_fraction).max(23.0);
    let body_top = ground - height;
    let body = Rect::from_min_max(pos2(left, body_top), pos2(left + width, ground));
    let face = if seed % 3 == 0 {
        Color32::from_rgb(17, 18, 17)
    } else if seed % 3 == 1 {
        Color32::from_rgb(20, 20, 18)
    } else {
        Color32::from_rgb(23, 21, 18)
    };

    match roof_kind {
        1 => {
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(left + width * 0.16, body_top - 9.0),
                    pos2(left + width * 0.84, body_top),
                ),
                CornerRadius::ZERO,
                face,
            );
            painter.line_segment(
                [
                    pos2(left + width * 0.16, body_top - 9.0),
                    pos2(left + width * 0.84, body_top - 9.0),
                ],
                Stroke::new(1.0, ACCENT.gamma_multiply(0.28)),
            );
        }
        2 => {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    pos2(left, body_top),
                    pos2(left + width * 0.5, body_top - 12.0),
                    pos2(left + width, body_top),
                ],
                face,
                Stroke::new(1.0, ACCENT.gamma_multiply(0.33)),
            ));
        }
        3 => {
            let tank_x = left + width * 0.54;
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(tank_x - 7.0, body_top - 9.0),
                    pos2(tank_x + 7.0, body_top),
                ),
                CornerRadius::same(4),
                Color32::from_rgb(20, 20, 18),
            );
            painter.line_segment(
                [
                    pos2(tank_x - 5.0, body_top),
                    pos2(tank_x - 5.0, body_top + 5.0),
                ],
                Stroke::new(1.0, Color32::from_rgb(11, 12, 11)),
            );
            painter.line_segment(
                [
                    pos2(tank_x + 5.0, body_top),
                    pos2(tank_x + 5.0, body_top + 5.0),
                ],
                Stroke::new(1.0, Color32::from_rgb(11, 12, 11)),
            );
        }
        _ => {}
    }

    painter.rect_filled(body, CornerRadius::ZERO, face);
    painter.line_segment(
        [
            pos2(body.left(), body.top()),
            pos2(body.right(), body.top()),
        ],
        Stroke::new(1.0, ACCENT.gamma_multiply(0.28)),
    );

    if seed % 4 == 2 || seed == 8 || seed == 18 {
        let antenna_x = body.center().x;
        painter.line_segment(
            [
                pos2(antenna_x, body.top()),
                pos2(antenna_x, body.top() - 15.0),
            ],
            Stroke::new(1.0, SECONDARY.gamma_multiply(0.42)),
        );
        painter.rect_filled(
            Rect::from_center_size(pos2(antenna_x, body.top() - 16.0), vec2(2.5, 3.0)),
            CornerRadius::ZERO,
            ACCENT_HOVER.gamma_multiply(0.72),
        );
    }

    let mut row = 0usize;
    let mut y = body.top() + 11.0;
    while y + 5.0 < body.bottom() - 5.0 {
        let mut column = 0usize;
        let mut x = body.left() + 8.0;
        while x + 4.0 < body.right() - 5.0 {
            if (seed * 3 + row * 5 + column * 7) % 6 <= 2 {
                painter.rect_filled(
                    Rect::from_min_size(pos2(x, y), vec2(4.0, 5.0)),
                    CornerRadius::ZERO,
                    if (seed + row + column) % 4 == 0 {
                        ACCENT_HOVER.gamma_multiply(0.88)
                    } else {
                        ACCENT.gamma_multiply(0.72)
                    },
                );
            }
            column += 1;
            x += 11.0;
        }
        row += 1;
        y += 12.0;
    }
}

fn paint_pixel_sun(painter: &egui::Painter, center: egui::Pos2, radius: f32) {
    let rows = [
        (-15.0, 20.0),
        (-11.0, 29.0),
        (-6.0, 35.0),
        (0.0, 39.0),
        (6.0, 35.0),
        (11.0, 29.0),
        (15.0, 20.0),
    ];
    for (dy, width) in rows {
        painter.rect_filled(
            Rect::from_center_size(pos2(center.x, center.y + dy), vec2(width, 6.0)),
            CornerRadius::ZERO,
            ACCENT,
        );
    }
    painter.rect_filled(
        Rect::from_center_size(center, vec2(radius * 1.12, radius * 1.35)),
        CornerRadius::ZERO,
        ACCENT_HOVER.gamma_multiply(0.78),
    );
}

fn paint_retro_street_light(
    painter: &egui::Painter,
    base: egui::Pos2,
    height: f32,
    faces_right: bool,
) {
    let direction = if faces_right { 1.0 } else { -1.0 };
    let pole_top = pos2(base.x, base.y - height);
    let lamp_center = pos2(pole_top.x + direction * 8.0, pole_top.y + 2.0);

    painter.circle_filled(
        lamp_center,
        15.0,
        Color32::from_rgba_unmultiplied(232, 226, 211, 25),
    );
    painter.circle_filled(
        lamp_center,
        9.0,
        Color32::from_rgba_unmultiplied(230, 226, 214, 48),
    );
    painter.line_segment(
        [pos2(base.x, base.y), pole_top],
        Stroke::new(3.0, Color32::from_rgb(110, 106, 98)),
    );
    painter.line_segment(
        [pole_top, pos2(lamp_center.x, pole_top.y)],
        Stroke::new(3.0, Color32::from_rgb(110, 106, 98)),
    );
    painter.add(egui::Shape::convex_polygon(
        vec![
            pos2(lamp_center.x - 6.0, lamp_center.y - 5.0),
            pos2(lamp_center.x + 6.0, lamp_center.y - 5.0),
            pos2(lamp_center.x + 4.0, lamp_center.y + 6.0),
            pos2(lamp_center.x - 4.0, lamp_center.y + 6.0),
        ],
        Color32::from_rgb(94, 89, 80),
        Stroke::new(1.0, SECONDARY.gamma_multiply(0.72)),
    ));
    painter.rect_filled(
        Rect::from_center_size(pos2(lamp_center.x, lamp_center.y + 1.0), vec2(5.0, 7.0)),
        CornerRadius::ZERO,
        Color32::from_rgb(238, 231, 214),
    );
    painter.rect_filled(
        Rect::from_min_max(pos2(base.x - 5.0, base.y - 3.0), pos2(base.x + 5.0, base.y)),
        CornerRadius::ZERO,
        Color32::from_rgb(110, 106, 98),
    );
}

fn wrapped_scene_x(rect: Rect, offset: f32, cloud_width: f32) -> f32 {
    let span = rect.width() + cloud_width * 2.0;
    rect.left() - cloud_width + offset.rem_euclid(span)
}

fn paint_pixel_cloud(
    painter: &egui::Painter,
    clip: Rect,
    x: f32,
    y: f32,
    scale: f32,
    color: Color32,
) {
    let blocks = [
        (0.0, 10.0, 72.0, 5.0),
        (9.0, 6.0, 54.0, 5.0),
        (20.0, 2.0, 31.0, 5.0),
        (27.0, 0.0, 17.0, 4.0),
        (64.0, 12.0, 17.0, 3.0),
    ];
    for (dx, dy, width, height) in blocks {
        let cloud_block = Rect::from_min_size(
            pos2(x + dx * scale, y + dy * scale),
            vec2(width * scale, height * scale),
        );
        if clip.intersects(cloud_block) {
            painter.rect_filled(cloud_block, CornerRadius::ZERO, color);
        }
    }
}

fn minimal_player_controls(
    ui: &mut Ui,
    state: &mut UiState,
    navigation: &mut MinimalNavigation,
    width: f32,
    track_id: TrackId,
) {
    Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::ZERO)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.set_width(width);
            minimal_transport(ui, state, navigation, width, track_id);
            let (divider, _) = ui.allocate_exact_size(vec2(width, 1.0), Sense::hover());
            ui.painter().line_segment(
                [
                    pos2(divider.left() + 14.0, divider.center().y),
                    pos2(divider.right() - 14.0, divider.center().y),
                ],
                Stroke::new(1.0, SUBTLE),
            );
            minimal_volume_control(ui, state, navigation, width);
        });
}

fn minimal_transport(
    ui: &mut Ui,
    state: &mut UiState,
    navigation: &mut MinimalNavigation,
    width: f32,
    track_id: TrackId,
) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 92.0), Sense::hover());
    let y = rect.center().y;
    let previous = pos2(rect.left() + 100.0, y);
    let play = rect.center();
    let next = pos2(rect.right() - 100.0, y);
    let like = pos2(rect.right() - 26.0, y);
    let is_playing = state.is_playing();
    let liked = state.is_liked(track_id);

    if minimal_round_control(
        ui,
        previous,
        0,
        Icon::Previous,
        46.0,
        navigation.focus == 0,
        false,
    )
    .clicked()
    {
        navigation.focus = 0;
        state.previous();
    }
    if minimal_round_control(
        ui,
        play,
        1,
        if is_playing { Icon::Pause } else { Icon::Play },
        62.0,
        navigation.focus == 1,
        true,
    )
    .clicked()
    {
        navigation.focus = 1;
        state.toggle_playback();
    }
    if minimal_round_control(ui, next, 2, Icon::Next, 46.0, navigation.focus == 2, false).clicked()
    {
        navigation.focus = 2;
        state.next();
    }
    if minimal_round_control(
        ui,
        like,
        3,
        if liked { Icon::LikeFilled } else { Icon::Like },
        36.0,
        navigation.focus == 3,
        false,
    )
    .clicked()
    {
        navigation.focus = 3;
        state.toggle_like(track_id);
    }
}

fn minimal_volume_control(
    ui: &mut Ui,
    state: &mut UiState,
    navigation: &mut MinimalNavigation,
    width: f32,
) {
    // 92 px transport + 1 px divider + 66 px volume makes the visible
    // controls panel end on the same baseline as the 378 px artwork panel.
    const HEIGHT: f32 = 66.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, HEIGHT), Sense::hover());

    paint_icon(
        ui.painter(),
        Icon::Volume,
        Rect::from_center_size(pos2(rect.left() + 26.0, rect.center().y), vec2(22.0, 22.0)),
        SECONDARY,
    );

    let decrease_center = pos2(rect.left() + 62.0, rect.center().y);
    let increase_center = pos2(rect.right() - 34.0, rect.center().y);
    if minimal_volume_step(ui, decrease_center, "decrease", "−", navigation.focus == 4).clicked()
    {
        navigation.focus = 4;
        let volume = (state.player_state().volume - 0.05).clamp(0.0, 1.0);
        state.set_volume(volume);
    }
    if minimal_volume_step(ui, increase_center, "increase", "+", navigation.focus == 5).clicked() {
        navigation.focus = 5;
        let volume = (state.player_state().volume + 0.05).clamp(0.0, 1.0);
        state.set_volume(volume);
    }

    let percentage_right = rect.right() - 60.0;
    let track_rect = Rect::from_center_size(
        pos2(
            (rect.left() + 94.0 + percentage_right - 56.0) * 0.5,
            rect.center().y,
        ),
        vec2(
            (percentage_right - 56.0 - (rect.left() + 94.0)).max(80.0),
            12.0,
        ),
    );
    let volume = state.player_state().volume.clamp(0.0, 1.0);
    let slider_response = ui.interact(
        track_rect.expand2(vec2(0.0, 11.0)),
        ui.id().with("minimal-volume-slider"),
        Sense::click_and_drag(),
    );
    ui.painter().rect_filled(
        Rect::from_center_size(track_rect.center(), vec2(track_rect.width(), 5.0)),
        CornerRadius::same(3),
        SUBTLE,
    );
    let played_width = track_rect.width() * volume;
    if played_width > 0.0 {
        ui.painter().rect_filled(
            Rect::from_min_size(
                pos2(track_rect.left(), track_rect.center().y - 2.5),
                vec2(played_width, 5.0),
            ),
            CornerRadius::same(3),
            ACCENT,
        );
    }
    let knob_x = track_rect.left() + played_width;
    ui.painter()
        .circle_filled(pos2(knob_x, track_rect.center().y), 7.0, TEXT);
    ui.painter().circle_stroke(
        pos2(knob_x, track_rect.center().y),
        7.0,
        Stroke::new(2.0, ACCENT),
    );

    if let Some(pointer) = slider_response.interact_pointer_pos()
        && (slider_response.clicked() || slider_response.dragged())
    {
        let requested = ((pointer.x - track_rect.left()) / track_rect.width()).clamp(0.0, 1.0);
        if (requested - volume).abs() > f32::EPSILON {
            state.set_volume(requested);
        }
    }

    ui.painter().text(
        pos2(percentage_right, rect.center().y),
        Align2::RIGHT_CENTER,
        format!("{}%", (volume * 100.0).round() as u8),
        egui::FontId::proportional(font_size(11.0)),
        SECONDARY,
    );
}

fn minimal_volume_step(
    ui: &mut Ui,
    center: egui::Pos2,
    id: &'static str,
    label: &'static str,
    focused: bool,
) -> Response {
    let rect = Rect::from_center_size(center, vec2(34.0, 34.0));
    let response = ui.interact(rect, ui.id().with(("minimal-volume", id)), Sense::click());
    ui.painter().circle(
        center,
        16.0,
        if focused { WARM } else { RAISED },
        Stroke::new(
            if focused { 2.0 } else { 1.0 },
            if focused { ACCENT } else { BORDER },
        ),
    );
    ui.painter().text(
        center,
        Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(font_size(18.0)),
        if focused { ACCENT } else { TEXT },
    );
    response
}

fn minimal_round_control(
    ui: &mut Ui,
    center: egui::Pos2,
    id_salt: u8,
    icon: Icon,
    size: f32,
    focused: bool,
    primary: bool,
) -> Response {
    let rect = Rect::from_center_size(center, vec2(size, size));
    let response = ui.interact(
        rect,
        ui.id().with(("minimal-control", id_salt)),
        Sense::click(),
    );
    let fill = if primary {
        ACCENT
    } else if focused {
        WARM
    } else {
        SURFACE
    };
    ui.painter().circle(
        rect.center(),
        size * 0.46,
        fill,
        Stroke::new(
            if focused { 3.0 } else { 1.5 },
            if focused || primary {
                ACCENT_HOVER
            } else {
                BORDER
            },
        ),
    );
    paint_icon(
        ui.painter(),
        icon,
        Rect::from_center_size(rect.center(), vec2(size * 0.43, size * 0.43)),
        if primary {
            APP_BG
        } else if focused {
            ACCENT
        } else {
            TEXT
        },
    );
    response
}

fn minimal_search(
    ui: &mut Ui,
    state: &mut UiState,
    artwork_manager: &mut ArtworkManager,
    navigation: &mut MinimalNavigation,
) {
    section(
        ui,
        "TÌM KIẾM",
        "Tìm bài hát và danh sách phát trên SoundCloud.",
    );
    ui.add_space(10.0);
    let focused = navigation.focus == 0;
    Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(
            if focused { 2.0 } else { 1.0 },
            if focused { ACCENT } else { BORDER },
        ))
        .corner_radius(CornerRadius::same(11))
        .inner_margin(Margin::symmetric(14, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let response = ui.add_sized(
                    [ui.available_width() - 100.0, 42.0],
                    egui::TextEdit::singleline(&mut state.query)
                        .id(search_field_id())
                        .font(egui::TextStyle::Body)
                        .hint_text("Tên bài hát, nghệ sĩ hoặc danh sách phát"),
                );
                if response.clicked() {
                    navigation.focus = 0;
                }
                let submit = hifi_button(ui, "TÌM", true, !state.query.trim().is_empty());
                if submit.clicked()
                    || (response.lost_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter)))
                {
                    state.submit_search();
                }
            });
        });
    ui.add_space(12.0);

    let tracks = state.search_results();
    let playlists = state.search_playlists();
    let scroll = egui::ScrollArea::vertical()
        .id_salt("minimal-search-results")
        .auto_shrink([false, false])
        .show(ui, |ui| match state.search_status() {
            SearchStatus::Idle => minimal_empty(ui, Icon::Search, "Sẵn sàng tìm kiếm"),
            SearchStatus::Loading => {
                ui.horizontal_centered(|ui| {
                    ui.add(egui::Spinner::new().color(ACCENT));
                    text(ui, "Đang tìm trên SoundCloud...", 15.0, TEXT, true);
                });
            }
            SearchStatus::Empty => minimal_empty(ui, Icon::Search, "Không tìm thấy kết quả"),
            SearchStatus::Error => minimal_empty(ui, Icon::Search, "Không thể tải kết quả"),
            SearchStatus::Results { .. } => {
                ui.columns(2, |columns| {
                    let (left, right) = columns.split_at_mut(1);
                    let left = &mut left[0];
                    let right = &mut right[0];
                    micro(left, "BÀI HÁT");
                    left.add_space(6.0);
                    for (index, track_id) in tracks.iter().copied().enumerate() {
                        if let Some(track) = state.track(track_id) {
                            let focus = 1 + index;
                            let response = minimal_track_card(
                                left,
                                artwork_manager,
                                &track,
                                index + 1,
                                navigation.focus == focus,
                            );
                            if navigation.focus == focus {
                                response.scroll_to_me(Some(Align::Center));
                            }
                            if response.clicked() {
                                navigation.focus = focus;
                                state.select_track_from_context(tracks.clone(), index);
                                apply_minimal_page(state, navigation, MinimalPage::Player);
                            }
                            left.add_space(7.0);
                        }
                    }

                    micro(right, "DANH SÁCH PHÁT");
                    right.add_space(6.0);
                    for (index, playlist) in playlists.iter().enumerate() {
                        let focus = 1 + tracks.len() + index;
                        let response = minimal_playlist_card(
                            right,
                            artwork_manager,
                            playlist,
                            navigation.focus == focus,
                        );
                        if navigation.focus == focus {
                            response.scroll_to_me(Some(Align::Center));
                        }
                        if response.clicked() {
                            navigation.focus = 0;
                            navigation.page = MinimalPage::Playlists;
                            state.open_playlist(playlist.id);
                        }
                        right.add_space(7.0);
                    }
                });
            }
        });
    if scroll.state.offset.y > 0.0 {
        let remaining = scroll.content_size.y - scroll.state.offset.y - scroll.inner_rect.height();
        if remaining <= 180.0 {
            state.load_more_search_results();
        }
    }
}

fn minimal_playlists(
    ui: &mut Ui,
    state: &mut UiState,
    artwork_manager: &mut ArtworkManager,
    navigation: &mut MinimalNavigation,
) {
    if state.main_page() == Page::Playlist {
        minimal_playlist_detail(ui, state, artwork_manager, navigation);
        return;
    }
    if state.main_page() == Page::Likes {
        minimal_liked_tracks(ui, state, artwork_manager, navigation);
        return;
    }

    section(
        ui,
        "DANH SÁCH PHÁT",
        "Các danh sách phát trong tài khoản SoundCloud của bạn.",
    );
    ui.add_space(9.0);
    ui.horizontal(|ui| {
        let refresh = hifi_button(
            ui,
            "LÀM MỚI",
            navigation.focus == 0,
            state.auth_state() == AuthState::Authorized,
        );
        if refresh.clicked() {
            navigation.focus = 0;
            state.refresh_user_library();
        }
        let create = hifi_button(
            ui,
            "DANH SÁCH MỚI",
            navigation.focus == 1,
            state.auth_state() == AuthState::Authorized && !state.playlist_create_pending(),
        );
        if create.clicked() {
            navigation.focus = 1;
            state.open_create_playlist();
        }
    });
    ui.add_space(10.0);
    let playlists = state.library_playlists();
    let liked_ids = state.liked_track_ids();
    let liked_artwork = liked_ids
        .first()
        .and_then(|track_id| state.track(*track_id))
        .and_then(|track| track.artwork_url);
    egui::ScrollArea::vertical()
        .id_salt("minimal-library-playlists")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.columns(2, |columns| {
                let liked = minimal_liked_collection_card(
                    &mut columns[0],
                    artwork_manager,
                    liked_artwork.as_deref(),
                    liked_ids.len(),
                    navigation.focus == 2,
                );
                if navigation.focus == 2 {
                    liked.scroll_to_me(Some(Align::Center));
                }
                if liked.clicked() {
                    navigation.focus = 0;
                    state.navigate_main(Page::Likes);
                }
                columns[0].add_space(8.0);

                for (index, playlist) in playlists.iter().enumerate() {
                    let column = &mut columns[(index + 1) % 2];
                    let focus = index + 3;
                    let response = minimal_playlist_card(
                        column,
                        artwork_manager,
                        playlist,
                        navigation.focus == focus,
                    );
                    if navigation.focus == focus {
                        response.scroll_to_me(Some(Align::Center));
                    }
                    if response.clicked() {
                        navigation.focus = 0;
                        state.open_playlist(playlist.id);
                    }
                    column.add_space(8.0);
                }
            });
        });
}

fn minimal_liked_tracks(
    ui: &mut Ui,
    state: &mut UiState,
    artwork_manager: &mut ArtworkManager,
    navigation: &mut MinimalNavigation,
) {
    ui.horizontal(|ui| {
        if icon_button_sized(
            ui,
            "minimal-liked-back",
            Icon::Back,
            false,
            true,
            38.0,
            "Quay lại",
        )
        .clicked()
        {
            state.navigate_back();
            navigation.focus = 0;
        }
        ui.vertical(|ui| {
            micro(ui, "THƯ VIỆN");
            text(ui, "Bài hát đã thích", 24.0, TEXT, true);
        });
    });
    ui.add_space(10.0);
    let ids = state.liked_track_ids();
    egui::ScrollArea::vertical()
        .id_salt("minimal-liked-tracks")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if ids.is_empty() {
                let label = match state.library_status() {
                    LibraryStatus::Loading | LibraryStatus::Idle => "Đang tải bài hát đã thích...",
                    LibraryStatus::Error => "Không thể tải bài hát đã thích",
                    LibraryStatus::Empty | LibraryStatus::Loaded => "Chưa có bài hát đã thích",
                };
                minimal_empty(ui, Icon::Like, label);
                return;
            }
            for (index, track_id) in ids.iter().copied().enumerate() {
                if let Some(track) = state.track(track_id) {
                    let response = minimal_track_card(
                        ui,
                        artwork_manager,
                        &track,
                        index + 1,
                        navigation.focus == index,
                    );
                    if navigation.focus == index {
                        response.scroll_to_me(Some(Align::Center));
                    }
                    if response.clicked() {
                        navigation.focus = index;
                        state.select_track_from_context(ids.clone(), index);
                    }
                    ui.add_space(7.0);
                }
            }
        });
}

fn minimal_playlist_detail(
    ui: &mut Ui,
    state: &mut UiState,
    artwork_manager: &mut ArtworkManager,
    navigation: &mut MinimalNavigation,
) {
    let Some(playlist) = state.current_playlist() else {
        minimal_empty(ui, Icon::Playlist, "Không có dữ liệu danh sách phát");
        return;
    };
    ui.horizontal(|ui| {
        if icon_button_sized(
            ui,
            "minimal-playlist-back",
            Icon::Back,
            false,
            true,
            38.0,
            "Quay lại",
        )
        .clicked()
        {
            state.navigate_back();
            navigation.focus = 0;
        }
        ui.vertical(|ui| {
            micro(ui, "DANH SÁCH PHÁT");
            text(ui, &playlist.title, 24.0, TEXT, true);
        });
    });
    ui.add_space(10.0);
    let ids = playlist.track_ids.clone();
    egui::ScrollArea::vertical()
        .id_salt(("minimal-playlist", playlist.id.get()))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if ids.is_empty() {
                let label = match state.playlist_status() {
                    PlaylistStatus::Loading => "Đang tải bài hát...",
                    PlaylistStatus::Error => "Không thể tải bài hát",
                    PlaylistStatus::Empty | PlaylistStatus::Idle | PlaylistStatus::Loaded => {
                        "Danh sách phát chưa có bài hát"
                    }
                };
                minimal_empty(ui, Icon::Playlist, label);
                return;
            }
            for (index, track_id) in ids.iter().copied().enumerate() {
                if let Some(track) = state.track(track_id) {
                    let response = minimal_track_card(
                        ui,
                        artwork_manager,
                        &track,
                        index + 1,
                        navigation.focus == index,
                    );
                    if navigation.focus == index {
                        response.scroll_to_me(Some(Align::Center));
                    }
                    if response.clicked() {
                        navigation.focus = index;
                        state.select_track_from_context(ids.clone(), index);
                    }
                    ui.add_space(7.0);
                }
            }
        });
}

fn minimal_settings(ui: &mut Ui, state: &mut UiState, navigation: &mut MinimalNavigation) {
    section(ui, "CÀI ĐẶT", "Giao diện và các tùy chọn hiển thị.");
    ui.add_space(10.0);
    let full = minimal_setting_card(
        ui,
        Icon::Library,
        "Giao diện đầy đủ",
        "Thanh bên, các trang chi tiết và điều khiển chuột.",
        !state.minimal_interface,
        navigation.focus == 0,
    );
    if full.clicked() {
        navigation.focus = 0;
        state.set_minimal_interface(false);
    }
    ui.add_space(8.0);
    let compact = minimal_setting_card(
        ui,
        Icon::Play,
        "Giao diện tối giản",
        "Chữ lớn và điều khiển trực tiếp bằng D-pad.",
        state.minimal_interface,
        navigation.focus == 1,
    );
    if compact.clicked() {
        navigation.focus = 1;
        state.set_minimal_interface(true);
    }
    ui.add_space(8.0);
    let always_on = minimal_setting_card(
        ui,
        Icon::Settings,
        "Luôn giữ màn hình",
        "Giảm sáng và giảm render theo thời gian chờ của StockOS.",
        state.always_keep_screen_on,
        navigation.focus == 2,
    );
    if always_on.clicked() {
        navigation.focus = 2;
        state.set_always_keep_screen_on(!state.always_keep_screen_on);
    }
    ui.add_space(8.0);
    let battery = minimal_setting_card(
        ui,
        Icon::Volume,
        "Hiển thị pin",
        "Hiện biểu tượng pin và phần trăm ở thanh trên.",
        state.show_battery_percentage,
        navigation.focus == 3,
    );
    if battery.clicked() {
        navigation.focus = 3;
        state.set_show_battery_percentage(!state.show_battery_percentage);
    }
}

fn minimal_setting_card(
    ui: &mut Ui,
    icon: Icon,
    title: &str,
    detail: &str,
    enabled: bool,
    focused: bool,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 82.0), Sense::click());
    ui.painter().rect(
        rect,
        CornerRadius::same(11),
        if focused { WARM } else { SURFACE },
        Stroke::new(
            if focused { 2.0 } else { 1.0 },
            if focused { ACCENT } else { BORDER },
        ),
        egui::StrokeKind::Inside,
    );
    paint_icon(
        ui.painter(),
        icon,
        Rect::from_center_size(pos2(rect.left() + 36.0, rect.center().y), vec2(30.0, 30.0)),
        if focused { ACCENT } else { SECONDARY },
    );
    ui.painter().text(
        pos2(rect.left() + 68.0, rect.center().y - 13.0),
        Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(font_size(17.0)),
        TEXT,
    );
    ui.painter().text(
        pos2(rect.left() + 68.0, rect.center().y + 15.0),
        Align2::LEFT_CENTER,
        detail,
        egui::FontId::proportional(font_size(12.0)),
        SECONDARY,
    );
    let toggle =
        Rect::from_center_size(pos2(rect.right() - 42.0, rect.center().y), vec2(54.0, 28.0));
    ui.painter().rect_filled(
        toggle,
        CornerRadius::same(14),
        if enabled { ACCENT } else { RAISED },
    );
    ui.painter().circle_filled(
        pos2(
            if enabled {
                toggle.right() - 14.0
            } else {
                toggle.left() + 14.0
            },
            toggle.center().y,
        ),
        10.0,
        if enabled { APP_BG } else { SECONDARY },
    );
    response
}

fn minimal_track_card(
    ui: &mut Ui,
    artwork_manager: &mut ArtworkManager,
    track: &Track,
    number: usize,
    focused: bool,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 68.0), Sense::click());
    ui.painter().rect(
        rect,
        CornerRadius::same(11),
        if focused { WARM } else { SURFACE },
        Stroke::new(
            if focused { 2.0 } else { 1.0 },
            if focused { ACCENT } else { BORDER },
        ),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        pos2(rect.left() + 20.0, rect.center().y),
        Align2::CENTER_CENTER,
        format!("{number:02}"),
        egui::FontId::proportional(font_size(10.5)),
        SECONDARY,
    );
    let content = Rect::from_min_max(
        pos2(rect.left() + 42.0, rect.top() + 8.0),
        rect.max - vec2(10.0, 8.0),
    );
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(content)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            artwork(ui, artwork_manager, track, 52.0);
            ui.add_space(10.0);
            ui.vertical(|ui| {
                ui.add_sized(
                    [ui.available_width(), 26.0],
                    egui::Label::new(
                        RichText::new(&track.title)
                            .size(font_size(15.0))
                            .strong()
                            .color(TEXT),
                    )
                    .truncate(),
                );
                ui.add_sized(
                    [ui.available_width(), 20.0],
                    egui::Label::new(
                        RichText::new(&track.artist)
                            .size(font_size(11.5))
                            .color(SECONDARY),
                    )
                    .truncate(),
                );
            });
        },
    );
    response
}

fn minimal_playlist_card(
    ui: &mut Ui,
    artwork_manager: &mut ArtworkManager,
    playlist: &Playlist,
    focused: bool,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 84.0), Sense::click());
    ui.painter().rect(
        rect,
        CornerRadius::same(11),
        if focused { WARM } else { SURFACE },
        Stroke::new(
            if focused { 2.0 } else { 1.0 },
            if focused { ACCENT } else { BORDER },
        ),
        egui::StrokeKind::Inside,
    );
    let content = rect.shrink2(vec2(10.0, 10.0));
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(content)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            minimal_url_artwork(
                ui,
                artwork_manager,
                playlist.artwork_url.as_deref(),
                64.0,
                Icon::Playlist,
            );
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.add_sized(
                    [ui.available_width(), 30.0],
                    egui::Label::new(
                        RichText::new(&playlist.title)
                            .size(font_size(16.0))
                            .strong()
                            .color(TEXT),
                    )
                    .truncate(),
                );
                text(
                    ui,
                    &format!("{} bài hát", playlist.track_count),
                    11.5,
                    SECONDARY,
                    false,
                );
            });
        },
    );
    response
}

fn minimal_liked_collection_card(
    ui: &mut Ui,
    artwork_manager: &mut ArtworkManager,
    artwork_url: Option<&str>,
    track_count: usize,
    focused: bool,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 84.0), Sense::click());
    ui.painter().rect(
        rect,
        CornerRadius::same(11),
        if focused { WARM } else { SURFACE },
        Stroke::new(
            if focused { 2.0 } else { 1.0 },
            if focused { ACCENT } else { BORDER },
        ),
        egui::StrokeKind::Inside,
    );
    let content = rect.shrink2(vec2(10.0, 10.0));
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(content)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            minimal_url_artwork(ui, artwork_manager, artwork_url, 64.0, Icon::Like);
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.add_sized(
                    [ui.available_width(), 30.0],
                    egui::Label::new(
                        RichText::new("Bài hát đã thích")
                            .size(font_size(16.0))
                            .strong()
                            .color(TEXT),
                    )
                    .truncate(),
                );
                text(
                    ui,
                    &format!("{track_count} bài hát"),
                    11.5,
                    SECONDARY,
                    false,
                );
            });
        },
    );
    response
}

fn minimal_url_artwork(
    ui: &mut Ui,
    artwork_manager: &mut ArtworkManager,
    url: Option<&str>,
    size: f32,
    fallback: Icon,
) {
    let slot = Rect::from_min_size(ui.next_widget_position(), vec2(size, size));
    let visible = ui.is_rect_visible(slot);
    if let Some(texture) = artwork_manager.texture_for_visible_url(url, visible) {
        ui.add(
            egui::Image::from_texture(texture)
                .fit_to_exact_size(vec2(size, size))
                .corner_radius(CornerRadius::same(8)),
        );
        return;
    }
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(8),
        RAISED,
        Stroke::new(1.0, BORDER),
        egui::StrokeKind::Inside,
    );
    paint_icon(ui.painter(), fallback, rect.shrink(size * 0.28), ACCENT);
}

fn minimal_empty(ui: &mut Ui, icon: Icon, label: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space(52.0);
        let (rect, _) = ui.allocate_exact_size(vec2(54.0, 54.0), Sense::hover());
        paint_icon(ui.painter(), icon, rect.shrink(5.0), ACCENT);
        ui.add_space(8.0);
        text(ui, label, 18.0, TEXT, true);
    });
}

fn sidebar(ui: &mut Ui, state: &mut UiState, app_logo: &egui::TextureHandle) {
    let width = if state.sidebar_collapsed {
        64.0
    } else {
        SIDEBAR_WIDTH
    };
    egui::Panel::left("soundcloud-sidebar")
        .exact_size(width)
        .frame(
            Frame::new()
                .fill(SIDEBAR_BG)
                .stroke(Stroke::new(1.0_f32, SUBTLE))
                .inner_margin(Margin::same(10)),
        )
        .show(ui, |ui| {
            brand(ui, state.sidebar_collapsed, app_logo);
            ui.add_space(14.0);
            if !state.sidebar_collapsed {
                micro(ui, "TRÌNH ĐƠN");
            }
            nav(ui, state, Page::Home, Icon::Home, "Trang chủ");
            nav(ui, state, Page::Discover, Icon::Discover, "Khám phá");
            nav(ui, state, Page::Search, Icon::Search, "Tìm kiếm");
            nav(ui, state, Page::Library, Icon::Library, "Thư viện");
            nav(ui, state, Page::Likes, Icon::Like, "Đã thích");
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(5.0);
            nav(ui, state, Page::Settings, Icon::Settings, "Cài đặt");
            nav(ui, state, Page::Profile, Icon::User, "Tài khoản");
            ui.add_space(5.0);
            ui.horizontal(|ui| {
                let collapse = icon_button(
                    ui,
                    "collapse",
                    if state.sidebar_collapsed {
                        Icon::ChevronRight
                    } else {
                        Icon::ChevronLeft
                    },
                    false,
                    true,
                    if state.sidebar_collapsed {
                        "Mở rộng thanh bên"
                    } else {
                        "Thu gọn thanh bên"
                    },
                );
                if collapse.clicked() {
                    state.sidebar_collapsed = !state.sidebar_collapsed;
                }
                if !state.sidebar_collapsed {
                    text(ui, "Thu gọn menu", 11.0, SECONDARY, false);
                }
            });
        });
}

fn brand(ui: &mut Ui, compact: bool, app_logo: &egui::TextureHandle) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(38.0, 38.0), Sense::hover());
        ui.painter().image(
            app_logo.id(),
            rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        if !compact {
            text(ui, "BRICKWAVE", 17.0, ACCENT, true);
        }
    });
}

fn nav(ui: &mut Ui, state: &mut UiState, target: Page, icon: Icon, label: &str) {
    let page = state.main_page();
    let selected = page == target || (target == Page::Stations && page == Page::Playlist);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
    let fill = if selected {
        WARM
    } else if response.hovered() {
        RAISED
    } else {
        Color32::TRANSPARENT
    };
    let border = if response.has_focus() || selected {
        ACCENT
    } else if response.hovered() {
        BORDER
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(SMALL_RADIUS),
        fill,
        Stroke::new(1.0_f32, border),
        egui::StrokeKind::Inside,
    );
    if selected {
        ui.painter().rect_filled(
            Rect::from_min_size(rect.left_top(), vec2(3.0, rect.height())),
            CornerRadius::same(1),
            ACCENT,
        );
    }
    paint_icon(
        ui.painter(),
        icon,
        Rect::from_min_size(rect.min + vec2(8.0, 7.0), vec2(22.0, 22.0)),
        if selected { ACCENT } else { SECONDARY },
    );
    if !state.sidebar_collapsed {
        ui.painter().text(
            rect.min + vec2(39.0, 18.0),
            Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(font_size(13.0)),
            if selected { TEXT } else { SECONDARY },
        );
    }
    if response.clicked() {
        state.navigate_main(target);
    }
    if state.sidebar_collapsed {
        response.on_hover_text(label);
    }
}

fn empty_artwork(ui: &mut Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same((size * 0.13) as u8),
        RAISED,
        Stroke::new(1.0, SUBTLE),
        egui::StrokeKind::Inside,
    );
    paint_icon(
        ui.painter(),
        Icon::Brand,
        rect.shrink(size * 0.24),
        SECONDARY,
    );
}

fn player_bar(ui: &mut Ui, state: &mut UiState, artwork_manager: &mut ArtworkManager) {
    egui::Panel::bottom("soundcloud-player")
        .exact_size(PLAYER_HEIGHT)
        .frame(
            Frame::new()
                .fill(SIDEBAR_BG)
                .stroke(Stroke::new(1.0_f32, BORDER))
                .inner_margin(Margin::symmetric(18, 10)),
        )
        .show(ui, |ui| {
            let track = state.current_track();
            let is_playing = state.is_playing();
            let live = state.playback_is_unavailable();
            let progress = state.progress();
            let position = state.player_state().position_seconds;
            let volume = state.player_state().volume;
            let title = track
                .as_ref()
                .map(|track| track.title.as_str())
                .unwrap_or(if live {
                    "Chọn một bài hát"
                } else {
                    "Trình phát sẵn sàng"
                });
            let artist = track
                .as_ref()
                .map(|track| track.artist.as_str())
                .unwrap_or(if live {
                    "Hãy chọn bài hát"
                } else {
                    "Hãy chọn bài hát xem trước"
                });
            let duration = track
                .as_ref()
                .map(Track::duration_label)
                .unwrap_or_else(|| "--:--".to_owned());
            let full_width = ui.available_width();
            let side_width = (full_width * 0.24).clamp(225.0, 250.0);
            let left_width = side_width;
            let right_width = side_width;
            const COLUMN_GAP: f32 = 12.0;
            const ROW_HEIGHT: f32 = 88.0;
            // `allocate_ui_with_layout` grows its parent allocation whenever a
            // child overflows. The left column gains Add/Like controls after a
            // track is selected, so the old horizontal layout expanded that
            // column and pushed the transport, timeline and volume controls to
            // the right as playback began. Allocate the row once, then place
            // each column in an absolute rect so playback state cannot move the
            // other columns.
            // Equal side columns make the transport's Play/Pause button the
            // fixed visual axis of the whole player bar.
            let center_width =
                (full_width - left_width - right_width - COLUMN_GAP * 2.0).max(350.0);
            let (row_rect, _) =
                ui.allocate_exact_size(vec2(full_width, ROW_HEIGHT), Sense::hover());
            let left_rect = Rect::from_min_size(row_rect.min, vec2(left_width, ROW_HEIGHT));
            let center_rect = Rect::from_min_size(
                pos2(left_rect.right() + COLUMN_GAP, row_rect.top()),
                vec2(center_width, ROW_HEIGHT),
            );
            let right_rect = Rect::from_min_size(
                pos2(center_rect.right() + COLUMN_GAP, row_rect.top()),
                vec2(right_width, ROW_HEIGHT),
            );

            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(left_rect)
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| {
                    ui.set_clip_rect(left_rect);
                    ui.spacing_mut().item_spacing.x = 7.0;
                    let copy_width = (left_width - 58.0 - 7.0 - 4.0).max(64.0);
                    if let Some(track) = track.as_ref() {
                        let art = artwork(ui, artwork_manager, track, 58.0);
                        if art.clicked() {
                            state.open_now_playing();
                        }
                    } else {
                        empty_artwork(ui, 58.0);
                    }
                    ui.vertical(|ui| {
                        micro(
                            ui,
                            if live {
                                "THÔNG TIN BÀI HÁT"
                            } else if is_playing {
                                "ĐANG PHÁT"
                            } else {
                                "TRÌNH PHÁT SẴN SÀNG"
                            },
                        );
                        ui.add_sized(
                            [copy_width, 18.0],
                            egui::Label::new(
                                RichText::new(title)
                                    .size(font_size(13.0))
                                    .color(TEXT)
                                    .strong(),
                            )
                            .truncate(),
                        );
                        ui.add_sized(
                            [copy_width, 16.0],
                            egui::Label::new(
                                RichText::new(artist).size(font_size(11.0)).color(SECONDARY),
                            )
                            .truncate(),
                        );
                    });
                },
            );
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(center_rect)
                    .layout(Layout::top_down(Align::Center)),
                |ui| {
                    ui.set_clip_rect(center_rect);
                    transport_controls(ui, state, false);
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        const TIME_LABEL_WIDTH: f32 = 48.0;
                        const TIMELINE_GAP: f32 = 6.0;
                        ui.spacing_mut().item_spacing.x = TIMELINE_GAP;
                        ui.add_sized(
                            [TIME_LABEL_WIDTH, 24.0],
                            egui::Label::new(
                                RichText::new(format_position(position))
                                    .size(font_size(10.0))
                                    .color(SECONDARY),
                            )
                            .halign(Align::Max),
                        );
                        let mut requested_progress = progress;
                        slider(
                            ui,
                            &mut requested_progress,
                            (center_width - TIME_LABEL_WIDTH * 2.0 - TIMELINE_GAP * 2.0).max(160.0),
                            if live {
                                "Chưa thể phát bài hát này"
                            } else {
                                "Tiến độ bài hát"
                            },
                        );
                        if !live && (requested_progress - progress).abs() > f32::EPSILON {
                            state.seek_progress(requested_progress);
                        }
                        ui.add_sized(
                            [TIME_LABEL_WIDTH, 24.0],
                            egui::Label::new(
                                RichText::new(&duration)
                                    .size(font_size(10.0))
                                    .color(SECONDARY),
                            )
                            .halign(Align::Min),
                        );
                    });
                },
            );
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(right_rect)
                    .layout(Layout::top_down(Align::Center)),
                |ui| {
                    ui.set_clip_rect(right_rect);
                    ui.horizontal(|ui| {
                        let _ = icon_button(ui, "volume", Icon::Volume, false, true, "Âm lượng");
                        let mut requested_volume = volume;
                        slider(
                            ui,
                            &mut requested_volume,
                            (right_width - 48.0).max(76.0),
                            "Âm lượng",
                        );
                        if (requested_volume - volume).abs() > f32::EPSILON {
                            state.set_volume(requested_volume);
                        }
                    });
                    // Keep the three track actions in fixed, equal-width slots
                    // under the volume control. Text beside Queue previously
                    // shifted the group whenever it changed to "OPEN".
                    ui.columns(3, |columns| {
                        columns[0].with_layout(Layout::top_down(Align::Center), |ui| {
                            let q = icon_button(
                                ui,
                                "queue",
                                Icon::Queue,
                                state.queue_open,
                                true,
                                if live {
                                    "Hàng chờ bài hát"
                                } else {
                                    "Hàng chờ xem trước"
                                },
                            );
                            if q.clicked() {
                                state.queue_open = !state.queue_open;
                            }
                        });

                        columns[1].with_layout(Layout::top_down(Align::Center), |ui| {
                            let add_enabled = track.is_some()
                                && state.data_mode == DataMode::Live
                                && !state.playlist_add_pending();
                            let add = icon_button(
                                ui,
                                "player-add-playlist",
                                Icon::Save,
                                false,
                                add_enabled,
                                "Thêm vào danh sách phát",
                            );
                            if add.clicked() {
                                if let Some(track) = track.as_ref() {
                                    state.open_add_to_playlist(track.id);
                                }
                            }
                        });

                        columns[2].with_layout(Layout::top_down(Align::Center), |ui| {
                            let liked =
                                track.as_ref().is_some_and(|track| state.is_liked(track.id));
                            let like_enabled = track
                                .as_ref()
                                .is_some_and(|track| !state.like_pending(track.id));
                            let like = icon_button(
                                ui,
                                "player-like",
                                if liked { Icon::LikeFilled } else { Icon::Like },
                                liked,
                                like_enabled,
                                "Thích bài hát",
                            );
                            if like.clicked() {
                                if let Some(track) = track.as_ref() {
                                    state.toggle_like(track.id);
                                }
                            }
                        });
                    });
                },
            );
        });
}
fn transport_controls(ui: &mut Ui, state: &mut UiState, include_like: bool) {
    let small_controls = if include_like { 5.0 } else { 4.0 };
    let gaps = if include_like { 5.0 } else { 4.0 };
    let width = CONTROL_HITBOX * small_controls + 43.0 + CONTROL_GAP * gaps;
    ui.allocate_ui_with_layout(
        vec2(width, 43.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = CONTROL_GAP;
            let shuffle = state.player_state().shuffle_enabled;
            let repeat_mode = state.player_state().repeat_mode;
            let current_track = state.current_track();
            let playing = state.is_playing();
            if icon_button(
                ui,
                "shuffle",
                Icon::Shuffle,
                shuffle,
                true,
                "Phát ngẫu nhiên",
            )
            .clicked()
            {
                state.set_shuffle(!shuffle);
            }
            if icon_button(ui, "previous", Icon::Previous, false, true, "Bài trước").clicked() {
                state.previous();
            }
            if big_play(ui, playing).clicked() {
                state.toggle_playback();
            }
            if icon_button(ui, "next", Icon::Next, false, true, "Bài tiếp theo").clicked() {
                state.next();
            }
            let repeat = if repeat_mode == RepeatMode::One {
                Icon::RepeatOne
            } else {
                Icon::Repeat
            };
            if icon_button(
                ui,
                "repeat",
                repeat,
                repeat_mode != RepeatMode::Off,
                true,
                "Lặp lại",
            )
            .clicked()
            {
                state.cycle_repeat();
            }
            if include_like {
                if let Some(track) = current_track {
                    let liked = state.is_liked(track.id);
                    let like = icon_button(
                        ui,
                        "transport-like",
                        if liked { Icon::LikeFilled } else { Icon::Like },
                        liked,
                        true,
                        "Thích bài hát",
                    );
                    if like.clicked() {
                        state.toggle_like(track.id);
                    }
                }
            }
        },
    );
}

fn big_play(ui: &mut Ui, playing: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(43.0, 43.0), Sense::click());
    ui.painter().circle_filled(
        rect.center(),
        20.5,
        if response.hovered() {
            ACCENT_HOVER
        } else {
            ACCENT
        },
    );
    if response.has_focus() {
        ui.painter()
            .circle_stroke(rect.center(), 21.5, Stroke::new(1.0_f32, TEXT));
    }
    let icon_rect = Rect::from_center_size(rect.center(), vec2(ICON_SIZE_LARGE, ICON_SIZE_LARGE));
    paint_icon(
        ui.painter(),
        if playing { Icon::Pause } else { Icon::Play },
        icon_rect,
        APP_BG,
    );
    response
}

fn slider(ui: &mut Ui, value: &mut f32, width: f32, hint: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 24.0), Sense::click_and_drag());
    if let Some(p) = response.interact_pointer_pos() {
        if response.dragged() || response.clicked() {
            *value = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        }
    }
    let track = Rect::from_center_size(rect.center(), vec2(rect.width(), 5.0));
    let filled = Rect::from_min_max(
        track.min,
        pos2(track.left() + track.width() * *value, track.bottom()),
    );
    ui.painter()
        .rect_filled(track, CornerRadius::same(3), SUBTLE);
    ui.painter()
        .rect_filled(filled, CornerRadius::same(3), ACCENT);
    ui.painter().circle_filled(
        pos2(track.left() + track.width() * *value, track.center().y),
        if response.hovered() { 6.0 } else { 5.0 },
        TEXT,
    );
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(SMALL_RADIUS),
            Stroke::new(1.0_f32, ACCENT),
            egui::StrokeKind::Inside,
        );
    }
    response.on_hover_text(hint)
}

fn page(
    ui: &mut Ui,
    state: &mut UiState,
    artwork_manager: &mut ArtworkManager,
    waveform_manager: &mut WaveformManager,
) {
    let page = state.main_page();
    if page == Page::NowPlaying {
        now_playing(ui, state, artwork_manager, waveform_manager);
        return;
    }
    header(ui, state, artwork_manager);
    ui.add_space(12.0);
    let scroll = egui::ScrollArea::vertical()
        .id_salt(("main-page", page))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if state.data_mode == DataMode::Live && page == Page::Stations {
                live_metadata_placeholder(ui, state);
                return;
            }
            match page {
                Page::Home => home(ui, state, artwork_manager),
                Page::Discover => discover(ui, state, artwork_manager),
                Page::Search => search(ui, state, artwork_manager),
                Page::Library => library(ui, state, artwork_manager),
                Page::Likes => likes(ui, state, artwork_manager),
                Page::Playlist => playlist(ui, state, artwork_manager),
                Page::Profile => profile(ui, state, artwork_manager),
                Page::Stations => stations(ui, state, artwork_manager),
                Page::Following => following(ui, state, artwork_manager),
                Page::Settings => settings(ui, state),
                Page::NowPlaying => unreachable!(),
            }
            ui.add_space(12.0);
        });

    // Search pagination is driven by deliberate user scrolling. Requiring a
    // non-zero offset prevents a short first page from fetching every page as
    // soon as it is rendered. UiState also permits only one request in flight.
    if page == Page::Search && scroll.state.offset.y > 0.0 {
        let remaining = scroll.content_size.y - scroll.state.offset.y - scroll.inner_rect.height();
        if remaining <= 220.0 {
            state.load_more_search_results();
        }
    }
}

/// LIVE never substitutes preview collections for unavailable account data.
fn live_metadata_placeholder(ui: &mut Ui, state: &mut UiState) {
    section(
        ui,
        "BRICKWAVE",
        "Tìm bài hát và nghệ sĩ bằng trang Tìm kiếm.",
    );
    ui.add_space(12.0);
    empty(
        ui,
        Icon::Search,
        "Tìm nhạc",
        "Dùng ô tìm kiếm phía trên để tìm nhạc.",
    );
    if hifi_button(ui, "MỞ TÌM KIẾM", true, true).clicked() {
        state.navigate_main(Page::Search);
    }
}

fn header(ui: &mut Ui, state: &mut UiState, artwork_manager: &mut ArtworkManager) {
    ui.horizontal(|ui| {
        if state.can_navigate_back()
            && icon_button_sized(ui, "page-back", Icon::Back, false, true, 32.0, "Quay lại")
                .clicked()
        {
            state.navigate_back();
        }
        ui.vertical(|ui| {
            let title = if state.main_page() == Page::Playlist {
                state
                    .current_playlist()
                    .map(|playlist| playlist.title)
                    .unwrap_or_else(|| "Danh sách phát".to_owned())
            } else {
                state.main_page().title().to_owned()
            };
            text(ui, &title, 25.0, TEXT, true);
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            account_menu(ui, state, artwork_manager);
            ui.add_space(7.0);
            if state.show_battery_percentage {
                battery_indicator(ui, state.battery_percentage());
                ui.add_space(7.0);
            }
            search_box(ui, state, 270.0);
        });
    });
}

fn battery_indicator(ui: &mut Ui, percentage: Option<u8>) {
    let (rect, response) = ui.allocate_exact_size(vec2(72.0, TOP_CONTROL_HEIGHT), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(TOP_CONTROL_RADIUS),
        if response.hovered() { RAISED } else { SURFACE },
        Stroke::new(
            1.0,
            if response.hovered() {
                ACCENT_HOVER
            } else {
                BORDER
            },
        ),
        egui::StrokeKind::Inside,
    );

    let body = Rect::from_center_size(pos2(rect.left() + 19.0, rect.center().y), vec2(23.0, 12.0));
    ui.painter().rect_stroke(
        body,
        CornerRadius::same(2),
        Stroke::new(1.4, TEXT),
        egui::StrokeKind::Inside,
    );
    ui.painter().rect_filled(
        Rect::from_min_max(
            pos2(body.right(), body.center().y - 3.0),
            pos2(body.right() + 3.0, body.center().y + 3.0),
        ),
        CornerRadius::same(1),
        TEXT,
    );
    if let Some(value) = percentage {
        let fill_width = (body.width() - 4.0) * value as f32 / 100.0;
        if fill_width > 0.0 {
            ui.painter().rect_filled(
                Rect::from_min_size(
                    body.min + vec2(2.0, 2.0),
                    vec2(fill_width, body.height() - 4.0),
                ),
                CornerRadius::same(1),
                if value <= 20 { ACCENT_HOVER } else { ACCENT },
            );
        }
    }
    ui.painter().text(
        pos2(rect.left() + 38.0, rect.center().y),
        Align2::LEFT_CENTER,
        percentage
            .map(|value| format!("{value}%"))
            .unwrap_or_else(|| "--%".to_owned()),
        egui::FontId::proportional(font_size(10.5)),
        if percentage.is_some() { TEXT } else { DISABLED },
    );
    response.on_hover_text(match percentage {
        Some(value) => format!("Pin {value}%"),
        None => "Không đọc được phần trăm pin".to_owned(),
    });
}

fn search_field_id() -> Id {
    Id::new("brickwave-global-search-input")
}

fn playlist_title_field_id() -> Id {
    Id::new("brickwave-playlist-title-input")
}

fn stop_timer_hours_field_id() -> Id {
    Id::new("brickwave-stop-timer-hours")
}

fn stop_timer_minutes_field_id() -> Id {
    Id::new("brickwave-stop-timer-minutes")
}

fn search_box(ui: &mut Ui, state: &mut UiState, width: f32) {
    let mut submit = false;
    let (rect, container_response) =
        ui.allocate_exact_size(vec2(width, TOP_CONTROL_HEIGHT), Sense::hover());
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(TOP_CONTROL_RADIUS),
        if container_response.hovered() {
            RAISED
        } else {
            SURFACE
        },
    );

    // These slots never change when text is entered. The magnifier therefore
    // remains aligned with the account avatar and the TextEdit never jumps
    // when the clear button appears.
    let search_rect =
        Rect::from_center_size(pos2(rect.left() + 20.0, rect.center().y), vec2(28.0, 28.0));
    let clear_rect =
        Rect::from_center_size(pos2(rect.right() - 18.0, rect.center().y), vec2(24.0, 24.0));
    let field_rect = Rect::from_min_max(
        pos2(search_rect.right() + 4.0, rect.top() + 4.0),
        pos2(clear_rect.left() - 4.0, rect.bottom() - 4.0),
    );
    let can_submit = !state.query.trim().is_empty();
    let search_response = ui
        .interact(search_rect, ui.id().with("submit-search"), Sense::click())
        .on_hover_text(if can_submit {
            "Bắt đầu tìm kiếm"
        } else {
            "Hãy nhập nội dung cần tìm"
        });
    paint_icon(
        ui.painter(),
        Icon::Search,
        search_rect.shrink(5.0),
        if can_submit || search_response.hovered() {
            ACCENT
        } else {
            SECONDARY
        },
    );
    submit |= can_submit && search_response.clicked();

    let field = ui
        .scope_builder(
            egui::UiBuilder::new()
                .max_rect(field_rect)
                .layout(Layout::left_to_right(Align::Center)),
            |ui| {
                ui.add_sized(
                    field_rect.size(),
                    egui::TextEdit::singleline(&mut state.query)
                        .id(search_field_id())
                        .frame(Frame::NONE)
                        .vertical_align(Align::Center)
                        .text_color(TEXT)
                        .hint_text(RichText::new("Tìm bài hát và danh sách phát").color(SECONDARY)),
                )
            },
        )
        .inner;
    submit |= field.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));

    let mut clear_hovered = false;
    if !state.query.is_empty() {
        let clear = ui
            .interact(clear_rect, ui.id().with("clear-search"), Sense::click())
            .on_hover_text("Xóa nội dung tìm kiếm");
        clear_hovered = clear.hovered();
        paint_icon(
            ui.painter(),
            Icon::Close,
            clear_rect.shrink(5.0),
            if clear.hovered() { TEXT } else { SECONDARY },
        );
        if clear.clicked() {
            state.clear_search_input();
            field.request_focus();
        }
    }

    let focused = field.has_focus();
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(TOP_CONTROL_RADIUS),
        Stroke::new(
            1.0,
            if focused {
                ACCENT
            } else if container_response.hovered() || search_response.hovered() || clear_hovered {
                ACCENT_HOVER
            } else {
                BORDER
            },
        ),
        egui::StrokeKind::Inside,
    );
    if submit && !state.query.trim().is_empty() {
        state.submit_search();
    }
}
fn account_menu(ui: &mut Ui, state: &mut UiState, artwork_manager: &mut ArtworkManager) {
    let button = account_button(ui, state, artwork_manager);
    egui::Popup::menu(&button)
        .width(242.0)
        .frame(
            Frame::new()
                .fill(SURFACE)
                .stroke(Stroke::new(1.0_f32, BORDER))
                .corner_radius(CornerRadius::same(RADIUS))
                .inner_margin(Margin::same(10)),
        )
        .show(|ui| {
            ui.set_min_width(225.0);
            ui.horizontal(|ui| {
                let (avatar_rect, _) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::hover());
                paint_account_avatar(ui, state, artwork_manager, avatar_rect);
                ui.vertical(|ui| {
                    let account_name = state
                        .current_profile()
                        .map(|profile| profile.username.as_str())
                        .unwrap_or_else(|| {
                            if state.auth_state() == AuthState::AuthorizationPending {
                                "Đang khôi phục tài khoản"
                            } else if state.data_mode == DataMode::Live {
                                "Kết nối tài khoản"
                            } else {
                                "Tài khoản xem trước"
                            }
                        });
                    text(ui, account_name, 14.0, TEXT, true);
                    text(
                        ui,
                        if state.auth_state() == AuthState::Authorized {
                            "Tài khoản đã kết nối"
                        } else if state.data_mode == DataMode::Live {
                            "Tài khoản SoundCloud"
                        } else {
                            "Dữ liệu và trình phát thử nghiệm"
                        },
                        10.0,
                        SECONDARY,
                        false,
                    );
                });
            });
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(3.0);
            if matches!(
                state.auth_state(),
                AuthState::Authorized | AuthState::AuthorizationPending
            ) {
                account_item(ui, state, Icon::User, "Hồ sơ", Some(Page::Profile), true);
            } else if account_item(ui, state, Icon::User, "Kết nối tài khoản", None, true) {
                state.open_login();
                ui.close();
            }
            account_item(ui, state, Icon::Like, "Đã thích", Some(Page::Likes), true);
            if account_item(
                ui,
                state,
                Icon::Playlist,
                "Danh sách phát",
                Some(Page::Library),
                true,
            ) {
                state.library_tab = LibraryTab::Playlists;
            }
            account_item(
                ui,
                state,
                Icon::Discover,
                "Trạm nhạc",
                Some(Page::Stations),
                true,
            );
            account_item(
                ui,
                state,
                Icon::User,
                "Đang theo dõi",
                Some(Page::Following),
                true,
            );
            account_item(ui, state, Icon::Discover, "Gợi ý theo dõi", None, false);
            ui.add_space(4.0);
            micro(ui, "CÔNG CỤ NGHỆ SĨ");
            account_item(ui, state, Icon::Brand, "Dùng thử Artist Pro", None, false);
            account_item(ui, state, Icon::Save, "Quyền lợi", None, false);
            account_item(ui, state, Icon::Library, "Bài hát", None, false);
            account_item(ui, state, Icon::Discover, "Thống kê", None, false);
            account_item(ui, state, Icon::More, "Phân phối", None, false);
        });
}

fn account_button(ui: &mut Ui, state: &UiState, artwork_manager: &mut ArtworkManager) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(178.0, TOP_CONTROL_HEIGHT), Sense::click());
    let fill = if response.hovered() { RAISED } else { SURFACE };
    let border = if response.has_focus() {
        ACCENT
    } else if response.hovered() {
        ACCENT_HOVER
    } else {
        BORDER
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(TOP_CONTROL_RADIUS),
        fill,
        Stroke::new(1.0_f32, border),
        egui::StrokeKind::Inside,
    );
    paint_account_avatar(
        ui,
        state,
        artwork_manager,
        Rect::from_center_size(pos2(rect.left() + 20.0, rect.center().y), vec2(26.0, 26.0)),
    );
    ui.painter().text(
        pos2(rect.left() + 40.0, rect.center().y),
        Align2::LEFT_CENTER,
        state
            .current_profile()
            .map(|profile| profile.username.as_str())
            .unwrap_or_else(|| {
                if state.auth_state() == AuthState::AuthorizationPending {
                    "Đang khôi phục"
                } else if state.data_mode == DataMode::Live {
                    "Kết nối tài khoản"
                } else {
                    "Tài khoản xem trước"
                }
            }),
        egui::FontId::proportional(font_size(12.0)),
        TEXT,
    );
    paint_icon(
        ui.painter(),
        Icon::ChevronDown,
        Rect::from_center_size(
            pos2(rect.right() - 18.0, rect.center().y),
            vec2(ICON_SIZE_SMALL, ICON_SIZE_SMALL),
        ),
        SECONDARY,
    );
    response.on_hover_text("Mở trình đơn tài khoản")
}

fn paint_account_avatar(
    ui: &Ui,
    state: &UiState,
    artwork_manager: &mut ArtworkManager,
    rect: Rect,
) {
    let texture = state
        .current_profile()
        .and_then(|profile| profile.avatar_url.as_deref())
        .and_then(|url| artwork_manager.texture_for_visible_url(Some(url), true));
    if let Some(texture) = texture {
        egui::Image::from_texture(texture)
            .fit_to_exact_size(rect.size())
            .corner_radius(CornerRadius::same((rect.width() * 0.5) as u8))
            .paint_at(ui, rect);
        ui.painter().circle_stroke(
            rect.center(),
            rect.width() * 0.5,
            Stroke::new(1.0_f32, ACCENT),
        );
    } else {
        paint_avatar(ui.painter(), rect);
    }
}
fn paint_avatar(painter: &egui::Painter, rect: Rect) {
    painter.circle_filled(rect.center(), rect.width() * 0.5, WARM);
    painter.circle_stroke(
        rect.center(),
        rect.width() * 0.5,
        Stroke::new(1.0_f32, ACCENT),
    );
    paint_icon(
        painter,
        Icon::User,
        Rect::from_center_size(rect.center(), vec2(ICON_SIZE_MEDIUM, ICON_SIZE_MEDIUM)),
        TEXT,
    );
}
fn avatar(ui: &mut Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    paint_avatar(ui.painter(), rect);
}
fn account_item(
    ui: &mut Ui,
    state: &mut UiState,
    icon: Icon,
    label: &str,
    target: Option<Page>,
    enabled: bool,
) -> bool {
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), 31.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let fill = if enabled && response.hovered() {
        RAISED
    } else {
        Color32::TRANSPARENT
    };
    let border = if response.has_focus() {
        ACCENT
    } else {
        Color32::TRANSPARENT
    };
    let color = if enabled {
        if response.hovered() { TEXT } else { SECONDARY }
    } else {
        DISABLED
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(SMALL_RADIUS),
        fill,
        Stroke::new(1.0_f32, border),
        egui::StrokeKind::Inside,
    );
    paint_icon(
        ui.painter(),
        icon,
        Rect::from_min_size(rect.min + vec2(6.0, 5.0), vec2(21.0, 21.0)),
        color,
    );
    ui.painter().text(
        rect.min + vec2(34.0, 15.5),
        Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(font_size(12.0)),
        color,
    );
    if enabled {
        paint_icon(
            ui.painter(),
            Icon::ChevronRight,
            Rect::from_min_size(rect.right_top() - vec2(27.0, -5.0), vec2(21.0, 21.0)),
            SECONDARY,
        );
    }
    let clicked = response.clicked();
    if clicked {
        if let Some(target) = target {
            state.navigate_main(target);
            ui.close();
        }
    }
    clicked
}

fn home(ui: &mut Ui, state: &mut UiState, artwork_manager: &mut ArtworkManager) {
    if state.data_mode == DataMode::Live {
        live_home(ui, state, artwork_manager);
        return;
    }
    hero(ui, state, artwork_manager);
    ui.add_space(17.0);
    section(
        ui,
        "ĐANG THỊNH HÀNH",
        "Chọn một bài hát để dùng thử trình phát cục bộ.",
    );
    ui.add_space(5.0);
    ui.columns(2, |columns| {
        for (slot, index) in [0_usize, 1, 2, 3].iter().enumerate() {
            track_card(&mut columns[slot % 2], state, artwork_manager, *index);
        }
    });
    ui.add_space(14.0);
    section(
        ui,
        "DÀNH CHO BRICK",
        "Các bộ sưu tập được tối ưu cho màn hình 1024 × 768.",
    );
    ui.horizontal_wrapped(|ui| {
        chip(ui, state, "Sau hoàng hôn", state.preview_track(1).id);
        chip(ui, state, "Băng nhạc ấm", state.preview_track(4).id);
        chip(ui, state, "Tập trung nhẹ", state.preview_track(2).id);
    });
}
fn section(ui: &mut Ui, title: &str, detail: &str) {
    micro(ui, title);
    text(ui, detail, 12.0, SECONDARY, false);
}
fn hero(ui: &mut Ui, state: &mut UiState, artwork_manager: &mut ArtworkManager) {
    let featured = state.preview_track(0);
    let active = state.is_current_track(featured.id);
    let playing = active && state.is_playing();
    Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_min_height(158.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    micro(ui, "TRẠM NHẠC / MIX NỔI BẬT");
                    ui.add_space(8.0);
                    text(ui, "Night Drive", 29.0, TEXT, true);
                    text(
                        ui,
                        "Synth ấm, phố đêm yên tĩnh và hàng chờ đậm chất cassette.",
                        12.0,
                        SECONDARY,
                        false,
                    );
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if hifi_button(
                            ui,
                            if playing {
                                "TẠM DỪNG MIX"
                            } else {
                                "PHÁT MIX"
                            },
                            true,
                            true,
                        )
                        .clicked()
                        {
                            if active {
                                state.toggle_playback();
                            } else {
                                state.play_collection_from_preview(0);
                            }
                        }
                        if hifi_button(ui, "LƯU", false, true).clicked() {
                            state.toggle_like(featured.id);
                        }
                        let _ = icon_button(
                            ui,
                            "mix-more",
                            Icon::More,
                            false,
                            false,
                            "Chưa có thêm tùy chọn trong chế độ xem trước",
                        );
                    });
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    artwork(ui, artwork_manager, &featured, 138.0);
                });
            });
        });
}
fn chip(ui: &mut Ui, state: &mut UiState, label: &str, track_id: TrackId) {
    if hifi_button(ui, label, false, true).clicked() {
        state.play_track_from_preview(track_id);
    }
}
fn track_card(
    ui: &mut Ui,
    state: &mut UiState,
    artwork_manager: &mut ArtworkManager,
    index: usize,
) {
    let track = state.preview_track(index);
    let active = state.is_current_track(track.id);
    let playing = active && state.is_playing();
    Frame::new()
        .fill(if active { WARM } else { SURFACE })
        .stroke(Stroke::new(1.0_f32, if active { ACCENT } else { BORDER }))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_min_height(86.0);
            ui.horizontal(|ui| {
                artwork(ui, artwork_manager, &track, 64.0);
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    micro(ui, if playing { "ĐANG PHÁT" } else { &track.mood });
                    ui.add_sized(
                        [145.0, 20.0],
                        egui::Label::new(
                            RichText::new(&track.title)
                                .size(font_size(14.0))
                                .color(TEXT)
                                .strong(),
                        )
                        .truncate(),
                    );
                    ui.add_sized(
                        [145.0, 17.0],
                        egui::Label::new(
                            RichText::new(&track.artist)
                                .size(font_size(11.0))
                                .color(SECONDARY),
                        )
                        .truncate(),
                    );
                    text(ui, &track.duration_label(), 10.0, SECONDARY, false);
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let liked = state.is_liked(track.id);
                    let like = icon_button(
                        ui,
                        ("card-like", track.id.get()),
                        if liked { Icon::LikeFilled } else { Icon::Like },
                        liked,
                        !state.like_pending(track.id),
                        "Thích bài hát",
                    );
                    if like.clicked() {
                        state.toggle_like(track.id);
                    }
                    let play = icon_button(
                        ui,
                        ("card-play", track.id.get()),
                        if playing { Icon::Pause } else { Icon::Play },
                        playing,
                        true,
                        if playing { "Tạm dừng" } else { "Phát" },
                    );
                    if play.clicked() {
                        if active {
                            state.toggle_playback();
                        } else {
                            state.play_track_from_preview(track.id);
                        }
                    }
                });
            });
        });
    ui.add_space(8.0);
}

fn discover(ui: &mut Ui, state: &mut UiState, artwork_manager: &mut ArtworkManager) {
    if state.data_mode == DataMode::Live {
        live_discover(ui, state, artwork_manager);
        return;
    }
    section(
        ui,
        "KHÁM PHÁ",
        "Duyệt dữ liệu xem trước theo đúng thứ tự của hàng chờ.",
    );
    ui.add_space(9.0);
    ui.horizontal_wrapped(|ui| {
        for (label, track_id) in [
            ("Electronic mới", state.preview_track(1).id),
            ("Indie về đêm", state.preview_track(3).id),
            ("Tập trung", state.preview_track(2).id),
            ("Không gian ambient", state.preview_track(0).id),
        ] {
            chip(ui, state, label, track_id);
        }
    });
    ui.add_space(12.0);
    let ids = state.home_track_ids();
    track_list(ui, state, artwork_manager, &ids);
}

fn live_home(ui: &mut Ui, state: &mut UiState, artwork_manager: &mut ArtworkManager) {
    if state.auth_state() != AuthState::Authorized {
        section(
            ui,
            "TRANG CHỦ CÔNG KHAI",
            "Tìm bài hát công khai mà không cần kết nối tài khoản.",
        );
        ui.add_space(12.0);
        empty(
            ui,
            Icon::Search,
            "Khám phá âm nhạc",
            "Dùng ô phía trên hoặc mở trang Tìm kiếm. Nội dung tài khoản sẽ xuất hiện sau khi kết nối.",
        );
        if hifi_button(ui, "MỞ TÌM KIẾM", true, true).clicked() {
            state.navigate_main(Page::Search);
        }
        return;
    }
    let recommended = state.discover_track_ids();
    let following = state.home_track_ids();
    let likes = state.liked_track_ids();
    let playlists = state.library_playlists();
    let has_content = !recommended.is_empty()
        || !following.is_empty()
        || !likes.is_empty()
        || !playlists.is_empty();

    match state.library_status() {
        LibraryStatus::Idle | LibraryStatus::Loading if !has_content => {
            home_loading(ui);
            return;
        }
        LibraryStatus::Error => {
            home_error_notice(ui, state);
            if !has_content {
                return;
            }
            ui.add_space(14.0);
        }
        LibraryStatus::Empty | LibraryStatus::Loaded if !has_content => {
            empty(
                ui,
                Icon::Discover,
                "Trang chủ đang chờ bạn",
                "Theo dõi nghệ sĩ, thích bài hát hoặc tạo danh sách phát để cá nhân hóa nội dung.",
            );
            return;
        }
        _ => {}
    }

    mobile_track_shelf(
        ui,
        state,
        artwork_manager,
        "Hợp với gu của bạn",
        &recommended,
        Page::Discover,
    );
    mobile_track_shelf(
        ui,
        state,
        artwork_manager,
        "Mới từ người bạn theo dõi",
        &following,
        Page::Following,
    );
    mobile_track_shelf(
        ui,
        state,
        artwork_manager,
        "Bài hát bạn đã thích",
        &likes,
        Page::Likes,
    );
    mobile_playlist_shelf(ui, state, artwork_manager, &playlists);
}

fn home_loading(ui: &mut Ui) {
    Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(Margin::same(18))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().color(ACCENT));
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    text(ui, "Đang tải Trang chủ", 16.0, TEXT, true);
                    text(
                        ui,
                        "Đang tổng hợp đề xuất, bài mới và thư viện của bạn.",
                        11.0,
                        SECONDARY,
                        false,
                    );
                });
            });
        });
}

fn home_error_notice(ui: &mut Ui, state: &mut UiState) {
    Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0_f32, ACCENT))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                paint_icon(
                    ui.painter(),
                    Icon::Discover,
                    Rect::from_min_size(ui.next_widget_position(), vec2(24.0, 24.0)),
                    ACCENT,
                );
                ui.allocate_space(vec2(27.0, 24.0));
                ui.vertical(|ui| {
                    text(ui, "Không thể làm mới Trang chủ", 13.0, TEXT, true);
                    text(
                        ui,
                        "Nội dung đã tải trước đó vẫn được giữ nguyên.",
                        10.0,
                        SECONDARY,
                        false,
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if hifi_button(ui, "THỬ LẠI", false, true).clicked() {
                        state.refresh_user_library();
                    }
                });
            });
        });
}

fn mobile_track_shelf(
    ui: &mut Ui,
    state: &mut UiState,
    artwork_manager: &mut ArtworkManager,
    title: &str,
    track_ids: &[TrackId],
    target: Page,
) {
    if track_ids.is_empty() {
        return;
    }

    if mobile_shelf_header(ui, title) {
        state.navigate_main(target);
    }
    ui.add_space(7.0);
    let card_width = mobile_card_width(ui.available_width());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = HOME_SHELF_GAP;
        for &track_id in track_ids.iter().take(HOME_SHELF_COLUMNS) {
            mobile_track_card(ui, state, artwork_manager, track_id, card_width);
        }
    });
    ui.add_space(17.0);
}

fn mobile_playlist_shelf(
    ui: &mut Ui,
    state: &mut UiState,
    artwork_manager: &mut ArtworkManager,
    playlists: &[Playlist],
) {
    if playlists.is_empty() {
        return;
    }

    if mobile_shelf_header(ui, "Danh sách phát của bạn") {
        state.library_tab = LibraryTab::Playlists;
        state.navigate_main(Page::Library);
    }
    ui.add_space(7.0);
    let card_width = mobile_card_width(ui.available_width());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = HOME_SHELF_GAP;
        for playlist in playlists.iter().take(HOME_SHELF_COLUMNS) {
            mobile_playlist_card(ui, state, artwork_manager, playlist, card_width);
        }
    });
    ui.add_space(17.0);
}

fn mobile_shelf_header(ui: &mut Ui, title: &str) -> bool {
    let mut see_all = false;
    ui.horizontal(|ui| {
        text(ui, title, 18.0, TEXT, true);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            see_all = hifi_button(ui, "XEM TẤT CẢ", false, true).clicked();
        });
    });
    see_all
}

fn mobile_card_width(available_width: f32) -> f32 {
    ((available_width - HOME_SHELF_GAP * (HOME_SHELF_COLUMNS - 1) as f32)
        / HOME_SHELF_COLUMNS as f32)
        .max(108.0)
}

fn mobile_track_card(
    ui: &mut Ui,
    state: &mut UiState,
    artwork_manager: &mut ArtworkManager,
    track_id: TrackId,
    card_width: f32,
) {
    let Some(track) = state.track(track_id) else {
        return;
    };
    let active = state.is_current_track(track.id);
    let playing = active && state.is_playing();
    let art_size = card_width - 10.0;
    let card_height = art_size + 49.0;
    let (rect, response) = ui.allocate_exact_size(vec2(card_width, card_height), Sense::click());
    let fill = if active {
        WARM
    } else if response.hovered() {
        SURFACE
    } else {
        Color32::TRANSPARENT
    };
    let border = if active || response.has_focus() {
        ACCENT
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(RADIUS),
        fill,
        Stroke::new(1.0_f32, border),
        egui::StrokeKind::Inside,
    );

    let artwork_rect = Rect::from_min_size(rect.min + vec2(5.0, 5.0), vec2(art_size, art_size));
    paint_mobile_track_artwork(ui, artwork_manager, &track, artwork_rect);
    if response.hovered() || response.has_focus() || active {
        ui.painter()
            .circle_filled(artwork_rect.center(), 19.0, APP_BG.gamma_multiply(0.88));
        paint_icon(
            ui.painter(),
            if playing { Icon::Pause } else { Icon::Play },
            Rect::from_center_size(artwork_rect.center(), vec2(22.0, 22.0)),
            TEXT,
        );
    }

    let title_rect = Rect::from_min_size(
        pos2(rect.left() + 5.0, artwork_rect.bottom() + 5.0),
        vec2(art_size, 19.0),
    );
    ui.painter().with_clip_rect(title_rect).text(
        title_rect.left_center(),
        Align2::LEFT_CENTER,
        &track.title,
        egui::FontId::proportional(font_size(13.0)),
        if active { ACCENT } else { TEXT },
    );
    let artist_rect = title_rect.translate(vec2(0.0, 18.0));
    ui.painter().with_clip_rect(artist_rect).text(
        artist_rect.left_center(),
        Align2::LEFT_CENTER,
        &track.artist,
        egui::FontId::proportional(font_size(11.0)),
        SECONDARY,
    );

    if response.clicked() {
        if active {
            state.toggle_playback();
        } else {
            state.select_track(track.id);
        }
    }
    response.on_hover_text(format!("{} — {}", track.title, track.artist));
}

fn paint_mobile_track_artwork(
    ui: &mut Ui,
    artwork_manager: &mut ArtworkManager,
    track: &Track,
    rect: Rect,
) {
    let visible = ui.is_rect_visible(rect);
    if let Some(url) = track.artwork_url.as_deref() {
        if let Some(texture) = artwork_manager.texture_for_visible_url(Some(url), visible) {
            ui.put(
                rect,
                egui::Image::from_texture(texture)
                    .fit_to_exact_size(rect.size())
                    .corner_radius(CornerRadius::same(8))
                    .sense(Sense::hover()),
            );
            return;
        }
    }
    if let Some(id) = track.artwork {
        let texture = artwork_manager.fixture_texture_for(id);
        ui.put(
            rect,
            egui::Image::from_texture(texture)
                .fit_to_exact_size(rect.size())
                .corner_radius(CornerRadius::same(8))
                .sense(Sense::hover()),
        );
        return;
    }
    let tint = Color32::from_rgb(track.tint[0], track.tint[1], track.tint[2]);
    paint_mobile_artwork_placeholder(ui, rect, tint);
}

fn mobile_playlist_card(
    ui: &mut Ui,
    state: &mut UiState,
    artwork_manager: &mut ArtworkManager,
    playlist: &Playlist,
    card_width: f32,
) {
    let art_size = card_width - 10.0;
    let card_height = art_size + 49.0;
    let (rect, response) = ui.allocate_exact_size(vec2(card_width, card_height), Sense::click());
    ui.painter().rect(
        rect,
        CornerRadius::same(RADIUS),
        if response.hovered() {
            SURFACE
        } else {
            Color32::TRANSPARENT
        },
        Stroke::new(
            1.0_f32,
            if response.has_focus() {
                ACCENT
            } else {
                Color32::TRANSPARENT
            },
        ),
        egui::StrokeKind::Inside,
    );
    let artwork_rect = Rect::from_min_size(rect.min + vec2(5.0, 5.0), vec2(art_size, art_size));
    let visible = ui.is_rect_visible(artwork_rect);
    let texture = playlist
        .artwork_url
        .as_deref()
        .and_then(|url| artwork_manager.texture_for_visible_url(Some(url), visible));
    if let Some(texture) = texture {
        ui.put(
            artwork_rect,
            egui::Image::from_texture(texture)
                .fit_to_exact_size(artwork_rect.size())
                .corner_radius(CornerRadius::same(8))
                .sense(Sense::hover()),
        );
    } else {
        paint_mobile_artwork_placeholder(ui, artwork_rect, ACCENT);
    }
    if response.hovered() || response.has_focus() {
        ui.painter()
            .circle_filled(artwork_rect.center(), 19.0, APP_BG.gamma_multiply(0.88));
        paint_icon(
            ui.painter(),
            Icon::Playlist,
            Rect::from_center_size(artwork_rect.center(), vec2(22.0, 22.0)),
            TEXT,
        );
    }
    let title_rect = Rect::from_min_size(
        pos2(rect.left() + 5.0, artwork_rect.bottom() + 5.0),
        vec2(art_size, 19.0),
    );
    ui.painter().with_clip_rect(title_rect).text(
        title_rect.left_center(),
        Align2::LEFT_CENTER,
        &playlist.title,
        egui::FontId::proportional(font_size(13.0)),
        TEXT,
    );
    let detail_rect = title_rect.translate(vec2(0.0, 18.0));
    ui.painter().with_clip_rect(detail_rect).text(
        detail_rect.left_center(),
        Align2::LEFT_CENTER,
        format!("{} bài hát", playlist.track_count),
        egui::FontId::proportional(font_size(11.0)),
        SECONDARY,
    );
    if response.clicked() {
        state.open_playlist(playlist.id);
    }
    response.on_hover_text(&playlist.title);
}

fn paint_mobile_artwork_placeholder(ui: &Ui, rect: Rect, tint: Color32) {
    ui.painter().rect(
        rect,
        CornerRadius::same(8),
        tint.gamma_multiply(0.22),
        Stroke::new(1.0_f32, tint.gamma_multiply(0.72)),
        egui::StrokeKind::Inside,
    );
    paint_icon(
        ui.painter(),
        Icon::Brand,
        Rect::from_center_size(rect.center(), vec2(32.0, 32.0)),
        tint,
    );
}

fn live_discover(ui: &mut Ui, state: &mut UiState, artwork_manager: &mut ArtworkManager) {
    section(
        ui,
        "KHÁM PHÁ",
        "Bài hát liên quan dựa trên nội dung đã thích và Trang chủ.",
    );
    ui.add_space(8.0);
    if hifi_button(
        ui,
        "LÀM MỚI",
        false,
        state.auth_state() == AuthState::Authorized,
    )
    .clicked()
    {
        state.refresh_user_library();
    }
    ui.add_space(8.0);
    let track_ids = state.discover_track_ids();
    live_track_collection(
        ui,
        state,
        artwork_manager,
        track_ids,
        "Đang tải bài hát liên quan",
        "Cần ít nhất một bài đã thích hoặc một bài từ Trang chủ để tạo đề xuất.",
    );
}

fn live_track_collection(
    ui: &mut Ui,
    state: &mut UiState,
    artwork_manager: &mut ArtworkManager,
    track_ids: Vec<TrackId>,
    loading: &str,
    empty_detail: &str,
) {
    match state.library_status() {
        LibraryStatus::Idle | LibraryStatus::Loading => empty(
            ui,
            Icon::Discover,
            loading,
            "Brickwave đang đọc dữ liệu tài khoản từ SoundCloud.",
        ),
        LibraryStatus::Error => {
            empty(
                ui,
                Icon::Discover,
                "Không thể tải dữ liệu SoundCloud",
                "Dữ liệu trước đó vẫn được giữ nguyên. Chọn Làm mới để thử lại.",
            );
        }
        LibraryStatus::Empty | LibraryStatus::Loaded if track_ids.is_empty() => {
            empty(ui, Icon::Discover, "Chưa có bài hát", empty_detail)
        }
        LibraryStatus::Empty | LibraryStatus::Loaded => {
            track_list(ui, state, artwork_manager, &track_ids)
        }
    }
}
fn search(ui: &mut Ui, state: &mut UiState, artwork_manager: &mut ArtworkManager) {
    if state.data_mode == DataMode::Live {
        match state.search_status() {
            SearchStatus::Idle => {
                section(
                    ui,
                    "TÌM KIẾM TRÊN BRICKWAVE",
                    "Nhập tên bài hát, nghệ sĩ hoặc thể loại rồi nhấn Enter.",
                );
                ui.add_space(10.0);
                empty(
                    ui,
                    Icon::Search,
                    "Sẵn sàng tìm kiếm",
                    "Tìm bài hát, nghệ sĩ và danh sách phát trên SoundCloud.",
                );
            }
            SearchStatus::Loading => {
                section(
                    ui,
                    "TÌM KIẾM TRÊN BRICKWAVE",
                    "Đang yêu cầu dữ liệu công khai.",
                );
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().color(ACCENT));
                    ui.add_space(8.0);
                    text(ui, "Đang tìm kiếm...", 14.0, TEXT, true);
                });
            }
            SearchStatus::Results { .. } => {
                let tracks = state.search_results();
                let playlists = state.search_playlists();
                ui.columns(2, |columns| {
                    let (track_column, playlist_column) = columns.split_at_mut(1);
                    let track_column = &mut track_column[0];
                    let playlist_column = &mut playlist_column[0];

                    section(
                        track_column,
                        "BÀI HÁT",
                        "Chọn một bài hát để tạo hàng chờ phát.",
                    );
                    track_column.add_space(7.0);
                    if tracks.is_empty() {
                        search_column_empty(
                            track_column,
                            Icon::Search,
                            "Trang này chưa có bài hát",
                        );
                    } else {
                        for (index, track_id) in tracks.iter().copied().enumerate() {
                            search_track_card(
                                track_column,
                                state,
                                artwork_manager,
                                track_id,
                                index,
                                &tracks,
                            );
                            track_column.add_space(7.0);
                        }
                    }

                    section(
                        playlist_column,
                        "DANH SÁCH PHÁT",
                        "Mở danh sách phát để xem các bài theo thứ tự.",
                    );
                    playlist_column.add_space(7.0);
                    if playlists.is_empty() {
                        search_column_empty(
                            playlist_column,
                            Icon::Playlist,
                            "Trang này chưa có danh sách phát",
                        );
                    } else {
                        for playlist in &playlists {
                            search_playlist_card(playlist_column, state, artwork_manager, playlist);
                            playlist_column.add_space(7.0);
                        }
                    }
                });

                ui.add_space(10.0);
                if state.search_loading_more() {
                    ui.horizontal_centered(|ui| {
                        ui.add(egui::Spinner::new().color(ACCENT));
                        text(ui, "Đang tải thêm...", 12.0, SECONDARY, false);
                    });
                } else if state.search_has_more() {
                    ui.vertical_centered(|ui| {
                        text(ui, "Cuộn xuống để tải thêm", 11.0, SECONDARY, false);
                    });
                }
            }
            SearchStatus::Empty => {
                section(
                    ui,
                    "KHÔNG CÓ KẾT QUẢ",
                    "Không có nội dung phù hợp với từ khóa này.",
                );
                ui.add_space(10.0);
                empty(
                    ui,
                    Icon::Search,
                    "Không tìm thấy kết quả",
                    "Hãy thử tên bài hát, nghệ sĩ hoặc thể loại khác.",
                );
            }
            SearchStatus::Error => {
                section(
                    ui,
                    "TÌM KIẾM TRÊN BRICKWAVE",
                    "Máy chủ không trả về dữ liệu có thể sử dụng.",
                );
                ui.add_space(10.0);
                empty(
                    ui,
                    Icon::Search,
                    "Không thể tìm kiếm",
                    "Kiểm tra kết nối rồi thử lại. Dữ liệu hiện có vẫn được giữ nguyên.",
                );
            }
        }
        return;
    }

    let results = state.search_results();
    if state.submitted_query.is_empty() {
        section(
            ui,
            "TÌM TRONG DỮ LIỆU XEM TRƯỚC",
            "Nhập nghệ sĩ, tên bài hát hoặc thể loại vào ô phía trên.",
        );
    } else {
        section(
            ui,
            "TÌM KIẾM CỤC BỘ",
            &format!("Có {} kết quả cục bộ.", results.len()),
        );
    }
    ui.add_space(8.0);
    if results.is_empty() {
        empty(
            ui,
            Icon::Search,
            "Không có bài hát phù hợp",
            "Hãy thử tên bài hát, nghệ sĩ hoặc thể loại trong dữ liệu xem trước.",
        );
    } else {
        track_list(ui, state, artwork_manager, &results);
    }
}

fn search_column_empty(ui: &mut Ui, icon: Icon, label: &str) {
    Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0_f32, SUBTLE))
        .corner_radius(CornerRadius::same(SMALL_RADIUS))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
                paint_icon(ui.painter(), icon, rect, SECONDARY);
                text(ui, label, 12.0, SECONDARY, false);
            });
        });
}

fn search_track_card(
    ui: &mut Ui,
    state: &mut UiState,
    artwork_manager: &mut ArtworkManager,
    track_id: TrackId,
    index: usize,
    context: &[TrackId],
) {
    let Some(track) = state.track(track_id) else {
        return;
    };
    let active = state.is_current_track(track_id);
    let card = Frame::new()
        .fill(if active { WARM } else { SURFACE })
        .stroke(Stroke::new(1.0_f32, if active { ACCENT } else { SUBTLE }))
        .corner_radius(CornerRadius::same(SMALL_RADIUS))
        .inner_margin(Margin::symmetric(9, 8))
        .show(ui, |ui| {
            ui.set_min_height(52.0);
            ui.horizontal(|ui| {
                artwork(ui, artwork_manager, &track, 48.0);
                ui.add_space(7.0);
                let copy_width = (ui.available_width() - 62.0).max(96.0);
                ui.allocate_ui_with_layout(
                    vec2(copy_width, 48.0),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.add_sized(
                            [copy_width, 23.0],
                            egui::Label::new(
                                RichText::new(&track.title)
                                    .size(font_size(13.0))
                                    .color(TEXT)
                                    .strong(),
                            )
                            .truncate(),
                        );
                        ui.add_sized(
                            [copy_width, 19.0],
                            egui::Label::new(
                                RichText::new(&track.artist)
                                    .size(font_size(11.0))
                                    .color(SECONDARY),
                            )
                            .truncate(),
                        );
                    },
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let (icon_rect, _) = ui.allocate_exact_size(vec2(26.0, 34.0), Sense::hover());
                    paint_icon(
                        ui.painter(),
                        if active && state.is_playing() {
                            Icon::Pause
                        } else {
                            Icon::Play
                        },
                        icon_rect.shrink(5.0),
                        if active { ACCENT } else { SECONDARY },
                    );
                    text(ui, &track.duration_label(), 10.0, SECONDARY, false);
                });
            });
        });
    let response = ui.interact(
        card.response.rect,
        ui.id().with(("search-track", track_id.get())),
        Sense::click(),
    );
    if response.clicked() {
        state.select_track_from_context(context.to_vec(), index);
    }
    response.on_hover_text(format!("Chọn {}", track.title));
}

fn search_playlist_card(
    ui: &mut Ui,
    state: &mut UiState,
    artwork_manager: &mut ArtworkManager,
    playlist: &Playlist,
) {
    let card = Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0_f32, SUBTLE))
        .corner_radius(CornerRadius::same(SMALL_RADIUS))
        .inner_margin(Margin::symmetric(9, 8))
        .show(ui, |ui| {
            ui.set_min_height(52.0);
            ui.horizontal(|ui| {
                if let Some(url) = playlist.artwork_url.as_deref() {
                    let slot = Rect::from_min_size(ui.next_widget_position(), vec2(48.0, 48.0));
                    let visible = ui.is_rect_visible(slot);
                    if let Some(texture) =
                        artwork_manager.texture_for_visible_url(Some(url), visible)
                    {
                        ui.add(
                            egui::Image::from_texture(texture)
                                .fit_to_exact_size(vec2(48.0, 48.0))
                                .corner_radius(CornerRadius::same(7)),
                        );
                    } else {
                        empty_artwork(ui, 48.0);
                    }
                } else {
                    empty_artwork(ui, 48.0);
                }
                ui.add_space(7.0);
                let copy_width = (ui.available_width() - 38.0).max(96.0);
                ui.allocate_ui_with_layout(
                    vec2(copy_width, 48.0),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.add_sized(
                            [copy_width, 23.0],
                            egui::Label::new(
                                RichText::new(&playlist.title)
                                    .size(font_size(13.0))
                                    .color(TEXT)
                                    .strong(),
                            )
                            .truncate(),
                        );
                        text(
                            ui,
                            &format!("{} bài hát", playlist.track_count),
                            11.0,
                            SECONDARY,
                            false,
                        );
                    },
                );
                let (icon_rect, _) = ui.allocate_exact_size(vec2(26.0, 34.0), Sense::hover());
                paint_icon(
                    ui.painter(),
                    Icon::ChevronRight,
                    icon_rect.shrink(5.0),
                    SECONDARY,
                );
            });
        });
    let response = ui.interact(
        card.response.rect,
        ui.id().with(("search-playlist", playlist.id.get())),
        Sense::click(),
    );
    if response.clicked() {
        state.open_playlist(playlist.id);
    }
    response.on_hover_text(format!("Mở {}", playlist.title));
}

fn library(ui: &mut Ui, state: &mut UiState, artwork_manager: &mut ArtworkManager) {
    if state.data_mode == DataMode::Live {
        live_library(ui, state, artwork_manager);
        return;
    }
    section(
        ui,
        "THƯ VIỆN CỦA BẠN",
        "Các bài hát và bộ sưu tập cục bộ trong chế độ xem trước.",
    );
    ui.add_space(9.0);
    ui.horizontal(|ui| {
        if hifi_button(ui, "BÀI HÁT", state.library_tab == LibraryTab::Tracks, true).clicked() {
            state.library_tab = LibraryTab::Tracks;
        }
        if hifi_button(
            ui,
            "DANH SÁCH PHÁT",
            state.library_tab == LibraryTab::Playlists,
            true,
        )
        .clicked()
        {
            state.library_tab = LibraryTab::Playlists;
        }
    });
    ui.add_space(10.0);
    match state.library_tab {
        LibraryTab::Tracks => {
            let ids = state.home_track_ids();
            track_list(ui, state, artwork_manager, &ids);
        }
        LibraryTab::Playlists => playlist_panel(
            ui,
            state,
            artwork_manager,
            "Night Drive",
            "10 bài hát / Bộ sưu tập cục bộ",
            0,
        ),
    }
}
fn likes(ui: &mut Ui, state: &mut UiState, artwork_manager: &mut ArtworkManager) {
    if state.data_mode == DataMode::Live {
        live_likes(ui, state, artwork_manager);
        return;
    }
    section(
        ui,
        "BÀI HÁT ĐÃ THÍCH",
        "Danh sách cục bộ được quản lý bằng nút hình trái tim.",
    );
    ui.add_space(8.0);
    let ids = state.liked_track_ids();
    if ids.is_empty() {
        empty(
            ui,
            Icon::Like,
            "Chưa có bài hát đã thích",
            "Dùng nút trái tim trên thẻ, hàng bài hát hoặc thanh trình phát.",
        );
    } else {
        track_list(ui, state, artwork_manager, &ids);
    }
}

fn live_library(ui: &mut Ui, state: &mut UiState, artwork_manager: &mut ArtworkManager) {
    section(
        ui,
        "THƯ VIỆN BRICKWAVE",
        "Bài hát và danh sách phát từ tài khoản SoundCloud đã kết nối.",
    );
    ui.add_space(9.0);
    // The two library tabs and Refresh are deliberately kept on one compact
    // row. Creation belongs to the Playlists view below this navigation row.
    ui.horizontal(|ui| {
        if hifi_button(
            ui,
            "BÀI HÁT ĐÃ THÍCH",
            state.library_tab == LibraryTab::Tracks,
            true,
        )
        .clicked()
        {
            state.library_tab = LibraryTab::Tracks;
        }
        if hifi_button(
            ui,
            "DANH SÁCH PHÁT",
            state.library_tab == LibraryTab::Playlists,
            true,
        )
        .clicked()
        {
            state.library_tab = LibraryTab::Playlists;
        }
        if state.auth_state() == AuthState::Authorized
            && hifi_button(ui, "LÀM MỚI", false, true).clicked()
        {
            state.refresh_user_library();
        }
    });
    ui.add_space(9.0);
    if state.auth_state() != AuthState::Authorized {
        empty(
            ui,
            Icon::User,
            "Hãy kết nối tài khoản",
            "Mở Tài khoản và đăng nhập bằng mã QR để tải bài hát đã thích và danh sách phát.",
        );
        if hifi_button(ui, "MỞ TÀI KHOẢN", true, true).clicked() {
            state.navigate_main(Page::Profile);
        }
        return;
    }
    if state.library_tab == LibraryTab::Playlists {
        if hifi_button(ui, "DANH SÁCH MỚI", true, !state.playlist_create_pending()).clicked() {
            state.open_create_playlist();
        }
        ui.add_space(9.0);
    }
    match state.library_status() {
        LibraryStatus::Idle | LibraryStatus::Loading => empty(
            ui,
            Icon::Library,
            "Đang tải thư viện",
            "Brickwave đang đọc dữ liệu tài khoản từ SoundCloud.",
        ),
        LibraryStatus::Error => empty(
            ui,
            Icon::Library,
            "Không thể tải thư viện",
            "Tài khoản vẫn được kết nối. Chọn Làm mới để thử lại.",
        ),
        LibraryStatus::Empty | LibraryStatus::Loaded => match state.library_tab {
            LibraryTab::Tracks => {
                let ids = state.liked_track_ids();
                if ids.is_empty() {
                    empty(
                        ui,
                        Icon::Like,
                        "Chưa có bài hát đã thích",
                        "Tài khoản này chưa trả về bài hát đã thích nào.",
                    );
                } else {
                    track_list(ui, state, artwork_manager, &ids);
                }
            }
            LibraryTab::Playlists => {
                let playlists = state.library_playlists();
                if playlists.is_empty() {
                    empty(
                        ui,
                        Icon::Playlist,
                        "Chưa có danh sách phát",
                        "Tài khoản này chưa có danh sách phát nào.",
                    );
                } else {
                    for playlist in playlists {
                        live_playlist_card(ui, state, artwork_manager, &playlist);
                        ui.add_space(7.0);
                    }
                }
            }
        },
    }
}

fn live_likes(ui: &mut Ui, state: &mut UiState, artwork_manager: &mut ArtworkManager) {
    section(
        ui,
        "BÀI HÁT ĐÃ THÍCH",
        "Các bài hát đã thích trong tài khoản SoundCloud đang kết nối.",
    );
    ui.add_space(8.0);
    if state.auth_state() != AuthState::Authorized {
        empty(
            ui,
            Icon::User,
            "Hãy kết nối tài khoản",
            "Mở Tài khoản và đăng nhập bằng mã QR để tải bài hát đã thích.",
        );
        return;
    }
    match state.library_status() {
        LibraryStatus::Idle | LibraryStatus::Loading => empty(
            ui,
            Icon::Like,
            "Đang tải bài hát đã thích",
            "Đang đọc bài hát đã thích từ SoundCloud.",
        ),
        LibraryStatus::Error => {
            empty(
                ui,
                Icon::Like,
                "Không thể tải bài hát đã thích",
                "Tài khoản vẫn được kết nối. Chọn Làm mới để thử lại.",
            );
            if hifi_button(ui, "LÀM MỚI", true, true).clicked() {
                state.refresh_user_library();
            }
        }
        LibraryStatus::Empty | LibraryStatus::Loaded => {
            let ids = state.liked_track_ids();
            if ids.is_empty() {
                empty(
                    ui,
                    Icon::Like,
                    "Chưa có bài hát đã thích",
                    "Tài khoản này chưa trả về bài hát đã thích nào.",
                );
            } else {
                track_list(ui, state, artwork_manager, &ids);
            }
        }
    }
}

fn live_playlist_card(
    ui: &mut Ui,
    state: &mut UiState,
    artwork_manager: &mut ArtworkManager,
    playlist: &Playlist,
) {
    const DELETE_ACTION_WIDTH: f32 = 92.0;
    let card = Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(url) = playlist.artwork_url.as_deref() {
                    let slot = Rect::from_min_size(ui.next_widget_position(), vec2(64.0, 64.0));
                    let visible = ui.is_rect_visible(slot);
                    if let Some(texture) =
                        artwork_manager.texture_for_visible_url(Some(url), visible)
                    {
                        ui.add(
                            egui::Image::from_texture(texture)
                                .fit_to_exact_size(vec2(64.0, 64.0))
                                .corner_radius(CornerRadius::same(8)),
                        );
                    } else {
                        empty_artwork(ui, 64.0);
                    }
                } else {
                    empty_artwork(ui, 64.0);
                }
                ui.add_space(8.0);
                let reserved = if playlist.editable {
                    DELETE_ACTION_WIDTH + 8.0
                } else {
                    0.0
                };
                let text_width = (ui.available_width() - reserved).max(120.0);
                ui.allocate_ui_with_layout(
                    vec2(text_width, 64.0),
                    Layout::top_down(Align::Min),
                    |ui| {
                        micro(ui, "DANH SÁCH PHÁT BRICKWAVE");
                        ui.add_sized(
                            [text_width, 24.0],
                            egui::Label::new(
                                RichText::new(&playlist.title)
                                    .size(font_size(17.0))
                                    .color(TEXT)
                                    .strong(),
                            )
                            .truncate(),
                        );
                        let detail = playlist
                            .description
                            .as_deref()
                            .filter(|value| !value.trim().is_empty())
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("{} bài hát", playlist.track_count));
                        ui.add_sized(
                            [text_width, 20.0],
                            egui::Label::new(
                                RichText::new(detail).size(font_size(11.0)).color(SECONDARY),
                            )
                            .truncate(),
                        );
                    },
                );
            });
        });
    let response = ui.interact(
        card.response.rect,
        ui.id().with(("live-playlist", playlist.id.get())),
        Sense::click(),
    );
    let delete_clicked = if playlist.editable {
        let size = vec2(DELETE_ACTION_WIDTH - 8.0, 34.0);
        let rect = Rect::from_min_size(
            egui::pos2(
                card.response.rect.right() - size.x - 12.0,
                card.response.rect.top() + 12.0,
            ),
            size,
        );
        hifi_button_at(
            ui,
            ("delete-playlist-card", playlist.id.get()),
            rect,
            "XÓA",
            false,
            !state.playlist_delete_pending(),
        )
        .on_hover_text(format!("Xóa {}", playlist.title))
        .clicked()
    } else {
        false
    };
    if delete_clicked {
        state.request_delete_playlist(playlist.id);
    } else if response.clicked() {
        state.open_playlist(playlist.id);
    }
    response.on_hover_text(format!("Mở {}", playlist.title));
}
fn playlist(ui: &mut Ui, state: &mut UiState, artwork_manager: &mut ArtworkManager) {
    if state.data_mode == DataMode::Live {
        live_playlist(ui, state, artwork_manager);
        return;
    }
    playlist_panel(
        ui,
        state,
        artwork_manager,
        "Night Drive",
        "Mix nổi bật gọn nhẹ cho chế độ xem trước cục bộ.",
        0,
    );
    ui.add_space(12.0);
    let ids = state.home_track_ids();
    track_list(ui, state, artwork_manager, &ids);
}

fn live_playlist(ui: &mut Ui, state: &mut UiState, artwork_manager: &mut ArtworkManager) {
    let Some(playlist) = state.current_playlist() else {
        empty(
            ui,
            Icon::Playlist,
            "Chưa chọn danh sách phát",
            "Mở một danh sách phát từ Trang chủ hoặc Thư viện.",
        );
        return;
    };
    Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(Margin::same(13))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(url) = playlist.artwork_url.as_deref() {
                    let slot = Rect::from_min_size(ui.next_widget_position(), vec2(82.0, 82.0));
                    let visible = ui.is_rect_visible(slot);
                    if let Some(texture) =
                        artwork_manager.texture_for_visible_url(Some(url), visible)
                    {
                        ui.add(
                            egui::Image::from_texture(texture)
                                .fit_to_exact_size(vec2(82.0, 82.0))
                                .corner_radius(CornerRadius::same(9)),
                        );
                    } else {
                        empty_artwork(ui, 82.0);
                    }
                } else {
                    empty_artwork(ui, 82.0);
                }
                ui.add_space(9.0);
                ui.vertical(|ui| {
                    micro(ui, "DANH SÁCH PHÁT BRICKWAVE");
                    text(ui, &playlist.title, 23.0, TEXT, true);
                    let detail = playlist
                        .description
                        .as_deref()
                        .filter(|value| !value.trim().is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("{} bài hát", playlist.track_count));
                    ui.add_sized(
                        [ui.available_width(), 20.0],
                        egui::Label::new(
                            RichText::new(detail).size(font_size(12.0)).color(SECONDARY),
                        )
                        .truncate(),
                    );
                });
            });
        });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if state.playlist_status() == PlaylistStatus::Loaded
            && !playlist.track_ids.is_empty()
            && hifi_button(ui, "PHÁT TỪ ĐẦU", true, true).clicked()
        {
            state.select_track_from_context(playlist.track_ids.clone(), 0);
        }
        if playlist.editable {
            let remove_mode = state.playlist_track_remove_mode(playlist.id);
            if hifi_button(
                ui,
                if remove_mode {
                    "XONG"
                } else {
                    "XÓA BÀI HÁT"
                },
                remove_mode,
                !state.playlist_track_remove_pending(),
            )
            .clicked()
            {
                state.toggle_playlist_track_remove_mode(playlist.id);
            }
            if hifi_button(ui, "XÓA DANH SÁCH", false, !state.playlist_delete_pending()).clicked()
            {
                state.request_delete_playlist(playlist.id);
            }
        } else {
            hifi_button(ui, "CHỈ ĐỌC", false, false)
                .on_hover_text("Chỉ có thể sửa danh sách phát thuộc tài khoản này");
        }
    });
    if state.playlist_track_remove_mode(playlist.id) {
        ui.add_space(7.0);
        Frame::new()
            .fill(WARM)
            .stroke(Stroke::new(1.0, ACCENT))
            .corner_radius(CornerRadius::same(SMALL_RADIUS))
            .inner_margin(Margin::symmetric(11, 8))
            .show(ui, |ui| {
                text(
                    ui,
                    "CHẾ ĐỘ XÓA: chọn XÓA bên cạnh bài hát cần loại khỏi danh sách",
                    11.0,
                    ACCENT,
                    true,
                );
            });
    }
    ui.add_space(12.0);
    match state.playlist_status() {
        PlaylistStatus::Idle | PlaylistStatus::Loading => empty(
            ui,
            Icon::Playlist,
            "Đang tải danh sách phát",
            "Đang đọc danh sách bài hát theo thứ tự từ SoundCloud.",
        ),
        PlaylistStatus::Error => {
            empty(
                ui,
                Icon::Playlist,
                "Không thể tải danh sách phát",
                "Tài khoản và thư viện vẫn được giữ nguyên. Hãy thử tải lại.",
            );
            if hifi_button(ui, "THỬ LẠI", true, true).clicked() {
                state.retry_current_playlist();
            }
        }
        PlaylistStatus::Empty => empty(
            ui,
            Icon::Playlist,
            "Danh sách phát đang trống",
            "SoundCloud không trả về bài hát nào trong danh sách này.",
        ),
        PlaylistStatus::Loaded => playlist_track_list(
            ui,
            state,
            artwork_manager,
            playlist.id,
            playlist.editable,
            &playlist.track_ids,
        ),
    }
}
fn playlist_panel(
    ui: &mut Ui,
    state: &mut UiState,
    artwork_manager: &mut ArtworkManager,
    title: &str,
    detail: &str,
    start_at: usize,
) {
    let cover = state.preview_track(start_at);
    let active = state.is_current_track(cover.id);
    let playing = active && state.is_playing();
    Frame::new()
        .fill(if active { WARM } else { SURFACE })
        .stroke(Stroke::new(1.0_f32, if active { ACCENT } else { BORDER }))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(Margin::same(13))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                artwork(ui, artwork_manager, &cover, 82.0);
                ui.vertical(|ui| {
                    micro(
                        ui,
                        if playing {
                            "BỘ SƯU TẬP ĐANG PHÁT"
                        } else {
                            "DANH SÁCH ĐÃ LƯU"
                        },
                    );
                    text(ui, title, 23.0, TEXT, true);
                    text(ui, detail, 12.0, SECONDARY, false);
                    ui.add_space(7.0);
                    let button = playlist_play_button(ui, playing);
                    if button.clicked() {
                        if active {
                            state.toggle_playback();
                        } else {
                            state.play_collection_from_preview(start_at);
                        }
                    }
                });
            });
        });
}
fn playlist_play_button(ui: &mut Ui, playing: bool) -> Response {
    let label = if playing {
        "TẠM DỪNG"
    } else {
        "PHÁT DANH SÁCH"
    };
    let (rect, response) = ui.allocate_exact_size(vec2(166.0, 36.0), Sense::click());
    let fill = if response.hovered() {
        ACCENT_HOVER
    } else {
        ACCENT
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(SMALL_RADIUS),
        fill,
        Stroke::new(1.0_f32, ACCENT),
        egui::StrokeKind::Inside,
    );
    paint_icon(
        ui.painter(),
        if playing { Icon::Pause } else { Icon::Play },
        Rect::from_min_size(
            rect.min + vec2(10.0, 8.0),
            vec2(ICON_SIZE_SMALL, ICON_SIZE_SMALL),
        ),
        APP_BG,
    );
    ui.painter().text(
        rect.center() + vec2(7.0, 0.0),
        Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(font_size(11.0)),
        APP_BG,
    );
    response
}
fn profile(ui: &mut Ui, state: &mut UiState, artwork_manager: &mut ArtworkManager) {
    if state.data_mode == DataMode::Preview {
        section(
            ui,
            "HỒ SƠ",
            "Tài khoản xem trước chỉ dùng cục bộ. Chế độ trực tuyến dùng màn hình QR dành cho TrimUI.",
        );
        ui.add_space(9.0);
        Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0_f32, BORDER))
            .corner_radius(CornerRadius::same(RADIUS))
            .inner_margin(Margin::same(15))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    avatar(ui, 66.0);
                    ui.vertical(|ui| {
                        text(ui, "Tài khoản xem trước", 22.0, TEXT, true);
                        text(
                            ui,
                            "Dữ liệu cục bộ / Chế độ thử nghiệm",
                            12.0,
                            SECONDARY,
                            false,
                        );
                    });
                });
            });
        return;
    }

    if state.auth_state() != AuthState::Authorized {
        let restoring = state.auth_state() == AuthState::AuthorizationPending;
        let reconnect = matches!(
            state.auth_state(),
            AuthState::TokenExpired | AuthState::RefreshRequired
        );
        section(
            ui,
            "TÀI KHOẢN BRICKWAVE",
            if reconnect {
                "Phiên đã lưu cần được cấp quyền lại."
            } else if restoring {
                "Đang xác minh phiên tài khoản được mã hóa trên thiết bị."
            } else {
                "Kết nối tài khoản để tải bài hát đã thích, danh sách phát và hồ sơ SoundCloud."
            },
        );
        ui.add_space(9.0);
        Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0_f32, BORDER))
            .corner_radius(CornerRadius::same(RADIUS))
            .inner_margin(Margin::same(16))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    avatar(ui, 66.0);
                    ui.vertical(|ui| {
                        text(
                            ui,
                            if reconnect {
                                "Kết nối lại SoundCloud"
                            } else if restoring {
                                "Đang khôi phục tài khoản SoundCloud"
                            } else {
                                "Kết nối tài khoản SoundCloud"
                            },
                            20.0,
                            TEXT,
                            true,
                        );
                        text(
                            ui,
                            "Màn hình đăng nhập QR sẽ được mở riêng.",
                            12.0,
                            SECONDARY,
                            false,
                        );
                        if let Some(error) = state.auth_error() {
                            text(ui, error, 11.0, Color32::from_rgb(255, 150, 105), false);
                        }
                        ui.add_space(10.0);
                        if restoring {
                            ui.add(egui::Spinner::new().color(ACCENT));
                        } else if hifi_button(
                            ui,
                            if reconnect {
                                "KẾT NỐI LẠI"
                            } else {
                                "KẾT NỐI SOUNDCLOUD"
                            },
                            true,
                            true,
                        )
                        .clicked()
                        {
                            state.open_login();
                        }
                    });
                });
            });
        return;
    }

    section(
        ui,
        "TÀI KHOẢN BRICKWAVE",
        "Phiên tài khoản đã lưu đang hoạt động trên thiết bị này.",
    );
    ui.add_space(9.0);
    Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| authorized_profile(ui, state, artwork_manager));
}

fn authorized_profile(ui: &mut Ui, state: &mut UiState, artwork_manager: &mut ArtworkManager) {
    let Some(profile) = state.current_profile().cloned() else {
        text(ui, "Đang tải hồ sơ SoundCloud…", 18.0, TEXT, true);
        return;
    };
    ui.horizontal(|ui| {
        if let Some(url) = profile.avatar_url.as_deref() {
            if let Some(texture) = artwork_manager.texture_for_visible_url(Some(url), true) {
                ui.add(
                    egui::Image::from_texture(texture)
                        .fit_to_exact_size(vec2(72.0, 72.0))
                        .corner_radius(CornerRadius::same(36)),
                );
            } else {
                avatar(ui, 72.0);
            }
        } else {
            avatar(ui, 72.0);
        }
        ui.vertical(|ui| {
            text(
                ui,
                profile.display_name.as_deref().unwrap_or(&profile.username),
                22.0,
                TEXT,
                true,
            );
            text(
                ui,
                &format!("@{}", profile.username),
                12.0,
                SECONDARY,
                false,
            );
            text(ui, "Đã kết nối tài khoản SoundCloud", 11.0, ACCENT, true);
        });
    });
    ui.add_space(12.0);
    ui.separator();
    ui.add_space(10.0);
    if hifi_button(ui, "ĐĂNG XUẤT", false, true).clicked() {
        state.logout();
    }
}
fn stations(ui: &mut Ui, state: &mut UiState, artwork_manager: &mut ArtworkManager) {
    section(
        ui,
        "TRẠM NHẠC",
        "Các mix đã lưu được hiển thị dưới dạng bộ sưu tập cục bộ gọn nhẹ.",
    );
    ui.add_space(9.0);
    playlist_panel(
        ui,
        state,
        artwork_manager,
        "Night Drive",
        "Trạm đã lưu / Electronic ấm áp",
        0,
    );
    ui.add_space(9.0);
    playlist_panel(
        ui,
        state,
        artwork_manager,
        "Soft Focus",
        "Trạm đã lưu / Tập trung nhẹ",
        9,
    );
}
fn following(ui: &mut Ui, state: &mut UiState, artwork_manager: &mut ArtworkManager) {
    if state.data_mode == DataMode::Live {
        section(
            ui,
            "ĐANG THEO DÕI",
            "Bài hát và lượt đăng lại mới nhất từ những người bạn theo dõi.",
        );
        ui.add_space(9.0);
        let ids = state.home_track_ids();
        live_track_collection(
            ui,
            state,
            artwork_manager,
            ids,
            "Đang tải nội dung theo dõi",
            "Chưa có bài hát mới từ những người bạn theo dõi.",
        );
        return;
    }
    empty(
        ui,
        Icon::User,
        "Chưa kết nối nội dung theo dõi",
        "Chế độ xem trước chỉ mô phỏng bố cục khi chưa có tài khoản mạng.",
    );
}
fn settings(ui: &mut Ui, state: &mut UiState) {
    section(
        ui,
        "CÀI ĐẶT",
        "Điều khiển phát nhạc, hiển thị và tiết kiệm điện.",
    );
    ui.add_space(9.0);
    Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(Margin::same(13))
        .show(ui, |ui| {
            micro(ui, "GIAO DIỆN");
            ui.add_space(5.0);
            ui.horizontal(|ui| {
                text(ui, "Bố cục", 14.0, TEXT, true);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if hifi_button(ui, "TỐI GIẢN", state.minimal_interface, true).clicked() {
                        state.set_minimal_interface(true);
                    }
                    if hifi_button(ui, "ĐẦY ĐỦ", !state.minimal_interface, true).clicked() {
                        state.set_minimal_interface(false);
                    }
                });
            });
            text(
                ui,
                "Giao diện tối giản dùng nút lớn và điều hướng trực tiếp bằng D-pad.",
                11.0,
                SECONDARY,
                false,
            );
            ui.add_space(14.0);
            ui.separator();
            ui.add_space(12.0);
            micro(ui, "PHÁT NHẠC");
            ui.horizontal(|ui| {
                text(ui, "Âm lượng mặc định", 13.0, TEXT, true);
                let volume = state.player_state().volume;
                let mut requested_volume = volume;
                slider(ui, &mut requested_volume, 180.0, "Âm lượng mặc định");
                if (requested_volume - volume).abs() > f32::EPSILON {
                    state.set_volume(requested_volume);
                }
            });
            ui.add_space(14.0);
            ui.separator();
            ui.add_space(12.0);
            micro(ui, "HẸN GIỜ DỪNG PHÁT");
            ui.add_space(5.0);
            text(
                ui,
                "Tạm dừng nhạc sau thời gian đã chọn và giữ nguyên bài hiện tại để phát tiếp.",
                11.0,
                SECONDARY,
                false,
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                text(ui, "Giờ", 12.0, TEXT, true);
                ui.add_sized(
                    [58.0, 38.0],
                    egui::TextEdit::singleline(state.stop_timer_hours_mut())
                        .id(stop_timer_hours_field_id())
                        .horizontal_align(Align::Center)
                        .vertical_align(Align::Center)
                        .char_limit(2)
                        .hint_text("00"),
                );
                text(ui, "Phút", 12.0, TEXT, true);
                ui.add_sized(
                    [58.0, 38.0],
                    egui::TextEdit::singleline(state.stop_timer_minutes_mut())
                        .id(stop_timer_minutes_field_id())
                        .horizontal_align(Align::Center)
                        .vertical_align(Align::Center)
                        .char_limit(2)
                        .hint_text("30"),
                );
                if hifi_button(ui, "ĐẶT GIỜ", true, true).clicked() {
                    state.set_stop_timer();
                }
                let timer_active = state.stop_timer_remaining().is_some();
                if hifi_button(ui, "HỦY", false, timer_active).clicked() {
                    state.cancel_stop_timer();
                }
            });
            state.sanitize_stop_timer_inputs();
            if let Some(remaining) = state.stop_timer_remaining() {
                text(
                    ui,
                    &format!(
                        "ĐANG BẬT · tạm dừng sau {}",
                        format_timer_duration(remaining)
                    ),
                    12.0,
                    ACCENT,
                    true,
                );
            } else {
                text(ui, "Chưa bật hẹn giờ", 11.0, DISABLED, false);
            }
            ui.add_space(14.0);
            ui.separator();
            ui.add_space(12.0);
            micro(ui, "MÀN HÌNH");
            ui.add_space(5.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    text(ui, "Luôn giữ màn hình sáng", 14.0, TEXT, true);
                    text(
                        ui,
                        "Giảm sáng màn hình StockOS và giảm tốc độ dựng hình sau thời gian chờ.",
                        11.0,
                        SECONDARY,
                        false,
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let enabled = state.always_keep_screen_on;
                    if hifi_button(ui, if enabled { "BẬT" } else { "TẮT" }, enabled, true).clicked()
                    {
                        state.set_always_keep_screen_on(!enabled);
                    }
                });
            });
            ui.add_space(14.0);
            ui.separator();
            ui.add_space(12.0);
            micro(ui, "TRẠNG THÁI PIN");
            ui.add_space(5.0);
            let battery_detail = state
                .battery_percentage()
                .map(|value| format!("StockOS báo còn {value}% pin"))
                .unwrap_or_else(|| "Đang chờ dữ liệu pin tương thích từ StockOS".to_owned());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    text(ui, "Hiển thị pin trên thanh trên", 14.0, TEXT, true);
                    text(ui, &battery_detail, 11.0, SECONDARY, false);
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let enabled = state.show_battery_percentage;
                    if hifi_button(ui, if enabled { "BẬT" } else { "TẮT" }, enabled, true).clicked()
                    {
                        state.set_show_battery_percentage(!enabled);
                    }
                });
            });
        });
}
fn empty(ui: &mut Ui, icon: Icon, title: &str, detail: &str) {
    Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(Margin::same(22))
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::hover());
                paint_icon(ui.painter(), icon, rect, ACCENT);
                text(ui, title, 17.0, TEXT, true);
                text(ui, detail, 12.0, SECONDARY, false);
            });
        });
}

fn track_list(
    ui: &mut Ui,
    state: &mut UiState,
    artwork_manager: &mut ArtworkManager,
    track_ids: &[TrackId],
) {
    for (number, &track_id) in track_ids.iter().enumerate() {
        track_row(
            ui,
            state,
            artwork_manager,
            track_id,
            number + 1,
            None,
            Some((track_ids, number)),
            None,
        );
        ui.add_space(6.0);
    }
}

fn playlist_track_list(
    ui: &mut Ui,
    state: &mut UiState,
    artwork_manager: &mut ArtworkManager,
    playlist_id: crate::backend::PlaylistId,
    editable: bool,
    track_ids: &[TrackId],
) {
    let remove_mode = editable && state.playlist_track_remove_mode(playlist_id);
    for (track_index, &track_id) in track_ids.iter().enumerate() {
        track_row(
            ui,
            state,
            artwork_manager,
            track_id,
            track_index + 1,
            None,
            Some((track_ids, track_index)),
            editable.then_some((playlist_id, track_index, remove_mode)),
        );
        ui.add_space(6.0);
    }
}

fn track_row(
    ui: &mut Ui,
    state: &mut UiState,
    artwork_manager: &mut ArtworkManager,
    track_id: TrackId,
    number: usize,
    queue_entry: Option<crate::player::QueueEntryId>,
    context: Option<(&[TrackId], usize)>,
    playlist_context: Option<(crate::backend::PlaylistId, usize, bool)>,
) {
    let Some(track) = state.track(track_id) else {
        return;
    };
    let active = queue_entry
        .map(|entry| state.is_current_queue_entry(entry))
        .unwrap_or_else(|| state.is_current_track(track.id));
    let playing = active && state.is_playing();
    let metadata_label = if track.mood.trim().is_empty() {
        "Brickwave"
    } else {
        track.mood.as_str()
    };
    Frame::new()
        .fill(if active { WARM } else { SURFACE })
        .stroke(Stroke::new(1.0_f32, if active { ACCENT } else { SUBTLE }))
        .corner_radius(CornerRadius::same(SMALL_RADIUS))
        .inner_margin(Margin::symmetric(9, 7))
        .show(ui, |ui| {
            ui.set_min_height(45.0);
            ui.horizontal(|ui| {
                let (num, _) = ui.allocate_exact_size(vec2(22.0, 34.0), Sense::hover());
                if active {
                    paint_icon(
                        ui.painter(),
                        if playing { Icon::Pause } else { Icon::Play },
                        num.shrink(8.0),
                        ACCENT,
                    );
                } else {
                    ui.painter().text(
                        num.center(),
                        Align2::CENTER_CENTER,
                        format!("{:02}", number),
                        egui::FontId::proportional(font_size(10.0)),
                        SECONDARY,
                    );
                }
                artwork(ui, artwork_manager, &track, 36.0);
                ui.add_sized(
                    [176.0, 25.0],
                    egui::Label::new(
                        RichText::new(&track.title)
                            .size(font_size(13.0))
                            .color(TEXT)
                            .strong(),
                    )
                    .truncate(),
                );
                ui.add_sized(
                    [135.0, 25.0],
                    egui::Label::new(
                        RichText::new(&track.artist)
                            .size(font_size(12.0))
                            .color(SECONDARY),
                    )
                    .truncate(),
                );
                ui.add_sized(
                    [112.0, 25.0],
                    egui::Label::new(
                        RichText::new(metadata_label)
                            .size(font_size(11.0))
                            .color(SECONDARY),
                    )
                    .truncate(),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some((playlist_id, track_index, true)) = playlist_context {
                        let remove =
                            hifi_button(ui, "XÓA", true, !state.playlist_track_remove_pending());
                        if remove.clicked() {
                            state.request_remove_track_from_playlist(
                                playlist_id,
                                track.id,
                                track_index,
                            );
                        }
                    } else if playlist_context.is_none() {
                        let add = icon_button(
                            ui,
                            ("row-add-playlist", track.id.get(), number),
                            Icon::Save,
                            false,
                            state.data_mode == DataMode::Live && !state.playlist_add_pending(),
                            "Thêm vào danh sách phát",
                        );
                        if add.clicked() {
                            state.open_add_to_playlist(track.id);
                        }
                    }
                    if !matches!(playlist_context, Some((_, _, true))) {
                        let liked = state.is_liked(track.id);
                        let like = icon_button(
                            ui,
                            ("row-like", track.id.get(), number),
                            if liked { Icon::LikeFilled } else { Icon::Like },
                            liked,
                            !state.like_pending(track.id),
                            "Thích bài hát",
                        );
                        if like.clicked() {
                            state.toggle_like(track.id);
                        }
                        text(ui, &track.duration_label(), 10.0, SECONDARY, false);
                        let play = icon_button(
                            ui,
                            ("row-play", track.id.get(), number),
                            if playing { Icon::Pause } else { Icon::Play },
                            playing,
                            true,
                            if state.data_mode == DataMode::Live {
                                "Chọn bài hát"
                            } else if playing {
                                "Tạm dừng"
                            } else {
                                "Phát"
                            },
                        );
                        if play.clicked() {
                            if active {
                                state.toggle_playback();
                            } else if let Some(entry_id) = queue_entry {
                                state.select_queue_entry(entry_id);
                            } else if let Some((track_ids, start_at)) = context {
                                state.select_track_from_context(track_ids.to_vec(), start_at);
                            } else {
                                state.select_track(track.id);
                            }
                        }
                    }
                });
            });
        });
}
fn artwork(
    ui: &mut Ui,
    artwork_manager: &mut ArtworkManager,
    track: &Track,
    size: f32,
) -> Response {
    if let Some(url) = track.artwork_url.as_deref() {
        let slot = Rect::from_min_size(ui.next_widget_position(), vec2(size, size));
        let visible = ui.is_rect_visible(slot);
        if let Some(texture) = artwork_manager.texture_for_visible_url(Some(url), visible) {
            return ui
                .add(
                    egui::Image::from_texture(texture)
                        .fit_to_exact_size(vec2(size, size))
                        .corner_radius(CornerRadius::same((size * 0.13) as u8))
                        .sense(Sense::click()),
                )
                .on_hover_text("Mở màn hình Đang phát");
        }
    } else if let Some(artwork_id) = track.artwork {
        let texture = artwork_manager.fixture_texture_for(artwork_id);
        return ui
            .add(
                egui::Image::from_texture(texture)
                    .fit_to_exact_size(vec2(size, size))
                    .corner_radius(CornerRadius::same((size * 0.13) as u8))
                    .sense(Sense::click()),
            )
            .on_hover_text("Mở màn hình Đang phát");
    }
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let tint = Color32::from_rgb(track.tint[0], track.tint[1], track.tint[2]);
    let p = ui.painter();
    p.rect(
        rect,
        CornerRadius::same((size * 0.13) as u8),
        tint.gamma_multiply(0.72),
        Stroke::new(1.0_f32, tint),
        egui::StrokeKind::Inside,
    );
    let inner = rect.shrink(size * 0.15);
    p.rect_filled(inner, CornerRadius::same((size * 0.08) as u8), RAISED);
    p.circle_stroke(
        inner.center(),
        inner.width() * 0.27,
        Stroke::new((size * 0.035).max(1.0), tint),
    );
    p.circle_filled(inner.center(), inner.width() * 0.07, tint);
    p.line_segment(
        [
            pos2(inner.left() + 3.0, inner.bottom() - inner.height() * 0.22),
            pos2(inner.right() - 3.0, inner.bottom() - inner.height() * 0.22),
        ],
        Stroke::new((size * 0.035).max(1.0), TEXT.gamma_multiply(0.72)),
    );
    response.on_hover_text("Mở màn hình Đang phát")
}

fn now_playing(
    ui: &mut Ui,
    state: &mut UiState,
    artwork_manager: &mut ArtworkManager,
    waveform_manager: &mut WaveformManager,
) {
    let live = state.playback_is_unavailable();
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), 38.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            let back = icon_button(ui, "back", Icon::Back, false, true, "Quay lại");
            if back.clicked() {
                state.close_now_playing();
            }
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                micro(ui, "CHI TIẾT BÀI HÁT");
                text(ui, "Đang phát", 23.0, TEXT, true);
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let q = icon_button(
                    ui,
                    "now-queue",
                    Icon::Queue,
                    state.queue_open,
                    true,
                    if live {
                        "Hàng chờ bài hát"
                    } else {
                        "Hàng chờ"
                    },
                );
                if q.clicked() {
                    state.queue_open = !state.queue_open;
                }
            });
        },
    );
    ui.painter().line_segment(
        [
            pos2(ui.min_rect().left(), ui.cursor().top()),
            pos2(ui.max_rect().right(), ui.cursor().top()),
        ],
        Stroke::new(1.0, SUBTLE),
    );
    ui.add_space(12.0);

    egui::ScrollArea::vertical()
        .id_salt("now-playing-content")
        .auto_shrink([false, false])
        .scroll_source(egui::scroll_area::ScrollSource::ALL)
        .show(ui, |ui| {
            now_playing_content(ui, state, artwork_manager, waveform_manager);
            ui.add_space(12.0);
        });
}

fn now_playing_content(
    ui: &mut Ui,
    state: &mut UiState,
    artwork_manager: &mut ArtworkManager,
    waveform_manager: &mut WaveformManager,
) {
    let live = state.playback_is_unavailable();
    let Some(track) = state.current_track() else {
        empty(
            ui,
            Icon::Search,
            if live {
                "Chọn một bài hát"
            } else {
                "Chưa chọn bài hát"
            },
            if live {
                "Tìm kiếm rồi chọn một bài để xem thông tin và hàng chờ."
            } else {
                "Chọn một bài hát xem trước để mở màn hình Đang phát."
            },
        );
        return;
    };

    let position = state.player_state().position_seconds;
    let position_label = if live {
        "--:--".to_owned()
    } else {
        format_position(position)
    };
    let card_width = ui.available_width();
    let artwork_size = if card_width < 690.0 { 210.0 } else { 228.0 };
    let card_height = 274.0;
    let (card_rect, _) = ui.allocate_exact_size(vec2(card_width, card_height), Sense::hover());
    paint_now_playing_card(ui.painter(), card_rect);

    let inner = card_rect.shrink(18.0);
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.set_clip_rect(inner);
            artwork(ui, artwork_manager, &track, artwork_size);
            ui.add_space(22.0);

            let detail_width = (inner.width() - artwork_size - 22.0).max(280.0);
            ui.allocate_ui_with_layout(
                vec2(detail_width, inner.height()),
                Layout::top_down(Align::Min),
                |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    micro(
                        ui,
                        if track.mood.trim().is_empty() {
                            "BÀI HÁT BRICKWAVE"
                        } else {
                            &track.mood
                        },
                    );
                    ui.add_space(1.0);
                    let display_title = ellipsize_chars(&track.title, 58);
                    ui.add_sized(
                        [detail_width, 58.0],
                        egui::Label::new(
                            RichText::new(display_title)
                                .size(font_size(27.0))
                                .color(TEXT)
                                .strong(),
                        )
                        .wrap(),
                    );
                    ui.add_sized(
                        [detail_width, 22.0],
                        egui::Label::new(
                            RichText::new(&track.artist)
                                .size(font_size(16.0))
                                .color(SECONDARY),
                        )
                        .truncate(),
                    );
                    ui.add_space(8.0);
                    now_playing_waveform(
                        ui,
                        state,
                        waveform_manager,
                        detail_width,
                        live,
                        track.waveform_url.as_deref(),
                        &position_label,
                        &track.duration_label(),
                    );
                },
            );
        },
    );
    if state.queue_open {
        ui.add_space(12.0);
        section(
            ui,
            "HÀNG CHỜ",
            if live {
                "Các bài hát trong ngữ cảnh hiện tại."
            } else {
                "Chọn bài bất kỳ hoặc cuộn xuống để xem thêm."
            },
        );
        ui.add_space(6.0);
        let entries = state.queue().to_vec();
        for (number, entry) in entries.iter().enumerate() {
            track_row(
                ui,
                state,
                artwork_manager,
                entry.track_id,
                number + 1,
                Some(entry.id),
                None,
                None,
            );
            ui.add_space(6.0);
        }
    }
}

fn paint_now_playing_card(painter: &egui::Painter, rect: Rect) {
    painter.rect_filled(rect, CornerRadius::same(RADIUS), SURFACE);

    // A restrained warm-to-charcoal gradient keeps the artwork side distinct
    // without competing with cover art or track metadata.
    let gradient = rect.shrink(1.0);
    let mut mesh = egui::Mesh::default();
    let left = Color32::from_rgb(0x32, 0x2b, 0x24);
    let right = Color32::from_rgb(0x20, 0x21, 0x1f);
    mesh.colored_vertex(gradient.left_top(), left);
    mesh.colored_vertex(gradient.right_top(), right);
    mesh.colored_vertex(gradient.right_bottom(), right);
    mesh.colored_vertex(gradient.left_bottom(), left);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
    painter.rect_stroke(
        rect,
        CornerRadius::same(RADIUS),
        Stroke::new(1.0, BORDER),
        egui::StrokeKind::Inside,
    );
}

fn now_playing_waveform(
    ui: &mut Ui,
    state: &mut UiState,
    waveform_manager: &mut WaveformManager,
    width: f32,
    unavailable: bool,
    waveform_url: Option<&str>,
    position: &str,
    duration: &str,
) {
    const PANEL_HEIGHT: f32 = 98.0;
    const HORIZONTAL_PADDING: f32 = 2.0;
    const TOP_PADDING: f32 = 3.0;
    const TIME_ROW_HEIGHT: f32 = 18.0;

    let sense = if unavailable {
        Sense::hover()
    } else {
        Sense::click_and_drag()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(width, PANEL_HEIGHT), sense);
    let painter = ui.painter();

    let waveform_rect = Rect::from_min_max(
        pos2(rect.left() + HORIZONTAL_PADDING, rect.top() + TOP_PADDING),
        pos2(
            rect.right() - HORIZONTAL_PADDING,
            rect.bottom() - TIME_ROW_HEIGHT - 4.0,
        ),
    );

    let progress = state.progress().clamp(0.0, 1.0);
    let future_color = if unavailable {
        DISABLED.gamma_multiply(0.45)
    } else {
        TEXT.gamma_multiply(0.72)
    };
    let played_color = if unavailable { DISABLED } else { ACCENT };

    if let Some(samples) = waveform_manager.samples_for_url(waveform_url) {
        const BAR_SLOT_WIDTH: f32 = 3.5;
        let bar_count = ((waveform_rect.width() / BAR_SLOT_WIDTH).floor() as usize).clamp(48, 180);
        let slot_width = waveform_rect.width() / bar_count as f32;
        let bar_width = (slot_width * 0.58).clamp(1.25, 2.2);
        // Keep a little vertical headroom so loud tracks do not turn into a
        // clipped rectangle at the top and bottom of the waveform area.
        let max_half_height = ((waveform_rect.height() * 0.5 - 2.0) * 0.9).max(3.0);
        for index in 0..bar_count {
            let x = waveform_rect.left() + (index as f32 + 0.5) * slot_width;
            let amplitude =
                display_amplitude(bar_amplitude(&samples.values, index, bar_count)).max(0.025);
            let half_height = max_half_height * amplitude;
            let bar = Rect::from_min_max(
                pos2(x - bar_width * 0.5, waveform_rect.center().y - half_height),
                pos2(x + bar_width * 0.5, waveform_rect.center().y + half_height),
            );
            let bar_progress = (index as f32 + 0.5) / bar_count as f32;
            painter.rect_filled(
                bar,
                CornerRadius::same(1),
                if bar_progress <= progress {
                    played_color
                } else {
                    future_color
                },
            );
        }
    } else {
        painter.line_segment(
            [
                pos2(waveform_rect.left(), waveform_rect.center().y),
                pos2(waveform_rect.right(), waveform_rect.center().y),
            ],
            Stroke::new(1.0, future_color),
        );
        painter.text(
            waveform_rect.center(),
            Align2::CENTER_CENTER,
            if waveform_url.is_some() {
                "Đang tải dạng sóng"
            } else {
                "Không có dạng sóng"
            },
            egui::FontId::proportional(font_size(10.5)),
            SECONDARY,
        );
    }

    if response.hovered() && !unavailable {
        if let Some(pointer) = response.hover_pos() {
            let marker_x = pointer.x.clamp(waveform_rect.left(), waveform_rect.right());
            painter.line_segment(
                [
                    pos2(marker_x, waveform_rect.top()),
                    pos2(marker_x, waveform_rect.bottom()),
                ],
                Stroke::new(1.0, ACCENT_HOVER.gamma_multiply(0.85)),
            );
        }
    }

    painter.text(
        pos2(waveform_rect.left(), rect.bottom() - 6.0),
        Align2::LEFT_BOTTOM,
        position,
        egui::FontId::proportional(font_size(10.5)),
        if unavailable { DISABLED } else { ACCENT },
    );
    painter.text(
        pos2(waveform_rect.right(), rect.bottom() - 6.0),
        Align2::RIGHT_BOTTOM,
        duration,
        egui::FontId::proportional(font_size(10.5)),
        SECONDARY,
    );

    if !unavailable {
        if let Some(pointer) = response.interact_pointer_pos() {
            if response.clicked() || response.dragged() {
                let requested =
                    ((pointer.x - waveform_rect.left()) / waveform_rect.width()).clamp(0.0, 1.0);
                if (requested - progress).abs() > f32::EPSILON {
                    state.seek_progress(requested);
                }
            }
        }
    }
    response.on_hover_text(if unavailable {
        "Không thể phát bài hát"
    } else {
        "Nhấn hoặc kéo dạng sóng để tua"
    });
}

fn ellipsize_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let mut shortened = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    shortened.push('…');
    shortened
}

fn create_playlist_dialog(ctx: &egui::Context, state: &mut UiState) {
    if !state.create_playlist_dialog_open() {
        return;
    }
    let screen = ctx.content_rect();
    let layer = egui::LayerId::new(egui::Order::Foreground, Id::new("create-playlist-overlay"));
    egui::Area::new(layer.id)
        .order(layer.order)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            ui.set_min_size(screen.size());
            let (overlay, _) = ui.allocate_exact_size(screen.size(), Sense::click());
            ui.painter()
                .rect_filled(overlay, CornerRadius::ZERO, Color32::from_black_alpha(185));

            let panel_size = vec2(
                560.0_f32.min(screen.width() - 32.0),
                245.0_f32.min(screen.height() - 32.0),
            );
            let panel_center = pos2(screen.center().x, screen.top() + panel_size.y * 0.5 + 38.0);
            let panel = Rect::from_center_size(panel_center, panel_size);
            ui.scope_builder(egui::UiBuilder::new().max_rect(panel), |ui| {
                Frame::new()
                    .fill(SIDEBAR_BG)
                    .stroke(Stroke::new(1.0, ACCENT))
                    .corner_radius(CornerRadius::same(RADIUS))
                    .inner_margin(Margin::same(18))
                    .show(ui, |ui| {
                        ui.set_min_size(panel_size - vec2(36.0, 36.0));
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                micro(ui, "THƯ VIỆN CỦA BẠN");
                                text(ui, "Tạo danh sách phát", 22.0, TEXT, true);
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if icon_button(
                                    ui,
                                    "close-create-playlist",
                                    Icon::Close,
                                    false,
                                    !state.playlist_create_pending(),
                                    "Đóng",
                                )
                                .clicked()
                                {
                                    state.close_create_playlist();
                                }
                            });
                        });
                        ui.add_space(12.0);
                        text(ui, "TÊN DANH SÁCH", 11.0, SECONDARY, true);
                        let edit = egui::TextEdit::singleline(state.new_playlist_title_mut())
                            .id(playlist_title_field_id())
                            .font(egui::TextStyle::Heading)
                            .hint_text("Nhập tên")
                            .char_limit(100);
                        ui.add_sized([ui.available_width(), 44.0], edit);
                        ui.add_space(8.0);
                        text(
                            ui,
                            "Danh sách mới mặc định ở chế độ riêng tư. Chọn ô nhập để mở bàn phím.",
                            11.0,
                            SECONDARY,
                            false,
                        );
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if hifi_button(
                                ui,
                                if state.playlist_create_pending() {
                                    "ĐANG TẠO..."
                                } else {
                                    "TẠO"
                                },
                                true,
                                !state.playlist_create_pending(),
                            )
                            .clicked()
                            {
                                state.submit_create_playlist();
                            }
                            if hifi_button(ui, "HỦY", false, !state.playlist_create_pending())
                                .clicked()
                            {
                                state.close_create_playlist();
                            }
                        });
                    });
            });
        });
}

fn remove_track_from_playlist_dialog(ctx: &egui::Context, state: &mut UiState) {
    let Some((playlist_id, track_id, _)) = state.remove_playlist_track() else {
        return;
    };
    let track_title = state
        .track(track_id)
        .map(|track| track.title)
        .unwrap_or_else(|| "Bài hát đã chọn".to_owned());
    let playlist_title = state
        .library_playlists()
        .into_iter()
        .find(|playlist| playlist.id == playlist_id)
        .map(|playlist| playlist.title)
        .unwrap_or_else(|| "danh sách này".to_owned());
    let screen = ctx.content_rect();
    let layer = egui::LayerId::new(
        egui::Order::Foreground,
        Id::new("remove-track-from-playlist-overlay"),
    );
    egui::Area::new(layer.id)
        .order(layer.order)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            ui.set_min_size(screen.size());
            let (overlay, _) = ui.allocate_exact_size(screen.size(), Sense::click());
            ui.painter()
                .rect_filled(overlay, CornerRadius::ZERO, Color32::from_black_alpha(190));
            let panel_size = vec2(
                540.0_f32.min(screen.width() - 32.0),
                238.0_f32.min(screen.height() - 32.0),
            );
            let panel = Rect::from_center_size(overlay.center(), panel_size);
            ui.scope_builder(egui::UiBuilder::new().max_rect(panel), |ui| {
                Frame::new()
                    .fill(SIDEBAR_BG)
                    .stroke(Stroke::new(1.0, ACCENT))
                    .corner_radius(CornerRadius::same(RADIUS))
                    .inner_margin(Margin::same(20))
                    .show(ui, |ui| {
                        ui.set_min_size(panel_size - vec2(40.0, 40.0));
                        micro(ui, "XÓA BÀI HÁT");
                        text(ui, "Xóa khỏi danh sách phát?", 22.0, TEXT, true);
                        ui.add_space(7.0);
                        text(ui, &ellipsize_chars(&track_title, 55), 14.0, TEXT, true);
                        text(
                            ui,
                            &format!(
                                "Khỏi {}. Bản thân bài hát sẽ không bị xóa.",
                                ellipsize_chars(&playlist_title, 38)
                            ),
                            11.0,
                            SECONDARY,
                            false,
                        );
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            if hifi_button(
                                ui,
                                if state.playlist_track_remove_pending() {
                                    "ĐANG XÓA..."
                                } else {
                                    "XÓA"
                                },
                                true,
                                !state.playlist_track_remove_pending(),
                            )
                            .clicked()
                            {
                                state.confirm_remove_track_from_playlist();
                            }
                            if hifi_button(ui, "HỦY", false, !state.playlist_track_remove_pending())
                                .clicked()
                            {
                                state.close_remove_track_from_playlist();
                            }
                        });
                    });
            });
        });
}

fn delete_playlist_dialog(ctx: &egui::Context, state: &mut UiState) {
    let Some(playlist_id) = state.delete_playlist_id() else {
        return;
    };
    let title = state
        .library_playlists()
        .into_iter()
        .find(|playlist| playlist.id == playlist_id)
        .map(|playlist| playlist.title)
        .unwrap_or_else(|| "Danh sách phát đã chọn".to_owned());
    let screen = ctx.content_rect();
    let layer = egui::LayerId::new(egui::Order::Foreground, Id::new("delete-playlist-overlay"));
    egui::Area::new(layer.id)
        .order(layer.order)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            ui.set_min_size(screen.size());
            let (overlay, _) = ui.allocate_exact_size(screen.size(), Sense::click());
            ui.painter()
                .rect_filled(overlay, CornerRadius::ZERO, Color32::from_black_alpha(190));
            let panel_size = vec2(
                520.0_f32.min(screen.width() - 32.0),
                230.0_f32.min(screen.height() - 32.0),
            );
            let panel = Rect::from_center_size(overlay.center(), panel_size);
            ui.scope_builder(egui::UiBuilder::new().max_rect(panel), |ui| {
                Frame::new()
                    .fill(SIDEBAR_BG)
                    .stroke(Stroke::new(1.0, ACCENT))
                    .corner_radius(CornerRadius::same(RADIUS))
                    .inner_margin(Margin::same(20))
                    .show(ui, |ui| {
                        ui.set_min_size(panel_size - vec2(40.0, 40.0));
                        micro(ui, "XÓA DANH SÁCH PHÁT");
                        text(ui, "Xóa khỏi SoundCloud?", 22.0, TEXT, true);
                        ui.add_space(7.0);
                        text(ui, &ellipsize_chars(&title, 55), 14.0, TEXT, true);
                        text(
                            ui,
                            "Danh sách phát sẽ bị xóa vĩnh viễn. Các bài hát bên trong không bị xóa.",
                            11.0,
                            SECONDARY,
                            false,
                        );
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            if hifi_button(
                                ui,
                                if state.playlist_delete_pending() {
                                    "ĐANG XÓA..."
                                } else {
                                    "XÓA"
                                },
                                true,
                                !state.playlist_delete_pending(),
                            )
                            .clicked()
                            {
                                state.confirm_delete_playlist();
                            }
                            if hifi_button(ui, "HỦY", false, !state.playlist_delete_pending())
                                .clicked()
                            {
                                state.close_delete_playlist();
                            }
                        });
                    });
            });
        });
}

fn exit_confirmation_dialog(ctx: &egui::Context, open: &mut bool) -> bool {
    if !*open {
        return false;
    }
    let mut confirmed = false;
    let mut cancelled = false;
    let screen = ctx.content_rect();
    let layer = egui::LayerId::new(egui::Order::Foreground, Id::new("exit-app-overlay"));
    egui::Area::new(layer.id)
        .order(layer.order)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            ui.set_min_size(screen.size());
            let (overlay, _) = ui.allocate_exact_size(screen.size(), Sense::click());
            ui.painter()
                .rect_filled(overlay, CornerRadius::ZERO, Color32::from_black_alpha(205));
            let panel_size = vec2(
                500.0_f32.min(screen.width() - 32.0),
                224.0_f32.min(screen.height() - 32.0),
            );
            let panel = Rect::from_center_size(overlay.center(), panel_size);
            ui.scope_builder(egui::UiBuilder::new().max_rect(panel), |ui| {
                Frame::new()
                    .fill(SIDEBAR_BG)
                    .stroke(Stroke::new(1.0, ACCENT))
                    .corner_radius(CornerRadius::same(RADIUS))
                    .inner_margin(Margin::same(20))
                    .show(ui, |ui| {
                        ui.set_min_size(panel_size - vec2(40.0, 40.0));
                        micro(ui, "THOÁT BRICKWAVE");
                        text(ui, "Quay lại màn hình chính?", 24.0, TEXT, true);
                        ui.add_space(7.0);
                        text(
                            ui,
                            "Nhạc sẽ dừng trước khi ứng dụng đóng.",
                            13.0,
                            SECONDARY,
                            false,
                        );
                        ui.add_space(18.0);
                        ui.horizontal(|ui| {
                            if hifi_button(ui, "A  THOÁT", true, true).clicked() {
                                confirmed = true;
                            }
                            if hifi_button(ui, "B  HỦY", false, true).clicked() {
                                cancelled = true;
                            }
                        });
                        ui.add_space(8.0);
                        micro(ui, "A: THOÁT NGAY   B: HỦY");
                    });
            });
        });
    if confirmed {
        *open = false;
        println!("BRICKWAVE_EXIT_CONFIRM action=exit");
    } else if cancelled {
        *open = false;
        println!("BRICKWAVE_EXIT_CONFIRM state=cancelled source=pointer");
    }
    confirmed
}

fn add_to_playlist_dialog(ctx: &egui::Context, state: &mut UiState) {
    let Some(track_id) = state.add_to_playlist_track() else {
        return;
    };
    let track_title = state
        .track(track_id)
        .map(|track| track.title)
        .unwrap_or_else(|| "Bài hát đã chọn".to_owned());
    let playlists: Vec<_> = state
        .library_playlists()
        .into_iter()
        .filter(|playlist| playlist.editable)
        .collect();
    let screen = ctx.content_rect();
    let layer = egui::LayerId::new(egui::Order::Foreground, Id::new("add-playlist-overlay"));
    egui::Area::new(layer.id)
        .order(layer.order)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            ui.set_min_size(screen.size());
            let (overlay, _) = ui.allocate_exact_size(screen.size(), Sense::click());
            ui.painter()
                .rect_filled(overlay, CornerRadius::ZERO, Color32::from_black_alpha(185));

            let panel_size = vec2(
                530.0_f32.min(screen.width() - 32.0),
                430.0_f32.min(screen.height() - 32.0),
            );
            let panel = Rect::from_center_size(overlay.center(), panel_size);
            ui.scope_builder(egui::UiBuilder::new().max_rect(panel), |ui| {
                Frame::new()
                    .fill(SIDEBAR_BG)
                    .stroke(Stroke::new(1.0, ACCENT))
                    .corner_radius(CornerRadius::same(RADIUS))
                    .inner_margin(Margin::same(18))
                    .show(ui, |ui| {
                        ui.set_min_size(panel_size - vec2(36.0, 36.0));
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                micro(ui, "LƯU BÀI HÁT");
                                text(ui, "Thêm vào danh sách phát", 22.0, TEXT, true);
                                text(
                                    ui,
                                    &ellipsize_chars(&track_title, 54),
                                    12.0,
                                    SECONDARY,
                                    false,
                                );
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if icon_button(
                                    ui,
                                    "close-add-playlist",
                                    Icon::Close,
                                    false,
                                    true,
                                    "Đóng",
                                )
                                .clicked()
                                {
                                    state.close_add_to_playlist();
                                }
                            });
                        });
                        ui.add_space(12.0);
                        ui.separator();
                        ui.add_space(10.0);
                        if playlists.is_empty() {
                            text(
                                ui,
                                "Tài khoản này chưa có danh sách phát nào có thể chỉnh sửa.",
                                13.0,
                                SECONDARY,
                                false,
                            );
                        } else {
                            egui::ScrollArea::vertical()
                                .id_salt("add-to-playlist-list")
                                .max_height(panel_size.y - 122.0)
                                .auto_shrink([false, false])
                                .scroll_source(egui::scroll_area::ScrollSource::ALL)
                                .show(ui, |ui| {
                                    for playlist in &playlists {
                                        let label = format!(
                                            "{}   ·   {} bài hát",
                                            ellipsize_chars(&playlist.title, 42),
                                            playlist.track_count
                                        );
                                        if hifi_button(
                                            ui,
                                            &label,
                                            false,
                                            !state.playlist_add_pending(),
                                        )
                                        .clicked()
                                        {
                                            state.add_track_to_playlist(playlist.id);
                                        }
                                        ui.add_space(7.0);
                                    }
                                });
                        }
                    });
            });
        });
}

#[cfg(test)]
mod now_playing_ui_tests {
    use super::ellipsize_chars;

    #[test]
    fn title_ellipsis_is_unicode_safe_and_bounded() {
        let title = "Đường về phía trước vẫn còn rất dài và nhiều ký tự tiếng Việt";
        let shortened = ellipsize_chars(title, 24);
        assert_eq!(shortened.chars().count(), 24);
        assert!(shortened.ends_with('…'));
        assert!(shortened.is_char_boundary(shortened.len()));
    }
}
fn toast(ctx: &egui::Context, state: &mut UiState) {
    let Some(message) = state.toast.as_deref() else {
        return;
    };
    let message = message.to_owned();
    egui::Area::new(Id::new("soundcloud-toast"))
        .anchor(Align2::RIGHT_TOP, vec2(-18.0, 18.0))
        .show(ctx, |ui| {
            Frame::new()
                .fill(SURFACE)
                .stroke(Stroke::new(1.0_f32, ACCENT))
                .corner_radius(CornerRadius::same(SMALL_RADIUS))
                .inner_margin(Margin::symmetric(11, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                        paint_icon(ui.painter(), Icon::Brand, rect, ACCENT);
                        text(ui, &message, 12.0, TEXT, false);
                        let close =
                            icon_button(ui, "toast-close", Icon::Close, false, true, "Đóng");
                        if close.clicked() {
                            state.toast = None;
                        }
                    });
                });
        });
}

#[cfg(test)]
mod toast_lifetime_tests {
    use std::time::{Duration, Instant};

    use super::{TOAST_VISIBLE_DURATION, ToastLifetime};

    #[test]
    fn toast_expires_after_five_seconds_and_new_text_restarts_the_timer() {
        let start = Instant::now();
        let mut lifetime = ToastLifetime::default();
        assert!(!lifetime.observe(Some("Playing Track A"), start));
        assert!(!lifetime.observe(
            Some("Playing Track A"),
            start + TOAST_VISIBLE_DURATION - Duration::from_millis(1)
        ));
        assert!(lifetime.observe(Some("Playing Track A"), start + TOAST_VISIBLE_DURATION));

        assert!(!lifetime.observe(Some("Playing Track B"), start + TOAST_VISIBLE_DURATION));
        assert!(!lifetime.observe(
            Some("Playing Track B"),
            start + TOAST_VISIBLE_DURATION + Duration::from_secs(4)
        ));
        assert!(lifetime.observe(Some("Playing Track B"), start + TOAST_VISIBLE_DURATION * 2));
    }
}
