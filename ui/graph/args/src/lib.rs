//! The flags naming a joint build graph's inputs, and the documents they
//! resolve to. Nothing here opens a window or draws, so a parse can be
//! exercised without a display.

use clap::Args;
use std::path::PathBuf;

/// The files a joint build graph is read from.
#[derive(Args, Debug)]
#[command(next_help_heading = "Graph (one per invocation; each file given at most once)")]
pub struct GraphArgs {
    /// `bazel aquery --output=streamed_proto` file.
    #[arg(long, value_name = "FILE")]
    pub aquery: Option<PathBuf>,
    /// `bazel cquery --output=streamed_proto` file (run cquery with
    /// `--transitions=lite` for configuration-resolved edges).
    #[arg(long, value_name = "FILE")]
    pub cquery: Option<PathBuf>,
    /// `bazel query --output=streamed_proto` file.
    #[arg(long, value_name = "FILE")]
    pub query: Option<PathBuf>,
}

impl GraphArgs {
    /// The joint graph these files build, and `None` where none was given.
    pub fn source(self) -> Option<GraphSource> {
        let inputs: Vec<Input> = [
            (InputKind::Aquery, self.aquery),
            (InputKind::Cquery, self.cquery),
            (InputKind::Query, self.query),
        ]
        .into_iter()
        .filter_map(|(kind, path)| path.map(|path| Input { kind, path }))
        .collect();
        (!inputs.is_empty()).then_some(GraphSource { inputs })
    }
}

/// Which Bazel query produced a `--output=streamed_proto` file, selecting the
/// reader and builder used to turn it into a graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    Aquery,
    Cquery,
    Query,
}

/// A graph input: a streamed_proto file and the query kind that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Input {
    pub kind: InputKind,
    pub path: PathBuf,
}

/// One build graph, merged from every input given for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphSource {
    pub inputs: Vec<Input>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        graph: GraphArgs,
    }

    fn source(args: &[&str]) -> Option<GraphSource> {
        Cli::try_parse_from(std::iter::once("graph").chain(args.iter().copied()))
            .expect("the flags parse")
            .graph
            .source()
    }

    #[test]
    fn naming_no_query_file_names_no_graph() {
        assert_eq!(source(&[]), None);
    }

    /// Every query file given feeds the one graph, in a fixed order whatever
    /// order the flags came in.
    #[test]
    fn the_query_files_merge_into_a_single_graph() {
        let inputs = |paths: &[(InputKind, &str)]| {
            paths
                .iter()
                .map(|&(kind, path)| Input {
                    kind,
                    path: path.into(),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            source(&["--query", "q.pb", "--aquery", "a.pb", "--cquery", "c.pb"])
                .expect("three files name a graph")
                .inputs,
            inputs(&[
                (InputKind::Aquery, "a.pb"),
                (InputKind::Cquery, "c.pb"),
                (InputKind::Query, "q.pb"),
            ])
        );
    }

    /// One graph is drawn per invocation, so a second `--aquery` would replace
    /// the first rather than open a second graph. The help says so, since
    /// nothing in the parse can.
    #[test]
    fn the_help_says_a_graph_is_built_once_per_invocation() {
        let help = Cli::command().render_long_help().to_string();
        assert!(
            help.contains("one per invocation; each file given at most once"),
            "the help leaves the graph's arity unsaid: {help}"
        );
    }
}
