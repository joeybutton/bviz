//! Lays out the compound graph with elk-rs (a pure-Rust port of the Eclipse
//! Layout Kernel) and collects absolute geometry ready to render.
//!
//! The graph is serialized to ELK's JSON format: nested `children` arrays mirror
//! the containment forest, and every edge is declared on the root, where the
//! layered algorithm's `INCLUDE_CHILDREN` hierarchy handling routes it across
//! nesting levels. Leaf nodes are sized from their label length; container nodes
//! are sized by ELK to fit their children, with extra top padding reserved for
//! the title band. Shape and edge coordinates are requested in the root frame,
//! so positions and edge bend points come back already absolute.

use std::collections::HashMap;

use org_eclipse_elk_graph_json::org::eclipse::elk::graph::json::layout_api::layout_json;
use serde_json::{Map, Value, json};

use crate::graph::{Graph, Node};

const LEAF_HEIGHT: f64 = 30.0;
const CHAR_WIDTH: f64 = 7.0;
const LABEL_PADDING: f64 = 16.0;
/// Inset reserved at the top of each container for its title; the other three
/// sides keep a uniform border.
const LABEL_BAND: f64 = 26.0;
const BORDER: f64 = 12.0;

/// A laid-out box in absolute coordinates, top-left origin.
pub struct Box {
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
    // Children indexed by parent id; a node that appears as a key is a container.
    let mut children_of: HashMap<&str, Vec<&Node>> = HashMap::new();
    let mut roots: Vec<&Node> = Vec::new();
    for node in &graph.nodes {
        match node.parent.as_deref() {
            Some(parent) => children_of.entry(parent).or_default().push(node),
            None => roots.push(node),
        }
    }

    let children: Vec<Value> = roots.iter().map(|n| node_json(n, &children_of)).collect();
    let edges: Vec<Value> = graph
        .edges
        .iter()
        .enumerate()
        .map(|(i, e)| json!({"id": format!("e{i}"), "sources": [e.from], "targets": [e.to]}))
        .collect();
    let graph_json = json!({"id": "root", "children": children, "edges": edges});

    // Lay the dependency flow out top-to-bottom and route every edge through the
    // single hierarchy-spanning layered pass, so a node sits below the inputs it
    // consumes and cross-container edges run orthogonally between subgraphs.
    // Coordinates come back in the root frame, hence already absolute.
    let options = json!({
        "org.eclipse.elk.algorithm": "org.eclipse.elk.layered",
        "org.eclipse.elk.direction": "DOWN",
        "org.eclipse.elk.hierarchyHandling": "INCLUDE_CHILDREN",
        "org.eclipse.elk.spacing.nodeNode": "24",
        "org.eclipse.elk.layered.spacing.nodeNodeBetweenLayers": "50",
        "org.eclipse.elk.json.shapeCoords": "ROOT",
        "org.eclipse.elk.json.edgeCoords": "ROOT",
    });

    let result = layout_json(&graph_json.to_string(), &options.to_string())
        .unwrap_or_else(|err| panic!("elk layout failed: {err}"));
    let laid: Value =
        serde_json::from_str(&result).expect("elk returned well-formed JSON it just produced");

    let mut geometry: HashMap<String, Rect> = HashMap::new();
    collect_nodes(&laid, &mut geometry);
    let mut connectors = Vec::new();
    collect_edges(&laid, &mut connectors);

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
        let rect = geometry
            .get(&node.id)
            .copied()
            .expect("every node is positioned after layout");
        boxes.push((
            depth_of[node.id.as_str()],
            Box {
                label: node.label.clone(),
                kind: node.kind.clone(),
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
                compound: children_of.contains_key(node.id.as_str()),
            },
        ));
    }
    // Parent-before-child so a renderer can draw containers under their children.
    boxes.sort_by_key(|(depth, _)| *depth);
    let boxes: Vec<Box> = boxes.into_iter().map(|(_, b)| b).collect();

    let width = boxes.iter().map(|b| b.x + b.width).fold(0.0, f64::max);
    let height = boxes.iter().map(|b| b.y + b.height).fold(0.0, f64::max);

    LaidOut {
        boxes,
        connectors,
        width,
        height,
    }
}

/// Serializes a node and, recursively, its subtree as an ELK JSON node. Leaves
/// carry an explicit size; containers are left unsized for ELK to fit around
/// their children and get extra top padding to clear the title band.
fn node_json(node: &Node, children_of: &HashMap<&str, Vec<&Node>>) -> Value {
    let mut obj = Map::new();
    obj.insert("id".into(), json!(node.id));
    match children_of.get(node.id.as_str()) {
        Some(kids) => {
            obj.insert(
                "layoutOptions".into(),
                json!({"org.eclipse.elk.padding":
                    format!("[top={LABEL_BAND},left={BORDER},bottom={BORDER},right={BORDER}]")}),
            );
            let children: Vec<Value> = kids.iter().map(|n| node_json(n, children_of)).collect();
            obj.insert("children".into(), Value::Array(children));
        }
        None => {
            obj.insert(
                "width".into(),
                json!(node.label.chars().count() as f64 * CHAR_WIDTH + LABEL_PADDING),
            );
            obj.insert("height".into(), json!(LEAF_HEIGHT));
        }
    }
    Value::Object(obj)
}

#[derive(Clone, Copy)]
struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

/// Records each node's laid-out rectangle, descending through `children`.
fn collect_nodes(node: &Value, out: &mut HashMap<String, Rect>) {
    if let (Some(id), Some(width), Some(height)) = (
        node.get("id").and_then(Value::as_str),
        node.get("width").and_then(Value::as_f64),
        node.get("height").and_then(Value::as_f64),
    ) {
        out.insert(
            id.to_string(),
            Rect {
                x: node.get("x").and_then(Value::as_f64).unwrap_or(0.0),
                y: node.get("y").and_then(Value::as_f64).unwrap_or(0.0),
                width,
                height,
            },
        );
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            collect_nodes(child, out);
        }
    }
}

/// Collects every edge's routing polyline, descending through `children`. An
/// ELK edge carries one or more sections, each a start point, optional bend
/// points, and an end point.
fn collect_edges(node: &Value, out: &mut Vec<Connector>) {
    if let Some(edges) = node.get("edges").and_then(Value::as_array) {
        for edge in edges {
            let Some(sections) = edge.get("sections").and_then(Value::as_array) else {
                continue;
            };
            for section in sections {
                let mut points = Vec::new();
                points.extend(point(section.get("startPoint")));
                if let Some(bends) = section.get("bendPoints").and_then(Value::as_array) {
                    points.extend(bends.iter().filter_map(|b| point(Some(b))));
                }
                points.extend(point(section.get("endPoint")));
                if points.len() >= 2 {
                    out.push(Connector { points });
                }
            }
        }
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            collect_edges(child, out);
        }
    }
}

fn point(value: Option<&Value>) -> Option<(f64, f64)> {
    let value = value?;
    Some((
        value.get("x").and_then(Value::as_f64)?,
        value.get("y").and_then(Value::as_f64)?,
    ))
}
