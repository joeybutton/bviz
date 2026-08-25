//! Replays the terminal output of a Bazel build, captured in a
//! `--build_event_binary_file`, in a gpui window: seek anywhere in the build and
//! play it back at real or multiplied speed.
//!
//! Bazel does not timestamp its progress output, so accurate real-time playback
//! needs the build captured with `--show_timestamps`, whose `(HH:MM:SS)` prefixes
//! become the timeline. Without them the timeline is interpolated between the
//! build's sparse timestamped events (start, finish, action and test
//! completions) and is approximate.

mod color;
mod terminal;

use bviz_terminal::Replay;
use bviz_ui_components::Pending;
use bviz_ui_terminal_args::{ConsoleArgs, ConsoleSource};
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use gpui::{prelude::*, px, size};
use std::path::PathBuf;
use terminal::TerminalView;

/// A build's console output being read and reconstructed.
pub struct Load(Pending<Replay>);

impl From<ConsoleSource> for Load {
    fn from(source: ConsoleSource) -> Self {
        Self(Pending::start(move || {
            let path = source.path.display();
            let bytes =
                std::fs::read(&source.path).map_err(|e| format!("cannot read {path}: {e}"))?;
            let output = bviz_terminal::parse(&bytes)
                .map_err(|e| format!("cannot parse build event stream: {e}"))?;
            Ok(Replay::from_output(output))
        }))
    }
}

/// Opens a window replaying the build `load` reads.
pub fn open_window(load: impl Into<Load>) {
    let load = load.into();
    bviz_ui_shell::run(
        "bviz console",
        size(px(1100.), px(720.)),
        move |window, cx| cx.new(|cx| TerminalView::new(load, window, cx)),
    );
}

/// Replays the console output of a Bazel build.
#[derive(Parser)]
#[command(name = "terminal", version, about)]
struct Cli {
    #[command(flatten)]
    console: ConsoleArgs,
    /// The same file `--bep` names, given without the flag.
    #[arg(value_name = "FILE")]
    files: Vec<PathBuf>,
}

impl Cli {
    fn sources(self) -> impl Iterator<Item = ConsoleSource> {
        let positional = self.files.into_iter().map(|path| ConsoleSource { path });
        self.console.sources().chain(positional)
    }
}

/// Parses the build event stream and opens the console-replay window.
pub fn run() {
    let mut sources = Cli::parse().sources();
    let Some(source) = sources.next() else {
        Cli::command()
            .error(
                ErrorKind::MissingRequiredArgument,
                "--bep or a file argument is required",
            )
            .exit()
    };
    if sources.next().is_some() {
        Cli::command()
            .error(
                ErrorKind::TooManyValues,
                "a window shows one document, so one is opened per invocation",
            )
            .exit()
    }
    open_window(source);
}
