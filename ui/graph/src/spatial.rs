use crate::GraphPanel;
use crate::camera::Rect;
use crate::derive::{Drawn, Shown, ViewKey};
use crate::morph::Frame;
use bviz_graph_layout::{Box, Connector, Id, LaidOut};
use fixedbitset::FixedBitSet;
use gpui::{Context, Task};
use std::ops::Range;
use std::sync::Arc;

/// How many items a [`Grid`]'s finest level lays one cell per.
const ITEMS_PER_CELL: f64 = 1.0;

/// A drawing's boxes and connector routes bucketed by where they lie in layout
/// space, so a frame reaches what lies near the part of the drawing it shows
/// without walking all of it.
pub(crate) struct Spatial {
    boxes: Grid,
    routes: Grid,
    /// Each connector's index under each box it ends at, ascending by box.
    ends: Vec<(Id, u32)>,
}

impl Spatial {
    fn of(g: &LaidOut) -> Self {
        Self {
            boxes: Grid::build(g.boxes.len(), || {
                g.boxes.iter().enumerate().map(|(i, b)| {
                    let r = Rect::of(b);
                    (i, Span::of(&r))
                })
            }),
            routes: Grid::build(g.connectors.len(), || {
                g.connectors.iter().enumerate().flat_map(|(i, c)| {
                    c.points
                        .windows(2)
                        .map(move |leg| (i, Span::leg(leg[0], leg[1])))
                })
            }),
            ends: {
                let mut ends = Vec::with_capacity(g.connectors.len() * 2);
                for (i, c) in g.connectors.iter().enumerate() {
                    ends.extend([(c.from, narrow(i)), (c.to, narrow(i))]);
                }
                ends.sort_unstable();
                ends
            },
        }
    }

    /// The connectors ending at box `id`.
    fn ending_at(&self, id: Id) -> impl Iterator<Item = usize> + '_ {
        let first = self.ends.partition_point(|&(end, _)| end < id);
        self.ends[first..]
            .iter()
            .take_while(move |&&(end, _)| end == id)
            .map(|&(_, i)| i as usize)
    }
}

/// A drawing paired with its [`Spatial`] index, so a query's answers index the
/// drawing they were bucketed from. Absent an index, every item is a candidate.
pub(crate) struct Indexed<'a> {
    g: &'a LaidOut,
    spatial: Option<Arc<Spatial>>,
}

impl<'a> Indexed<'a> {
    #[cfg(test)]
    pub(crate) fn of(g: &'a LaidOut) -> Self {
        Self {
            g,
            spatial: Some(Arc::new(Spatial::of(g))),
        }
    }

    #[cfg(test)]
    fn unbuilt(g: &'a LaidOut) -> Self {
        Self { g, spatial: None }
    }

    /// The boxes of the drawing that may be drawn over `r` this frame, in the
    /// drawing's order, each where `morph` places it and at the alpha it paints
    /// at: every box that is, and some that lie near it. A morph moves boxes off
    /// the rects the index holds them at, so while one runs every box is a
    /// candidate.
    pub(crate) fn boxes_near<'m>(
        &self,
        r: &Rect,
        morph: Option<&'m Frame>,
    ) -> impl Iterator<Item = (&'a Box, Rect, f32)> + use<'a, 'm> {
        let candidates = match (&self.spatial, morph) {
            (Some(spatial), None) => Candidates::Near(spatial.boxes.near(r).into_ones()),
            _ => Candidates::Every(0..self.g.boxes.len()),
        };
        let boxes = &self.g.boxes;
        candidates.map(move |i| {
            let b = &boxes[i];
            let (rect, alpha) = morph.map_or((Rect::of(b), 1.0), |m| m.place(b));
            (b, rect, alpha)
        })
    }

    /// The connectors whose routes may cross `r`, in the drawing's order: every
    /// one that does and either ends at a box of `anchors` or a view the size
    /// of `r` [could hold](Rect::could_hold), and some that pass near it.
    pub(crate) fn routes_near<A: IntoIterator<Item = Id>>(
        &self,
        r: &Rect,
        anchors: A,
    ) -> impl Iterator<Item = &'a Connector> + use<'a, A> {
        let connectors = &self.g.connectors;
        let candidates = match &self.spatial {
            Some(spatial) => {
                let mut near = spatial.routes.near_no_larger(r);
                for anchor in anchors {
                    for i in spatial.ending_at(anchor) {
                        near.insert(i);
                    }
                }
                Candidates::Near(near.into_ones())
            }
            None => Candidates::Every(0..connectors.len()),
        };
        candidates.map(move |i| &connectors[i])
    }
}

/// Item indices a query visits, ascending.
enum Candidates {
    Every(Range<usize>),
    Near(fixedbitset::IntoOnes),
}

impl Iterator for Candidates {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        match self {
            Self::Every(all) => all.next(),
            Self::Near(ones) => ones.next(),
        }
    }
}

/// The index of every view of the drawing a panel draws that a frame has
/// asked for, so each is bucketed once however many frames draw it. A large
/// view takes a frame or more to bucket, so that runs off the UI thread.
#[derive(Default)]
pub(crate) struct SpatialMemo(Vec<(ViewKey, Built)>);

enum Built {
    /// The build is held so that it is not cancelled; it redraws the canvas
    /// once it lands.
    Pending {
        _build: Task<()>,
    },
    Ready(Arc<Spatial>),
}

impl SpatialMemo {
    /// The view `drawn` shows at `zoom`, paired with its index where that has
    /// been built; `None` where that view is yet to be derived.
    pub(crate) fn indexed<'a>(
        &mut self,
        drawn: &'a Drawn,
        zoom: f32,
        cx: &mut Context<GraphPanel>,
    ) -> Option<(Shown<'a>, Indexed<'a>)> {
        let shown = drawn.shown(zoom)?;
        let key = shown.key();
        self.0.retain(|(held, _)| held.drawing() == key.drawing());
        let spatial = match self.0.iter().find(|(held, _)| *held == key) {
            Some((_, Built::Ready(spatial))) => Some(Arc::clone(spatial)),
            Some((_, Built::Pending { .. })) => None,
            None => {
                let build = build(drawn.clone(), zoom, key, cx);
                self.0.push((key, Built::Pending { _build: build }));
                None
            }
        };
        let g = shown.view();
        Some((shown, Indexed { g, spatial }))
    }

    /// Takes up the index a build asked for `asked` produced, held under the key
    /// of the view it was built of: a level the background derives between the
    /// ask and the build can move which view the same zoom shows.
    fn land(&mut self, asked: ViewKey, built: Option<(ViewKey, Arc<Spatial>)>) {
        self.0.retain(|(held, _)| *held != asked);
        if let Some((key, spatial)) = built {
            self.0.retain(|(held, _)| *held != key);
            self.0.push((key, Built::Ready(spatial)));
        }
    }
}

/// Indexes the view `drawn` shows at `zoom` on the background executor, landing
/// it in the panel's [`SpatialMemo`] as built for `asked`.
fn build(drawn: Drawn, zoom: f32, asked: ViewKey, cx: &mut Context<GraphPanel>) -> Task<()> {
    cx.spawn(async move |panel, cx| {
        let built = cx
            .background_executor()
            .spawn(async move {
                let shown = drawn.shown(zoom)?;
                Some((shown.key(), Arc::new(Spatial::of(shown.view()))))
            })
            .await;
        let _ = panel.update(cx, |panel, cx| {
            panel.spatial.land(asked, built);
            panel.invalidate_canvas_only(cx);
        });
    })
}

/// A closed axis-aligned extent in layout space.
#[derive(Clone, Copy)]
struct Span {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

impl Span {
    /// The extent `r` covers, its far edges summed as [`Rect::overlaps`] sums
    /// them.
    fn of(r: &Rect) -> Self {
        Self {
            left: r.x,
            top: r.y,
            right: r.x + r.w,
            bottom: r.y + r.h,
        }
    }

    fn leg(from: (f64, f64), to: (f64, f64)) -> Self {
        Self {
            left: from.0.min(to.0),
            top: from.1.min(to.1),
            right: from.0.max(to.0),
            bottom: from.1.max(to.1),
        }
    }
}

/// Uniform grids of cells over the extent its items span, each twice as coarse
/// as the one before and the last a single cell. A span is listed in the finest
/// grid whose cells are at least its size, so it meets at most two cells along
/// either axis however long it is. Each cell lists, ascending and once, every
/// item with a span meeting it that its grid holds.
///
/// An item's cells and a query's are both found by [`Level::column`] and
/// [`Level::row`], which never decrease along their axis; so where an item's
/// span meets a query's, their cell ranges in the item's grid meet too, and the
/// query reaches it.
struct Grid {
    left: f64,
    top: f64,
    levels: Vec<Level>,
    /// Where each cell's run of `items` begins, the levels' cells in turn, each
    /// level row by row; followed by where the last one ends.
    starts: Vec<u32>,
    items: Vec<u32>,
    /// How many items the grid indexes, which a query's answer is sized by.
    len: usize,
}

/// One of a [`Grid`]'s grids.
struct Level {
    cell_width: f64,
    cell_height: f64,
    columns: usize,
    rows: usize,
    /// The level's first cell among all the grid's.
    first: usize,
}

impl Level {
    fn column(&self, x: f64, left: f64) -> usize {
        Self::cell_along(x - left, self.cell_width, self.columns)
    }

    fn row(&self, y: f64, top: f64) -> usize {
        Self::cell_along(y - top, self.cell_height, self.rows)
    }

    /// The cell along one axis that `offset` into the extent falls in, the edge
    /// cells taking whatever lies past them.
    fn cell_along(offset: f64, size: f64, count: usize) -> usize {
        (offset / size).floor().clamp(0.0, (count - 1) as f64) as usize
    }

    /// The cells of this level `s` meets, row by row: each row's run of cells
    /// as a range of the grid's cell numbers.
    fn cells(&self, s: &Span, left: f64, top: f64) -> impl Iterator<Item = Range<usize>> + use<> {
        let columns = self.column(s.left, left)..self.column(s.right, left) + 1;
        let rows = self.row(s.top, top)..self.row(s.bottom, top) + 1;
        let (first, across) = (self.first, self.columns);
        rows.map(move |row| {
            let start = first + row * across;
            start + columns.start..start + columns.end
        })
    }
}

impl Grid {
    /// Buckets the `len` items `spans` yields, each by index and as many spans
    /// as it covers. `spans` is walked once to size the grids, once to count
    /// each cell's items and once to list them.
    fn build<I: Iterator<Item = (usize, Span)>>(len: usize, spans: impl Fn() -> I) -> Self {
        let (mut left, mut top) = (f64::INFINITY, f64::INFINITY);
        let (mut right, mut bottom) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for (_, s) in spans() {
            left = left.min(s.left);
            top = top.min(s.top);
            right = right.max(s.right);
            bottom = bottom.max(s.bottom);
        }
        if left > right || top > bottom {
            (left, top, right, bottom) = (0.0, 0.0, 0.0, 0.0);
        }
        let levels = Self::levels(right - left, bottom - top, len);
        let coarsest = &levels[levels.len() - 1];
        let cell_count = coarsest.first + coarsest.columns * coarsest.rows;
        let mut grid = Self {
            left,
            top,
            levels,
            starts: Vec::with_capacity(cell_count + 1),
            items: Vec::new(),
            len,
        };

        // The last item each cell took, so an item whose spans share a cell is
        // listed there once; items arrive in ascending order.
        let mut last = vec![usize::MAX; cell_count];
        let mut counts = vec![0usize; cell_count];
        grid.each_cell(spans(), &mut last, |cell, _| counts[cell] += 1);

        grid.starts.push(0);
        let mut total = 0;
        for count in &mut counts {
            total += *count;
            grid.starts.push(narrow(total));
            // Each cell's count becomes where its next item is written.
            *count = total - *count;
        }
        let mut items = vec![0; total];
        last.fill(usize::MAX);
        grid.each_cell(spans(), &mut last, |cell, item| {
            items[counts[cell]] = narrow(item);
            counts[cell] += 1;
        });
        grid.items = items;
        grid
    }

    /// The grids over an extent `width` by `height` holding `len` items, finest
    /// first.
    fn levels(width: f64, height: f64, len: usize) -> Vec<Level> {
        let cells = (len as f64 / ITEMS_PER_CELL).ceil().max(1.0);
        let side = (width * height / cells).sqrt();
        let finest_across = |extent: f64| {
            if side > 0.0 {
                (extent / side).ceil().clamp(1.0, cells)
            } else if extent > 0.0 {
                cells
            } else {
                1.0
            }
        };
        let cell = |extent: f64| {
            if extent > 0.0 {
                extent / finest_across(extent)
            } else {
                1.0
            }
        };
        let (mut cell_width, mut cell_height) = (cell(width), cell(height));
        let across = |extent: f64, size: f64| ((extent / size).ceil() as usize).max(1);
        let finest = across(width, cell_width).max(across(height, cell_height));
        let mut levels = Vec::with_capacity(finest.ilog2() as usize + 2);
        let mut first = 0;
        loop {
            let level = Level {
                cell_width,
                cell_height,
                columns: across(width, cell_width),
                rows: across(height, cell_height),
                first,
            };
            first += level.columns * level.rows;
            let single = level.columns == 1 && level.rows == 1;
            levels.push(level);
            if single {
                return levels;
            }
            cell_width *= 2.0;
            cell_height *= 2.0;
        }
    }

    /// The level a span is listed in: the finest whose cells are at least its
    /// size, or the coarsest, which is one cell over everything.
    fn level_of(&self, s: &Span) -> &Level {
        let (width, height) = (s.right - s.left, s.bottom - s.top);
        self.levels
            .iter()
            .find(|l| width <= l.cell_width && height <= l.cell_height)
            .unwrap_or(&self.levels[self.levels.len() - 1])
    }

    /// Calls `visit` with each cell an item of `spans` is listed in and the
    /// item, once per pair.
    fn each_cell(
        &self,
        spans: impl Iterator<Item = (usize, Span)>,
        last: &mut [usize],
        mut visit: impl FnMut(usize, usize),
    ) {
        for (item, s) in spans {
            for run in self.level_of(&s).cells(&s, self.left, self.top) {
                for cell in run {
                    if last[cell] != item {
                        last[cell] = item;
                        visit(cell, item);
                    }
                }
            }
        }
    }

    /// Every item with a span in a cell `r` meets.
    fn near(&self, r: &Rect) -> FixedBitSet {
        self.near_in(r, &self.levels)
    }

    /// Every item with a span no larger than `r` in a cell `r` meets, and some
    /// others. A span that size is listed no coarser than the first level whose
    /// cells are each at least the size of `r`, so only those levels are
    /// visited.
    fn near_no_larger(&self, r: &Rect) -> FixedBitSet {
        let holding = self
            .levels
            .iter()
            .position(|l| r.w <= l.cell_width && r.h <= l.cell_height)
            .map_or(self.levels.len(), |level| level + 1);
        self.near_in(r, &self.levels[..holding])
    }

    /// Every item listed in `levels` with a span in a cell `r` meets.
    fn near_in(&self, r: &Rect, levels: &[Level]) -> FixedBitSet {
        let s = Span::of(r);
        let mut found = FixedBitSet::with_capacity(self.len);
        for level in levels {
            for run in level.cells(&s, self.left, self.top) {
                let listed = self.starts[run.start] as usize..self.starts[run.end] as usize;
                for &item in &self.items[listed] {
                    found.insert(item as usize);
                }
            }
        }
        found
    }
}

/// `n` as a [`Grid`] stores it.
fn narrow(n: usize) -> u32 {
    u32::try_from(n).expect("a grid holds more entries than a u32 numbers")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paint::drawn_route;
    use bviz_graph_layout::Kind;
    use std::collections::HashSet;

    fn random_drawing(rng: &mut fastrand::Rng, boxes: usize, routes: usize) -> LaidOut {
        let coordinate = |rng: &mut fastrand::Rng| rng.f64() * 1000.0 - 100.0;
        let boxes: Vec<Box> = (0..boxes)
            .map(|i| {
                let (x, y) = (coordinate(rng), coordinate(rng));
                Box {
                    id: Id(i as u32),
                    label: String::new(),
                    kind: Kind::Target,
                    x,
                    y,
                    width: coordinate(rng).abs() / 4.0,
                    height: coordinate(rng).abs() / 8.0,
                    compound: false,
                    group: false,
                }
            })
            .collect();
        let ends = boxes.len().max(1) as u32;
        let connectors = (0..routes)
            .map(|_| {
                let legs = rng.usize(1..6);
                let (x, y) = (coordinate(rng), coordinate(rng));
                let reach = [10.0, 100.0, 1000.0][rng.usize(..3)];
                let mut step = || (rng.f64() - 0.5) * reach;
                let points = (0..=legs).map(|_| (x + step(), y + step())).collect();
                Connector {
                    points,
                    kinds: Vec::new(),
                    from: Id(rng.u32(..ends)),
                    to: Id(rng.u32(..ends)),
                    transition: None,
                }
            })
            .collect();
        LaidOut {
            boxes,
            connectors,
            ..LaidOut::default()
        }
    }

    /// Some of the first `boxes` boxes, as a frame's drawn boxes anchor routes.
    fn random_anchors(rng: &mut fastrand::Rng, boxes: usize) -> HashSet<Id> {
        (0..boxes as u32)
            .filter(|_| rng.u8(..10) == 0)
            .map(Id)
            .collect()
    }

    fn random_view(rng: &mut fastrand::Rng) -> Rect {
        Rect {
            x: rng.f64() * 1200.0 - 200.0,
            y: rng.f64() * 1200.0 - 200.0,
            w: rng.f64() * 400.0,
            h: rng.f64() * 400.0,
        }
    }

    #[test]
    fn boxes_near_a_rect_hold_every_box_over_it_in_drawing_order() {
        let mut rng = fastrand::Rng::with_seed(7);
        for size in [0, 1, 3, 50, 400] {
            let g = random_drawing(&mut rng, size, 0);
            let indexed = Indexed::of(&g);
            for _ in 0..200 {
                let view = random_view(&mut rng);
                let over: Vec<Id> = indexed
                    .boxes_near(&view, None)
                    .filter(|(_, rect, _)| view.overlaps(rect))
                    .map(|(b, ..)| b.id)
                    .collect();
                let scanned: Vec<Id> = g
                    .boxes
                    .iter()
                    .filter(|b| view.overlaps(&Rect::of(b)))
                    .map(|b| b.id)
                    .collect();
                assert_eq!(over, scanned);
            }
        }
    }

    #[test]
    fn boxes_near_a_point_hold_every_box_over_it_in_drawing_order() {
        let mut rng = fastrand::Rng::with_seed(5);
        let g = random_drawing(&mut rng, 400, 0);
        let indexed = Indexed::of(&g);
        for _ in 0..2000 {
            let (x, y) = (rng.f32() * 1200.0 - 200.0, rng.f32() * 1200.0 - 200.0);
            let at = Rect {
                x: x as f64,
                y: y as f64,
                w: 0.0,
                h: 0.0,
            };
            let over: Vec<Id> = indexed
                .boxes_near(&at, None)
                .filter(|(_, rect, _)| rect.covers(x, y))
                .map(|(b, ..)| b.id)
                .collect();
            let scanned: Vec<Id> = g
                .boxes
                .iter()
                .filter(|b| Rect::of(b).covers(x, y))
                .map(|b| b.id)
                .collect();
            assert_eq!(over, scanned);
        }
    }

    #[test]
    fn routes_near_a_rect_hold_every_route_drawn_over_it_in_drawing_order() {
        let mut rng = fastrand::Rng::with_seed(11);
        for size in [0, 1, 3, 50, 400] {
            let g = random_drawing(&mut rng, size, size * 2);
            let indexed = Indexed::of(&g);
            let position = |c: &Connector| g.connectors.iter().position(|o| std::ptr::eq(o, c));
            for _ in 0..200 {
                let view = random_view(&mut rng);
                let anchors = random_anchors(&mut rng, size);
                let drawn: Vec<_> = indexed
                    .routes_near(&view, anchors.iter().copied())
                    .filter(|c| drawn_route(c, &view, &anchors))
                    .map(position)
                    .collect();
                let scanned: Vec<_> = g
                    .connectors
                    .iter()
                    .filter(|c| drawn_route(c, &view, &anchors))
                    .map(position)
                    .collect();
                assert_eq!(drawn, scanned);
            }
        }
    }

    #[test]
    fn an_unbuilt_index_answers_every_query_as_the_built_one_does() {
        let mut rng = fastrand::Rng::with_seed(13);
        let g = random_drawing(&mut rng, 400, 800);
        let (built, unbuilt) = (Indexed::of(&g), Indexed::unbuilt(&g));
        let boxes_over = |indexed: &Indexed, view: &Rect| -> Vec<Id> {
            indexed
                .boxes_near(view, None)
                .filter(|(_, rect, _)| view.overlaps(rect))
                .map(|(b, ..)| b.id)
                .collect()
        };
        let routes_drawn = |indexed: &Indexed, view: &Rect, anchors: &HashSet<Id>| {
            indexed
                .routes_near(view, anchors.iter().copied())
                .filter(|c| drawn_route(c, view, anchors))
                .map(std::ptr::from_ref)
                .collect::<Vec<_>>()
        };
        for _ in 0..200 {
            let view = random_view(&mut rng);
            let anchors = random_anchors(&mut rng, g.boxes.len());
            assert_eq!(boxes_over(&built, &view), boxes_over(&unbuilt, &view));
            assert_eq!(
                routes_drawn(&built, &view, &anchors),
                routes_drawn(&unbuilt, &view, &anchors)
            );
        }
    }

    #[test]
    fn a_route_along_a_cell_border_is_near_a_view_touching_it() {
        let flat = |y: f64| Connector {
            points: vec![(0.0, y), (100.0, y)],
            kinds: Vec::new(),
            from: Id(0),
            to: Id(1),
            transition: None,
        };
        let g = LaidOut {
            connectors: (0..=10).map(|i| flat(i as f64 * 10.0)).collect(),
            ..LaidOut::default()
        };
        let indexed = Indexed::of(&g);
        let touching = Rect {
            x: 40.0,
            y: 50.0,
            w: 10.0,
            h: 0.0,
        };
        assert!(
            indexed
                .routes_near(&touching, [Id(0)])
                .any(|c| c.points[0].1 == 50.0)
        );
    }
}
