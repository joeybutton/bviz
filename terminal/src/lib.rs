//! Non-UI terminal support: parsing Bazel Build Event Protocol output and
//! emulating a terminal to reproduce the final rendered state of a build.

pub use bazel_build_event_proto as bep;

mod display;
mod output;
mod replay;
mod screen;

pub use alacritty_terminal::grid::Scroll;
pub use alacritty_terminal::index::Side;
pub use alacritty_terminal::selection::SelectionType;
pub use display::{DisplaySource, Terminal, Url};
pub use output::{parse, BuildOutput};
pub use replay::Replay;
pub use screen::Screen;

/// The terminal cell and its styling vocabulary, re-exported from
/// `alacritty_terminal` so renderers can read cells while depending only on this
/// crate.
pub use alacritty_terminal::term::cell::{Cell, Flags};
pub use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
