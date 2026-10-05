//! The flags naming the compact execution logs and execution graph logs spawn
//! time is read from, and the documents they resolve to. Nothing here opens a
//! window or draws, so a parse can be exercised without a display.

use clap::Args;
use std::path::PathBuf;

/// The logs spawn time is read from.
#[derive(Args, Debug)]
#[command(next_help_heading = "Spawn time by phase (one panel per file)")]
pub struct ExecLogArgs {
    /// `bazel build --execution_log_compact_file=FILE` output.
    #[arg(long = "exec_log", value_name = "FILE")]
    pub exec_log: Vec<PathBuf>,
    /// `bazel build --experimental_enable_execution_graph_log
    /// --experimental_execution_graph_log_path=FILE` output.
    #[arg(long = "exec_graph_log", value_name = "FILE")]
    pub exec_graph_log: Vec<PathBuf>,
}

impl ExecLogArgs {
    /// The execution logs, then the execution graph logs, each kind in the
    /// order given.
    pub fn sources(self) -> impl Iterator<Item = ExecLogSource> {
        let logs = self.exec_log.into_iter().map(ExecLogSource::Spawns);
        let graphs = self.exec_graph_log.into_iter().map(ExecLogSource::Graph);
        logs.chain(graphs)
    }

    /// How many files the flags name.
    pub fn len(&self) -> usize {
        self.exec_log.len() + self.exec_graph_log.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One log, by the path of its file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecLogSource {
    /// A compact execution log.
    Spawns(PathBuf),
    /// An execution graph log.
    Graph(PathBuf),
}

impl ExecLogSource {
    pub fn path(&self) -> &PathBuf {
        match self {
            ExecLogSource::Spawns(path) | ExecLogSource::Graph(path) => path,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        exec_log: ExecLogArgs,
    }

    fn sources(args: &[&str]) -> Vec<ExecLogSource> {
        Cli::try_parse_from(std::iter::once("execlog").chain(args.iter().copied()))
            .expect("the flags parse")
            .exec_log
            .sources()
            .collect()
    }

    #[test]
    fn a_repeated_exec_log_names_a_document_each() {
        assert_eq!(
            sources(&["--exec_log", "one.binpb.zst", "--exec_log", "two.binpb.zst"]),
            [
                ExecLogSource::Spawns("one.binpb.zst".into()),
                ExecLogSource::Spawns("two.binpb.zst".into()),
            ]
        );
    }

    #[test]
    fn graph_logs_follow_the_execution_logs() {
        assert_eq!(
            sources(&[
                "--exec_graph_log",
                "graph.zst",
                "--exec_log",
                "exec.binpb.zst",
            ]),
            [
                ExecLogSource::Spawns("exec.binpb.zst".into()),
                ExecLogSource::Graph("graph.zst".into()),
            ]
        );
    }

    #[test]
    fn naming_no_log_names_no_document() {
        assert_eq!(sources(&[]), []);
    }
}
