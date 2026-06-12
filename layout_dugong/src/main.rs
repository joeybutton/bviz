//! Compound-graph layout via dugong (a Rust port of dagre).
//!
//! This entry point lays out a small nested graph and prints the resulting
//! geometry, confirming the layout engine is wired up. Bazel-graph ingest and
//! SVG emission build on top of it.

use dugong::graphlib::{Graph, GraphOptions};
use dugong::{EdgeLabel, GraphLabel, NodeLabel, layout_dagreish};

fn main() {
    let mut graph: Graph<NodeLabel, EdgeLabel, GraphLabel> = Graph::new(GraphOptions {
        compound: true,
        ..GraphOptions::default()
    });
    graph.set_graph(GraphLabel::default());

    // A container "parent" holding two leaves, plus a leaf at the root, wired
    // parent.a -> parent.b -> root.
    graph.set_node("parent", NodeLabel::default());
    for id in ["a", "b", "c"] {
        graph.set_node(
            id,
            NodeLabel {
                width: 60.0,
                height: 30.0,
                ..NodeLabel::default()
            },
        );
    }
    graph.set_parent("a", "parent");
    graph.set_parent("b", "parent");
    graph.set_edge("a", "b");
    graph.set_edge("b", "c");

    layout_dagreish(&mut graph);

    for id in ["parent", "a", "b", "c"] {
        let node = graph.node(id).expect("node present after layout");
        println!(
            "{id}: x={:?} y={:?} w={} h={}",
            node.x, node.y, node.width, node.height
        );
    }
    let edge_keys: Vec<_> = graph.edges().cloned().collect();
    for key in edge_keys {
        let edge = graph
            .edge(&key.v, &key.w, key.name.as_deref())
            .expect("edge present after layout");
        let points: Vec<(f64, f64)> = edge.points.iter().map(|p| (p.x, p.y)).collect();
        println!("{} -> {}: {:?}", key.v, key.w, points);
    }
}
