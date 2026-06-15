//! A display terminal: the build's reconstructed output laid out for a window of
//! a given size. The reconstructed screen is serialized to a clean ANSI stream
//! and replayed into an alacritty terminal sized to the window, so alacritty
//! wraps it; resizing re-wraps and reflows, and scrolling moves the viewport.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Cell;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;

use crate::output::BuildOutput;
use crate::screen::Screen;

/// History depth; large enough to retain every line of a build's output.
const SCROLLBACK: usize = 100_000;

/// The reconstructed build output, replayed into a terminal that wraps it to the
/// window size. [`resize`](Terminal::resize) re-wraps; [`scroll`](Terminal::scroll)
/// moves the viewport over the scrollback.
pub struct Terminal {
    term: Term<VoidListener>,
    columns: usize,
    rows: usize,
}

/// Terminal dimensions for the display emulator.
struct Size {
    columns: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

impl Terminal {
    /// Reconstruct the build's final screen (at the width Bazel used) and replay
    /// it into a display terminal of `columns` by `rows`, which wraps it to that
    /// width. The viewport starts at the first line of the build.
    pub fn new(output: &BuildOutput, columns: usize, rows: usize) -> Terminal {
        let columns = columns.max(1);
        let rows = rows.max(1);
        let config = Config {
            scrolling_history: SCROLLBACK,
            ..Config::default()
        };
        let mut term = Term::new(config, &Size { columns, rows }, VoidListener);
        let mut processor: Processor = Processor::new();
        processor.advance(&mut term, &Screen::from_output(output).to_ansi());
        // Reflow can leave content the cursor scrolled past in history; show the
        // build from its first line, leaving the rest to scrollback.
        term.scroll_display(Scroll::Top);
        Terminal {
            term,
            columns,
            rows,
        }
    }

    /// Resize the terminal, re-wrapping its content to the new width while keeping
    /// the current scroll position (alacritty clamps it to the new history).
    pub fn resize(&mut self, columns: usize, rows: usize) {
        let columns = columns.max(1);
        let rows = rows.max(1);
        if columns == self.columns && rows == self.rows {
            return;
        }
        self.term.resize(Size { columns, rows });
        self.columns = columns;
        self.rows = rows;
    }

    /// Move the viewport over the scrollback.
    pub fn scroll(&mut self, scroll: Scroll) {
        self.term.scroll_display(scroll);
    }

    pub fn columns(&self) -> usize {
        self.columns
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Lines of scrollback above the viewport.
    pub fn history_size(&self) -> usize {
        self.term.grid().history_size()
    }

    /// How many lines the viewport is scrolled up from the bottom.
    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// The currently visible rows, top to bottom, at the current scroll position.
    pub fn viewport(&self) -> Vec<Vec<Cell>> {
        let grid = self.term.grid();
        let top = -(grid.display_offset() as i32);
        (0..self.rows)
            .map(|v| {
                let row = &grid[Line(top + v as i32)];
                (0..self.columns)
                    .map(|col| row[Column(col)].clone())
                    .collect()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport_text(terminal: &Terminal) -> String {
        terminal
            .viewport()
            .iter()
            .map(|row| row.iter().map(|cell| cell.c).collect::<String>())
            .map(|line| line.trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .to_string()
    }

    #[test]
    fn wraps_to_width() {
        // A 12-character logical line wraps to two visual rows at width 6.
        let output = BuildOutput {
            columns: 40,
            bytes: b"abcdefghijkl\n".to_vec(),
        };
        let terminal = Terminal::new(&output, 6, 10);
        assert_eq!(viewport_text(&terminal), "abcdef\nghijkl");
    }

    #[test]
    fn reflows_on_resize() {
        let output = BuildOutput {
            columns: 40,
            bytes: b"abcdefghijkl\n".to_vec(),
        };
        let mut terminal = Terminal::new(&output, 6, 10);
        assert_eq!(viewport_text(&terminal), "abcdef\nghijkl");

        // Widening rejoins the wrapped halves onto one line.
        terminal.resize(20, 10);
        assert_eq!(viewport_text(&terminal), "abcdefghijkl");
    }

    #[test]
    fn scrolls_over_history() {
        let mut bytes = Vec::new();
        for i in 0..20 {
            bytes.extend_from_slice(format!("line{i:02}\n").as_bytes());
        }
        let output = BuildOutput { columns: 40, bytes };
        let mut terminal = Terminal::new(&output, 40, 5);

        // The viewport starts at the top of the output.
        assert!(viewport_text(&terminal).starts_with("line00"));
        // Scrolling to the bottom reveals the last lines.
        terminal.scroll(Scroll::Bottom);
        assert!(viewport_text(&terminal).contains("line19"));
        // And back to the top.
        terminal.scroll(Scroll::Top);
        assert!(viewport_text(&terminal).starts_with("line00"));
    }

    #[test]
    fn lines_start_at_column_zero() {
        // Each logical line must begin at the left edge, not where the previous
        // one ended.
        let output = BuildOutput {
            columns: 40,
            bytes: b"alpha\nbeta\ngamma\n".to_vec(),
        };
        let terminal = Terminal::new(&output, 40, 10);
        assert_eq!(viewport_text(&terminal), "alpha\nbeta\ngamma");
    }
}
