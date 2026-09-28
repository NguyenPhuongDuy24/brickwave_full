use std::time::Duration;

use egui::{
    self, Align, Align2, CornerRadius, Frame, Layout, Margin, Rect, Response, RichText, Sense,
    Stroke, Ui, pos2, vec2,
};

use crate::icons::{Glyph, IconRegistry};
use crate::state::UiState;
use crate::theme::{
    ACCENT, ACCENT_HOVER, APP_BG, BORDER, RADIUS, RAISED, SECONDARY, SMALL_RADIUS, SUBTLE, SURFACE,
    TEXT, WARM, font_size,
};

const CARD_WIDTH: f32 = 500.0;
const CARD_HEIGHT_LOADING: f32 = 288.0;
const CARD_HEIGHT_ERROR: f32 = 328.0;
const ERROR: egui::Color32 = egui::Color32::from_rgb(255, 150, 105);

/// Full-screen saved-session gate. QR controls deliberately do not exist in
/// this view, so a network hiccup during restore cannot look like a fresh
/// account login.
pub fn render(ui: &mut Ui, state: &mut UiState, app_logo: &egui::TextureHandle) {
    let error = state.auth_error().map(str::to_owned);
    let card_height = if error.is_some() {
        CARD_HEIGHT_ERROR
    } else {
        CARD_HEIGHT_LOADING
    };
    let available = ui.available_rect_before_wrap().size();

    ui.allocate_ui_with_layout(available, Layout::top_down(Align::Center), |ui| {
        ui.add_space(((available.y - card_height) * 0.5).max(24.0));
        Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(CornerRadius::same(RADIUS))
            .inner_margin(Margin::same(28))
            .show(ui, |ui| {
                ui.set_width((CARD_WIDTH - 56.0).min((available.x - 56.0).max(260.0)));
                ui.set_min_height(card_height - 56.0);
                ui.with_layout(Layout::top_down(Align::Center), |ui| {
                    brand_mark(ui, app_logo, error.is_some());
                    ui.add_space(17.0);

                    if let Some(message) = error {
                        ui.label(
                            RichText::new("Không thể khôi phục phiên của bạn")
                                .size(font_size(23.0))
                                .color(TEXT),
                        );
                        ui.add_space(8.0);
                        ui.label(RichText::new(message).size(font_size(12.0)).color(ERROR));
                        ui.add_space(7.0);
                        ui.label(
                            RichText::new(
                                "Thử lại phiên đã lưu hoặc đăng nhập lại bằng mã QR mới.",
                            )
                            .size(font_size(11.0))
                            .color(SECONDARY),
                        );
                        ui.add_space(20.0);
                        ui.horizontal(|ui| {
                            if restore_button(ui, "restore-retry", "THỬ LẠI", true).clicked() {
                                state.retry_restore_session();
                            }
                            if restore_button(ui, "restore-sign-in", "ĐĂNG NHẬP LẠI", false)
                                .clicked()
                            {
                                state.abandon_restore_and_login();
                            }
                        });
                    } else {
                        ui.label(
                            RichText::new("Chào mừng trở lại")
                                .size(font_size(25.0))
                                .color(TEXT),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("Đang khôi phục phiên SoundCloud")
                                .size(font_size(13.0))
                                .color(SECONDARY),
                        );
                        ui.add_space(17.0);
                        waveform(ui);
                        ui.add_space(15.0);
                        ui.label(
                            RichText::new("Đang kiểm tra tài khoản đã lưu...")
                                .size(font_size(11.0))
                                .color(SECONDARY),
                        );
                        ui.ctx().request_repaint_after(Duration::from_millis(33));
                    }
                });
            });
    });
}

fn brand_mark(ui: &mut Ui, app_logo: &egui::TextureHandle, failed: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(58.0, 58.0), Sense::hover());
    if failed {
        ui.painter().rect(
            rect,
            CornerRadius::same(14),
            WARM,
            Stroke::new(1.0, ERROR),
            egui::StrokeKind::Inside,
        );
        IconRegistry::paint(ui.painter(), Glyph::Warning, rect.shrink(13.0), ERROR);
    } else {
        ui.painter().image(
            app_logo.id(),
            rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    }
}

fn waveform(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(150.0, 48.0), Sense::hover());
    let time = ui.input(|input| input.time) as f32;
    let bar_width = 10.0;
    let gap = 9.0;
    let total_width = bar_width * 5.0 + gap * 4.0;
    let left = rect.center().x - total_width * 0.5;

    for index in 0..5 {
        let wave = (time * 4.4 + index as f32 * 0.78).sin();
        let height = 13.0 + (wave * 0.5 + 0.5) * 29.0;
        let center = pos2(
            left + index as f32 * (bar_width + gap) + bar_width * 0.5,
            rect.center().y,
        );
        let bar = Rect::from_center_size(center, vec2(bar_width, height));
        let color = if index == 2 { ACCENT_HOVER } else { ACCENT };
        ui.painter().rect_filled(bar, CornerRadius::same(4), color);
    }
}

fn restore_button(ui: &mut Ui, id: &'static str, text: &str, primary: bool) -> Response {
    let width = (text.len() as f32 * 7.4 + 34.0).max(112.0);
    let (rect, response) = ui
        .push_id(id, |ui| {
            ui.allocate_exact_size(vec2(width, 40.0), Sense::click())
        })
        .inner;
    let fill = if primary {
        if response.hovered() {
            ACCENT_HOVER
        } else {
            ACCENT
        }
    } else if response.hovered() {
        RAISED
    } else {
        SURFACE
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(SMALL_RADIUS),
        fill,
        Stroke::new(
            1.0,
            if primary || response.hovered() {
                ACCENT
            } else {
                SUBTLE
            },
        ),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(font_size(11.0)),
        if primary { APP_BG } else { TEXT },
    );
    response
}
