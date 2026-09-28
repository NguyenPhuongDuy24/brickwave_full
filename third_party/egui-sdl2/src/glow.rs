//! Integration between [`egui`] and [`glow`] for SDL2 applications.
//!
//! This module provides [`EguiGlow`], a convenience wrapper that bundles
//! together:
//! - [`egui::Context`] for running your UI
//! - [`crate::State`] for event/input handling
//! - [`egui_glow::Painter`] for rendering with OpenGL (via [`glow`])
//!
//! # When to use
//! Use [`EguiGlow`] if you want to render egui using OpenGL through glow
//! in an SDL2 application. If you prefer SDL2’s `Canvas` renderer, see the
//! [`crate::canvas`] module instead.
//!
//! # Usage
//! Typical usage is to:
//! 1. Create an [`EguiGlow`] for your SDL2 window and GL context
//! 2. Pass SDL2 events to [`EguiGlow::on_event`]
//! 3. Call [`EguiGlow::run`] providing your UI function
//! 4. Paint egui output via [`EguiGlow::paint`]
//!

/// Integration between [`egui`] and [`glow`] for app based on [`sdl2`].
pub struct EguiGlow {
    run_output: crate::EguiRunOutput,
    pub ctx: egui::Context,
    pub state: crate::State,
    pub painter: egui_glow::Painter,
}

impl EguiGlow {
    /// For automatic shader version detection set `shader_version` to `None`.
    pub fn new(
        window: &sdl2::video::Window,
        glow_ctx: std::sync::Arc<glow::Context>,
        shader_version: Option<egui_glow::ShaderVersion>,
        dithering: bool,
    ) -> Self {
        let painter = egui_glow::Painter::new(glow_ctx, "", shader_version, dithering)
            .map_err(|err| {
                log::error!("error occurred in initializing painter:\n{err}");
            })
            .unwrap();
        let ctx = egui::Context::default();
        let state = crate::State::new(window, ctx.clone(), egui::ViewportId::ROOT);
        let run_output = crate::EguiRunOutput::default();

        Self {
            painter,
            run_output,
            state,
            ctx,
        }
    }

    #[inline]
    pub fn on_event(
        &mut self,
        window: &sdl2::video::Window,
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
    /// (see [`crate::EguiRunOutput::repaint_delay`]). `ZERO` means repaint now
    /// (e.g. a freshly shown anchored `Area`'s sizing pass needs its follow-up
    /// frame); `Duration::MAX` means egui is idle. Event-driven loops should
    /// fold this into their idle wait.
    #[inline]
    pub fn repaint_delay(&self) -> std::time::Duration {
        self.run_output.repaint_delay
    }

    /// Paint the results of the last call to [`Self::run`].
    pub fn paint(&mut self) {
        let pixels_per_point = self.run_output.pixels_per_point;
        let (mut textures_delta, shapes) = self.run_output.take();
        let mut clipped_primitives = self.ctx.tessellate(shapes, pixels_per_point);
        // egui laid out for the drawable (physical) size and the GL viewport
        // covers the physical framebuffer, so pass drawable size — not the
        // logical window size — or content is clipped/scaled wrong on HiDPI.
        let screen_size = self.state.get_drawable_size();
        // The viewport is the window; a turned frame was laid out for the screen
        // the other way round, so its geometry comes back here. GL draws the
        // turned triangles at no cost, which is why this backend needs no
        // offscreen pass.
        let rotation = self.state.rotation();
        if rotation != crate::Rotation::None && pixels_per_point > 0.0 {
            let window = egui::vec2(screen_size.0 as f32, screen_size.1 as f32) / pixels_per_point;
            rotation.turn_primitives(&mut clipped_primitives, window);
        }
        self.painter.paint_and_update_textures(
            screen_size.into(),
            pixels_per_point,
            &clipped_primitives,
            // egui_glow 0.36 drains the deltas in place.
            &mut textures_delta,
        );
    }

    #[inline]
    pub fn clear(&self, color: [f32; 4]) {
        // Physical framebuffer size, matching the viewport used in `paint`.
        let size = self.state.get_drawable_size();
        self.painter.clear(size.into(), color);
    }

    /// Call to release the allocated graphics resources.
    pub fn destroy(&mut self) {
        self.painter.destroy();
    }
}
