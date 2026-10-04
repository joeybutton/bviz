//! The command line of the window that shows every kind of Bazel build
//! document: the viewers' flag fragments joined into one parser, and what
//! a command line names for each viewer to read. It names no window and draws
//! nothing, so a parse can be exercised without a display.

use bviz_ui_console_args::{ConsoleArgs, ConsoleSource};
use bviz_ui_execlog_args::{ExecLogArgs, ExecLogSource};
use bviz_ui_graph_args::{GraphArgs, GraphSource};
use bviz_ui_trace_args::{TraceArgs, TraceSource};
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};

/// Opens Bazel build documents: the dependency graph, the action timeline, the
/// console replay, the spawn time by phase.
///
/// The graph and each profile, build event stream and execution log named on
/// the command line take a region of the one window each, the regions stacked
/// one above another.
///
/// Files written by one Bazel invocation open as one document, which stands
/// where the first of them does. A profile, a build event stream and an
/// execution log each carry the id of the invocation that wrote them, and one
/// file of each kind carrying the same id are that invocation's document. A
/// further file of a kind the invocation holds is a document of its own, and so
/// is a file carrying no id.
///
/// The query, cquery and aquery files carry no id, so bviz cannot check which
/// build a graph describes. It takes the graph for the invocation's when every
/// id the other files carry is the same one, be that a single file's, and opens
/// the graph as a document of its own when they carry several ids or none.
#[derive(Parser, Debug)]
#[command(name = "bviz", bin_name = "bviz", version, about)]
pub struct Cli {
    #[command(flatten)]
    pub graph: GraphArgs,
    #[command(flatten)]
    pub trace: TraceArgs,
    #[command(flatten)]
    pub console: ConsoleArgs,
    #[command(flatten)]
    pub exec_log: ExecLogArgs,
}

/// The flags of [`Cli`] that name a file, for the command line that gives none.
const SOURCE_FLAGS: &str = "--aquery, --cquery, --query, --profile, --bep or --exec_log";

impl Cli {
    /// What this command line names to open.
    ///
    /// The rejection of a command line naming nothing is written out here rather
    /// than left to a clap argument group, because these fragments are shared
    /// with the per-viewer binaries, where a different flag is the required one.
    pub fn sources(self) -> Result<Sources, clap::Error> {
        let mut files = Vec::with_capacity(
            self.trace.profile.len() + self.console.bep.len() + self.exec_log.exec_log.len(),
        );
        files.extend(self.trace.sources().map(Source::Trace));
        files.extend(self.console.sources().map(Source::Console));
        files.extend(self.exec_log.sources().map(Source::ExecLog));
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

/// A file a viewer reads on its own: a profile, a build event stream or an
/// execution log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Trace(TraceSource),
    Console(ConsoleSource),
    ExecLog(ExecLogSource),
}

/// What a command line names to open: the one graph its graph flags describe,
/// where it gives any, and each repeatable flag's files in the order given.
/// Either way it names at least one thing: a window with nothing to show is a
/// usage mistake, refused before any window opens.
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

    fn exec_log(path: &str) -> Source {
        Source::ExecLog(ExecLogSource { path: path.into() })
    }

    /// A window with nothing to show is refused, in the terms clap refuses a
    /// missing required argument — which is what fixes the exit code the shell
    /// sees.
    #[test]
    fn a_command_line_naming_no_file_is_a_usage_error() {
        let err = sources(&[]).expect_err("no file was named");
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
        assert!(
            err.to_string().contains(SOURCE_FLAGS),
            "the error names no flag to give: {err}"
        );
    }

    #[test]
    fn each_flag_names_a_source_of_its_own_kind() {
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
            sources(&["--exec_log", "e.zst"]).unwrap(),
            files(exec_log("e.zst"), Vec::new())
        );
        assert_eq!(
            sources(&["--profile", "one.gz", "--profile", "two.gz"]).unwrap(),
            files(trace("one.gz"), vec![trace("two.gz")])
        );
    }

    /// Files of every kind open together, the graph ahead of the rest wherever
    /// its flags stand.
    #[test]
    fn a_graph_and_one_file_of_each_kind_are_four_sources() {
        let named = sources(&[
            "--exec_log",
            "e.zst",
            "--profile",
            "p.gz",
            "--bep",
            "b.bep",
            "--aquery",
            "a.pb",
        ])
        .unwrap();
        assert_eq!(
            named,
            graph(
                &[(InputKind::Aquery, "a.pb")],
                vec![trace("p.gz"), console("b.bep"), exec_log("e.zst")]
            )
        );
    }

    /// The flags are the surface users' scripts are written against, so their
    /// names and the file placeholder they take are pinned here.
    #[test]
    fn the_flags_keep_their_names() {
        let help = Cli::command().render_long_help().to_string();
        for flag in [
            "--aquery",
            "--cquery",
            "--query",
            "--profile",
            "--bep",
            "--exec_log",
        ] {
            assert!(
                help.contains(&format!("{flag} <FILE>")),
                "no {flag}: {help}"
            );
        }
    }
}
