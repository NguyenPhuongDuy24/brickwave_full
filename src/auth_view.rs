use std::time::{SystemTime, UNIX_EPOCH};

use egui::{
    self, Align, Align2, Color32, CornerRadius, Frame, Layout, Margin, Rect, Response, RichText,
    Sense, Stroke, Ui, Vec2, pos2, vec2,
};
use qrcodegen::{QrCode, QrCodeEcc};

use crate::backend::{AuthState, QrLoginSession};
use crate::icons::{Glyph, IconRegistry};
use crate::state::UiState;
use crate::theme::{
    ACCENT, ACCENT_HOVER, APP_BG, BORDER, DISABLED, RADIUS, RAISED, SECONDARY, SMALL_RADIUS,
    SUBTLE, SURFACE, TEXT, WARM, font_size,
};

const ERROR: Color32 = Color32::from_rgb(255, 150, 105);
const SUCCESS: Color32 = Color32::from_rgb(118, 201, 142);
const CARD_MAX_WIDTH: f32 = 840.0;
const CARD_HEIGHT_WIDE: f32 = 558.0;
const CARD_PADDING: f32 = 22.0;
const COLUMN_GAP: f32 = 26.0;
const WIDE_LAYOUT_MIN: f32 = 700.0;
const HEADER_HEIGHT: f32 = 58.0;
const FOOTER_HEIGHT: f32 = 54.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthStep {
    Scan,
    Confirm,
    Connected,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct AuthLayout {
    card_width: f32,
    card_height: f32,
    qr_column_width: f32,
    qr_size: f32,
    wide: bool,
}

#[derive(Clone, Copy)]
struct AuthPresentation {
    step: AuthStep,
    status: &'static str,
    status_icon: Glyph,
    status_color: Color32,
    title: &'static str,
    detail: &'static str,
}

pub fn render(ui: &mut Ui, state: &mut UiState, app_logo: &egui::TextureHandle) {
    let available = ui.available_rect_before_wrap().size();
    login_header(ui, state, app_logo, available.x);
    let center_height = (available.y - HEADER_HEIGHT - FOOTER_HEIGHT).max(0.0);
    ui.allocate_ui_with_layout(
        vec2(available.x, center_height),
        Layout::top_down(Align::Center),
        |ui| render_center(ui, state),
    );
    ui.allocate_ui_with_layout(
        vec2(available.x, FOOTER_HEIGHT),
        Layout::top_down(Align::Center),
        |ui| {
            ui.add_space(5.0);
            security_note(ui);
        },
    );
}

fn login_header(ui: &mut Ui, state: &mut UiState, app_logo: &egui::TextureHandle, width: f32) {
    let response = ui.allocate_ui_with_layout(
        vec2(width, HEADER_HEIGHT),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.add_space(22.0);
            let (brand_rect, _) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::hover());
            ui.painter().image(
                app_logo.id(),
                brand_rect,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            ui.add_space(9.0);
            ui.vertical(|ui| {
                label(ui, "BRICKWAVE", 17.0, ACCENT, true);
                label(ui, "TÀI KHOẢN SOUNDCLOUD", 9.0, SECONDARY, true);
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(22.0);
                if state.can_leave_login()
                    && auth_button(ui, "auth-back", Glyph::Back, "QUAY LẠI", false, true).clicked()
                {
                    state.back_from_login();
                }
            });
        },
    );
    ui.painter().line_segment(
        [
            response.response.rect.left_bottom(),
            response.response.rect.right_bottom(),
        ],
        Stroke::new(1.0, SUBTLE),
    );
}

fn render_center(ui: &mut Ui, state: &mut UiState) {
    let available = ui.available_rect_before_wrap().size();
    let layout = auth_layout(available);
    let top_space = ((available.y - layout.card_height) * 0.5).max(0.0);
    ui.add_space(top_space);

    ui.with_layout(Layout::top_down(Align::Center), |ui| {
        Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(CornerRadius::same(RADIUS))
            .inner_margin(Margin::same(CARD_PADDING as i8))
            .show(ui, |ui| {
                ui.set_width(layout.card_width - CARD_PADDING * 2.0);
                ui.set_min_height(layout.card_height - CARD_PADDING * 2.0);
                let expired = state
                    .qr_login()
                    .is_some_and(|login| login_is_expired(login, now_ms()));

                if layout.wide {
                    ui.scope(|ui| {
                        // The split owns its gutter. Removing egui's implicit
                        // horizontal item gaps keeps the computed 840/762px
                        // card widths exact instead of expanding the frame.
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.horizontal(|ui| {
                            ui.allocate_ui_with_layout(
                                vec2(
                                    layout.qr_column_width,
                                    layout.card_height - CARD_PADDING * 2.0,
                                ),
                                Layout::top_down(Align::Center),
                                |ui| {
                                    render_qr_column(ui, state.qr_login(), layout.qr_size, expired)
                                },
                            );
                            ui.add_space(COLUMN_GAP);
                            let right_width = layout.card_width
                                - CARD_PADDING * 2.0
                                - layout.qr_column_width
                                - COLUMN_GAP;
                            ui.allocate_ui_with_layout(
                                vec2(right_width, layout.card_height - CARD_PADDING * 2.0),
                                Layout::top_down(Align::Min),
                                |ui| render_auth_details(ui, state, expired),
                            );
                        });
                    });
                } else {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.vertical_centered(|ui| {
                                render_qr_column(ui, state.qr_login(), layout.qr_size, expired);
                            });
                            ui.add_space(18.0);
                            render_auth_details(ui, state, expired);
                        });
                }
            });
    });
}

fn auth_layout(available: Vec2) -> AuthLayout {
    let card_width = available.x.min(CARD_MAX_WIDTH).max(0.0);
    let wide = card_width >= WIDE_LAYOUT_MIN;
    let card_height = if wide {
        available.y.min(CARD_HEIGHT_WIDE)
    } else {
        available.y.min(720.0)
    };
    let content_width = (card_width - CARD_PADDING * 2.0).max(0.0);
    let qr_column_width = if wide {
        (content_width * 0.35).clamp(238.0, 274.0)
    } else {
        content_width
    };
    let qr_size = if wide {
        (qr_column_width - 12.0).clamp(218.0, 258.0)
    } else {
        (content_width - 24.0).clamp(180.0, 238.0)
    };
    AuthLayout {
        card_width,
        card_height,
        qr_column_width,
        qr_size,
        wide,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

fn login_is_expired(login: &QrLoginSession, now_ms: u64) -> bool {
    login.expires_at_ms <= now_ms
}

fn render_qr_column(ui: &mut Ui, login: Option<&QrLoginSession>, qr_size: f32, expired: bool) {
    ui.add_space(3.0);
    label(ui, "QUÉT ĐỂ KẾT NỐI", 10.0, SECONDARY, true);
    ui.add_space(10.0);
    match (login, expired) {
        (Some(login), false) => paint_qr(ui, &login.pairing_url, qr_size),
        _ => paint_qr_placeholder(ui, qr_size, expired),
    }
    ui.add_space(12.0);
    let note = if expired {
        "Mã ghép nối này không còn hiệu lực."
    } else if login.is_some() {
        "Mở camera điện thoại và quét mã."
    } else {
        "Tạo mã ngắn hạn để bắt đầu."
    };
    wrapped_label(ui, note, 11.0, SECONDARY, false);
}

fn render_auth_details(ui: &mut Ui, state: &mut UiState, expired: bool) {
    let auth_state = state.auth_state();
    let presentation = auth_presentation(auth_state, expired);

    label(ui, "TÀI KHOẢN BRICKWAVE", 10.0, ACCENT, true);
    ui.add_space(5.0);
    wrapped_label(ui, presentation.title, 25.0, TEXT, true);
    ui.add_space(3.0);
    wrapped_label(ui, presentation.detail, 12.0, SECONDARY, false);
    ui.add_space(13.0);
    progress_indicator(ui, presentation.step, auth_state == AuthState::Authorized);
    ui.add_space(12.0);

    if let Some(login) = state.qr_login().filter(|_| !expired) {
        confirmation_code(ui, &login.confirmation_code);
        ui.add_space(8.0);
        wrapped_label(
            ui,
            if auth_state == AuthState::PairingConfirmation {
                "Xác nhận mã trùng khớp này trên Brickwave."
            } else {
                "Xác nhận mã này trên điện thoại."
            },
            11.0,
            SECONDARY,
            false,
        );
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            countdown_badge(ui, login.expires_at_ms);
            ui.add_space(8.0);
            status_badge(ui, presentation);
        });
    } else {
        status_panel(ui, presentation);
    }

    if let Some(error) = state.auth_error() {
        ui.add_space(8.0);
        wrapped_label(ui, error, 11.0, ERROR, true);
    }

    ui.add_space(14.0);
    render_actions(ui, state, expired);
}

fn auth_presentation(state: AuthState, expired: bool) -> AuthPresentation {
    if expired || state == AuthState::TokenExpired {
        return AuthPresentation {
            step: AuthStep::Scan,
            status: "Đã hết hạn",
            status_icon: Glyph::Clock,
            status_color: ERROR,
            title: "Tạo mã QR mới",
            detail: "Phiên ghép nối trước đã hết hạn và không thể sử dụng lại.",
        };
    }

    match state {
        AuthState::CreatingQr => AuthPresentation {
            step: AuthStep::Scan,
            status: "Đang tạo mã QR",
            status_icon: Glyph::Refresh,
            status_color: ACCENT,
            title: "Kết nối tài khoản SoundCloud",
            detail: "Brickwave đang chuẩn bị một phiên ghép nối ngắn hạn.",
        },
        AuthState::WaitingForScan => AuthPresentation {
            step: AuthStep::Scan,
            status: "Đang chờ quét mã",
            status_icon: Glyph::Phone,
            status_color: ACCENT,
            title: "Kết nối tài khoản SoundCloud",
            detail: "Quét mã QR rồi đăng nhập trên trang SoundCloud chính thức.",
        },
        AuthState::WaitingForAuthorization => AuthPresentation {
            step: AuthStep::Confirm,
            status: "Đang chờ cấp quyền",
            status_icon: Glyph::Clock,
            status_color: ACCENT,
            title: "Hoàn tất đăng nhập trên điện thoại",
            detail: "Quay lại đây sau khi SoundCloud cấp quyền cho Brickwave.",
        },
        AuthState::PairingConfirmation => AuthPresentation {
            step: AuthStep::Confirm,
            status: "Đang chờ xác nhận",
            status_icon: Glyph::Check,
            status_color: ACCENT,
            title: "Xác nhận thiết bị này",
            detail: "SoundCloud đã cấp quyền. Chỉ xác nhận khi hai thiết bị hiển thị cùng một mã.",
        },
        AuthState::AuthorizationPending => AuthPresentation {
            step: AuthStep::Connected,
            status: "Đang kết nối",
            status_icon: Glyph::Connect,
            status_color: ACCENT,
            title: "Đang khôi phục tài khoản SoundCloud",
            detail: "Brickwave đang xác minh phiên được mã hóa đã lưu trên thiết bị.",
        },
        AuthState::Authorized => AuthPresentation {
            step: AuthStep::Connected,
            status: "Đã kết nối",
            status_icon: Glyph::Check,
            status_color: SUCCESS,
            title: "Đã kết nối SoundCloud",
            detail: "Tài khoản của bạn đã sẵn sàng.",
        },
        AuthState::Cancelled => AuthPresentation {
            step: AuthStep::Scan,
            status: "Đã hủy",
            status_icon: Glyph::Close,
            status_color: SECONDARY,
            title: "Kết nối tài khoản SoundCloud",
            detail: "Yêu cầu ghép nối trước đã bị hủy.",
        },
        AuthState::LoginFailed => AuthPresentation {
            step: AuthStep::Scan,
            status: "Lỗi",
            status_icon: Glyph::Warning,
            status_color: ERROR,
            title: "Đăng nhập SoundCloud thất bại",
            detail: "Kiểm tra thông báo bên dưới rồi tạo yêu cầu ghép nối mới.",
        },
        AuthState::RefreshRequired => AuthPresentation {
            step: AuthStep::Connected,
            status: "Lỗi",
            status_icon: Glyph::Warning,
            status_color: ERROR,
            title: "Kết nối lại tài khoản SoundCloud",
            detail: "Phiên tài khoản đã lưu không thể làm mới được nữa.",
        },
        AuthState::Unconfigured | AuthState::ServiceReady | AuthState::LoggedOut => {
            AuthPresentation {
                step: AuthStep::Scan,
                status: "Sẵn sàng kết nối",
                status_icon: Glyph::QrCode,
                status_color: SECONDARY,
                title: "Kết nối tài khoản SoundCloud",
                detail: "Dùng điện thoại để cấp quyền cho Brickwave mà không cần nhập trên máy.",
            }
        }
        AuthState::TokenExpired => unreachable!("handled before match"),
    }
}

fn progress_indicator(ui: &mut Ui, active: AuthStep, all_complete: bool) {
    let width = ui.available_width().max(240.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 52.0), Sense::hover());
    let painter = ui.painter();
    let labels = ["QUÉT", "XÁC NHẬN", "KẾT NỐI"];
    let active_index = match active {
        AuthStep::Scan => 0,
        AuthStep::Confirm => 1,
        AuthStep::Connected => 2,
    };
    let left = rect.left() + 30.0;
    let right = rect.right() - 42.0;
    let y = rect.top() + 14.0;
    let step_width = (right - left) * 0.5;

    painter.line_segment([pos2(left, y), pos2(right, y)], Stroke::new(1.0, SUBTLE));
    for (index, name) in labels.iter().enumerate() {
        let center = pos2(left + step_width * index as f32, y);
        let completed = all_complete || index < active_index;
        let current = !all_complete && index == active_index;
        let color = if completed || current {
            ACCENT
        } else {
            DISABLED
        };
        painter.circle_filled(center, 10.0, if current { WARM } else { SURFACE });
        painter.circle_stroke(center, 10.0, Stroke::new(1.3, color));
        if completed {
            IconRegistry::paint(
                painter,
                Glyph::Check,
                Rect::from_center_size(center, vec2(13.0, 13.0)),
                color,
            );
        } else {
            painter.circle_filled(center, 3.1, color);
        }
        painter.text(
            center + vec2(0.0, 19.0),
            Align2::CENTER_CENTER,
            *name,
            egui::FontId::proportional(font_size(9.0)),
            color,
        );
    }
}

fn confirmation_code(ui: &mut Ui, code: &str) {
    label(ui, "MÃ XÁC NHẬN THIẾT BỊ", 10.0, SECONDARY, true);
    ui.add_space(5.0);
    let display = code
        .chars()
        .map(|ch| ch.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 58.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(SMALL_RADIUS),
        RAISED,
        Stroke::new(1.0, ACCENT),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        display,
        egui::FontId::monospace(28.0),
        ACCENT,
    );
}

fn countdown_badge(ui: &mut Ui, expires_at_ms: u64) {
    let seconds = expires_at_ms.saturating_sub(now_ms()) / 1000;
    let label = format!("Hết hạn sau {}:{:02}", seconds / 60, seconds % 60);
    badge(ui, Glyph::Clock, &label, SECONDARY, 126.0);
}

fn status_badge(ui: &mut Ui, presentation: AuthPresentation) {
    let width = (ui.available_width() - 1.0).max(150.0);
    badge(
        ui,
        presentation.status_icon,
        presentation.status,
        presentation.status_color,
        width,
    );
}

fn badge(ui: &mut Ui, icon: Glyph, value: &str, color: Color32, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 30.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(15),
        APP_BG,
        Stroke::new(1.0, SUBTLE),
        egui::StrokeKind::Inside,
    );
    IconRegistry::paint(
        ui.painter(),
        icon,
        Rect::from_center_size(pos2(rect.left() + 16.0, rect.center().y), vec2(15.0, 15.0)),
        color,
    );
    ui.painter().text(
        pos2(rect.left() + 29.0, rect.center().y),
        Align2::LEFT_CENTER,
        value,
        egui::FontId::proportional(font_size(10.5)),
        color,
    );
}

fn status_panel(ui: &mut Ui, presentation: AuthPresentation) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(SMALL_RADIUS),
        APP_BG,
        Stroke::new(1.0, SUBTLE),
        egui::StrokeKind::Inside,
    );
    IconRegistry::paint(
        ui.painter(),
        presentation.status_icon,
        Rect::from_center_size(pos2(rect.left() + 24.0, rect.center().y), vec2(20.0, 20.0)),
        presentation.status_color,
    );
    ui.painter().text(
        pos2(rect.left() + 44.0, rect.center().y),
        Align2::LEFT_CENTER,
        presentation.status,
        egui::FontId::proportional(font_size(12.0)),
        presentation.status_color,
    );
}

fn render_actions(ui: &mut Ui, state: &mut UiState, expired: bool) {
    let auth_state = state.auth_state();
    ui.horizontal(|ui| match auth_state {
        AuthState::CreatingQr => {
            if auth_button(ui, "auth-cancel", Glyph::Close, "HỦY", false, true).clicked() {
                state.cancel_login_and_return();
            }
        }
        AuthState::WaitingForScan | AuthState::WaitingForAuthorization if !expired => {
            if auth_button(
                ui,
                "auth-refresh",
                Glyph::Refresh,
                "TẠO LẠI QR",
                false,
                true,
            )
            .clicked()
            {
                restart_qr_login(state);
            }
            if auth_button(ui, "auth-cancel", Glyph::Close, "HỦY", false, true).clicked() {
                state.cancel_login_and_return();
            }
        }
        AuthState::PairingConfirmation if !expired => {
            if auth_button(
                ui,
                "auth-confirm",
                Glyph::Check,
                "XÁC NHẬN THIẾT BỊ",
                true,
                true,
            )
            .clicked()
            {
                state.confirm_pairing();
            }
            if auth_button(ui, "auth-cancel", Glyph::Close, "HỦY", false, true).clicked() {
                state.cancel_login_and_return();
            }
        }
        AuthState::AuthorizationPending | AuthState::Authorized => {}
        _ => {
            if auth_button(
                ui,
                "auth-generate",
                Glyph::Refresh,
                if expired {
                    "TẠO MÃ QR MỚI"
                } else {
                    "KẾT NỐI SOUNDCLOUD"
                },
                true,
                true,
            )
            .clicked()
            {
                if state.qr_login().is_some() {
                    restart_qr_login(state);
                } else {
                    state.start_qr_login();
                }
            }
        }
    });
}

fn restart_qr_login(state: &mut UiState) {
    state.cancel_qr_login();
    state.start_qr_login();
}

fn auth_button(
    ui: &mut Ui,
    id: &'static str,
    icon: Glyph,
    value: &str,
    primary: bool,
    enabled: bool,
) -> Response {
    let width = (value.len() as f32 * 7.4 + 50.0).max(112.0);
    let (rect, response) = ui.allocate_exact_size(
        vec2(width, 40.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let fill = if !enabled {
        SURFACE
    } else if primary {
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
    let border = if response.has_focus() || primary {
        ACCENT
    } else if response.hovered() {
        ACCENT_HOVER
    } else {
        BORDER
    };
    let color = if !enabled {
        DISABLED
    } else if primary {
        APP_BG
    } else {
        TEXT
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(SMALL_RADIUS),
        fill,
        Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    IconRegistry::paint(
        ui.painter(),
        icon,
        Rect::from_center_size(pos2(rect.left() + 20.0, rect.center().y), vec2(16.0, 16.0)),
        color,
    );
    ui.painter().text(
        pos2(rect.left() + 34.0, rect.center().y),
        Align2::LEFT_CENTER,
        value,
        egui::FontId::proportional(font_size(11.0)),
        color,
    );
    let _ = id;
    response
}

fn security_note(ui: &mut Ui) {
    Frame::new()
        .fill(APP_BG)
        .stroke(Stroke::new(1.0, SUBTLE))
        .corner_radius(CornerRadius::same(SMALL_RADIUS))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (icon_rect, _) = ui.allocate_exact_size(vec2(18.0, 30.0), Sense::hover());
                IconRegistry::paint(
                    ui.painter(),
                    Glyph::Lock,
                    Rect::from_center_size(icon_rect.center(), vec2(16.0, 16.0)),
                    ACCENT,
                );
                wrapped_label(
                    ui,
                    "Đăng nhập trên trang SoundCloud chính thức.\nMật khẩu của bạn không bao giờ được gửi vào Brickwave.",
                    10.5,
                    SECONDARY,
                    false,
                );
            });
        });
}

fn paint_qr(ui: &mut Ui, value: &str, target_size: f32) {
    let Ok(code) = QrCode::encode_text(value, QrCodeEcc::Medium) else {
        paint_qr_placeholder(ui, target_size, false);
        return;
    };
    const QUIET_ZONE: i32 = 4;
    let modules = code.size() + QUIET_ZONE * 2;
    let module_size = (target_size / modules as f32).floor().max(3.0);
    let actual_size = module_size * modules as f32;
    let (rect, _) = ui.allocate_exact_size(vec2(actual_size, actual_size), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, Color32::WHITE);
    for y in 0..code.size() {
        for x in 0..code.size() {
            if code.get_module(x, y) {
                let min = rect.min
                    + vec2(
                        (x + QUIET_ZONE) as f32 * module_size,
                        (y + QUIET_ZONE) as f32 * module_size,
                    );
                ui.painter().rect_filled(
                    Rect::from_min_size(min, vec2(module_size, module_size)),
                    CornerRadius::ZERO,
                    Color32::BLACK,
                );
            }
        }
    }
}

fn paint_qr_placeholder(ui: &mut Ui, size: f32, expired: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(SMALL_RADIUS),
        APP_BG,
        Stroke::new(1.0, if expired { ERROR } else { BORDER }),
        egui::StrokeKind::Inside,
    );
    IconRegistry::paint(
        ui.painter(),
        if expired { Glyph::Clock } else { Glyph::QrCode },
        Rect::from_center_size(rect.center() - vec2(0.0, 13.0), vec2(48.0, 48.0)),
        if expired { ERROR } else { SECONDARY },
    );
    ui.painter().text(
        rect.center() + vec2(0.0, 28.0),
        Align2::CENTER_CENTER,
        if expired {
            "QR ĐÃ HẾT HẠN"
        } else {
            "CHƯA TẠO MÃ QR"
        },
        egui::FontId::proportional(font_size(10.0)),
        if expired { ERROR } else { SECONDARY },
    );
}

fn label(ui: &mut Ui, value: &str, size: f32, color: Color32, strong: bool) {
    let mut text = RichText::new(value).size(font_size(size)).color(color);
    if strong {
        text = text.strong();
    }
    ui.label(text);
}

fn wrapped_label(ui: &mut Ui, value: &str, size: f32, color: Color32, strong: bool) {
    let mut text = RichText::new(value).size(font_size(size)).color(color);
    if strong {
        text = text.strong();
    }
    ui.add(egui::Label::new(text).wrap());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_card_fits_both_required_viewports() {
        // LoginScreen owns the complete viewport; only its own header/footer
        // are removed before centering the card.
        for viewport in [vec2(1024.0, 768.0), vec2(1280.0, 960.0)] {
            let available = vec2(viewport.x, viewport.y - HEADER_HEIGHT - FOOTER_HEIGHT);
            let layout = auth_layout(available);
            assert!(layout.card_width <= available.x);
            assert!(layout.card_height <= available.y);
            assert!(layout.wide);
            assert!(layout.qr_size >= 218.0);
        }
    }

    #[test]
    fn narrow_layout_stacks_without_exceeding_available_width() {
        let available = vec2(620.0, 720.0);
        let layout = auth_layout(available);
        assert!(!layout.wide);
        assert!(layout.card_width <= available.x);
        assert!(layout.qr_size <= layout.qr_column_width);
    }

    #[test]
    fn countdown_uses_the_session_expiry() {
        let login = QrLoginSession {
            pairing_id: "redacted-pairing-id".to_owned(),
            pairing_url: "https://brickwave.example/auth/connect?pairing=redacted".to_owned(),
            device_secret: "redacted-device-proof".to_owned(),
            confirmation_code: "123456".to_owned(),
            expires_at_ms: 20_000,
        };
        assert!(!login_is_expired(&login, 19_999));
        assert!(login_is_expired(&login, 20_000));
    }

    #[test]
    fn pairing_url_encodes_locally_with_a_four_module_quiet_zone() {
        let value =
            "https://brickwave-api.example/auth/connect?pairing=abcdefghijklmnopqrstuvwxyz012345";
        let code = QrCode::encode_text(value, QrCodeEcc::Medium).expect("pairing URL fits QR");
        assert!(code.size() >= 21);
        assert!((0..code.size()).any(|y| (0..code.size()).any(|x| code.get_module(x, y))));
        const QUIET_ZONE: i32 = 4;
        assert_eq!(QUIET_ZONE, 4);
    }

    #[test]
    fn progress_reflects_real_auth_states() {
        assert_eq!(
            auth_presentation(AuthState::WaitingForScan, false).step,
            AuthStep::Scan
        );
        assert_eq!(
            auth_presentation(AuthState::PairingConfirmation, false).step,
            AuthStep::Confirm
        );
        assert_eq!(
            auth_presentation(AuthState::AuthorizationPending, false).step,
            AuthStep::Connected
        );
        assert_eq!(
            auth_presentation(AuthState::WaitingForScan, true).status,
            "Đã hết hạn"
        );
    }
}
