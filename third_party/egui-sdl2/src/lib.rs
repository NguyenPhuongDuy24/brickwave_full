//! # egui-sdl2
//!
//! Integration between [`egui`](https://github.com/emilk/egui) and
//! [`sdl2`](https://github.com/Rust-SDL2/rust-sdl2).
//!
//! ## Features
//! - Translate SDL2 events into [`egui`] events.
//! - Handle [`egui::PlatformOutput`] (clipboard, cursor updates, links).
//! - Render with the SDL2 software renderer via [`sdl2::render::Canvas`] (`canvas-backend` feature).
//! - Render with OpenGL via [`glow`] (`glow-backend` feature).
//! - Render with WebGPU via [`wgpu`](https://github.com/gfx-rs/wgpu) (`wgpu-backend` feature).
//! - Or let [`EguiWindow`] pick: it walks a list of renderers and keeps the
//!   first the device actually supports.
//!
//! ## Usage
//! ```no_run
//! // Create SDL2 window and canvas:
//! let sdl = sdl2::init().unwrap();
//! let video = sdl.video().unwrap();
//! let window = video.window("Egui SDL2 Canvas", 800, 600).build().unwrap();
//! let mut canvas = window.into_canvas().build().unwrap();
//! // Create egui renderer; the canvas stays yours:
//! let mut egui = egui_sdl2::EguiCanvas::new(&canvas);
//! let mut event_pump = sdl.event_pump().unwrap();
//! loop {
//!    // Feed SDL2 events into egui:
//!    for event in event_pump.poll_iter() {
//!        egui.on_event(&canvas, &event);
//!    }
//!    // Call `run` + `paint` each frame, over anything you drew yourself:
//!    egui.run(|ctx: &egui::Context| {});
//!    canvas.clear();
//!    egui.paint(&mut canvas);
//!    canvas.present();
//!    std::thread::sleep(std::time::Duration::from_secs_f64(1.0 / 60.0));
//!}
//! ```

pub use egui;
#[cfg(feature = "glow-backend")]
pub use egui_glow;
pub use sdl2;

#[cfg(feature = "canvas-backend")]
pub mod canvas;
#[cfg(feature = "glow-backend")]
pub mod glow;
pub mod rotation;
pub mod state;
#[cfg(feature = "wgpu-backend")]
pub mod wgpu;
#[cfg(any(
    feature = "glow-backend",
    feature = "canvas-backend",
    feature = "wgpu-backend"
))]
pub mod window;

#[cfg(feature = "canvas-backend")]
pub use canvas::EguiCanvas;
#[cfg(feature = "glow-backend")]
pub use glow::*;
pub use rotation::Rotation;
pub use state::*;
#[cfg(feature = "wgpu-backend")]
pub use wgpu::EguiWgpu;
#[cfg(any(
    feature = "glow-backend",
    feature = "canvas-backend",
    feature = "wgpu-backend"
))]
pub use window::{EguiWindow, Renderer};

/// The results of running one frame of `egui`.
///
/// `EguiRunOutput` collects the renderable shapes, texture updates, and scale
/// factor from a single `egui` run. It also provides convenience methods for
/// updating its contents from an `egui::Context` and for draining the data
/// when it is time to render.
///
/// This is typically created once per backend instance and reused across frames.
pub struct EguiRunOutput {
    /// The clipped shapes that should be rendered for the current frame.
    ///
    /// This is produced by egui’s tessellation step and represents what should
    /// be drawn to the screen.
    pub shapes: Vec<egui::epaint::ClippedShape>,

    /// The logical-to-physical pixel scaling factor used by egui in this frame.
    ///
    /// Backends should respect this when converting coordinates to pixels.
    pub pixels_per_point: f32,

    /// The delta of texture updates required for this frame.
    ///
    /// Includes new textures to upload and old textures to free.
    pub textures_delta: egui::TexturesDelta,

    /// How long until egui wants the ROOT viewport repainted, as reported by
    /// the last [`Self::update`] (egui's `ViewportOutput::repaint_delay`).
    ///
    /// `Duration::ZERO` means egui needs another frame *immediately* — e.g. the
    /// frame just run was a sizing pass for a freshly shown [`egui::Area`], whose
    /// real (positioned, visible) frame must follow. `Duration::MAX` means egui
    /// is idle and the backend may block on input. Event-driven backends should
    /// fold this into their idle wait so animations and first-frame layout show
    /// without an extra input event.
    pub repaint_delay: std::time::Duration,
}

impl Default for EguiRunOutput {
    /// Creates an empty `EguiRunOutput` with no shapes, no texture updates,
    /// and a scale factor of `1.0`.
    fn default() -> Self {
        Self {
            shapes: Default::default(),
            pixels_per_point: 1.0,
            textures_delta: Default::default(),
            repaint_delay: std::time::Duration::MAX,
        }
    }
}

impl Drop for EguiRunOutput {
    fn drop(&mut self) {
        // An app may quit between `run` and `paint`; deltas never painted are
        // dropped on purpose, which egui 0.36's own drop check asserts against.
        self.textures_delta.clear();
    }
}

impl EguiRunOutput {
    /// Run `egui` for one frame and update this output with the results.
    ///
    /// # Parameters
    /// - `ctx`: The [`egui::Context`] used to run the UI.
    /// - `state`: A backend state that provides input for egui and
    ///   handles platform output (clipboard, cursor, etc.).
    /// - `run_ui`: A closure that builds the UI using the given `egui::Context`.
    ///
    /// # Behavior
    /// - Takes input events from `state`.
    /// - Runs egui with the provided `run_ui` closure.
    /// - Handles platform output via `state`.
    /// - Stores the frame’s shapes, texture updates, and scale factor
    ///   in this `EguiRunOutput`.
    #[inline]
    pub fn update(
        &mut self,
        ctx: &egui::Context,
        state: &mut State,
        mut run_ui: impl FnMut(&egui::Context),
    ) {
        self.update_ui(ctx, state, |ui| run_ui(ui.ctx()));
    }

    /// Like [`Self::update`], but hands the closure egui's root [`egui::Ui`].
    ///
    /// Panels ([`egui::CentralPanel`], [`egui::TopBottomPanel`], …) are shown into
    /// a `Ui`, so a full-screen layout needs this rather than [`Self::update`].
    #[inline]
    pub fn update_ui(
        &mut self,
        ctx: &egui::Context,
        state: &mut State,
        run_ui: impl FnMut(&mut egui::Ui),
    ) {
        let raw_input = state.take_egui_input();
        let egui::FullOutput {
            platform_output,
            viewport_output,
            textures_delta,
            shapes,
            pixels_per_point,
        } = ctx.run_ui(raw_input, run_ui);
        state.handle_platform_output(platform_output);

        self.shapes = shapes;
        self.textures_delta.append(textures_delta);
        self.pixels_per_point = pixels_per_point;
        // Surface egui's own repaint timing for the ROOT viewport so event-driven
        // backends don't drop it: a sizing pass for a freshly shown anchored Area
        // reports `ZERO` here, asking for the follow-up frame that actually paints
        // it. `MAX` if egui didn't report (idle — wait on input).
        self.repaint_delay = viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(std::time::Duration::MAX, |v| v.repaint_delay);
    }

    /// Take ownership of the texture updates and shapes for the current frame.
    ///
    /// This clears both fields in the struct, leaving them empty for the next frame.
    ///
    /// # Returns
    /// - `(textures_delta, shapes)` where:
    ///   - `textures_delta`: The [`egui::TexturesDelta`] with texture uploads/free requests.
    ///   - `shapes`: The tessellated shapes that should be rendered.
    #[inline]
    pub fn take(&mut self) -> (egui::TexturesDelta, Vec<egui::epaint::ClippedShape>) {
        let textures_delta = std::mem::take(&mut self.textures_delta);
        let shapes = std::mem::take(&mut self.shapes);

        (textures_delta, shapes)
    }
}
