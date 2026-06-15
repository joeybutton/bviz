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

use std::collections::HashMap;

use org_eclipse_elk_alg_layered::org::eclipse::elk::alg::layered::options::LayeredOptions;
use org_eclipse_elk_alg_layered::org::eclipse::elk::alg::layered::plain_java_initialization::initialize_plain_java_layout;
use org_eclipse_elk_core::org::eclipse::elk::core::math::{ElkPadding, KVector};
use org_eclipse_elk_core::org::eclipse::elk::core::options::{
    CoreOptions, Direction, HierarchyHandling, SizeConstraint,
};
use org_eclipse_elk_core::org::eclipse::elk::core::util::BasicProgressMonitor;
use org_eclipse_elk_core::org::eclipse::elk::core::{IGraphLayoutEngine, RecursiveGraphLayoutEngine};
use org_eclipse_elk_graph::org::eclipse::elk::graph::properties::Property;
use org_eclipse_elk_graph::org::eclipse::elk::graph::util::ElkGraphUtil;
use org_eclipse_elk_graph::org::eclipse::elk::graph::{
    ElkConnectableShapeRef, ElkEdgeRef, ElkNodeRef,
};

use crate::graph::{Graph, Node};

const LEAF_HEIGHT: f64 = 30.0;
const CHAR_WIDTH: f64 = 7.0;
const LABEL_PADDING: f64 = 16.0;
/// Horizontal space (in layout pixels, pre-zoom) reserved on an expandable leaf
/// box for its disclosure caret, left of the label.
pub const CARET_RESERVE: f64 = 12.0;
/// Inset reserved at the top of each container for its title; the other three
/// sides keep a uniform border.
const LABEL_BAND: f64 = 26.0;
const BORDER: f64 = 12.0;

/// A laid-out box in absolute coordinates, top-left origin.
pub struct Box {
    /// The graph node id this box renders, for hit-testing back to the node.
    pub id: String,
    pub label: String,
    pub kind: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// A container holds child boxes; its label sits in the top band.
    pub compound: bool,
}

/// An edge polyline: start, bend points, end, in order.
pub struct Connector {
    pub points: Vec<(f64, f64)>,
}

#[derive(Default)]
pub struct LaidOut {
    pub boxes: Vec<Box>,
    pub connectors: Vec<Connector>,
    pub width: f64,
    pub height: f64,
}

pub fn layout(graph: &Graph) -> LaidOut {
    initialize_plain_java_layout();

    // Children indexed by parent id; a node that appears as a key is a container.
    let mut children_of: HashMap<&str, Vec<&Node>> = HashMap::new();
    for node in &graph.nodes {
        if let Some(parent) = node.parent.as_deref() {
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
    set_property(&root, CoreOptions::SPACING_NODE_NODE, 24.0);
    set_property(&root, LayeredOptions::SPACING_NODE_NODE_BETWEEN_LAYERS, 50.0);

    // Build the ELK node tree, remembering each graph node's ELK node by id.
    let mut elk_of: HashMap<&str, ElkNodeRef> = HashMap::new();
    for node in &graph.nodes {
        if node.parent.is_none() {
            build_node(node, &root, &children_of, &mut elk_of);
        }
    }

    // create_simple_edge places each edge in the common container of its
    // endpoints, which is where ELK then routes and reports it.
    let mut edges: Vec<ElkEdgeRef> = Vec::with_capacity(graph.edges.len());
    for edge in &graph.edges {
        let (Some(from), Some(to)) = (
            elk_of.get(edge.from.as_str()),
            elk_of.get(edge.to.as_str()),
        ) else {
            continue;
        };
        edges.push(ElkGraphUtil::create_simple_edge(
            ElkConnectableShapeRef::Node(from.clone()),
            ElkConnectableShapeRef::Node(to.clone()),
        ));
    }

    let mut engine = RecursiveGraphLayoutEngine::new();
    let mut monitor = BasicProgressMonitor::new();
    engine.layout(&root, &mut monitor);

    // Containment depth per node, used only to draw parents before children.
    let parent_of: HashMap<&str, &str> = graph
        .nodes
        .iter()
        .filter_map(|node| node.parent.as_deref().map(|p| (node.id.as_str(), p)))
        .collect();
    let mut depth_of: HashMap<&str, usize> = HashMap::with_capacity(graph.nodes.len());
    for node in &graph.nodes {
        let mut depth = 0;
        let mut id = node.id.as_str();
        while let Some(&parent) = parent_of.get(id) {
            depth += 1;
            id = parent;
        }
        depth_of.insert(node.id.as_str(), depth);
    }

    let mut boxes: Vec<(usize, Box)> = Vec::with_capacity(graph.nodes.len());
    for node in &graph.nodes {
        let elk = &elk_of[node.id.as_str()];
        let (x, y) = absolute_origin(elk);
        let (width, height) = dimensions(elk);
        boxes.push((
            depth_of[node.id.as_str()],
            Box {
                id: node.id.clone(),
                label: node.label.clone(),
                kind: node.kind.clone(),
                x,
                y,
                width,
                height,
                compound: children_of.contains_key(node.id.as_str()),
            },
        ));
    }
    // Parent-before-child so a renderer can draw containers under their children.
    boxes.sort_by_key(|(depth, _)| *depth);
    let boxes: Vec<Box> = boxes.into_iter().map(|(_, b)| b).collect();

    let connectors = edges.iter().filter_map(connector).collect();

    let width = boxes.iter().map(|b| b.x + b.width).fold(0.0, f64::max);
    let height = boxes.iter().map(|b| b.y + b.height).fold(0.0, f64::max);

    LaidOut {
        boxes,
        connectors,
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
    children_of: &HashMap<&'g str, Vec<&'g Node>>,
    elk_of: &mut HashMap<&'g str, ElkNodeRef>,
) {
    let elk = ElkGraphUtil::create_node(Some(parent.clone()));
    match children_of.get(node.id.as_str()) {
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
            // A collapsible leaf (a collapsed group) reserves room for its
            // disclosure caret left of the label, matching the renderer's caret
            // footprint.
            let caret = if node.id.starts_with("group:") {
                CARET_RESERVE
            } else {
                0.0
            };
            let width = node.label.chars().count() as f64 * CHAR_WIDTH + LABEL_PADDING + caret;
            elk.borrow_mut()
                .connectable()
                .shape()
                .set_dimensions(width, LEAF_HEIGHT);
        }
    }
    elk_of.insert(node.id.as_str(), elk);
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
fn connector(edge: &ElkEdgeRef) -> Option<Connector> {
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
    (points.len() >= 2).then_some(Connector { points })
}
