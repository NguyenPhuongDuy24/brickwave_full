//! Presenting the UI at a quarter turn to the window, for a panel that is not
//! mounted the way it is read: egui lays out for the turned screen, and the
//! backend puts that frame on the window the other way round.

use egui::{ClippedPrimitive, Pos2, Rect, Vec2};

/// How far the UI is turned clockwise on its way to the window.
///
/// A quarter turn trades the screen's width and height, so egui lays out for a
/// portrait screen on a landscape panel (and the other way round). Pointer and
/// touch positions travel back the same way, so a tap lands where it looks.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default, Hash)]
pub enum Rotation {
    #[default]
    None,
    /// A quarter turn clockwise: the screen's top edge runs down the window's
    /// right side.
    Cw90,
    /// Upside down, for a panel mounted that way.
    Cw180,
    /// A quarter turn counterclockwise: the screen's top edge runs up the
    /// window's left side.
    Cw270,
}

impl Rotation {
    /// In turn order, for a setting that cycles them.
    pub const ALL: [Rotation; 4] = [
        Rotation::None,
        Rotation::Cw90,
        Rotation::Cw180,
        Rotation::Cw270,
    ];

    /// Quarter turns clockwise, 0 to 3.
    #[inline]
    pub fn quarter_turns(self) -> u8 {
        match self {
            Rotation::None => 0,
            Rotation::Cw90 => 1,
            Rotation::Cw180 => 2,
            Rotation::Cw270 => 3,
        }
    }

    /// Wrapping, and negative turns count counterclockwise, so callers may add
    /// turns without normalising first.
    #[inline]
    pub fn from_quarter_turns(turns: i32) -> Self {
        Self::ALL[turns.rem_euclid(4) as usize]
    }

    /// Clockwise degrees, as `SDL_RenderCopyEx` takes them.
    #[inline]
    pub fn degrees(self) -> f64 {
        self.quarter_turns() as f64 * 90.0
    }

    /// Whether width and height trade places.
    #[inline]
    pub fn swaps_axes(self) -> bool {
        matches!(self, Rotation::Cw90 | Rotation::Cw270)
    }

    /// The screen egui lays out for, inside a window of `window`.
    #[inline]
    pub fn screen_size(self, window: Vec2) -> Vec2 {
        if self.swaps_axes() {
            Vec2::new(window.y, window.x)
        } else {
            window
        }
    }

    /// Where a point of the turned screen lands in the window. `window` is the
    /// window's size in the same unit as `p` — points for geometry egui laid
    /// out, pixels for a frame being presented.
    #[inline]
    pub fn to_window(self, p: Pos2, window: Vec2) -> Pos2 {
        match self {
            Rotation::None => p,
            Rotation::Cw90 => Pos2::new(window.x - p.y, p.x),
            Rotation::Cw180 => Pos2::new(window.x - p.x, window.y - p.y),
            Rotation::Cw270 => Pos2::new(p.y, window.y - p.x),
        }
    }

    /// Where a point of the window falls in the turned screen — the way
    /// pointer input travels.
    #[inline]
    pub fn from_window(self, p: Pos2, window: Vec2) -> Pos2 {
        match self {
            Rotation::None => p,
            Rotation::Cw90 => Pos2::new(p.y, window.x - p.x),
            Rotation::Cw180 => Pos2::new(window.x - p.x, window.y - p.y),
            Rotation::Cw270 => Pos2::new(window.y - p.y, p.x),
        }
    }

    /// A quarter turn maps an axis-aligned rect onto another one, so a clip
    /// rect survives the trip.
    #[inline]
    pub fn rect_to_window(self, rect: Rect, window: Vec2) -> Rect {
        if self == Rotation::None {
            return rect;
        }
        Rect::from_two_pos(
            self.to_window(rect.min, window),
            self.to_window(rect.max, window),
        )
    }

    /// Turn a tessellated frame into window space, for backends that put their
    /// geometry on screen as it comes. `window` is the window's size in points.
    ///
    /// Paint callbacks are moved but not turned: what they draw is the app's
    /// own, out of reach here.
    pub fn turn_primitives(self, jobs: &mut [ClippedPrimitive], window: Vec2) {
        if self == Rotation::None {
            return;
        }
        for job in jobs.iter_mut() {
            job.clip_rect = self.rect_to_window(job.clip_rect, window);
            match &mut job.primitive {
                egui::epaint::Primitive::Mesh(mesh) => {
                    for vertex in &mut mesh.vertices {
                        vertex.pos = self.to_window(vertex.pos, window);
                    }
                }
                egui::epaint::Primitive::Callback(callback) => {
                    log::warn!("a paint callback is not turned with the screen");
                    callback.rect = self.rect_to_window(callback.rect, window);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Vec2 = Vec2::new(640.0, 480.0);

    /// The corners of the turned screen, clockwise from its top left.
    fn corners(rotation: Rotation) -> Vec<Pos2> {
        let screen = rotation.screen_size(WINDOW);
        [
            Pos2::new(0.0, 0.0),
            Pos2::new(screen.x, 0.0),
            Pos2::new(screen.x, screen.y),
            Pos2::new(0.0, screen.y),
        ]
        .iter()
        .map(|p| rotation.to_window(*p, WINDOW))
        .collect()
    }

    #[test]
    fn a_quarter_turn_trades_width_for_height() {
        assert_eq!(Rotation::None.screen_size(WINDOW), WINDOW);
        assert_eq!(Rotation::Cw180.screen_size(WINDOW), WINDOW);
        let turned = Vec2::new(WINDOW.y, WINDOW.x);
        assert_eq!(Rotation::Cw90.screen_size(WINDOW), turned);
        assert_eq!(Rotation::Cw270.screen_size(WINDOW), turned);
    }

    #[test]
    fn the_screen_lands_on_the_window_and_nowhere_else() {
        // Every turn covers the window exactly: no gap, no overhang, and the
        // corners still go round in order.
        for rotation in Rotation::ALL {
            let corners = corners(rotation);
            let rect = Rect::from_points(&corners);
            assert_eq!(rect.min, Pos2::ZERO, "{rotation:?} leaves the window");
            assert_eq!(rect.max, WINDOW.to_pos2(), "{rotation:?} leaves the window");
        }
    }

    #[test]
    fn the_screens_top_left_goes_where_the_turn_says() {
        let top_left = |rotation: Rotation| corners(rotation)[0];
        assert_eq!(top_left(Rotation::None), Pos2::new(0.0, 0.0));
        assert_eq!(top_left(Rotation::Cw90), Pos2::new(WINDOW.x, 0.0));
        assert_eq!(top_left(Rotation::Cw180), WINDOW.to_pos2());
        assert_eq!(top_left(Rotation::Cw270), Pos2::new(0.0, WINDOW.y));
    }

    #[test]
    fn a_pointer_comes_back_to_where_it_was_pressed() {
        // What the backend presents and what input undoes must be one map, or a
        // tap lands somewhere other than what it is under.
        for rotation in Rotation::ALL {
            let screen = rotation.screen_size(WINDOW);
            for p in [
                Pos2::ZERO,
                Pos2::new(screen.x, screen.y),
                Pos2::new(screen.x / 3.0, screen.y / 7.0),
            ] {
                let there_and_back = rotation.from_window(rotation.to_window(p, WINDOW), WINDOW);
                assert!(
                    there_and_back.distance(p) < 1e-3,
                    "{rotation:?}: {p:?} came back as {there_and_back:?}"
                );
            }
        }
    }

    #[test]
    fn turns_wrap_in_both_directions() {
        assert_eq!(Rotation::from_quarter_turns(4), Rotation::None);
        assert_eq!(Rotation::from_quarter_turns(-1), Rotation::Cw270);
        assert_eq!(Rotation::from_quarter_turns(5), Rotation::Cw90);
        for rotation in Rotation::ALL {
            assert_eq!(
                Rotation::from_quarter_turns(rotation.quarter_turns() as i32),
                rotation
            );
        }
    }

    #[test]
    fn a_clip_rect_stays_a_rect_the_right_way_up() {
        let rect = Rect::from_min_max(Pos2::new(10.0, 20.0), Pos2::new(110.0, 60.0));
        for rotation in Rotation::ALL {
            let turned = rotation.rect_to_window(rect, WINDOW);
            assert!(turned.min.x <= turned.max.x && turned.min.y <= turned.max.y);
            let (w, h) = (rect.width(), rect.height());
            let (tw, th) = (turned.width(), turned.height());
            if rotation.swaps_axes() {
                assert!(
                    (tw - h).abs() < 1e-3 && (th - w).abs() < 1e-3,
                    "{rotation:?}"
                );
            } else {
                assert!(
                    (tw - w).abs() < 1e-3 && (th - h).abs() < 1e-3,
                    "{rotation:?}"
                );
            }
        }
    }

    #[test]
    fn a_turned_mesh_holds_its_shape() {
        let mut mesh = egui::Mesh::default();
        let vertex = |x: f32, y: f32| egui::epaint::Vertex {
            pos: Pos2::new(x, y),
            uv: Pos2::ZERO,
            color: egui::Color32::WHITE,
        };
        mesh.vertices = vec![vertex(0.0, 0.0), vertex(40.0, 0.0), vertex(40.0, 10.0)];
        let mut jobs = vec![ClippedPrimitive {
            clip_rect: Rect::from_min_max(Pos2::ZERO, Pos2::new(480.0, 640.0)),
            primitive: egui::epaint::Primitive::Mesh(mesh),
        }];
        Rotation::Cw90.turn_primitives(&mut jobs, WINDOW);
        let egui::epaint::Primitive::Mesh(mesh) = &jobs[0].primitive else {
            unreachable!("the job was built as a mesh");
        };
        // The screen's top-left corner is the window's top-right one, and the
        // run of text along its top edge now runs down that side.
        assert_eq!(mesh.vertices[0].pos, Pos2::new(640.0, 0.0));
        assert_eq!(mesh.vertices[1].pos, Pos2::new(640.0, 40.0));
        assert_eq!(mesh.vertices[2].pos, Pos2::new(630.0, 40.0));
        assert_eq!(
            jobs[0].clip_rect,
            Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(640.0, 480.0))
        );
    }
}
