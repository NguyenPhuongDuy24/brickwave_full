[![CI](https://github.com/mxmgorin/egui-sdl2/actions/workflows/ci.yml/badge.svg)](https://github.com/mxmgorin/egui-sdl2/actions)
[![Documentation](https://docs.rs/egui-sdl2/badge.svg)](https://docs.rs/egui-sdl2)
[![Dependencies](https://deps.rs/repo/github/mxmgorin/egui-sdl2/status.svg)](https://deps.rs/repo/github/mxmgorin/egui-sdl2)
[![crates.io](https://img.shields.io/crates/v/egui-sdl2.svg)](https://crates.io/crates/egui-sdl2)
[![Downloads](https://img.shields.io/crates/d/egui-sdl2)](https://crates.io/crates/egui-sdl2)
[![License](https://img.shields.io/crates/l/egui-sdl2)](#license)

# egui-sdl2

**[egui](https://github.com/emilk/egui) on [SDL2](https://github.com/Rust-SDL2/rust-sdl2): SDL2 event handling plus three swappable rendering backends behind one consistent API.**

<p align="center"><img src="https://raw.githubusercontent.com/mxmgorin/egui-sdl2/main/assets/demo.gif" alt="egui-sdl2 demo" width="460"></p>

## Why egui-sdl2?

- **Three rendering backends, one API.** Software ([`Canvas`](https://docs.rs/sdl2/latest/sdl2/render/struct.Canvas.html)),
  OpenGL ([`glow`](https://crates.io/crates/glow)), and WebGPU ([`wgpu`](https://github.com/gfx-rs/wgpu)) — pick one
  with a feature flag and use the same `on_event` / `run` / `paint` loop for all of them.
- **Tracks the latest egui.** Kept in step with current egui releases so you are not stuck on an old version.
- **Built on the official design.** Mirrors the structure of the upstream `egui-winit`, `egui_glow`, and
  `egui-wgpu` crates, so the API feels familiar and behaves predictably.
- **Batteries included.** Translates SDL2 events into egui events and handles `egui::PlatformOutput`
  (clipboard, cursor updates, opening links). Both `egui` and `sdl2` are re-exported for convenience, and
  the `sdl2` re-export forwards all of SDL2's feature flags.

## Rendering backends

Enable exactly what you need via feature flags:

- `canvas-backend` — software rendering via [`Canvas`](https://docs.rs/sdl2/latest/sdl2/render/struct.Canvas.html)
- `glow-backend` — OpenGL via [`glow`](https://crates.io/crates/glow)
- `wgpu-backend` — WebGPU via [`wgpu`](https://github.com/gfx-rs/wgpu)

### Or let it pick

`EguiWindow` owns the window and walks a list of renderers, keeping the first
that comes up. A device with missing or broken GL drivers falls through to SDL's
renderer and still shows a UI instead of exiting:

```rust
let mut egui = egui_sdl2::EguiWindow::new(
    &video,
    "Egui SDL2",
    (800, 600),
    |builder| { builder.resizable(); },
    &egui_sdl2::Renderer::FALLBACK_CHAIN, // GLES 3.0, then GL 3.2 core, then Canvas
)?;
println!("running on {:?}", egui.renderer());

loop {
    for event in event_pump.poll_iter() {
        egui.on_event(&event);
    }
    egui.run(|ctx: &egui::Context| {});
    egui.paint([0.1, 0.1, 0.1, 1.0]); // clears, paints and presents
}
```

## Usage

```rust
// Create SDL2 window and canvas:
let sdl = sdl2::init().unwrap();
let video = sdl.video().unwrap();
let window = video.window("Egui SDL2 Canvas", 800, 600).build().unwrap();
let mut canvas = window.into_canvas().build().unwrap();
// Create egui renderer; the canvas stays yours:
let mut egui = egui_sdl2::EguiCanvas::new(&canvas);
let mut event_pump = sdl.event_pump().unwrap();
loop {
    // Feed SDL2 events into egui:
    for event in event_pump.poll_iter() {
        egui.on_event(&canvas, &event);
    }
    // Call `run` + `paint` each frame, over anything you drew yourself:
    egui.run(|ctx: &egui::Context| {});
    canvas.clear();
    egui.paint(&mut canvas);
    canvas.present();
    std::thread::sleep(std::time::Duration::from_secs_f64(1.0 / 60.0));
}
```

To get started, create an [`EguiGlow`](https://docs.rs/egui-sdl2/latest/egui_sdl2/glow/index.html),
[`EguiCanvas`](https://docs.rs/egui-sdl2/latest/egui_sdl2/canvas/index.html), or
[`EguiWgpu`](https://docs.rs/egui-sdl2/latest/egui_sdl2/wgpu/index.html) instance to manage rendering.
Pass SDL2 events to `on_event`, then call `run` and `paint` each frame. For event handling only, you can use
the [`State`](https://docs.rs/egui-sdl2/latest/egui_sdl2/state/index.html) type.

Examples are available in the [examples/](https://github.com/mxmgorin/egui-sdl2/tree/main/examples/)
directory. To run the `canvas` example:

```sh
cargo run --example canvas
```

The `window` example shows the picking path; `RENDERER=canvas cargo run
--example window` forces the fallback.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
