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
    CoreOptions, Direction, EdgeRouting as ElkEdgeRouting, HierarchyHandling, SizeConstraint,
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

const LEAF_HEIGHT: f64 = 30.0;
const CHAR_WIDTH: f64 = 7.0;
const LABEL_PADDING: f64 = 22.0;
/// Horizontal space (in layout pixels, pre-zoom) reserved on an expandable leaf
/// box for its disclosure caret, left of the label.
pub const CARET_RESERVE: f64 = 12.0;
/// Inset reserved at the top of each container for its title; the other three
/// sides keep a uniform border.
const LABEL_BAND: f64 = 26.0;
const BORDER: f64 = 12.0;

/// How the layered algorithm draws edges between nodes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EdgeRouting {
    Orthogonal,
    Polyline,
    Splines,
}

/// How the layered algorithm positions nodes within their layer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NodePlacement {
    BrandesKoepf,
    NetworkSimplex,
    LinearSegments,
    Simple,
}

/// The post-layout horizontal compaction pass, if any.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PostCompaction {
    None,
    Left,
    Right,
    EdgeLength,
}

/// The ELK layout options the layout exposes for tuning how tightly edges and
/// nodes pack. [`Tuning::default`] holds the values the layout otherwise bakes
/// in; the `SCALARS`/`CHOICES` specs describe each field for a control panel,
/// including the stock ELK default to mark against.
///
/// The edge-spacing fields are the layered algorithm's between-layers spacings,
/// which govern the routing channels between adjacent layers — the space dense
/// edges consume. The generic `spacing.edgeEdge`/`spacing.edgeNode` options have
/// no effect on a layered layout in elk-rs, so they are not exposed.
#[derive(Clone)]
pub struct Tuning {
    pub spacing_node_node: f64,
    pub node_node_between_layers: f64,
    pub edge_edge_between_layers: f64,
    pub edge_node_between_layers: f64,
    pub edge_routing: EdgeRouting,
    pub node_placement: NodePlacement,
    pub post_compaction: PostCompaction,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            spacing_node_node: 24.0,
            node_node_between_layers: 50.0,
            edge_edge_between_layers: 10.0,
            edge_node_between_layers: 10.0,
            edge_routing: EdgeRouting::Orthogonal,
            node_placement: NodePlacement::BrandesKoepf,
            post_compaction: PostCompaction::None,
        }
    }
}

/// A continuous [`Tuning`] field, described for a slider.
pub struct ScalarSpec {
    pub label: &'static str,
    pub min: f64,
    pub max: f64,
    /// ELK's own default for this option, to mark on the slider.
    pub elk_default: f64,
}

/// A discrete [`Tuning`] field, described for a dropdown.
pub struct ChoiceSpec {
    pub label: &'static str,
    pub options: &'static [&'static str],
    /// Index into `options` of ELK's own default, to mark in the menu.
    pub elk_default: usize,
}

impl Tuning {
    /// The scalar fields, ordered to match [`Tuning::scalar`]/[`Tuning::set_scalar`].
    pub const SCALARS: &'static [ScalarSpec] = &[
        ScalarSpec { label: "node spacing", min: 0.0, max: 80.0, elk_default: 20.0 },
        ScalarSpec { label: "layer spacing", min: 0.0, max: 120.0, elk_default: 20.0 },
        ScalarSpec { label: "edge–edge", min: 2.0, max: 30.0, elk_default: 10.0 },
        ScalarSpec { label: "edge–node", min: 2.0, max: 30.0, elk_default: 10.0 },
    ];

    /// The discrete fields, ordered to match [`Tuning::choice`]/[`Tuning::set_choice`].
    pub const CHOICES: &'static [ChoiceSpec] = &[
        ChoiceSpec {
            label: "routing",
            options: &["orthogonal", "polyline", "splines"],
            elk_default: 0,
        },
        ChoiceSpec {
            label: "placement",
            options: &["brandes-köpf", "network-simplex", "linear-segments", "simple"],
            elk_default: 0,
        },
        ChoiceSpec {
            label: "compaction",
            options: &["none", "left", "right", "edge-length"],
            elk_default: 0,
        },
    ];

    pub fn scalar(&self, i: usize) -> f64 {
        match i {
            0 => self.spacing_node_node,
            1 => self.node_node_between_layers,
            2 => self.edge_edge_between_layers,
            3 => self.edge_node_between_layers,
            _ => unreachable!(),
        }
    }

    pub fn set_scalar(&mut self, i: usize, v: f64) {
        match i {
            0 => self.spacing_node_node = v,
            1 => self.node_node_between_layers = v,
            2 => self.edge_edge_between_layers = v,
            3 => self.edge_node_between_layers = v,
            _ => unreachable!(),
        }
    }

    pub fn choice(&self, i: usize) -> usize {
        match i {
            0 => self.edge_routing as usize,
            1 => self.node_placement as usize,
            2 => self.post_compaction as usize,
            _ => unreachable!(),
        }
    }

    pub fn set_choice(&mut self, i: usize, v: usize) {
        match (i, v) {
            (0, 0) => self.edge_routing = EdgeRouting::Orthogonal,
            (0, 1) => self.edge_routing = EdgeRouting::Polyline,
            (0, 2) => self.edge_routing = EdgeRouting::Splines,
            (1, 0) => self.node_placement = NodePlacement::BrandesKoepf,
            (1, 1) => self.node_placement = NodePlacement::NetworkSimplex,
            (1, 2) => self.node_placement = NodePlacement::LinearSegments,
            (1, 3) => self.node_placement = NodePlacement::Simple,
            (2, 0) => self.post_compaction = PostCompaction::None,
            (2, 1) => self.post_compaction = PostCompaction::Left,
            (2, 2) => self.post_compaction = PostCompaction::Right,
            (2, 3) => self.post_compaction = PostCompaction::EdgeLength,
            _ => unreachable!(),
        }
    }
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
    pub boxes: Vec<Box>,
    pub connectors: Vec<Connector>,
    /// Every connected node pair in the view before any edges were dropped for
    /// drawing (e.g. by transitive reduction), so hovering can still light up a
    /// node's full set of neighbors even where no connector is drawn to them.
    pub full_edges: Vec<(Id, Id)>,
    pub width: f64,
    pub height: f64,
}

pub fn layout(graph: &Graph, tuning: &Tuning) -> LaidOut {
    initialize_plain_java_layout();

    // Children indexed by parent id; a node that appears as a key is a container.
    let mut children_of: HashMap<Id, Vec<&Node>> = HashMap::new();
    for node in &graph.nodes {
        if let Some(parent) = node.parent {
            children_of.entry(parent).or_default().push(node);
        }
    }

    // Lay the dependency flow out top-to-bottom and route every edge through the
    // single hierarchy-spanning layered pass, so a node sits below the inputs it
    // consumes and cross-container edges run orthogonally between subgraphs.
    let root = ElkGraphUtil::create_graph();
    set_property(&root, CoreOptions::ALGORITHM, "org.eclipse.elk.layered".to_string());
    set_property(&root, CoreOptions::DIRECTION, Direction::Down);
    set_property(&root, CoreOptions::HIERARCHY_HANDLING, HierarchyHandling::IncludeChildren);
    apply_tuning(&root, tuning);

    // Build the ELK node tree, remembering each graph node's ELK node by id.
    let mut elk_of: HashMap<Id, ElkNodeRef> = HashMap::new();
    for node in &graph.nodes {
        if node.parent.is_none() {
            build_node(node, &root, &children_of, &mut elk_of);
        }
    }
    // INCLUDE_CHILDREN lays each container out as its own nested graph that reads
    // spacing and edge routing from that container's own properties, so the tuning
    // is set on every container, not only the root, to reach the leaves inside it.
    for node in &graph.nodes {
        if children_of.contains_key(&node.id) {
            apply_tuning(&elk_of[&node.id], tuning);
        }
    }

    // Route one edge per ordered node pair, not one per provenance: the parallel
    // query/cquery/aquery edges between the same two boxes would each cost ELK a
    // full route, and that edge count is what dominates layout time on a dense
    // view. Collect the distinct kinds per pair (keyed in canonical id order) so
    // the renderer can still draw each along the shared route.
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
        set_property(&root, LayeredOptions::THOROUGHNESS, 1);
    }
    // create_simple_edge places each edge in the common container of its
    // endpoints, which is where ELK then routes and reports it.
    let mut edges: Vec<(ElkEdgeRef, Vec<EdgeKind>, Id, Id)> = Vec::with_capacity(kinds_of.len());
    for ((from_id, to_id), mut kinds) in kinds_of {
        kinds.sort_unstable();
        let (from, to) = (&elk_of[&from_id], &elk_of[&to_id]);
        edges.push((
            ElkGraphUtil::create_simple_edge(
                ElkConnectableShapeRef::Node(from.clone()),
                ElkConnectableShapeRef::Node(to.clone()),
            ),
            kinds,
            from_id,
            to_id,
        ));
    }

    let mut engine = RecursiveGraphLayoutEngine::new();
    let mut monitor = BasicProgressMonitor::new();
    engine.layout(&root, &mut monitor);

    // Containment depth per node, used only to draw parents before children.
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
    // Parent-before-child so a renderer can draw containers under their children.
    boxes.sort_by_key(|(depth, _)| *depth);
    let boxes: Vec<Box> = boxes.into_iter().map(|(_, b)| b).collect();

    let connectors = edges
        .into_iter()
        .filter_map(|(edge, kinds, from, to)| {
            connector(&edge).map(|points| Connector {
                points,
                kinds,
                from,
                to,
            })
        })
        .collect();

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
            // ELK sizes a container to its children; widen the floor to the
            // bold, centered title so a title wider than the children does not
            // overflow the box. The extra 2*BORDER keeps it clear of the
            // rounded corners and border.
            //
            // The layered algorithm always lays out left-to-right internally and
            // rotates the result into DIRECTION; for the Down direction used here
            // the container's minimum size is consumed in that pre-rotation frame
            // and never transposed back, so the title width goes in y to land on
            // the box's output width.
            let title_width =
                node.label.chars().count() as f64 * CHAR_WIDTH + LABEL_PADDING + 2.0 * BORDER;
            set_property(
                &elk,
                CoreOptions::NODE_SIZE_CONSTRAINTS,
                SizeConstraint::minimum_size(),
            );
            set_property(
                &elk,
                CoreOptions::NODE_SIZE_MINIMUM,
                KVector { x: 0.0, y: title_width },
            );
            for kid in kids {
                build_node(kid, &elk, children_of, elk_of);
            }
        }
        None => {
            // Every leaf reserves room for a disclosure caret left of the label:
            // the renderer draws one on any expandable box (a collapsed container
            // or group), and the uniform reserve also keeps plain leaves from
            // crowding their labels.
            let width =
                node.label.chars().count() as f64 * CHAR_WIDTH + LABEL_PADDING + CARET_RESERVE;
            elk.borrow_mut()
                .connectable()
                .shape()
                .set_dimensions(width, LEAF_HEIGHT);
        }
    }
    elk_of.insert(node.id, elk);
}

fn apply_tuning(root: &ElkNodeRef, t: &Tuning) {
    set_property(root, CoreOptions::SPACING_NODE_NODE, t.spacing_node_node);
    set_property(
        root,
        LayeredOptions::SPACING_NODE_NODE_BETWEEN_LAYERS,
        t.node_node_between_layers,
    );
    set_property(
        root,
        LayeredOptions::SPACING_EDGE_EDGE_BETWEEN_LAYERS,
        t.edge_edge_between_layers,
    );
    set_property(
        root,
        LayeredOptions::SPACING_EDGE_NODE_BETWEEN_LAYERS,
        t.edge_node_between_layers,
    );
    set_property(
        root,
        CoreOptions::EDGE_ROUTING,
        match t.edge_routing {
            EdgeRouting::Orthogonal => ElkEdgeRouting::Orthogonal,
            EdgeRouting::Polyline => ElkEdgeRouting::Polyline,
            EdgeRouting::Splines => ElkEdgeRouting::Splines,
        },
    );
    set_property(
        root,
        LayeredOptions::NODE_PLACEMENT_STRATEGY,
        match t.node_placement {
            NodePlacement::BrandesKoepf => NodePlacementStrategy::BrandesKoepf,
            NodePlacement::NetworkSimplex => NodePlacementStrategy::NetworkSimplex,
            NodePlacement::LinearSegments => NodePlacementStrategy::LinearSegments,
            NodePlacement::Simple => NodePlacementStrategy::Simple,
        },
    );
    set_property(
        root,
        LayeredOptions::COMPACTION_POST_COMPACTION_STRATEGY,
        match t.post_compaction {
            PostCompaction::None => GraphCompactionStrategy::None,
            PostCompaction::Left => GraphCompactionStrategy::Left,
            PostCompaction::Right => GraphCompactionStrategy::Right,
            PostCompaction::EdgeLength => GraphCompactionStrategy::EdgeLength,
        },
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

/// A node's absolute top-left: its parent-relative position plus the absolute
/// origin of its parent, accumulated up to the root.
fn absolute_origin(node: &ElkNodeRef) -> (f64, f64) {
    let (x, y) = {
        let mut node = node.borrow_mut();
        let shape = node.connectable().shape();
        (shape.x(), shape.y())
    };
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
