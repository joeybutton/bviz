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
mod model;

pub use console::{ConsolePanel, register_keys};
use model::ConsoleModel;

use bviz_invocation::{InvocationId, Session};
use bviz_ui_components::Pending;
use bviz_ui_console_args::{BEP_LONG, ConsoleArgs, ConsoleSource};
use bviz_ui_playback::DocumentClock;
use bviz_ui_workspace::{Document, Member, ViewSpec, document_label};
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use gpui::{App, Entity, SharedString, Window, prelude::*};
use std::path::PathBuf;

/// A build's console output being read and reconstructed.
pub struct Load {
    stream: Pending<model::Stream>,
    title: SharedString,
}

/// The invocation that wrote the build event stream at `source`, read from the
/// head of the file alone.
pub fn invocation_id(source: &ConsoleSource) -> Option<InvocationId> {
    bviz_terminal::probe_invocation_id(&source.path)
}

impl From<ConsoleSource> for Load {
    fn from(source: ConsoleSource) -> Self {
        let title = document_label(&source.path);
        Self {
            stream: Pending::start(move || {
                let path = source.path.display();
                let bytes =
                    std::fs::read(&source.path).map_err(|e| format!("cannot read {path}: {e}"))?;
                model::Stream::parse(&bytes)
            }),
            title,
        }
    }
}

/// The views replaying the build `load` reads, which tells `session` what its
/// stream knows of the invocation where it is opened as part of one, and plays
/// on `clock` where its document plays back.
pub fn open(
    load: impl Into<Load>,
    session: Option<Entity<Session>>,
    clock: Option<DocumentClock>,
    _window: &mut Window,
    cx: &mut App,
) -> Member {
    let load = load.into();
    let label = load.title;
    let model = cx.new(|cx| ConsoleModel::reading(load.stream, session, clock, cx));
    let console = ViewSpec::new(move |cx| cx.new(|cx| ConsolePanel::new(model.clone(), cx)));
    Member::new(label, console, Vec::new())
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
                format!("--{BEP_LONG} or a file argument is required"),
            )
            .exit()
    };
    bviz_ui_workspace::run_panels(
        "bviz console",
        bviz_ui_workspace::TEXT_WINDOW_SIZE,
        first,
        loads.collect(),
        |load, window, cx| {
            Document::playing_on(DocumentClock::alone(cx), |clock| {
                open(load, None, Some(clock.clone()), window, cx).into()
            })
        },
        &[console::register_keys],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_terminal::bep::build_event_stream::{BuildEvent, BuildStarted, build_event::Payload};
    use prost::Message;

    #[test]
    fn a_source_names_the_invocation_its_stream_was_written_by() {
        let dir = std::env::var_os("TEST_TMPDIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let source = |name: &str, stream: Option<&[u8]>| {
            let path = std::path::Path::new(&dir).join(name);
            if let Some(stream) = stream {
                std::fs::write(&path, stream).unwrap();
            }
            ConsoleSource { path }
        };
        let started = BuildEvent {
            payload: Some(Payload::Started(BuildStarted {
                uuid: "a5f7f3e6-4fda".into(),
                ..Default::default()
            })),
            ..Default::default()
        }
        .encode_length_delimited_to_vec();
        let named = source("named.bep", Some(&started));
        assert_eq!(invocation_id(&named), InvocationId::new("a5f7f3e6-4fda"));
        let unnamed = source("unnamed.bep", Some(&[]));
        assert_eq!(invocation_id(&unnamed), None);
        assert_eq!(invocation_id(&source("missing.bep", None)), None);
    }
}
