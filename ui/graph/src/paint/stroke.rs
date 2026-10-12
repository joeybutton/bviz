//! Connector lanes tessellated with lyon and batched into as few paths as keep their stack.

use super::connector::{Ink, arrowhead, bridge_pattern, direction, stem_trim, stroke_width};
use super::footprint::Footprints;
use super::paint_path;
use crate::Palette;
use bviz_graph_layout::EdgeKind;
use gpui::{Path, Pixels, Point, Rgba, Size, Window, point, px};
use lyon_tessellation::math::Point as LyonPoint;
use lyon_tessellation::path::builder::NoAttributes;
use lyon_tessellation::{
    BuffersBuilder, StrokeBuilder, StrokeOptions, StrokeTessellator, StrokeVertex, VertexBuffers,
};
use rustc_hash::FxHashMap;

/// One path of lanes, all in the `ink`th ink a frame draws. Batches are painted
/// in their order: by `layer`, then by ink.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct Batch {
    pub(super) layer: u32,
    pub(super) ink: u32,
}

/// A frame's connector lanes, gathered into as few paths as keep their stack:
/// a lane is drawn over every lane of another ink that it overlaps and that was
/// added before it. The order connectors are added in thus sets the stack, so
/// where lanes of several provenances crowd one bundle they interleave and no
/// provenance buries another. Lanes of one ink composite alike in either order,
/// since the path rasterizer blends every triangle on its own, so they share a
/// batch wherever no other ink comes between them.
pub(super) struct ConnectorInk {
    zoom: f32,
    inks: Vec<Ink>,
    batches: FxHashMap<Batch, VertexBuffers<LyonPoint, u32>>,
    footprints: Footprints,
    /// The cells of the lane being added.
    footprint: Vec<usize>,
    tessellator: StrokeTessellator,
}

impl ConnectorInk {
    pub(super) fn new(zoom: f32, window: Size<Pixels>) -> Self {
        Self {
            zoom,
            inks: Vec::new(),
            batches: FxHashMap::default(),
            footprints: Footprints::new(window),
            footprint: Vec::new(),
            tessellator: StrokeTessellator::new(),
        }
    }

    /// Strokes a connector lane (screen coordinates) and caps its end with a
    /// filled arrowhead pointing along the final segment.
    pub(super) fn add_lane(&mut self, points: &[(f32, f32)], ink: Ink) {
        let [.., before, last] = points else {
            return;
        };
        let zoom = self.zoom;
        let tip = point(px(last.0), px(last.1));
        let (dir, seg) = direction(point(px(before.0), px(before.1)), tip);
        let trim = stem_trim(zoom).min(seg);
        let line_end = LyonPoint::new(last.0 - dir.0 * trim, last.1 - dir.1 * trim);
        let head = arrowhead(tip, dir, zoom);
        let width = f32::from(stroke_width(ink.width(), zoom));
        let options = StrokeOptions::default().with_line_width(width);

        self.footprint.clear();
        // A miter reaches no further from its corner than the limit allows.
        let reach = width / 2.0 * options.miter_limit;
        for segment in points.windows(2) {
            self.footprints.cover(segment, reach, &mut self.footprint);
        }
        let corners = head.map(|p| (f32::from(p.x), f32::from(p.y)));
        self.footprints.cover(&corners, 0.0, &mut self.footprint);

        let rank = match self.inks.iter().position(|i| *i == ink) {
            Some(rank) => rank,
            None => {
                self.inks.push(ink);
                self.inks.len() - 1
            }
        } as u32;
        let cells = &mut self.footprints.cells;
        let under = self
            .footprint
            .iter()
            .filter_map(|&c| cells[c].last_unlike(rank))
            .max();
        let batch = match under {
            None => Batch {
                layer: 0,
                ink: rank,
            },
            Some(under) if under.ink < rank => Batch {
                layer: under.layer,
                ink: rank,
            },
            Some(under) => Batch {
                layer: under.layer + 1,
                ink: rank,
            },
        };
        for &c in &self.footprint {
            cells[c].draw(batch);
        }

        let buffers = self.batches.entry(batch).or_default();
        let mut output = BuffersBuilder::new(buffers, |v: StrokeVertex| v.position());
        let mut stroke = self.tessellator.builder(&options, &mut output);
        let course = points[..points.len() - 1]
            .iter()
            .map(|&(x, y)| LyonPoint::new(x, y))
            .chain(std::iter::once(line_end));
        match ink.kind {
            EdgeKind::Transitive => {
                let (dash, gap) = bridge_pattern(zoom);
                add_dashed(course, f32::from(dash), f32::from(gap), &mut stroke);
            }
            _ => add_solid(course, &mut stroke),
        }
        // A lane the tessellator rejects is left out.
        let _ = stroke.build();

        let first = buffers.vertices.len() as u32;
        let corners = head.map(|p| LyonPoint::new(f32::from(p.x), f32::from(p.y)));
        buffers.vertices.extend(corners);
        buffers.indices.extend(first..first + 3);
    }

    /// Every batch's ink and geometry, in the order they are painted.
    fn into_batches(self) -> impl Iterator<Item = (Ink, VertexBuffers<LyonPoint, u32>)> {
        let mut batches: Vec<_> = self.batches.into_iter().collect();
        batches.sort_unstable_by_key(|(batch, _)| *batch);
        let inks = self.inks;
        batches
            .into_iter()
            .map(move |(batch, buffers)| (inks[batch.ink as usize], buffers))
    }

    /// Paints every batch, in order.
    pub(super) fn paint(self, palette: &Palette, window: &mut Window) {
        for (ink, buffers) in self.into_batches() {
            paint_triangles(triangles(&buffers), ink.color(palette), window);
        }
    }
}

/// The triangles `buffers` holds, by their corners.
fn triangles(
    buffers: &VertexBuffers<LyonPoint, u32>,
) -> impl ExactSizeIterator<Item = [Point<Pixels>; 3]> {
    let at = |i: u32| {
        let v = buffers.vertices[i as usize];
        point(px(v.x), px(v.y))
    };
    let corners = buffers.indices.chunks_exact(3);
    corners.map(move |t| [at(t[0]), at(t[1]), at(t[2])])
}

/// The texture coordinate gpui's path rasterizer reads as a solid fill.
const SOLID: Point<f32> = Point { x: 0.0, y: 1.0 };

/// Fills `triangles` in `color` as one path.
fn paint_triangles(
    mut triangles: impl ExactSizeIterator<Item = [Point<Pixels>; 3]>,
    color: Rgba,
    window: &mut Window,
) {
    let vertices = 3 * triangles.len();
    let Some([a, b, c]) = triangles.next() else {
        return;
    };
    let mut path = Path::new(a);
    path.vertices.reserve_exact(vertices);
    path.push_triangle((a, b, c), (SOLID, SOLID, SOLID));
    for [a, b, c] in triangles {
        path.push_triangle((a, b, c), (SOLID, SOLID, SOLID));
    }
    paint_path(path, color, window);
}

/// Adds the polyline `course` to `stroke` as one open subpath.
fn add_solid(
    mut course: impl Iterator<Item = LyonPoint>,
    stroke: &mut NoAttributes<StrokeBuilder<'_>>,
) {
    let Some(first) = course.next() else {
        return;
    };
    stroke.begin(first);
    for p in course {
        stroke.line_to(p);
    }
    stroke.end(false);
}

/// Adds the polyline `course` to `stroke` as dashes `dash` long, `gap` apart,
/// the first starting at its first point and the last cut short at its end.
fn add_dashed(
    mut course: impl Iterator<Item = LyonPoint>,
    dash: f32,
    gap: f32,
    stroke: &mut NoAttributes<StrokeBuilder<'_>>,
) {
    let Some(mut from) = course.next() else {
        return;
    };
    stroke.begin(from);
    let mut drawing = true;
    let mut left = dash;
    for to in course {
        let length = (to - from).length();
        let mut done = 0.0;
        while length - done > left {
            done += left;
            let at = from.lerp(to, done / length);
            match drawing {
                true => {
                    stroke.line_to(at);
                    stroke.end(false);
                }
                false => {
                    stroke.begin(at);
                }
            }
            drawing = !drawing;
            left = if drawing { dash } else { gap };
        }
        left -= length - done;
        if drawing {
            stroke.line_to(to);
        }
        from = to;
    }
    if drawing {
        stroke.end(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paint::connector::CONNECTOR_WIDTH;
    use gpui::PathBuilder;

    /// Each triangle of `vertices` (taken three at a time) with its corners
    /// rounded to a hundredth of a pixel and put in order, the triangles sorted.
    fn triangle_set(vertices: &[Point<Pixels>]) -> Vec<[(i64, i64); 3]> {
        let round = |p: &Point<Pixels>| {
            let at = |v: Pixels| (f32::from(v) * 100.0).round() as i64;
            (at(p.x), at(p.y))
        };
        let mut set: Vec<[(i64, i64); 3]> = vertices
            .chunks_exact(3)
            .map(|t| {
                let mut corners = [round(&t[0]), round(&t[1]), round(&t[2])];
                corners.sort();
                corners
            })
            .collect();
        set.sort();
        set
    }

    /// A window wide enough for every lane a test draws.
    const WINDOW: Size<Pixels> = Size {
        width: px(1000.0),
        height: px(1000.0),
    };

    /// The ink of an unmarked line of `kind`.
    fn plain(kind: EdgeKind) -> Ink {
        Ink {
            kind,
            marked: false,
            alpha: 1.0,
        }
    }

    /// The batches `inked` holds, in the order they are painted, each as its
    /// triangles' corners.
    fn painted(inked: ConnectorInk) -> Vec<Vec<Point<Pixels>>> {
        inked
            .into_batches()
            .map(|(_, buffers)| triangles(&buffers).flatten().collect())
            .collect()
    }

    /// A lane is inked as the very triangles gpui's own stroke and fill give the
    /// line and the head it stands for, its dashes included: a dash may run
    /// around a bend.
    #[test]
    fn a_lane_is_inked_as_gpui_draws_it() {
        let zoom = 1.0;
        let lane = [(0.0, 0.0), (37.0, 0.0), (37.0, 30.0), (100.0, 30.0)];
        let tip = point(px(100.0), px(30.0));
        let stem_end = point(px(100.0 - stem_trim(zoom)), px(30.0));
        for kind in [EdgeKind::Query, EdgeKind::Transitive] {
            let mut inked = ConnectorInk::new(zoom, WINDOW);
            inked.add_lane(&lane, plain(kind));
            let ours = painted(inked).concat();

            let mut line = PathBuilder::stroke(stroke_width(CONNECTOR_WIDTH, zoom));
            if kind == EdgeKind::Transitive {
                let (dash, gap) = bridge_pattern(zoom);
                line = line.dash_array(&[dash, gap]);
            }
            line.move_to(point(px(0.0), px(0.0)));
            line.line_to(point(px(37.0), px(0.0)));
            line.line_to(point(px(37.0), px(30.0)));
            line.line_to(stem_end);
            let mut head = PathBuilder::fill();
            head.add_polygon(&arrowhead(tip, (1.0, 0.0), zoom), true);
            let theirs: Vec<Point<Pixels>> = [line, head]
                .into_iter()
                .flat_map(|path| path.build().expect("a polyline tessellates").vertices)
                .map(|v| v.xy_position)
                .collect();
            assert_eq!(triangle_set(&ours), triangle_set(&theirs), "{kind:?}");
        }
    }

    /// Lanes that cross no lane of another ink share their ink's one path, however
    /// many a frame draws.
    #[test]
    fn lanes_apart_from_other_inks_share_their_paths() {
        let mut inked = ConnectorInk::new(1.0, WINDOW);
        for row in 0..5 {
            let y = row as f32 * 200.0;
            inked.add_lane(&[(0.0, y), (300.0, y)], plain(EdgeKind::Query));
            inked.add_lane(&[(0.0, y), (300.0, y)], plain(EdgeKind::Aquery));
            inked.add_lane(&[(600.0, y), (900.0, y)], plain(EdgeKind::Query));
        }
        assert_eq!(painted(inked).len(), 2);
    }

    /// A lane goes over every lane of another ink it was added after and
    /// overlaps, so batching leaves the stack as the lanes were added.
    #[test]
    fn a_lane_goes_over_the_lanes_of_other_inks_before_it() {
        let mut inked = ConnectorInk::new(1.0, WINDOW);
        let (a, b, c) = (
            [(0.0, 10.0), (300.0, 10.0)],
            [(100.0, 0.0), (100.0, 300.0)],
            [(0.0, 100.0), (300.0, 100.0)],
        );
        inked.add_lane(&a, plain(EdgeKind::Query));
        inked.add_lane(&b, plain(EdgeKind::Aquery));
        inked.add_lane(&c, plain(EdgeKind::Query));
        let reach = |batch: &Vec<Point<Pixels>>| {
            let ys = batch.iter().map(|p| f32::from(p.y));
            ys.fold(f32::MIN, f32::max)
        };
        let order: Vec<f32> = painted(inked).iter().map(reach).collect();
        assert_eq!(order.len(), 3, "{order:?}");
        assert!(
            order[0] < 50.0 && order[1] > 250.0 && order[2] < 250.0,
            "{order:?}"
        );
    }
}
