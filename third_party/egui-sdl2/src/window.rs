//! A window plus the best renderer it can get: [`EguiWindow`] walks a list of
//! [`Renderer`]s and keeps the first that comes up, so an app on a device with
//! missing or broken GL drivers still shows a UI instead of exiting.
//!
//! The window is owned here because the attempts need different ones — GL sets
//! flags on the [`WindowBuilder`], wgpu takes the window by value — and a
//! builder cannot be reused. Pass the title and size; `configure` runs on a
//! fresh builder per attempt, for whatever else the window needs.
//!
//! ```no_run
//! let sdl = sdl2::init().unwrap();
//! let video = sdl.video().unwrap();
//! let mut egui = egui_sdl2::EguiWindow::new(
//!     &video,
//!     "Egui SDL2",
//!     (800, 600),
//!     |builder| {
//!         builder.resizable();
//!     },
//!     &egui_sdl2::Renderer::FALLBACK_CHAIN,
//! )
//! .unwrap();
//! let mut event_pump = sdl.event_pump().unwrap();
//! loop {
//!     for event in event_pump.poll_iter() {
//!         egui.on_event(&event);
//!     }
//!     egui.run(|ctx: &egui::Context| {});
//!     egui.paint([0.1, 0.1, 0.1, 1.0]);
//! }
//! ```

#[cfg(feature = "canvas-backend")]
use crate::canvas::painter::BYTES_PER_PIXEL;
use crate::Rotation;
use sdl2::event::Event;
use sdl2::video::{Window, WindowBuilder};
use sdl2::VideoSubsystem;

/// A way to put egui on screen. Apps list the ones they accept, best first.
#[non_exhaustive]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Renderer {
    /// OpenGL ES 3.0 through [`glow`](https://crates.io/crates/glow) — what
    /// mobile and handheld GPU blobs expose.
    Gles3,
    /// Desktop OpenGL 3.2 core through glow.
    Gl32,
    /// SDL's own 2D renderer: an accelerated driver when SDL finds one, its
    /// software rasterizer otherwise. The one that needs no GL at all.
    Canvas,
    /// [`Self::Canvas`] rasterized offscreen, presented as one texture copy per
    /// frame — for drivers that show nothing else, like the Miyoo Mini's
    /// `mmiyoo`. Costs an upload per frame.
    CanvasBlit,
    /// wgpu (`wgpu-backend` feature).
    Wgpu,
}

impl Renderer {
    /// GL first, SDL's renderer as the safety net.
    pub const FALLBACK_CHAIN: [Renderer; 3] = [Renderer::Gles3, Renderer::Gl32, Renderer::Canvas];

    fn name(self) -> &'static str {
        match self {
            Renderer::Gles3 => "GLES 3.0",
            Renderer::Gl32 => "GL 3.2 core",
            Renderer::Canvas => "SDL renderer",
            Renderer::CanvasBlit => "SDL renderer (offscreen blit)",
            Renderer::Wgpu => "wgpu",
        }
    }
}

enum Backend {
    #[cfg(feature = "glow-backend")]
    Glow {
        window: Window,
        // Kept alive for the lifetime of the window; dropping it destroys the context.
        _gl_context: sdl2::video::GLContext,
        egui: crate::EguiGlow,
    },
    #[cfg(feature = "canvas-backend")]
    Canvas {
        canvas: sdl2::render::WindowCanvas,
        egui: crate::EguiCanvas,
        /// Built the first time a turn is presented, and only then: an unturned
        /// frame goes straight to the window as it always did.
        turned: Option<TurnedTarget>,
    },
    #[cfg(feature = "canvas-backend")]
    CanvasBlit {
        canvas: sdl2::render::WindowCanvas,
        /// egui is drawn here, by SDL's software renderer.
        offscreen: sdl2::render::Canvas<sdl2::surface::Surface<'static>>,
        /// The offscreen pixels, uploaded and copied once per frame.
        present: sdl2::render::Texture,
        /// Window size `offscreen` and `present` were built for.
        size: (u32, u32),
        /// The turned copy of the offscreen frame, reused across frames.
        frame: Vec<u8>,
        egui: crate::EguiCanvas<sdl2::surface::SurfaceContext<'static>>,
    },
    // Boxed: an EguiWgpu is twice the size of the other variants.
    #[cfg(feature = "wgpu-backend")]
    Wgpu { egui: Box<crate::EguiWgpu> },
}

/// egui and the window it draws into, over whichever renderer was available.
/// Vsync is on; every backend clears, paints and presents in [`Self::paint`].
pub struct EguiWindow {
    backend: Backend,
    renderer: Renderer,
}

impl EguiWindow {
    /// Try `order` in sequence, returning the first renderer that comes up.
    /// The error is the last attempt's, since that is the one that decided it.
    pub fn new(
        video: &VideoSubsystem,
        title: &str,
        size: (u32, u32),
        configure: impl Fn(&mut WindowBuilder),
        order: &[Renderer],
    ) -> Result<Self, String> {
        let make_window = |video: &VideoSubsystem, gl: bool| {
            let mut builder = video.window(title, size.0, size.1);
            if gl {
                builder.opengl();
            }
            configure(&mut builder);
            builder.build().map_err(|e| e.to_string())
        };
        let mut last = "no renderer requested".to_string();
        for &renderer in order {
            match build(video, &make_window, renderer) {
                Ok(backend) => {
                    log::info!("egui renderer: {}", renderer.name());
                    return Ok(Self { backend, renderer });
                }
                Err(e) => {
                    log::warn!("{} unavailable: {e}", renderer.name());
                    last = e;
                }
            }
        }
        Err(last)
    }

    /// Which one won, for the app's own logging and about screens.
    pub fn renderer(&self) -> Renderer {
        self.renderer
    }

    /// Present the UI at a quarter turn to the window, for a panel that is not
    /// mounted the way it is read.
    ///
    /// egui lays out for the turned screen — a quarter turn trades the window's
    /// width and height — and this window puts the frame back on the panel:
    /// [`Renderer::Gles3`] and [`Renderer::Gl32`] turn the geometry as they draw
    /// it, the SDL renderers paint offscreen and present that turned. Pointer
    /// and touch positions travel back the same way, so a tap lands where it
    /// looks. May be called at any time; nothing is rebuilt on a change of turn.
    pub fn set_rotation(&mut self, rotation: Rotation) {
        match &mut self.backend {
            #[cfg(feature = "glow-backend")]
            Backend::Glow { egui, .. } => egui.state.set_rotation(rotation),
            #[cfg(feature = "canvas-backend")]
            Backend::Canvas { egui, .. } => egui.state.set_rotation(rotation),
            #[cfg(feature = "canvas-backend")]
            Backend::CanvasBlit { egui, .. } => egui.state.set_rotation(rotation),
            #[cfg(feature = "wgpu-backend")]
            Backend::Wgpu { egui } => egui.state.set_rotation(rotation),
        }
    }

    pub fn rotation(&self) -> Rotation {
        match &self.backend {
            #[cfg(feature = "glow-backend")]
            Backend::Glow { egui, .. } => egui.state.rotation(),
            #[cfg(feature = "canvas-backend")]
            Backend::Canvas { egui, .. } => egui.state.rotation(),
            #[cfg(feature = "canvas-backend")]
            Backend::CanvasBlit { egui, .. } => egui.state.rotation(),
            #[cfg(feature = "wgpu-backend")]
            Backend::Wgpu { egui } => egui.state.rotation(),
        }
    }

    pub fn ctx(&self) -> &egui::Context {
        match &self.backend {
            #[cfg(feature = "glow-backend")]
            Backend::Glow { egui, .. } => &egui.ctx,
            #[cfg(feature = "canvas-backend")]
            Backend::Canvas { egui, .. } => &egui.ctx,
            #[cfg(feature = "canvas-backend")]
            Backend::CanvasBlit { egui, .. } => &egui.ctx,
            #[cfg(feature = "wgpu-backend")]
            Backend::Wgpu { egui } => &egui.ctx,
        }
    }

    pub fn window(&self) -> &Window {
        match &self.backend {
            #[cfg(feature = "glow-backend")]
            Backend::Glow { window, .. } => window,
            #[cfg(feature = "canvas-backend")]
            Backend::Canvas { canvas, .. } => canvas.window(),
            #[cfg(feature = "canvas-backend")]
            Backend::CanvasBlit { canvas, .. } => canvas.window(),
            #[cfg(feature = "wgpu-backend")]
            Backend::Wgpu { egui } => &egui.window,
        }
    }

    /// For the things SDL only does through the window itself, like
    /// [`Window::set_icon`].
    pub fn window_mut(&mut self) -> &mut Window {
        match &mut self.backend {
            #[cfg(feature = "glow-backend")]
            Backend::Glow { window, .. } => window,
            #[cfg(feature = "canvas-backend")]
            Backend::Canvas { canvas, .. } => canvas.window_mut(),
            #[cfg(feature = "canvas-backend")]
            Backend::CanvasBlit { canvas, .. } => canvas.window_mut(),
            #[cfg(feature = "wgpu-backend")]
            Backend::Wgpu { egui } => &mut egui.window,
        }
    }

    /// Feed an SDL event to egui; wgpu also resizes its surface here.
    pub fn on_event(&mut self, event: &Event) -> crate::EventResponse {
        match &mut self.backend {
            #[cfg(feature = "glow-backend")]
            Backend::Glow { window, egui, .. } => egui.state.on_event(window, event),
            #[cfg(feature = "canvas-backend")]
            Backend::Canvas { canvas, egui, .. } => egui.state.on_event(canvas.window(), event),
            #[cfg(feature = "canvas-backend")]
            Backend::CanvasBlit { canvas, egui, .. } => egui.state.on_event(canvas.window(), event),
            #[cfg(feature = "wgpu-backend")]
            Backend::Wgpu { egui } => egui.on_event(event),
        }
    }

    /// Run the UI; [`Self::paint`] puts the result on screen.
    pub fn run(&mut self, run_ui: impl FnMut(&egui::Context)) {
        match &mut self.backend {
            #[cfg(feature = "glow-backend")]
            Backend::Glow { egui, .. } => egui.run(run_ui),
            #[cfg(feature = "canvas-backend")]
            Backend::Canvas { egui, .. } => egui.run(run_ui),
            #[cfg(feature = "canvas-backend")]
            Backend::CanvasBlit { egui, .. } => egui.run(run_ui),
            #[cfg(feature = "wgpu-backend")]
            Backend::Wgpu { egui } => egui.run(run_ui),
        }
    }

    /// Like [`Self::run`], but hands the closure egui's root [`egui::Ui`].
    pub fn run_ui(&mut self, run_ui: impl FnMut(&mut egui::Ui)) {
        match &mut self.backend {
            #[cfg(feature = "glow-backend")]
            Backend::Glow { egui, .. } => egui.run_ui(run_ui),
            #[cfg(feature = "canvas-backend")]
            Backend::Canvas { egui, .. } => egui.run_ui(run_ui),
            #[cfg(feature = "canvas-backend")]
            Backend::CanvasBlit { egui, .. } => egui.run_ui(run_ui),
            #[cfg(feature = "wgpu-backend")]
            Backend::Wgpu { egui } => egui.run_ui(run_ui),
        }
    }

    /// How long until egui wants another frame, from the last [`Self::run`].
    pub fn repaint_delay(&self) -> std::time::Duration {
        match &self.backend {
            #[cfg(feature = "glow-backend")]
            Backend::Glow { egui, .. } => egui.repaint_delay(),
            #[cfg(feature = "canvas-backend")]
            Backend::Canvas { egui, .. } => egui.repaint_delay(),
            #[cfg(feature = "canvas-backend")]
            Backend::CanvasBlit { egui, .. } => egui.repaint_delay(),
            #[cfg(feature = "wgpu-backend")]
            Backend::Wgpu { egui } => egui.repaint_delay(),
        }
    }

    /// Clear to `clear_color`, paint the last [`Self::run`], present.
    pub fn paint(&mut self, clear_color: [f32; 4]) {
        match &mut self.backend {
            #[cfg(feature = "glow-backend")]
            Backend::Glow { window, egui, .. } => {
                egui.clear(clear_color);
                egui.paint();
                window.gl_swap_window();
            }
            #[cfg(feature = "canvas-backend")]
            Backend::Canvas {
                canvas,
                egui,
                turned,
            } => {
                let rotation = egui.state.rotation();
                if rotation == Rotation::None {
                    canvas.set_draw_color(rgb(clear_color));
                    canvas.clear();
                    egui.paint(canvas);
                    canvas.present();
                } else {
                    paint_turned(canvas, egui, turned, rotation, clear_color);
                }
            }
            #[cfg(feature = "canvas-backend")]
            Backend::CanvasBlit {
                canvas,
                offscreen,
                present,
                size,
                frame,
                egui,
            } => {
                // Rebuild on resize so surface, texture and window stay 1:1.
                if canvas.output_size().is_ok_and(|s| s != *size) {
                    let format = egui.painter.format();
                    match rebuild_blit_targets(canvas, format) {
                        Ok((new_offscreen, new_present, new_size)) => {
                            let rotation = egui.state.rotation();
                            egui.destroy();
                            *egui = crate::EguiCanvas::for_surface_with_format(
                                canvas.window(),
                                &new_offscreen,
                                format,
                            );
                            // The fresh state starts unturned; the window's turn
                            // outlives the target it was being presented on.
                            egui.state.set_rotation(rotation);
                            *offscreen = new_offscreen;
                            *present = new_present;
                            *size = new_size;
                        }
                        Err(e) => log::error!("could not resize the offscreen target: {e}"),
                    }
                }

                offscreen.set_draw_color(rgb(clear_color));
                offscreen.clear();
                egui.paint(offscreen);
                let rotation = egui.state.rotation();
                let surface = offscreen.surface();
                let pitch = surface.pitch() as usize;
                match surface.without_lock() {
                    // The offscreen is square and egui painted into its top-left
                    // corner, so the wider pitch is all SDL needs to pick an
                    // unturned frame out of it. A turned one is copied out here
                    // rather than rotated by the driver: this mode exists for
                    // drivers that show a texture copy and little else.
                    Some(pixels) => {
                        let uploaded = if rotation == Rotation::None {
                            present.update(None, pixels, pitch)
                        } else {
                            rotate_frame(rotation, pixels, pitch, *size, frame);
                            present.update(None, frame, size.0 as usize * BYTES_PER_PIXEL)
                        };
                        if let Err(e) = uploaded {
                            log::error!("could not upload the offscreen frame: {e}");
                        }
                    }
                    None => log::error!("offscreen surface has no readable pixels"),
                }
                if let Err(e) = canvas.copy(present, None, None) {
                    log::error!("could not blit the offscreen frame: {e}");
                }
                canvas.present();
            }
            #[cfg(feature = "wgpu-backend")]
            Backend::Wgpu { egui } => egui.paint(clear_color),
        }
    }

    /// Release the renderer's graphics resources.
    pub fn destroy(&mut self) {
        match &mut self.backend {
            #[cfg(feature = "glow-backend")]
            Backend::Glow { egui, .. } => egui.destroy(),
            #[cfg(feature = "canvas-backend")]
            Backend::Canvas { egui, turned, .. } => {
                egui.destroy();
                if let Some(target) = turned.take() {
                    unsafe { target.texture.destroy() }
                }
            }
            #[cfg(feature = "canvas-backend")]
            Backend::CanvasBlit { egui, .. } => egui.destroy(),
            #[cfg(feature = "wgpu-backend")]
            Backend::Wgpu { .. } => {}
        }
    }
}

fn build(
    video: &VideoSubsystem,
    make_window: &impl Fn(&VideoSubsystem, bool) -> Result<Window, String>,
    renderer: Renderer,
) -> Result<Backend, String> {
    match renderer {
        Renderer::Gles3 => build_glow(video, make_window, sdl2::video::GLProfile::GLES, 3, 0),
        Renderer::Gl32 => build_glow(video, make_window, sdl2::video::GLProfile::Core, 3, 2),
        Renderer::Canvas => build_canvas(video, make_window),
        Renderer::CanvasBlit => build_canvas_blit(video, make_window),
        Renderer::Wgpu => build_wgpu(video, make_window),
    }
}

/// `SDL_GL_SetAttribute`, reported rather than asserted.
#[cfg(feature = "glow-backend")]
fn set_gl_attr(name: &str, attr: sdl2::sys::SDL_GLattr, value: i32) -> Result<(), String> {
    if unsafe { sdl2::sys::SDL_GL_SetAttribute(attr, value) } == 0 {
        return Ok(());
    }
    Err(format!("{name}={value} rejected: {}", sdl2::get_error()))
}

/// The values `SDL_GL_CONTEXT_PROFILE_MASK` takes.
#[cfg(feature = "glow-backend")]
fn gl_profile_value(profile: sdl2::video::GLProfile) -> i32 {
    use sdl2::video::GLProfile;
    match profile {
        GLProfile::Core => 1,
        GLProfile::Compatibility => 2,
        GLProfile::GLES => 4,
        GLProfile::Unknown(i) => i,
    }
}

#[cfg(feature = "glow-backend")]
fn build_glow(
    video: &VideoSubsystem,
    make_window: &impl Fn(&VideoSubsystem, bool) -> Result<Window, String>,
    profile: sdl2::video::GLProfile,
    major: u8,
    minor: u8,
) -> Result<Backend, String> {
    // Not `video.gl_attr()`: its setters panic on rejection, which would kill
    // the fallthrough on a device without GL.
    use sdl2::sys::SDL_GLattr::*;
    set_gl_attr(
        "profile_mask",
        SDL_GL_CONTEXT_PROFILE_MASK,
        gl_profile_value(profile),
    )?;
    set_gl_attr("major_version", SDL_GL_CONTEXT_MAJOR_VERSION, major as i32)?;
    set_gl_attr("minor_version", SDL_GL_CONTEXT_MINOR_VERSION, minor as i32)?;
    set_gl_attr("doublebuffer", SDL_GL_DOUBLEBUFFER, 1)?;

    let window = make_window(video, true)?;
    let gl_context = window.gl_create_context()?;
    window.gl_make_current(&gl_context)?;
    let _ = video.gl_set_swap_interval(sdl2::video::SwapInterval::VSync);

    let glow_ctx = std::sync::Arc::new(unsafe {
        glow::Context::from_loader_function(|name| {
            video.gl_get_proc_address(name) as *const std::os::raw::c_void
        })
    });
    let egui = crate::EguiGlow::new(&window, glow_ctx, None, false);
    Ok(Backend::Glow {
        window,
        _gl_context: gl_context,
        egui,
    })
}

#[cfg(not(feature = "glow-backend"))]
fn build_glow(
    _video: &VideoSubsystem,
    _make_window: &impl Fn(&VideoSubsystem, bool) -> Result<Window, String>,
    _profile: sdl2::video::GLProfile,
    _major: u8,
    _minor: u8,
) -> Result<Backend, String> {
    Err("built without the glow-backend feature".to_string())
}

#[cfg(feature = "canvas-backend")]
fn build_canvas(
    video: &VideoSubsystem,
    make_window: &impl Fn(&VideoSubsystem, bool) -> Result<Window, String>,
) -> Result<Backend, String> {
    let window = make_window(video, false)?;
    let canvas = window
        .into_canvas()
        .present_vsync()
        .build()
        .map_err(|e| e.to_string())?;
    log::debug!("SDL renderer driver: {}", canvas.info().name);
    let egui = crate::EguiCanvas::new(&canvas);
    Ok(Backend::Canvas {
        canvas,
        egui,
        turned: None,
    })
}

/// The offscreen surface, its presentation texture, and the size both cover.
#[cfg(feature = "canvas-backend")]
type BlitTargets = (
    sdl2::render::Canvas<sdl2::surface::Surface<'static>>,
    sdl2::render::Texture,
    (u32, u32),
);

#[cfg(feature = "canvas-backend")]
fn rebuild_blit_targets(
    canvas: &sdl2::render::WindowCanvas,
    format: sdl2::pixels::PixelFormatEnum,
) -> Result<BlitTargets, String> {
    let size = canvas.output_size()?;
    // Square, on the longer edge: a quarter turn lays the screen out as tall as
    // the window is wide, and sizing the surface to the turn instead would mean
    // rebuilding the renderer whenever the turn changed — which takes egui's
    // textures with it. egui paints into the top-left corner either way.
    let side = size.0.max(size.1);
    let surface = sdl2::surface::Surface::new(side, side, format)?;
    let offscreen = sdl2::render::Canvas::from_surface(surface)?;
    let mut present = canvas
        .texture_creator()
        .create_texture_streaming(format, size.0, size.1)
        .map_err(|e| e.to_string())?;
    // A whole frame replaces rather than blends. SDL gives a format with alpha
    // `BLEND` by default, which would dim any pixel the offscreen renderer left
    // short of opaque against whatever the window happened to hold, and there is
    // nothing under a full frame worth mixing in.
    present.set_blend_mode(sdl2::render::BlendMode::None);
    Ok((offscreen, present, size))
}

#[cfg(feature = "canvas-backend")]
fn build_canvas_blit(
    video: &VideoSubsystem,
    make_window: &impl Fn(&VideoSubsystem, bool) -> Result<Window, String>,
) -> Result<Backend, String> {
    let window = make_window(video, false)?;
    // No vsync request: not every driver this mode serves advertises it, and
    // asking excludes those that don't.
    let canvas = window.into_canvas().build().map_err(|e| e.to_string())?;
    log::debug!("SDL renderer driver: {} (blit)", canvas.info().name);
    // The window's renderer decides: the whole frame crosses to it every frame.
    let format = crate::canvas::painter::preferred_format(&canvas);
    log::debug!("blit format: {format:?}");
    let (offscreen, present, size) = rebuild_blit_targets(&canvas, format)?;
    let egui = crate::EguiCanvas::for_surface_with_format(canvas.window(), &offscreen, format);
    Ok(Backend::CanvasBlit {
        canvas,
        offscreen,
        present,
        size,
        frame: Vec::new(),
        egui,
    })
}

#[cfg(not(feature = "canvas-backend"))]
fn build_canvas_blit(
    _video: &VideoSubsystem,
    _make_window: &impl Fn(&VideoSubsystem, bool) -> Result<Window, String>,
) -> Result<Backend, String> {
    Err("built without the canvas-backend feature".to_string())
}

#[cfg(not(feature = "canvas-backend"))]
fn build_canvas(
    _video: &VideoSubsystem,
    _make_window: &impl Fn(&VideoSubsystem, bool) -> Result<Window, String>,
) -> Result<Backend, String> {
    Err("built without the canvas-backend feature".to_string())
}

#[cfg(feature = "wgpu-backend")]
fn build_wgpu(
    video: &VideoSubsystem,
    make_window: &impl Fn(&VideoSubsystem, bool) -> Result<Window, String>,
) -> Result<Backend, String> {
    let window = make_window(video, false)?;
    // wgpu's setup is async; this is startup, so blocking on it is the whole
    // ceremony an app would otherwise write itself.
    let egui = pollster::block_on(crate::EguiWgpu::new(window));
    Ok(Backend::Wgpu {
        egui: Box::new(egui),
    })
}

#[cfg(not(feature = "wgpu-backend"))]
fn build_wgpu(
    _video: &VideoSubsystem,
    _make_window: &impl Fn(&VideoSubsystem, bool) -> Result<Window, String>,
) -> Result<Backend, String> {
    Err("built without the wgpu-backend feature".to_string())
}

/// Where a turn is painted before it reaches the window: the window canvas has
/// no offscreen of its own, and SDL cannot rotate what it draws directly.
#[cfg(feature = "canvas-backend")]
pub struct TurnedTarget {
    /// Square, on the window's longer edge, so a change of turn fits without a
    /// rebuild. egui paints into its top-left corner.
    texture: sdl2::render::Texture,
    /// The window size it was built for.
    window: (u32, u32),
}

/// Paint egui into the turned target and copy that onto the window at an angle.
/// One rotated copy per frame, which every accelerated driver does for free and
/// SDL's own renderer has an exact path for at multiples of 90°.
#[cfg(feature = "canvas-backend")]
fn paint_turned(
    canvas: &mut sdl2::render::WindowCanvas,
    egui: &mut crate::EguiCanvas,
    turned: &mut Option<TurnedTarget>,
    rotation: Rotation,
    clear_color: [f32; 4],
) {
    let window = match canvas.output_size() {
        Ok(size) => size,
        Err(e) => return log::error!("could not read the window size: {e}"),
    };
    if turned.as_ref().is_none_or(|t| t.window != window) {
        let side = window.0.max(window.1);
        match canvas
            .texture_creator()
            .create_texture_target(egui.painter.format(), side, side)
        {
            Ok(mut texture) => {
                // A whole frame replaces rather than blends, as in the blit path.
                texture.set_blend_mode(sdl2::render::BlendMode::None);
                if let Some(old) = turned.replace(TurnedTarget { texture, window }) {
                    unsafe { old.texture.destroy() }
                }
            }
            // Every accelerated driver has render targets, and so does SDL's own
            // software renderer; a driver without them shows the frame unturned
            // rather than nothing at all.
            Err(e) => {
                log::error!("could not build a {side}x{side} target to turn the frame in: {e}");
                canvas.set_draw_color(rgb(clear_color));
                canvas.clear();
                egui.paint(canvas);
                canvas.present();
                return;
            }
        }
    }
    let Some(target) = turned.as_mut() else {
        unreachable!("the target was just built, or was already the right size")
    };

    let painted = canvas.with_texture_canvas(&mut target.texture, |target| {
        target.set_draw_color(rgb(clear_color));
        target.clear();
        egui.paint(target);
    });
    if let Err(e) = painted {
        return log::error!("could not paint into the turned target: {e}");
    }

    // The frame egui laid out, in the corner of the square target, and where the
    // turn lands it: SDL rotates a copy about the centre of its destination, so
    // a turned frame placed centrally comes down over the whole window.
    let (w, h) = if rotation.swaps_axes() {
        (window.1, window.0)
    } else {
        window
    };
    let src = sdl2::rect::Rect::new(0, 0, w, h);
    let dst = sdl2::rect::Rect::new(
        (window.0 as i32 - w as i32) / 2,
        (window.1 as i32 - h as i32) / 2,
        w,
        h,
    );
    canvas.set_draw_color(rgb(clear_color));
    canvas.clear();
    let copied = canvas.copy_ex(
        &target.texture,
        Some(src),
        Some(dst),
        rotation.degrees(),
        None,
        false,
        false,
    );
    if let Err(e) = copied {
        log::error!("could not present the turned frame: {e}");
    }
    canvas.present();
}

/// Copy the frame out of the square offscreen buffer turned, as a tight
/// window-sized one. `src` holds the screen egui painted — as wide as the window
/// is tall on a quarter turn — in the top-left corner of a buffer of `pitch`.
#[cfg(feature = "canvas-backend")]
fn rotate_frame(
    rotation: Rotation,
    src: &[u8],
    pitch: usize,
    window: (u32, u32),
    dst: &mut Vec<u8>,
) {
    let (width, height) = (window.0 as usize, window.1 as usize);
    let row = width * BYTES_PER_PIXEL;
    if dst.len() != row * height {
        dst.resize(row * height, 0);
    }
    // Walked by destination row, so the writes run straight through; on a
    // quarter turn it is the reads that step down a column instead, which is the
    // cheaper of the two to scatter.
    for (y, line) in dst.chunks_exact_mut(row).enumerate() {
        for (x, pixel) in line
            .as_chunks_mut::<BYTES_PER_PIXEL>()
            .0
            .iter_mut()
            .enumerate()
        {
            // Where this window pixel sits in the turned screen — the whole-pixel
            // form of `Rotation::from_window`.
            let (sx, sy) = match rotation {
                Rotation::None => (x, y),
                Rotation::Cw90 => (y, width - 1 - x),
                Rotation::Cw180 => (width - 1 - x, height - 1 - y),
                Rotation::Cw270 => (height - 1 - y, x),
            };
            let at = sy * pitch + sx * BYTES_PER_PIXEL;
            pixel.copy_from_slice(&src[at..at + BYTES_PER_PIXEL]);
        }
    }
}

/// egui and GL take linear floats; SDL clears in 8-bit channels.
#[cfg(feature = "canvas-backend")]
fn rgb(color: [f32; 4]) -> sdl2::pixels::Color {
    let byte = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    sdl2::pixels::Color::RGB(byte(color[0]), byte(color[1]), byte(color[2]))
}

#[cfg(all(test, feature = "canvas-backend"))]
mod tests {
    use super::*;

    /// A 3x2 window, so a quarter turn is visibly a different shape.
    const WINDOW: (u32, u32) = (3, 2);

    /// The screen for this turn, painted into the corner of a square buffer, one
    /// value per pixel repeated across its channels.
    fn painted(rotation: Rotation) -> (Vec<u8>, usize) {
        let (w, h) = if rotation.swaps_axes() {
            (WINDOW.1, WINDOW.0)
        } else {
            WINDOW
        };
        let side = WINDOW.0.max(WINDOW.1) as usize;
        let pitch = side * BYTES_PER_PIXEL;
        let mut buffer = vec![0u8; pitch * side];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let at = y * pitch + x * BYTES_PER_PIXEL;
                buffer[at..at + BYTES_PER_PIXEL].fill((y * 10 + x) as u8);
            }
        }
        (buffer, pitch)
    }

    /// One value per window pixel, row by row.
    fn presented(rotation: Rotation) -> Vec<u8> {
        let (src, pitch) = painted(rotation);
        let mut frame = Vec::new();
        rotate_frame(rotation, &src, pitch, WINDOW, &mut frame);
        assert_eq!(
            frame.len(),
            WINDOW.0 as usize * WINDOW.1 as usize * BYTES_PER_PIXEL
        );
        frame
            .as_chunks::<BYTES_PER_PIXEL>()
            .0
            .iter()
            .map(|pixel| {
                assert!(pixel.iter().all(|b| *b == pixel[0]), "a pixel was torn");
                pixel[0]
            })
            .collect()
    }

    #[test]
    fn an_unturned_frame_is_copied_across_as_it_is() {
        assert_eq!(presented(Rotation::None), [0, 1, 2, 10, 11, 12]);
    }

    #[test]
    fn a_quarter_turn_clockwise_stands_the_screen_up() {
        // The screen is 2 wide and 3 tall:  0  1  ·  10 11  ·  20 21
        // and comes down on the window as: 20 10 0  ·  21 11 1
        assert_eq!(presented(Rotation::Cw90), [20, 10, 0, 21, 11, 1]);
    }

    #[test]
    fn a_half_turn_reverses_both_axes() {
        assert_eq!(presented(Rotation::Cw180), [12, 11, 10, 2, 1, 0]);
    }

    #[test]
    fn a_quarter_turn_counterclockwise_is_the_other_way_round() {
        assert_eq!(presented(Rotation::Cw270), [1, 11, 21, 0, 10, 20]);
    }
}
