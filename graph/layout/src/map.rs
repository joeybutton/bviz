//! A zoom pyramid of views over one global placement.
//!
//! Every node is placed once, with the whole containment forest open, so a node
//! has a single absolute position that every level it appears in draws it at:
//! zooming reveals and hides boxes, and never moves one.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;

use bviz_parallel::worker_count;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

use crate::graph::{Containment, EdgeKind, Graph, Node};
use crate::intern::Id;
use crate::layout::{self, BORDER, Box, Connector, LABEL_BAND, LEAF_HEIGHT, LaidOut};
use crate::{lod, reduce};

/// A container opens once its smaller side covers this many screen pixels, the
/// point at which the boxes inside it are worth drawing. It sets how much of the
/// graph a level holds: the whole pyramid spans roughly the zoom range the
/// camera allows, so a larger value pushes every level's detail toward the
/// zoomed-in end and leaves the fitted view all but empty.
const OPEN_PX: f64 = 55.0;

/// Scale ratio between one level and the next, so a level spans a fixed zoom
/// factor whatever the graph's containers happen to measure.
const LEVEL_SCALE_STEP: f64 = 2.0;

/// How many edges one frame is laid out along at most.
const MAX_FRAME_EDGES: usize = 2000;

/// Places every node once and derives the zoom pyramid of views over it.
///
/// `base`'s node ids must be a dense permutation of `0..nodes.len()`, every
/// `parent` must name another of its nodes, and containment must be acyclic.
/// `reduced` must be the transitive reduction of `base`, whose nodes it shares.
pub fn map_layout(base: &Graph, reduced: &Graph) -> MapLayout {
    let containment = Containment::new(base);
    // The reduction reaches as far as the base, so it stacks the boxes the same
    // way for a fraction of the routing work.
    let drawing = {
        let fe = frame_edges(reduced, &containment);
        place(base, &fe, &containment)
    };
    let expandable = lod::expandable_containers(base);

    let view = |open: &HashSet<Id>| level(base, reduced, open, &drawing, &containment, &expandable);
    let mut open: HashSet<Id> = HashSet::new();
    let coarsest = view(&open);

    let scales = thresholds(&expandable, &drawing);
    let mut finer: Vec<(f64, LaidOut)> = Vec::with_capacity(scales.len());
    for scale in scales {
        let opened = open_at(&expandable, &drawing, scale);
        if opened != open {
            open = opened;
            finer.push((scale, view(&open)));
        }
    }
    MapLayout { coarsest, finer }
}

/// The views of one drawing at rising zoom, coarsest first.
pub struct MapLayout {
    coarsest: LaidOut,
    /// Ascending by the scale each view takes over from.
    finer: Vec<(f64, LaidOut)>,
}

impl MapLayout {
    /// The view to draw at `scale` screen pixels per layout unit.
    pub fn level_for_scale(&self, scale: f32) -> usize {
        self.finer
            .partition_point(|&(from, _)| from <= scale as f64)
    }

    pub fn level(&self, index: usize) -> &LaidOut {
        match index.checked_sub(1) {
            Some(finer) => &self.finer[finer].1,
            None => &self.coarsest,
        }
    }

    pub fn level_count(&self) -> usize {
        1 + self.finer.len()
    }

    pub fn width(&self) -> f64 {
        self.coarsest.width
    }

    pub fn height(&self) -> f64 {
        self.coarsest.height
    }
}

/// A node's box in the one absolute frame every level draws in.
#[derive(Clone, Copy)]
struct Placed {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl Placed {
    fn center(&self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    fn holds(&self, (x, y): (f64, f64)) -> bool {
        (self.x..self.x + self.width).contains(&x) && (self.y..self.y + self.height).contains(&y)
    }

    /// The scale (screen pixels per layout unit) from which this box is large
    /// enough on screen to show what it contains.
    fn open_scale(&self) -> f64 {
        OPEN_PX / self.width.min(self.height)
    }
}

/// The one placement the whole pyramid shares: a node drawn on several levels is
/// drawn at the same coordinates on each.
struct Drawing {
    placed: HashMap<Id, Placed>,
    width: f64,
    height: f64,
}

/// Lays out the whole containment forest with every container open: each frame
/// (a container, or the top frame holding the roots) places only its own
/// children, deepest frames first so a container is sized before the frame that
/// places it.
fn place(
    graph: &Graph,
    frames: &HashMap<Option<Id>, Vec<(Id, Id)>>,
    containment: &Containment,
) -> Drawing {
    let children_of = layout::children_by_parent(graph);
    let label_of: HashMap<Id, &str> = graph
        .nodes
        .iter()
        .map(|n| (n.id, n.label.as_str()))
        .collect();
    let no_edges: Vec<(Id, Id)> = Vec::new();
    let edges_of = |frame: Option<Id>| frames.get(&frame).unwrap_or(&no_edges).as_slice();

    let mut size = leaf_sizes(graph, &children_of);
    let mut offset: HashMap<Id, (f64, f64)> = HashMap::with_capacity(graph.nodes.len());
    for stratum in strata(&children_of, containment).iter_mut().rev() {
        // Frames of one stratum share no boxes, so they place in any order; the
        // sort only keeps that order the same from run to run.
        stratum.sort_unstable();
        let members = |c: &Id| -> Vec<Id> { children_of[c].iter().map(|n| n.id).collect() };
        let laid = in_parallel(stratum, |c| {
            lay_out_frame(&members(c), edges_of(Some(*c)), &size)
        });
        for (container, frame) in laid {
            fit_container(
                container,
                label_of[&container],
                frame,
                &mut size,
                &mut offset,
            );
        }
    }

    let mut roots: Vec<Id> = Vec::with_capacity(graph.nodes.len());
    roots.extend(
        graph
            .nodes
            .iter()
            .filter(|n| n.parent.is_none())
            .map(|n| n.id),
    );
    let top = on_worker(|| lay_out_frame(&roots, edges_of(None), &size));
    offset.extend(top.offsets);

    Drawing {
        placed: absolute(graph, containment, &size, &offset),
        width: top.width,
        height: top.height,
    }
}

/// Box sizes for the leaves, whose extent follows from their label alone and so
/// is known before any frame is laid out.
fn leaf_sizes(graph: &Graph, children_of: &HashMap<Id, Vec<&Node>>) -> HashMap<Id, (f64, f64)> {
    let mut size: HashMap<Id, (f64, f64)> = HashMap::with_capacity(graph.nodes.len());
    for node in &graph.nodes {
        if !children_of.contains_key(&node.id) {
            size.insert(node.id, (layout::leaf_width(&node.label), LEAF_HEIGHT));
        }
    }
    size
}

/// The containers at each containment depth, so a whole stratum can be laid out
/// once every frame under it has been.
fn strata(children_of: &HashMap<Id, Vec<&Node>>, containment: &Containment) -> Vec<Vec<Id>> {
    let deepest = children_of
        .keys()
        .map(|&c| containment.depth(c) as usize)
        .max();
    let mut strata: Vec<Vec<Id>> = vec![Vec::new(); deepest.map_or(0, |d| d + 1)];
    for &container in children_of.keys() {
        strata[containment.depth(container) as usize].push(container);
    }
    strata
}

/// Sizes `container` around the frame holding its children and settles where
/// each of them sits within it. The box holds its title even where that is wider
/// than the frame, which then sits centred in the width the title asks for.
fn fit_container(
    container: Id,
    label: &str,
    frame: Frame,
    size: &mut HashMap<Id, (f64, f64)>,
    offset: &mut HashMap<Id, (f64, f64)>,
) {
    let width = (frame.width + 2.0 * BORDER).max(layout::title_width(label));
    let inset = (width - frame.width) / 2.0;
    size.insert(container, (width, frame.height + LABEL_BAND + BORDER));
    offset.extend(
        frame
            .offsets
            .into_iter()
            .map(|(id, (x, y))| (id, (x + inset, y + LABEL_BAND))),
    );
}

/// Resolves every node's offset within its own frame against the frames holding
/// it, into the one absolute frame the pyramid draws in.
fn absolute(
    graph: &Graph,
    containment: &Containment,
    size: &HashMap<Id, (f64, f64)>,
    offset: &HashMap<Id, (f64, f64)>,
) -> HashMap<Id, Placed> {
    let mut by_depth: Vec<&Node> = graph.nodes.iter().collect();
    by_depth.sort_by_key(|n| containment.depth(n.id));
    let mut placed: HashMap<Id, Placed> = HashMap::with_capacity(graph.nodes.len());
    for node in by_depth {
        let (dx, dy) = offset[&node.id];
        let (x, y) = match node.parent {
            Some(parent) => {
                let outer = placed[&parent];
                (outer.x + dx, outer.y + dy)
            }
            None => (dx, dy),
        };
        let (width, height) = size[&node.id];
        placed.insert(
            node.id,
            Placed {
                x,
                y,
                width,
                height,
            },
        );
    }
    placed
}

/// One frame's own coordinate system: every offset is measured from its top-left
/// corner, and the extent is what its boxes span from there.
struct Frame {
    offsets: Vec<(Id, (f64, f64))>,
    width: f64,
    height: f64,
}

fn lay_out_frame(members: &[Id], edges: &[(Id, Id)], size: &HashMap<Id, (f64, f64)>) -> Frame {
    let boxes: Vec<(Id, f64, f64)> = members
        .iter()
        .map(|&id| {
            let (width, height) = size[&id];
            (id, width, height)
        })
        .collect();
    let (placements, width, height) = layout::layout_flat(&boxes, edges);
    Frame {
        offsets: placements.into_iter().map(|p| (p.id, (p.x, p.y))).collect(),
        width,
        height,
    }
}

/// Applies `job` to every id, spreading the work over the machine's cores and
/// returning each result beside the id it came from, in the order given.
fn in_parallel<T: Send>(ids: &[Id], job: impl Fn(&Id) -> T + Send + Sync) -> Vec<(Id, T)> {
    workers().install(|| ids.par_iter().map(|&id| (id, job(&id))).collect())
}

fn on_worker<T: Send>(job: impl FnOnce() -> T + Send) -> T {
    workers().install(job)
}

/// Stack for a layout worker: the engine recurses over the frame it is given, so
/// the depth follows the frame's size, and placing a whole graph hands it far
/// larger frames than laying out one view does. Sized well clear of what the
/// largest frames of a whole-repository build graph reach.
const WORKER_STACK: usize = 8 << 20;

/// The pool every frame is laid out on. Frame costs run orders of magnitude
/// apart, so the work is stolen rather than dealt out in equal shares, and the
/// pool outlives one stratum so the deepest strata of trivial frames cost no
/// thread of their own.
fn workers() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(worker_count())
            .stack_size(WORKER_STACK)
            .build()
            .expect("build the frame layout pool")
    })
}

/// The edges each frame lays out: an edge between two nodes becomes one edge
/// between the two children of their nearest common ancestor, keyed by that
/// ancestor (`None` for the top frame). Edges inside a single child carry no
/// information about how siblings sit, and are dropped.
fn frame_edges(graph: &Graph, containment: &Containment) -> HashMap<Option<Id>, Vec<(Id, Id)>> {
    let mut frames: HashMap<Option<Id>, BTreeSet<(Id, Id)>> =
        HashMap::with_capacity(graph.nodes.len());
    for edge in &graph.edges {
        if let Some((frame, from, to)) = containment.sibling_pair(edge.from, edge.to) {
            frames.entry(frame).or_default().insert((from, to));
        }
    }
    let cap = MAX_FRAME_EDGES;
    frames
        .into_iter()
        .map(|(frame, edges)| (frame, edges.into_iter().take(cap).collect()))
        .collect()
}

/// The scale from which each level past the coarsest takes over, rising by
/// [`LEVEL_SCALE_STEP`] from the first container to open through to the last.
fn thresholds(expandable: &HashSet<Id>, drawing: &Drawing) -> Vec<f64> {
    let (mut first, mut last) = (f64::INFINITY, 0.0f64);
    for container in expandable {
        let scale = drawing.placed[container].open_scale();
        first = first.min(scale);
        last = last.max(scale);
    }
    if !first.is_finite() {
        return Vec::new();
    }
    let steps = (last / first).log(LEVEL_SCALE_STEP).ceil() as usize + 1;
    let mut scales: Vec<f64> = Vec::with_capacity(steps);
    let mut scale = first;
    while scale < last {
        scales.push(scale);
        scale *= LEVEL_SCALE_STEP;
    }
    scales.push(last);
    scales
}

fn open_at(expandable: &HashSet<Id>, drawing: &Drawing, scale: f64) -> HashSet<Id> {
    expandable
        .iter()
        .copied()
        .filter(|c| drawing.placed[c].open_scale() <= scale)
        .collect()
}

/// The view of `base` with `open` opened, drawn at its nodes' global positions.
fn level(
    base: &Graph,
    reduced: &Graph,
    open: &HashSet<Id>,
    drawing: &Drawing,
    containment: &Containment,
    expandable: &HashSet<Id>,
) -> LaidOut {
    let visible = lod::collapse(base, open);
    let mut connected: HashSet<(Id, Id)> = HashSet::with_capacity(visible.edges.len());
    connected.extend(visible.edges.iter().map(|e| (e.from, e.to)));
    let boxes = boxes(&visible, drawing, containment);
    let drawn = drawn_edges(visible, reduced, open, expandable);
    LaidOut {
        boxes,
        connectors: { connectors(&drawn, drawing) },
        // The frames are laid out by layout_flat, which numbers no node.
        anchors: HashMap::new(),
        full_edges: connected.into_iter().collect(),
        width: drawing.width,
        height: drawing.height,
    }
}

/// The edges a level draws: those `visible` holds that the reduced base holds
/// between the same boxes. Folding a subtree into one box makes edges redundant
/// that are not redundant between nodes, so a view that folds anything is
/// reduced again.
fn drawn_edges(
    mut visible: Graph,
    reduced: &Graph,
    open: &HashSet<Id>,
    expandable: &HashSet<Id>,
) -> Graph {
    let kept = lod::collapse_edges(reduced, open);
    visible.edges.retain(|e| kept.contains(&(e.from, e.to)));
    match expandable.iter().all(|c| open.contains(c)) {
        true => visible,
        false => reduce::reduced(visible),
    }
}

/// Reads each visible node's global box.
fn boxes(visible: &Graph, drawing: &Drawing, containment: &Containment) -> Vec<Box> {
    let mut parents: HashSet<Id> = HashSet::with_capacity(visible.nodes.len());
    parents.extend(visible.nodes.iter().filter_map(|n| n.parent));
    let mut boxes: Vec<(u32, Box)> = Vec::with_capacity(visible.nodes.len());
    for node in &visible.nodes {
        let placed = drawing.placed[&node.id];
        boxes.push((
            containment.depth(node.id),
            Box {
                id: node.id,
                label: node.label.clone(),
                kind: node.kind,
                x: placed.x,
                y: placed.y,
                width: placed.width,
                height: placed.height,
                compound: parents.contains(&node.id),
                group: node.group,
            },
        ));
    }
    boxes.sort_by_key(|(depth, _)| *depth);
    boxes.into_iter().map(|(_, b)| b).collect()
}

/// One connector per drawn node pair.
fn connectors(drawn: &Graph, drawing: &Drawing) -> Vec<Connector> {
    let mut kinds_of: BTreeMap<(Id, Id), Vec<EdgeKind>> = BTreeMap::new();
    for edge in &drawn.edges {
        let kinds = kinds_of.entry((edge.from, edge.to)).or_default();
        if !kinds.contains(&edge.kind) {
            kinds.push(edge.kind);
        }
    }
    let mut connectors: Vec<Connector> = Vec::with_capacity(kinds_of.len());
    for ((from, to), mut kinds) in kinds_of {
        kinds.sort_unstable();
        let (tail, head) = (drawing.placed[&from], drawing.placed[&to]);
        connectors.push(Connector {
            points: route(&tail, &head),
            kinds,
            from,
            to,
        });
    }
    connectors
}

/// An axis-aligned route between two boxes. It leaves the middle of the side of
/// `from` that faces `to` and enters the middle of the facing side of `to`,
/// preferring the vertical sides so that the placement's top-to-bottom flow
/// draws as lanes. Boxes that overlap on both axes — one nested in the other —
/// have no side facing the other, and take a straight segment instead.
fn route(from: &Placed, to: &Placed) -> Vec<(f64, f64)> {
    let (fx, fy) = from.center();
    let (tx, ty) = to.center();
    if to.y > from.y + from.height {
        elbow((fx, from.y + from.height), (tx, to.y), Axis::Vertical)
    } else if from.y > to.y + to.height {
        elbow((fx, from.y), (tx, to.y + to.height), Axis::Vertical)
    } else if to.x > from.x + from.width {
        elbow((from.x + from.width, fy), (to.x, ty), Axis::Horizontal)
    } else if from.x > to.x + to.width {
        elbow((from.x, fy), (to.x + to.width, ty), Axis::Horizontal)
    } else {
        crossing(from, to)
    }
}

/// Which axis a route runs along, and so which one its jog crosses.
enum Axis {
    Vertical,
    Horizontal,
}

/// The route from `from` to `to` that runs along `axis` and takes the whole
/// offset across it in one jog, halfway along the gap the two leave. Two points
/// facing each other need no jog and take the direct leg.
fn elbow(from: (f64, f64), to: (f64, f64), axis: Axis) -> Vec<(f64, f64)> {
    match axis {
        Axis::Vertical if from.0 == to.0 => vec![from, to],
        Axis::Vertical => {
            let mid = (from.1 + to.1) / 2.0;
            vec![from, (from.0, mid), (to.0, mid), to]
        }
        Axis::Horizontal if from.1 == to.1 => vec![from, to],
        Axis::Horizontal => {
            let mid = (from.0 + to.0) / 2.0;
            vec![from, (mid, from.1), (mid, to.1), to]
        }
    }
}

/// The straight segment between two boxes neither of which has a side facing the
/// other: it sets out from `from`'s border and stops where it first meets `to`'s,
/// so it reaches the box it points at without running across it.
fn crossing(from: &Placed, to: &Placed) -> Vec<(f64, f64)> {
    let (x, y) = from.center();
    let (tx, ty) = to.center();
    // A box holding the other's center has no border between the two, so the
    // segment leaves by the side away from it and still has length to draw.
    let aim = match from.holds((tx, ty)) {
        true => (2.0 * x - tx, 2.0 * y - ty),
        false => (tx, ty),
    };
    let start = border_point(from, aim);
    vec![start, border_point(to, start)]
}

/// Where the ray from `b`'s center toward `aim` meets `b`'s border.
fn border_point(b: &Placed, aim: (f64, f64)) -> (f64, f64) {
    let (x, y) = b.center();
    let (dx, dy) = (aim.0 - x, aim.1 - y);
    // An aim on the center leaves no direction to run in, so the point takes the
    // flow the layout arranges every edge along.
    let (dx, dy) = if dx == 0.0 && dy == 0.0 {
        (0.0, 1.0)
    } else {
        (dx, dy)
    };
    let reach = (b.width / 2.0 / dx.abs()).min(b.height / 2.0 / dy.abs());
    (x + reach * dx, y + reach * dy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Kind;
    use crate::intern::Names;

    /// Three sibling containers of visibly different sizes: a chain of six
    /// long-labelled leaves, a chain of three, and a pair of short ones, plus a
    /// bare leaf root. Edges run within each container and between two of them.
    fn towers() -> (Graph, Names) {
        let mut g = Graph::new();
        let mut names = Names::default();
        let plan = [
            ("wide", 6, "a-long-leaf-label"),
            ("mid", 3, "leaf-label"),
            ("narrow", 2, "x"),
        ];
        for (tower, count, label) in plan {
            let root = names.intern(tower);
            g.add_node(root, tower, Kind::Package, None);
            let kids: Vec<Id> = (0..count)
                .map(|i| {
                    let id = names.intern(&format!("{tower}.{i}"));
                    g.add_node(id, label, Kind::Action, Some(root));
                    id
                })
                .collect();
            for pair in kids.windows(2) {
                g.add_edge(pair[0], pair[1], EdgeKind::Aquery);
            }
        }
        let lone = names.intern("lone");
        g.add_node(lone, "lone", Kind::Target, None);
        g.add_edge(
            names.get("wide.5").unwrap(),
            names.get("narrow.0").unwrap(),
            EdgeKind::Query,
        );
        (g, names)
    }

    fn mapped(base: &Graph) -> MapLayout {
        map_layout(base, &reduce::transitive_reduction(base))
    }

    fn built() -> (MapLayout, Names) {
        let (base, names) = towers();
        (mapped(&base), names)
    }

    fn box_of<'a>(view: &'a LaidOut, names: &Names, id: &str) -> Option<&'a Box> {
        let id = names.get(id)?;
        view.boxes.iter().find(|b| b.id == id)
    }

    fn drawn_pairs<'a>(view: &LaidOut, names: &'a Names) -> HashSet<(&'a str, &'a str)> {
        view.connectors
            .iter()
            .map(|c| (names.resolve(c.from), names.resolve(c.to)))
            .collect()
    }

    /// The first level drawing `child`, so the one at which its container opens.
    fn opens_at(map: &MapLayout, names: &Names, child: &str) -> usize {
        (0..map.level_count())
            .find(|&i| box_of(map.level(i), names, child).is_some())
            .unwrap()
    }

    #[test]
    fn a_node_keeps_one_position_across_every_level() {
        let (map, names) = built();
        for id in ["wide", "narrow", "lone", "wide.0"] {
            let placed: Vec<(u64, u64)> = (0..map.level_count())
                .filter_map(|i| box_of(map.level(i), &names, id))
                .map(|b| (b.x.to_bits(), b.y.to_bits()))
                .collect();
            assert!(!placed.is_empty(), "{id} is drawn on no level");
            assert!(
                placed.iter().all(|p| *p == placed[0]),
                "{id} moves between levels"
            );
        }
    }

    #[test]
    fn the_coarsest_level_draws_the_roots_alone() {
        let (map, names) = built();
        let coarsest = map.level(0);
        let drawn: HashSet<&str> = coarsest.boxes.iter().map(|b| names.resolve(b.id)).collect();
        assert_eq!(
            drawn,
            ["wide", "mid", "narrow", "lone"].into_iter().collect()
        );
        // Zoomed out far enough that nothing is legible, that is the view served.
        for scale in [0.0, 1e-9, 1e-4] {
            assert_eq!(map.level_for_scale(scale), 0);
        }
    }

    #[test]
    fn no_level_covers_more_than_a_zoom_step() {
        let (map, _) = built();
        for pair in map.finer.windows(2) {
            let (coarser, finer) = (pair[0].0, pair[1].0);
            assert!(finer > coarser, "{finer} does not follow {coarser}");
            assert!(
                finer <= coarser * LEVEL_SCALE_STEP,
                "{coarser} holds until {finer}, more than one zoom step",
            );
        }
    }

    #[test]
    fn zooming_in_only_adds_boxes() {
        let (map, names) = built();
        let drawn = |i: usize| -> HashSet<Id> { map.level(i).boxes.iter().map(|b| b.id).collect() };
        for i in 1..map.level_count() {
            assert!(
                drawn(i - 1).is_subset(&drawn(i)),
                "level {i} drops a box level {} drew",
                i - 1
            );
        }
        let finest = map.level(map.level_count() - 1);
        for leaf in ["wide.0", "wide.5", "mid.2", "narrow.1", "lone"] {
            assert!(
                box_of(finest, &names, leaf).is_some(),
                "{leaf} is missing at full zoom"
            );
        }
    }

    #[test]
    fn every_level_spans_the_same_extent() {
        let (map, _) = built();
        assert!(map.width() > 0.0 && map.height() > 0.0);
        for i in 0..map.level_count() {
            assert_eq!(map.level(i).width, map.width());
            assert_eq!(map.level(i).height, map.height());
        }
    }

    #[test]
    fn a_smaller_container_opens_later() {
        let (map, names) = built();
        let side = |id: &str| {
            let b = box_of(map.level(0), &names, id).unwrap();
            b.width.min(b.height)
        };
        assert!(side("wide") > side("narrow"));
        assert!(opens_at(&map, &names, "wide.0") < opens_at(&map, &names, "narrow.0"));
    }

    #[test]
    fn edges_follow_the_boxes_a_level_draws() {
        let (map, names) = built();

        // Closed, a tower's internal chain is a self-loop and nothing is drawn
        // between its leaves.
        let coarse = map.level(0);
        assert!(box_of(coarse, &names, "narrow.0").is_none());
        assert!(drawn_pairs(coarse, &names).iter().all(|(a, b)| a != b));
        assert!(drawn_pairs(coarse, &names).contains(&("wide", "narrow")));

        // Open, the same dependency runs between the leaves themselves.
        let finest = map.level(map.level_count() - 1);
        let pairs = drawn_pairs(finest, &names);
        assert!(pairs.contains(&("wide.5", "narrow.0")));
        assert!(pairs.contains(&("narrow.0", "narrow.1")));
    }

    #[test]
    fn every_connector_meets_the_boxes_it_joins() {
        let (map, _) = built();
        for i in 0..map.level_count() {
            let view = map.level(i);
            let boxes: HashMap<Id, &Box> = view.boxes.iter().map(|b| (b.id, b)).collect();
            for connector in &view.connectors {
                let (tail, head) = (boxes[&connector.from], boxes[&connector.to]);
                let on_border = |b: &Box, (x, y): (f64, f64)| {
                    let slack = 1e-6;
                    x >= b.x - slack
                        && x <= b.x + b.width + slack
                        && y >= b.y - slack
                        && y <= b.y + b.height + slack
                };
                assert!(on_border(tail, connector.points[0]));
                assert!(on_border(head, *connector.points.last().unwrap()));
            }
        }
    }

    #[test]
    fn level_for_scale_rises_with_scale_and_clamps() {
        let (map, _) = built();
        assert_eq!(map.level_for_scale(0.0), 0);
        assert_eq!(map.level_for_scale(-1.0), 0);
        assert_eq!(map.level_for_scale(1e9), map.level_count() - 1);

        let mut previous = 0;
        for step in 0..500 {
            let level = map.level_for_scale(step as f32 * 0.05);
            assert!(level >= previous);
            assert!(level < map.level_count());
            previous = level;
        }
    }

    /// A container with an edge straight to one of its own children, which the
    /// graph model allows and no frame lays out side by side.
    fn parent_to_child() -> (Graph, Names) {
        let mut g = Graph::new();
        let mut names = Names::default();
        let outer = names.intern("outer");
        g.add_node(outer, "outer", Kind::Package, None);
        for kid in ["a", "b"] {
            let id = names.intern(kid);
            g.add_node(id, kid, Kind::Action, Some(outer));
        }
        let other = names.intern("other");
        g.add_node(other, "other", Kind::Target, None);
        g.add_edge(outer, names.get("a").unwrap(), EdgeKind::Aquery);
        g.add_edge(names.get("b").unwrap(), other, EdgeKind::Aquery);
        (g, names)
    }

    /// A container whose one child leaves it no room of its own, so the
    /// container's center falls inside the child and each box holds the other's.
    fn parent_to_only_child() -> (Graph, Names) {
        let mut g = Graph::new();
        let mut names = Names::default();
        let outer = names.intern("outer");
        g.add_node(outer, "outer", Kind::Package, None);
        let only = names.intern("only");
        g.add_node(only, "only", Kind::Action, Some(outer));
        let other = names.intern("other");
        g.add_node(other, "other", Kind::Target, None);
        g.add_edge(outer, only, EdgeKind::Aquery);
        g.add_edge(only, other, EdgeKind::Aquery);
        (g, names)
    }

    /// Asserts the connector drawn from `outer` to the box `inner` nested in it
    /// keeps clear of `inner`, meeting it at the border instead of running
    /// across it.
    fn assert_stops_at_the_nested_border(base: &Graph, names: &Names, inner: &str) {
        let map = mapped(base);
        let (outer, id) = (names.get("outer").unwrap(), names.get(inner).unwrap());
        let view = (0..map.level_count())
            .map(|i| map.level(i))
            .find(|v| v.boxes.iter().any(|b| b.id == id))
            .unwrap();
        let inner = view.boxes.iter().find(|b| b.id == id).unwrap();
        let connector = view
            .connectors
            .iter()
            .find(|c| c.from == outer && c.to == id)
            .unwrap();

        let strictly_inside = |(x, y): (f64, f64)| {
            let slack = 1e-6;
            x > inner.x + slack
                && x < inner.x + inner.width - slack
                && y > inner.y + slack
                && y < inner.y + inner.height - slack
        };
        for leg in connector.points.windows(2) {
            let (start, end) = (leg[0], leg[1]);
            for step in 0..=100 {
                let t = step as f64 / 100.0;
                let at = (
                    start.0 + t * (end.0 - start.0),
                    start.1 + t * (end.1 - start.1),
                );
                assert!(
                    !strictly_inside(at),
                    "the connector crosses the box it points at"
                );
            }
        }
    }

    #[test]
    fn a_connector_into_a_nested_box_stops_at_its_border() {
        let (base, names) = parent_to_child();
        assert_stops_at_the_nested_border(&base, &names, "a");
        // A lone child fills its container, so the two centers sit inside each
        // other's box and the segment has to pick its ends from that.
        let (base, names) = parent_to_only_child();
        assert_stops_at_the_nested_border(&base, &names, "only");
    }

    /// Each leg of a route runs along one axis, so the whole route is
    /// rectilinear.
    fn axis_aligned(points: &[(f64, f64)]) -> bool {
        points
            .windows(2)
            .all(|leg| leg[0].0 == leg[1].0 || leg[0].1 == leg[1].1)
    }

    #[test]
    fn a_route_leaves_and_enters_the_facing_borders_along_the_axes() {
        let above = Placed {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 20.0,
        };
        let below = Placed {
            x: 300.0,
            y: 200.0,
            width: 60.0,
            height: 40.0,
        };
        let beside = Placed {
            x: 400.0,
            y: 0.0,
            width: 40.0,
            height: 60.0,
        };

        let down = route(&above, &below);
        assert!(axis_aligned(&down), "{down:?}");
        assert_eq!(down[0], (50.0, 20.0), "leaves the source's bottom border");
        assert_eq!(
            *down.last().unwrap(),
            (330.0, 200.0),
            "enters the target's top border"
        );
        assert!(
            down[1..3].iter().all(|p| p.1 == 110.0),
            "jogs across between the two: {down:?}"
        );

        let up = route(&below, &above);
        assert!(axis_aligned(&up), "{up:?}");
        assert_eq!(up[0], (330.0, 200.0), "leaves the source's top border");
        assert_eq!(
            *up.last().unwrap(),
            (50.0, 20.0),
            "enters the target's bottom border"
        );

        // Overlapping rows leave no vertical gap, so the route runs sideways.
        let across = route(&above, &beside);
        assert!(axis_aligned(&across), "{across:?}");
        assert_eq!(across[0], (100.0, 10.0), "leaves the source's right border");
        assert_eq!(
            *across.last().unwrap(),
            (400.0, 30.0),
            "enters the target's left border"
        );
    }

    #[test]
    fn boxes_facing_each_other_head_on_take_one_leg() {
        let above = Placed {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 20.0,
        };
        let below = Placed {
            x: 20.0,
            y: 200.0,
            width: 60.0,
            height: 40.0,
        };
        assert_eq!(route(&above, &below), vec![(50.0, 20.0), (50.0, 200.0)]);
    }

    #[test]
    fn boxes_sharing_a_center_still_get_a_segment() {
        let square = Placed {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        let inside = Placed {
            x: 40.0,
            y: 40.0,
            width: 20.0,
            height: 20.0,
        };
        let points = route(&square, &inside);
        assert!(
            points.iter().all(|p| p.0.is_finite() && p.1.is_finite()),
            "{points:?}"
        );
        assert_ne!(points[0], *points.last().unwrap());
    }

    /// A frame holding a tangle is laid out along a subset of it. The edges of a
    /// frame settle only the order its boxes take, and the layout engine spends
    /// a dummy node on every layer each one spans, so an uncapped tangle costs
    /// more than the ordering it buys is worth.
    #[test]
    fn a_frame_denser_than_the_cap_is_laid_out_along_part_of_it() {
        let mut g = Graph::new();
        let mut names = Names::default();
        let root = names.intern("root");
        g.add_node(root, "root", Kind::Package, None);
        let members: Vec<Id> = (0..80)
            .map(|i| {
                let id = names.intern(&format!("m{i}"));
                g.add_node(id, "m", Kind::Action, Some(root));
                id
            })
            .collect();
        for (i, &from) in members.iter().enumerate() {
            for &to in &members[i + 1..] {
                g.add_edge(from, to, EdgeKind::Aquery);
            }
        }
        assert!(
            g.edges.len() > MAX_FRAME_EDGES,
            "the fixture holds {} edges, within the cap",
            g.edges.len(),
        );

        let frames = frame_edges(&g, &Containment::new(&g));
        assert_eq!(frames[&Some(root)].len(), MAX_FRAME_EDGES);
    }

    /// A frame within the cap keeps every edge it has, so the cap costs the
    /// ordering of nothing it does not have to.
    #[test]
    fn a_frame_within_the_cap_keeps_every_edge() {
        let (base, names) = towers();
        let reduced = reduce::transitive_reduction(&base);
        let containment = Containment::new(&reduced);
        let frames = frame_edges(&reduced, &containment);
        let wide = names.get("wide").unwrap();

        let held: usize = frames.values().map(Vec::len).sum();
        let sided = reduced
            .edges
            .iter()
            .filter(|e| containment.sibling_pair(e.from, e.to).is_some())
            .count();
        assert!(held > 0 && held <= sided);
        assert!(frames[&Some(wide)].len() < MAX_FRAME_EDGES);
    }

    #[test]
    fn a_child_carrying_a_lower_id_than_its_parent_lays_out() {
        // Nothing lines a node's id up with its parent's or with its position in
        // the graph: the first child is interned and added before the container.
        let mut g = Graph::new();
        let mut names = Names::default();
        let first = names.intern("first");
        let root = names.intern("root");
        let second = names.intern("second");
        g.add_node(first, "first", Kind::Action, Some(root));
        g.add_node(root, "root", Kind::Package, None);
        g.add_node(second, "second", Kind::Action, Some(root));
        g.add_edge(first, second, EdgeKind::Aquery);

        let finest = mapped(&g);
        let finest = finest.level(finest.level_count() - 1);
        assert_eq!(finest.boxes.len(), 3);
        let outer = finest.boxes.iter().find(|b| b.id == root).unwrap();
        for kid in [first, second] {
            let inner = finest.boxes.iter().find(|b| b.id == kid).unwrap();
            assert!(
                inner.x >= outer.x && inner.y >= outer.y,
                "a child sits outside its container"
            );
        }
    }
}
