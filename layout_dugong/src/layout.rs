//! Lays out the compound graph with dugong (a Rust port of dagre) and collects
//! absolute geometry ready to render.
//!
//! Leaf nodes are sized from their label length; container nodes are left at
//! zero size for dugong to expand around their children. Layout goes through
//! `layout_dagreish`, whose border-segment pass gives each container node its
//! bounds. dugong reports node positions as centers, which are converted to
//! top-left corners here; edge points are already absolute.

use std::collections::{HashMap, HashSet};

use dugong::graphlib::{Graph as DugongGraph, GraphOptions};
use dugong::{EdgeLabel, GraphLabel, NodeLabel, RankDir, layout_dagreish};

// Lay the dependency flow out left-to-right: each action chain reads along the
// rank direction, so a node sits next to the inputs it consumes, and the
// independent per-target subgraphs stack down the cross axis rather than
// spreading the whole graph horizontally. The separations are tightened from
// dagre's defaults to keep the cross-target edges that bundle between subgraphs
// readable without inflating the empty space around them.
fn graph_label() -> GraphLabel {
    GraphLabel {
        rankdir: RankDir::LR,
        ranksep: 60.0,
        nodesep: 24.0,
        edgesep: 12.0,
        ..GraphLabel::default()
    }
}

use crate::graph::Graph;

const LEAF_HEIGHT: f64 = 30.0;
const CHAR_WIDTH: f64 = 7.0;
const LABEL_PADDING: f64 = 16.0;

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
    let containers: HashSet<&str> = graph
        .nodes
        .iter()
        .filter_map(|node| node.parent.as_deref())
        .collect();

    let mut dugong: DugongGraph<NodeLabel, EdgeLabel, GraphLabel> =
        DugongGraph::new(GraphOptions {
            compound: true,
            ..GraphOptions::default()
        });
    dugong.set_graph(graph_label());

    for node in &graph.nodes {
        let label = if containers.contains(node.id.as_str()) {
            // Sized by dugong to fit its children.
            NodeLabel::default()
        } else {
            NodeLabel {
                width: node.label.chars().count() as f64 * CHAR_WIDTH + LABEL_PADDING,
                height: LEAF_HEIGHT,
                ..NodeLabel::default()
            }
        };
        dugong.set_node(node.id.clone(), label);
    }
    for node in &graph.nodes {
        if let Some(parent) = &node.parent {
            dugong.set_parent(node.id.clone(), parent.clone());
        }
    }
    for edge in &graph.edges {
        dugong.set_edge(edge.from.clone(), edge.to.clone());
    }

    layout_dagreish(&mut dugong);

    let depth_of = depths(graph);
    let mut boxes: Vec<(usize, Box)> = Vec::with_capacity(graph.nodes.len());
    for node in &graph.nodes {
        let laid = dugong.node(&node.id).expect("node present after layout");
        let (width, height) = (laid.width, laid.height);
        let cx = laid.x.unwrap_or(0.0);
        let cy = laid.y.unwrap_or(0.0);
        boxes.push((
            depth_of[node.id.as_str()],
            Box {
                label: node.label.clone(),
                kind: node.kind.clone(),
                x: cx - width / 2.0,
                y: cy - height / 2.0,
                width,
                height,
                compound: containers.contains(node.id.as_str()),
            },
        ));
    }
    // Parent-before-child so a renderer can draw containers under their children.
    boxes.sort_by_key(|(depth, _)| *depth);
    let boxes: Vec<Box> = boxes.into_iter().map(|(_, b)| b).collect();

    let mut connectors = Vec::new();
    let edge_keys: Vec<_> = dugong.edges().cloned().collect();
    for key in edge_keys {
        let edge = dugong
            .edge(&key.v, &key.w, key.name.as_deref())
            .expect("edge present after layout");
        connectors.push(Connector {
            points: edge.points.iter().map(|p| (p.x, p.y)).collect(),
        });
    }

    let width = boxes.iter().map(|b| b.x + b.width).fold(0.0, f64::max);
    let height = boxes.iter().map(|b| b.y + b.height).fold(0.0, f64::max);

    LaidOut {
        boxes,
        connectors,
        width,
        height,
    }
}

/// Containment depth of each node id (a root is 0).
fn depths(graph: &Graph) -> HashMap<&str, usize> {
    let parent_of: HashMap<&str, &str> = graph
        .nodes
        .iter()
        .filter_map(|node| node.parent.as_deref().map(|p| (node.id.as_str(), p)))
        .collect();
    let mut depth_of = HashMap::with_capacity(graph.nodes.len());
    for node in &graph.nodes {
        let mut depth = 0;
        let mut id = node.id.as_str();
        while let Some(&parent) = parent_of.get(id) {
            depth += 1;
            id = parent;
        }
        depth_of.insert(node.id.as_str(), depth);
    }
    depth_of
}
