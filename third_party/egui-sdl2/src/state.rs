//! State management for egui + SDL2 integration.
//!
//! This module provides [`State`], which is responsible for translating
//! SDL2 events and window data into egui input/output.
//! Each SDL2 window (viewport) should have its own [`State`] instance.
//!
//!  # Usage
//! Typical usage is to:
//! 1. Create a [`State`] from an SDL2 [`Window`]
//! 2. Call [`State::on_event`] for every SDL2 event
//! 3. Retrieve input via [`State::take_egui_input`] before each frame
//! 4. Run your egui UI code
//! 5. Apply [`egui::PlatformOutput`] (cursor, clipboard, etc.)
//!
use egui::{Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect};
use sdl2::event::WindowEvent;
use sdl2::keyboard::Keycode;
use sdl2::keyboard::Mod;
use sdl2::keyboard::Scancode;
use sdl2::mouse::{Cursor, MouseButton, SystemCursor};
use sdl2::video::Window;

#[must_use]
#[derive(Clone, Copy, Debug, Default)]
pub struct EventResponse {
    /// If true, egui consumed this event, i.e. wants exclusive use of this event
    /// (e.g. a mouse click on an egui window, or entering text into a text field).
    ///
    /// For instance, if you use egui for a game, you should only
    /// pass on the events to your game when [`Self::consumed`] is `false`.
    ///
    /// Note that egui uses `tab` to move focus between elements, so this will always be `true` for tabs.
    pub consumed: bool,

    /// Do we need an egui refresh because of this event?
    pub repaint: bool,
}

/// Handles the integration between egui and a sdl2 Window.
///
/// Instantiate one of these per viewport/window.
pub struct State {
    egui_ctx: egui::Context,
    egui_input: egui::RawInput,
    start_time: std::time::Instant,
    viewport_id: egui::ViewportId,
    pointer_pos_in_points: Option<egui::Pos2>,
    /// The finger currently driving synthesized pointer events. The first finger
    /// down becomes the pointer; extra fingers only feed multi-touch gestures so
    /// they don't emit phantom clicks. Cleared on its up/cancel.
    pointer_touch_id: Option<i64>,
    current_cursor: Option<CurrentCursor>,
    /// The held modifier keys. egui 0.36 takes them as a `ModifiersChanged`
    /// event rather than a `RawInput` field, so the current set lives here.
    modifiers: egui::Modifiers,
    clipboard: sdl2::clipboard::ClipboardUtil,
    /// How far the frame is turned on its way to the window. The layout rect is
    /// the turned screen, and pointer positions come back through it.
    rotation: crate::Rotation,
    window_size: (u32, u32), // cache value and update on events
    // Drawable size in pixels, cached and refreshed on resize. `take_egui_input`
    // divides it by the *current* zoom each frame to rebuild `screen_rect`, so a
    // `set_zoom_factor` after construction is reflected without waiting for a
    // resize event (otherwise the UI lays out for the wrong rect until rotation).
    drawable_size: (u32, u32),
}

/// A file dropped onto the window. egui 0.36 takes dropped files as a trait
/// object the integration owns; SDL hands over a path, and the bytes are read
/// from it on demand.
#[derive(Debug)]
struct SdlDroppedFile(std::path::PathBuf);

impl egui::DroppedFile for SdlDroppedFile {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

/// Represents currently active cursor.
///
/// Contains egui icon and allocation of sdl2 cursor.
struct CurrentCursor {
    icon: egui::CursorIcon,
    cursor: Option<sdl2::mouse::Cursor>, // keep reference
}

impl State {
    pub fn new(window: &Window, egui_ctx: egui::Context, viewport_id: egui::ViewportId) -> Self {
        // Unturned until the embedder says otherwise; `set_rotation` is what a
        // turned panel calls, and the rect is rebuilt every frame regardless.
        let screen_rect = new_screen_rect(&egui_ctx, window, crate::Rotation::None);
        let mut egui_input = egui::RawInput {
            focused: false, // event will tell us when we have focus
            screen_rect,
            ..Default::default()
        };
        egui_input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(native_pixels_per_point(window));
        let clipboard = window.subsystem().clipboard();
        let window_size = window.size();
        let drawable_size = window.drawable_size();

        State {
            egui_ctx,
            viewport_id,
            clipboard,
            start_time: std::time::Instant::now(),
            egui_input,
            pointer_pos_in_points: None,
            pointer_touch_id: None,
            current_cursor: None,
            modifiers: egui::Modifiers::default(),
            rotation: crate::Rotation::None,
            window_size,
            drawable_size,
        }
    }

    /// Present the UI at a quarter turn to the window, for a panel that is not
    /// mounted the way it is read.
    ///
    /// This is the half of the turn that egui sees: the layout rect becomes the
    /// turned screen, and pointer positions are mapped back into it. The other
    /// half is the backend's — see [`crate::EguiWindow::set_rotation`], which
    /// sets both.
    #[inline]
    pub fn set_rotation(&mut self, rotation: crate::Rotation) {
        self.rotation = rotation;
    }

    #[inline]
    pub fn rotation(&self) -> crate::Rotation {
        self.rotation
    }

    #[inline]
    pub fn get_window_size(&self) -> (u32, u32) {
        self.window_size
    }

    /// Drawable (physical pixel) size, cached and refreshed on resize.
    ///
    /// This is the size a GPU backend should use for its viewport/framebuffer:
    /// on HiDPI it differs from [`Self::get_window_size`] (logical points).
    #[inline]
    pub fn get_drawable_size(&self) -> (u32, u32) {
        self.drawable_size
    }

    #[inline]
    pub fn get_pointer_pos_in_points(&self) -> Option<egui::Pos2> {
        self.pointer_pos_in_points
    }

    /// Cap the texture size egui lays its font atlas out for, from the painter's
    /// own limit. egui defaults to 2048, past what handheld drivers accept, and
    /// the atlas is allocated before the first frame.
    #[inline]
    pub fn set_max_texture_side(&mut self, max_texture_side: Option<usize>) {
        self.egui_input.max_texture_side = max_texture_side;
    }

    #[inline]
    pub fn set_theme(&mut self, theme: egui::Theme) {
        self.egui_input.system_theme.replace(theme);
    }

    /// Call with the output given by `egui`.
    ///
    /// This will, if needed:
    /// * update the cursor
    /// * copy text to the clipboard
    /// * open any clicked urls
    /// *
    #[inline]
    pub fn handle_platform_output(&mut self, platform_output: egui::PlatformOutput) {
        for command in &platform_output.commands {
            match command {
                egui::OutputCommand::CopyText(text) => {
                    let result = self.clipboard.set_clipboard_text(text);

                    if result.is_err() {
                        log::warn!("Failed to set copied text to clipboard");
                    }
                }
                egui::OutputCommand::CopyImage(_color_image) => {
                    log::warn!("CopyImage is not supported")
                }
                egui::OutputCommand::OpenUrl(_url) => {
                    #[cfg(feature = "links")]
                    if let Err(err) = webbrowser::open(&_url.url) {
                        log::warn!("Failed to open url: {}", err);
                    }

                    #[cfg(not(feature = "links"))]
                    {
                        log::warn!("Cannot open url - feature \"links\" not enabled.");
                    }
                }
            }
        }

        self.set_cursor_icon(platform_output.cursor_icon);
    }

    /// Prepare for a new frame by extracting the accumulated input,
    ///
    /// as well as setting [the time](egui::RawInput::time)
    ///
    /// You need to set [`egui::RawInput::viewports`] yourself though.
    #[inline]
    pub fn take_egui_input(&mut self) -> egui::RawInput {
        self.egui_input.time = Some(self.start_time.elapsed().as_secs_f64());
        // Tell egui which viewport is now active:
        self.egui_input.viewport_id = self.viewport_id;

        // Rebuild `screen_rect` from the cached drawable size and the *current*
        // zoom factor every frame. `screen_rect` is in points (pixels / ppp), and
        // ppp depends on `zoom_factor`, which the embedder can change after `new`
        // (e.g. a HiDPI `set_zoom_factor` on Android). Recomputing here keeps the
        // layout rect correct without needing a resize event to trigger it.
        let native_ppp = self
            .egui_input
            .viewports
            .get(&self.viewport_id)
            .and_then(|v| v.native_pixels_per_point)
            .unwrap_or(1.0);
        let ppp = self.egui_ctx.zoom_factor() * native_ppp;
        if ppp > 0.0 {
            let points = egui::vec2(self.drawable_size.0 as f32, self.drawable_size.1 as f32) / ppp;
            if points.x > 0.0 && points.y > 0.0 {
                // The screen egui lays out for is the *turned* one: a quarter
                // turn trades width for height.
                let screen = self.rotation.screen_size(points);
                self.egui_input.screen_rect =
                    Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen));
            }
        }

        self.egui_input.take()
    }

    /// Pixels-per-point for mapping window pixel coordinates to egui points.
    ///
    /// Built from the *cached* native pixels-per-point — the same value
    /// [`Self::take_egui_input`] lays `screen_rect` out with (stored on the
    /// viewport by [`Self::on_size_chage`]) — times the *live* zoom factor.
    /// Reading the stored native ppp instead of re-querying the window keeps
    /// pointer coordinates on the same basis as the layout and avoids an SDL
    /// size query on every pointer event; reading `zoom_factor` live keeps
    /// `set_zoom_factor`/ctrl-wheel zoom reflected within the same frame.
    #[inline]
    fn cached_pixels_per_point(&self) -> f32 {
        let native_ppp = self
            .egui_input
            .viewports
            .get(&self.viewport_id)
            .and_then(|v| v.native_pixels_per_point)
            .unwrap_or(1.0);
        self.egui_ctx.zoom_factor() * native_ppp
    }

    /// Convert window pixel coordinates to egui points via
    /// [`Self::cached_pixels_per_point`], and back through the rotation: egui
    /// laid the frame out for the turned screen, so that is where a press has to
    /// land.
    #[inline]
    fn pos_in_points(&self, x: f32, y: f32) -> egui::Pos2 {
        let ppp = self.cached_pixels_per_point();
        let scale = if ppp > 0.0 { ppp } else { 1.0 };
        let window = egui::vec2(self.drawable_size.0 as f32, self.drawable_size.1 as f32) / scale;
        self.rotation.from_window(egui::pos2(x, y) / scale, window)
    }

    /// Call this when there is a new event.
    ///
    /// The result can be extracted with [`Self::take_egui_input`].
    pub fn on_event(
        &mut self,
        window: &sdl2::video::Window,
        event: &sdl2::event::Event,
    ) -> EventResponse {
        use sdl2::event::Event::*;
        match event {
            Window { win_event, .. } => self.on_window_event(*win_event, window),
            MouseButtonDown {
                mouse_btn, x, y, ..
            } => self.on_mouse_button_event(*mouse_btn, true, *x, *y),
            MouseButtonUp {
                mouse_btn, x, y, ..
            } => self.on_mouse_button_event(*mouse_btn, false, *x, *y),
            MouseMotion { x, y, .. } => {
                let pos = self.pos_in_points(*x as f32, *y as f32);
                self.pointer_pos_in_points = Some(pos);
                self.egui_input.events.push(egui::Event::PointerMoved(pos));
                EventResponse {
                    repaint: true,
                    consumed: self.egui_ctx.egui_is_using_pointer(),
                }
            }
            MouseWheel { x, y, .. } => {
                let dx = *x as f32;
                let dy = *y as f32;

                if self.modifiers.command {
                    // zoom
                    let delta = (dy / 125.0).exp();
                    self.egui_input.events.push(egui::Event::Zoom(delta));
                } else if self.modifiers.shift {
                    // horizontal scroll
                    self.egui_input.events.push(egui::Event::MouseWheel {
                        unit: MouseWheelUnit::Line,
                        delta: egui::vec2(dx + dy, 0.0),
                        phase: egui::TouchPhase::Move,
                        modifiers: self.modifiers,
                    });
                } else {
                    // regular scroll
                    self.egui_input.events.push(egui::Event::MouseWheel {
                        unit: MouseWheelUnit::Line,
                        delta: egui::vec2(dx, dy),
                        phase: egui::TouchPhase::Move,
                        modifiers: self.modifiers,
                    });
                }
                EventResponse {
                    repaint: true,
                    consumed: self.egui_ctx.egui_wants_pointer_input(),
                }
            }
            KeyUp {
                keycode: Some(kc),
                scancode: Some(sc),
                keymod,
                repeat,
                ..
            } => self.on_keyboard_event(*kc, *sc, *keymod, false, *repeat),
            KeyDown {
                keycode: Some(kc),
                scancode: Some(sc),
                keymod,
                repeat,
                ..
            } => {
                let resp = self.on_keyboard_event(*kc, *sc, *keymod, true, *repeat);

                if self.modifiers.command && *kc == Keycode::C {
                    self.egui_input.events.push(egui::Event::Copy);
                } else if self.modifiers.command && *kc == Keycode::X {
                    self.egui_input.events.push(egui::Event::Cut);
                } else if self.modifiers.command && *kc == Keycode::V {
                    if let Ok(contents) = self.clipboard.clipboard_text() {
                        self.egui_input.events.push(egui::Event::Text(contents));
                    }
                }

                resp
            }
            TextInput { text, .. } => {
                let mut resp = EventResponse {
                    consumed: true,
                    repaint: false,
                };
                if !text.is_empty() {
                    // On some platforms we get here when the user presses Cmd-C (copy), ctrl-W, etc.
                    // We need to ignore these characters that are side-effects of commands.
                    let is_cmd =
                        self.modifiers.ctrl || self.modifiers.command || self.modifiers.mac_cmd;

                    if !is_cmd {
                        self.egui_input
                            .events
                            .push(egui::Event::Text(text.to_owned()));

                        resp.repaint = true;
                    }
                }

                resp
            }
            DropFile { filename, .. } => {
                self.egui_input
                    .dropped_files
                    .push(std::sync::Arc::new(SdlDroppedFile(
                        std::path::PathBuf::from(filename),
                    )));
                EventResponse {
                    repaint: true,
                    consumed: false,
                }
            }
            FingerDown {
                touch_id,
                finger_id,
                x,
                y,
                pressure,
                ..
            } => self.on_touch(TouchInfo {
                phase: egui::TouchPhase::Start,
                touch_id: *touch_id,
                finger_id: *finger_id,
                x: *x,
                y: *y,
                pressure: *pressure,
            }),
            FingerUp {
                touch_id,
                finger_id,
                x,
                y,
                pressure,
                ..
            } => self.on_touch(TouchInfo {
                phase: egui::TouchPhase::End,
                touch_id: *touch_id,
                finger_id: *finger_id,
                x: *x,
                y: *y,
                pressure: *pressure,
            }),
            FingerMotion {
                touch_id,
                finger_id,
                x,
                y,
                pressure,
                ..
            } => self.on_touch(TouchInfo {
                phase: egui::TouchPhase::Move,
                touch_id: *touch_id,
                finger_id: *finger_id,
                x: *x,
                y: *y,
                pressure: *pressure,
            }),
            _ => EventResponse::default(),
        }
    }

    #[inline]
    fn on_touch(&mut self, info: TouchInfo) -> EventResponse {
        let consumed = match info.phase {
            egui::TouchPhase::Start | egui::TouchPhase::End | egui::TouchPhase::Cancel => {
                self.egui_ctx.egui_wants_pointer_input()
            }
            egui::TouchPhase::Move => self.egui_ctx.egui_is_using_pointer(),
        };

        // SDL finger coordinates are normalized to the window (0.0..=1.0), unlike
        // mouse events which arrive in window coordinates. Scale them to the
        // window's pixel space so `pos_in_points` (which divides by ppp) yields
        // the right egui position — otherwise every touch maps to ~(0,0). Use the
        // *cached* window size so this numerator shares one size basis with the
        // cached ppp denominator below; mixing a live size here with the cached
        // ppp would misplace touches during a mid-resize transient.
        let (win_w, win_h) = self.window_size;
        let pixel_x = info.x * win_w as f32;
        let pixel_y = info.y * win_h as f32;
        let pos = self.pos_in_points(pixel_x, pixel_y);
        self.egui_input.events.push(egui::Event::Touch {
            device_id: egui::TouchDeviceId(info.touch_id as u64),
            id: egui::TouchId::from(info.finger_id as u64),
            phase: info.phase,
            pos,
            force: Some(info.pressure),
        });

        // egui's widget layer reacts to pointer events, not raw touch events, so
        // synthesize a primary-button pointer stream from the first finger (the
        // same thing egui-winit does). Without this, taps never click buttons on
        // platforms where the windowing layer doesn't synthesize mouse events from
        // touch (e.g. Android with SDL_TOUCH_MOUSE_EVENTS off). Extra fingers are
        // left to the multi-touch event above so they don't emit phantom presses.
        match info.phase {
            egui::TouchPhase::Start if self.pointer_touch_id.is_none() => {
                self.pointer_touch_id = Some(info.finger_id);
                self.pointer_pos_in_points = Some(pos);
                // Move to the press point first so egui has a current pointer pos.
                self.egui_input.events.push(egui::Event::PointerMoved(pos));
                self.egui_input.events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: self.modifiers,
                });
            }
            egui::TouchPhase::Move if self.pointer_touch_id == Some(info.finger_id) => {
                self.pointer_pos_in_points = Some(pos);
                self.egui_input.events.push(egui::Event::PointerMoved(pos));
            }
            egui::TouchPhase::End | egui::TouchPhase::Cancel
                if self.pointer_touch_id == Some(info.finger_id) =>
            {
                self.pointer_touch_id = None;
                if info.phase == egui::TouchPhase::End {
                    self.egui_input.events.push(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: self.modifiers,
                    });
                }
                // A touch pointer has no hover position once lifted; tell egui it's
                // gone so the next press starts a fresh interaction.
                self.egui_input.events.push(egui::Event::PointerGone);
                self.pointer_pos_in_points = None;
            }
            _ => {}
        }

        EventResponse {
            repaint: true,
            consumed,
        }
    }

    fn on_window_event(&mut self, event: WindowEvent, window: &Window) -> EventResponse {
        match event {
            WindowEvent::Minimized
            | WindowEvent::Maximized
            | WindowEvent::Resized(_, _)
            | WindowEvent::SizeChanged(_, _) => {
                self.on_size_chage(window);
                EventResponse {
                    repaint: true,
                    consumed: false,
                }
            }
            WindowEvent::Shown
            | WindowEvent::Hidden
            | WindowEvent::Exposed
            | WindowEvent::Moved(_, _)
            | WindowEvent::Restored
            | WindowEvent::Enter
            | WindowEvent::Close => EventResponse {
                consumed: false,
                repaint: true,
            },
            WindowEvent::Leave => {
                self.pointer_pos_in_points = None;
                self.egui_input.events.push(egui::Event::PointerGone);
                EventResponse {
                    repaint: true,
                    consumed: false,
                }
            }
            WindowEvent::TakeFocus | WindowEvent::FocusGained => {
                self.egui_input.focused = true;
                self.egui_input
                    .events
                    .push(egui::Event::WindowFocused(true));
                EventResponse {
                    repaint: true,
                    consumed: false,
                }
            }
            WindowEvent::FocusLost => {
                self.egui_input.focused = false;
                self.egui_input
                    .events
                    .push(egui::Event::WindowFocused(false));
                EventResponse {
                    repaint: true,
                    consumed: false,
                }
            }
            WindowEvent::HitTest
            | WindowEvent::ICCProfChanged
            | WindowEvent::DisplayChanged(_)
            | WindowEvent::None => EventResponse::default(),
        }
    }

    fn on_mouse_button_event(
        &mut self,
        button: MouseButton,
        pressed: bool,
        x: i32,
        y: i32,
    ) -> EventResponse {
        let Some(button) = into_egui_button(button) else {
            return EventResponse::default();
        };

        let pos = self.pos_in_points(x as f32, y as f32);
        self.pointer_pos_in_points = Some(pos);
        self.egui_input.events.push(egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: self.modifiers,
        });
        EventResponse {
            repaint: true,
            consumed: self.egui_ctx.egui_wants_pointer_input(),
        }
    }

    fn on_keyboard_event(
        &mut self,
        keycode: Keycode,
        scancode: Scancode,
        keymod: Mod,
        pressed: bool,
        repeat: bool,
    ) -> EventResponse {
        let Some(key) = into_egui_key(keycode) else {
            return EventResponse::default();
        };

        let modifiers = into_egui_modifiers(keymod);
        if modifiers != self.modifiers {
            self.modifiers = modifiers;
            // Before the key event, so egui reads the key under the new set.
            self.egui_input
                .events
                .push(egui::Event::ModifiersChanged(modifiers));
        }
        self.egui_input.events.push(egui::Event::Key {
            key,
            physical_key: into_egui_physical_key(scancode),
            pressed,
            repeat,
            modifiers: self.modifiers,
        });
        // When pressing the Tab key, egui focuses the first focusable element, hence Tab always consumes.
        let consumed = self.egui_ctx.egui_wants_keyboard_input() || key == Key::Tab;
        EventResponse {
            repaint: true,
            consumed,
        }
    }

    /// Refresh the cached window/drawable size and native pixels-per-point from
    /// the live window. Call once per frame so an orientation change is reflected
    /// even when the platform doesn't deliver a size-changed event (Android is
    /// unreliable here — the surface can resize on rotation without an event).
    #[inline]
    pub fn sync_window_size(&mut self, window: &Window) {
        self.on_size_chage(window);
    }

    #[inline]
    fn on_size_chage(&mut self, window: &Window) {
        self.window_size = window.size();
        self.drawable_size = window.drawable_size();
        self.egui_input.screen_rect = new_screen_rect(&self.egui_ctx, window, self.rotation);
        self.egui_input
            .viewports
            .entry(self.viewport_id)
            .or_default()
            .native_pixels_per_point = Some(native_pixels_per_point(window));
    }

    #[inline]
    fn set_cursor_icon(&mut self, cursor_icon: egui::CursorIcon) {
        if let Some(cursor) = &self.current_cursor {
            if cursor.icon == cursor_icon {
                return;
            }
        }

        if self.pointer_pos_in_points.is_some() {
            let system_cursor = into_sdl2_cursor(cursor_icon);
            let mut current_cursor = CurrentCursor {
                icon: cursor_icon,
                cursor: None,
            };

            match Cursor::from_system(system_cursor) {
                Ok(cursor) => {
                    cursor.set();
                    current_cursor.cursor = Some(cursor);
                }
                Err(e) => {
                    log::warn!("Failed to set cursor: {e}")
                }
            }
            self.current_cursor.replace(current_cursor);
        } else {
            self.current_cursor = None;
        }
    }
}

#[inline]
pub fn poiner_pos_in_points(
    egui_ctx: &egui::Context,
    window: &Window,
    x: f32,
    y: f32,
) -> egui::Pos2 {
    let pixels_per_point = pixels_per_point(egui_ctx, window);
    egui::pos2(x, y) / pixels_per_point
}

#[inline]
pub fn into_egui_modifiers(m: Mod) -> Modifiers {
    let mut mods = Modifiers::NONE;

    if m.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD) {
        mods.ctrl = true;
        mods.command = true;
    }

    if m.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD) {
        mods.shift = true;
    }

    if m.intersects(Mod::LALTMOD | Mod::RALTMOD) {
        mods.alt = true;
    }

    if m.intersects(Mod::LGUIMOD | Mod::RGUIMOD) {
        mods.mac_cmd = true;
        mods.command = true;
    }

    mods
}

#[inline]
fn into_sdl2_cursor(cursor_icon: egui::CursorIcon) -> SystemCursor {
    match cursor_icon {
        egui::CursorIcon::Crosshair => SystemCursor::Crosshair,
        egui::CursorIcon::Default => SystemCursor::Arrow,
        egui::CursorIcon::Grab => SystemCursor::Hand,
        egui::CursorIcon::Grabbing => SystemCursor::SizeAll,
        egui::CursorIcon::Move => SystemCursor::SizeAll,
        egui::CursorIcon::PointingHand => SystemCursor::Hand,
        egui::CursorIcon::ResizeHorizontal => SystemCursor::SizeWE,
        egui::CursorIcon::ResizeNeSw => SystemCursor::SizeNESW,
        egui::CursorIcon::ResizeNwSe => SystemCursor::SizeNWSE,
        egui::CursorIcon::ResizeVertical => SystemCursor::SizeNS,
        egui::CursorIcon::Text => SystemCursor::IBeam,
        egui::CursorIcon::NotAllowed | egui::CursorIcon::NoDrop => SystemCursor::No,
        egui::CursorIcon::Wait => SystemCursor::Wait,
        //There doesn't seem to be a suitable SDL equivalent...
        _ => SystemCursor::Arrow,
    }
}

#[inline]
pub fn screen_size_in_pixels(window: &Window) -> egui::Vec2 {
    let (width, height) = window.drawable_size();
    egui::vec2(width as f32, height as f32)
}

#[inline]
pub fn pixels_per_point(egui_ctx: &egui::Context, window: &Window) -> f32 {
    let native_pixels_per_point = native_pixels_per_point(window);
    let egui_zoom_factor = egui_ctx.zoom_factor();
    egui_zoom_factor * native_pixels_per_point
}

#[inline]
fn new_screen_rect(
    egui_ctx: &egui::Context,
    window: &Window,
    rotation: crate::Rotation,
) -> Option<Rect> {
    let screen_size_in_pixels = screen_size_in_pixels(window);
    let screen_size_in_points = screen_size_in_pixels / pixels_per_point(egui_ctx, window);

    (screen_size_in_points.x > 0.0 && screen_size_in_points.y > 0.0)
        .then(|| Rect::from_min_size(Pos2::ZERO, rotation.screen_size(screen_size_in_points)))
}

#[inline]
pub fn native_pixels_per_point(window: &Window) -> f32 {
    let (win_w, win_h) = window.size();
    let (draw_w, _draw_h) = window.drawable_size();

    if win_w > 0 && win_h > 0 {
        draw_w as f32 / win_w as f32
    } else {
        1.0
    }
}

#[inline]
pub fn into_egui_button(btn: MouseButton) -> Option<PointerButton> {
    match btn {
        MouseButton::Left => Some(egui::PointerButton::Primary),
        MouseButton::Middle => Some(egui::PointerButton::Middle),
        MouseButton::Right => Some(egui::PointerButton::Secondary),
        MouseButton::Unknown => None,
        MouseButton::X1 => Some(egui::PointerButton::Extra1),
        MouseButton::X2 => Some(egui::PointerButton::Extra2),
    }
}

pub fn into_egui_key(key: Keycode) -> Option<Key> {
    Some(match key {
        Keycode::Left => Key::ArrowLeft,
        Keycode::Up => Key::ArrowUp,
        Keycode::Right => Key::ArrowRight,
        Keycode::Down => Key::ArrowDown,

        Keycode::Escape => Key::Escape,
        Keycode::Tab => Key::Tab,
        Keycode::Backspace => Key::Backspace,
        Keycode::Space => Key::Space,
        Keycode::Return => Key::Enter,

        Keycode::Insert => Key::Insert,
        Keycode::Home => Key::Home,
        Keycode::Delete => Key::Delete,
        Keycode::End => Key::End,
        Keycode::PageDown => Key::PageDown,
        Keycode::PageUp => Key::PageUp,

        Keycode::Kp0 | Keycode::Num0 => Key::Num0,
        Keycode::Kp1 | Keycode::Num1 => Key::Num1,
        Keycode::Kp2 | Keycode::Num2 => Key::Num2,
        Keycode::Kp3 | Keycode::Num3 => Key::Num3,
        Keycode::Kp4 | Keycode::Num4 => Key::Num4,
        Keycode::Kp5 | Keycode::Num5 => Key::Num5,
        Keycode::Kp6 | Keycode::Num6 => Key::Num6,
        Keycode::Kp7 | Keycode::Num7 => Key::Num7,
        Keycode::Kp8 | Keycode::Num8 => Key::Num8,
        Keycode::Kp9 | Keycode::Num9 => Key::Num9,

        Keycode::A => Key::A,
        Keycode::B => Key::B,
        Keycode::C => Key::C,
        Keycode::D => Key::D,
        Keycode::E => Key::E,
        Keycode::F => Key::F,
        Keycode::G => Key::G,
        Keycode::H => Key::H,
        Keycode::I => Key::I,
        Keycode::J => Key::J,
        Keycode::K => Key::K,
        Keycode::L => Key::L,
        Keycode::M => Key::M,
        Keycode::N => Key::N,
        Keycode::O => Key::O,
        Keycode::P => Key::P,
        Keycode::Q => Key::Q,
        Keycode::R => Key::R,
        Keycode::S => Key::S,
        Keycode::T => Key::T,
        Keycode::U => Key::U,
        Keycode::V => Key::V,
        Keycode::W => Key::W,
        Keycode::X => Key::X,
        Keycode::Y => Key::Y,
        Keycode::Z => Key::Z,

        Keycode::F1 => Key::F1,
        Keycode::F2 => Key::F2,
        Keycode::F3 => Key::F3,
        Keycode::F4 => Key::F4,
        Keycode::F5 => Key::F5,
        Keycode::F6 => Key::F6,
        Keycode::F7 => Key::F7,
        Keycode::F8 => Key::F8,
        Keycode::F9 => Key::F9,
        Keycode::F10 => Key::F10,
        Keycode::F11 => Key::F11,
        Keycode::F12 => Key::F12,

        Keycode::Minus => Key::Minus,
        Keycode::Equals => Key::Equals,
        Keycode::Semicolon => Key::Semicolon,
        Keycode::Comma => Key::Comma,
        Keycode::Period => Key::Period,
        Keycode::Slash => Key::Slash,
        Keycode::Backslash => Key::Backslash,

        _ => {
            return None;
        }
    })
}

pub fn into_egui_physical_key(scancode: Scancode) -> Option<Key> {
    match scancode {
        Scancode::A => Some(Key::A),
        Scancode::B => Some(Key::B),
        Scancode::C => Some(Key::C),
        Scancode::D => Some(Key::D),
        Scancode::E => Some(Key::E),
        Scancode::F => Some(Key::F),
        Scancode::G => Some(Key::G),
        Scancode::H => Some(Key::H),
        Scancode::I => Some(Key::I),
        Scancode::J => Some(Key::J),
        Scancode::K => Some(Key::K),
        Scancode::L => Some(Key::L),
        Scancode::M => Some(Key::M),
        Scancode::N => Some(Key::N),
        Scancode::O => Some(Key::O),
        Scancode::P => Some(Key::P),
        Scancode::Q => Some(Key::Q),
        Scancode::R => Some(Key::R),
        Scancode::S => Some(Key::S),
        Scancode::T => Some(Key::T),
        Scancode::U => Some(Key::U),
        Scancode::V => Some(Key::V),
        Scancode::W => Some(Key::W),
        Scancode::X => Some(Key::X),
        Scancode::Y => Some(Key::Y),
        Scancode::Z => Some(Key::Z),

        Scancode::Num0 => Some(Key::Num0),
        Scancode::Num1 => Some(Key::Num1),
        Scancode::Num2 => Some(Key::Num2),
        Scancode::Num3 => Some(Key::Num3),
        Scancode::Num4 => Some(Key::Num4),
        Scancode::Num5 => Some(Key::Num5),
        Scancode::Num6 => Some(Key::Num6),
        Scancode::Num7 => Some(Key::Num7),
        Scancode::Num8 => Some(Key::Num8),
        Scancode::Num9 => Some(Key::Num9),

        Scancode::F1 => Some(Key::F1),
        Scancode::F2 => Some(Key::F2),
        Scancode::F3 => Some(Key::F3),
        Scancode::F4 => Some(Key::F4),
        Scancode::F5 => Some(Key::F5),
        Scancode::F6 => Some(Key::F6),
        Scancode::F7 => Some(Key::F7),
        Scancode::F8 => Some(Key::F8),
        Scancode::F9 => Some(Key::F9),
        Scancode::F10 => Some(Key::F10),
        Scancode::F11 => Some(Key::F11),
        Scancode::F12 => Some(Key::F12),

        Scancode::Up => Some(Key::ArrowUp),
        Scancode::Down => Some(Key::ArrowDown),
        Scancode::Left => Some(Key::ArrowLeft),
        Scancode::Right => Some(Key::ArrowRight),

        Scancode::Return => Some(Key::Enter),
        Scancode::Escape => Some(Key::Escape),
        Scancode::Backspace => Some(Key::Backspace),
        Scancode::Tab => Some(Key::Tab),
        Scancode::Space => Some(Key::Space),

        _ => None,
    }
}

struct TouchInfo {
    phase: egui::TouchPhase,
    touch_id: i64,
    finger_id: i64,
    x: f32,
    y: f32,
    pressure: f32,
}
