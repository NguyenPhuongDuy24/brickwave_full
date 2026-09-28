use crate::common::UiExample;
use sdl2::{
    event::{Event, WindowEvent},
    video::GLContext,
};
use std::{sync::Arc, time::Duration};
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
    _gl_ctx: GLContext,
    window: sdl2::video::Window,
    egui: egui_sdl2::EguiGlow,
    ui: UiExample,
}

impl App {
    pub fn new(sdl: &sdl2::Sdl) -> Self {
        let video = sdl.video().unwrap();
        let window = video
            .window("Egui SDL2 Glow", 800, 600)
            .opengl()
            .resizable()
            .build()
            .unwrap();
        let gl_ctx = window
            .gl_create_context()
            .expect("Failed to create GL context");
        window.gl_make_current(&gl_ctx).unwrap();
        let glow_ctx = Arc::new(unsafe {
            glow::Context::from_loader_function(|name| video.gl_get_proc_address(name) as *const _)
        });
        let egui = egui_sdl2::EguiGlow::new(&window, glow_ctx, None, false);

        Self {
            _gl_ctx: gl_ctx,
            window,
            egui,
            ui: UiExample::default(),
        }
    }

    pub fn shutdown(&mut self) {
        self.egui.destroy();
    }

    pub fn handle_event(&mut self, event: &Event) {
        let resp = self.egui.state.on_event(&self.window, event);

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
        self.egui.clear(self.ui.color);
        self.egui.paint();
        self.window.gl_swap_window();
    }
}
