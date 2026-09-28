use egui::{Event, ViewportId};
use egui_wgpu::capture::CaptureState;
use egui_wgpu::wgpu::{CompositeAlphaMode, TextureFormat};
use egui_wgpu::wgpu::{
    Surface, SurfaceConfiguration, SurfaceTargetUnsafe, TextureUsages, TextureViewDescriptor,
};
use egui_wgpu::{RenderState, RendererOptions, SurfaceErrorAction, WgpuConfiguration, WgpuError};
use std::{num::NonZeroU32, sync::Arc};

struct SurfaceState {
    surface: Surface<'static>,
    alpha_mode: CompositeAlphaMode,
    width: u32,
    height: u32,
}

/// Everything you need to paint egui with [`wgpu`](https://docs.rs/wgpu) on [`sdl2`].
pub struct Painter {
    context: egui::Context,
    configuration: WgpuConfiguration,
    msaa_samples: u32,
    support_transparent_backbuffer: bool,
    depth_format: Option<TextureFormat>,
    screen_capture_state: Option<CaptureState>,

    instance: egui_wgpu::wgpu::Instance,
    render_state: Option<RenderState>,

    // Per viewport/window:
    depth_texture_view: egui::ViewportIdMap<egui_wgpu::wgpu::TextureView>,
    msaa_texture_view: egui::ViewportIdMap<egui_wgpu::wgpu::TextureView>,
    surfaces: egui::ViewportIdMap<SurfaceState>,
    capture_tx: egui_wgpu::capture::CaptureSender,
    capture_rx: egui_wgpu::capture::CaptureReceiver,
    options: egui_wgpu::RendererOptions,
}

impl Painter {
    /// Manages [`wgpu`](https://docs.rs/wgpu) state, including surface state, required to render egui.
    ///
    /// Only the [`egui_wgpu::wgpu::Instance`] is initialized here. Device selection and the initialization
    /// of render + surface state is deferred until the painter is given its first window target
    /// via [`set_window()`](Self::set_window). (Ensuring that a device that's compatible with the
    /// native window is chosen)
    ///
    /// Before calling [`paint_and_update_textures()`](Self::paint_and_update_textures) a
    /// [`egui_wgpu::wgpu::Surface`] must be initialized (and corresponding render state) by calling
    /// [`set_window()`](Self::set_window) once you have
    /// a [`sdl2::video::Window`] with a valid `.raw_window_handle()`
    /// associated.
    pub async fn new(
        context: egui::Context,
        configuration: WgpuConfiguration,
        msaa_samples: u32,
        depth_format: Option<TextureFormat>,
        support_transparent_backbuffer: bool,
        dithering: bool,
    ) -> Self {
        let (capture_tx, capture_rx) = std::sync::mpsc::channel();
        let instance = configuration.wgpu_setup.new_instance().await;

        Self {
            context,
            configuration,
            msaa_samples,
            support_transparent_backbuffer,
            depth_format,
            screen_capture_state: None,

            instance,
            render_state: None,

            depth_texture_view: Default::default(),
            surfaces: Default::default(),
            msaa_texture_view: Default::default(),

            capture_tx,
            capture_rx,
            options: RendererOptions {
                msaa_samples,
                depth_stencil_format: depth_format,
                dithering,
                predictable_texture_filtering: true,
            },
        }
    }

    /// Get the [`RenderState`].
    ///
    /// Will return [`None`] if the render state has not been initialized yet.
    pub fn render_state(&self) -> Option<RenderState> {
        self.render_state.clone()
    }

    /// Updates the [`sdl2::video::Window`] associated with the [`Painter`] without taking ownership of the window.
    ///
    /// # Safety
    /// The user is responsible for ensuring that the window is alive for as long as it is set.
    pub async unsafe fn set_window(
        &mut self,
        viewport_id: ViewportId,
        window: &sdl2::video::Window,
    ) -> Result<(), WgpuError> {
        let size = window.size();
        if !self.surfaces.contains_key(&viewport_id) {
            let surface = unsafe {
                let target = SurfaceTargetUnsafe::from_window(&window).unwrap();
                self.instance.create_surface_unsafe(target)?
            };
            self.add_surface(surface, viewport_id, size).await?;
        }

        Ok(())
    }

    async fn add_surface(
        &mut self,
        surface: Surface<'static>,
        viewport_id: ViewportId,
        size: (u32, u32),
    ) -> Result<(), WgpuError> {
        let render_state = if let Some(render_state) = &self.render_state {
            render_state
        } else {
            let render_state = RenderState::create(
                &self.configuration,
                &self.instance,
                Some(&surface),
                self.options,
            )
            .await?;
            self.render_state.get_or_insert(render_state)
        };
        let alpha_mode = if self.support_transparent_backbuffer {
            let supported_alpha_modes = surface.get_capabilities(&render_state.adapter).alpha_modes;

            // Prefer pre multiplied over post multiplied!
            if supported_alpha_modes.contains(&egui_wgpu::wgpu::CompositeAlphaMode::PreMultiplied) {
                egui_wgpu::wgpu::CompositeAlphaMode::PreMultiplied
            } else if supported_alpha_modes
                .contains(&egui_wgpu::wgpu::CompositeAlphaMode::PostMultiplied)
            {
                egui_wgpu::wgpu::CompositeAlphaMode::PostMultiplied
            } else {
                log::warn!(
                    "Transparent window was requested, but the active wgpu surface does not support a `CompositeAlphaMode` with transparency."
                );
                egui_wgpu::wgpu::CompositeAlphaMode::Auto
            }
        } else {
            egui_wgpu::wgpu::CompositeAlphaMode::Auto
        };
        self.surfaces.insert(
            viewport_id,
            SurfaceState {
                surface,
                width: size.0,
                height: size.1,
                alpha_mode,
            },
        );
        let Some(width) = NonZeroU32::new(size.0) else {
            log::debug!("The window width was zero; skipping generate textures");
            return Ok(());
        };
        let Some(height) = NonZeroU32::new(size.1) else {
            log::debug!("The window height was zero; skipping generate textures");
            return Ok(());
        };
        self.resize_and_generate_depth_texture_view_and_msaa_view(viewport_id, width, height);
        Ok(())
    }

    /// Returns the maximum texture dimension supported if known
    ///
    /// This API will only return a known dimension after `set_window()` has been called
    /// at least once, since the underlying device and render state are initialized lazily
    /// once we have a window (that may determine the choice of adapter/device).
    pub fn max_texture_side(&self) -> Option<usize> {
        self.render_state
            .as_ref()
            .map(|rs| rs.device.limits().max_texture_dimension_2d as usize)
    }

    fn resize_and_generate_depth_texture_view_and_msaa_view(
        &mut self,
        viewport_id: ViewportId,
        width_in_pixels: NonZeroU32,
        height_in_pixels: NonZeroU32,
    ) {
        let width = width_in_pixels.get();
        let height = height_in_pixels.get();

        let render_state = self.render_state.as_ref().unwrap();
        let surface_state = self.surfaces.get_mut(&viewport_id).unwrap();

        surface_state.width = width;
        surface_state.height = height;

        configure_surface(surface_state, render_state, &self.configuration);

        if let Some(depth_format) = self.depth_format {
            self.depth_texture_view.insert(
                viewport_id,
                render_state
                    .device
                    .create_texture(&egui_wgpu::wgpu::TextureDescriptor {
                        label: Some("egui_depth_texture"),
                        size: egui_wgpu::wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: self.msaa_samples,
                        dimension: egui_wgpu::wgpu::TextureDimension::D2,
                        format: depth_format,
                        usage: egui_wgpu::wgpu::TextureUsages::RENDER_ATTACHMENT
                            | egui_wgpu::wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[depth_format],
                    })
                    .create_view(&egui_wgpu::wgpu::TextureViewDescriptor::default()),
            );
        }

        if let Some(render_state) = (self.msaa_samples > 1)
            .then_some(self.render_state.as_ref())
            .flatten()
        {
            let texture_format = render_state.target_format;
            self.msaa_texture_view.insert(
                viewport_id,
                render_state
                    .device
                    .create_texture(&egui_wgpu::wgpu::TextureDescriptor {
                        label: Some("egui_msaa_texture"),
                        size: egui_wgpu::wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: self.msaa_samples,
                        dimension: egui_wgpu::wgpu::TextureDimension::D2,
                        format: texture_format,
                        usage: egui_wgpu::wgpu::TextureUsages::RENDER_ATTACHMENT,
                        view_formats: &[texture_format],
                    })
                    .create_view(&egui_wgpu::wgpu::TextureViewDescriptor::default()),
            );
        }
    }

    pub fn on_window_resized(
        &mut self,
        viewport_id: ViewportId,
        width_in_pixels: NonZeroU32,
        height_in_pixels: NonZeroU32,
    ) {
        if self.surfaces.contains_key(&viewport_id) {
            self.resize_and_generate_depth_texture_view_and_msaa_view(
                viewport_id,
                width_in_pixels,
                height_in_pixels,
            );
        } else {
            log::warn!(
                "Ignoring window resize notification with no surface created via Painter::set_window()"
            );
        }
    }

    /// Returns two things:
    ///
    /// The approximate number of seconds spent on vsync-waiting (if any),
    /// and the captures captured screenshot if it was requested.
    ///
    /// If `capture_data` isn't empty, a screenshot will be captured.
    pub fn paint_and_update_textures(
        &mut self,
        viewport_id: ViewportId,
        pixels_per_point: f32,
        clear_color: [f32; 4],
        clipped_primitives: &[egui::ClippedPrimitive],
        // Drained: egui 0.36 asserts on drop that every delta was handled.
        textures_delta: &mut egui::TexturesDelta,
        capture_data: Vec<egui::UserData>,
    ) -> f32 {
        let capture = !capture_data.is_empty();
        let mut vsync_sec = 0.0;

        let Some(render_state) = self.render_state.as_mut() else {
            // Nothing to apply the deltas to; dropping them unapplied would
            // trip egui 0.36's drop check.
            textures_delta.clear();
            return vsync_sec;
        };
        let Some(surface_state) = self.surfaces.get(&viewport_id) else {
            textures_delta.clear();
            return vsync_sec;
        };

        let mut encoder = render_state.device.create_command_encoder(
            &egui_wgpu::wgpu::CommandEncoderDescriptor {
                label: Some("encoder"),
            },
        );

        // Upload all resources for the GPU.
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [surface_state.width, surface_state.height],
            pixels_per_point,
        };

        let user_cmd_bufs = {
            let mut renderer = render_state.renderer.write();
            // egui 0.36 batches several deltas per texture; apply them in order.
            for (id, image_deltas) in textures_delta.set.drain() {
                for image_delta in image_deltas {
                    renderer.update_texture(
                        &render_state.device,
                        &render_state.queue,
                        id,
                        &image_delta,
                    );
                }
            }

            renderer.update_buffers(
                &render_state.device,
                &render_state.queue,
                &mut encoder,
                clipped_primitives,
                &screen_descriptor,
            )
        };

        let output_frame = {
            // This is what vsync-waiting happens on my Mac.
            let start = web_time::Instant::now();
            let output_frame = surface_state.surface.get_current_texture();
            vsync_sec += start.elapsed().as_secs_f32();
            output_frame
        };

        let output_frame = match output_frame {
            egui_wgpu::wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            // egui uses the suboptimal frame as-is and defers reconfiguration to the next frame.
            egui_wgpu::wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            other => match (*self.configuration.on_surface_status)(&other) {
                // `Reconfigure` reconfigures the existing surface; `RecreateSurface` asks for a
                // fresh surface object. This painter only reconfigures in place, so both map to
                // the same recovery path: reconfigure and skip the current frame.
                SurfaceErrorAction::Reconfigure | SurfaceErrorAction::RecreateSurface => {
                    configure_surface(surface_state, render_state, &self.configuration);
                    return vsync_sec;
                }
                SurfaceErrorAction::SkipFrame => {
                    return vsync_sec;
                }
            },
        };

        let mut capture_buffer = None;
        {
            let renderer = render_state.renderer.read();

            let target_texture = if capture {
                let capture_state = self.screen_capture_state.get_or_insert_with(|| {
                    CaptureState::new(&render_state.device, &output_frame.texture)
                });
                capture_state.update(&render_state.device, &output_frame.texture);

                &capture_state.texture
            } else {
                &output_frame.texture
            };
            let target_view = target_texture.create_view(&TextureViewDescriptor::default());

            let (view, resolve_target) = (self.msaa_samples > 1)
                .then_some(self.msaa_texture_view.get(&viewport_id))
                .flatten()
                .map_or((&target_view, None), |texture_view| {
                    (texture_view, Some(&target_view))
                });

            let render_pass = encoder.begin_render_pass(&egui_wgpu::wgpu::RenderPassDescriptor {
                label: Some("egui_render"),
                color_attachments: &[Some(egui_wgpu::wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target,
                    ops: egui_wgpu::wgpu::Operations {
                        load: egui_wgpu::wgpu::LoadOp::Clear(egui_wgpu::wgpu::Color {
                            r: clear_color[0] as f64,
                            g: clear_color[1] as f64,
                            b: clear_color[2] as f64,
                            a: clear_color[3] as f64,
                        }),
                        store: egui_wgpu::wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: self.depth_texture_view.get(&viewport_id).map(|view| {
                    egui_wgpu::wgpu::RenderPassDepthStencilAttachment {
                        view,
                        depth_ops: Some(egui_wgpu::wgpu::Operations {
                            load: egui_wgpu::wgpu::LoadOp::Clear(1.0),
                            // It is very unlikely that the depth buffer is needed after egui finished rendering
                            // so no need to store it. (this can improve performance on tiling GPUs like mobile chips or Apple Silicon)
                            store: egui_wgpu::wgpu::StoreOp::Discard,
                        }),
                        stencil_ops: None,
                    }
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // Forgetting the pass' lifetime means that we are no longer compile-time protected from
            // runtime errors caused by accessing the parent encoder before the render pass is dropped.
            // Since we don't pass it on to the renderer, we should be perfectly safe against this mistake here!
            renderer.render(
                &mut render_pass.forget_lifetime(),
                clipped_primitives,
                &screen_descriptor,
            );

            if capture {
                if let Some(capture_state) = &mut self.screen_capture_state {
                    capture_buffer = Some(capture_state.copy_textures(
                        &render_state.device,
                        &output_frame,
                        &mut encoder,
                    ));
                }
            }
        }

        let encoded = { encoder.finish() };

        // Submit the commands: both the main buffer and user-defined ones.
        {
            // wgpu doesn't document where vsync can happen. Maybe here?
            let start = web_time::Instant::now();
            render_state
                .queue
                .submit(user_cmd_bufs.into_iter().chain([encoded]));
            vsync_sec += start.elapsed().as_secs_f32();
        };

        // Free textures marked for destruction **after** queue submit since they might still be used in the current frame.
        // Calling `egui_wgpu::wgpu::Texture::destroy` on a texture that is still in use would invalidate the command buffer(s) it is used in.
        // However, once we called `egui_wgpu::wgpu::Queue::submit`, it is up for wgpu to determine how long the underlying gpu resource has to live.
        {
            let mut renderer = render_state.renderer.write();
            for id in textures_delta.free.drain() {
                renderer.free_texture(&id);
            }
        }

        if let Some(capture_buffer) = capture_buffer {
            if let Some(screen_capture_state) = &mut self.screen_capture_state {
                screen_capture_state.read_screen_rgba(
                    self.context.clone(),
                    capture_buffer,
                    capture_data,
                    self.capture_tx.clone(),
                    viewport_id,
                );
            }
        }

        {
            // wgpu doesn't document where vsync can happen. Maybe here?
            let start = web_time::Instant::now();
            // wgpu 30 presents through the queue rather than the texture.
            render_state.queue.present(output_frame);
            vsync_sec += start.elapsed().as_secs_f32();
        }

        vsync_sec
    }

    /// Call this at the beginning of each frame to receive the requested screenshots.
    pub fn handle_screenshots(&self, events: &mut Vec<Event>) {
        for (viewport_id, user_data, screenshot) in self.capture_rx.try_iter() {
            let screenshot: Arc<egui::ColorImage> = Arc::new(screenshot);
            for data in user_data {
                events.push(Event::Screenshot {
                    viewport_id,
                    user_data: data,
                    image: screenshot.clone(),
                });
            }
        }
    }
}

fn configure_surface(
    surface_state: &SurfaceState,
    render_state: &RenderState,
    config: &WgpuConfiguration,
) {
    let width = surface_state.width;
    let height = surface_state.height;

    let mut surf_config = SurfaceConfiguration {
        usage: TextureUsages::RENDER_ATTACHMENT,
        format: render_state.target_format,
        present_mode: config.surface.present_mode,
        alpha_mode: surface_state.alpha_mode,
        view_formats: vec![render_state.target_format],
        ..surface_state
            .surface
            .get_default_config(&render_state.adapter, width, height)
            .expect("The surface isn't supported by this adapter")
    };

    if let Some(desired_maximum_frame_latency) = config.surface.desired_maximum_frame_latency {
        surf_config.desired_maximum_frame_latency = desired_maximum_frame_latency;
    }

    surface_state
        .surface
        .configure(&render_state.device, &surf_config);
}
