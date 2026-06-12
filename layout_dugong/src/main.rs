//! Renders a Bazel `aquery --output=streamed_proto` file as a compound graph.
//!
//! Usage: `layout_dugong <aquery.streamed_proto>`. The action graph is read into
//! the target -> configured-target -> action/artifact hierarchy; layout and SVG
//! emission build on top of it.

mod aquery;
mod graph;
mod streamed_proto;

use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: layout_dugong <aquery.streamed_proto>");
        return ExitCode::from(2);
    };

    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("error reading {path}: {err}");
            return ExitCode::FAILURE;
        }
    };

    let containers = match streamed_proto::read_action_graph_containers(&bytes) {
        Ok(containers) => containers,
        Err(err) => {
            eprintln!("error decoding aquery proto: {err}");
            return ExitCode::FAILURE;
        }
    };

    let graph = aquery::build(&containers, false);

    println!("{} nodes, {} edges", graph.nodes.len(), graph.edges.len());
    for node in &graph.nodes {
        let parent = node.parent.as_deref().unwrap_or("-");
        println!(
            "  [{}] {} ({}) parent={parent}",
            node.kind, node.id, node.label
        );
    }
    for edge in &graph.edges {
        println!("  {} -> {}", edge.from, edge.to);
    }

    ExitCode::SUCCESS
}
