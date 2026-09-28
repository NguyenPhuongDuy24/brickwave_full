//! Integration between [`egui`] and SDL2’s [`sdl2::render::Canvas`] API.
//!
//! This module provides [`EguiCanvas`], a convenience wrapper that bundles
//! together:
//! - [`egui::Context`] for running your UI
//! - [`crate::State`] for event and input handling
//! - [`Painter`] for rendering using [`sdl2::render::Canvas`]
//!
//! # When to use
//! Use [`EguiCanvas`] if you want to render egui using SDL2’s 2D canvas API
//! instead of OpenGL.
//!
//! # Usage
//! The [`Canvas`] stays owned by you: build it however you like, then pass it to
//! the calls that need it. Typical usage is to:
//! 1. Create an [`EguiCanvas`] for your SDL2 canvas
//! 2. Pass SDL2 events to [`EguiCanvas::on_event`]
//! 3. Call [`EguiCanvas::run`] providing our UI function
//! 4. Paint egui output over whatever you drew via [`EguiCanvas::paint`]
//!
pub mod painter;
pub use painter::*;

use sdl2::render::{Canvas, RenderTarget};
use sdl2::surface::{Surface, SurfaceContext};
use sdl2::video::{Window, WindowContext};

/// Integration between [`egui`] and [`sdl2::render::Canvas`] for app based on [`sdl2`].
///
/// `C` is the render target's context: a window's by default, a surface's when
/// painting offscreen (see [`EguiCanvas::for_target`]).
pub struct EguiCanvas<C = WindowContext> {
    /// The last [`Self::run`]'s output, for a caller that paints it in steps of
    /// its own — timing them apart, or holding a rasterized frame back to reuse.
    /// [`Self::paint`] does the whole of it in one call.
    pub run_output: crate::EguiRunOutput,
    pub ctx: egui::Context,
    pub state: crate::State,
    pub painter: Painter<C>,
}

impl EguiCanvas<WindowContext> {
    /// Pass the same `canvas` to [`Self::on_event`] and [`Self::paint`].
    pub fn new(canvas: &Canvas<Window>) -> Self {
        Self::assemble(canvas.window(), Painter::new(canvas))
    }

    /// [`Self::new`] painting in `format` (see [`Painter::with_format`]).
    pub fn with_format(canvas: &Canvas<Window>, format: sdl2::pixels::PixelFormatEnum) -> Self {
        Self::assemble(canvas.window(), Painter::with_format(canvas, format))
    }

    #[inline]
    pub fn on_event(
        &mut self,
        canvas: &Canvas<Window>,
        event: &sdl2::event::Event,
    ) -> crate::EventResponse {
        self.state.on_event(canvas.window(), event)
    }
}

impl<'s> EguiCanvas<SurfaceContext<'s>> {
    /// Paint into a surface rather than the window, for drivers that only present
    /// texture copies. Input and sizing still come from `window`.
    pub fn for_surface(window: &Window, canvas: &Canvas<Surface<'s>>) -> Self {
        Self::assemble(window, Painter::for_surface(canvas))
    }

    /// [`Self::for_surface`] painting in `format` (see
    /// [`Painter::for_surface_with_format`]).
    pub fn for_surface_with_format(
        window: &Window,
        canvas: &Canvas<Surface<'s>>,
        format: sdl2::pixels::PixelFormatEnum,
    ) -> Self {
        Self::assemble(window, Painter::for_surface_with_format(canvas, format))
    }
}

impl<C> EguiCanvas<C> {
    fn assemble(window: &Window, painter: Painter<C>) -> Self {
        let ctx = egui::Context::default();
        // Feathering stays on: its hue-less transparent ends are given their
        // path's hue in `Painter::flush_triangles`, which makes SDL's
        // straight-alpha interpolation a correct antialiased fade.
        let mut state = crate::State::new(window, ctx.clone(), egui::ViewportId::ROOT);
        state.set_max_texture_side(painter.max_texture_side());

        Self {
            ctx,
            painter,
            state,
            run_output: crate::EguiRunOutput::default(),
        }
    }

    /// [`EguiCanvas::on_event`] for an offscreen canvas, which has no window of
    /// its own.
    #[inline]
    pub fn on_window_event(
        &mut self,
        window: &Window,
        event: &sdl2::event::Event,
    ) -> crate::EventResponse {
        self.state.on_event(window, event)
    }

    /// Call [`Self::paint`] later to paint.
    #[inline]
    pub fn run(&mut self, run_ui: impl FnMut(&egui::Context)) {
        self.run_output.update(&self.ctx, &mut self.state, run_ui);
    }

    /// Like [`Self::run`], but hands the closure egui's root [`egui::Ui`], which
    /// is what panels are shown into.
    #[inline]
    pub fn run_ui(&mut self, run_ui: impl FnMut(&mut egui::Ui)) {
        self.run_output
            .update_ui(&self.ctx, &mut self.state, run_ui);
    }

    /// How long until egui wants another frame, from the last [`Self::run`]
    /// (see [`crate::EguiRunOutput::repaint_delay`]).
    #[inline]
    pub fn repaint_delay(&self) -> std::time::Duration {
        self.run_output.repaint_delay
    }

    /// Paint the results of the last call to [`Self::run`]. Clear the canvas (and
    /// draw your own content) beforehand; present it afterwards.
    pub fn paint<T: RenderTarget<Context = C>>(&mut self, canvas: &mut Canvas<T>) {
        let pixels_per_point = self.run_output.pixels_per_point;
        let (mut textures_delta, shapes) = self.run_output.take();
        let clipped_primitives = self.ctx.tessellate(shapes, pixels_per_point);
        if let Err(e) = self.painter.paint_and_update_textures(
            canvas,
            pixels_per_point,
            &mut textures_delta,
            clipped_primitives,
        ) {
            log::error!("Failed to paint: {e}");
        }
    }

    /// Call to release the allocated graphics resources.
    pub fn destroy(&mut self) {
        self.painter.destroy();
    }
}
