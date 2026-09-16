//! The command line of the window that shows every kind of Bazel build
//! document: the three viewers' flag fragments joined into one parser, and the
//! documents an invocation resolves to. It names no window and draws nothing,
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

/// The flags of [`Cli`] that name a document, for the invocation that gives none.
const SOURCE_FLAGS: &str = "--aquery, --cquery, --query, --profile or --bep";

impl Cli {
    /// The documents this invocation names, graph first and each repeatable
    /// flag's in the order given.
    ///
    /// The rejection of an invocation naming none is written out here rather
    /// than left to a clap argument group, because these fragments are shared
    /// with the per-viewer binaries, where a different flag is the required one.
    pub fn sources(self) -> Result<Sources, clap::Error> {
        let mut named = Vec::with_capacity(1 + self.trace.profile.len() + self.console.bep.len());
        named.extend(self.graph.source().map(Source::Graph));
        named.extend(self.trace.sources().map(Source::Trace));
        named.extend(self.console.sources().map(Source::Console));
        let mut named = named.into_iter();
        let Some(first) = named.next() else {
            return Err(Self::command().error(
                ErrorKind::MissingRequiredArgument,
                format!("one of {SOURCE_FLAGS} is required"),
            ));
        };
        Ok(Sources {
            first,
            rest: named.collect(),
        })
    }
}

/// A document to open, whichever viewer shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Graph(GraphSource),
    Trace(TraceSource),
    Console(ConsoleSource),
}

/// The documents an invocation names, in the order a window shows them. Holding
/// the first apart from the rest is what makes it at least one: a window with no
/// document to show is a usage mistake, refused before any window opens.
#[derive(Clone, Debug)]
pub struct Sources {
    first: Source,
    rest: Vec<Source>,
}

impl Sources {
    /// The document the window shows at the top, and those stacked below it.
    pub fn into_parts(self) -> (Source, Vec<Source>) {
        (self.first, self.rest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_ui_graph_args::{Input, InputKind};

    fn sources(args: &[&str]) -> Result<Vec<Source>, clap::Error> {
        let cli = Cli::try_parse_from(std::iter::once("bviz").chain(args.iter().copied()))?;
        let (first, rest) = cli.sources()?.into_parts();
        Ok(std::iter::once(first).chain(rest).collect())
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
            [graph(&[(InputKind::Aquery, "a.pb")])]
        );
        assert_eq!(
            sources(&["--cquery", "c.pb"]).unwrap(),
            [graph(&[(InputKind::Cquery, "c.pb")])]
        );
        assert_eq!(
            sources(&["--query", "q.pb"]).unwrap(),
            [graph(&[(InputKind::Query, "q.pb")])]
        );
        assert_eq!(sources(&["--profile", "p.gz"]).unwrap(), [trace("p.gz")]);
        assert_eq!(sources(&["--bep", "b.bep"]).unwrap(), [console("b.bep")]);
        assert_eq!(
            sources(&["--profile", "one.gz", "--profile", "two.gz"]).unwrap(),
            [trace("one.gz"), trace("two.gz")]
        );
    }

    /// Documents of every kind open together.
    #[test]
    fn a_graph_a_profile_and_a_bep_are_three_documents() {
        let named = sources(&["--profile", "p.gz", "--bep", "b.bep", "--aquery", "a.pb"]).unwrap();
        assert_eq!(
            named,
            [
                graph(&[(InputKind::Aquery, "a.pb")]),
                trace("p.gz"),
                console("b.bep"),
            ]
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
