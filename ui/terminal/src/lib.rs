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

use std::process::ExitCode;

use gpui::{prelude::*, px, size};
use terminal::TerminalView;

pub fn run() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: terminal <build_event_binary_file>");
        return ExitCode::FAILURE;
    };
    bviz_ui_shell::run(
        "bazel_term",
        size(px(1100.), px(720.)),
        move |window, cx| cx.new(|cx| TerminalView::new(path, window, cx)),
    );
    ExitCode::SUCCESS
}
