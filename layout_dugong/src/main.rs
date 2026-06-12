//! Renders a Bazel `aquery --output=streamed_proto` file as an SVG compound
//! graph.
//!
//! Usage: `layout_dugong <aquery.streamed_proto> [out.svg]`. The action graph is
//! read into the target -> configured-target -> action/artifact hierarchy, laid
//! out with dugong, and emitted as SVG (to the output file, or stdout).

mod aquery;
mod graph;
mod layout;
mod streamed_proto;
mod svg;

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(input) = args.next() else {
        eprintln!("usage: layout_dugong <aquery.streamed_proto> [out.svg]");
        return ExitCode::from(2);
    };
    let output = args.next();

    let bytes = match std::fs::read(&input) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("error reading {input}: {err}");
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
    let document = svg::emit(&laid_out);

    match output {
        Some(path) => {
            if let Err(err) = std::fs::write(&path, document) {
                eprintln!("error writing {path}: {err}");
                return ExitCode::FAILURE;
            }
        }
        None => print!("{document}"),
    }

    ExitCode::SUCCESS
}
