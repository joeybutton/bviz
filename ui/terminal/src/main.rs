//! Renders the final terminal state of a Bazel build, captured in a
//! `--build_event_binary_file`, in a gpui window.

mod terminal;

use std::process::ExitCode;

use gpui::{prelude::*, px, size};
use terminal::TerminalView;

fn main() -> ExitCode {
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
