#[allow(dead_code)]
pub const TARGET_FPS: f64 = 60.0;

pub struct UiExample {
    pub multiline_text: String,
    pub slider_value: f32,
    pub color: [f32; 4],
    pub quit: bool,
}

impl Default for UiExample {
    fn default() -> Self {
        Self {
            multiline_text: String::new(),
            slider_value: 0.0,
            color: egui::Rgba::from_rgb(0.35, 0.55, 0.95).to_array(),
            quit: false,
        }
    }
}

impl UiExample {
    pub fn update(&mut self, ctx: &egui::Context) {
        // Keep animating even without input events (spinner, progress bar).
        ctx.request_repaint();
        let t = ctx.input(|i| i.time) as f32;

        egui::Window::new("egui-sdl2")
            .default_pos([32.0, 32.0])
            .default_width(320.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    ui.label(egui::RichText::new("egui on SDL2").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new("software · OpenGL · wgpu").weak());
                    });
                });

                ui.separator();

                if ui.button("Greet").clicked() {
                    self.multiline_text = "Hello, world!".to_string();
                    println!("{}", self.multiline_text);
                }

                ui.text_edit_multiline(&mut self.multiline_text);
                ui.add(egui::Slider::new(&mut self.slider_value, 0.0..=50.0).text("Slider"));

                // A little ambient motion so the UI is lively without input.
                let progress = 0.5 * (1.0 + (t * 1.5).sin());
                ui.add(egui::ProgressBar::new(progress).show_percentage());

                ui.horizontal(|ui| {
                    ui.label("Color: ");
                    ui.color_edit_button_rgba_premultiplied(&mut self.color);
                });

                ui.separator();

                if ui.button("Quit?").clicked() {
                    self.quit = true;
                }
            });
    }
}
