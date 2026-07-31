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

use std::collections::{BTreeMap, HashMap};

use org_eclipse_elk_alg_layered::org::eclipse::elk::alg::layered::options::{
    GraphCompactionStrategy, LayeredOptions, NodePlacementStrategy,
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
pub(crate) const LABEL_BAND: f64 = 26.0;
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

#[derive(Default)]
pub struct LaidOut {
    /// Parent before child, so a renderer draws a container under its children.
    pub boxes: Vec<Box>,
    pub connectors: Vec<Connector>,
    /// Every connected node pair in the view before any edges were dropped for
    /// drawing (e.g. by transitive reduction), so hovering can still light up a
    /// node's full set of neighbors even where no connector is drawn to them.
    pub full_edges: Vec<(Id, Id)>,
    pub width: f64,
    pub height: f64,
}

pub fn layout(graph: &Graph) -> LaidOut {
    let children_of = children_by_parent(graph);
    let (root, elk_of) = build_elk_graph(graph, &children_of);
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
) -> (ElkNodeRef, HashMap<Id, ElkNodeRef>) {
    // Routing every edge through one hierarchy-spanning layered pass runs the
    // cross-container edges orthogonally between subgraphs.
    let root = layered_root();
    set_property(&root, CoreOptions::HIERARCHY_HANDLING, HierarchyHandling::IncludeChildren);

    let mut elk_of: HashMap<Id, ElkNodeRef> = HashMap::new();
    for node in &graph.nodes {
        if node.parent.is_none() {
            build_node(node, &root, children_of, &mut elk_of);
        }
    }
    for node in &graph.nodes {
        if children_of.contains_key(&node.id) {
            apply_options(&elk_of[&node.id]);
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
