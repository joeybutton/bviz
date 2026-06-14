//! Renders the final terminal state of a Bazel build, captured in a
//! `--build_event_binary_file`, in a gpui window.

mod terminal;

use std::process::ExitCode;

use gpui::prelude::*;
use terminal::TerminalView;

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: terminal <build_event_binary_file>");
        return ExitCode::FAILURE;
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("cannot read {path}: {err}");
            return ExitCode::FAILURE;
        }
    };
    let output = match bviz_terminal::parse(&bytes) {
        Ok(output) => output,
        Err(err) => {
            eprintln!("cannot parse build event stream: {err}");
            return ExitCode::FAILURE;
        }
    };
    let screen = bviz_terminal::Screen::from_output(&output);

    bviz_ui_shell::run("bazel_term", move |window, cx| {
        cx.new(|cx| TerminalView::new(screen, window, cx))
    });
    ExitCode::SUCCESS
}
