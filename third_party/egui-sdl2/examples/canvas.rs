use crate::common::UiExample;
use sdl2::event::{Event, WindowEvent};
use sdl2::pixels::Color;
use sdl2::render::Canvas;
use sdl2::video::Window;
use std::time::Duration;
mod common;

fn main() {
    let sdl = sdl2::init().unwrap();
    let mut event_pump = sdl.event_pump().unwrap();
    let mut app = App::new(&sdl);
    let frame_dur = Duration::from_secs_f64(1.0 / common::TARGET_FPS);

    while !app.ui.quit {
        for event in event_pump.poll_iter() {
            app.handle_event(&event);
        }

        app.update();
        std::thread::sleep(frame_dur);
    }

    app.shutdown();
}

struct App {
    canvas: Canvas<Window>,
    egui: egui_sdl2::EguiCanvas,
    ui: UiExample,
}

impl App {
    pub fn new(sdl: &sdl2::Sdl) -> Self {
        let video = sdl.video().unwrap();
        let window = video
            .window("Egui SDL2 Canvas", 800, 600)
            .resizable()
            .build()
            .unwrap();
        let canvas = window.into_canvas().build().unwrap();
        let egui = egui_sdl2::EguiCanvas::new(&canvas);

        Self {
            canvas,
            egui,
            ui: UiExample::default(),
        }
    }

    pub fn shutdown(&mut self) {
        self.egui.destroy();
    }

    pub fn handle_event(&mut self, event: &Event) {
        let resp = self.egui.on_event(&self.canvas, event);

        if !resp.consumed {
            if let Event::Window {
                win_event: WindowEvent::Close,
                ..
            } = event
            {
                self.ui.quit = true;
            }
        }
    }

    pub fn update(&mut self) {
        self.egui.run(|ctx| self.ui.update(ctx));
        self.canvas.set_draw_color(to_sdl_color(self.ui.color));
        self.canvas.clear();
        self.egui.paint(&mut self.canvas);
        self.canvas.present();
    }
}

fn to_sdl_color(c: [f32; 4]) -> Color {
    let ch = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;

    Color::RGBA(ch(c[0]), ch(c[1]), ch(c[2]), ch(c[3]))
}
