//! Replays the console output of a Bazel build, captured in a
//! `--build_event_binary_file`, in a gpui window: seek anywhere in the build and
//! play it back at real or multiplied speed.
//!
//! Bazel does not timestamp its progress output, so accurate real-time playback
//! needs the build captured with `--show_timestamps`, whose `(HH:MM:SS)` prefixes
//! become the timeline. Without them the timeline is interpolated between the
//! build's sparse timestamped events (start, finish, action and test
//! completions) and is approximate.

mod color;
mod console;

pub use console::register_keys;

use bviz_terminal::Replay;
use bviz_ui_components::Pending;
use bviz_ui_console_args::{ConsoleArgs, ConsoleSource};
use bviz_ui_workspace::{AnyPanel, PanelGroup, Weight, document_label};
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use console::ConsolePanel;
use gpui::{App, SharedString, Window, prelude::*, px, size};
use std::path::PathBuf;

/// A build's console output being read and reconstructed.
pub struct Load {
    replay: Pending<Replay>,
    title: SharedString,
}

impl From<ConsoleSource> for Load {
    fn from(source: ConsoleSource) -> Self {
        let title = document_label(&source.path);
        Self {
            replay: Pending::start(move || {
                let path = source.path.display();
                let bytes =
                    std::fs::read(&source.path).map_err(|e| format!("cannot read {path}: {e}"))?;
                let output = bviz_terminal::parse(&bytes)
                    .map_err(|e| format!("cannot parse build event stream: {e}"))?;
                Ok(Replay::from_output(output))
            }),
            title,
        }
    }
}

/// A panel replaying the build `load` reads.
pub fn open(load: impl Into<Load>, _window: &mut Window, cx: &mut App) -> PanelGroup {
    let load = load.into();
    PanelGroup::one(AnyPanel::new(cx.new(|cx| ConsolePanel::new(load, cx))))
        .with_weight(Weight::LOG)
}

/// Replays the console output of a Bazel build. Each file given opens a replay
/// of its own.
#[derive(Parser)]
#[command(name = "console", version, about)]
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

/// Parses the build event streams and opens the console-replay window.
pub fn run() {
    let mut loads = Cli::parse().sources().map(Load::from);
    let Some(first) = loads.next() else {
        Cli::command()
            .error(
                ErrorKind::MissingRequiredArgument,
                "--bep or a file argument is required",
            )
            .exit()
    };
    bviz_ui_workspace::run_panels(
        "bviz console",
        size(px(1100.), px(720.)),
        first,
        loads.collect(),
        open,
        &[console::register_keys],
    );
}
