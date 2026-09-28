//! Direct SDL2 host for TrimUI Brick Pro StockOS and NextUI.

use std::time::{Duration, Instant};

use egui_sdl2::sdl2;
use egui_sdl2::sdl2::event::{Event, WindowEvent};
use egui_sdl2::sdl2::keyboard::Keycode;
use egui_sdl2::sdl2::mouse::{MouseButton, MouseState, MouseWheelDirection};
use egui_sdl2::{EguiWindow, Renderer};

use crate::BrickwaveApp;
use crate::player::PlaybackStatus;
use crate::power_policy::{IdlePolicy, IdleStage, StockPowerController};
use crate::trimui_input::{
    HEIGHT, TrimuiAction, TrimuiInput, VirtualPointer, WIDTH, draw_virtual_cursor,
};
use trimui_ui_kit::keyboard::KeyboardControl;

const MAX_FRAME_DELTA: Duration = Duration::from_millis(50);
const ACTIVE_FRAME_TIME: Duration = Duration::from_millis(16);
const EFFICIENT_PLAYBACK_FRAME_TIME: Duration = Duration::from_millis(100);
const EFFICIENT_IDLE_FRAME_TIME: Duration = Duration::from_millis(250);
const ALWAYS_ON_DIMMED_FRAME_TIME: Duration = Duration::from_millis(500);
const SYSTEM_HANDOFF_POLL_TIME: Duration = Duration::from_millis(250);
const STOCKOS_WAKE_SETTLE_DELAY: Duration = Duration::from_millis(500);
const ANALOG_ACTIVITY_THRESHOLD: i32 = 5_000;
pub fn run() -> Result<(), String> {
    println!(
        "BRICKWAVE_TRIMUI_START version={} size={}x{}",
        env!("CARGO_PKG_VERSION"),
        WIDTH,
        HEIGHT
    );
    let sdl = sdl2::init().map_err(|error| format!("SDL_Init: {error}"))?;
    let video = sdl
        .video()
        .map_err(|error| format!("SDL video init: {error}"))?;
    let mut window = EguiWindow::new(
        &video,
        "Brickwave",
        (WIDTH as u32, HEIGHT as u32),
        |builder| {
            builder.fullscreen_desktop();
        },
        &Renderer::FALLBACK_CHAIN,
    )
    .map_err(|error| format!("SDL renderer setup: {error}"))?;
    println!("BRICKWAVE_RENDERER {:?}", window.renderer());

    let mut app = BrickwaveApp::new_with_context(window.ctx());
    let mut events = sdl
        .event_pump()
        .map_err(|error| format!("SDL event pump: {error}"))?;
    // KEY_POWER always remains owned by StockOS. The distinct BTN_MODE event is
    // Brickwave's MENU/exit-confirmation control.
    let mut input = TrimuiInput::open();
    let mut pointer = VirtualPointer::new(pointer_bounds(&window));
    let mut power = StockPowerController::new();
    let nextui_host = power.is_nextui();
    let mut idle_policy = IdlePolicy::new(
        app.state.always_keep_screen_on,
        power.stock_timeout(),
        Instant::now(),
    );
    // In follow-StockOS mode Brickwave must not advertise itself as a
    // stay-awake application.  Besides defeating the system idle policy, the
    // flag also leaves the StockOS power indicator in its awake state.  Only
    // the explicit always-on preference owns /tmp/stay_awake.
    // /tmp/stay_alive is independent: it lets StockOS switch the LCD off but
    // keeps keymon out of the deep-suspend path that can terminate the app.
    power.set_stay_alive(true);
    power.set_stay_awake(idle_policy.always_keep_screen_on());
    if nextui_host {
        println!(
            "BRICKWAVE_NEXTUI_HOST power_policy=external render_policy=active60,efficient10/4"
        );
    }
    let mut idle_display_engaged = false;
    let mut last_idle_stage = IdleStage::Active;
    let mut stockos_wake_at: Option<Instant> = None;
    let initial = pointer.position();
    send_pointer_move(&mut window, initial, initial);
    println!(
        "BRICKWAVE_VIRTUAL_MOUSE enabled=true deadzone_percent=15 a_click_drag=true b_back_only=true menu_exit_confirm=true y_select_pause=true start_play_resume=true l1_previous=true r1_next=true dpad_scroll=true dpad_keyboard_navigation=true"
    );

    let mut running = true;
    let mut first_frame = true;
    let mut previous_frame = Instant::now();
    while running {
        let frame_started = Instant::now();
        if stockos_wake_at.is_some_and(|deadline| frame_started >= deadline) {
            finish_stockos_wake(
                &mut app,
                window.ctx(),
                &mut power,
                &mut idle_policy,
                &mut idle_display_engaged,
                &mut pointer,
            );
            stockos_wake_at = None;
        }
        let configured_always_on = app.state.always_keep_screen_on;
        if configured_always_on != idle_policy.always_keep_screen_on() {
            idle_policy.set_always_keep_screen_on(configured_always_on, Instant::now());
            power.restore_brightness();
            power.set_stay_awake(configured_always_on);
            stockos_wake_at = None;
            idle_display_engaged = false;
            last_idle_stage = IdleStage::Active;
            println!(
                "BRICKWAVE_POWER_POLICY mode={} state=active reason=setting-change",
                if configured_always_on {
                    "always-on"
                } else {
                    "follow-stockos"
                }
            );
        }

        let mut woke_this_frame = false;
        for event in events.poll_iter() {
            let power_event = is_sdl_power_event(&event);
            let user_event = is_sdl_user_event(&event);
            if power_event {
                if idle_display_engaged {
                    request_idle_wake(
                        &mut power,
                        &mut idle_policy,
                        &mut idle_display_engaged,
                        &mut pointer,
                        &mut stockos_wake_at,
                        "sdl-power",
                    );
                }
                println!("BRICKWAVE_INPUT_POWER delegated=stockos source=sdl");
                continue;
            }
            if user_event {
                if idle_display_engaged {
                    request_idle_wake(
                        &mut power,
                        &mut idle_policy,
                        &mut idle_display_engaged,
                        &mut pointer,
                        &mut stockos_wake_at,
                        "sdl-input",
                    );
                    woke_this_frame = true;
                } else if stockos_wake_at.is_none() {
                    idle_policy.note_activity(Instant::now());
                }
            }
            if matches!(event, Event::Quit { .. })
                || matches!(
                    event,
                    Event::Window {
                        win_event: WindowEvent::Close,
                        ..
                    }
                )
            {
                running = false;
            }
            if matches!(
                event,
                Event::Window {
                    win_event: WindowEvent::FocusLost,
                    ..
                }
            ) {
                release_primary_button(&mut window, &mut pointer);
                pointer.reset_motion();
                println!("BRICKWAVE_VIRTUAL_MOUSE focus_lost_release=true");
            }
            if let Event::KeyDown {
                keycode: Some(Keycode::Escape),
                repeat: false,
                ..
            } = event
            {
                running = false;
            }
            // The first event only wakes a dimmed/suspended UI. Do not let it
            // click a control or open a modal underneath the old frame.
            if !(woke_this_frame && user_event) && stockos_wake_at.is_none() {
                let _ = window.on_event(&event);
            }
        }

        let actions = input.poll();
        let physical_activity = actions.iter().copied().any(is_physical_activity);
        let suppress_wake_actions = idle_display_engaged || stockos_wake_at.is_some();
        if physical_activity {
            if idle_display_engaged {
                request_idle_wake(
                    &mut power,
                    &mut idle_policy,
                    &mut idle_display_engaged,
                    &mut pointer,
                    &mut stockos_wake_at,
                    "physical-input",
                );
            } else if stockos_wake_at.is_none() {
                idle_policy.note_activity(Instant::now());
            }
        }
        for action in actions {
            if suppress_wake_actions {
                continue;
            }
            match action {
                TrimuiAction::Back => {
                    release_primary_button(&mut window, &mut pointer);
                    if app.dismiss_exit_confirmation() {
                        pointer.clear_dpad();
                    } else if app.handle_keyboard_control(KeyboardControl::Back) {
                        pointer.clear_dpad();
                    } else if app.handle_minimal_back() {
                        pointer.clear_dpad();
                    } else if app.state.dismiss_top_modal() {
                        pointer.clear_dpad();
                        println!("BRICKWAVE_CONTROL action=modal-close");
                    } else if app.state.can_navigate_back() {
                        app.state.navigate_back();
                        println!("BRICKWAVE_CONTROL action=back");
                    } else {
                        println!("BRICKWAVE_CONTROL action=back ignored=root");
                    }
                }
                TrimuiAction::Menu => {
                    release_primary_button(&mut window, &mut pointer);
                    pointer.reset_motion();
                    app.request_exit_confirmation();
                    println!("BRICKWAVE_CONTROL action=exit-confirmation");
                }
                TrimuiAction::Power(pressed) => {
                    println!(
                        "BRICKWAVE_INPUT_POWER delegated=stockos source=evdev pressed={pressed}"
                    );
                }
                TrimuiAction::Stop => {
                    if app.keyboard_is_open() || app.exit_confirmation_is_open() {
                        println!("BRICKWAVE_CONTROL action=pause ignored=overlay");
                    } else {
                        app.state.pause_playback();
                        println!("BRICKWAVE_CONTROL action=pause");
                    }
                }
                TrimuiAction::Start => {
                    if app.keyboard_is_open() || app.exit_confirmation_is_open() {
                        println!("BRICKWAVE_CONTROL action=start ignored=overlay");
                    } else {
                        app.state.start_playback();
                        println!("BRICKWAVE_CONTROL action=start");
                    }
                }
                TrimuiAction::Previous => {
                    if app.keyboard_is_open() || app.exit_confirmation_is_open() {
                        println!("BRICKWAVE_CONTROL action=previous ignored=overlay");
                    } else {
                        app.state.previous();
                        println!("BRICKWAVE_CONTROL action=previous");
                    }
                }
                TrimuiAction::Next => {
                    if app.keyboard_is_open() || app.exit_confirmation_is_open() {
                        println!("BRICKWAVE_CONTROL action=next ignored=overlay");
                    } else {
                        app.state.next();
                        println!("BRICKWAVE_CONTROL action=next");
                    }
                }
                TrimuiAction::Primary(pressed) => {
                    if app.exit_confirmation_is_open() {
                        release_primary_button(&mut window, &mut pointer);
                        if pressed {
                            app.confirm_exit_with_primary();
                        }
                    } else if app.handle_keyboard_control(KeyboardControl::Primary(pressed)) {
                        release_primary_button(&mut window, &mut pointer);
                    } else if app.handle_minimal_primary(pressed) {
                        release_primary_button(&mut window, &mut pointer);
                    } else if pointer.set_primary(pressed) {
                        send_primary_button(&mut window, pointer.position(), pressed);
                    }
                }
                TrimuiAction::SetStickX(value) => {
                    if !app.minimal_ui_enabled() {
                        pointer.set_stick_x(value);
                    }
                }
                TrimuiAction::SetStickY(value) => {
                    if !app.minimal_ui_enabled() {
                        pointer.set_stick_y(value);
                    }
                }
                TrimuiAction::SetDpadX(value) => {
                    if app.handle_keyboard_control(KeyboardControl::DpadX(value)) {
                        pointer.clear_dpad();
                    } else if app.handle_minimal_dpad_x(value) {
                        pointer.clear_dpad();
                    } else {
                        pointer.set_dpad_x(value);
                    }
                }
                TrimuiAction::SetDpadY(value) => {
                    if app.handle_keyboard_control(KeyboardControl::DpadY(value)) {
                        pointer.clear_dpad();
                    } else if app.handle_minimal_dpad_y(value) {
                        pointer.clear_dpad();
                    } else {
                        pointer.set_dpad_y(value);
                    }
                }
            }
        }

        // A physical A press confirms the exit before egui starts this frame.
        // Stop here so closing the modal cannot expose and present one last
        // frame of the app underneath it while MainUI is taking over.
        if app.take_exit_request() {
            prepare_clean_exit(&mut app, &mut window, &mut pointer, "physical-a");
            break;
        }

        let now = Instant::now();
        let delta = now
            .saturating_duration_since(previous_frame)
            .min(MAX_FRAME_DELTA);
        previous_frame = now;
        let exit_confirmation_open = app.exit_confirmation_is_open();
        if exit_confirmation_open {
            // Exit is a physical A/B decision. Freeze the virtual pointer so
            // the user never has to aim at a dialog button or wait for a click.
            pointer.reset_motion();
        } else if app.minimal_ui_enabled() {
            pointer.reset_motion();
        } else if !app.keyboard_is_open() {
            if let Some((previous, current)) = pointer.advance(delta, pointer_bounds(&window)) {
                idle_policy.note_activity(now);
                send_pointer_move(&mut window, previous, current);
            }
            if let Some((x, y)) = pointer.scroll_lines(delta) {
                idle_policy.note_activity(now);
                send_mouse_wheel(&mut window, pointer.position(), x, y);
            }
        }

        let idle_stage = idle_policy.stage(now);
        if idle_stage != last_idle_stage {
            println!(
                "BRICKWAVE_RENDER_POLICY state={} always_keep_screen_on={}",
                idle_stage_name(idle_stage),
                idle_policy.always_keep_screen_on()
            );
            last_idle_stage = idle_stage;
        }
        if idle_stage == IdleStage::Timeout && !idle_display_engaged {
            release_primary_button(&mut window, &mut pointer);
            pointer.reset_motion();
            if idle_policy.always_keep_screen_on() {
                power.set_stay_awake(true);
                power.dim();
                idle_display_engaged = true;
                println!("BRICKWAVE_POWER_POLICY state=dimmed render_fps=2 audio=continue");
            } else {
                let audio_stopped = app.state.suspend_audio_for_idle();
                app.service_workers();
                stockos_wake_at = None;
                power.restore_brightness();
                power.set_stay_awake(false);
                idle_display_engaged = true;
                println!(
                    "BRICKWAVE_POWER_POLICY state=stockos-handoff render=suspended audio_stopped={audio_stopped}"
                );
            }
        }

        let render_frame = !(idle_display_engaged && !idle_policy.always_keep_screen_on());
        if !render_frame {
            app.service_workers();
        }

        if render_frame {
            window.run_ui(|ui| {
                app.render_ui(ui);
                if !app.keyboard_is_open()
                    && !app.exit_confirmation_is_open()
                    && !app.minimal_ui_enabled()
                {
                    draw_virtual_cursor(ui.ctx(), pointer.position());
                }
            });
        }
        if app.keyboard_is_open() || app.exit_confirmation_is_open() {
            pointer.clear_dpad();
        }

        // Pointer activation of the EXIT button is resolved inside run_ui.
        // Check it before paint/present for the same no-flash transition as A.
        if render_frame && app.take_exit_request() {
            prepare_clean_exit(&mut app, &mut window, &mut pointer, "pointer");
            break;
        }

        if render_frame {
            window.paint([
                0x10 as f32 / 255.0,
                0x11 as f32 / 255.0,
                0x10 as f32 / 255.0,
                1.0,
            ]);
        }

        if first_frame {
            first_frame = false;
            let logical = window.window().size();
            let drawable = window.window().drawable_size();
            println!(
                "BRICKWAVE_FIRST_FRAME renderer={:?} logical={}x{} drawable={}x{} pixels_per_point={}",
                window.renderer(),
                logical.0,
                logical.1,
                drawable.0,
                drawable.1,
                window.ctx().pixels_per_point()
            );
        }
        let audio_busy = matches!(
            app.state.player_state().playback_status,
            PlaybackStatus::Loading | PlaybackStatus::Playing
        );
        let target_frame_time = match idle_stage {
            IdleStage::Active => ACTIVE_FRAME_TIME,
            IdleStage::Efficient if audio_busy => EFFICIENT_PLAYBACK_FRAME_TIME,
            IdleStage::Efficient => EFFICIENT_IDLE_FRAME_TIME,
            IdleStage::Timeout if idle_policy.always_keep_screen_on() => {
                ALWAYS_ON_DIMMED_FRAME_TIME
            }
            IdleStage::Timeout => SYSTEM_HANDOFF_POLL_TIME,
        };
        if let Some(remaining) = target_frame_time.checked_sub(frame_started.elapsed()) {
            std::thread::sleep(remaining);
        }
    }

    release_primary_button(&mut window, &mut pointer);
    power.cleanup();
    app.playback_engine.shutdown();
    app.backend.shutdown();
    window.destroy();
    drop(window);
    drop(app);
    println!("BRICKWAVE_CLEAN_EXIT code=0");
    Ok(())
}

fn is_sdl_user_event(event: &Event) -> bool {
    match event {
        Event::KeyDown {
            keycode: Some(Keycode::Power),
            ..
        }
        | Event::KeyUp {
            keycode: Some(Keycode::Power),
            ..
        } => false,
        Event::KeyDown { .. }
        | Event::MouseMotion { .. }
        | Event::MouseButtonDown { .. }
        | Event::MouseWheel { .. } => true,
        _ => false,
    }
}

fn is_sdl_power_event(event: &Event) -> bool {
    matches!(
        event,
        Event::KeyDown {
            keycode: Some(Keycode::Power),
            ..
        } | Event::KeyUp {
            keycode: Some(Keycode::Power),
            ..
        }
    )
}

fn is_physical_activity(action: TrimuiAction) -> bool {
    match action {
        TrimuiAction::Primary(false) | TrimuiAction::Power(false) => false,
        TrimuiAction::SetDpadX(value) | TrimuiAction::SetDpadY(value) => value != 0,
        TrimuiAction::SetStickX(value) | TrimuiAction::SetStickY(value) => {
            value.unsigned_abs() >= ANALOG_ACTIVITY_THRESHOLD as u32
        }
        TrimuiAction::Back
        | TrimuiAction::Menu
        | TrimuiAction::Power(true)
        | TrimuiAction::Stop
        | TrimuiAction::Start
        | TrimuiAction::Previous
        | TrimuiAction::Next
        | TrimuiAction::Primary(true) => true,
    }
}

fn idle_stage_name(stage: IdleStage) -> &'static str {
    match stage {
        IdleStage::Active => "active",
        IdleStage::Efficient => "efficient",
        IdleStage::Timeout => "timeout",
    }
}

fn request_idle_wake(
    power: &mut StockPowerController,
    idle_policy: &mut IdlePolicy,
    idle_display_engaged: &mut bool,
    pointer: &mut VirtualPointer,
    stockos_wake_at: &mut Option<Instant>,
    source: &str,
) {
    if !idle_policy.always_keep_screen_on() {
        if stockos_wake_at.is_none() {
            *stockos_wake_at = Some(Instant::now() + STOCKOS_WAKE_SETTLE_DELAY);
            pointer.reset_motion();
            println!("BRICKWAVE_POWER_POLICY state=wake-pending owner=stockos source={source}");
        }
        return;
    }

    power.restore_brightness();
    power.set_stay_awake(true);
    idle_policy.note_activity(Instant::now());
    *idle_display_engaged = false;
    pointer.reset_motion();
    println!("BRICKWAVE_POWER_POLICY state=active reason=wake source={source}");
}

fn finish_stockos_wake(
    app: &mut BrickwaveApp,
    ctx: &egui::Context,
    power: &mut StockPowerController,
    idle_policy: &mut IdlePolicy,
    idle_display_engaged: &mut bool,
    pointer: &mut VirtualPointer,
) {
    // Do not write brightness here. StockOS owns the complete off/wake cycle
    // in follow-stockos mode; Brickwave only resumes after that cycle settles.
    // Keep ownership with the OS instead of recreating /tmp/stay_awake here.
    power.set_stay_awake(false);
    power.turn_leds_off_after_wake();
    idle_policy.note_activity(Instant::now());
    *idle_display_engaged = false;
    pointer.reset_motion();
    app.on_stockos_wake(ctx);
    println!("BRICKWAVE_POWER_POLICY state=active reason=stockos-wake-settled");
}

fn prepare_clean_exit(
    app: &mut BrickwaveApp,
    window: &mut EguiWindow,
    pointer: &mut VirtualPointer,
    source: &str,
) {
    release_primary_button(window, pointer);
    pointer.reset_motion();
    app.stop_audio_for_exit();
    println!("BRICKWAVE_EXIT_PRESENT skipped=true source={source}");
}

fn pointer_bounds(window: &EguiWindow) -> (i32, i32) {
    let drawable = window.window().drawable_size();
    (
        drawable.0.min(WIDTH as u32).max(1) as i32,
        drawable.1.min(HEIGHT as u32).max(1) as i32,
    )
}

fn send_pointer_move(window: &mut EguiWindow, previous: (i32, i32), pointer: (i32, i32)) {
    let event = Event::MouseMotion {
        timestamp: 0,
        window_id: window.window().id(),
        which: 0,
        mousestate: MouseState::from_sdl_state(0),
        x: pointer.0,
        y: pointer.1,
        xrel: pointer.0 - previous.0,
        yrel: pointer.1 - previous.1,
    };
    let _ = window.on_event(&event);
}

fn send_primary_button(window: &mut EguiWindow, pointer: (i32, i32), pressed: bool) {
    let event = if pressed {
        Event::MouseButtonDown {
            timestamp: 0,
            window_id: window.window().id(),
            which: 0,
            mouse_btn: MouseButton::Left,
            clicks: 1,
            x: pointer.0,
            y: pointer.1,
        }
    } else {
        Event::MouseButtonUp {
            timestamp: 0,
            window_id: window.window().id(),
            which: 0,
            mouse_btn: MouseButton::Left,
            clicks: 1,
            x: pointer.0,
            y: pointer.1,
        }
    };
    let _ = window.on_event(&event);
    println!(
        "BRICKWAVE_MOUSE_PRIMARY state={} x={} y={}",
        if pressed { "down" } else { "up" },
        pointer.0,
        pointer.1
    );
}

fn release_primary_button(window: &mut EguiWindow, pointer: &mut VirtualPointer) {
    if pointer.set_primary(false) {
        send_primary_button(window, pointer.position(), false);
    }
}

fn send_mouse_wheel(window: &mut EguiWindow, pointer: (i32, i32), x: i32, y: i32) {
    if x == 0 && y == 0 {
        return;
    }
    let event = Event::MouseWheel {
        timestamp: 0,
        window_id: window.window().id(),
        which: 0,
        x,
        y,
        direction: MouseWheelDirection::Normal,
        precise_x: x as f32,
        precise_y: y as f32,
        mouse_x: pointer.0,
        mouse_y: pointer.1,
    };
    let _ = window.on_event(&event);
}
