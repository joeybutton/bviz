//! Emulating a terminal to recover the final on-screen state of a build.
//!
//! With `--curses`, Bazel's console output is a stream of ANSI control sequences
//! that repaint a progress display in place. Feeding it through a terminal
//! emulator at the width Bazel used resolves those sequences into a flat
//! character grid: the build output as it finally appeared.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Cell;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor};

use crate::output::BuildOutput;

/// Rows in the internal emulator. The build event stream carries the terminal
/// width but not its height, and Bazel's progress repaints move the cursor only
/// within the block they just wrote, so any height exceeding that block
/// reproduces the output; this is generously larger. Content above the screen
/// scrolls into history and is preserved by `scrolling_history`.
const INTERNAL_ROWS: usize = 100;

/// History depth; large enough to retain every line of a build's output.
const SCROLLBACK: usize = 100_000;

/// The final terminal state: rows from the top of scrollback history down to the
/// last non-blank screen line, each `columns` cells wide. Cells are
/// `alacritty_terminal`'s, carrying their character, colors, and style flags.
pub struct Screen {
    pub columns: usize,
    pub rows: Vec<Vec<Cell>>,
}

/// Terminal dimensions for the emulator. Height is fixed; see [`INTERNAL_ROWS`].
struct Size {
    columns: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    fn screen_lines(&self) -> usize {
        INTERNAL_ROWS
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

impl Screen {
    /// Replay a build's captured output through a terminal emulator and snapshot
    /// the resulting grid.
    pub fn from_output(output: &BuildOutput) -> Screen {
        let columns = output.columns as usize;
        let config = Config {
            scrolling_history: SCROLLBACK,
            ..Config::default()
        };
        let mut term = Term::new(config, &Size { columns }, VoidListener);
        let mut processor: Processor = Processor::new();
        processor.advance(&mut term, &lf_to_crlf(&output.bytes));

        let grid = term.grid();
        let mut rows: Vec<Vec<Cell>> = (grid.topmost_line().0..=grid.bottommost_line().0)
            .map(|line| {
                let row = &grid[Line(line)];
                (0..columns).map(|col| row[Column(col)].clone()).collect()
            })
            .collect();

        while rows.last().is_some_and(|row| is_blank(row)) {
            rows.pop();
        }
        Screen { columns, rows }
    }

    /// The plain text of the screen, one line per row with trailing blanks trimmed.
    pub fn text(&self) -> String {
        self.rows
            .iter()
            .map(|row| row.iter().map(|cell| cell.c).collect::<String>())
            .map(|line| line.trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Translate bare line feeds to CRLF.
///
/// Bazel writes progress repaints assuming a `\n` returns the cursor to column
/// zero, which on a real terminal is the tty's `ONLCR` translation, applied
/// after the bytes the build event stream captures. Without it, alacritty keeps
/// the column on a line feed, so each progress line starts mid-row and the
/// repaints drift down the screen instead of overwriting in place.
fn lf_to_crlf(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut prev = 0u8;
    for &b in bytes {
        if b == b'\n' && prev != b'\r' {
            out.push(b'\r');
        }
        out.push(b);
        prev = b;
    }
    out
}

/// A row is blank if every cell is an unstyled space on the default background.
fn is_blank(row: &[Cell]) -> bool {
    row.iter()
        .all(|cell| cell.c == ' ' && cell.bg == Color::Named(NamedColor::Background))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::parse;

    const SAMPLE: &[u8] = include_bytes!("../testdata/sample.bep");

    #[test]
    fn renders_final_build_output() {
        let output = parse(SAMPLE).expect("parse sample stream");
        let screen = Screen::from_output(&output);
        assert_eq!(screen.columns, 100);

        // The build finished, so its concluding line survives into the grid.
        let text = screen.text();
        assert!(
            text.contains("Build completed successfully"),
            "final screen:\n{text}"
        );

        // The progress display is repainted in place, not appended: the dozens
        // of intermediate "Analyzing:" updates collapse to at most the final one.
        let analyzing = text
            .lines()
            .filter(|line| line.contains("Analyzing:"))
            .count();
        assert!(analyzing <= 1, "progress did not collapse:\n{text}");
    }

    #[test]
    fn collapses_in_place_repaints() {
        // Bazel's repaint idiom: a line, a line feed, then CR + cursor-up +
        // clear-line before the next. These must overwrite onto one row.
        let output = BuildOutput {
            columns: 20,
            bytes: b"first\n\r\x1b[1A\x1b[Ksecond\n\r\x1b[1A\x1b[Kthird\n".to_vec(),
        };
        let screen = Screen::from_output(&output);
        assert_eq!(screen.text(), "third");
    }

    #[test]
    fn golden_window_shape() {
        // A miniature build window exercising the layout that matters: literal
        // indentation that must be preserved, and a two-line progress block that
        // is repainted twice in place and finally replaced by a colored result.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"Build:\n  //app:bin\n  //app:test\n");
        bytes.extend_from_slice(b"Running 0/2\n   local\n");
        bytes.extend_from_slice(b"\r\x1b[1A\x1b[K\r\x1b[1A\x1b[K");
        bytes.extend_from_slice(b"Running 2/2\n   done\n");
        bytes.extend_from_slice(b"\r\x1b[1A\x1b[K\r\x1b[1A\x1b[K");
        bytes.extend_from_slice(b"\x1b[32mPASS\x1b[0m all\n");
        let screen = Screen::from_output(&BuildOutput { columns: 40, bytes });

        // Shape: the indents survive and the progress block collapses to PASS.
        assert_eq!(screen.text(), "Build:\n  //app:bin\n  //app:test\nPASS all");

        // Color: the result line carries the green set by the SGR sequence.
        let pass = &screen.rows[3];
        assert_eq!(pass[0].c, 'P');
        assert_eq!(pass[0].fg, Color::Named(NamedColor::Green));
    }
}
