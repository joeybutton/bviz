//! Lays out the compound graph with elk-rs (a pure-Rust port of the Eclipse
//! Layout Kernel) and collects absolute geometry ready to render.
//!
//! The graph is built straight into ELK's in-memory model: nested `ElkNode`s
//! mirror the containment forest and edges connect the leaf nodes. The layered
//! algorithm runs as one hierarchy-spanning pass (`INCLUDE_CHILDREN`), routing
//! cross-container edges orthogonally. Leaf nodes are sized from their label
//! length; container nodes are sized by ELK to fit their children, with extra
//! top padding reserved for the title band.
//!
//! ELK reports a node's position relative to its parent and an edge's points
//! relative to the edge's containing node, so both are accumulated up the
//! containment chain into the absolute, top-left frame the renderer expects.

use std::collections::{BTreeMap, HashMap, HashSet};

use org_eclipse_elk_alg_layered::org::eclipse::elk::alg::layered::options::{
    CycleBreakingStrategy, GraphCompactionStrategy, InteractiveReferencePoint, LayeredOptions,
    LayeringStrategy, NodePlacementStrategy,
};
use org_eclipse_elk_alg_layered::org::eclipse::elk::alg::layered::plain_java_initialization::initialize_plain_java_layout;
use org_eclipse_elk_core::org::eclipse::elk::core::math::{ElkPadding, KVector};
use org_eclipse_elk_core::org::eclipse::elk::core::options::{
    CoreOptions, Direction, EdgeRouting, HierarchyHandling, SizeConstraint,
};
use org_eclipse_elk_core::org::eclipse::elk::core::util::BasicProgressMonitor;
use org_eclipse_elk_core::org::eclipse::elk::core::{IGraphLayoutEngine, RecursiveGraphLayoutEngine};
use org_eclipse_elk_graph::org::eclipse::elk::graph::properties::Property;
use org_eclipse_elk_graph::org::eclipse::elk::graph::util::ElkGraphUtil;
use org_eclipse_elk_graph::org::eclipse::elk::graph::{
    ElkConnectableShapeRef, ElkEdgeRef, ElkNodeRef,
};

use crate::graph::{EdgeKind, Graph, Kind, Node};
use crate::intern::Id;

/// Above this many routed node pairs the view is a dense tangle whose crossings
/// ELK's default thoroughness spends most of the layout time refining for little
/// readable gain, so the layout drops to a single crossing-minimization sweep.
/// Every capped startup view and ordinary expansion stays below this, keeping the
/// default thoroughness, so their layouts are unaffected.
const DENSE_ROUTED_EDGES: usize = 1000;

/// Above this many edges one flat frame is a tangle whose long edges become
/// chains of dummy nodes, and balancing those by network simplex runs orders of
/// magnitude longer than the rest of the layout together. Such a frame places by
/// the simple strategy, which fills each layer left to right in time linear in
/// the frame, and on a tangle lands narrower than the balanced placement does.
const TANGLED_FRAME_EDGES: usize = 500;

pub(crate) const LEAF_HEIGHT: f64 = 30.0;
const CHAR_WIDTH: f64 = 7.0;
const LABEL_PADDING: f64 = 22.0;
/// Horizontal space (in layout pixels, pre-zoom) reserved on an expandable leaf
/// box for its disclosure caret, left of the label.
pub const CARET_RESERVE: f64 = 12.0;
/// Inset reserved at the top of each container for its title; the other three
/// sides keep a uniform border.
pub const LABEL_BAND: f64 = 26.0;
pub(crate) const BORDER: f64 = 12.0;

/// Width of a leaf box holding `label`. Every leaf reserves the caret space,
/// expandable or not, which also keeps plain leaves from crowding their labels.
pub(crate) fn leaf_width(label: &str) -> f64 {
    label.chars().count() as f64 * CHAR_WIDTH + LABEL_PADDING + CARET_RESERVE
}

/// Width a container needs for its bold, centered title to sit clear of the
/// rounded corners and border.
pub(crate) fn title_width(label: &str) -> f64 {
    label.chars().count() as f64 * CHAR_WIDTH + LABEL_PADDING + 2.0 * BORDER
}

/// A laid-out box in absolute coordinates, top-left origin.
pub struct Box {
    /// The graph node id this box renders, for hit-testing back to the node.
    pub id: Id,
    pub label: String,
    pub kind: Kind,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// A container holds child boxes; its label sits in the top band.
    pub compound: bool,
    /// A synthetic group container or collapsed group leaf, the boxes the
    /// renderer lets the user expand or collapse.
    pub group: bool,
}

/// An edge polyline: start, bend points, end, in order. One connector carries
/// every provenance that runs between the same node pair, since ELK routes the
/// pair once and the renderer draws those kinds along the shared route.
pub struct Connector {
    pub points: Vec<(f64, f64)>,
    /// The distinct [`EdgeKind`]s on this pair, in enum order.
    pub kinds: Vec<EdgeKind>,
    /// Node ids the edge runs between, for hit-testing a node's connections.
    pub from: Id,
    pub to: Id,
}

/// A node's place in the layered configuration of the frame that laid it out.
/// The drawn coordinates follow from it, so two drawings whose nodes anchor
/// alike read the same way however far their boxes moved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Anchor {
    /// How far along the layout direction the node sits, counted in layers.
    pub layer: u32,
    /// Rank across the layer, ascending along the frame's other axis.
    pub slot: u32,
}

#[derive(Default)]
pub struct LaidOut {
    /// Parent before child, so a renderer draws a container under its children.
    pub boxes: Vec<Box>,
    pub connectors: Vec<Connector>,
    /// The layered configuration this drawing realizes, for reproducing its
    /// reading order in another drawing. A node the layout reported no place for
    /// is absent, and a drawing from a layout that numbers no node at all leaves
    /// this empty.
    pub anchors: HashMap<Id, Anchor>,
    /// Every connected node pair in the view before any edges were dropped for
    /// drawing (e.g. by transitive reduction), so hovering can still light up a
    /// node's full set of neighbors even where no connector is drawn to them.
    pub full_edges: Vec<(Id, Id)>,
    pub width: f64,
    pub height: f64,
}

pub fn layout(graph: &Graph) -> LaidOut {
    lay_out(graph, None)
}

/// Lays `graph` out held to `anchors`: each frame takes its layering, its cycle
/// breaking, and the order within each layer from the given configuration instead
/// of deriving one, so the anchored nodes come back in the order they were given.
/// The layers and slots they come back numbered by are the drawing's own, counted
/// densely from zero, not the numbers they were pinned at. Coordinates and edge
/// routes are computed as usual, so the drawing fits whatever the boxes now
/// measure.
///
/// A node `anchors` omits carries no pinned position, so the crossing
/// minimization ranks it freely within its layer. The layerer reads a position
/// for every node and sees such a node at the origin, which puts it in the first
/// layer; from there ELK's feasibility repair pushes any node that would draw at
/// or above one of its sources one layer past that source, cascading along the
/// edges out of each node it moves.
pub fn layout_pinned(graph: &Graph, anchors: &HashMap<Id, Anchor>) -> LaidOut {
    lay_out(graph, Some(anchors))
}

fn lay_out(graph: &Graph, pinned: Option<&HashMap<Id, Anchor>>) -> LaidOut {
    let children_of = children_by_parent(graph);
    let (root, elk_of) = build_elk_graph(graph, &children_of, pinned);
    let edges = route_edges(graph, &elk_of, &root);

    let mut engine = RecursiveGraphLayoutEngine::new();
    let mut monitor = BasicProgressMonitor::new();
    engine.layout(&root, &mut monitor);

    let boxes = collect_boxes(graph, &children_of, &elk_of);
    let connectors = collect_connectors(edges);

    let width = boxes.iter().map(|b| b.x + b.width).fold(0.0, f64::max);
    let height = boxes.iter().map(|b| b.y + b.height).fold(0.0, f64::max);

    LaidOut {
        boxes,
        connectors,
        anchors: collect_anchors(&elk_of),
        full_edges: Vec::new(),
        width,
        height,
    }
}

/// A root graph laying the dependency flow out top-to-bottom through ELK's
/// layered algorithm, so a node sits below the inputs it consumes.
fn layered_root() -> ElkNodeRef {
    initialize_plain_java_layout();
    let root = ElkGraphUtil::create_graph();
    set_property(&root, CoreOptions::ALGORITHM, "org.eclipse.elk.layered".to_string());
    set_property(&root, CoreOptions::DIRECTION, Direction::Down);
    apply_options(&root);
    root
}

/// A box's top-left corner within the frame that laid it out.
pub(crate) struct Placement {
    pub id: Id,
    pub x: f64,
    pub y: f64,
}

/// Lays out sized boxes and their edges as one flat graph, returning each box's
/// position within the frame and the frame's own size.
pub(crate) fn layout_flat(
    boxes: &[(Id, f64, f64)],
    edges: &[(Id, Id)],
) -> (Vec<Placement>, f64, f64) {
    // A frame of one box has nothing to decide, and such frames are the bulk of
    // a containment forest.
    if let [(id, width, height)] = *boxes {
        return (vec![Placement { id, x: 0.0, y: 0.0 }], width, height);
    }
    if boxes.is_empty() {
        return (Vec::new(), 0.0, 0.0);
    }

    let root = layered_root();
    // A frame is one of many that make up a drawing, so it gets the single sweep
    // that DENSE_ROUTED_EDGES reserves for a dense view.
    set_property(&root, LayeredOptions::THOROUGHNESS, 1);
    if edges.len() > TANGLED_FRAME_EDGES {
        set_property(
            &root,
            LayeredOptions::NODE_PLACEMENT_STRATEGY,
            NodePlacementStrategy::Simple,
        );
    }

    let mut elk_of: HashMap<Id, ElkNodeRef> = HashMap::with_capacity(boxes.len());
    for &(id, width, height) in boxes {
        let elk = ElkGraphUtil::create_node(Some(root.clone()));
        elk.borrow_mut().connectable().shape().set_dimensions(width, height);
        elk_of.insert(id, elk);
    }
    for (from, to) in edges {
        ElkGraphUtil::create_simple_edge(
            ElkConnectableShapeRef::Node(elk_of[from].clone()),
            ElkConnectableShapeRef::Node(elk_of[to].clone()),
        );
    }

    let mut engine = RecursiveGraphLayoutEngine::new();
    let mut monitor = BasicProgressMonitor::new();
    engine.layout(&root, &mut monitor);

    // ELK insets the graph by its own padding; anchoring at the boxes' own
    // extent leaves the frame's size to the caller alone.
    let placed: Vec<(Id, f64, f64, f64, f64)> = boxes
        .iter()
        .map(|&(id, ..)| {
            let (x, y) = origin(&elk_of[&id]);
            let (width, height) = dimensions(&elk_of[&id]);
            (id, x, y, width, height)
        })
        .collect();
    let left = placed.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let top = placed.iter().map(|p| p.2).fold(f64::INFINITY, f64::min);
    let width = placed.iter().map(|p| p.1 + p.3).fold(0.0, f64::max) - left;
    let height = placed.iter().map(|p| p.2 + p.4).fold(0.0, f64::max) - top;

    let placements = placed
        .into_iter()
        .map(|(id, x, y, ..)| Placement { id, x: x - left, y: y - top })
        .collect();
    (placements, width, height)
}

/// Children indexed by parent id; a node that appears as a key is a container.
pub(crate) fn children_by_parent(graph: &Graph) -> HashMap<Id, Vec<&Node>> {
    let mut children_of: HashMap<Id, Vec<&Node>> = HashMap::new();
    for node in &graph.nodes {
        if let Some(parent) = node.parent {
            children_of.entry(parent).or_default().push(node);
        }
    }
    children_of
}

/// Builds the ELK graph mirroring the containment forest and maps each graph node
/// to its ELK node by id. The root and every container carry the layout options,
/// since INCLUDE_CHILDREN lays each container out as its own nested graph reading
/// its own properties.
fn build_elk_graph<'g>(
    graph: &'g Graph,
    children_of: &HashMap<Id, Vec<&'g Node>>,
    pinned: Option<&HashMap<Id, Anchor>>,
) -> (ElkNodeRef, HashMap<Id, ElkNodeRef>) {
    // Routing every edge through one hierarchy-spanning layered pass runs the
    // cross-container edges orthogonally between subgraphs.
    let root = layered_root();
    set_property(&root, CoreOptions::HIERARCHY_HANDLING, HierarchyHandling::IncludeChildren);
    report_anchors(&root);

    let mut elk_of: HashMap<Id, ElkNodeRef> = HashMap::new();
    for node in &graph.nodes {
        if node.parent.is_none() {
            build_node(node, &root, children_of, &mut elk_of);
        }
    }
    for node in &graph.nodes {
        if children_of.contains_key(&node.id) {
            apply_options(&elk_of[&node.id]);
            report_anchors(&elk_of[&node.id]);
        }
    }
    if let Some(anchors) = pinned {
        let anchored = anchored_frames(graph, anchors);
        if anchored.contains(&None) {
            pin_frame(&root);
        }
        for node in &graph.nodes {
            if children_of.contains_key(&node.id) && anchored.contains(&Some(node.id)) {
                pin_frame(&elk_of[&node.id]);
            }
            if let Some(&anchor) = anchors.get(&node.id) {
                pin_node(&elk_of[&node.id], anchor);
            }
        }
    }
    (root, elk_of)
}

/// A node pair ELK routes, and the edge kinds that share the route.
struct RoutedEdge {
    edge: ElkEdgeRef,
    kinds: Vec<EdgeKind>,
    from: Id,
    to: Id,
}

/// Routes one ELK edge per ordered node pair, collapsing the parallel provenance
/// edges between the same two boxes into a single route: the parallel
/// query/cquery/aquery edges would each cost ELK a full route, and that edge count
/// is what dominates layout time on a dense view. The distinct kinds per pair ride
/// along so the renderer can still draw each along the shared route.
fn route_edges(
    graph: &Graph,
    elk_of: &HashMap<Id, ElkNodeRef>,
    root: &ElkNodeRef,
) -> Vec<RoutedEdge> {
    // Ordered by id pair so the routed edges, and thus the connectors, come out
    // in a deterministic order regardless of input edge order.
    let mut kinds_of: BTreeMap<(Id, Id), Vec<EdgeKind>> = BTreeMap::new();
    for edge in &graph.edges {
        if !elk_of.contains_key(&edge.from) || !elk_of.contains_key(&edge.to) {
            continue;
        }
        let kinds = kinds_of.entry((edge.from, edge.to)).or_default();
        if !kinds.contains(&edge.kind) {
            kinds.push(edge.kind);
        }
    }
    // A dense tangle's crossing minimization is the bulk of its layout time and a
    // single sweep reads about as well, so spend the default effort only on views
    // sparse enough for it to matter and finish quickly.
    if kinds_of.len() > DENSE_ROUTED_EDGES {
        set_property(root, LayeredOptions::THOROUGHNESS, 1);
    }
    // create_simple_edge places each edge in the common container of its
    // endpoints, which is where ELK then routes and reports it.
    let mut edges: Vec<RoutedEdge> = Vec::with_capacity(kinds_of.len());
    for ((from_id, to_id), mut kinds) in kinds_of {
        kinds.sort_unstable();
        let (from, to) = (&elk_of[&from_id], &elk_of[&to_id]);
        edges.push(RoutedEdge {
            edge: ElkGraphUtil::create_simple_edge(
                ElkConnectableShapeRef::Node(from.clone()),
                ElkConnectableShapeRef::Node(to.clone()),
            ),
            kinds,
            from: from_id,
            to: to_id,
        });
    }
    edges
}

/// Containment depth per node, used only to draw parents before children.
fn node_depths(graph: &Graph) -> HashMap<Id, usize> {
    let parent_of: HashMap<Id, Id> = graph
        .nodes
        .iter()
        .filter_map(|node| node.parent.map(|p| (node.id, p)))
        .collect();
    let mut depth_of: HashMap<Id, usize> = HashMap::with_capacity(graph.nodes.len());
    for node in &graph.nodes {
        let mut depth = 0;
        let mut id = node.id;
        while let Some(&parent) = parent_of.get(&id) {
            depth += 1;
            id = parent;
        }
        depth_of.insert(node.id, depth);
    }
    depth_of
}

/// Reads each node's laid-out box in absolute coordinates.
fn collect_boxes(
    graph: &Graph,
    children_of: &HashMap<Id, Vec<&Node>>,
    elk_of: &HashMap<Id, ElkNodeRef>,
) -> Vec<Box> {
    let depth_of = node_depths(graph);
    let mut boxes: Vec<(usize, Box)> = Vec::with_capacity(graph.nodes.len());
    for node in &graph.nodes {
        let elk = &elk_of[&node.id];
        let (x, y) = absolute_origin(elk);
        let (width, height) = dimensions(elk);
        boxes.push((
            depth_of[&node.id],
            Box {
                id: node.id,
                label: node.label.clone(),
                kind: node.kind,
                x,
                y,
                width,
                height,
                compound: children_of.contains_key(&node.id),
                group: node.group,
            },
        ));
    }
    boxes.sort_by_key(|(depth, _)| *depth);
    boxes.into_iter().map(|(_, b)| b).collect()
}

/// Reads the anchor of every node the layout numbered.
fn collect_anchors(elk_of: &HashMap<Id, ElkNodeRef>) -> HashMap<Id, Anchor> {
    let mut anchors = HashMap::with_capacity(elk_of.len());
    for (&id, elk) in elk_of {
        if let Some(anchor) = anchor(elk) {
            anchors.insert(id, anchor);
        }
    }
    anchors
}

/// A node's place in its frame, or `None` where the layout numbered none: the
/// properties then carry their -1 default, out of an unsigned rank's range.
fn anchor(node: &ElkNodeRef) -> Option<Anchor> {
    let mut node = node.borrow_mut();
    let properties = node.connectable().shape().graph_element().properties();
    let layer = properties.get_property(LayeredOptions::LAYERING_LAYER_ID)?;
    let slot = properties.get_property(LayeredOptions::CROSSING_MINIMIZATION_POSITION_ID)?;
    Some(Anchor {
        layer: u32::try_from(layer).ok()?,
        slot: u32::try_from(slot).ok()?,
    })
}

/// Builds a drawable connector for each routed pair, dropping any ELK left without
/// a polyline of at least a start and an end.
fn collect_connectors(edges: Vec<RoutedEdge>) -> Vec<Connector> {
    edges
        .into_iter()
        .filter_map(|RoutedEdge { edge, kinds, from, to }| {
            connector(&edge).map(|points| Connector {
                points,
                kinds,
                from,
                to,
            })
        })
        .collect()
}

/// Creates the ELK node for `node` under `parent` and recurses into its subtree,
/// recording the node by id. Leaves carry an explicit size; containers are left
/// unsized for ELK to fit around their children and get extra top padding to
/// clear the title band.
fn build_node<'g>(
    node: &'g Node,
    parent: &ElkNodeRef,
    children_of: &HashMap<Id, Vec<&'g Node>>,
    elk_of: &mut HashMap<Id, ElkNodeRef>,
) {
    let elk = ElkGraphUtil::create_node(Some(parent.clone()));
    match children_of.get(&node.id) {
        Some(kids) => {
            set_property(
                &elk,
                CoreOptions::PADDING,
                ElkPadding::with_values(LABEL_BAND, BORDER, BORDER, BORDER),
            );
            // ELK sizes a container to its children; the title floor keeps a
            // title wider than the children from overflowing the box.
            //
            // The layered algorithm always lays out left-to-right internally and
            // rotates the result into DIRECTION; for the Down direction used here
            // the container's minimum size is consumed in that pre-rotation frame
            // and never transposed back, so the title width goes in y to land on
            // the box's output width.
            set_property(
                &elk,
                CoreOptions::NODE_SIZE_CONSTRAINTS,
                SizeConstraint::minimum_size(),
            );
            set_property(
                &elk,
                CoreOptions::NODE_SIZE_MINIMUM,
                KVector { x: 0.0, y: title_width(&node.label) },
            );
            for kid in kids {
                build_node(kid, &elk, children_of, elk_of);
            }
        }
        None => {
            elk.borrow_mut()
                .connectable()
                .shape()
                .set_dimensions(leaf_width(&node.label), LEAF_HEIGHT);
        }
    }
    elk_of.insert(node.id, elk);
}

/// Sets the layout options on `node`. INCLUDE_CHILDREN lays each container out as
/// its own nested graph that reads spacing and routing from its own properties,
/// so this runs on every container, not only the root, to reach the leaves inside
/// it.
///
/// The spacings pack tight to keep a dense view compact and its labels legible;
/// the exception is the in-flow edge-to-node clearance (between layers), kept
/// higher so edges clear the boxes they pass rather than reading as overlap.
/// Across-flow edge-to-edge spacing is not set: `spacing.edgeEdge` is inert in
/// elk-rs's layered placement, and `spacing.nodeNode` already spaces the parallel
/// edge lanes within a layer.
fn apply_options(node: &ElkNodeRef) {
    set_property(node, CoreOptions::SPACING_NODE_NODE, 10.0);
    set_property(node, LayeredOptions::SPACING_NODE_NODE_BETWEEN_LAYERS, 10.0);
    set_property(node, LayeredOptions::SPACING_EDGE_EDGE_BETWEEN_LAYERS, 2.0);
    set_property(node, LayeredOptions::SPACING_EDGE_NODE_BETWEEN_LAYERS, 10.0);
    set_property(node, CoreOptions::SPACING_EDGE_NODE, 2.0);
    set_property(node, CoreOptions::EDGE_ROUTING, EdgeRouting::Orthogonal);
    set_property(node, LayeredOptions::NODE_PLACEMENT_STRATEGY, NodePlacementStrategy::NetworkSimplex);
    set_property(
        node,
        LayeredOptions::COMPACTION_POST_COMPACTION_STRATEGY,
        GraphCompactionStrategy::EdgeLength,
    );
}

/// Each frame that holds a node `anchors` carries a place for, named by the
/// parent its members share; the root frame's members are the nodes without a
/// parent, so it is named `None`.
fn anchored_frames(graph: &Graph, anchors: &HashMap<Id, Anchor>) -> HashSet<Option<Id>> {
    let mut frames = HashSet::new();
    for node in &graph.nodes {
        if anchors.contains_key(&node.id) {
            frames.insert(node.parent);
        }
    }
    frames
}

/// Has the frame `node` heads read its layering, its cycle breaking, and the
/// order within each layer off its nodes' positions, which is what [`pin_node`]
/// writes an [`Anchor`] as. A frame takes its configuration from the anchors when
/// one of its members carries one, and derives one of its own otherwise.
fn pin_frame(node: &ElkNodeRef) {
    set_property(node, LayeredOptions::LAYERING_STRATEGY, LayeringStrategy::Interactive);
    set_property(
        node,
        LayeredOptions::CYCLE_BREAKING_STRATEGY,
        CycleBreakingStrategy::Interactive,
    );
    set_property(node, LayeredOptions::CROSSING_MINIMIZATION_SEMI_INTERACTIVE, true);
    // Read each pinned position as written, rather than offset by half a node's
    // extent, which at cycle breaking a leaf has and a container has not.
    set_property(
        node,
        LayeredOptions::INTERACTIVE_REFERENCE_POINT,
        InteractiveReferencePoint::TopLeft,
    );
}

/// Spacing between the pseudo coordinates of consecutive layers and slots. It
/// exceeds any extent a node has along the flow axis while the layout reads these
/// positions, so that two layers' spans stay disjoint: the layerer groups nodes
/// into layers by overlapping spans and merges two whose spans touch.
const STRIDE: f64 = 1_000_000.0;

/// Writes `anchor` where the layout reads a configuration from: a pseudo position
/// laying the layer out along the flow axis and the slot across it, both as the
/// node's shape position, which the layerer reads, and as the position property,
/// which the crossing minimization orders each layer by.
fn pin_node(node: &ElkNodeRef, anchor: Anchor) {
    let at = KVector { x: f64::from(anchor.slot) * STRIDE, y: f64::from(anchor.layer) * STRIDE };
    node.borrow_mut().connectable().shape().set_location(at.x, at.y);
    set_property(node, LayeredOptions::POSITION, at);
}

/// Has the frame `node` heads number its nodes' layers and in-layer positions,
/// which costs it an extra pass once the layout is otherwise settled. A frame
/// that omits this leaves its nodes without an [`Anchor`].
fn report_anchors(node: &ElkNodeRef) {
    set_property(node, LayeredOptions::GENERATE_POSITION_AND_LAYER_IDS, true);
}

fn set_property<T: Clone + Send + Sync + 'static>(
    node: &ElkNodeRef,
    property: &Property<T>,
    value: T,
) {
    node.borrow_mut()
        .connectable()
        .shape()
        .graph_element()
        .properties_mut()
        .set_property(property, Some(value));
}

fn dimensions(node: &ElkNodeRef) -> (f64, f64) {
    let mut node = node.borrow_mut();
    let shape = node.connectable().shape();
    (shape.width(), shape.height())
}

/// A node's top-left within its parent.
fn origin(node: &ElkNodeRef) -> (f64, f64) {
    let mut node = node.borrow_mut();
    let shape = node.connectable().shape();
    (shape.x(), shape.y())
}

/// A node's absolute top-left: its parent-relative position plus the absolute
/// origin of its parent, accumulated up to the root.
fn absolute_origin(node: &ElkNodeRef) -> (f64, f64) {
    let (x, y) = origin(node);
    match node.borrow().parent() {
        Some(parent) => {
            let (px, py) = absolute_origin(&parent);
            (x + px, y + py)
        }
        None => (x, y),
    }
}

/// An edge's routing polyline in absolute coordinates: ELK reports each section's
/// points relative to the edge's containing node, so they are shifted by that
/// node's absolute origin.
fn connector(edge: &ElkEdgeRef) -> Option<Vec<(f64, f64)>> {
    let (ox, oy) = edge
        .borrow()
        .containing_node()
        .map(|node| absolute_origin(&node))
        .unwrap_or((0.0, 0.0));

    let mut points = Vec::new();
    let mut edge = edge.borrow_mut();
    for section in edge.sections().iter() {
        let mut section = section.borrow_mut();
        points.push((section.start_x() + ox, section.start_y() + oy));
        for bend in section.bend_points().iter() {
            let bend = bend.borrow();
            points.push((bend.x() + ox, bend.y() + oy));
        }
        points.push((section.end_x() + ox, section.end_y() + oy));
    }
    (points.len() >= 2).then_some(points)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intern::Names;

    /// Three package containers of leaves, with dependencies inside each and
    /// across the boundaries, so the root frame and the container frames each
    /// span several layers and one layer holds boxes of differing width.
    fn compound_graph() -> Graph {
        let mut names = Names::default();
        let mut graph = Graph::new();
        let mut add = |graph: &mut Graph, name: &str, kind: Kind, parent: Option<Id>| {
            let id = names.intern(name);
            graph.add_node(id, name, kind, parent);
            id
        };

        let lib = add(&mut graph, "lib", Kind::Package, None);
        let front = add(&mut graph, "front", Kind::Package, None);
        let back = add(&mut graph, "back", Kind::Package, None);

        let source = add(&mut graph, "source", Kind::Source, Some(lib));
        let archive = add(&mut graph, "archive", Kind::Artifact, Some(lib));
        let entry = add(&mut graph, "entry", Kind::Target, Some(front));
        let widget = add(&mut graph, "widget", Kind::Target, Some(front));
        let much_longer_label = add(&mut graph, "much_longer_label", Kind::Target, Some(front));
        let bundle = add(&mut graph, "bundle", Kind::Target, Some(back));
        let image = add(&mut graph, "image", Kind::Target, Some(back));

        for (from, to) in [
            (source, archive),
            (archive, entry),
            (entry, widget),
            (entry, much_longer_label),
            (widget, bundle),
            (much_longer_label, bundle),
            (bundle, image),
        ] {
            graph.add_edge(from, to, EdgeKind::Query);
        }

        graph
    }

    fn compound() -> (Graph, LaidOut) {
        let graph = compound_graph();
        let laid = layout(&graph);
        (graph, laid)
    }

    /// The compound fixture with a dependency from the last package back into the
    /// first, so the three packages depend on one another in a cycle that the root
    /// frame has to break to lay out.
    fn cyclic() -> (Graph, LaidOut) {
        let mut graph = compound_graph();
        let (image, source) = (labeled(&graph, "image"), labeled(&graph, "source"));
        graph.add_edge(image, source, EdgeKind::Query);
        let laid = layout(&graph);
        (graph, laid)
    }

    /// Two packages, neither depending on the other. One holds a frame whose
    /// balanced layering no layering from the sources alone reaches: `early`
    /// reaches `late` in one step while `first` reaches it through `middle`, so a
    /// balanced layering draws `early` beside `middle` and only a layering that
    /// pushes every source to the top strands it above them. The other holds a
    /// pair for a pinning to bite on.
    fn unbalanced_graph() -> Graph {
        let mut names = Names::default();
        let mut graph = Graph::new();
        let mut add = |graph: &mut Graph, name: &str, kind: Kind, parent: Option<Id>| {
            let id = names.intern(name);
            graph.add_node(id, name, kind, parent);
            id
        };

        let compute = add(&mut graph, "compute", Kind::Package, None);
        let report = add(&mut graph, "report", Kind::Package, None);

        let early = add(&mut graph, "early", Kind::Target, Some(compute));
        let first = add(&mut graph, "first", Kind::Target, Some(compute));
        let middle = add(&mut graph, "middle", Kind::Target, Some(compute));
        let late = add(&mut graph, "late", Kind::Target, Some(compute));
        let draft = add(&mut graph, "draft", Kind::Target, Some(report));
        let published = add(&mut graph, "published", Kind::Target, Some(report));

        for (from, to) in [(early, late), (first, middle), (middle, late), (draft, published)] {
            graph.add_edge(from, to, EdgeKind::Query);
        }

        graph
    }

    fn labeled(graph: &Graph, label: &str) -> Id {
        graph
            .nodes
            .iter()
            .find(|node| node.label == label)
            .unwrap_or_else(|| panic!("the fixture has no node labeled {label}"))
            .id
    }

    /// A configuration keyed by node label and ordered, so a mismatch reports
    /// which nodes moved.
    fn by_label(graph: &Graph, anchors: &HashMap<Id, Anchor>) -> BTreeMap<String, Anchor> {
        graph
            .nodes
            .iter()
            .filter_map(|node| Some((node.label.clone(), *anchors.get(&node.id)?)))
            .collect()
    }

    /// The boxes of each frame the layout ran: the root frame, keyed by `None`,
    /// and one per container.
    fn frames<'a>(graph: &Graph, laid: &'a LaidOut) -> HashMap<Option<Id>, Vec<&'a Box>> {
        let parent_of: HashMap<Id, Option<Id>> =
            graph.nodes.iter().map(|node| (node.id, node.parent)).collect();
        let mut frames: HashMap<Option<Id>, Vec<&Box>> = HashMap::new();
        for drawn in &laid.boxes {
            frames.entry(parent_of[&drawn.id]).or_default().push(drawn);
        }
        frames
    }

    #[test]
    fn every_box_carries_an_anchor() {
        let (_, laid) = compound();
        for drawn in &laid.boxes {
            assert!(laid.anchors.contains_key(&drawn.id), "{} has no anchor", drawn.label);
        }
    }

    #[test]
    fn a_greater_layer_draws_below_every_box_of_a_lesser_one_in_the_frame() {
        let (graph, laid) = compound();
        let mut compared = 0;
        for boxes in frames(&graph, &laid).values() {
            for above in boxes {
                for below in boxes {
                    if laid.anchors[&above.id].layer >= laid.anchors[&below.id].layer {
                        continue;
                    }
                    compared += 1;
                    assert!(
                        above.y + above.height < below.y,
                        "{} (layer {}) does not end above {} (layer {})",
                        above.label,
                        laid.anchors[&above.id].layer,
                        below.label,
                        laid.anchors[&below.id].layer,
                    );
                }
            }
        }
        assert!(compared > 0, "no frame spans two layers");
    }

    #[test]
    fn a_greater_slot_draws_right_of_a_lesser_one_in_the_same_layer() {
        let (graph, laid) = compound();
        let mut compared = 0;
        for boxes in frames(&graph, &laid).values() {
            for left in boxes {
                for right in boxes {
                    let (left_at, right_at) = (laid.anchors[&left.id], laid.anchors[&right.id]);
                    if left_at.layer != right_at.layer || left_at.slot >= right_at.slot {
                        continue;
                    }
                    compared += 1;
                    assert!(
                        left.x < right.x,
                        "{} (slot {}) does not draw left of {} (slot {})",
                        left.label,
                        left_at.slot,
                        right.label,
                        right_at.slot,
                    );
                }
            }
        }
        assert!(compared > 0, "no layer holds two boxes");
    }

    /// Holding a drawing to its own configuration is a fixed point. This says
    /// nothing about whether the configuration was read, since laying the same
    /// graph out twice repeats itself either way; it guards the pinned path
    /// against drifting away from the free one.
    #[test]
    fn a_layout_held_to_the_configuration_it_produced_reproduces_it() {
        let (graph, laid) = compound();
        let held = layout_pinned(&graph, &laid.anchors);
        assert_eq!(by_label(&graph, &held.anchors), by_label(&graph, &laid.anchors));
    }

    /// The drawing follows the configuration it is given rather than the one the
    /// free layout derives, which reproducing a configuration alone cannot show:
    /// laying the same graph out twice repeats itself whether or not the anchors
    /// are read.
    #[test]
    fn a_pair_pinned_the_other_way_round_comes_back_the_other_way_round() {
        let (graph, laid) = compound();
        let (one, other) = (labeled(&graph, "widget"), labeled(&graph, "much_longer_label"));
        let (one_at, other_at) = (laid.anchors[&one], laid.anchors[&other]);
        assert_eq!(one_at.layer, other_at.layer, "the fixture holds these two in one layer");

        let mut pinning = laid.anchors.clone();
        pinning.insert(one, other_at);
        pinning.insert(other, one_at);

        let held = layout_pinned(&graph, &pinning);
        assert_eq!((held.anchors[&one], held.anchors[&other]), (other_at, one_at));
    }

    /// The fixed point of the round trip holds for a graph whose frames have a
    /// cycle to break as well; like the acyclic round trip, it guards the pinned
    /// path rather than showing the configuration was read.
    #[test]
    fn a_layout_of_packages_in_a_cycle_reproduces_its_own_configuration() {
        let (graph, laid) = cyclic();
        let held = layout_pinned(&graph, &laid.anchors);
        assert_eq!(by_label(&graph, &held.anchors), by_label(&graph, &laid.anchors));
    }

    /// The layering follows the configuration it is given: a node pinned a layer
    /// past the frame-mate the free layout draws it beside comes back below it,
    /// which no layering derived from the graph alone would produce.
    #[test]
    fn a_node_pinned_a_layer_on_comes_back_below_the_mate_it_shared_one_with() {
        let (graph, laid) = compound();
        let (stays, moves) = (labeled(&graph, "widget"), labeled(&graph, "much_longer_label"));
        let moves_at = laid.anchors[&moves];
        let stays_at = laid.anchors[&stays];
        assert_eq!(stays_at.layer, moves_at.layer, "the fixture holds these two in one layer");

        let mut pinning = laid.anchors.clone();
        pinning.insert(moves, Anchor { layer: moves_at.layer + 1, ..moves_at });

        let held = layout_pinned(&graph, &pinning);
        assert!(
            held.anchors[&stays].layer < held.anchors[&moves].layer,
            "much_longer_label (layer {}) did not come back below widget (layer {})",
            held.anchors[&moves].layer,
            held.anchors[&stays].layer,
        );
    }

    /// The cycle breaking follows the configuration it is given: pinning the three
    /// packages in an order that makes a different edge of their cycle the
    /// backward one leaves every other edge running forward, so the packages come
    /// back in the pinned order. Breaking the cycle at the edge the graph alone
    /// suggests would leave one edge pointing against the pinning, and the
    /// layerer's feasibility repair would push its target past its source.
    #[test]
    fn a_cycle_pinned_to_break_at_another_edge_comes_back_in_the_pinned_order() {
        let (graph, laid) = cyclic();
        let (lib, front, back) =
            (labeled(&graph, "lib"), labeled(&graph, "front"), labeled(&graph, "back"));

        // The packages depend on one another lib -> front -> back -> lib; rotating
        // them to back, lib, front makes front -> back the edge that runs backward.
        let mut pinning = laid.anchors.clone();
        for (package, layer) in [(back, 0), (lib, 1), (front, 2)] {
            pinning.insert(package, Anchor { layer, ..laid.anchors[&package] });
        }

        let held = layout_pinned(&graph, &pinning);
        let (back_at, lib_at, front_at) =
            (held.anchors[&back], held.anchors[&lib], held.anchors[&front]);
        assert!(
            back_at.layer < lib_at.layer && lib_at.layer < front_at.layer,
            "the packages came back at layers back {}, lib {}, front {}",
            back_at.layer,
            lib_at.layer,
            front_at.layer,
        );
    }

    /// A frame none of whose members is pinned derives its own configuration, the
    /// same one the free layout gives it, while the frames around it are held to
    /// theirs. This is the frame a container just revealed heads: its members were
    /// in no earlier drawing, so a pinning has nothing to say about them.
    #[test]
    fn a_frame_with_no_member_pinned_is_laid_out_as_the_free_layout_lays_it_out() {
        let graph = unbalanced_graph();
        let free = layout(&graph);
        let revealed = labeled(&graph, "compute");

        let mut pinning = free.anchors.clone();
        for node in graph.nodes.iter().filter(|node| node.parent == Some(revealed)) {
            pinning.remove(&node.id);
        }

        let held = layout_pinned(&graph, &pinning);
        for node in graph.nodes.iter().filter(|node| node.parent == Some(revealed)) {
            assert_eq!(
                held.anchors[&node.id], free.anchors[&node.id],
                "{} moved in a frame nothing pinned",
                node.label,
            );
        }
    }

    #[test]
    fn an_unpinned_node_is_placed_and_leaves_the_pinned_order_intact() {
        let (graph, laid) = compound();
        let loose = labeled(&graph, "widget");
        let mut pinning = laid.anchors.clone();
        pinning.remove(&loose);

        let held = layout_pinned(&graph, &pinning);
        assert!(held.anchors.contains_key(&loose), "the unpinned node came back unplaced");

        let mut compared = 0;
        for boxes in frames(&graph, &laid).values() {
            for first in boxes {
                for second in boxes {
                    if first.id == second.id {
                        continue;
                    }
                    let (Some(&was_first), Some(&was_second)) =
                        (pinning.get(&first.id), pinning.get(&second.id))
                    else {
                        continue;
                    };
                    let (now_first, now_second) =
                        (held.anchors[&first.id], held.anchors[&second.id]);
                    compared += 1;
                    assert_eq!(
                        now_first.layer.cmp(&now_second.layer),
                        was_first.layer.cmp(&was_second.layer),
                        "{} (layer {} -> {}) and {} (layer {} -> {}) swapped layer order",
                        first.label,
                        was_first.layer,
                        now_first.layer,
                        second.label,
                        was_second.layer,
                        now_second.layer,
                    );
                    if was_first.layer == was_second.layer {
                        assert_eq!(
                            now_first.slot.cmp(&now_second.slot),
                            was_first.slot.cmp(&was_second.slot),
                            "{} (slot {} -> {}) and {} (slot {} -> {}) swapped slot order",
                            first.label,
                            was_first.slot,
                            now_first.slot,
                            second.label,
                            was_second.slot,
                            now_second.slot,
                        );
                    }
                }
            }
        }
        assert!(compared > 0, "no frame holds two pinned boxes");
    }
}
