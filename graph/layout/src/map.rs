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
///
/// A frame's edges only settle the order its boxes are placed in: the map routes
/// every connector itself, between boxes already placed, so the layout engine's
/// own routing is never drawn. The engine nonetheless materializes a dummy node
/// for each layer an edge spans, so a frame holding a dense tangle costs memory
/// super-linearly in its edges while buying an order no reader can make out. Past
/// this many the frame is laid out along a subset, which orders its boxes by as
/// much of the tangle as stays affordable and leaves what is drawn unchanged.
const MAX_FRAME_EDGES: usize = 2000;

/// Places every node once and derives the zoom pyramid of views over it, with
/// nothing folded away.
///
/// `base`'s node ids must be a dense permutation of `0..nodes.len()`, every
/// `parent` must name another of its nodes, and containment must be acyclic.
/// `reduced` must be the transitive reduction of `base`, whose nodes it shares.
pub fn map_layout(base: &Graph, reduced: &Graph) -> MapLayout {
    let containment = Containment::new(base);
    // The reduction reaches as far as the base, so it stacks the boxes the same
    // way for a fraction of the routing work.
    let placement = Placement::new(base, &containment, frame_edges(reduced, &containment));
    views(base, reduced, containment, placement, HashSet::new())
}

/// Folds `container` away where the map draws it open, and unfolds it where the
/// map has it folded, re-placing what that resizing reaches and deriving the
/// pyramid afresh.
///
/// `container` must be one of [`MapLayout::foldable`]; `base` and `reduced` must
/// be the graphs `previous` was built from.
pub fn fold(previous: &MapLayout, base: &Graph, reduced: &Graph, container: Id) -> MapLayout {
    let mut folded = previous.folded.clone();
    if !folded.remove(&container) {
        folded.insert(container);
    }
    let mut placement = previous.placement.clone();
    placement.refit(base, &previous.containment, &folded, container);
    views(
        base,
        reduced,
        previous.containment.clone(),
        placement,
        folded,
    )
}

/// The pyramid over one placement, together with the state a further fold starts
/// from.
fn views(
    base: &Graph,
    reduced: &Graph,
    containment: Containment,
    placement: Placement,
    folded: HashSet<Id>,
) -> MapLayout {
    let hidden = hidden_under(base, &containment, &folded);
    let drawing = placement.drawing(base, &containment, &hidden);
    // A folded container never opens, and nothing inside one is placed at all,
    // so neither is a container the zoom can reveal. The zoom reveals only
    // multi-child containers: a single-child one is transparent to it, its child
    // standing in the same place whether or not the zoom has reached it.
    let expandable: HashSet<Id> = lod::expandable_containers(base)
        .into_iter()
        .filter(|c| !hidden.contains(c) && !folded.contains(c))
        .collect();
    // A fold, unlike the zoom, reaches every container the drawing holds,
    // single-child ones included: the reader picks a box by what it is, not by
    // how many children it turned out to have. A folded one is still offered,
    // since clicking it again is what brings its subtree back.
    let foldable: HashSet<Id> = lod::containers(base)
        .into_iter()
        .filter(|c| !hidden.contains(c))
        .collect();

    let view = |open: &HashSet<Id>| {
        level(
            base,
            reduced,
            open,
            &folded,
            &drawing,
            &containment,
            &expandable,
        )
    };
    let mut open: HashSet<Id> = HashSet::new();
    let coarsest = view(&open);

    let scales = thresholds(&expandable, &drawing);
    let mut finer: Vec<(f64, LaidOut)> = Vec::with_capacity(scales.len());
    for scale in scales {
        let opened = open_at(&expandable, &drawing, scale);
        if opened != open {
            open = opened;
            let v = view(&open);
            finer.push((scale, v));
        }
    }
    MapLayout {
        coarsest,
        finer,
        placement,
        containment,
        folded,
        foldable,
    }
}

/// The views of one drawing at rising zoom, coarsest first.
pub struct MapLayout {
    coarsest: LaidOut,
    /// Ascending by the scale each view takes over from.
    finer: Vec<(f64, LaidOut)>,
    /// Where the drawing put every box, kept so a fold can close the frames it
    /// reaches up around the change without laying them out again.
    placement: Placement,
    containment: Containment,
    folded: HashSet<Id>,
    foldable: HashSet<Id>,
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

    /// The containers [`fold`] takes: those drawn open that it can fold away,
    /// and those already folded that it can bring back.
    pub fn foldable(&self) -> &HashSet<Id> {
        &self.foldable
    }

    /// The containers currently folded away, drawn as leaves in place of their
    /// subtrees.
    pub fn folded(&self) -> &HashSet<Id> {
        &self.folded
    }
}

/// Every node with a folded strict ancestor, which the placement leaves out
/// entirely: its position would be the one it held inside a container that no
/// longer spans it.
fn hidden_under(base: &Graph, containment: &Containment, folded: &HashSet<Id>) -> HashSet<Id> {
    if folded.is_empty() {
        return HashSet::new();
    }
    let mut by_depth: Vec<Id> = base.nodes.iter().map(|n| n.id).collect();
    by_depth.sort_by_key(|&id| containment.depth(id));
    let mut hidden: HashSet<Id> = HashSet::new();
    for id in by_depth {
        // A node is reached only after its parent, so the parent's own verdict
        // is already in.
        if let Some(parent) = containment.parent(id)
            && (folded.contains(&parent) || hidden.contains(&parent))
        {
            hidden.insert(id);
        }
    }
    hidden
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

/// One global placement: where every node sits within the frame that placed it,
/// and what every box measures.
///
/// A frame is named by the container whose children it places, the top frame
/// holding the roots being `None`. The layout engine runs once, when the
/// placement is built. A fold afterwards never moves a box: it changes the
/// measure of the container it acts on, and the frames above that one take in
/// whatever their members now span. Nothing is re-laid-out, so no box can end up
/// on the other side of a box it was beside — the price is that a folded
/// subtree's space is left standing rather than closed up.
#[derive(Clone)]
struct Placement {
    /// The boxes each frame places.
    members: HashMap<Option<Id>, Vec<Id>>,
    /// The space each container keeps to the left and right of its children,
    /// held fixed so a container that changes measure does not shift them.
    pad: HashMap<Id, f64>,
    /// Each node's box measure; a folded container measures as a leaf.
    size: HashMap<Id, (f64, f64)>,
    /// Each node's top-left within the frame that placed it, settled once.
    offset: HashMap<Id, (f64, f64)>,
    /// What the top frame spans, and so the extent every level covers.
    width: f64,
    height: f64,
}

impl Placement {
    /// Lays out the whole containment forest with every container open: each
    /// frame places only its own children, deepest frames first so a container
    /// is sized before the frame that places it.
    fn new(
        graph: &Graph,
        containment: &Containment,
        edges: HashMap<Option<Id>, Vec<(Id, Id)>>,
    ) -> Self {
        let children_of = layout::children_by_parent(graph);
        let mut placement = Self {
            members: members_by_frame(graph, &children_of),
            pad: HashMap::with_capacity(children_of.len()),
            size: leaf_sizes(graph, &children_of),
            offset: HashMap::with_capacity(graph.nodes.len()),
            width: 0.0,
            height: 0.0,
        };

        let label_of = labels(graph);
        let no_edges: Vec<(Id, Id)> = Vec::new();
        let edges_of = |frame: Option<Id>| edges.get(&frame).unwrap_or(&no_edges).as_slice();
        for stratum in strata(&children_of, containment).iter_mut().rev() {
            // Frames of one stratum share no boxes, so they place in any order;
            // the sort only keeps that order the same from run to run.
            stratum.sort_unstable();
            let laid = in_parallel(stratum, |c| {
                lay_out_frame(
                    &placement.members[&Some(*c)],
                    edges_of(Some(*c)),
                    &placement.size,
                )
            });
            for (container, frame) in laid {
                placement.adopt(Some(container), &label_of, frame);
            }
        }
        let top =
            on_worker(|| lay_out_frame(&placement.members[&None], edges_of(None), &placement.size));
        placement.adopt(None, &label_of, top);
        placement
    }

    /// Takes in that `toggled` has been folded away or brought back: its box
    /// takes a leaf's measure or its own again, the frame holding it closes up
    /// around the change, and each container above it in turn does the same. The
    /// walk stops at the first container that comes back the measure it already
    /// had, since one of unchanged measure leaves the frame holding it as it was.
    ///
    /// `toggled` must have no folded ancestor.
    fn refit(
        &mut self,
        graph: &Graph,
        containment: &Containment,
        folded: &HashSet<Id>,
        toggled: Id,
    ) {
        let label_of = labels(graph);
        let mut changed = toggled;
        let mut measure = match folded.contains(&toggled) {
            true => (layout::leaf_width(label_of[&toggled]), LEAF_HEIGHT),
            false => self.fitted(toggled, label_of[&toggled]),
        };
        let mut frame = containment.parent(toggled);
        loop {
            self.reflow(frame, changed, measure);
            let Some(container) = frame else {
                (self.width, self.height) = self.spanned(None);
                return;
            };
            let fitted = self.fitted(container, label_of[&container]);
            if self.size[&container] == fitted {
                return;
            }
            (changed, measure) = (container, fitted);
            frame = containment.parent(container);
        }
    }

    /// Closes `frame` up around `changed` taking the measure `measure`.
    ///
    /// The frame is read as rows of boxes whose vertical spans overlap, and each
    /// row keeps the gap it had to the row above. Space a change frees down the
    /// frame is therefore taken up, and space it needs is made, without any row
    /// passing another.
    ///
    /// Only the rows move. Closing a row up along its own axis as well would
    /// reclaim the width a narrowed box frees, but each row would have to close
    /// independently, and two boxes of different rows have no gap between them to
    /// keep — so one could come back on the other side of the other, which is the
    /// one thing this must never do.
    ///
    /// `changed` itself does not move: its column is untouched, and the rows its
    /// own row is measured against are all above the change, so its row resolves
    /// to the top it already had. That holds at every frame on the walk, so the
    /// box the reader clicked stays put.
    fn reflow(&mut self, frame: Option<Id>, changed: Id, measure: (f64, f64)) {
        let rows = self.rows(frame);
        // The spans the new ones keep the gaps of, read before the change lands
        // so that the gaps are the ones the reader has been looking at.
        let was: HashMap<Id, (f64, f64)> = self.members[&frame]
            .iter()
            .map(|&id| (id, (self.offset[&id].1, self.size[&id].1)))
            .collect();
        let spans: Vec<(f64, f64)> = rows.iter().map(|row| span(row, &was)).collect();
        self.size.insert(changed, measure);

        let mut bottom = 0.0f64;
        for (i, row) in rows.iter().enumerate() {
            let top = match i {
                0 => spans[0].0,
                _ => bottom + (spans[i].0 - spans[i - 1].1),
            };
            for id in row {
                self.offset.get_mut(id).expect("a placed box").1 = was[id].0 + top - spans[i].0;
            }
            bottom = row
                .iter()
                .fold(0.0f64, |b, id| b.max(self.offset[id].1 + self.size[id].1));
        }
    }

    /// The rows `frame`'s boxes read as: boxes whose vertical spans overlap sit
    /// side by side and make one row, and the rows run down the frame in order.
    fn rows(&self, frame: Option<Id>) -> Vec<Vec<Id>> {
        let mut by_top = self.members[&frame].clone();
        // Ties broken by id, so one geometry always reads as the same rows.
        by_top.sort_by(|a, b| {
            self.offset[a]
                .1
                .total_cmp(&self.offset[b].1)
                .then(a.0.cmp(&b.0))
        });
        let mut rows: Vec<Vec<Id>> = Vec::new();
        let mut bottom = f64::NEG_INFINITY;
        for id in by_top {
            let (top, height) = (self.offset[&id].1, self.size[&id].1);
            match rows.last_mut() {
                Some(row) if top < bottom => {
                    row.push(id);
                    bottom = bottom.max(top + height);
                }
                _ => {
                    rows.push(vec![id]);
                    bottom = top + height;
                }
            }
        }
        rows
    }

    /// What `container` measures around the children it holds where they stand:
    /// far enough to cover them, keeping the space it set aside for its title and
    /// the padding it was built with.
    fn fitted(&self, container: Id, label: &str) -> (f64, f64) {
        let (right, bottom) = self.spanned(Some(container));
        (
            (right + self.pad[&container]).max(layout::title_width(label)),
            bottom + BORDER,
        )
    }

    /// How far the boxes of `frame` reach from its origin.
    fn spanned(&self, frame: Option<Id>) -> (f64, f64) {
        self.members[&frame]
            .iter()
            .fold((0.0f64, 0.0f64), |(right, bottom), id| {
                let ((x, y), (width, height)) = (self.offset[id], self.size[id]);
                (right.max(x + width), bottom.max(y + height))
            })
    }

    /// Takes on a freshly laid-out frame: where it leaves its boxes, and the
    /// measure it gives the container that heads it.
    fn adopt(&mut self, frame: Option<Id>, label_of: &HashMap<Id, &str>, laid: FlatFrame) {
        // Where the frame's top-left corner sits in the box heading it. The top
        // frame heads none and is the drawing's own origin.
        let (fx, fy) = match frame {
            Some(container) => {
                let (size, pad) = container_size(label_of[&container], (laid.width, laid.height));
                self.size.insert(container, size);
                self.pad.insert(container, pad);
                (pad, LABEL_BAND)
            }
            None => {
                (self.width, self.height) = (laid.width, laid.height);
                (0.0, 0.0)
            }
        };
        self.offset.extend(
            laid.offsets
                .into_iter()
                .map(|(id, (x, y))| (id, (x + fx, y + fy))),
        );
    }

    /// Resolves every drawn node's offset within its own frame against the
    /// frames holding it, into the one absolute frame the pyramid draws in.
    /// Nodes `hidden` names sit inside a folded container and are left out.
    fn drawing(&self, graph: &Graph, containment: &Containment, hidden: &HashSet<Id>) -> Drawing {
        let mut by_depth: Vec<&Node> = graph
            .nodes
            .iter()
            .filter(|n| !hidden.contains(&n.id))
            .collect();
        by_depth.sort_by_key(|n| containment.depth(n.id));
        let mut placed: HashMap<Id, Placed> = HashMap::with_capacity(by_depth.len());
        for node in by_depth {
            let (dx, dy) = self.offset[&node.id];
            let (x, y) = match node.parent {
                Some(parent) => {
                    let outer = placed[&parent];
                    (outer.x + dx, outer.y + dy)
                }
                None => (dx, dy),
            };
            let (width, height) = self.size[&node.id];
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
        Drawing {
            placed,
            width: self.width,
            height: self.height,
        }
    }
}

/// What `row` spanned down its frame, from the tops and heights its boxes had
/// before the reflow.
fn span(row: &[Id], was: &HashMap<Id, (f64, f64)>) -> (f64, f64) {
    row.iter()
        .fold((f64::INFINITY, 0.0f64), |(top, bottom), id| {
            let (y, height) = was[id];
            (top.min(y), bottom.max(y + height))
        })
}

/// One frame's own coordinate system: every offset is measured from its
/// top-left corner, and the extent is what its boxes span from there.
struct FlatFrame {
    offsets: Vec<(Id, (f64, f64))>,
    width: f64,
    height: f64,
}

fn lay_out_frame(members: &[Id], edges: &[(Id, Id)], size: &HashMap<Id, (f64, f64)>) -> FlatFrame {
    let boxes: Vec<(Id, f64, f64)> = members
        .iter()
        .map(|&id| {
            let (width, height) = size[&id];
            (id, width, height)
        })
        .collect();
    let (placements, width, height) = layout::layout_flat(&boxes, edges);
    FlatFrame {
        offsets: placements.into_iter().map(|p| (p.id, (p.x, p.y))).collect(),
        width,
        height,
    }
}

/// The box a container measures around the frame holding its children, and the
/// inset that leaves each child within it. The box holds its title even where
/// that is wider than the frame, which then sits centred in the width the title
/// asks for.
fn container_size(label: &str, frame: (f64, f64)) -> ((f64, f64), f64) {
    let width = (frame.0 + 2.0 * BORDER).max(layout::title_width(label));
    (
        (width, frame.1 + LABEL_BAND + BORDER),
        (width - frame.0) / 2.0,
    )
}

fn labels(graph: &Graph) -> HashMap<Id, &str> {
    graph
        .nodes
        .iter()
        .map(|n| (n.id, n.label.as_str()))
        .collect()
}

/// The boxes each frame places, the roots standing under `None`.
fn members_by_frame(
    graph: &Graph,
    children_of: &HashMap<Id, Vec<&Node>>,
) -> HashMap<Option<Id>, Vec<Id>> {
    let mut members: HashMap<Option<Id>, Vec<Id>> = HashMap::with_capacity(children_of.len() + 1);
    for (&container, kids) in children_of {
        members.insert(Some(container), kids.iter().map(|n| n.id).collect());
    }
    members.insert(
        None,
        graph
            .nodes
            .iter()
            .filter(|n| n.parent.is_none())
            .map(|n| n.id)
            .collect(),
    );
    members
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
    frames
        .into_iter()
        .map(|(frame, edges)| (frame, edges.into_iter().take(MAX_FRAME_EDGES).collect()))
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
    folded: &HashSet<Id>,
    drawing: &Drawing,
    containment: &Containment,
    expandable: &HashSet<Id>,
) -> LaidOut {
    let visible = lod::collapse(base, open, folded);
    let mut connected: HashSet<(Id, Id)> = HashSet::with_capacity(visible.edges.len());
    connected.extend(visible.edges.iter().map(|e| (e.from, e.to)));
    let boxes = boxes(&visible, drawing, containment);
    let drawn = drawn_edges(visible, reduced, open, folded, expandable);
    LaidOut {
        boxes,
        connectors: connectors(&drawn, drawing),
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
    folded: &HashSet<Id>,
    expandable: &HashSet<Id>,
) -> Graph {
    let kept = lod::collapse_edges(reduced, open, folded);
    visible.edges.retain(|e| kept.contains(&(e.from, e.to)));
    // A view that folds nothing away — every container the zoom can open is
    // open, and the reader has folded none shut — draws each edge between the
    // nodes it already ran between, so the base's reduction still describes it.
    match folded.is_empty() && expandable.iter().all(|c| open.contains(c)) {
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

    /// `map` with `container` folded, or unfolded where it already was folded.
    fn folded(map: &MapLayout, base: &Graph, container: Id) -> MapLayout {
        fold(map, base, &reduce::transitive_reduction(base), container)
    }

    /// Every box the whole pyramid draws, by the level it first appears on, so a
    /// fold's effect on both what is drawn and where can be read off one table.
    fn drawn_boxes<'a>(
        map: &MapLayout,
        names: &'a Names,
    ) -> HashMap<&'a str, (f64, f64, f64, f64)> {
        let mut all: HashMap<&str, (f64, f64, f64, f64)> = HashMap::new();
        for i in 0..map.level_count() {
            for b in &map.level(i).boxes {
                all.insert(names.resolve(b.id), (b.x, b.y, b.width, b.height));
            }
        }
        all
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

    #[test]
    fn a_folded_container_measures_as_a_leaf_and_draws_nothing_inside() {
        let (base, names) = towers();
        let wide = names.get("wide").unwrap();
        let map = folded(&mapped(&base), &base, wide);

        let drawn = drawn_boxes(&map, &names);
        let (_, _, width, height) = drawn["wide"];
        assert_eq!(
            (width, height),
            (layout::leaf_width("wide"), LEAF_HEIGHT),
            "a folded container keeps a container's measure"
        );
        for child in ["wide.0", "wide.5"] {
            assert!(
                !drawn.contains_key(child),
                "{child} is drawn inside a folded container"
            );
        }
        // The towers beside it are untouched by the fold and still open up.
        assert!(drawn.contains_key("mid.0") && drawn.contains_key("narrow.0"));
    }

    #[test]
    fn folding_closes_the_gap_the_container_leaves() {
        let (base, names) = towers();
        let map = mapped(&base);
        let open = (map.width(), map.height());
        let folded = folded(&map, &base, names.get("wide").unwrap());
        assert!(
            folded.width() * folded.height() < open.0 * open.1,
            "the drawing spans {}x{} folded, no less than the {}x{} it spanned open",
            folded.width(),
            folded.height(),
            open.0,
            open.1,
        );
    }

    #[test]
    fn unfolding_puts_every_box_back_where_it_was() {
        let (base, names) = towers();
        let map = mapped(&base);
        let before = drawn_boxes(&map, &names);

        let wide = names.get("wide").unwrap();
        let there_and_back = folded(&folded(&map, &base, wide), &base, wide);

        assert_eq!(
            (there_and_back.width(), there_and_back.height()),
            (map.width(), map.height())
        );
        let after = drawn_boxes(&there_and_back, &names);
        let moved: Vec<&str> = before
            .keys()
            .filter(|id| before[*id] != after.get(*id).copied().unwrap_or_default())
            .copied()
            .collect();
        assert!(moved.is_empty(), "boxes did not come back: {moved:?}");
    }

    /// Each fold re-lays out the frames holding it, and those frames' members
    /// include containers folded earlier. Their measure is an input to that
    /// layout, not something it derives, so an earlier fold survives a later
    /// one.
    #[test]
    fn folding_a_second_container_leaves_the_first_folded() {
        let (base, names) = towers();
        let (wide, mid) = (names.get("wide").unwrap(), names.get("mid").unwrap());

        let both = folded(&folded(&mapped(&base), &base, wide), &base, mid);

        assert_eq!(*both.folded(), [wide, mid].into_iter().collect());
        let drawn = drawn_boxes(&both, &names);
        for (container, label) in [(wide, "wide"), (mid, "mid")] {
            let (_, _, width, height) = drawn[names.resolve(container)];
            assert_eq!(
                (width, height),
                (layout::leaf_width(label), LEAF_HEIGHT),
                "{label} did not stay folded",
            );
        }
        for child in ["wide.0", "mid.0"] {
            assert!(!drawn.contains_key(child), "{child} came back");
        }
        // The one tower left open still opens up.
        assert!(drawn.contains_key("narrow.0"));
    }

    /// The box the reader clicked stays under the cursor. Everything its own
    /// place is measured against is untouched by the fold, so it resolves to the
    /// place it already held, at its own frame and at every frame above.
    #[test]
    fn a_fold_leaves_the_box_it_acts_on_where_it_stood() {
        let (base, names) = towers();
        for target in ["wide", "mid", "narrow"] {
            let map = mapped(&base);
            let before = drawn_boxes(&map, &names);
            let id = names.get(target).unwrap();
            let after = drawn_boxes(&folded(&map, &base, id), &names);
            assert_eq!(
                (before[target].0, before[target].1),
                (after[target].0, after[target].1),
                "{target} moved when it was folded",
            );
        }
    }

    /// Every pair of boxes one frame places keeps the way it reads: a fold takes
    /// up the space it frees by closing the frame's rows and their boxes up, and
    /// closing up never lets one box past another.
    #[test]
    fn a_fold_reorders_nothing() {
        let (base, names) = towers();
        let map = mapped(&base);
        let before = drawn_boxes(&map, &names);
        let wide = names.get("wide").unwrap();
        let after = drawn_boxes(&folded(&map, &base, wide), &names);

        let mut compared = 0;
        for one in &base.nodes {
            for other in &base.nodes {
                if one.id == other.id || one.parent != other.parent {
                    continue;
                }
                let (a, b) = (names.resolve(one.id), names.resolve(other.id));
                let (Some(wa), Some(wb), Some(na), Some(nb)) =
                    (before.get(a), before.get(b), after.get(a), after.get(b))
                else {
                    continue;
                };
                compared += 1;
                assert_eq!(
                    wa.0.total_cmp(&wb.0),
                    na.0.total_cmp(&nb.0),
                    "{a}/{b} swapped sides"
                );
                assert_eq!(
                    wa.1.total_cmp(&wb.1),
                    na.1.total_cmp(&nb.1),
                    "{a}/{b} swapped rows"
                );
            }
        }
        assert!(compared > 0);
    }

    /// The space a fold frees is taken up rather than left standing: the boxes
    /// below the folded one close up over it.
    #[test]
    fn a_fold_pulls_what_follows_it_into_the_space_freed() {
        let (base, names) = towers();
        let map = mapped(&base);
        let before = drawn_boxes(&map, &names);
        let wide = names.get("wide").unwrap();
        let after = drawn_boxes(&folded(&map, &base, wide), &names);

        // Some root below or right of the folded tower came back nearer to it.
        let closed_up = ["mid", "narrow", "lone"]
            .into_iter()
            .any(|id| after[id].0 < before[id].0 || after[id].1 < before[id].1);
        assert!(closed_up, "nothing moved into the space the fold freed");
    }

    #[test]
    fn folding_offers_the_container_back_and_takes_its_subtree_out_of_reach() {
        let mut g = Graph::new();
        let mut names = Names::default();
        let outer = names.intern("outer");
        g.add_node(outer, "outer", Kind::Package, None);
        let inner = names.intern("inner");
        g.add_node(inner, "inner", Kind::Package, Some(outer));
        let sibling = names.intern("sibling");
        g.add_node(sibling, "sibling", Kind::Action, Some(outer));
        for kid in ["a", "b"] {
            let id = names.intern(kid);
            g.add_node(id, kid, Kind::Action, Some(inner));
        }

        let map = mapped(&g);
        assert_eq!(*map.foldable(), [outer, inner].into_iter().collect());
        assert!(map.folded().is_empty());

        let map = folded(&map, &g, outer);
        assert_eq!(*map.folded(), [outer].into_iter().collect());
        assert_eq!(
            *map.foldable(),
            [outer].into_iter().collect(),
            "a container inside a folded one is still offered a handle",
        );
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
