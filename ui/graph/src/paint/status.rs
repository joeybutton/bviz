//! The badge that says how the build ended for a box, and where it stands.

use super::{faded, paint_path};
use crate::Palette;
use crate::status::Status;
use gpui::{
    BorderStyle, Bounds, Hsla, PathBuilder, Pixels, Point, Rgba, Window, point, px, quad, size,
};
use rustc_hash::FxHashMap;
use std::cmp::Reverse;

/// Half the width (pre-zoom px) of the glyph a status badge is drawn as, which
/// stands it as tall as the capitals of the label it is set before.
const STATUS_HALF: f32 = 5.0;

/// Screen half-width a status glyph is never drawn below. Its shape is what
/// tells one status from another, and fewer pixels than this leave every shape
/// a dot.
pub(crate) const STATUS_FLOOR: f32 = 4.0;

/// Screen width of the canvas-coloured rim around a status glyph. A status
/// colour is held to its contrast against the canvas, which no fill a box wears
/// answers for, so the glyph brings that ground with it.
pub(crate) const STATUS_RIM: f32 = 1.5;

/// Bare ground (pre-zoom px) between a status badge and what follows it in a
/// label.
const STATUS_GAP: f32 = 2.0;

/// The share of a status glyph's half-width a ring's stroke takes, and the
/// screen width that stroke is never drawn below.
const RING_STROKE: f32 = 0.45;
const RING_STROKE_FLOOR: f32 = 1.5;

/// The share of a status glyph's half-width a bar's half-height takes.
const BAR_HALF_HEIGHT: f32 = 0.4;

/// The share of a status glyph's half-width a square's half-side takes, set
/// below the whole so the square weighs no more than the disc.
const SQUARE_HALF_SIDE: f32 = 0.85;

/// The shares of a status glyph's half-width a triangle's apex stands above
/// its middle and its base lies below it: short of the whole, so the triangle
/// weighs no more than the disc.
const TRIANGLE_APEX: f32 = 0.95;
const TRIANGLE_BASE: f32 = 0.8;

/// The share of a status glyph's half-width a cross's arms are thick to
/// either side of their middle.
const CROSS_ARM: f32 = 0.3;

/// The shape a status badge is drawn as. Each status takes one no other does,
/// so a reader who cannot tell their inks apart still tells the statuses
/// apart. None is the diamond of a transition badge, which a theme may give an
/// ink a status wears and which is rimmed and floored alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Glyph {
    Disc,
    Bar,
    Ring,
    TriangleUp,
    Square,
    TriangleDown,
    Cross,
}

impl Glyph {
    pub(super) fn of(status: Status) -> Self {
        match status {
            Status::Succeeded => Glyph::Disc,
            Status::Skipped => Glyph::Bar,
            Status::Unfinished => Glyph::Ring,
            Status::Flaky => Glyph::TriangleUp,
            Status::TestFailed => Glyph::Square,
            Status::Failed => Glyph::TriangleDown,
            Status::RootCause => Glyph::Cross,
        }
    }

    /// Whether [`paint_status`] draws the glyph as paths rather than quads.
    fn traced(self) -> bool {
        match self {
            Glyph::TriangleUp | Glyph::TriangleDown | Glyph::Cross => true,
            Glyph::Disc | Glyph::Bar | Glyph::Ring | Glyph::Square => false,
        }
    }
}

/// The corners of a triangle standing on its base, about its middle, for a
/// glyph of half-width one.
pub(super) const TRIANGLE: [(f32, f32); 3] = [
    (0.0, -TRIANGLE_APEX),
    (1.0, TRIANGLE_BASE),
    (-1.0, TRIANGLE_BASE),
];

/// [`TRIANGLE`] hanging from its base.
pub(super) const TRIANGLE_DOWN: [(f32, f32); 3] = [
    (0.0, TRIANGLE_APEX),
    (-1.0, -TRIANGLE_BASE),
    (1.0, -TRIANGLE_BASE),
];

/// The corners of a cross lying on the diagonals, about its middle, for a
/// glyph of half-width one: an upright cross of arms reaching one from the
/// middle, turned an eighth of a turn.
pub(super) fn cross() -> [(f32, f32); 12] {
    let t = CROSS_ARM;
    [
        (-t, -1.0),
        (t, -1.0),
        (t, -t),
        (1.0, -t),
        (1.0, t),
        (t, t),
        (t, 1.0),
        (-t, 1.0),
        (-t, t),
        (-1.0, t),
        (-1.0, -t),
        (-t, -t),
    ]
    .map(|(x, y)| {
        (
            (x - y) * std::f32::consts::FRAC_1_SQRT_2,
            (x + y) * std::f32::consts::FRAC_1_SQRT_2,
        )
    })
}

/// The screen half-width a status glyph is drawn at where the label it goes
/// with, or the box it stands for, is drawn at `scale` of its layout size.
pub(super) fn status_half(scale: f32) -> f32 {
    (STATUS_HALF * scale).max(STATUS_FLOOR)
}

/// The screen width held for a status badge left of a label drawn at `scale`
/// of its layout size: the glyph, its rim to either side, and the gap before
/// what follows.
pub(super) fn status_reserve(scale: f32) -> f32 {
    2. * (status_half(scale) + STATUS_RIM) + STATUS_GAP * scale
}

/// A status badge a frame draws, in screen pixels.
#[derive(Clone, Copy)]
pub(super) struct StatusBadge {
    pub(super) center: Point<Pixels>,
    pub(super) half: f32,
    pub(super) status: Status,
    /// The opacity of the box the badge belongs to, which it fades with.
    pub(super) alpha: f32,
}

impl StatusBadge {
    /// The square the badge is drawn within, rim and all.
    pub(super) fn bounds(&self) -> Bounds<Pixels> {
        let reach = px(self.half + STATUS_RIM);
        Bounds::new(
            point(self.center.x - reach, self.center.y - reach),
            size(reach * 2., reach * 2.),
        )
    }

    pub(super) fn traced(&self) -> bool {
        Glyph::of(self.status).traced()
    }

    pub(super) fn paint(&self, palette: &Palette, window: &mut Window) {
        paint_status(
            self.center,
            self.half,
            self.status,
            self.alpha,
            palette,
            window,
        );
    }
}

/// The badge of a box drawn without its label, which stands only where the
/// ground is free.
pub(super) struct BareBadge {
    /// The badge where it stands by choice.
    pub(super) badge: StatusBadge,
    /// Where it stands with that ground taken, for a box that has such a
    /// place.
    pub(super) otherwise: Option<Point<Pixels>>,
}

/// Side of the squares [`BadgeGround`] finds badges by: the extent of a badge
/// held at its floor.
const BADGE_CELL: f32 = 2.0 * (STATUS_FLOOR + STATUS_RIM);

/// The cells a badge held at its floor reaches at most.
const CELLS_PER_BADGE: usize = 4;

/// The ground the status badges of one frame have taken.
struct BadgeGround {
    /// Each bounds taken, once for every cell it reaches, with the entry
    /// that cell held before it.
    taken: Vec<(Bounds<Pixels>, Option<usize>)>,
    /// The entry of `taken` each cell held last.
    cells: FxHashMap<(i32, i32), usize>,
}

impl BadgeGround {
    /// Ground with room for `badges` of them.
    fn with_capacity(badges: usize) -> Self {
        let entries = badges * CELLS_PER_BADGE;
        Self {
            taken: Vec::with_capacity(entries),
            cells: FxHashMap::with_capacity_and_hasher(entries, Default::default()),
        }
    }

    fn cells(bounds: Bounds<Pixels>) -> impl Iterator<Item = (i32, i32)> {
        let cell = |at: Pixels| (f32::from(at) / BADGE_CELL).floor() as i32;
        let (from, to) = (bounds.origin, bounds.bottom_right());
        (cell(from.x)..=cell(to.x))
            .flat_map(move |x| (cell(from.y)..=cell(to.y)).map(move |y| (x, y)))
    }

    /// Whether `bounds` lands on none of the bounds taken.
    fn is_free(&self, bounds: Bounds<Pixels>) -> bool {
        Self::cells(bounds).all(|cell| {
            let mut entry = self.cells.get(&cell).copied();
            while let Some((taken, before)) = entry.map(|entry| self.taken[entry]) {
                if taken.intersects(&bounds) {
                    return false;
                }
                entry = before;
            }
            true
        })
    }

    fn take(&mut self, bounds: Bounds<Pixels>) {
        for cell in Self::cells(bounds) {
            let before = self.cells.insert(cell, self.taken.len());
            self.taken.push((bounds, before));
        }
    }
}

/// Adds to `placed` each of `bare` that finds free ground. A container stands
/// at least as severe as any box it holds and is walked before it, so placing
/// the most severe first leaves its badge standing where the two would meet.
pub(super) fn place_bare(
    mut bare: Vec<BareBadge>,
    floated: &[Bounds<Pixels>],
    placed: &mut Vec<StatusBadge>,
) {
    let mut ground = BadgeGround::with_capacity(placed.len() + bare.len());
    for badge in placed.iter() {
        ground.take(badge.bounds());
    }
    bare.sort_by_key(|bare| Reverse(bare.badge.status));
    for bare in bare {
        let standing = std::iter::once(bare.badge.center)
            .chain(bare.otherwise)
            .map(|center| StatusBadge {
                center,
                ..bare.badge
            })
            .find(|badge| {
                let bounds = badge.bounds();
                ground.is_free(bounds) && !floated.iter().any(|taken| taken.intersects(&bounds))
            });
        if let Some(badge) = standing {
            ground.take(badge.bounds());
            placed.push(badge);
        }
    }
}

/// Paints the badge of `status` about `center`: its glyph, `half` to either
/// side, over a rim of the canvas's colour.
pub(crate) fn paint_status(
    center: Point<Pixels>,
    half: f32,
    status: Status,
    alpha: f32,
    palette: &Palette,
    window: &mut Window,
) {
    let rim = faded(palette.background, alpha);
    let ink = faded(palette.status_color(status), alpha);
    // A shape of upright sides or of a round one is a quad, whose coverage
    // is computed rather than sampled; the rest are paths.
    let mut block = |half_width: f32, half_height: f32, corner: f32| {
        for (grow, color) in [(STATUS_RIM, rim), (0.0, ink)] {
            let (w, h) = (half_width + grow, half_height + grow);
            window.paint_quad(quad(
                Bounds::new(
                    point(center.x - px(w), center.y - px(h)),
                    size(px(2. * w), px(2. * h)),
                ),
                px(if corner > 0.0 { corner + grow } else { 0.0 }),
                color,
                px(0.),
                Hsla::transparent_black(),
                BorderStyle::Solid,
            ));
        }
    };
    match Glyph::of(status) {
        Glyph::Disc => block(half, half, half),
        Glyph::Bar => block(half, half * BAR_HALF_HEIGHT, 0.0),
        Glyph::Square => block(half * SQUARE_HALF_SIDE, half * SQUARE_HALF_SIDE, 0.0),
        Glyph::Ring => {
            let reach = half + STATUS_RIM;
            window.paint_quad(quad(
                Bounds::new(
                    point(center.x - px(reach), center.y - px(reach)),
                    size(px(2. * reach), px(2. * reach)),
                ),
                px(reach),
                rim,
                px(0.),
                Hsla::transparent_black(),
                BorderStyle::Solid,
            ));
            window.paint_quad(quad(
                Bounds::new(
                    point(center.x - px(half), center.y - px(half)),
                    size(px(2. * half), px(2. * half)),
                ),
                px(half),
                Hsla::transparent_black(),
                px((half * RING_STROKE).max(RING_STROKE_FLOOR)),
                Hsla::from(ink),
                BorderStyle::Solid,
            ));
        }
        Glyph::TriangleUp => paint_rimmed(TRIANGLE, center, half, rim, ink, window),
        Glyph::TriangleDown => paint_rimmed(TRIANGLE_DOWN, center, half, rim, ink, window),
        Glyph::Cross => paint_rimmed(cross(), center, half, rim, ink, window),
    }
}

/// Fills the polygon `unit` gives for a half-width of one, scaled to `half`
/// about `center`, in `ink` over a `rim` standing [`STATUS_RIM`] out from it.
fn paint_rimmed<const N: usize>(
    unit: [(f32, f32); N],
    center: Point<Pixels>,
    half: f32,
    rim: Rgba,
    ink: Rgba,
    window: &mut Window,
) {
    let scaled = unit.map(|(x, y)| (x * half, y * half));
    for (outline, color) in [(outset(scaled, STATUS_RIM), rim), (scaled, ink)] {
        let corners = outline.map(|(x, y)| point(center.x + px(x), center.y + px(y)));
        let mut shape = PathBuilder::fill();
        shape.add_polygon(&corners, true);
        if let Ok(path) = shape.build() {
            paint_path(path, color, window);
        }
    }
}

/// `polygon` with every side moved `by` outward along its normal, the corners
/// following to where the moved sides meet. No two consecutive sides of
/// `polygon` may run in opposite directions.
pub(super) fn outset<const N: usize>(polygon: [(f32, f32); N], by: f32) -> [(f32, f32); N] {
    let n = N;
    let side = |i: usize| (polygon[i], polygon[(i + 1) % n]);
    let turning: f32 = (0..n)
        .map(|i| {
            let (a, b) = side(i);
            a.0 * b.1 - b.0 * a.1
        })
        .sum();
    // The normal of the side leaving corner `i`, pointing out of the polygon
    // whichever way round its corners are listed.
    let normal = |i: usize| {
        let (a, b) = side(i);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let length = dx.hypot(dy).max(f32::EPSILON) * turning.signum();
        (dy / length, -dx / length)
    };
    std::array::from_fn(|i| {
        let (before, after) = (normal((i + n - 1) % n), normal(i));
        let reach = by / (1.0 + before.0 * after.0 + before.1 * after.1);
        (
            polygon[i].0 + (before.0 + after.0) * reach,
            polygon[i].1 + (before.1 + after.1) * reach,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paint::connector::DIAMOND;

    /// A status is read off the shape of its badge by a reader who cannot tell
    /// the inks apart, the three failures sharing one ink among them.
    #[test]
    fn no_two_statuses_share_a_glyph() {
        for (i, status) in Status::ALL.iter().enumerate() {
            for other in &Status::ALL[i + 1..] {
                assert_ne!(
                    Glyph::of(*status),
                    Glyph::of(*other),
                    "{status:?}/{other:?}"
                );
            }
        }
    }

    /// A bounds of the extent a badge held at its floor has, with its top left
    /// corner at `(x, y)`.
    fn badge_bounds(x: f32, y: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(BADGE_CELL), px(BADGE_CELL)))
    }

    /// Whether two badges meet is a matter of their bounds alone, wherever
    /// the two fall among the cells they are found by.
    #[test]
    fn a_badge_meets_another_only_where_their_bounds_do() {
        const STEPS: u8 = 44;
        for step in 0..STEPS {
            let at = 100.0 + 2.0 * BADGE_CELL * f32::from(step) / f32::from(STEPS);
            let mut ground = BadgeGround::with_capacity(1);
            ground.take(badge_bounds(at, at));
            for cells in [1.05, 1.25, 1.5, 1.75, 1.95, 3.0] {
                let apart = cells * BADGE_CELL;
                for (x, y) in [(apart, 0.0), (0.0, apart), (-apart, 0.0), (0.0, -apart)] {
                    assert!(
                        ground.is_free(badge_bounds(at + x, at + y)),
                        "at {at}, apart by {x}, {y}"
                    );
                }
            }
            for cells in [0.0, 0.5, 0.95] {
                let apart = cells * BADGE_CELL;
                for (x, y) in [(apart, 0.0), (0.0, apart), (-apart, -apart)] {
                    assert!(
                        !ground.is_free(badge_bounds(at + x, at + y)),
                        "at {at}, apart by {x}, {y}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_badge_meets_any_of_several_reaching_one_cell() {
        // Three badges clear of one another, each reaching the cell between
        // them.
        let standing = [(95.0, 95.0), (107.0, 95.0), (95.0, 107.0)];
        let mut ground = BadgeGround::with_capacity(standing.len());
        for (x, y) in standing {
            assert!(ground.is_free(badge_bounds(x, y)));
            ground.take(badge_bounds(x, y));
        }
        for (x, y) in standing {
            assert!(!ground.is_free(badge_bounds(x + 0.5, y + 0.5)));
        }
        assert!(ground.is_free(badge_bounds(107.0, 107.0)));
    }

    fn bare_at(x: f32, y: f32, status: Status) -> BareBadge {
        BareBadge {
            badge: StatusBadge {
                center: point(px(x), px(y)),
                half: STATUS_FLOOR,
                status,
                alpha: 1.0,
            },
            otherwise: None,
        }
    }

    fn placed_at(placed: &[StatusBadge]) -> Vec<(f32, f32, Status)> {
        placed
            .iter()
            .map(|badge| (badge.center.x.into(), badge.center.y.into(), badge.status))
            .collect()
    }

    #[test]
    fn of_bare_badges_on_one_ground_the_most_severe_stands() {
        let mut placed = Vec::new();
        place_bare(
            vec![
                bare_at(100.0, 100.0, Status::Succeeded),
                bare_at(105.0, 100.0, Status::RootCause),
                bare_at(110.0, 100.0, Status::Failed),
                bare_at(300.0, 100.0, Status::Succeeded),
            ],
            &[],
            &mut placed,
        );
        assert_eq!(
            placed_at(&placed),
            [
                (105.0, 100.0, Status::RootCause),
                (300.0, 100.0, Status::Succeeded)
            ]
        );
    }

    #[test]
    fn of_bare_badges_as_severe_on_one_ground_the_first_walked_stands() {
        let mut placed = Vec::new();
        place_bare(
            vec![
                bare_at(100.0, 100.0, Status::Failed),
                bare_at(105.0, 100.0, Status::Failed),
            ],
            &[],
            &mut placed,
        );
        assert_eq!(placed_at(&placed), [(100.0, 100.0, Status::Failed)]);
    }

    #[test]
    fn a_bare_badge_gives_way_to_a_badge_on_its_label_and_to_a_floated_title() {
        let labelled = bare_at(100.0, 100.0, Status::Succeeded).badge;
        let title = Bounds::new(point(px(190.0), px(90.0)), size(px(40.0), px(17.0)));
        let mut placed = vec![labelled];
        place_bare(
            vec![
                bare_at(104.0, 104.0, Status::RootCause),
                bare_at(200.0, 100.0, Status::RootCause),
                bare_at(300.0, 100.0, Status::RootCause),
            ],
            &[title],
            &mut placed,
        );
        assert_eq!(
            placed_at(&placed),
            [
                (100.0, 100.0, Status::Succeeded),
                (300.0, 100.0, Status::RootCause)
            ]
        );
    }

    #[test]
    fn a_bare_badge_whose_ground_is_taken_stands_at_its_other_place() {
        let title = Bounds::new(point(px(90.0), px(90.0)), size(px(40.0), px(17.0)));
        let elsewhere = |x, y| BareBadge {
            otherwise: Some(point(px(x), px(y))),
            ..bare_at(100.0, 100.0, Status::Succeeded)
        };
        let mut placed = Vec::new();
        place_bare(
            vec![elsewhere(100.0, 120.0), elsewhere(110.0, 100.0)],
            &[title],
            &mut placed,
        );
        assert_eq!(placed_at(&placed), [(100.0, 120.0, Status::Succeeded)]);

        let mut unhindered = Vec::new();
        place_bare(vec![elsewhere(100.0, 120.0)], &[], &mut unhindered);
        assert_eq!(placed_at(&unhindered), [(100.0, 100.0, Status::Succeeded)]);
    }

    /// The corners of the glyph `status` is drawn as, for a half-width of
    /// one; `None` for a round one.
    fn glyph_corners(status: Status) -> Option<Vec<(f32, f32)>> {
        let block = |w: f32, h: f32| vec![(-w, -h), (w, -h), (w, h), (-w, h)];
        match Glyph::of(status) {
            Glyph::Disc | Glyph::Ring => None,
            Glyph::Bar => Some(block(1.0, BAR_HALF_HEIGHT)),
            Glyph::Square => Some(block(SQUARE_HALF_SIDE, SQUARE_HALF_SIDE)),
            Glyph::TriangleUp => Some(TRIANGLE.to_vec()),
            Glyph::TriangleDown => Some(TRIANGLE_DOWN.to_vec()),
            Glyph::Cross => Some(cross().to_vec()),
        }
    }

    #[test]
    fn no_status_glyph_is_the_diamond_of_a_transition_badge() {
        for status in Status::ALL {
            let Some(corners) = glyph_corners(status) else {
                continue;
            };
            let diamond = corners.len() == DIAMOND.len()
                && DIAMOND.iter().all(|(x, y)| {
                    corners.iter().any(|(cx, cy)| {
                        (cx - x).abs() < OUTLINE_SLACK && (cy - y).abs() < OUTLINE_SLACK
                    })
                });
            assert!(!diamond, "{status:?}");
        }
    }

    /// How far two coordinates may part and still count as one, covering the
    /// rounding of the f32 arithmetic an outline goes through.
    const OUTLINE_SLACK: f32 = 1e-4;

    fn assert_outline(found: &[(f32, f32)], expected: &[(f32, f32)]) {
        assert_eq!(found.len(), expected.len());
        for (found, expected) in found.iter().zip(expected) {
            assert!(
                (found.0 - expected.0).abs() < OUTLINE_SLACK
                    && (found.1 - expected.1).abs() < OUTLINE_SLACK,
                "{found:?} for {expected:?}"
            );
        }
    }

    /// A rim stands the same width off every side of its glyph, round a corner
    /// turning either way and whichever way round the corners are listed.
    #[test]
    fn an_outline_set_out_keeps_one_distance_from_every_side() {
        let square = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)];
        let grown = [(-1.5, -1.5), (1.5, -1.5), (1.5, 1.5), (-1.5, 1.5)];
        assert_outline(&outset(square, 0.5), &grown);
        let (mut reversed, mut grown_reversed) = (square, grown);
        reversed.reverse();
        grown_reversed.reverse();
        assert_outline(&outset(reversed, 0.5), &grown_reversed);

        // An L, whose one corner turns the other way from the rest.
        let l = [
            (0.0, 0.0),
            (2.0, 0.0),
            (2.0, 1.0),
            (1.0, 1.0),
            (1.0, 2.0),
            (0.0, 2.0),
        ];
        let grown = [
            (-0.25, -0.25),
            (2.25, -0.25),
            (2.25, 1.25),
            (1.25, 1.25),
            (1.25, 2.25),
            (-0.25, 2.25),
        ];
        assert_outline(&outset(l, 0.25), &grown);
    }
}
