//! Integration between [`egui`] and [`wgpu`](https://docs.rs/wgpu) API.
//!
//! This module provides [`EguiWgpu`], a convenience wrapper that bundles
//! together:
//! - [`egui::Context`] for running your UI
//! - [`crate::State`] for event and input handling
//! - [`Painter`] for rendering using [`wgpu`](https://docs.rs/wgpu)

//! # Usage
//! Typical usage is to:
//! 1. Create an [`EguiWgpu`] for your SDL2 window
//! 2. Pass SDL2 events to [`EguiWgpu::on_event`]
//! 3. Call [`EguiWgpu::run`] providing our UI function
//! 4. Paint egui output via [`EguiWgpu::paint`]
//!

use egui_wgpu::wgpu::rwh::{DisplayHandle, HandleError, HasDisplayHandle, RawDisplayHandle};
use std::num::NonZeroU32;
pub mod painter;
pub use painter::*;

/// An owned, `'static` clone of an SDL2 window's display handle.
///
/// Since wgpu 29 / egui-wgpu 0.34 the wgpu instance must be created with a display handle
/// on some platforms (e.g. Wayland on Linux), otherwise surface creation fails with
/// `MissingDisplayHandle`. [`egui_wgpu::WgpuSetup::from_display_handle`] requires an owned
/// `Send + Sync + 'static` handle, which the borrowed `DisplayHandle<'_>` from SDL2 is not.
#[derive(Clone, Copy, Debug)]
struct SdlDisplayHandle(RawDisplayHandle);

// SAFETY: a `RawDisplayHandle` is a plain pointer/id into the SDL2 video subsystem, which
// stays alive for as long as the window we render to. We never mutate through it.
unsafe impl Send for SdlDisplayHandle {}
unsafe impl Sync for SdlDisplayHandle {}

impl HasDisplayHandle for SdlDisplayHandle {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        // SAFETY: see the `Send`/`Sync` justification above.
        Ok(unsafe { DisplayHandle::borrow_raw(self.0) })
    }
}

/// Integration between [`egui`] and [`wgpu`](https://docs.rs/wgpu) for app based on [`sdl2`].
pub struct EguiWgpu {
    run_output: crate::EguiRunOutput,
    viewport_id: egui::ViewportId,
    pub ctx: egui::Context,
    pub state: crate::State,
    pub painter: painter::Painter,
    pub window: sdl2::video::Window,
}

impl EguiWgpu {
    pub async fn new(window: sdl2::video::Window) -> Self {
        let ctx = egui::Context::default();
        let viewport_id = egui::ViewportId::ROOT;
        let state = crate::State::new(&window, ctx.clone(), viewport_id);
        let run_output = crate::EguiRunOutput::default();
        let raw_display_handle = window
            .display_handle()
            .expect("SDL2 window must expose a display handle")
            .as_raw();
        let config = egui_wgpu::WgpuConfiguration {
            wgpu_setup: egui_wgpu::WgpuSetup::from_display_handle(SdlDisplayHandle(
                raw_display_handle,
            )),
            ..Default::default()
        };
        let mut painter = painter::Painter::new(ctx.clone(), config, 1, None, true, false).await;
        // SAFETY:
        // Window lives as long as self
        unsafe {
            painter.set_window(viewport_id, &window).await.unwrap();
        }

        Self {
            window,
            ctx,
            painter,
            state,
            run_output,
            viewport_id,
        }
    }

    #[inline]
    pub fn on_event(&mut self, event: &sdl2::event::Event) -> crate::EventResponse {
        match event {
            sdl2::event::Event::Window {
                window_id,
                win_event:
                    sdl2::event::WindowEvent::Resized(w, h)
                    | sdl2::event::WindowEvent::SizeChanged(w, h),
                ..
            } if *window_id == self.window.id() && *w > 0 && *h > 0 => {
                let w = NonZeroU32::new(*w as u32).unwrap();
                let h = NonZeroU32::new(*h as u32).unwrap();
                self.painter.on_window_resized(self.viewport_id, w, h);
            }
            _ => {}
        }

        self.state.on_event(&self.window, event)
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

    /// Paint the results of the last call to [`Self::run`].
    pub fn paint(&mut self, clear_color: [f32; 4]) {
        let pixels_per_point = self.run_output.pixels_per_point;
        let (mut textures_delta, shapes) = self.run_output.take();
        let mut clipped_primitives = self.ctx.tessellate(shapes, pixels_per_point);
        // A turned frame was laid out for the screen the other way round; the
        // surface is still the window, so bring the geometry back to it.
        let rotation = self.state.rotation();
        if rotation != crate::Rotation::None && pixels_per_point > 0.0 {
            let size = self.state.get_drawable_size();
            let window = egui::vec2(size.0 as f32, size.1 as f32) / pixels_per_point;
            rotation.turn_primitives(&mut clipped_primitives, window);
        }
        self.painter.paint_and_update_textures(
            self.viewport_id,
            pixels_per_point,
            clear_color,
            &clipped_primitives,
            &mut textures_delta,
            Vec::with_capacity(0),
        );
    }
}
