//! Bottom-anchored virtual keyboard with deterministic D-pad navigation.

use std::time::Duration;

use egui::{
    Align2, Color32, CornerRadius, FontId, Id, Key, Modifiers, Order, Rect, Sense, Stroke, Ui,
    pos2, vec2,
};

const PANEL_MARGIN_PX: f32 = 16.0;
const PANEL_PADDING_PX: f32 = 14.0;
const KEY_GAP_PX: f32 = 8.0;
const KEY_HEIGHT_PX: f32 = 52.0;
const ROW_GAP_PX: f32 = 8.0;
const TARGET_PANEL_HEIGHT_PX: f32 = 288.0;
const MAX_PANEL_HEIGHT_FRACTION: f32 = 0.40;
const INITIAL_REPEAT_DELAY: Duration = Duration::from_millis(300);
const REPEAT_INTERVAL: Duration = Duration::from_millis(90);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyboardAction {
    Insert(String),
    Backspace,
    Clear,
    Submit,
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyboardConfig {
    pub submit_label: String,
}

impl Default for KeyboardConfig {
    fn default() -> Self {
        Self {
            submit_label: "Done".to_owned(),
        }
    }
}

impl KeyboardConfig {
    pub fn search() -> Self {
        Self {
            submit_label: "Search".to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct KeyboardTheme {
    pub panel: Color32,
    pub panel_border: Color32,
    pub key: Color32,
    pub key_hover: Color32,
    pub key_pressed: Color32,
    pub action_key: Color32,
    pub selected_key: Color32,
    pub selected_border: Color32,
    pub confirm_key: Color32,
    pub text: Color32,
    pub selected_text: Color32,
}

impl Default for KeyboardTheme {
    fn default() -> Self {
        Self {
            panel: Color32::from_rgb(25, 31, 42),
            panel_border: Color32::from_rgb(81, 96, 120),
            key: Color32::from_rgb(49, 61, 79),
            key_hover: Color32::from_rgb(68, 86, 110),
            key_pressed: Color32::from_rgb(88, 109, 138),
            action_key: Color32::from_rgb(62, 75, 94),
            selected_key: Color32::from_rgb(26, 118, 92),
            selected_border: Color32::WHITE,
            confirm_key: Color32::from_rgb(24, 139, 84),
            text: Color32::from_rgb(242, 246, 250),
            selected_text: Color32::WHITE,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct KeyboardLayout {
    pub screen_width: f32,
    pub screen_height: f32,
    pub pixels_per_point: f32,
    pub panel_x: f32,
    pub panel_y: f32,
    pub panel_width: f32,
    pub panel_height: f32,
    pub row_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyboardControl {
    DpadX(i8),
    DpadY(i8),
    Primary(bool),
    Back,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug)]
struct KeyboardGeometry {
    panel_rect: Rect,
    inner_rect: Rect,
    key_height: f32,
    row_gap: f32,
    pixels_per_point: f32,
}

impl KeyboardGeometry {
    fn report(self, viewport: Rect) -> KeyboardLayout {
        let scale = self.pixels_per_point;
        KeyboardLayout {
            screen_width: viewport.width() * scale,
            screen_height: viewport.height() * scale,
            pixels_per_point: scale,
            panel_x: self.panel_rect.left() * scale,
            panel_y: self.panel_rect.top() * scale,
            panel_width: self.panel_rect.width() * scale,
            panel_height: self.panel_rect.height() * scale,
            row_count: 4,
        }
    }
}

#[derive(Clone, Copy)]
enum KeyKind {
    Letter(&'static str),
    Text(&'static str),
    Shift,
    ToggleSymbols,
    Space,
    Backspace,
    Clear,
    Submit,
    Close,
}

#[derive(Clone, Copy)]
struct KeySpec {
    label: &'static str,
    weight: f32,
    kind: KeyKind,
    style: KeyStyle,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum KeyStyle {
    Normal,
    Action,
    Confirm,
}

const fn key(label: &'static str, kind: KeyKind) -> KeySpec {
    KeySpec {
        label,
        weight: 1.0,
        kind,
        style: KeyStyle::Normal,
    }
}

const fn wide_key(label: &'static str, weight: f32, kind: KeyKind, style: KeyStyle) -> KeySpec {
    KeySpec {
        label,
        weight,
        kind,
        style,
    }
}

const LETTER_ROW_0: [KeySpec; 10] = [
    key("Q", KeyKind::Letter("Q")),
    key("W", KeyKind::Letter("W")),
    key("E", KeyKind::Letter("E")),
    key("R", KeyKind::Letter("R")),
    key("T", KeyKind::Letter("T")),
    key("Y", KeyKind::Letter("Y")),
    key("U", KeyKind::Letter("U")),
    key("I", KeyKind::Letter("I")),
    key("O", KeyKind::Letter("O")),
    key("P", KeyKind::Letter("P")),
];
const LETTER_ROW_1: [KeySpec; 9] = [
    key("A", KeyKind::Letter("A")),
    key("S", KeyKind::Letter("S")),
    key("D", KeyKind::Letter("D")),
    key("F", KeyKind::Letter("F")),
    key("G", KeyKind::Letter("G")),
    key("H", KeyKind::Letter("H")),
    key("J", KeyKind::Letter("J")),
    key("K", KeyKind::Letter("K")),
    key("L", KeyKind::Letter("L")),
];
const LETTER_ROW_2: [KeySpec; 9] = [
    wide_key("Shift", 1.5, KeyKind::Shift, KeyStyle::Action),
    key("Z", KeyKind::Letter("Z")),
    key("X", KeyKind::Letter("X")),
    key("C", KeyKind::Letter("C")),
    key("V", KeyKind::Letter("V")),
    key("B", KeyKind::Letter("B")),
    key("N", KeyKind::Letter("N")),
    key("M", KeyKind::Letter("M")),
    wide_key("Backspace", 1.8, KeyKind::Backspace, KeyStyle::Action),
];
const SYMBOL_ROW_0: [KeySpec; 10] = [
    key("1", KeyKind::Text("1")),
    key("2", KeyKind::Text("2")),
    key("3", KeyKind::Text("3")),
    key("4", KeyKind::Text("4")),
    key("5", KeyKind::Text("5")),
    key("6", KeyKind::Text("6")),
    key("7", KeyKind::Text("7")),
    key("8", KeyKind::Text("8")),
    key("9", KeyKind::Text("9")),
    key("0", KeyKind::Text("0")),
];
const SYMBOL_ROW_1: [KeySpec; 10] = [
    key("!", KeyKind::Text("!")),
    key("@", KeyKind::Text("@")),
    key("#", KeyKind::Text("#")),
    key("$", KeyKind::Text("$")),
    key("%", KeyKind::Text("%")),
    key("^", KeyKind::Text("^")),
    key("&", KeyKind::Text("&")),
    key("*", KeyKind::Text("*")),
    key("(", KeyKind::Text("(")),
    key(")", KeyKind::Text(")")),
];
const SYMBOL_ROW_2: [KeySpec; 9] = [
    key("-", KeyKind::Text("-")),
    key("_", KeyKind::Text("_")),
    key("=", KeyKind::Text("=")),
    key("+", KeyKind::Text("+")),
    key("[", KeyKind::Text("[")),
    key("]", KeyKind::Text("]")),
    key("{", KeyKind::Text("{")),
    key("}", KeyKind::Text("}")),
    wide_key("Backspace", 1.8, KeyKind::Backspace, KeyStyle::Action),
];
const BOTTOM_ROW: [KeySpec; 5] = [
    wide_key("123", 1.10, KeyKind::ToggleSymbols, KeyStyle::Action),
    wide_key("Space", 3.40, KeyKind::Space, KeyStyle::Normal),
    wide_key("Clear", 1.15, KeyKind::Clear, KeyStyle::Action),
    wide_key("Submit", 1.50, KeyKind::Submit, KeyStyle::Confirm),
    wide_key("Close", 1.35, KeyKind::Close, KeyStyle::Action),
];

pub struct VirtualKeyboard {
    open: bool,
    shifted: bool,
    symbols: bool,
    selected_row: usize,
    selected_col: usize,
    dpad_x: i8,
    dpad_y: i8,
    held_direction: Option<Direction>,
    repeat_elapsed: Duration,
    next_repeat: Duration,
    primary_down: bool,
    layout_report_pending: bool,
    pending: Vec<KeyboardAction>,
    config: KeyboardConfig,
    theme: KeyboardTheme,
}

impl Default for VirtualKeyboard {
    fn default() -> Self {
        Self::new(KeyboardTheme::default())
    }
}

impl VirtualKeyboard {
    pub fn new(theme: KeyboardTheme) -> Self {
        Self {
            open: false,
            shifted: false,
            symbols: false,
            selected_row: 0,
            selected_col: 0,
            dpad_x: 0,
            dpad_y: 0,
            held_direction: None,
            repeat_elapsed: Duration::ZERO,
            next_repeat: INITIAL_REPEAT_DELAY,
            primary_down: false,
            layout_report_pending: false,
            pending: Vec::new(),
            config: KeyboardConfig::default(),
            theme,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn selected_key(&self) -> (usize, usize) {
        (self.selected_row, self.selected_col)
    }

    pub fn has_held_direction(&self) -> bool {
        self.open && self.held_direction.is_some()
    }

    pub fn open(&mut self, config: KeyboardConfig) -> bool {
        if self.open {
            return false;
        }
        self.open = true;
        self.shifted = false;
        self.symbols = false;
        self.selected_row = 0;
        self.selected_col = 0;
        self.config = config;
        self.layout_report_pending = true;
        self.reset_controls();
        true
    }

    pub fn close(&mut self) -> bool {
        if !self.open {
            return false;
        }
        self.open = false;
        self.shifted = false;
        self.layout_report_pending = false;
        self.reset_controls();
        true
    }

    pub fn request_close(&mut self) -> bool {
        if self.close() {
            self.pending.push(KeyboardAction::Close);
            true
        } else {
            false
        }
    }

    pub fn take_actions(&mut self) -> Vec<KeyboardAction> {
        std::mem::take(&mut self.pending)
    }

    /// Returns `true` when the keyboard consumed the handheld control.
    pub fn handle_control(&mut self, control: KeyboardControl) -> bool {
        if !self.open {
            return false;
        }
        match control {
            KeyboardControl::DpadX(value) => {
                self.dpad_x = value.clamp(-1, 1);
                self.refresh_direction(true);
            }
            KeyboardControl::DpadY(value) => {
                self.dpad_y = value.clamp(-1, 1);
                self.refresh_direction(true);
            }
            KeyboardControl::Primary(pressed) => {
                if pressed {
                    self.primary_down = true;
                } else if self.primary_down {
                    self.primary_down = false;
                    self.activate_selected();
                }
            }
            KeyboardControl::Back => {
                self.request_close();
            }
        }
        true
    }

    pub fn advance(&mut self, delta: Duration) {
        let Some(direction) = self.held_direction else {
            return;
        };
        self.repeat_elapsed = self.repeat_elapsed.saturating_add(delta);
        let mut repeats = 0;
        while self.repeat_elapsed >= self.next_repeat && repeats < 4 {
            self.move_selection(direction);
            self.next_repeat = self.next_repeat.saturating_add(REPEAT_INTERVAL);
            repeats += 1;
        }
    }

    /// Desktop preview mapping: arrows navigate, Enter activates and Escape closes.
    pub fn handle_egui_controls(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        let (left, right, up, down, activate, close) = ctx.input_mut(|input| {
            (
                input.count_and_consume_key(Modifiers::NONE, Key::ArrowLeft),
                input.count_and_consume_key(Modifiers::NONE, Key::ArrowRight),
                input.count_and_consume_key(Modifiers::NONE, Key::ArrowUp),
                input.count_and_consume_key(Modifiers::NONE, Key::ArrowDown),
                input.consume_key(Modifiers::NONE, Key::Enter),
                input.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        for _ in 0..left {
            self.move_selection(Direction::Left);
        }
        for _ in 0..right {
            self.move_selection(Direction::Right);
        }
        for _ in 0..up {
            self.move_selection(Direction::Up);
        }
        for _ in 0..down {
            self.move_selection(Direction::Down);
        }
        if activate {
            self.activate_selected();
        }
        if close {
            self.request_close();
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<KeyboardLayout> {
        if !self.open {
            return None;
        }
        let viewport = ctx.content_rect();
        let geometry = calculate_geometry(viewport, ctx.pixels_per_point());
        let panel_rect = geometry.panel_rect;
        let report = self
            .layout_report_pending
            .then(|| geometry.report(viewport));
        self.layout_report_pending = false;

        egui::Area::new(Id::new("trimui-virtual-keyboard-panel"))
            .order(Order::Foreground)
            .interactable(true)
            .movable(false)
            .fixed_pos(panel_rect.min)
            .show(ctx, |ui| {
                ui.set_min_size(panel_rect.size());
                ui.set_max_size(panel_rect.size());
                let actual_panel = ui.max_rect();
                ui.painter()
                    .rect_filled(actual_panel, CornerRadius::same(12), self.theme.panel);
                ui.painter().rect_stroke(
                    actual_panel,
                    CornerRadius::same(12),
                    Stroke::new(1.0, self.theme.panel_border),
                    egui::StrokeKind::Inside,
                );
                let offset = actual_panel.min - panel_rect.min;
                let mut keyboard_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(geometry.inner_rect.translate(offset))
                        .layout(egui::Layout::top_down(egui::Align::Center)),
                );
                if let Some((row, col)) = draw_keyboard(&mut keyboard_ui, self, geometry) {
                    self.selected_row = row;
                    self.selected_col = col;
                    self.activate_selected();
                }
            });
        report
    }

    fn reset_controls(&mut self) {
        self.dpad_x = 0;
        self.dpad_y = 0;
        self.held_direction = None;
        self.repeat_elapsed = Duration::ZERO;
        self.next_repeat = INITIAL_REPEAT_DELAY;
        self.primary_down = false;
    }

    fn refresh_direction(&mut self, move_immediately: bool) {
        let direction = if self.dpad_x < 0 {
            Some(Direction::Left)
        } else if self.dpad_x > 0 {
            Some(Direction::Right)
        } else if self.dpad_y < 0 {
            Some(Direction::Up)
        } else if self.dpad_y > 0 {
            Some(Direction::Down)
        } else {
            None
        };
        if direction != self.held_direction {
            self.held_direction = direction;
            self.repeat_elapsed = Duration::ZERO;
            self.next_repeat = INITIAL_REPEAT_DELAY;
            if move_immediately {
                if let Some(direction) = direction {
                    self.move_selection(direction);
                }
            }
        }
    }

    fn move_selection(&mut self, direction: Direction) {
        match direction {
            Direction::Left => self.selected_col = self.selected_col.saturating_sub(1),
            Direction::Right => {
                self.selected_col =
                    (self.selected_col + 1).min(self.row(self.selected_row).len() - 1)
            }
            Direction::Up | Direction::Down => {
                let next_row = match direction {
                    Direction::Up => self.selected_row.saturating_sub(1),
                    Direction::Down => (self.selected_row + 1).min(3),
                    _ => unreachable!(),
                };
                if next_row != self.selected_row {
                    let center = key_center(self.row(self.selected_row), self.selected_col);
                    self.selected_row = next_row;
                    self.selected_col = nearest_key(self.row(next_row), center);
                }
            }
        }
    }

    fn activate_selected(&mut self) {
        let kind = self.row(self.selected_row)[self.selected_col].kind;
        match kind {
            KeyKind::Letter(label) => {
                let text = if self.shifted {
                    label.to_owned()
                } else {
                    label.to_ascii_lowercase()
                };
                self.shifted = false;
                self.pending.push(KeyboardAction::Insert(text));
            }
            KeyKind::Text(text) => self.pending.push(KeyboardAction::Insert(text.to_owned())),
            KeyKind::Shift => self.shifted = !self.shifted,
            KeyKind::ToggleSymbols => {
                let center = key_center(self.row(self.selected_row), self.selected_col);
                self.symbols = !self.symbols;
                self.shifted = false;
                self.selected_col = nearest_key(self.row(self.selected_row), center);
            }
            KeyKind::Space => self.pending.push(KeyboardAction::Insert(" ".to_owned())),
            KeyKind::Backspace => self.pending.push(KeyboardAction::Backspace),
            KeyKind::Clear => self.pending.push(KeyboardAction::Clear),
            KeyKind::Submit => {
                self.close();
                self.pending.push(KeyboardAction::Submit);
            }
            KeyKind::Close => {
                self.close();
                self.pending.push(KeyboardAction::Close);
            }
        }
    }

    fn row(&self, row: usize) -> &[KeySpec] {
        if row == 3 {
            &BOTTOM_ROW
        } else if self.symbols {
            match row {
                0 => &SYMBOL_ROW_0,
                1 => &SYMBOL_ROW_1,
                _ => &SYMBOL_ROW_2,
            }
        } else {
            match row {
                0 => &LETTER_ROW_0,
                1 => &LETTER_ROW_1,
                _ => &LETTER_ROW_2,
            }
        }
    }
}

fn calculate_geometry(viewport: Rect, pixels_per_point: f32) -> KeyboardGeometry {
    let scale = pixels_per_point.max(0.1);
    let margin = (PANEL_MARGIN_PX / scale).min(viewport.width() * 0.10);
    let panel_height = (TARGET_PANEL_HEIGHT_PX / scale)
        .min(viewport.height() * MAX_PANEL_HEIGHT_FRACTION)
        .min(viewport.height())
        .max(1.0);
    let panel_rect = Rect::from_min_size(
        pos2(viewport.left() + margin, viewport.bottom() - panel_height),
        vec2((viewport.width() - margin * 2.0).max(1.0), panel_height),
    );
    let padding = (PANEL_PADDING_PX / scale).min(panel_rect.width() * 0.10);
    let inner_rect = panel_rect.shrink2(vec2(padding, padding.min(panel_rect.height() * 0.20)));
    let row_gap = (ROW_GAP_PX / scale).min(inner_rect.height() / 12.0);
    let available_key_height = (inner_rect.height() - row_gap * 3.0).max(4.0);
    let key_height = (KEY_HEIGHT_PX / scale).min(available_key_height / 4.0);
    KeyboardGeometry {
        panel_rect,
        inner_rect,
        key_height,
        row_gap,
        pixels_per_point: scale,
    }
}

fn draw_keyboard(
    ui: &mut Ui,
    keyboard: &VirtualKeyboard,
    geometry: KeyboardGeometry,
) -> Option<(usize, usize)> {
    let mut clicked = None;
    for row_index in 0..4 {
        let row = keyboard.row(row_index);
        let rect = keyboard_row_rect(ui.max_rect(), row_index, geometry);
        if let Some(column) = draw_row(ui, rect, row_index, row, keyboard) {
            clicked = Some((row_index, column));
        }
    }
    clicked
}

fn keyboard_row_rect(inner: Rect, row: usize, geometry: KeyboardGeometry) -> Rect {
    let rows_height = geometry.key_height * 4.0 + geometry.row_gap * 3.0;
    let top = inner.center().y - rows_height / 2.0
        + row as f32 * (geometry.key_height + geometry.row_gap);
    Rect::from_min_size(
        pos2(inner.left(), top),
        vec2(inner.width(), geometry.key_height),
    )
}

fn draw_row(
    ui: &mut Ui,
    row_rect: Rect,
    row_index: usize,
    row: &[KeySpec],
    keyboard: &VirtualKeyboard,
) -> Option<usize> {
    let gap = KEY_GAP_PX / ui.ctx().pixels_per_point().max(0.1);
    let usable = row_rect.width() - gap * row.len().saturating_sub(1) as f32;
    let total_weight: f32 = row.iter().map(|key| key.weight).sum();
    let mut x = row_rect.left();
    let mut clicked = None;
    for (column, spec) in row.iter().enumerate() {
        let width = usable.max(0.0) * spec.weight / total_weight;
        let rect = Rect::from_min_size(pos2(x, row_rect.top()), vec2(width, row_rect.height()));
        let label = match spec.kind {
            KeyKind::ToggleSymbols if keyboard.symbols => "ABC",
            KeyKind::Submit => keyboard.config.submit_label.as_str(),
            _ => spec.label,
        };
        if key_button(
            ui,
            rect,
            Id::new("trimui-virtual-keyboard-key")
                .with(row_index)
                .with(column),
            label,
            spec.style,
            keyboard.selected_row == row_index && keyboard.selected_col == column,
            keyboard.shifted && matches!(spec.kind, KeyKind::Shift),
            keyboard.theme,
        ) {
            clicked = Some(column);
        }
        x += width + gap;
    }
    clicked
}

#[allow(clippy::too_many_arguments)]
fn key_button(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    label: &str,
    style: KeyStyle,
    selected: bool,
    latched: bool,
    theme: KeyboardTheme,
) -> bool {
    let response = ui.interact(rect, id, Sense::click());
    let base = if selected || latched {
        theme.selected_key
    } else {
        match style {
            KeyStyle::Normal => theme.key,
            KeyStyle::Action => theme.action_key,
            KeyStyle::Confirm => theme.confirm_key,
        }
    };
    let fill = if response.is_pointer_button_down_on() {
        theme.key_pressed
    } else if response.hovered() {
        theme.key_hover
    } else {
        base
    };
    ui.painter().rect_filled(rect, CornerRadius::same(7), fill);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(7),
        Stroke::new(
            if selected { 2.0 } else { 1.0 },
            if selected {
                theme.selected_border
            } else {
                theme.panel_border
            },
        ),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional((rect.height() * 0.34).clamp(12.0, 18.0)),
        if selected || latched {
            theme.selected_text
        } else {
            theme.text
        },
    );
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

fn key_center(row: &[KeySpec], index: usize) -> f32 {
    let total: f32 = row.iter().map(|key| key.weight).sum();
    let before: f32 = row.iter().take(index).map(|key| key.weight).sum();
    (before + row[index].weight * 0.5) / total.max(f32::EPSILON)
}

fn nearest_key(row: &[KeySpec], center: f32) -> usize {
    row.iter()
        .enumerate()
        .min_by(|(left, _), (right, _)| {
            (key_center(row, *left) - center)
                .abs()
                .total_cmp(&(key_center(row, *right) - center).abs())
        })
        .map(|(index, _)| index)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_keyboard() -> VirtualKeyboard {
        let mut keyboard = VirtualKeyboard::default();
        assert!(keyboard.open(KeyboardConfig::search()));
        keyboard
    }

    #[test]
    fn panel_is_bottom_anchored_at_1024_by_768() {
        let viewport = Rect::from_min_size(pos2(0.0, 0.0), vec2(1024.0, 768.0));
        let geometry = calculate_geometry(viewport, 1.0);
        assert_eq!(geometry.panel_rect.left(), 16.0);
        assert_eq!(geometry.panel_rect.top(), 480.0);
        assert_eq!(geometry.panel_rect.width(), 992.0);
        assert_eq!(geometry.panel_rect.height(), 288.0);
    }

    #[test]
    fn dpad_does_not_wrap_at_row_edges() {
        let mut keyboard = open_keyboard();
        keyboard.handle_control(KeyboardControl::DpadX(-1));
        keyboard.handle_control(KeyboardControl::DpadX(0));
        assert_eq!(keyboard.selected_key(), (0, 0));
        for _ in 0..15 {
            keyboard.handle_control(KeyboardControl::DpadX(1));
            keyboard.handle_control(KeyboardControl::DpadX(0));
        }
        assert_eq!(keyboard.selected_key(), (0, 9));
    }

    #[test]
    fn vertical_navigation_uses_nearest_key_center() {
        let mut keyboard = open_keyboard();
        for _ in 0..9 {
            keyboard.handle_control(KeyboardControl::DpadX(1));
            keyboard.handle_control(KeyboardControl::DpadX(0));
        }
        keyboard.handle_control(KeyboardControl::DpadY(1));
        keyboard.handle_control(KeyboardControl::DpadY(0));
        assert_eq!(keyboard.selected_key(), (1, 8));
    }

    #[test]
    fn primary_activates_once_on_release() {
        let mut keyboard = open_keyboard();
        assert!(keyboard.handle_control(KeyboardControl::Primary(true)));
        assert!(keyboard.take_actions().is_empty());
        assert!(keyboard.handle_control(KeyboardControl::Primary(false)));
        assert_eq!(
            keyboard.take_actions(),
            vec![KeyboardAction::Insert("q".to_owned())]
        );
        keyboard.handle_control(KeyboardControl::Primary(false));
        assert!(keyboard.take_actions().is_empty());
    }

    #[test]
    fn back_is_consumed_and_closes_keyboard() {
        let mut keyboard = open_keyboard();
        assert!(keyboard.handle_control(KeyboardControl::Back));
        assert!(!keyboard.is_open());
        assert_eq!(keyboard.take_actions(), vec![KeyboardAction::Close]);
    }

    #[test]
    fn held_direction_repeats_after_delay() {
        let mut keyboard = open_keyboard();
        keyboard.handle_control(KeyboardControl::DpadX(1));
        assert_eq!(keyboard.selected_key(), (0, 1));
        keyboard.advance(Duration::from_millis(299));
        assert_eq!(keyboard.selected_key(), (0, 1));
        keyboard.advance(Duration::from_millis(1));
        assert_eq!(keyboard.selected_key(), (0, 2));
        keyboard.advance(Duration::from_millis(90));
        assert_eq!(keyboard.selected_key(), (0, 3));
    }

    #[test]
    fn controls_pass_through_while_closed() {
        let mut keyboard = VirtualKeyboard::default();
        assert!(!keyboard.handle_control(KeyboardControl::Back));
        assert!(!keyboard.handle_control(KeyboardControl::Primary(true)));
    }
}
