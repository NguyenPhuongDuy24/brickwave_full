//! A painted image keeps its colours in whatever format the painter was given.
//! The channel shuffle an upload does is invisible to the atlas (white with
//! coverage in alpha), so this draws a coloured texture instead.
#![cfg(feature = "canvas-backend")]

use egui_sdl2::canvas::Painter;
use sdl2::pixels::{Color, PixelFormatEnum};
use sdl2::render::Canvas;
use sdl2::surface::Surface;

const SIDE: u32 = 64;
const INK: [u8; 3] = [200, 100, 50];

/// The pixel at the centre of a frame filled by one coloured egui image, as the
/// bytes it is held in.
fn painted_centre(format: PixelFormatEnum) -> [u8; 4] {
    let surface = Surface::new(SIDE, SIDE, format).expect("a surface in a 32-bit format");
    let mut canvas = Canvas::from_surface(surface).expect("SDL's software renderer");
    let mut painter = Painter::for_surface_with_format(&canvas, format);

    let ctx = egui::Context::default();
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SIDE as f32, SIDE as f32));
    let input = egui::RawInput {
        screen_rect: Some(screen),
        ..Default::default()
    };
    let image = egui::ColorImage::new(
        [2, 2],
        vec![egui::Color32::from_rgb(INK[0], INK[1], INK[2]); 4],
    );
    let mut output = ctx.run_ui(input, |ui| {
        let texture = ui
            .ctx()
            .load_texture("ink", image.clone(), egui::TextureOptions::NEAREST);
        ui.image((texture.id(), screen.size()));
    });
    let primitives = ctx.tessellate(std::mem::take(&mut output.shapes), output.pixels_per_point);
    canvas.set_draw_color(Color::RGB(0, 0, 0));
    canvas.clear();
    painter
        .paint_and_update_textures(
            &mut canvas,
            output.pixels_per_point,
            &mut output.textures_delta,
            primitives,
        )
        .expect("painting into a surface");
    painter.destroy();

    let surface = canvas.surface();
    let pitch = surface.pitch() as usize;
    let at = (SIDE as usize / 2) * pitch + (SIDE as usize / 2) * 4;
    let pixels = surface.without_lock().expect("a surface owns its pixels");
    pixels[at..at + 4].try_into().expect("four bytes")
}

#[test]
fn abgr_holds_egui_s_own_byte_order() {
    let [r, g, b, a] = painted_centre(PixelFormatEnum::ABGR8888);
    assert_eq!([r, g, b], INK);
    assert_eq!(a, 255);
}

#[test]
fn argb_holds_the_same_colour_shuffled() {
    let [b, g, r, a] = painted_centre(PixelFormatEnum::ARGB8888);
    assert_eq!([r, g, b], INK);
    assert_eq!(a, 255);
}
