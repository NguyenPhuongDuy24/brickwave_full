use egui::{Color32, CornerRadius, Stroke, Style, Vec2};

pub const APP_BG: Color32 = Color32::from_rgb(0x10, 0x11, 0x10);
pub const SIDEBAR_BG: Color32 = Color32::from_rgb(0x19, 0x1a, 0x18);
pub const SURFACE: Color32 = Color32::from_rgb(0x24, 0x24, 0x20);
pub const RAISED: Color32 = Color32::from_rgb(0x30, 0x2c, 0x26);
pub const WARM: Color32 = Color32::from_rgb(0x39, 0x25, 0x1b);
pub const ACCENT: Color32 = Color32::from_rgb(0xff, 0x65, 0x00);
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0xff, 0x85, 0x3a);
pub const TEXT: Color32 = Color32::from_rgb(0xf2, 0xe8, 0xd8);
pub const SECONDARY: Color32 = Color32::from_rgb(0xae, 0xa7, 0x9a);
pub const BORDER: Color32 = Color32::from_rgb(0x66, 0x59, 0x4b);
pub const SUBTLE: Color32 = Color32::from_rgb(0x38, 0x35, 0x30);
pub const DISABLED: Color32 = Color32::from_rgb(0x6f, 0x69, 0x60);

pub const RADIUS: u8 = 8;
pub const SMALL_RADIUS: u8 = 5;
pub const FONT_SCALE: f32 = 1.18;

pub const fn font_size(size: f32) -> f32 {
    size * FONT_SCALE
}

pub fn apply(style: &mut Style) {
    for font in style.text_styles.values_mut() {
        font.size = font_size(font.size);
    }
    style.spacing.item_spacing = Vec2::new(9.0, 9.0);
    style.spacing.button_padding = Vec2::new(11.0, 7.0);
    style.spacing.interact_size = Vec2::new(34.0, 32.0);
    style.visuals.dark_mode = true;
    style.visuals.override_text_color = Some(TEXT);
    style.visuals.weak_text_color = Some(SECONDARY);
    style.visuals.faint_bg_color = SURFACE;
    style.visuals.extreme_bg_color = APP_BG;
    style.visuals.text_edit_bg_color = Some(SURFACE);
    style.visuals.code_bg_color = RAISED;
    style.visuals.selection.bg_fill = WARM;
    style.visuals.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    style.visuals.window_fill = SURFACE;
    style.visuals.window_stroke = Stroke::new(1.0_f32, BORDER);
    style.visuals.window_corner_radius = CornerRadius::same(RADIUS);
    style.visuals.menu_corner_radius = CornerRadius::same(RADIUS);
    style.visuals.panel_fill = APP_BG;
    style.visuals.button_frame = true;
    style.visuals.slider_trailing_fill = true;
    style.visuals.disabled_alpha = 0.62;

    let widgets = &mut style.visuals.widgets;
    widgets.noninteractive.bg_fill = SURFACE;
    widgets.noninteractive.weak_bg_fill = SURFACE;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    widgets.noninteractive.corner_radius = CornerRadius::same(SMALL_RADIUS);

    widgets.inactive.bg_fill = RAISED;
    widgets.inactive.weak_bg_fill = SURFACE;
    widgets.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    widgets.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    widgets.inactive.corner_radius = CornerRadius::same(SMALL_RADIUS);

    widgets.hovered.bg_fill = WARM;
    widgets.hovered.weak_bg_fill = WARM;
    widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT_HOVER);
    widgets.hovered.fg_stroke = Stroke::new(1.5_f32, TEXT);
    widgets.hovered.corner_radius = CornerRadius::same(SMALL_RADIUS);

    widgets.active.bg_fill = ACCENT;
    widgets.active.weak_bg_fill = ACCENT;
    widgets.active.bg_stroke = Stroke::new(1.0_f32, ACCENT_HOVER);
    widgets.active.fg_stroke = Stroke::new(1.5_f32, APP_BG);
    widgets.active.corner_radius = CornerRadius::same(SMALL_RADIUS);

    widgets.open = widgets.hovered.clone();
}
