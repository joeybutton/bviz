//! A connector's lanes, the arrowhead capping them, and its transition badge.

use super::stroke::ConnectorInk;
use super::{faded, paint_path};
use crate::Palette;
use crate::camera::{Camera, to_screen};
use bviz_graph_layout::transition::Transition;
use bviz_graph_layout::{Connector, EdgeKind};
use gpui::{Bounds, PathBuilder, Pixels, Point, Rgba, Window, point, px};

/// Arrowhead size along and across the edge direction, in layout pixels.
const ARROW_LENGTH: f64 = 9.0;
const ARROW_HALF_WIDTH: f64 = 4.0;
/// How far the stroke runs past the arrowhead base into the head, closing the
/// seam without letting the line broaden the visible tip.
const ARROW_OVERDRAW: f64 = 1.0;

/// Stroke width (pre-zoom px) of the line a connector's provenance is drawn in.
pub(super) const CONNECTOR_WIDTH: f32 = 1.5;

/// Screen width the thinnest connector line is never drawn below. A band
/// narrower than a pixel can pass between the rasterizer's samples entirely, so
/// whether the line is drawn at all turns on where the pan happens to leave it;
/// a pixel across, it meets a sample wherever it falls.
const CONNECTOR_FLOOR: f32 = 1.0;

/// The zoom at which the thinnest line reaches [`CONNECTOR_FLOOR`]: at and above
/// it the camera sets every stroke's width, below it the floor does.
const FLOORED_BELOW: f32 = CONNECTOR_FLOOR / CONNECTOR_WIDTH;

/// The share of its own opacity a connector is never drawn below, however far
/// the zoom is out: a route a map draws too faint to find is off the map. Set
/// where a line thinned to that share still stands a
/// [`bviz_ui_theme::MIN_DISTINCT_COLOR`] off every ground it crosses, which is
/// the distance the unthinned line is held to.
pub(super) const TAPER_FLOOR: f32 = 0.3;

/// Length (pre-zoom px) of one dash of a bridged route and of the gap after it,
/// the gap wide enough that the dashes read as separate marks at the widths a
/// bundle of lanes can reach.
const BRIDGE_DASH: f32 = 7.0;
const BRIDGE_GAP: f32 = 5.0;

/// Screen length a dash or a gap of a bridged route is never drawn below. Path
/// coverage is sampled rather than computed, so a pattern the camera has shrunk
/// toward a pixel is drawn or dropped by where the pan happens to leave each
/// dash, and the route flickers between a dotted line and a solid one; several
/// pixels long, every dash and every gap meets samples wherever it falls.
const BRIDGE_PATTERN_FLOOR: f32 = 3.0;

/// Stroke width (pre-zoom px) of that line where a marking picks the connector out.
/// A doubling is the smallest step that reads as a heavier line rather than as
/// an artifact of antialiasing.
const MARKED_WIDTH: f32 = 2.0 * CONNECTOR_WIDTH;

/// Half the diagonal (pre-zoom px) of the diamond set halfway along a connector
/// that crosses a configuration transition, wide enough to stand across every
/// lane of a bundle so the mark reads as the pair's rather than one lane's.
const BADGE_HALF: f32 = 5.0;

/// Screen half-diagonal a transition badge is never drawn below: it marks an
/// edge a reader is looking for, which zooming out to find must not shrink away.
pub(crate) const BADGE_FLOOR: f32 = 4.0;

/// Screen width of the canvas-coloured rim around a transition badge, which
/// parts the badge from the route it sits on.
const BADGE_RIM: f32 = 1.5;

/// Bare ground (pre-zoom px) left between the strokes of two adjacent lanes, so
/// a bundle of them reads as several lines rather than as one broad one. The
/// zoom carries it along with the strokes it separates, so what holds at every
/// zoom is its ratio to them.
const LANE_GAP: f32 = 1.0;

/// Perpendicular gap (pre-zoom px) between the parallel provenance lines drawn
/// along a node pair that several queries connect. Either lane may be marked and
/// so drawn at [`MARKED_WIDTH`], and every lane of an unmarked bundle pays for
/// that too.
const PARALLEL_SPACING: f32 = MARKED_WIDTH + LANE_GAP;

/// The screen polylines of the connector being inked and of the lane being
/// drawn off it, which every connector of a frame reuses.
#[derive(Default)]
pub(super) struct Courses {
    route: Vec<(f32, f32)>,
    lane: Vec<(f32, f32)>,
}

/// Inks a connector's provenances as evenly spaced parallel lanes centered on
/// its route, `marked` where a marking has picked the connector out.
pub(super) fn ink_lanes(
    c: &Connector,
    marked: bool,
    alpha: f32,
    bounds: Bounds<Pixels>,
    camera: Camera,
    courses: &mut Courses,
    ink: &mut ConnectorInk,
) {
    let z = camera.zoom;
    let Courses { route, lane } = courses;
    route.clear();
    route.extend(c.points.iter().map(|(x, y)| {
        let s = to_screen(*x, *y, bounds, camera);
        (f32::from(s.x), f32::from(s.y))
    }));
    let n = c.kinds.len() as f32;
    for (i, kind) in c.kinds.iter().enumerate() {
        let slot = i as f32 - (n - 1.0) / 2.0;
        let course = lane_polyline(route, slot * PARALLEL_SPACING * z, lane);
        let style = Ink {
            kind: *kind,
            marked,
            alpha,
        };
        ink.add_lane(course, style);
    }
}

/// Sets the diamond marking `transition` halfway along `c`'s route.
pub(super) fn paint_badge(
    c: &Connector,
    transition: Transition,
    alpha: f32,
    bounds: Bounds<Pixels>,
    camera: Camera,
    palette: &Palette,
    window: &mut Window,
) {
    let Some((x, y)) = midway(&c.points) else {
        return;
    };
    let center = to_screen(x, y, bounds, camera);
    let half = (BADGE_HALF * camera.zoom).max(BADGE_FLOOR);
    let rim = faded(palette.background, alpha);
    paint_diamond(center, half + BADGE_RIM, rim, window);
    let fill = faded(palette.transition_color(transition), alpha);
    paint_diamond(center, half, fill, window);
}

/// The corners of a square standing on one corner, about its middle, for a
/// half-diagonal of one.
pub(super) const DIAMOND: [(f32, f32); 4] = [(0.0, -1.0), (1.0, 0.0), (0.0, 1.0), (-1.0, 0.0)];

/// Fills the square standing on one corner, centred on `center`, whose corners
/// lie `half` from it.
pub(crate) fn paint_diamond(center: Point<Pixels>, half: f32, color: Rgba, window: &mut Window) {
    let corners = DIAMOND.map(|(x, y)| point(center.x + px(x * half), center.y + px(y * half)));
    let mut diamond = PathBuilder::fill();
    diamond.add_polygon(&corners, true);
    if let Ok(path) = diamond.build() {
        paint_path(path, color, window);
    }
}

/// The point halfway along the polyline `points` by length; `None` for a route
/// with no points.
fn midway(points: &[(f64, f64)]) -> Option<(f64, f64)> {
    let first = *points.first()?;
    let legs = || {
        points.windows(2).map(|leg| {
            let (a, b) = (leg[0], leg[1]);
            (a, b, (b.0 - a.0).hypot(b.1 - a.1))
        })
    };
    let mut left = legs().map(|(_, _, length)| length).sum::<f64>() / 2.0;
    for (a, b, length) in legs() {
        if left <= length && length > 0.0 {
            let t = left / length;
            return Some((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t));
        }
        left -= length;
    }
    Some(first)
}

/// A polyline parallel to `pts` at signed perpendicular distance `d` (screen px),
/// for drawing a node pair's several provenances as evenly spaced parallel lanes.
/// Every vertex is pushed off its segments' normals — the endpoints perpendicular
/// to their one segment, the interior mitered at the corners — so a lane stays
/// parallel to the route along its whole length and meets each node offset
/// alongside its siblings rather than converging to a single point. An offset
/// lane is laid out in `out`.
pub(crate) fn lane_polyline<'a>(
    pts: &'a [(f32, f32)],
    d: f32,
    out: &'a mut Vec<(f32, f32)>,
) -> &'a [(f32, f32)] {
    if d == 0.0 || pts.len() < 2 {
        return pts;
    }
    let left_normal = |from: (f32, f32), to: (f32, f32)| {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let l = (dx * dx + dy * dy).sqrt().max(1e-3);
        (-dy / l, dx / l)
    };
    let n = pts.len();
    out.clear();
    out.reserve(n);
    let ns = left_normal(pts[0], pts[1]);
    out.push((pts[0].0 + ns.0 * d, pts[0].1 + ns.1 * d));
    for i in 1..n - 1 {
        out.push(miter_vertex(pts[i - 1], pts[i], pts[i + 1], d));
    }
    let ne = left_normal(pts[n - 2], pts[n - 1]);
    out.push((pts[n - 1].0 + ne.0 * d, pts[n - 1].1 + ne.1 * d));
    out
}

/// Offsets the corner `b` (between segments `a->b` and `b->c`) by `d` along the
/// miter direction, keeping it distance `d` from both segments. A near-reversal,
/// or a miter that would shoot far past a sharp corner, falls back to one
/// segment's normal.
fn miter_vertex(a: (f32, f32), b: (f32, f32), c: (f32, f32), d: f32) -> (f32, f32) {
    let unit = |(x, y): (f32, f32)| {
        let l = (x * x + y * y).sqrt().max(1e-3);
        (x / l, y / l)
    };
    let d1 = unit((b.0 - a.0, b.1 - a.1));
    let d2 = unit((c.0 - b.0, c.1 - b.1));
    let (n1, n2) = ((-d1.1, d1.0), (-d2.1, d2.0)); // left normals
    let m = (n1.0 + n2.0, n1.1 + n2.1);
    let ml = (m.0 * m.0 + m.1 * m.1).sqrt();
    if ml < 1e-3 {
        return (b.0 + n1.0 * d, b.1 + n1.1 * d);
    }
    let mdir = (m.0 / ml, m.1 / ml);
    let cos_half = (mdir.0 * n1.0 + mdir.1 * n1.1).max(0.35); // miter limit
    let scale = d / cos_half;
    (b.0 + mdir.0 * scale, b.1 + mdir.1 * scale)
}

/// What a connector lane is drawn in: its provenance's line, heavier and lifted
/// where `marked`, at `alpha` of its own opacity.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct Ink {
    pub(super) kind: EdgeKind,
    pub(super) marked: bool,
    pub(super) alpha: f32,
}

impl Ink {
    pub(super) fn color(self, palette: &Palette) -> Rgba {
        faded(palette.edge_color(self.kind, self.marked), self.alpha)
    }

    pub(super) fn width(self) -> f32 {
        match self.marked {
            true => MARKED_WIDTH,
            false => CONNECTOR_WIDTH,
        }
    }
}

/// The screen width a line of pre-zoom `width` is stroked at under `zoom`. The
/// floor lands on the zoom, which every width shares, so the widths hold their
/// ratio to one another and a marked line goes on reading as the heavier one. It
/// reaches the stroke alone: a route's course, the arrowhead capping it and the
/// spacing of a bundle's lanes all follow the camera, so a route goes on meeting
/// its boxes where the layout puts it.
pub(super) fn stroke_width(width: f32, zoom: f32) -> Pixels {
    px(width * zoom.max(FLOORED_BELOW))
}

/// The dash and the gap a bridged route is drawn in at `zoom`, each held to
/// [`BRIDGE_PATTERN_FLOOR`] however far the camera is out. The arrowhead capping
/// the route is filled whole, so the direction it reports is not a dash the
/// pattern might have left out.
pub(super) fn bridge_pattern(zoom: f32) -> (Pixels, Pixels) {
    (
        px((BRIDGE_DASH * zoom).max(BRIDGE_PATTERN_FLOOR)),
        px((BRIDGE_GAP * zoom).max(BRIDGE_PATTERN_FLOOR)),
    )
}

/// The share of its own opacity a connector is drawn at, standing for the share
/// of the drawn band the line itself accounts for: [`stroke_width`] holds the
/// stroke while the graph goes on shrinking under it, so the ink is taken back
/// down by as much as the held width adds. Bounded below by [`TAPER_FLOOR`].
pub(super) fn ink_taper(zoom: f32) -> f32 {
    (zoom / FLOORED_BELOW).clamp(TAPER_FLOOR, 1.0)
}

/// The unit vector from `a` to `b`, and how far apart they are.
pub(super) fn direction(a: Point<Pixels>, b: Point<Pixels>) -> ((f32, f32), f32) {
    let (dx, dy) = (f32::from(b.x - a.x), f32::from(b.y - a.y));
    let len = (dx * dx + dy * dy).sqrt().max(1e-3);
    ((dx / len, dy / len), len)
}

/// How far back from the tip a connector's line stops.
pub(super) fn stem_trim(zoom: f32) -> f32 {
    (ARROW_LENGTH - ARROW_OVERDRAW) as f32 * zoom
}

/// The triangle capping a connector at `tip` and pointing along the unit vector
/// `dir`, sized as [`ARROW_LENGTH`] and [`ARROW_HALF_WIDTH`] give it at `zoom`.
pub(super) fn arrowhead(tip: Point<Pixels>, dir: (f32, f32), zoom: f32) -> [Point<Pixels>; 3] {
    let (length, half_width) = (ARROW_LENGTH as f32 * zoom, ARROW_HALF_WIDTH as f32 * zoom);
    let (ux, uy) = dir;
    let (nx, ny) = (-uy, ux);
    let base = point(tip.x - px(ux * length), tip.y - px(uy * length));
    [
        tip,
        point(base.x + px(nx * half_width), base.y + px(ny * half_width)),
        point(base.x - px(nx * half_width), base.y - px(ny * half_width)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::neighborhood::Neighborhood;
    use crate::paint::{Marks, UNMARKED_FADE};
    use bviz_graph_layout::Id;
    use std::collections::HashSet;
    use std::rc::Rc;

    /// A badge sits halfway along a route by length, whatever its bends, so a
    /// long leg carries it rather than the bend nearest the middle point.
    #[test]
    fn midway_falls_halfway_along_the_route() {
        assert_eq!(midway(&[]), None);
        assert_eq!(midway(&[(3.0, 4.0)]), Some((3.0, 4.0)));
        assert_eq!(midway(&[(0.0, 0.0), (10.0, 0.0)]), Some((5.0, 0.0)));
        assert_eq!(
            midway(&[(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (2.0, 20.0)]),
            Some((2.0, 9.0))
        );
    }

    /// The zooms the stroke invariants are checked over, spanning both sides of
    /// the zoom at which the thinnest line reaches the floor.
    const STROKE_ZOOMS: [f32; 8] = [1e-4, 1e-3, 0.01, 0.1, 0.5, 0.9, 1.0, 4.0];

    /// A line thinner than a pixel is drawn or not according to where the pan
    /// leaves it, so the zoom-out that shrinks a graph to a map must not take
    /// the routes off it.
    #[test]
    fn a_connector_holds_a_pixel_of_width_however_far_the_zoom_is_out() {
        for zoom in STROKE_ZOOMS {
            let w = stroke_width(CONNECTOR_WIDTH, zoom);
            assert!(w >= px(CONNECTOR_FLOOR), "zoom {zoom}: {w:?}");
        }
    }

    /// A mark says which route it picks out by the heavier line it draws it in,
    /// which it has to go on saying at a zoom holding the thinnest line up.
    #[test]
    fn a_marked_connector_stays_the_heavier_line_at_every_zoom() {
        for zoom in STROKE_ZOOMS {
            let (marked, bare) = (
                stroke_width(MARKED_WIDTH, zoom),
                stroke_width(CONNECTOR_WIDTH, zoom),
            );
            assert!(marked > bare, "zoom {zoom}: {marked:?} over {bare:?}");
        }
    }

    /// A dash the camera has shrunk toward a pixel is kept or dropped by where
    /// the pan leaves it, so a bridge zoomed out to a map reads as a dotted line
    /// rather than flickering between one and a solid line; zoomed in, the
    /// pattern follows the camera like the route it runs along.
    #[test]
    fn a_bridge_holds_its_pattern_however_far_the_zoom_is_out() {
        for zoom in STROKE_ZOOMS {
            let (dash, gap) = bridge_pattern(zoom);
            assert!(dash >= px(BRIDGE_PATTERN_FLOOR), "zoom {zoom}: {dash:?}");
            assert!(gap >= px(BRIDGE_PATTERN_FLOOR), "zoom {zoom}: {gap:?}");
        }
        assert_eq!(
            bridge_pattern(4.0),
            (px(BRIDGE_DASH * 4.0), px(BRIDGE_GAP * 4.0))
        );
    }

    /// The floor is what a zoom-out runs into, and nothing a line wider than it
    /// meets: above the floor the camera alone says how wide a line is drawn.
    #[test]
    fn a_line_the_zoom_keeps_above_the_floor_follows_the_camera() {
        for zoom in [0.7, 1.0, 4.0] {
            assert_eq!(
                stroke_width(CONNECTOR_WIDTH, zoom),
                px(CONNECTOR_WIDTH * zoom)
            );
        }
    }

    /// Above the floor a line is drawn at the width the camera asks for, so all
    /// of the band drawn is its own and none of the ink is taken back. The taper
    /// meets the floor at the zoom the floor begins at, leaving no step in a line
    /// as the zoom carries it across.
    #[test]
    fn a_line_the_zoom_keeps_above_the_floor_is_drawn_whole() {
        for zoom in STROKE_ZOOMS.iter().filter(|z| **z >= FLOORED_BELOW) {
            assert_eq!(ink_taper(*zoom), 1.0, "zoom {zoom}");
        }
        assert_eq!(ink_taper(FLOORED_BELOW), 1.0);
        let approaching = ink_taper(FLOORED_BELOW * (1.0 - CONTINUITY_STEP));
        assert!(
            (1.0 - approaching) <= CONTINUITY_STEP,
            "the taper steps to {approaching} as the floor is met"
        );
    }

    /// How far below [`FLOORED_BELOW`] the taper is read to check it meets the
    /// floor continuously, which bounds the step it may take there as well.
    const CONTINUITY_STEP: f32 = 1e-3;

    /// A map fits a whole graph into a window, taking the zoom as far out as the
    /// reader ever sees it; the routes drawn there are thinned but never thinned
    /// away.
    #[test]
    fn the_taper_holds_its_floor_however_far_the_zoom_is_out() {
        let zooms = STROKE_ZOOMS
            .iter()
            .copied()
            .chain([0.0, f32::MIN_POSITIVE, 1e-30]);
        for zoom in zooms {
            let taper = ink_taper(zoom);
            assert!((TAPER_FLOOR..=1.0).contains(&taper), "zoom {zoom}: {taper}");
        }
    }

    /// A selection says which routes the reader is following and how far the rest
    /// falls back; the zoom says how much of a drawn band is the line's own. The
    /// two answer different questions, so each takes its share of what the other
    /// leaves and a selection reads the same at every zoom.
    #[test]
    fn a_selection_stands_a_connector_off_the_graph_by_as_much_at_every_zoom() {
        let marks = Marks {
            hover: Rc::new(Neighborhood::none()),
            selection: Rc::new(Neighborhood {
                boxes: HashSet::from([Id(0)]),
                edges: HashSet::new(),
            }),
        };
        for zoom in STROKE_ZOOMS {
            let marked = marks.connector_alpha(true, 1.0, 1.0, zoom);
            let unmarked = marks.connector_alpha(false, 1.0, 1.0, zoom);
            assert_eq!(marked, ink_taper(zoom), "zoom {zoom}");
            assert_eq!(unmarked, marked * UNMARKED_FADE, "zoom {zoom}");
        }
    }

    /// A morph fades its routes out and back as the layout it draws them from is
    /// replaced, which the taper has to leave whole: a route at the turn is gone,
    /// not merely faint.
    #[test]
    fn a_morph_takes_its_routes_all_the_way_out_at_every_zoom() {
        let marks = Marks {
            hover: Rc::new(Neighborhood::none()),
            selection: Rc::new(Neighborhood::none()),
        };
        for zoom in STROKE_ZOOMS {
            assert_eq!(
                marks.connector_alpha(true, 1.0, 0.0, zoom),
                0.0,
                "zoom {zoom}"
            );
        }
    }
}
