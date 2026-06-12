//! Renders a Bazel `aquery --output=streamed_proto` file as a compound graph.
//!
//! Usage: `layout_dugong <aquery.streamed_proto>`. The action graph is read into
//! the target -> configured-target -> action/artifact hierarchy; layout and SVG
//! emission build on top of it.

mod aquery;
mod graph;
mod layout;
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
    let laid_out = layout::layout(&graph);

    println!(
        "{} boxes, {} connectors, {:.0}x{:.0}",
        laid_out.boxes.len(),
        laid_out.connectors.len(),
        laid_out.width,
        laid_out.height
    );
    for b in &laid_out.boxes {
        let kind = if b.compound { "container" } else { &b.kind };
        println!(
            "  [{kind}] {} @ ({:.0},{:.0}) {:.0}x{:.0}",
            b.label, b.x, b.y, b.width, b.height
        );
    }
    for c in &laid_out.connectors {
        println!("  edge {:?}", c.points);
    }

    ExitCode::SUCCESS
}
