//! A zoom pyramid of views over one global placement.
//!
//! Every node is placed once, with the whole containment forest open, so a node
//! has a single absolute position that every level it appears in draws it at:
//! zooming reveals and hides boxes, and never moves one.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;

use bviz_parallel::worker_count;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

use crate::graph::{EdgeKind, Graph, Node};
use crate::intern::Id;
use crate::layout::{self, Box, Connector, LaidOut, BORDER, LABEL_BAND, LEAF_HEIGHT};
use crate::{lod, reduce};

/// A container opens once its smaller side covers this many screen pixels, the
/// point at which the boxes inside it are worth drawing.
const OPEN_PX: f64 = 220.0;

/// Scale ratio between one level and the next, so a level spans a fixed zoom
/// factor whatever the graph's containers happen to measure.
const LEVEL_SCALE_STEP: f64 = 2.0;

/// Places every node once and derives the zoom pyramid of views over it.
///
/// `base`'s node ids must be a dense permutation of `0..nodes.len()`, every
/// `parent` must name another of its nodes, and containment must be acyclic.
/// `reduced` must be the transitive reduction of `base`, whose nodes it shares.
pub fn map_layout(base: &Graph, reduced: &Graph) -> MapLayout {
    let containment = Containment::new(base);
    // The reduction reaches as far as the base, so it stacks the boxes the same
    // way for a fraction of the routing work.
    let drawing = place(base, &frame_edges(reduced, &containment), &containment);
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
        self.finer.partition_point(|&(from, _)| from <= scale as f64)
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
    let label_of: HashMap<Id, &str> =
        graph.nodes.iter().map(|n| (n.id, n.label.as_str())).collect();
    let no_edges: Vec<(Id, Id)> = Vec::new();
    let edges_of = |frame: Option<Id>| frames.get(&frame).unwrap_or(&no_edges).as_slice();

    let mut size = leaf_sizes(graph, &children_of);
    let mut offset: HashMap<Id, (f64, f64)> = HashMap::with_capacity(graph.nodes.len());
    for stratum in strata(&children_of, containment).iter_mut().rev() {
        // Frames of one stratum share no boxes, so they place in any order; the
        // sort only keeps that order the same from run to run.
        stratum.sort_unstable();
        let members = |c: &Id| -> Vec<Id> { children_of[c].iter().map(|n| n.id).collect() };
        let laid =
            in_parallel(stratum, |c| lay_out_frame(&members(c), edges_of(Some(*c)), &size));
        for (container, frame) in laid {
            fit_container(container, label_of[&container], frame, &mut size, &mut offset);
        }
    }

    let mut roots: Vec<Id> = Vec::with_capacity(graph.nodes.len());
    roots.extend(graph.nodes.iter().filter(|n| n.parent.is_none()).map(|n| n.id));
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
    let deepest = children_of.keys().map(|&c| containment.depth(c) as usize).max();
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
    offset
        .extend(frame.offsets.into_iter().map(|(id, (x, y))| (id, (x + inset, y + LABEL_BAND))));
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
        placed.insert(node.id, Placed { x, y, width, height });
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
fn frame_edges(
    graph: &Graph,
    containment: &Containment,
) -> HashMap<Option<Id>, Vec<(Id, Id)>> {
    let mut frames: HashMap<Option<Id>, BTreeSet<(Id, Id)>> =
        HashMap::with_capacity(graph.nodes.len());
    for edge in &graph.edges {
        if let Some((frame, from, to)) = containment.sibling_pair(edge.from, edge.to) {
            frames.entry(frame).or_default().insert((from, to));
        }
    }
    frames.into_iter().map(|(frame, edges)| (frame, edges.into_iter().collect())).collect()
}

/// A graph's containment forest, indexed by node id.
struct Containment {
    parent: Vec<Option<Id>>,
    depth: Vec<u32>,
}

impl Containment {
    fn new(graph: &Graph) -> Self {
        let mentioned = graph.nodes.iter().flat_map(|n| [Some(n.id), n.parent]).flatten();
        let len = mentioned.map(|id| id.0 as usize + 1).max().unwrap_or(0);
        let mut parent: Vec<Option<Id>> = vec![None; len];
        for node in &graph.nodes {
            parent[node.id.0 as usize] = node.parent;
        }
        let mut depth: Vec<u32> = vec![0; len];
        for node in &graph.nodes {
            let mut steps = 0;
            let mut id = node.id;
            while let Some(up) = parent[id.0 as usize] {
                steps += 1;
                id = up;
            }
            depth[node.id.0 as usize] = steps;
        }
        Self { parent, depth }
    }

    fn parent(&self, id: Id) -> Option<Id> {
        self.parent[id.0 as usize]
    }

    /// `id`'s containment parent, or `id` itself where it is a root.
    fn up(&self, id: Id) -> Id {
        self.parent(id).unwrap_or(id)
    }

    fn depth(&self, id: Id) -> u32 {
        self.depth[id.0 as usize]
    }

    /// The frame that places `from` and `to` side by side — their nearest common
    /// ancestor, or `None` for the top frame when they are in different trees —
    /// and the child of it each of them descends from. `None` overall when one
    /// of the two contains the other, which no frame places side by side.
    fn sibling_pair(&self, from: Id, to: Id) -> Option<(Option<Id>, Id, Id)> {
        let (mut a, mut b) = (from, to);
        while self.depth(a) > self.depth(b) {
            a = self.up(a);
        }
        while self.depth(b) > self.depth(a) {
            b = self.up(b);
        }
        if a == b {
            return None;
        }
        // Both stand at one depth from here, so they reach the roots together
        // and meet at latest where both parents are `None`.
        while self.parent(a) != self.parent(b) {
            a = self.up(a);
            b = self.up(b);
        }
        Some((self.parent(a), a, b))
    }
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
        connectors: connectors(&drawn, drawing),
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

/// One straight connector per drawn node pair.
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
            points: vec![border_point(&tail, &head), border_point(&head, &tail)],
            kinds,
            from,
            to,
        });
    }
    connectors
}

/// Where the line joining the two boxes' centers meets `from`'s border on the
/// side the drawn segment runs from.
fn border_point(from: &Placed, to: &Placed) -> (f64, f64) {
    let (x, y) = from.center();
    let (tx, ty) = to.center();
    let (dx, dy) = (tx - x, ty - y);
    let (dx, dy) = if dx == 0.0 && dy == 0.0 {
        // Boxes sharing a center leave no direction to run in, so the segment
        // takes the flow the layout arranges every edge along.
        (0.0, 1.0)
    } else if from.holds((tx, ty)) {
        // A box holding the other's center has no border between the two, so the
        // segment sets out from the far side and stops where it reaches the box
        // it points at.
        (-dx, -dy)
    } else {
        (dx, dy)
    };
    let reach = (from.width / 2.0 / dx.abs()).min(from.height / 2.0 / dy.abs());
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
        let plan = [("wide", 6, "a-long-leaf-label"), ("mid", 3, "leaf-label"), ("narrow", 2, "x")];
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
        g.add_edge(names.get("wide.5").unwrap(), names.get("narrow.0").unwrap(), EdgeKind::Query);
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
        view.connectors.iter().map(|c| (names.resolve(c.from), names.resolve(c.to))).collect()
    }

    /// The first level drawing `child`, so the one at which its container opens.
    fn opens_at(map: &MapLayout, names: &Names, child: &str) -> usize {
        (0..map.level_count()).find(|&i| box_of(map.level(i), names, child).is_some()).unwrap()
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
            assert!(placed.iter().all(|p| *p == placed[0]), "{id} moves between levels");
        }
    }

    #[test]
    fn the_coarsest_level_draws_the_roots_alone() {
        let (map, names) = built();
        let coarsest = map.level(0);
        let drawn: HashSet<&str> = coarsest.boxes.iter().map(|b| names.resolve(b.id)).collect();
        assert_eq!(drawn, ["wide", "mid", "narrow", "lone"].into_iter().collect());
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
            assert!(drawn(i - 1).is_subset(&drawn(i)), "level {i} drops a box level {} drew", i - 1);
        }
        let finest = map.level(map.level_count() - 1);
        for leaf in ["wide.0", "wide.5", "mid.2", "narrow.1", "lone"] {
            assert!(box_of(finest, &names, leaf).is_some(), "{leaf} is missing at full zoom");
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
                assert_eq!(connector.points.len(), 2);
                assert!(on_border(tail, connector.points[0]));
                assert!(on_border(head, connector.points[1]));
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

    #[test]
    fn a_connector_into_a_nested_box_stops_at_its_border() {
        let (base, names) = parent_to_child();
        let map = mapped(&base);
        let (outer, a) = (names.get("outer").unwrap(), names.get("a").unwrap());
        let view = (0..map.level_count())
            .map(|i| map.level(i))
            .find(|v| v.boxes.iter().any(|b| b.id == a))
            .unwrap();
        let inner = view.boxes.iter().find(|b| b.id == a).unwrap();
        let connector =
            view.connectors.iter().find(|c| c.from == outer && c.to == a).unwrap();

        let (start, end) = (connector.points[0], connector.points[1]);
        let strictly_inside = |(x, y): (f64, f64)| {
            let slack = 1e-6;
            x > inner.x + slack
                && x < inner.x + inner.width - slack
                && y > inner.y + slack
                && y < inner.y + inner.height - slack
        };
        assert!(!strictly_inside(start), "the connector starts inside the box it points at");
        // Every point up to the border is clear of the nested box, so the
        // segment stops at it instead of running across it.
        for step in 0..100 {
            let t = step as f64 / 100.0;
            let at = (start.0 + t * (end.0 - start.0), start.1 + t * (end.1 - start.1));
            assert!(!strictly_inside(at), "the connector crosses the box it points at");
        }
    }

    #[test]
    fn boxes_sharing_a_center_still_get_a_segment() {
        let square = Placed { x: 0.0, y: 0.0, width: 100.0, height: 100.0 };
        let inside = Placed { x: 40.0, y: 40.0, width: 20.0, height: 20.0 };
        let (start, end) = (border_point(&square, &inside), border_point(&inside, &square));
        assert!(start.0.is_finite() && start.1.is_finite());
        assert!(end.0.is_finite() && end.1.is_finite());
        assert_ne!(start, end);
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
            assert!(inner.x >= outer.x && inner.y >= outer.y, "a child sits outside its container");
        }
    }
}
