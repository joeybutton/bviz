//! The command line of the window that shows every kind of Bazel build
//! document: the three viewers' flag fragments joined into one parser, and the
//! documents a command line resolves to. It names no window and draws nothing,
//! so a parse can be exercised without a display.

use bviz_ui_console_args::{ConsoleArgs, ConsoleSource};
use bviz_ui_graph_args::{GraphArgs, GraphSource};
use bviz_ui_trace_args::{TraceArgs, TraceSource};
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};

/// Opens Bazel build documents: the dependency graph, the action timeline, the
/// console replay.
///
/// Every document named on the command line takes a region of the one window,
/// the regions stacked one above another.
#[derive(Parser, Debug)]
#[command(name = "bviz", bin_name = "bviz", version, about)]
pub struct Cli {
    #[command(flatten)]
    pub graph: GraphArgs,
    #[command(flatten)]
    pub trace: TraceArgs,
    #[command(flatten)]
    pub console: ConsoleArgs,
}

/// The flags of [`Cli`] that name a document, for the command line that gives
/// none.
const SOURCE_FLAGS: &str = "--aquery, --cquery, --query, --profile or --bep";

impl Cli {
    /// The documents this command line names, graph first and each repeatable
    /// flag's in the order given.
    ///
    /// The rejection of a command line naming none is written out here rather
    /// than left to a clap argument group, because these fragments are shared
    /// with the per-viewer binaries, where a different flag is the required one.
    pub fn sources(self) -> Result<Sources, clap::Error> {
        let mut files = Vec::with_capacity(self.trace.profile.len() + self.console.bep.len());
        files.extend(self.trace.sources().map(Source::Trace));
        files.extend(self.console.sources().map(Source::Console));
        if let Some(graph) = self.graph.source() {
            return Ok(Sources::Graph { graph, files });
        }
        let mut files = files.into_iter();
        let Some(first) = files.next() else {
            return Err(Self::command().error(
                ErrorKind::MissingRequiredArgument,
                format!("one of {SOURCE_FLAGS} is required"),
            ));
        };
        Ok(Sources::Files {
            first,
            rest: files.collect(),
        })
    }
}

/// A document read from one file: a profile or a build event stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Trace(TraceSource),
    Console(ConsoleSource),
}

/// The documents a command line names, in the order a window shows them: the
/// one graph its graph flags describe, where it gives any, then each repeatable
/// flag's in the order given. Either way it names at least one: a window with
/// no document to show is a usage mistake, refused before any window opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sources {
    Graph {
        graph: GraphSource,
        files: Vec<Source>,
    },
    Files {
        first: Source,
        rest: Vec<Source>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_ui_graph_args::{Input, InputKind};

    fn sources(args: &[&str]) -> Result<Sources, clap::Error> {
        Cli::try_parse_from(std::iter::once("bviz").chain(args.iter().copied()))?.sources()
    }

    fn graph(inputs: &[(InputKind, &str)], files: Vec<Source>) -> Sources {
        let inputs = inputs
            .iter()
            .map(|&(kind, path)| Input {
                kind,
                path: path.into(),
            })
            .collect();
        Sources::Graph {
            graph: GraphSource::new(inputs).expect("the inputs name a graph"),
            files,
        }
    }

    fn files(first: Source, rest: Vec<Source>) -> Sources {
        Sources::Files { first, rest }
    }

    fn trace(path: &str) -> Source {
        Source::Trace(TraceSource { path: path.into() })
    }

    fn console(path: &str) -> Source {
        Source::Console(ConsoleSource { path: path.into() })
    }

    /// A window with nothing to show is refused, in the terms clap refuses a
    /// missing required argument — which is what fixes the exit code the shell
    /// sees.
    #[test]
    fn a_command_line_naming_no_document_is_a_usage_error() {
        let err = sources(&[]).expect_err("no document was named");
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
        assert!(
            err.to_string().contains(SOURCE_FLAGS),
            "the error names no flag to give: {err}"
        );
    }

    #[test]
    fn each_flag_names_a_document_of_its_own_kind() {
        assert_eq!(
            sources(&["--aquery", "a.pb"]).unwrap(),
            graph(&[(InputKind::Aquery, "a.pb")], Vec::new())
        );
        assert_eq!(
            sources(&["--cquery", "c.pb"]).unwrap(),
            graph(&[(InputKind::Cquery, "c.pb")], Vec::new())
        );
        assert_eq!(
            sources(&["--query", "q.pb"]).unwrap(),
            graph(&[(InputKind::Query, "q.pb")], Vec::new())
        );
        assert_eq!(
            sources(&["--profile", "p.gz"]).unwrap(),
            files(trace("p.gz"), Vec::new())
        );
        assert_eq!(
            sources(&["--bep", "b.bep"]).unwrap(),
            files(console("b.bep"), Vec::new())
        );
        assert_eq!(
            sources(&["--profile", "one.gz", "--profile", "two.gz"]).unwrap(),
            files(trace("one.gz"), vec![trace("two.gz")])
        );
    }

    /// Documents of every kind open together, the graph ahead of the rest
    /// wherever its flags stand.
    #[test]
    fn a_graph_a_profile_and_a_bep_are_three_documents() {
        let named = sources(&["--profile", "p.gz", "--bep", "b.bep", "--aquery", "a.pb"]).unwrap();
        assert_eq!(
            named,
            graph(
                &[(InputKind::Aquery, "a.pb")],
                vec![trace("p.gz"), console("b.bep")]
            )
        );
    }

    /// The flags are the surface users' scripts are written against, so their
    /// names and the file placeholder they take are pinned here.
    #[test]
    fn the_flags_keep_their_names() {
        let help = Cli::command().render_long_help().to_string();
        for flag in ["--aquery", "--cquery", "--query", "--profile", "--bep"] {
            assert!(
                help.contains(&format!("{flag} <FILE>")),
                "no {flag}: {help}"
            );
        }
    }
}
