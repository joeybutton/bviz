//! A display terminal: the build's reconstructed output laid out for a window of
//! a given size. The reconstructed screen is serialized to a clean ANSI stream
//! and replayed into an alacritty terminal sized to the window, so alacritty
//! wraps it; resizing re-wraps and reflows, and scrolling moves the viewport.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::Cell;
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};

/// URLs to detect in the terminal text: the hint pattern alacritty highlights
/// with, matching a scheme then a run up to the first character a URL is not
/// written with — a control character, whitespace, a quote or bracket, or one
/// of `^`, `|`, `\`.
const URL_REGEX: &str = "(ipfs:|ipns:|magnet:|mailto:|gemini://|gopher://|https://|http://|news:|file:|git://|ssh:|ftp://)\
    [^\u{0000}-\u{001F}\u{007F}-\u{009F}<>\"\\s{-}\\^⟨⟩`\\\\]+";

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

use crate::columns::Columns;
use crate::output::BuildOutput;
use crate::screen::Screen;
use crate::{build_term, snapshot_rows};

mod search;
mod selection;
mod url;

/// A build's reconstructed output, serialized to a clean ANSI stream ready to
/// replay into a window-sized terminal. The reconstruction is window
/// independent, so it can be prepared off the UI thread.
pub struct DisplaySource {
    ansi: Vec<u8>,
}

impl DisplaySource {
    pub fn from_output(output: &BuildOutput) -> DisplaySource {
        DisplaySource {
            ansi: Screen::from_output(output).to_ansi(),
        }
    }

    /// The display source for a prefix of a build's output, at the width Bazel
    /// used: one playback frame's reconstructed screen.
    pub(crate) fn from_bytes(bytes: &[u8], columns: Columns) -> DisplaySource {
        DisplaySource {
            ansi: Screen::from_bytes(bytes, columns).to_ansi(),
        }
    }
}

/// A selection captured in content coordinates (each endpoint a line counted from
/// the top of the scrollback, and a column), so it survives a rebuild whose grid
/// has more history. Opaque: produced by [`Terminal::selection_span`] and consumed
/// by [`Terminal::restore_selection`].
#[derive(Clone, Copy)]
pub struct SelectionSpan {
    start: (i32, usize),
    end: (i32, usize),
}

/// The reconstructed build output, replayed into a terminal that wraps it to the
/// window size. [`resize`](Terminal::resize) re-wraps; [`scroll`](Terminal::scroll)
/// moves the viewport over the scrollback.
pub struct Terminal {
    term: Term<VoidListener>,
    columns: usize,
    rows: usize,
    url_regex: RegexSearch,
    /// Ranges matching the current find query, in reading order.
    search_matches: Vec<Match>,
    /// Index of the focused match among [`search_matches`].
    current_match: Option<usize>,
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

/// A viewport cell coordinate: `column` across, `row` down.
#[derive(Clone, Copy)]
pub struct CellPos {
    pub column: usize,
    pub row: usize,
}

impl Terminal {
    /// Reconstruct the build's final screen (at the width Bazel used) and replay
    /// it into a display terminal of `columns` by `rows`. Reconstruction happens
    /// inline; [`from_display`](Terminal::from_display) takes a prepared
    /// [`DisplaySource`] so the heavy step can run off the UI thread.
    pub fn new(output: &BuildOutput, columns: usize, rows: usize) -> Terminal {
        Terminal::from_display(&DisplaySource::from_output(output), columns, rows)
    }

    /// Replay an already-reconstructed [`DisplaySource`] into a display terminal
    /// of `columns` by `rows`, which wraps it to that width, or to the nearest
    /// the emulator runs at. The viewport starts at the first line of the build.
    pub fn from_display(source: &DisplaySource, columns: usize, rows: usize) -> Terminal {
        let columns = Columns::fitting(columns).get();
        let rows = rows.max(1);
        let mut term = build_term(&Size { columns, rows }, &source.ansi);
        // Reflow can leave content the cursor scrolled past in history; show the
        // build from its first line, leaving the rest to scrollback.
        term.scroll_display(Scroll::Top);
        Terminal {
            term,
            columns,
            rows,
            url_regex: RegexSearch::new(URL_REGEX).expect("valid URL regex"),
            search_matches: Vec::new(),
            current_match: None,
        }
    }

    /// Resize the terminal, re-wrapping its content to the new width while keeping
    /// the current scroll position (alacritty clamps it to the new history).
    pub fn resize(&mut self, columns: usize, rows: usize) {
        let columns = Columns::fitting(columns).get();
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

    /// Scroll so that the line `line` rows from the top of the scrollback sits at
    /// the top of the viewport, clamped to the available history. Used to hold a
    /// scroll position across a rebuild as content grows below it.
    pub fn scroll_to_top_line(&mut self, line: usize) {
        // The viewport top is `history_size - display_offset` lines from the top,
        // so the offset that puts `line` there is `history_size - line`.
        let target = self.history_size().saturating_sub(line) as i32;
        self.scroll_to_offset(target);
    }

    /// Move the viewport to a display offset, doing nothing if already there.
    fn scroll_to_offset(&mut self, target: i32) {
        let delta = target - self.display_offset() as i32;
        if delta != 0 {
            self.term.scroll_display(Scroll::Delta(delta));
        }
    }

    /// Scroll to a position given as a fraction of the scrollback: 0.0 is the
    /// first line, 1.0 is the last. Used to map a scroll-bar drag to a line.
    pub fn scroll_to_fraction(&mut self, fraction: f32) {
        let history = self.history_size() as f32;
        let target = ((1.0 - fraction.clamp(0.0, 1.0)) * history).round() as i32;
        self.scroll_to_offset(target);
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
        Point::new(
            Line(row as i32 - self.display_offset() as i32),
            Column(column),
        )
    }

    /// Grid line for a content line (counted from the top of the scrollback),
    /// clamped into the grid. Inverse of [`grid_to_content_line`](Self::grid_to_content_line).
    fn content_to_grid_line(&self, content_line: i32) -> Line {
        let grid = self.term.grid();
        let (top, bottom) = (grid.topmost_line().0, grid.bottommost_line().0);
        Line((content_line - self.history_size() as i32).clamp(top, bottom))
    }

    /// Content line, counted from the top of the scrollback, for a grid line.
    fn grid_to_content_line(&self, line: Line) -> i32 {
        line.0 + self.history_size() as i32
    }

    /// The currently visible rows, top to bottom, at the current scroll position.
    pub fn viewport(&self) -> Vec<Vec<Cell>> {
        let grid = self.term.grid();
        let top = -(grid.display_offset() as i32);
        snapshot_rows(grid, top..=top + self.rows as i32 - 1, self.columns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport_text(terminal: &Terminal) -> String {
        crate::screen::rows_to_text(&terminal.viewport())
            .trim_end()
            .to_string()
    }

    #[test]
    fn wraps_to_width() {
        // A 12-character logical line wraps to two visual rows at width 6.
        let output = BuildOutput::from_bytes(40, b"abcdefghijkl\n".to_vec());
        let terminal = Terminal::new(&output, 6, 10);
        assert_eq!(viewport_text(&terminal), "abcdef\nghijkl");
    }

    #[test]
    fn fits_a_window_narrower_than_the_emulator_runs_at() {
        let output = BuildOutput::from_bytes(40, b"ab\n".to_vec());
        let mut terminal = Terminal::new(&output, 1, 10);
        assert_eq!(viewport_text(&terminal), "ab");
        terminal.resize(0, 10);
        assert_eq!(terminal.columns(), terminal.term.columns());
    }

    #[test]
    fn reflows_on_resize() {
        let output = BuildOutput::from_bytes(40, b"abcdefghijkl\n".to_vec());
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
        let output = BuildOutput::from_bytes(40, bytes);
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
    fn scroll_to_top_line_holds_a_line_at_the_viewport_top() {
        let mut bytes = Vec::new();
        for i in 0..20 {
            bytes.extend_from_slice(format!("line{i:02}\n").as_bytes());
        }
        let output = BuildOutput::from_bytes(40, bytes);
        let mut terminal = Terminal::new(&output, 40, 5);

        // The named line is brought to the top of the viewport.
        terminal.scroll_to_top_line(10);
        assert!(viewport_text(&terminal).starts_with("line10"));
        // Line zero is the top of the scrollback.
        terminal.scroll_to_top_line(0);
        assert!(viewport_text(&terminal).starts_with("line00"));
    }

    #[test]
    fn scroll_to_fraction_maps_to_lines() {
        let mut bytes = Vec::new();
        for i in 0..20 {
            bytes.extend_from_slice(format!("line{i:02}\n").as_bytes());
        }
        let output = BuildOutput::from_bytes(40, bytes);
        let mut terminal = Terminal::new(&output, 40, 5);

        terminal.scroll_to_fraction(1.0);
        assert!(viewport_text(&terminal).contains("line19"));
        terminal.scroll_to_fraction(0.0);
        assert!(viewport_text(&terminal).starts_with("line00"));
    }

    #[test]
    fn selection_yields_text_and_spans() {
        let output = BuildOutput::from_bytes(40, b"alpha\nbeta\ngamma\n".to_vec());
        let mut terminal = Terminal::new(&output, 40, 10);
        // Select "lph": from the left of column 1 to the right of column 3.
        terminal.begin_selection(
            SelectionType::Simple,
            CellPos { column: 1, row: 0 },
            Side::Left,
        );
        terminal.extend_selection(CellPos { column: 3, row: 0 }, Side::Right);
        assert_eq!(terminal.selection_text().as_deref(), Some("lph"));
        let spans = terminal.selection_spans();
        assert_eq!(spans[0], Some((1, 3)));
        assert_eq!(spans[1], None);

        terminal.clear_selection();
        assert_eq!(terminal.selection_text(), None);
        assert!(terminal.selection_spans().iter().all(Option::is_none));
    }

    #[test]
    fn selection_survives_a_rebuild_with_more_output() {
        // Select "beta" on the second line of an early frame.
        let early = BuildOutput::from_bytes(40, b"alpha\nbeta\ngamma\n".to_vec());
        let mut terminal = Terminal::new(&early, 40, 10);
        terminal.begin_selection(
            SelectionType::Simple,
            CellPos { column: 0, row: 1 },
            Side::Left,
        );
        terminal.extend_selection(CellPos { column: 3, row: 1 }, Side::Right);
        assert_eq!(terminal.selection_text().as_deref(), Some("beta"));
        let span = terminal.selection_span().expect("a captured selection");

        // A later frame appends more output; the selection re-applies to the same
        // text, which keeps its position counted from the top.
        let later = BuildOutput::from_bytes(40, b"alpha\nbeta\ngamma\ndelta\nepsilon\n".to_vec());
        let mut rebuilt = Terminal::new(&later, 40, 10);
        assert_eq!(rebuilt.selection_text(), None);
        rebuilt.restore_selection(span);
        assert_eq!(rebuilt.selection_text().as_deref(), Some("beta"));
    }

    #[test]
    fn drag_select_spans_from_content_anchor_to_end_cell() {
        let output = BuildOutput::from_bytes(40, b"alpha\nbeta\ngamma\n".to_vec());
        let mut terminal = Terminal::new(&output, 40, 10);
        // Anchor at the start of the top line, far end inside the third line.
        terminal.drag_select((0, 0, Side::Left), (2, 2, Side::Right));
        assert_eq!(
            terminal.selection_text().as_deref(),
            Some("alpha\nbeta\ngam")
        );
    }

    #[test]
    fn search_finds_navigates_and_highlights() {
        let mut bytes = Vec::new();
        for i in 0..20 {
            bytes.extend_from_slice(format!("item {i:02} needle\n").as_bytes());
        }
        let output = BuildOutput::from_bytes(40, bytes);
        let mut terminal = Terminal::new(&output, 40, 5);

        // Every line carries one match; the first is focused and scrolled to.
        assert_eq!(terminal.set_search("needle"), 20);
        assert_eq!(terminal.current_match(), Some(1));
        assert!(viewport_text(&terminal).contains("item 00"));

        // Navigation wraps and moves the focus.
        terminal.search_prev();
        assert_eq!(terminal.current_match(), Some(20));
        assert!(viewport_text(&terminal).contains("item 19"));
        terminal.search_next();
        assert_eq!(terminal.current_match(), Some(1));

        // The focused match on the first visible row is flagged.
        let focused = terminal.search_spans()[0]
            .iter()
            .any(|&(_, _, focused)| focused);
        assert!(focused);

        terminal.clear_search();
        assert_eq!(terminal.search_count(), 0);
    }

    #[test]
    fn restore_search_keeps_the_focused_match() {
        let mut bytes = Vec::new();
        for i in 0..20 {
            bytes.extend_from_slice(format!("item {i:02} needle\n").as_bytes());
        }
        let output = BuildOutput::from_bytes(40, bytes);
        let mut terminal = Terminal::new(&output, 40, 5);

        terminal.set_search("needle");
        terminal.search_next();
        assert_eq!(terminal.current_match(), Some(2));

        // Re-applying the search (as a playback rebuild does) keeps the focused
        // match rather than resetting to the first.
        let current = terminal.current_match_index();
        terminal.restore_search("needle", current);
        assert_eq!(terminal.search_count(), 20);
        assert_eq!(terminal.current_match(), Some(2));

        // A focused index beyond the new match count clamps to the last match.
        terminal.restore_search("needle", Some(999));
        assert_eq!(terminal.current_match(), Some(20));

        // An empty query clears the search.
        terminal.restore_search("", Some(0));
        assert_eq!(terminal.search_count(), 0);
        assert_eq!(terminal.current_match(), None);
    }

    #[test]
    fn search_query_is_literal() {
        let output = BuildOutput::from_bytes(40, b"a.b axb a.b\n".to_vec());
        let mut terminal = Terminal::new(&output, 40, 5);
        // "a.b" matches literally (twice), not as the regex a-any-b (which would
        // also match "axb").
        assert_eq!(terminal.set_search("a.b"), 2);
    }

    #[test]
    fn detects_url_under_cell() {
        let output =
            BuildOutput::from_bytes(80, b"see https://example.com/path for details.\n".to_vec());
        let mut terminal = Terminal::new(&output, 80, 10);
        // "see " is four columns, so the URL starts at column 4.
        let url = terminal.url_at(10, 0).expect("url under the link");
        assert_eq!(url.text(), "https://example.com/path");
        assert_eq!(url.spans()[0], Some((4, 27)));

        // No URL over the leading prose.
        assert!(terminal.url_at(1, 0).is_none());
    }

    #[test]
    fn lists_every_url_the_viewport_draws() {
        let output = BuildOutput::from_bytes(
            20,
            b"a http://a.example/x and\nwrapped https://b.example/a/long/path.\n".to_vec(),
        );
        let mut terminal = Terminal::new(&output, 20, 10);
        let urls = terminal.urls();
        let texts: Vec<&str> = urls.iter().map(Url::text).collect();
        assert_eq!(
            texts,
            ["http://a.example/x", "https://b.example/a/long/path"]
        );
        let wrapped = &urls[1].spans()[..4];
        assert_eq!(wrapped, [None, None, Some((8, 19)), Some((0, 16))]);
    }

    #[test]
    fn trims_trailing_punctuation_from_url() {
        let output = BuildOutput::from_bytes(80, b"go to http://example.com.\n".to_vec());
        let mut terminal = Terminal::new(&output, 80, 10);
        let url = terminal.url_at(8, 0).expect("url under the link");
        assert_eq!(url.text(), "http://example.com");
    }

    /// A URL may contain an apostrophe, so the pattern matches through one and
    /// the quotes a tool writes around a URL land in the match.
    #[test]
    fn trims_the_quotes_a_tool_writes_around_a_url() {
        let output = BuildOutput::from_bytes(
            80,
            b"fatal: repository 'http://example.com/foo' not found\n".to_vec(),
        );
        let mut terminal = Terminal::new(&output, 80, 10);
        let url = terminal.url_at(25, 0).expect("url under the link");
        assert_eq!(url.text(), "http://example.com/foo");
    }

    #[test]
    fn semantic_and_line_selection_expand() {
        let output = BuildOutput::from_bytes(40, b"alpha beta\n".to_vec());
        let mut terminal = Terminal::new(&output, 40, 10);

        // A word selection anchored inside "beta" expands to the whole word.
        terminal.begin_selection(
            SelectionType::Semantic,
            CellPos { column: 7, row: 0 },
            Side::Left,
        );
        assert_eq!(terminal.selection_text().as_deref(), Some("beta"));

        // A line selection takes the whole line regardless of the column.
        terminal.begin_selection(
            SelectionType::Lines,
            CellPos { column: 2, row: 0 },
            Side::Left,
        );
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
        let output = BuildOutput::from_bytes(40, "a中b\n".as_bytes().to_vec());
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
        let output = BuildOutput::from_bytes(40, b"alpha\nbeta\ngamma\n".to_vec());
        let terminal = Terminal::new(&output, 40, 10);
        assert_eq!(viewport_text(&terminal), "alpha\nbeta\ngamma");
    }
}
