//! A zoom pyramid of views over one global placement.
//!
//! Every node is placed once, with the whole containment forest open, so a node
//! has a single absolute position that every level it appears in draws it at:
//! zooming reveals and hides boxes, and never moves one.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use bviz_parallel::workers;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

use crate::graph::{Containment, EdgeKind, Graph, Node, id_span};
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
/// Every `parent` of `base` must name another of its nodes and containment must
/// be acyclic; its ids are otherwise free.
/// `reduced` must be the transitive reduction of `base`, whose nodes it shares.
pub fn map_layout(base: Arc<Graph>, reduced: Arc<Graph>) -> MapLayout {
    let containment = Containment::new(&base);
    // The reduction reaches as far as the base, so it stacks the boxes the same
    // way for a fraction of the routing work.
    let drawing = place(&base, &frame_edges(&reduced, &containment), &containment);
    let expandable = lod::expandable_containers(&base);

    // Which containers each level opens, settled here because a level takes
    // over from the one below it: a threshold that opens nothing further is not
    // a level of its own.
    let mut open: HashSet<Id> = HashSet::new();
    let mut finer: Vec<(f64, Level)> = Vec::new();
    for from in thresholds(&expandable, &drawing) {
        let opened = open_at(&expandable, &drawing, from);
        if opened != open {
            open = opened;
            finer.push((from, Level::opening(Arc::new(open.clone()))));
        }
    }
    MapLayout {
        base,
        reduced,
        containment: Arc::new(containment),
        drawing: Arc::new(drawing),
        expandable: Arc::new(expandable),
        coarsest: Level::opening(Arc::new(HashSet::new())),
        finer,
    }
}

/// One view of the drawing, derived when a camera first calls for it.
///
/// Deriving one costs a fold of the whole graph and a reduction of what that
/// leaves, and a camera reads exactly one of them, so a pyramid built in full is
/// mostly work for a zoom the reader never reaches. What a level needs to derive
/// itself is held by the [`MapLayout`] around it.
struct Level {
    open: Arc<HashSet<Id>>,
    view: OnceLock<LaidOut>,
}

impl Level {
    /// The level opening `open`, with nothing derived yet.
    fn opening(open: Arc<HashSet<Id>>) -> Self {
        Level {
            open,
            view: OnceLock::new(),
        }
    }
}

/// The views of one drawing at rising zoom, coarsest first.
///
/// The placement, the containment forest it was read from and the scales the
/// levels take over at are shared with every [`restricted`](Self::restricted)
/// map derived from this one, so a restriction draws its boxes exactly where
/// this map draws them.
pub struct MapLayout {
    base: Arc<Graph>,
    reduced: Arc<Graph>,
    containment: Arc<Containment>,
    drawing: Arc<Drawing>,
    expandable: Arc<HashSet<Id>>,
    coarsest: Level,
    /// Ascending by the scale each view takes over from.
    finer: Vec<(f64, Level)>,
}

impl MapLayout {
    /// The view to draw at `scale` screen pixels per layout unit.
    pub fn level_for_scale(&self, scale: f32) -> usize {
        self.finer
            .partition_point(|&(from, _)| from <= scale as f64)
    }

    /// The view at `index`, deriving it if this is the first call for it.
    pub fn level(&self, index: usize) -> &LaidOut {
        let level = match index.checked_sub(1) {
            Some(finer) => &self.finer[finer].1,
            None => &self.coarsest,
        };
        level.view.get_or_init(|| {
            view_of(
                &self.base,
                &self.reduced,
                &level.open,
                &self.drawing,
                &self.containment,
                &self.expandable,
            )
        })
    }

    pub fn level_count(&self) -> usize {
        1 + self.finer.len()
    }

    /// The levels `base` leaves of this map, each drawn at the positions this
    /// map places its nodes at: a node this map places and `base` does not hold
    /// leaves a hole where it stood, and everything else stays put.
    ///
    /// `base`'s nodes must be a subset of the placed graph's, keyed by the same
    /// ids; `reduced` must be its transitive reduction. Which containers a level
    /// opens is the placement's to say, so it is carried over unchanged and a
    /// container `base` drops simply goes undrawn.
    pub fn restricted(&self, base: Arc<Graph>, reduced: Arc<Graph>) -> MapLayout {
        MapLayout {
            base,
            reduced,
            containment: Arc::clone(&self.containment),
            drawing: Arc::clone(&self.drawing),
            expandable: Arc::clone(&self.expandable),
            coarsest: Level::opening(Arc::clone(&self.coarsest.open)),
            finer: self
                .finer
                .iter()
                .map(|(from, level)| (*from, Level::opening(Arc::clone(&level.open))))
                .collect(),
        }
    }

    /// The extent every level spans, which follows from the placement they share
    /// and so needs none of them derived.
    pub fn width(&self) -> f64 {
        self.drawing.width
    }

    pub fn height(&self) -> f64 {
        self.drawing.height
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
    /// Each id's box, indexed by id over the placed graph's id space, `None`
    /// where that graph holds no node of the id.
    placed: Vec<Option<Placed>>,
    width: f64,
    height: f64,
}

impl Drawing {
    fn placed(&self, id: Id) -> Option<Placed> {
        self.placed.get(id.0 as usize).copied().flatten()
    }
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
) -> Vec<Option<Placed>> {
    let mut by_depth: Vec<&Node> = graph.nodes.iter().collect();
    by_depth.sort_by_key(|n| containment.depth(n.id));
    let mut placed: Vec<Option<Placed>> = vec![None; id_span(graph)];
    for node in by_depth {
        let (dx, dy) = offset[&node.id];
        // Containment is acyclic and the walk runs shallowest first, so a
        // container is placed before anything it holds.
        let (x, y) = match node.parent.and_then(|p| placed[p.0 as usize]) {
            Some(outer) => (outer.x + dx, outer.y + dy),
            None => (dx, dy),
        };
        let (width, height) = size[&node.id];
        placed[node.id.0 as usize] = Some(Placed {
            x,
            y,
            width,
            height,
        });
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
/// returning each result beside the id it came from, in the order given. Frame
/// costs run orders of magnitude apart, so the ids are stolen rather than dealt
/// out in equal shares.
fn in_parallel<T: Send>(ids: &[Id], job: impl Fn(&Id) -> T + Send + Sync) -> Vec<(Id, T)> {
    workers().install(|| ids.par_iter().map(|&id| (id, job(&id))).collect())
}

fn on_worker<T: Send>(job: impl FnOnce() -> T + Send) -> T {
    workers().install(job)
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
    for scale in expandable
        .iter()
        .filter_map(|&c| Some(drawing.placed(c)?.open_scale()))
    {
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
        .filter(|&c| drawing.placed(c).is_some_and(|p| p.open_scale() <= scale))
        .collect()
}

/// The view of `base` with `open` opened, drawn at its nodes' global positions.
fn view_of(
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
        let Some(placed) = drawing.placed(node.id) else {
            continue;
        };
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
        let (Some(tail), Some(head)) = (drawing.placed(from), drawing.placed(to)) else {
            continue;
        };
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
        towers_spaced(1)
    }

    /// [`towers`] over an id space `spacing` times as wide, so its nodes carry
    /// the ids they would after a restriction dropped the rest.
    fn towers_spaced(spacing: usize) -> (Graph, Names) {
        /// Interns `name` after burning `spacing - 1` ids on nothing, so a
        /// fixture built with `spacing > 1` leaves holes in its id space.
        fn spaced(names: &mut Names, spacing: usize, name: &str) -> Id {
            for i in 1..spacing {
                names.intern(&format!("{name}/hole{i}"));
            }
            names.intern(name)
        }

        let mut g = Graph::new();
        let mut names = Names::default();
        let plan = [
            ("wide", 6, "a-long-leaf-label"),
            ("mid", 3, "leaf-label"),
            ("narrow", 2, "x"),
        ];
        for (tower, count, label) in plan {
            let root = spaced(&mut names, spacing, tower);
            g.add_node(Node::new(root, tower, Kind::Package, None));
            let kids: Vec<Id> = (0..count)
                .map(|i| {
                    let id = spaced(&mut names, spacing, &format!("{tower}.{i}"));
                    g.add_node(Node::new(id, label, Kind::Action, Some(root)));
                    id
                })
                .collect();
            for pair in kids.windows(2) {
                g.add_edge(pair[0], pair[1], EdgeKind::Aquery);
            }
        }
        let lone = spaced(&mut names, spacing, "lone");
        g.add_node(Node::new(lone, "lone", Kind::Target, None));
        g.add_edge(
            names.get("wide.5").unwrap(),
            names.get("narrow.0").unwrap(),
            EdgeKind::Query,
        );
        (g, names)
    }

    fn mapped(base: &Graph) -> MapLayout {
        let reduced = reduce::transitive_reduction(base);
        map_layout(Arc::new(base.clone()), Arc::new(reduced))
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

    /// A view of every level a camera might reach costs a fold of the whole
    /// graph apiece, and the extent follows from the placement they share, so
    /// framing the map derives none of them.
    #[test]
    fn framing_the_map_derives_no_view() {
        let (map, _) = built();
        assert!(map.width() > 0.0 && map.height() > 0.0);
        assert!(map.level_count() > 1, "the fixture holds one level only");
        assert!(
            std::iter::once(&map.coarsest)
                .chain(map.finer.iter().map(|(_, level)| level))
                .all(|level| level.view.get().is_none()),
            "a view was derived before a camera called for one",
        );
    }

    /// A level derives its view once and keeps it, so a camera resting on one
    /// costs the fold only the first time it draws.
    #[test]
    fn a_level_derives_its_view_once() {
        let (map, _) = built();
        let first: *const LaidOut = map.level(1);
        assert!(map.finer[0].1.view.get().is_some());
        assert!(
            std::ptr::eq(first, map.level(1)),
            "the level derived its view a second time",
        );
        assert!(
            map.coarsest.view.get().is_none(),
            "drawing one level derived another",
        );
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

    /// A graph's ids need not run without gaps, and its largest may sit well
    /// past its node count, so the placement lands the same drawing either way.
    #[test]
    fn a_gapped_id_space_places_the_same_drawing() {
        let (dense, dense_names) = towers_spaced(1);
        let (gapped, gapped_names) = towers_spaced(3);
        assert!(
            gapped
                .nodes
                .iter()
                .any(|n| n.id.0 as usize >= gapped.nodes.len()),
            "the fixture's ids stay within its node count",
        );

        let (a, b) = (mapped(&dense), mapped(&gapped));
        assert_eq!(a.level_count(), b.level_count());
        assert_eq!((a.width(), a.height()), (b.width(), b.height()));
        let named = |view: &LaidOut, names: &Names| -> BTreeSet<(String, u64, u64)> {
            view.boxes
                .iter()
                .map(|b| {
                    (
                        names.resolve(b.id).to_string(),
                        b.x.to_bits(),
                        b.y.to_bits(),
                    )
                })
                .collect()
        };
        for i in 0..a.level_count() {
            assert_eq!(
                named(a.level(i), &dense_names),
                named(b.level(i), &gapped_names),
                "level {i} differs",
            );
        }
    }

    /// A restriction is drawn into the holes its dropped nodes leave: what
    /// survives is drawn where the whole graph draws it, at every level, and the
    /// extent the camera pans over is the one the placement fixed.
    #[test]
    fn a_restriction_draws_what_survives_where_the_whole_graph_draws_it() {
        let (base, names) = towers();
        let map = mapped(&base);
        let dropped: HashSet<Id> = ["mid", "mid.0", "mid.1", "mid.2", "wide.2"]
            .into_iter()
            .map(|id| names.get(id).unwrap())
            .collect();
        let keep: HashSet<Id> = base
            .nodes
            .iter()
            .map(|node| node.id)
            .filter(|id| !dropped.contains(id))
            .collect();

        let restricted = crate::filter::filter(&base, &map.reduced, &keep);
        let reduced = reduce::transitive_reduction(&restricted);
        let sliced = map.restricted(Arc::new(restricted), Arc::new(reduced));

        assert_eq!(sliced.level_count(), map.level_count());
        assert_eq!(
            (sliced.width(), sliced.height()),
            (map.width(), map.height())
        );
        for i in 0..map.level_count() {
            let placed = |view: &LaidOut| -> BTreeMap<Id, (u64, u64)> {
                view.boxes
                    .iter()
                    .map(|b| (b.id, (b.x.to_bits(), b.y.to_bits())))
                    .collect()
            };
            let (whole, slice) = (placed(map.level(i)), placed(sliced.level(i)));
            assert!(
                slice.keys().all(|id| keep.contains(id)),
                "level {i} draws a dropped box",
            );
            for (id, at) in &slice {
                assert_eq!(whole.get(id), Some(at), "level {i} moved a kept box");
            }
        }
        let finest = sliced.level(sliced.level_count() - 1);
        assert!(
            box_of(finest, &names, "wide.5").is_some() && box_of(finest, &names, "mid.0").is_none(),
            "the fixture's restriction drops nothing the finest level drew",
        );
    }

    #[test]
    fn an_id_the_drawing_holds_no_box_for_reads_absent() {
        let (base, names) = towers();
        let map = mapped(&base);
        assert!(map.drawing.placed(names.get("wide").unwrap()).is_some());
        assert!(
            map.drawing
                .placed(Id(base.nodes.len() as u32 + 100))
                .is_none()
        );
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
        g.add_node(Node::new(outer, "outer", Kind::Package, None));
        for kid in ["a", "b"] {
            let id = names.intern(kid);
            g.add_node(Node::new(id, kid, Kind::Action, Some(outer)));
        }
        let other = names.intern("other");
        g.add_node(Node::new(other, "other", Kind::Target, None));
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
        g.add_node(Node::new(outer, "outer", Kind::Package, None));
        let only = names.intern("only");
        g.add_node(Node::new(only, "only", Kind::Action, Some(outer)));
        let other = names.intern("other");
        g.add_node(Node::new(other, "other", Kind::Target, None));
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
        g.add_node(Node::new(root, "root", Kind::Package, None));
        let members: Vec<Id> = (0..80)
            .map(|i| {
                let id = names.intern(&format!("m{i}"));
                g.add_node(Node::new(id, "m", Kind::Action, Some(root)));
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
        g.add_node(Node::new(first, "first", Kind::Action, Some(root)));
        g.add_node(Node::new(root, "root", Kind::Package, None));
        g.add_node(Node::new(second, "second", Kind::Action, Some(root)));
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
