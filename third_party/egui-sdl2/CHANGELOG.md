# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.12.0] - 2026-09-02

### Added

- `Painter::paint_primitives_within` paints only the damaged part of a frame,
  for a caller that knows what changed since the last one.
- `EguiCanvas::run_output` is public, for a caller that paints a frame in steps
  of its own rather than through `EguiCanvas::paint`.

### Fixed

- The canvas backend lands an unscaled texture copy on whole pixels. The source
  rect is whole texels, so a fractional destination had SDL resample a 1:1 copy
  and drop a row or a column of it — at a fractional zoom a glyph lost the
  crossbar of an H, the arm of an F, the middle of an S.

## [0.11.0] - 2026-08-15

### Added

- The canvas backend's pixel format is configurable: `Painter::with_format`,
  `Painter::for_surface_with_format` and the matching `EguiCanvas` constructors
  take one, `Painter::format` reports it, and `preferred_format` names the one a
  renderer holds natively. egui's pixels are shuffled into it on upload.

### Changed

- The canvas backend takes its format from the renderer rather than always
  asking for `ABGR8888`, which is still the first choice — egui's own pixels are
  already in it. A renderer without it (the Miyoo Mini's offers `RGB565` and
  `ARGB8888`) converted on every upload instead, which under
  `Renderer::CanvasBlit` is the whole frame, every frame.

## [0.10.0] - 2026-08-11

### Added

- `Rotation` and `EguiWindow::set_rotation`: present the UI at a quarter turn to
  the window, for a panel that is not mounted the way it is read. egui lays out
  for the turned screen — a quarter turn trades the window's width and height —
  and each backend puts the frame back on the panel its own way: the glow and
  wgpu painters turn the tessellated geometry, the window canvas paints into a
  render target and copies that across at an angle, and the offscreen blit turns
  the frame as it uploads it, so a driver that shows nothing but a texture copy
  still gets a turned screen. Pointer and touch positions travel back through
  the turn, so a tap lands where it looks. `State::set_rotation` is the layout
  half on its own, for an app driving a painter itself.

### Changed

- The offscreen blit's surface is square, on the window's longer edge, so a
  change of turn rebuilds nothing — the renderer, and egui's textures with it,
  outlive the turn.

## [0.9.0] - 2026-08-11

### Changed

- Updated `egui`, `egui_glow` and `egui-wgpu` from 0.35 to 0.36 (wgpu 30, which
  presents through the queue). MSRV is 1.95.
- **Breaking:** `Painter::paint_and_update_textures` on the canvas and wgpu
  backends takes `&mut TexturesDelta` and drains it — egui 0.36 batches several
  deltas per texture and asserts on drop that every one was handled.
- Modifiers are tracked by `State` and sent as `ModifiersChanged` events;
  dropped files implement egui 0.36's `DroppedFile` trait, reading bytes from
  the path on demand.

## [0.8.2] - 2026-08-11

### Fixed

- Canvas backends draw partial alpha correctly. egui's colours are premultiplied
  while SDL's `BLEND` is `src*a + dst*(1-a)`, which multiplied by alpha a second
  time; vertex colours and textures are handed over unmultiplied now. The
  renderer's own blend mode is set for the run too, because SDL leaves it at
  `None` — an untextured half-transparent fill overwrote what lay under it instead
  of blending, so egui's fade-in of a new panel or window showed as a dark flash
  for a few frames, close to black on a dark theme.
- Feathered edges render as antialiasing rather than a dark fringe. Feathering
  fades a shape's edge to fully transparent, and epaint gives that end no hue —
  SDL interpolates vertex colours as straight alpha, so the gradient ran to
  black rather than to invisible, showing as a dark line along the bottom and
  right of the window, where a full-screen fill's soft edge lands. Transparent
  vertices now take the hue their triangle already carries, which makes the
  interpolation a correct antialiased fade.
- The frame `CanvasBlit` presents replaces the window's pixels rather than
  blending with them. SDL gives a texture whose format carries alpha
  `SDL_BLENDMODE_BLEND` by default, which dimmed any pixel left short of opaque.

## [0.8.1] - 2026-08-03

### Added

- `Renderer::CanvasBlit`: the canvas rasterized offscreen by SDL's software
  renderer and presented as one texture copy per frame, for drivers that show
  nothing else. The Miyoo Mini's `mmiyoo` drops geometry and window-surface
  updates without reporting an error, so anything drawn straight to the window
  never appears. Costs a full-frame upload, so `Canvas` stays the default.
- `Painter::for_surface` and `EguiCanvas::for_surface`, painting into a surface
  while input and sizing still come from the window. `Painter` and `EguiCanvas`
  gained a render-target context parameter, defaulted to `WindowContext`.
- `Painter::max_texture_side` and `State::set_max_texture_side`, so egui lays its
  font atlas out within what the driver holds.

### Fixed

- Text and rectangles are blitted rather than rasterized as triangles. SDL's
  software triangle blit drops the last row of a textured triangle (still present
  in the 2.26 forks handhelds ship), which clipped the bottom of every glyph; a
  blit is also cheaper than per-pixel triangle work where there is no GPU.
- Creating the font atlas no longer panics on drivers that cap texture size below
  egui's 2048 default — the cap is reported to egui instead.
- A rejected GL attribute no longer panics: `build_glow` sets them through
  `SDL_GL_SetAttribute` and reports the error, so `EguiWindow` falls through to
  the next renderer as intended. `video.gl_attr()`'s setters panic, which took
  the process down on devices without GL.

## [0.8.0] - 2026-08-02

### Added

- `EguiWindow`: owns the window and takes the first renderer from a list that
  comes up (`Renderer::FALLBACK_CHAIN` — GLES 3.0, GL 3.2 core, then `Canvas`),
  so a device with no usable GL still gets a UI. Its `paint` clears, paints and
  presents whichever backend won.

## [0.7.0] - 2026-07-30

### Added

- `EguiGlow::run_ui`, `EguiCanvas::run_ui`, `EguiWgpu::run_ui` and
  `EguiRunOutput::update_ui`, which hand the closure egui's root `Ui` instead of
  the `Context`. As of egui 0.35 panels (`CentralPanel`, `TopBottomPanel`, …) are
  shown into a `Ui`, so a full-screen layout was not expressible through `run`.

## [0.6.0] - 2026-07-30

### Changed

- **Breaking:** the canvas backend no longer takes ownership of the window or
  canvas. `EguiCanvas::new` and `Painter::new` take `&Canvas<Window>`, and
  `EguiCanvas::on_event`/`paint` (plus `Painter::paint_and_update_textures` and
  `paint_primitives`) take the canvas per call. Applications that already own a
  canvas — games, emulators, anything drawing its own frame — can now paint egui
  on top of it instead of restructuring around the painter's canvas.
- **Breaking:** removed `EguiCanvas::clear`/`present` and `Painter::canvas`; call
  `clear`/`present` on your own canvas instead.

### Added

- `EguiGlow::on_event`, mirroring `EguiCanvas::on_event`, so the glow backend
  doesn't have to reach through `EguiGlow::state`.

## [0.5.0] - 2026-07-21

### Changed

- Updated `egui`, `egui_glow`, and `egui-wgpu` from 0.34 to 0.35.
- Migrated to the egui-wgpu 0.35 API: `WgpuConfiguration`'s `present_mode` and
  `desired_maximum_frame_latency` now live under the new `surface` field, and the new
  `SurfaceErrorAction::Reconfigure` variant is handled.
- Bumped `pollster` (dev) to 1.0, `bytemuck` to 1.25, and `log` to 0.4.33.

### Fixed

- The published crate now includes the `LICENSE-APACHE` and `LICENSE-MIT` files
  (the previous `include` paths pointed outside the package).

### Documentation

- docs.rs now builds with every rendering backend, so the `wgpu` module is documented.
- Rewrote the README and fixed a broken intra-doc link in the glow module.

## [0.4.0] - 2026-06-16

### Added

- `State::get_drawable_size` helper.

### Changed

- `wgpu-backend` is now opt-in; the default features cover the Canvas and glow backends.
  Examples are gated behind their respective backend features.

### Fixed

- glow backend now uses the drawable (physical) size for the GL viewport.

### Performance

- Pointer coordinates are mapped via a cached points-per-pixel value.
- Canvas backend reuses its vertex buffer and de-duplicates clip-rect calls.

## Earlier releases

See the [GitHub releases](https://github.com/mxmgorin/egui-sdl2/releases) and
[tags](https://github.com/mxmgorin/egui-sdl2/tags) for versions 0.3.2 and earlier.

[Unreleased]: https://github.com/mxmgorin/egui-sdl2/compare/v0.12.0...HEAD
[0.12.0]: https://github.com/mxmgorin/egui-sdl2/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/mxmgorin/egui-sdl2/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/mxmgorin/egui-sdl2/compare/v0.9.0...v0.10.0
[0.9.0]: https://github.com/mxmgorin/egui-sdl2/compare/v0.8.2...v0.9.0
[0.8.2]: https://github.com/mxmgorin/egui-sdl2/compare/v0.8.1...v0.8.2
[0.8.1]: https://github.com/mxmgorin/egui-sdl2/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/mxmgorin/egui-sdl2/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/mxmgorin/egui-sdl2/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/mxmgorin/egui-sdl2/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/mxmgorin/egui-sdl2/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/mxmgorin/egui-sdl2/compare/v0.3.2...v0.4.0
