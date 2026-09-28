use crate::common::UiExample;
use sdl2::event::{Event, WindowEvent};
use std::time::Duration;
mod common;

fn main() {
    let sdl = sdl2::init().unwrap();
    let mut event_pump = sdl.event_pump().unwrap();
    let mut app = pollster::block_on(App::new(&sdl));
    let frame_dur = Duration::from_secs_f64(1.0 / common::TARGET_FPS);

    while !app.ui.quit {
        for event in event_pump.poll_iter() {
            app.handle_event(&event);
        }

        app.update();
        std::thread::sleep(frame_dur);
    }
}

struct App {
    egui: egui_sdl2::EguiWgpu,
    ui: common::UiExample,
}

impl App {
    pub async fn new(sdl: &sdl2::Sdl) -> Self {
        let video = sdl.video().unwrap();
        let window = video
            .window("Egui SDL2 WGPU", 800, 600)
            .resizable()
            .build()
            .unwrap();
        let egui = egui_sdl2::EguiWgpu::new(window).await;

        Self {
            egui,
            ui: UiExample::default(),
        }
    }

    pub fn handle_event(&mut self, event: &Event) {
        let resp = self.egui.on_event(event);

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
        self.egui.paint(self.ui.color);
    }
}
