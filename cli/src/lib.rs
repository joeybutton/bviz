//! The command line of the window that shows every kind of Bazel build
//! document: the three viewers' flag fragments joined into one parser, and the
//! document an invocation resolves to. It names no window and draws nothing,
//! so a parse can be exercised without a display.

use bviz_ui_graph_args::{GraphArgs, GraphSource};
use bviz_ui_terminal_args::{ConsoleArgs, ConsoleSource};
use bviz_ui_trace_args::{TraceArgs, TraceSource};
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};

/// Opens Bazel build documents: the dependency graph, the action timeline, the
/// console replay.
///
/// A window shows one document, so an invocation names one.
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

/// The flags of [`Cli`] that name a document, for the invocation that gives none.
const SOURCE_FLAGS: &str = "--aquery, --cquery, --query, --profile or --bep";

impl Cli {
    /// The document this invocation names.
    ///
    /// The rejection of an invocation naming none is written out here rather
    /// than left to a clap argument group, because these fragments are shared
    /// with the per-viewer binaries, where a different flag is the required one.
    pub fn source(self) -> Result<Source, clap::Error> {
        let mut named = self
            .graph
            .source()
            .map(Source::Graph)
            .into_iter()
            .chain(self.trace.sources().map(Source::Trace))
            .chain(self.console.sources().map(Source::Console));
        let Some(source) = named.next() else {
            return Err(Self::command().error(
                ErrorKind::MissingRequiredArgument,
                format!("one of {SOURCE_FLAGS} is required"),
            ));
        };
        if named.next().is_some() {
            return Err(Self::command().error(
                ErrorKind::TooManyValues,
                "a window shows one document, so one is opened per invocation",
            ));
        }
        Ok(source)
    }
}

/// A document to open, whichever viewer shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Graph(GraphSource),
    Trace(TraceSource),
    Console(ConsoleSource),
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_ui_graph_args::{Input, InputKind};

    fn source(args: &[&str]) -> Result<Source, clap::Error> {
        Cli::try_parse_from(std::iter::once("bviz").chain(args.iter().copied()))?.source()
    }

    fn graph(inputs: &[(InputKind, &str)]) -> Source {
        let inputs = inputs
            .iter()
            .map(|&(kind, path)| Input {
                kind,
                path: path.into(),
            })
            .collect();
        Source::Graph(GraphSource::new(inputs).expect("the inputs name a graph"))
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
    fn an_invocation_naming_no_document_is_a_usage_error() {
        let err = source(&[]).expect_err("no document was named");
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
        assert!(
            err.to_string().contains(SOURCE_FLAGS),
            "the error names no flag to give: {err}"
        );
    }

    #[test]
    fn each_flag_names_a_document_of_its_own_kind() {
        assert_eq!(
            source(&["--aquery", "a.pb"]).unwrap(),
            graph(&[(InputKind::Aquery, "a.pb")])
        );
        assert_eq!(
            source(&["--cquery", "c.pb"]).unwrap(),
            graph(&[(InputKind::Cquery, "c.pb")])
        );
        assert_eq!(
            source(&["--query", "q.pb"]).unwrap(),
            graph(&[(InputKind::Query, "q.pb")])
        );
        assert_eq!(source(&["--profile", "p.gz"]).unwrap(), trace("p.gz"));
        assert_eq!(source(&["--bep", "b.bep"]).unwrap(), console("b.bep"));
    }

    /// Documents of several kinds and a repeated flag's documents are both
    /// several, refused in the terms clap refuses a value too many.
    #[test]
    fn an_invocation_naming_several_documents_is_a_usage_error() {
        for args in [
            &["--profile", "p.gz", "--bep", "b.bep", "--aquery", "a.pb"][..],
            &["--profile", "one.gz", "--profile", "two.gz"][..],
        ] {
            let err = source(args).expect_err("several documents were named");
            assert_eq!(err.kind(), ErrorKind::TooManyValues, "{args:?}");
        }
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
