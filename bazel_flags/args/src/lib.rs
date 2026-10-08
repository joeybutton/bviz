//! The flags of the command that prints the bazelrc a build is captured with,
//! and the bazelrc they render. Nothing here opens a window or draws, so a
//! parse can be exercised without a display.

use bviz_bazel_flags::{BAZELRC_FILE, CaptureDir, ConfigName, Output, QueryCommand};
use bviz_ui_console_args::BEP_LONG;
use bviz_ui_execlog_args::EXEC_LOG_LONG;
use bviz_ui_graph_args::{AQUERY_LONG, CQUERY_LONG, QUERY_LONG};
use bviz_ui_trace_args::PROFILE_LONG;
use clap::Args;
use std::fmt::Write as _;
use std::io::Write as _;

/// Prints the bazelrc that passes every Bazel flag bviz reads a build
/// through, under one --config, headed by how to capture a build with it.
#[derive(Args, Clone, Debug, Default, PartialEq, Eq)]
pub struct BazelrcArgs {
    /// The name the flags are passed under, as --config=NAME.
    #[arg(long, value_name = "NAME", default_value_t)]
    pub config: ConfigName,
    /// The directory, relative to the workspace root, Bazel writes the files
    /// bviz reads into.
    #[arg(long, value_name = "DIR", default_value_t)]
    pub dir: CaptureDir,
}

impl BazelrcArgs {
    /// The bazelrc, headed by how to capture a build with it, open the capture
    /// and write the bazelrc again.
    pub fn bazelrc(&self) -> String {
        let (config, dir) = (&self.config, &self.dir);
        let open: Vec<String> = bviz_bazel_flags::outputs(dir)
            .map(|(output, path)| format!("--{} {path}", source_long(output)))
            .collect();
        format!(
            "\
{header}# and open the capture:
#   bviz {open}
#
# Regenerate rather than edit, from the workspace root:
#   {regenerate} > {dir}/{BAZELRC_FILE}

{settings}",
            header = bviz_bazel_flags::header(config, dir),
            open = open.join(" \\\n#     "),
            regenerate = self.regenerate_command(),
            settings = bviz_bazel_flags::settings(config, dir),
        )
    }

    /// Writes the [`bazelrc`](Self::bazelrc) to stdout, exiting with an error
    /// where it cannot.
    pub fn print(&self) {
        if let Err(e) = std::io::stdout().write_all(self.bazelrc().as_bytes()) {
            eprintln!("bviz: {e}");
            std::process::exit(1);
        }
    }

    /// The `bviz bazelrc` command line that writes this bazelrc again.
    fn regenerate_command(&self) -> String {
        let mut command = String::from("bviz bazelrc");
        if self.config != ConfigName::default() {
            let _ = write!(command, " --config={}", self.config);
        }
        if self.dir != CaptureDir::default() {
            let _ = write!(command, " --dir={}", self.dir);
        }
        command
    }
}

/// The long name of bviz's flag that opens a captured `output`.
fn source_long(output: Output) -> &'static str {
    match output {
        Output::Profile => PROFILE_LONG,
        Output::BuildEvents => BEP_LONG,
        Output::ExecLog => EXEC_LOG_LONG,
        Output::Query(QueryCommand::Query) => QUERY_LONG,
        Output::Query(QueryCommand::Cquery) => CQUERY_LONG,
        Output::Query(QueryCommand::Aquery) => AQUERY_LONG,
    }
}
