//! Central Phosphor icon registry for the egui preview.
//!
//! The `subset!` macro produces two small TTFs at compile time. Only the
//! glyphs listed here are embedded; the complete Phosphor font is not copied
//! into the executable and no texture is uploaded per frame.

use egui::{
    self, Align2, Color32, FontData, FontDefinitions, FontFamily, FontId, Painter, Rect, Vec2,
};

pub const ICON_SIZE_SMALL: f32 = 16.0;
pub const ICON_SIZE_MEDIUM: f32 = 20.0;
pub const ICON_SIZE_LARGE: f32 = 26.0;
pub const CONTROL_HITBOX: f32 = 34.0;
pub const CONTROL_GAP: f32 = 9.0;

egui_phosphor::subset! {
    /// Phosphor glyphs used by the SoundCloud Brick preview.
    pub mod phosphor {
        use regular::{
            ARROW_CLOCKWISE, ARROW_LEFT, BOOKMARK_SIMPLE, BOOKS, CARET_DOWN,
            CARET_LEFT, CARET_RIGHT, CHECK, CLOCK_COUNTDOWN, COMPASS,
            DEVICE_MOBILE_CAMERA, DOTS_THREE, GEAR, HEART, HOUSE, LINK,
            LOCK_KEY, MAGNIFYING_GLASS, PAUSE, PLAY, PLAYLIST, QR_CODE, QUEUE,
            REPEAT, REPEAT_ONCE, SHUFFLE, SKIP_BACK, SKIP_FORWARD,
            SPEAKER_HIGH, SPEAKER_SLASH, USER_CIRCLE, VINYL_RECORD,
            WARNING_CIRCLE, X,
        };
        use fill::{HEART};
    }
}

#[derive(Clone, Copy)]
pub enum Glyph {
    Brand,
    Home,
    Discover,
    Search,
    Library,
    Like,
    LikeFilled,
    Playlist,
    Settings,
    User,
    Play,
    Pause,
    Previous,
    Next,
    Shuffle,
    Repeat,
    RepeatOne,
    Volume,
    Mute,
    Queue,
    ChevronDown,
    ChevronLeft,
    ChevronRight,
    Close,
    More,
    Save,
    Back,
    Check,
    Clock,
    Lock,
    QrCode,
    Refresh,
    Connect,
    Phone,
    Warning,
}

pub struct IconRegistry;

impl IconRegistry {
    pub fn install_fonts(ctx: &egui::Context) {
        ctx.set_fonts(Self::font_definitions());
    }

    fn font_definitions() -> FontDefinitions {
        let mut fonts = FontDefinitions::default();
        fonts.font_data.insert(
            "brickwave_inter".to_owned(),
            FontData::from_static(include_bytes!("../assets/fonts/InterVariable.ttf")).into(),
        );
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "brickwave_inter".to_owned());
        // Icon glyphs live in the Unicode private-use area. Inter also maps a
        // few private-use codepoints, so it must never be a fallback inside an
        // icon family. Keep text and icons in fully isolated families.
        phosphor::regular::add_as_family(&mut fonts);
        phosphor::fill::add_as_family(&mut fonts);
        fonts.families.insert(
            phosphor::regular::family(),
            vec![phosphor::regular::FONT_NAME.to_owned()],
        );
        fonts.families.insert(
            phosphor::fill::family(),
            vec![phosphor::fill::FONT_NAME.to_owned()],
        );
        fonts
    }

    pub fn paint(painter: &Painter, glyph: Glyph, rect: Rect, color: Color32) {
        let (glyph, family, optical_y) = match glyph {
            Glyph::Brand => (
                phosphor::regular::VINYL_RECORD,
                phosphor::regular::family(),
                0.0,
            ),
            Glyph::Home => (phosphor::regular::HOUSE, phosphor::regular::family(), 0.0),
            Glyph::Discover => (phosphor::regular::COMPASS, phosphor::regular::family(), 0.0),
            Glyph::Search => (
                phosphor::regular::MAGNIFYING_GLASS,
                phosphor::regular::family(),
                0.0,
            ),
            Glyph::Library => (phosphor::regular::BOOKS, phosphor::regular::family(), 0.0),
            Glyph::Like => (phosphor::regular::HEART, phosphor::regular::family(), 0.4),
            Glyph::LikeFilled => (phosphor::fill::HEART, phosphor::fill::family(), 0.4),
            Glyph::Playlist => (
                phosphor::regular::PLAYLIST,
                phosphor::regular::family(),
                0.0,
            ),
            Glyph::Settings => (phosphor::regular::GEAR, phosphor::regular::family(), 0.0),
            Glyph::User => (
                phosphor::regular::USER_CIRCLE,
                phosphor::regular::family(),
                0.0,
            ),
            Glyph::Play => (phosphor::regular::PLAY, phosphor::regular::family(), 0.0),
            Glyph::Pause => (phosphor::regular::PAUSE, phosphor::regular::family(), 0.0),
            Glyph::Previous => (
                phosphor::regular::SKIP_BACK,
                phosphor::regular::family(),
                0.0,
            ),
            Glyph::Next => (
                phosphor::regular::SKIP_FORWARD,
                phosphor::regular::family(),
                0.0,
            ),
            Glyph::Shuffle => (phosphor::regular::SHUFFLE, phosphor::regular::family(), 0.0),
            Glyph::Repeat => (phosphor::regular::REPEAT, phosphor::regular::family(), 0.0),
            Glyph::RepeatOne => (
                phosphor::regular::REPEAT_ONCE,
                phosphor::regular::family(),
                0.0,
            ),
            Glyph::Volume => (
                phosphor::regular::SPEAKER_HIGH,
                phosphor::regular::family(),
                0.0,
            ),
            Glyph::Mute => (
                phosphor::regular::SPEAKER_SLASH,
                phosphor::regular::family(),
                0.0,
            ),
            Glyph::Queue => (phosphor::regular::QUEUE, phosphor::regular::family(), 0.0),
            Glyph::ChevronDown => (
                phosphor::regular::CARET_DOWN,
                phosphor::regular::family(),
                0.0,
            ),
            Glyph::ChevronLeft => (
                phosphor::regular::CARET_LEFT,
                phosphor::regular::family(),
                0.0,
            ),
            Glyph::ChevronRight => (
                phosphor::regular::CARET_RIGHT,
                phosphor::regular::family(),
                0.0,
            ),
            Glyph::Close => (phosphor::regular::X, phosphor::regular::family(), 0.0),
            Glyph::More => (
                phosphor::regular::DOTS_THREE,
                phosphor::regular::family(),
                0.0,
            ),
            Glyph::Save => (
                phosphor::regular::BOOKMARK_SIMPLE,
                phosphor::regular::family(),
                0.0,
            ),
            Glyph::Back => (
                phosphor::regular::ARROW_LEFT,
                phosphor::regular::family(),
                0.0,
            ),
            Glyph::Check => (phosphor::regular::CHECK, phosphor::regular::family(), 0.0),
            Glyph::Clock => (
                phosphor::regular::CLOCK_COUNTDOWN,
                phosphor::regular::family(),
                0.0,
            ),
            Glyph::Lock => (
                phosphor::regular::LOCK_KEY,
                phosphor::regular::family(),
                0.0,
            ),
            Glyph::QrCode => (phosphor::regular::QR_CODE, phosphor::regular::family(), 0.0),
            Glyph::Refresh => (
                phosphor::regular::ARROW_CLOCKWISE,
                phosphor::regular::family(),
                0.0,
            ),
            Glyph::Connect => (phosphor::regular::LINK, phosphor::regular::family(), 0.0),
            Glyph::Phone => (
                phosphor::regular::DEVICE_MOBILE_CAMERA,
                phosphor::regular::family(),
                0.0,
            ),
            Glyph::Warning => (
                phosphor::regular::WARNING_CIRCLE,
                phosphor::regular::family(),
                0.0,
            ),
        };
        let size = rect.width().min(rect.height()) * 0.88;
        painter.text(
            rect.center() + Vec2::new(0.0, optical_y),
            Align2::CENTER_CENTER,
            glyph,
            FontId::new(size, family),
            color,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{IconRegistry, phosphor};
    use egui::FontFamily;

    #[test]
    fn text_and_icon_fonts_use_isolated_families() {
        let fonts = IconRegistry::font_definitions();
        assert_eq!(
            fonts.families[&phosphor::regular::family()],
            [phosphor::regular::FONT_NAME]
        );
        assert_eq!(
            fonts.families[&phosphor::fill::family()],
            [phosphor::fill::FONT_NAME]
        );
        let proportional = &fonts.families[&FontFamily::Proportional];
        assert_eq!(
            proportional.first().map(String::as_str),
            Some("brickwave_inter")
        );
        assert!(
            !proportional
                .iter()
                .any(|name| name == phosphor::regular::FONT_NAME
                    || name == phosphor::fill::FONT_NAME)
        );
    }
}
