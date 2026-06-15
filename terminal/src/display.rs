//! A display terminal: the build's reconstructed output laid out for a window of
//! a given size. The reconstructed screen is serialized to a clean ANSI stream
//! and replayed into an alacritty terminal sized to the window, so alacritty
//! wraps it; resizing re-wraps and reflows, and scrolling moves the viewport.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Cell;
use alacritty_terminal::term::search::{RegexIter, RegexSearch};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;

/// URLs to detect in the terminal text, by scheme. Mirrors the set Zed's
/// terminal recognizes; the body excludes control characters, whitespace, and
/// delimiters that cannot appear unescaped in a URL.
const URL_REGEX: &str = "(ipfs:|ipns:|magnet:|mailto:|gemini://|gopher://|https://|http://|news:|file:|git://|ssh:|ftp://)\
    [^\u{0000}-\u{001F}\u{007F}-\u{009F}<>\"\\s{-}\\^⟨⟩`']+";

/// A URL found in the terminal: its text and, per visible row, the inclusive
/// column span it covers for highlighting.
#[derive(Clone)]
pub struct Url {
    text: String,
    spans: Vec<Option<(usize, usize)>>,
}

impl Url {
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Per visible row, top to bottom, the inclusive column span the URL covers.
    pub fn spans(&self) -> &[Option<(usize, usize)>] {
        &self.spans
    }
}

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
    url_regex: RegexSearch,
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
            url_regex: RegexSearch::new(URL_REGEX).expect("valid URL regex"),
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

    /// Scroll to a position given as a fraction of the scrollback: 0.0 is the
    /// first line, 1.0 is the last. Used to map a scroll-bar drag to a line.
    pub fn scroll_to_fraction(&mut self, fraction: f32) {
        let history = self.history_size() as f32;
        let target = ((1.0 - fraction.clamp(0.0, 1.0)) * history).round() as i32;
        let delta = target - self.display_offset() as i32;
        if delta != 0 {
            self.term.scroll_display(Scroll::Delta(delta));
        }
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

    /// Grid point for a viewport cell, given by its column and its row from the
    /// top of the viewport. Columns and rows are clamped to the grid.
    fn grid_point(&self, column: usize, row: usize) -> Point {
        let column = column.min(self.columns.saturating_sub(1));
        let row = row.min(self.rows.saturating_sub(1));
        Point::new(Line(row as i32 - self.display_offset() as i32), Column(column))
    }

    /// Begin a selection of the given type anchored at a viewport cell.
    pub fn begin_selection(&mut self, ty: SelectionType, column: usize, row: usize, side: Side) {
        let point = self.grid_point(column, row);
        self.term.selection = Some(Selection::new(ty, point, side));
    }

    /// Extend the in-progress selection to a viewport cell.
    pub fn extend_selection(&mut self, column: usize, row: usize, side: Side) {
        let point = self.grid_point(column, row);
        if let Some(selection) = self.term.selection.as_mut() {
            selection.update(point, side);
        }
    }

    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    /// The selected text, joined across wrapped lines with trailing blanks
    /// trimmed, or `None` when nothing is selected.
    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string()
    }

    /// For each visible row, top to bottom, the inclusive column span selected on
    /// that row, if any. Used to paint the selection highlight.
    pub fn selection_spans(&self) -> Vec<Option<(usize, usize)>> {
        let Some(range) = self
            .term
            .selection
            .as_ref()
            .and_then(|selection| selection.to_range(&self.term))
        else {
            return vec![None; self.rows];
        };
        let offset = self.display_offset() as i32;
        (0..self.rows)
            .map(|v| {
                let line = Line(v as i32 - offset);
                let mut span: Option<(usize, usize)> = None;
                for col in 0..self.columns {
                    if range.contains(Point::new(line, Column(col))) {
                        span = Some(span.map_or((col, col), |(s, _)| (s, col)));
                    }
                }
                span
            })
            .collect()
    }

    /// The URL under a viewport cell, if the cell falls within one. Detection is
    /// by scheme over the logical line containing the cell.
    pub fn url_at(&mut self, column: usize, row: usize) -> Option<Url> {
        let point = self.grid_point(column, row);
        let line_start = self.term.line_search_left(point);
        let line_end = self.term.line_search_right(point);
        let mut bounds = None;
        for m in RegexIter::new(
            line_start,
            line_end,
            Direction::Right,
            &self.term,
            &mut self.url_regex,
        ) {
            if m.contains(&point) {
                bounds = Some((*m.start(), *m.end()));
                break;
            }
            if *m.start() > point {
                break;
            }
        }
        let (start, mut end) = bounds?;

        // Trailing sentence punctuation usually belongs to the prose, not the
        // URL; drop it from both the opened text and the highlighted span.
        let full = self.term.bounds_to_string(start, end);
        let kept = full
            .trim_end_matches(['.', ',', ';', ':', '!', '?', ')'])
            .chars()
            .count();
        let removed = full.chars().count() - kept;
        if removed > 0 && removed <= end.column.0 {
            end = Point::new(end.line, Column(end.column.0 - removed));
        }
        let text: String = full.chars().take(kept).collect();
        if text.is_empty() {
            return None;
        }
        Some(Url {
            text,
            spans: self.point_range_spans(start, end),
        })
    }

    /// Per visible row, the inclusive column span covered by the linear grid
    /// range `start..=end` (reading order).
    fn point_range_spans(&self, start: Point, end: Point) -> Vec<Option<(usize, usize)>> {
        let offset = self.display_offset() as i32;
        let last_column = self.columns.saturating_sub(1);
        (0..self.rows)
            .map(|v| {
                let line = v as i32 - offset;
                if line < start.line.0 || line > end.line.0 {
                    return None;
                }
                let first = if line == start.line.0 { start.column.0 } else { 0 };
                let last = if line == end.line.0 {
                    end.column.0
                } else {
                    last_column
                };
                (first <= last).then_some((first.min(last_column), last.min(last_column)))
            })
            .collect()
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
    fn scroll_to_fraction_maps_to_lines() {
        let mut bytes = Vec::new();
        for i in 0..20 {
            bytes.extend_from_slice(format!("line{i:02}\n").as_bytes());
        }
        let output = BuildOutput { columns: 40, bytes };
        let mut terminal = Terminal::new(&output, 40, 5);

        terminal.scroll_to_fraction(1.0);
        assert!(viewport_text(&terminal).contains("line19"));
        terminal.scroll_to_fraction(0.0);
        assert!(viewport_text(&terminal).starts_with("line00"));
    }

    #[test]
    fn selection_yields_text_and_spans() {
        let output = BuildOutput {
            columns: 40,
            bytes: b"alpha\nbeta\ngamma\n".to_vec(),
        };
        let mut terminal = Terminal::new(&output, 40, 10);
        // Select "lph": from the left of column 1 to the right of column 3.
        terminal.begin_selection(SelectionType::Simple, 1, 0, Side::Left);
        terminal.extend_selection(3, 0, Side::Right);
        assert_eq!(terminal.selection_text().as_deref(), Some("lph"));
        let spans = terminal.selection_spans();
        assert_eq!(spans[0], Some((1, 3)));
        assert_eq!(spans[1], None);

        terminal.clear_selection();
        assert_eq!(terminal.selection_text(), None);
        assert!(terminal.selection_spans().iter().all(Option::is_none));
    }

    #[test]
    fn detects_url_under_cell() {
        let output = BuildOutput {
            columns: 80,
            bytes: b"see https://example.com/path for details.\n".to_vec(),
        };
        let mut terminal = Terminal::new(&output, 80, 10);
        // "see " is four columns, so the URL starts at column 4.
        let url = terminal.url_at(10, 0).expect("url under the link");
        assert_eq!(url.text(), "https://example.com/path");
        assert_eq!(url.spans()[0], Some((4, 27)));

        // No URL over the leading prose.
        assert!(terminal.url_at(1, 0).is_none());
    }

    #[test]
    fn trims_trailing_punctuation_from_url() {
        let output = BuildOutput {
            columns: 80,
            bytes: b"go to http://example.com.\n".to_vec(),
        };
        let mut terminal = Terminal::new(&output, 80, 10);
        let url = terminal.url_at(8, 0).expect("url under the link");
        assert_eq!(url.text(), "http://example.com");
    }

    #[test]
    fn semantic_and_line_selection_expand() {
        let output = BuildOutput {
            columns: 40,
            bytes: b"alpha beta\n".to_vec(),
        };
        let mut terminal = Terminal::new(&output, 40, 10);

        // A word selection anchored inside "beta" expands to the whole word.
        terminal.begin_selection(SelectionType::Semantic, 7, 0, Side::Left);
        assert_eq!(terminal.selection_text().as_deref(), Some("beta"));

        // A line selection takes the whole line regardless of the column.
        terminal.begin_selection(SelectionType::Lines, 2, 0, Side::Left);
        assert_eq!(
            terminal.selection_text().as_deref().map(str::trim_end),
            Some("alpha beta")
        );
    }

    #[test]
    fn wide_characters_occupy_two_columns() {
        // A wide character takes two columns; the cell after it is a spacer, so
        // the following character lands one column further right rather than
        // being shifted by a stray space.
        let output = BuildOutput {
            columns: 40,
            bytes: "a中b\n".as_bytes().to_vec(),
        };
        let terminal = Terminal::new(&output, 40, 10);
        let row = &terminal.viewport()[0];
        assert_eq!(row[0].c, 'a');
        assert_eq!(row[1].c, '中');
        assert_eq!(row[3].c, 'b');
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
