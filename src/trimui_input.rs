//! TrimUI Brick Pro input adapter shared by the SDL2 host.
//!
//! The stock input device exposes the face buttons and axes through Linux
//! evdev. SDL still handles ordinary keyboard/mouse events, while this module
//! turns the built-in controls into a single virtual mouse stream.

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::time::Duration;

pub const WIDTH: i32 = 1024;
pub const HEIGHT: i32 = 768;

const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const BTN_SOUTH: u16 = 0x130;
const BTN_EAST: u16 = 0x131;
const BTN_NORTH: u16 = 0x133;
const BTN_TL: u16 = 0x136;
const BTN_TR: u16 = 0x137;
const BTN_SELECT: u16 = 0x13a;
const BTN_START: u16 = 0x13b;
const BTN_MODE: u16 = 0x13c;
const KEY_UP: u16 = 103;
const KEY_DOWN: u16 = 108;
const KEY_LEFT: u16 = 105;
const KEY_RIGHT: u16 = 106;
const KEY_POWER: u16 = 116;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_HAT0X: u16 = 0x10;
const ABS_HAT0Y: u16 = 0x11;
const ANALOG_RAW_MAX: f32 = 32_760.0;
const ANALOG_DEADZONE: f32 = 0.15;
const ANALOG_MIN_SPEED: f32 = 110.0;
const ANALOG_MAX_SPEED: f32 = 1_150.0;
const DPAD_SCROLL_LINES_PER_SECOND: f32 = 18.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrimuiAction {
    Back,
    Menu,
    Power(bool),
    Stop,
    Start,
    Previous,
    Next,
    Primary(bool),
    SetStickX(i32),
    SetStickY(i32),
    SetDpadX(i8),
    SetDpadY(i8),
}

pub struct TrimuiInput {
    devices: Vec<InputDevice>,
}

struct InputDevice {
    path: String,
    file: File,
}

impl TrimuiInput {
    pub fn open() -> Self {
        let mut devices = Vec::new();
        for index in 0..32 {
            let path = format!("/dev/input/event{index}");
            let Ok(file) = OpenOptions::new().read(true).open(&path) else {
                continue;
            };
            let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
            if flags < 0
                || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                    < 0
            {
                continue;
            }
            println!("BRICKWAVE_INPUT_OPEN path={path}");
            devices.push(InputDevice { path, file });
        }
        if devices.is_empty() {
            println!("BRICKWAVE_INPUT_MISSING fallback=sdl-events");
        } else {
            println!(
                "BRICKWAVE_INPUT_PROFILE analog_x={ABS_X} analog_y={ABS_Y} dpad_x={ABS_HAT0X} dpad_y={ABS_HAT0Y} primary={BTN_EAST} back={BTN_SOUTH} power={KEY_POWER} menu={BTN_MODE} pause={BTN_NORTH}/{BTN_SELECT} start={BTN_START} previous={BTN_TL} next={BTN_TR}"
            );
        }
        Self { devices }
    }

    pub fn poll(&mut self) -> Vec<TrimuiAction> {
        let mut actions = Vec::new();
        for device in &mut self.devices {
            loop {
                let mut bytes = [0_u8; 24];
                match device.file.read(&mut bytes) {
                    Ok(24) => {
                        let kind = u16::from_ne_bytes([bytes[16], bytes[17]]);
                        let code = u16::from_ne_bytes([bytes[18], bytes[19]]);
                        let value =
                            i32::from_ne_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
                        if let Some(action) = input_action(kind, code, value) {
                            if !matches!(
                                action,
                                TrimuiAction::SetStickX(_) | TrimuiAction::SetStickY(_)
                            ) {
                                println!(
                                    "BRICKWAVE_INPUT_ACTION path={} action={action:?}",
                                    device.path
                                );
                            }
                            actions.push(action);
                        } else if kind == EV_KEY && value == 1 {
                            println!("BRICKWAVE_INPUT_UNMAPPED path={} code={code}", device.path);
                        }
                    }
                    Ok(0) => break,
                    Ok(_) => break,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) => {
                        println!(
                            "BRICKWAVE_INPUT_READ_ERROR path={} error={error}",
                            device.path
                        );
                        break;
                    }
                }
            }
        }
        actions
    }
}

fn input_action(kind: u16, code: u16, value: i32) -> Option<TrimuiAction> {
    if kind == EV_KEY {
        return match code {
            BTN_SOUTH if value == 1 => Some(TrimuiAction::Back),
            BTN_MODE if value == 1 => Some(TrimuiAction::Menu),
            KEY_POWER if matches!(value, 0 | 1) => Some(TrimuiAction::Power(value == 1)),
            BTN_NORTH | BTN_SELECT if value == 1 => Some(TrimuiAction::Stop),
            BTN_START if value == 1 => Some(TrimuiAction::Start),
            BTN_TL if value == 1 => Some(TrimuiAction::Previous),
            BTN_TR if value == 1 => Some(TrimuiAction::Next),
            BTN_EAST if matches!(value, 0 | 1) => Some(TrimuiAction::Primary(value == 1)),
            KEY_UP => Some(TrimuiAction::SetDpadY(if value == 0 { 0 } else { -1 })),
            KEY_DOWN => Some(TrimuiAction::SetDpadY(if value == 0 { 0 } else { 1 })),
            KEY_LEFT => Some(TrimuiAction::SetDpadX(if value == 0 { 0 } else { -1 })),
            KEY_RIGHT => Some(TrimuiAction::SetDpadX(if value == 0 { 0 } else { 1 })),
            _ => None,
        };
    }
    if kind == EV_ABS {
        return match code {
            ABS_HAT0X => Some(TrimuiAction::SetDpadX(value.clamp(-1, 1) as i8)),
            ABS_HAT0Y => Some(TrimuiAction::SetDpadY(value.clamp(-1, 1) as i8)),
            ABS_X => Some(TrimuiAction::SetStickX(value)),
            ABS_Y => Some(TrimuiAction::SetStickY(value)),
            _ => None,
        };
    }
    None
}

/// Pointer coordinates stay in SDL drawable pixels. egui-sdl2 performs the
/// pixels-per-point conversion once when it receives the synthetic event.
pub struct VirtualPointer {
    x: f32,
    y: f32,
    stick_x: i32,
    stick_y: i32,
    dpad_x: i8,
    dpad_y: i8,
    primary_down: bool,
    scroll_x: f32,
    scroll_y: f32,
}

impl VirtualPointer {
    pub fn new(bounds: (i32, i32)) -> Self {
        Self {
            x: bounds.0 as f32 / 2.0,
            y: bounds.1 as f32 / 2.0,
            stick_x: 0,
            stick_y: 0,
            dpad_x: 0,
            dpad_y: 0,
            primary_down: false,
            scroll_x: 0.0,
            scroll_y: 0.0,
        }
    }

    pub fn position(&self) -> (i32, i32) {
        (self.x.round() as i32, self.y.round() as i32)
    }

    pub fn set_stick_x(&mut self, value: i32) {
        self.stick_x = value.clamp(-(ANALOG_RAW_MAX as i32), ANALOG_RAW_MAX as i32);
    }

    pub fn set_stick_y(&mut self, value: i32) {
        self.stick_y = value.clamp(-(ANALOG_RAW_MAX as i32), ANALOG_RAW_MAX as i32);
    }

    pub fn set_dpad_x(&mut self, value: i8) {
        self.dpad_x = value.clamp(-1, 1);
    }

    pub fn set_dpad_y(&mut self, value: i8) {
        self.dpad_y = value.clamp(-1, 1);
    }

    pub fn clear_dpad(&mut self) {
        self.dpad_x = 0;
        self.dpad_y = 0;
        self.scroll_x = 0.0;
        self.scroll_y = 0.0;
    }

    pub fn set_primary(&mut self, pressed: bool) -> bool {
        if self.primary_down == pressed {
            false
        } else {
            self.primary_down = pressed;
            true
        }
    }

    pub fn reset_motion(&mut self) {
        self.stick_x = 0;
        self.stick_y = 0;
        self.dpad_x = 0;
        self.dpad_y = 0;
        self.scroll_x = 0.0;
        self.scroll_y = 0.0;
    }

    pub fn advance(
        &mut self,
        delta: Duration,
        bounds: (i32, i32),
    ) -> Option<((i32, i32), (i32, i32))> {
        let previous = self.position();
        let seconds = delta.as_secs_f32();
        self.x = (self.x + axis_speed(self.stick_x) * seconds)
            .clamp(0.0, bounds.0.saturating_sub(1) as f32);
        self.y = (self.y + axis_speed(self.stick_y) * seconds)
            .clamp(0.0, bounds.1.saturating_sub(1) as f32);
        let current = self.position();
        (current != previous).then_some((previous, current))
    }

    pub fn scroll_lines(&mut self, delta: Duration) -> Option<(i32, i32)> {
        let seconds = delta.as_secs_f32();
        self.scroll_x += self.dpad_x as f32 * DPAD_SCROLL_LINES_PER_SECOND * seconds;
        self.scroll_y += -self.dpad_y as f32 * DPAD_SCROLL_LINES_PER_SECOND * seconds;
        let x = take_whole_lines(&mut self.scroll_x);
        let y = take_whole_lines(&mut self.scroll_y);
        (x != 0 || y != 0).then_some((x, y))
    }
}

fn axis_speed(raw: i32) -> f32 {
    let normalized = (raw as f32 / ANALOG_RAW_MAX).clamp(-1.0, 1.0);
    let magnitude = normalized.abs();
    if magnitude <= ANALOG_DEADZONE {
        return 0.0;
    }
    let travel = (magnitude - ANALOG_DEADZONE) / (1.0 - ANALOG_DEADZONE);
    normalized.signum()
        * (ANALOG_MIN_SPEED + (ANALOG_MAX_SPEED - ANALOG_MIN_SPEED) * travel * travel)
}

fn take_whole_lines(value: &mut f32) -> i32 {
    let lines = value.trunc() as i32;
    *value -= lines as f32;
    lines
}

pub fn draw_virtual_cursor(ctx: &egui::Context, pointer: (i32, i32)) {
    let pixels_per_point = ctx.pixels_per_point().max(1.0);
    let tip = egui::pos2(
        pointer.0 as f32 / pixels_per_point,
        pointer.1 as f32 / pixels_per_point,
    );
    let size = 18.0 / pixels_per_point;
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("brickwave-virtual-mouse-cursor"),
    ));
    painter.add(egui::Shape::convex_polygon(
        vec![
            tip,
            tip + egui::vec2(0.0, size),
            tip + egui::vec2(size * 0.28, size * 0.70),
            tip + egui::vec2(size * 0.58, size),
            tip + egui::vec2(size * 0.78, size * 0.80),
            tip + egui::vec2(size * 0.48, size * 0.52),
            tip + egui::vec2(size, size * 0.48),
        ],
        egui::Color32::WHITE,
        egui::Stroke::new(2.0 / pixels_per_point, egui::Color32::BLACK),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brick_buttons_map_to_primary_back_and_scroll() {
        assert_eq!(
            input_action(EV_KEY, BTN_EAST, 1),
            Some(TrimuiAction::Primary(true))
        );
        assert_eq!(
            input_action(EV_KEY, BTN_EAST, 0),
            Some(TrimuiAction::Primary(false))
        );
        assert_eq!(input_action(EV_KEY, BTN_SOUTH, 1), Some(TrimuiAction::Back));
        assert_eq!(
            input_action(EV_KEY, KEY_POWER, 1),
            Some(TrimuiAction::Power(true))
        );
        assert_eq!(
            input_action(EV_KEY, KEY_POWER, 0),
            Some(TrimuiAction::Power(false))
        );
        assert_eq!(input_action(EV_KEY, BTN_MODE, 1), Some(TrimuiAction::Menu));
        assert_eq!(input_action(EV_KEY, BTN_NORTH, 1), Some(TrimuiAction::Stop));
        assert_eq!(
            input_action(EV_KEY, BTN_SELECT, 1),
            Some(TrimuiAction::Stop)
        );
        assert_eq!(
            input_action(EV_KEY, BTN_START, 1),
            Some(TrimuiAction::Start)
        );
        assert_eq!(
            input_action(EV_KEY, BTN_TL, 1),
            Some(TrimuiAction::Previous)
        );
        assert_eq!(input_action(EV_KEY, BTN_TR, 1), Some(TrimuiAction::Next));
        assert_eq!(
            input_action(EV_ABS, ABS_HAT0Y, -1),
            Some(TrimuiAction::SetDpadY(-1))
        );
    }

    #[test]
    fn pointer_deadzone_drag_and_bounds_are_deterministic() {
        assert_eq!(axis_speed(0), 0.0);
        assert_eq!(axis_speed((ANALOG_RAW_MAX * ANALOG_DEADZONE) as i32), 0.0);
        let mut pointer = VirtualPointer::new((100, 80));
        assert!(pointer.set_primary(true));
        assert!(!pointer.set_primary(true));
        pointer.set_stick_x(ANALOG_RAW_MAX as i32);
        pointer.set_stick_y(-(ANALOG_RAW_MAX as i32));
        for _ in 0..20 {
            pointer.advance(Duration::from_millis(50), (100, 80));
        }
        assert_eq!(pointer.position(), (99, 0));
        assert!(pointer.set_primary(false));
    }

    #[test]
    fn held_dpad_generates_repeatable_wheel_lines() {
        let mut pointer = VirtualPointer::new((WIDTH, HEIGHT));
        pointer.set_dpad_y(-1);
        assert_eq!(
            pointer.scroll_lines(Duration::from_millis(56)),
            Some((0, 1))
        );
    }
}
