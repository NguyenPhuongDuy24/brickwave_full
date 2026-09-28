//! `EguiWindow` picking a renderer by itself: GL first, SDL's renderer if the
//! device has no usable GL. Run with `RENDERER=canvas` to force the fallback.

use crate::common::UiExample;
use egui_sdl2::{EguiWindow, Renderer};
use sdl2::event::Event;
use std::time::Duration;
mod common;

fn main() {
    let sdl = sdl2::init().unwrap();
    let video = sdl.video().unwrap();
    let mut event_pump = sdl.event_pump().unwrap();

    let order: &[Renderer] = match std::env::var("RENDERER").as_deref() {
        Ok("canvas") => &[Renderer::Canvas],
        Ok("wgpu") => &[Renderer::Wgpu],
        _ => &Renderer::FALLBACK_CHAIN,
    };
    let mut egui = EguiWindow::new(
        &video,
        "Egui SDL2 Window",
        (800, 600),
        |builder| {
            builder.resizable();
        },
        order,
    )
    .expect("no renderer available");
    println!("running on {:?}", egui.renderer());

    let mut ui = UiExample::default();
    let frame_dur = Duration::from_secs_f64(1.0 / common::TARGET_FPS);
    while !ui.quit {
        for event in event_pump.poll_iter() {
            if matches!(event, Event::Quit { .. }) {
                ui.quit = true;
            }
            let _ = egui.on_event(&event);
        }
        egui.run(|ctx| ui.update(ctx));
        egui.paint(ui.color);
        std::thread::sleep(frame_dur);
    }

    egui.destroy();
}
