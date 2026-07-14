//! A display terminal: the build's reconstructed output laid out for a window of
//! a given size. The reconstructed screen is serialized to a clean ANSI stream
//! and replayed into an alacritty terminal sized to the window, so alacritty
//! wraps it; resizing re-wraps and reflows, and scrolling moves the viewport.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Cell;
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use alacritty_terminal::term::Term;

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
use crate::{build_term, snapshot_rows};

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
    pub(crate) fn from_bytes(bytes: &[u8], columns: u16) -> DisplaySource {
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

impl Terminal {
    /// Reconstruct the build's final screen (at the width Bazel used) and replay
    /// it into a display terminal of `columns` by `rows`. Reconstruction happens
    /// inline; [`from_display`](Terminal::from_display) takes a prepared
    /// [`DisplaySource`] so the heavy step can run off the UI thread.
    pub fn new(output: &BuildOutput, columns: usize, rows: usize) -> Terminal {
        Terminal::from_display(&DisplaySource::from_output(output), columns, rows)
    }

    /// Replay an already-reconstructed [`DisplaySource`] into a display terminal
    /// of `columns` by `rows`, which wraps it to that width. The viewport starts
    /// at the first line of the build.
    pub fn from_display(source: &DisplaySource, columns: usize, rows: usize) -> Terminal {
        let columns = columns.max(1);
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

    /// Scroll so that the line `line` rows from the top of the scrollback sits at
    /// the top of the viewport, clamped to the available history. Used to hold a
    /// scroll position across a rebuild as content grows below it.
    pub fn scroll_to_top_line(&mut self, line: usize) {
        // The viewport top is `history_size - display_offset` lines from the top,
        // so the offset that puts `line` there is `history_size - line`.
        let target = self.history_size().saturating_sub(line) as i32;
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

    /// Capture the current selection in content coordinates — lines counted from
    /// the top of the scrollback — so it can be re-created on a rebuilt grid whose
    /// history has grown. `None` when nothing is selected.
    pub fn selection_span(&self) -> Option<SelectionSpan> {
        let range = self
            .term
            .selection
            .as_ref()
            .and_then(|selection| selection.to_range(&self.term))?;
        let from_top = |point: Point| (self.grid_to_content_line(point.line), point.column.0);
        Some(SelectionSpan {
            start: from_top(range.start),
            end: from_top(range.end),
        })
    }

    /// Set an in-progress drag selection: the anchor in content coordinates (a
    /// line counted from the top of the scrollback, a column, and a side) and the
    /// far end at a viewport cell. The anchor stays on its text across rebuilds
    /// while the end follows the cursor's cell, so the selection grows as the view
    /// scrolls under a held cursor.
    pub fn drag_select(&mut self, anchor: (i32, usize, Side), end: (usize, usize, Side)) {
        let (anchor_line, anchor_column, anchor_side) = anchor;
        let (end_column, end_row, end_side) = end;
        let anchor_point = Point::new(
            self.content_to_grid_line(anchor_line),
            Column(anchor_column.min(self.columns.saturating_sub(1))),
        );
        let mut selection = Selection::new(SelectionType::Simple, anchor_point, anchor_side);
        selection.update(self.grid_point(end_column, end_row), end_side);
        self.term.selection = Some(selection);
    }

    /// Re-create a captured selection on the current grid, mapping its content
    /// lines back to grid lines (and clamping to the grid). Used to keep a
    /// selection across a playback frame change. Re-created as a simple range,
    /// which reproduces the selected cells for the selection kinds made here.
    pub fn restore_selection(&mut self, span: SelectionSpan) {
        let last_column = self.columns.saturating_sub(1);
        let to_point = |(from_top, column): (i32, usize)| {
            Point::new(
                self.content_to_grid_line(from_top),
                Column(column.min(last_column)),
            )
        };
        let mut selection = Selection::new(SelectionType::Simple, to_point(span.start), Side::Left);
        selection.update(to_point(span.end), Side::Right);
        self.term.selection = Some(selection);
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
        let (start, end) = self.url_match_at(point)?;
        let full = self.term.bounds_to_string(start, end);
        let (text, end) = Self::trim_trailing_punctuation(&full, end);
        if text.is_empty() {
            return None;
        }
        Some(Url {
            text,
            spans: self.point_range_spans(start, end),
        })
    }

    /// The inclusive linear grid bounds of the URL regex match containing
    /// `point` on its logical line, or `None` when no match covers it.
    fn url_match_at(&mut self, point: Point) -> Option<(Point, Point)> {
        let line_start = self.term.line_search_left(point);
        let line_end = self.term.line_search_right(point);
        for m in RegexIter::new(
            line_start,
            line_end,
            Direction::Right,
            &self.term,
            &mut self.url_regex,
        ) {
            if m.contains(&point) {
                return Some((*m.start(), *m.end()));
            }
            if *m.start() > point {
                break;
            }
        }
        None
    }

    /// Trailing sentence punctuation usually belongs to the prose, not the URL;
    /// drop it from both the opened text and the highlighted span's end column.
    fn trim_trailing_punctuation(full: &str, end: Point) -> (String, Point) {
        let kept = full
            .trim_end_matches(['.', ',', ';', ':', '!', '?', ')'])
            .chars()
            .count();
        let removed = full.chars().count() - kept;
        let end = if removed > 0 && removed <= end.column.0 {
            Point::new(end.line, Column(end.column.0 - removed))
        } else {
            end
        };
        let text: String = full.chars().take(kept).collect();
        (text, end)
    }

    /// The inclusive column span of the linear grid range `start..=end` (reading
    /// order) on visible row `v`, clamped to the grid, or `None` when the row
    /// falls outside the range. `offset` is the current display offset.
    fn row_column_span(
        &self,
        v: usize,
        offset: i32,
        start: Point,
        end: Point,
    ) -> Option<(usize, usize)> {
        let line = v as i32 - offset;
        if line < start.line.0 || line > end.line.0 {
            return None;
        }
        let last_column = self.columns.saturating_sub(1);
        let first = if line == start.line.0 {
            start.column.0
        } else {
            0
        };
        let last = if line == end.line.0 {
            end.column.0
        } else {
            last_column
        };
        (first <= last).then_some((first.min(last_column), last.min(last_column)))
    }

    /// Per visible row, the inclusive column span covered by the linear grid
    /// range `start..=end` (reading order).
    fn point_range_spans(&self, start: Point, end: Point) -> Vec<Option<(usize, usize)>> {
        let offset = self.display_offset() as i32;
        (0..self.rows)
            .map(|v| self.row_column_span(v, offset, start, end))
            .collect()
    }

    /// Find every literal occurrence of `query`, focus the first, and scroll it
    /// into view. Returns the number of matches. An empty query clears the search.
    pub fn set_search(&mut self, query: &str) -> usize {
        self.search_matches = self.find_matches(query);
        self.current_match = (!self.search_matches.is_empty()).then_some(0);
        if let Some(index) = self.current_match {
            self.scroll_to_match(index);
        }
        self.search_matches.len()
    }

    /// Re-run `query` against the current (reflowed or replayed) content, keeping
    /// the focused match at `current` clamped to the new match count and leaving
    /// the viewport as is. Used to carry a search across a playback frame change
    /// without snapping back to the first match or scrolling.
    pub fn restore_search(&mut self, query: &str, current: Option<usize>) {
        self.search_matches = self.find_matches(query);
        self.current_match = (!self.search_matches.is_empty())
            .then(|| current.unwrap_or(0).min(self.search_matches.len() - 1));
    }

    /// All matches of a literal `query` in reading order; empty for an empty or
    /// invalid query.
    fn find_matches(&self, query: &str) -> Vec<Match> {
        if query.is_empty() {
            return Vec::new();
        }
        let Ok(mut regex) = RegexSearch::new(&regex_escape(query)) else {
            return Vec::new();
        };
        let grid = self.term.grid();
        let start = Point::new(grid.topmost_line(), Column(0));
        let end = Point::new(grid.bottommost_line(), grid.last_column());
        RegexIter::new(start, end, Direction::Right, &self.term, &mut regex).collect()
    }

    pub fn clear_search(&mut self) {
        self.search_matches.clear();
        self.current_match = None;
    }

    /// Focus the next match, wrapping around, and scroll it into view.
    pub fn search_next(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        let next = self
            .current_match
            .map_or(0, |i| (i + 1) % self.search_matches.len());
        self.current_match = Some(next);
        self.scroll_to_match(next);
    }

    /// Focus the previous match, wrapping around, and scroll it into view.
    pub fn search_prev(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        let len = self.search_matches.len();
        let prev = self.current_match.map_or(len - 1, |i| (i + len - 1) % len);
        self.current_match = Some(prev);
        self.scroll_to_match(prev);
    }

    fn scroll_to_match(&mut self, index: usize) {
        let point = *self.search_matches[index].start();
        self.term.scroll_to_point(point);
    }

    pub fn search_count(&self) -> usize {
        self.search_matches.len()
    }

    /// The focused match's position among the matches, one-based for display.
    pub fn current_match(&self) -> Option<usize> {
        self.current_match.map(|i| i + 1)
    }

    /// The focused match's zero-based index, for carrying the selection across a
    /// rebuild via [`restore_search`](Self::restore_search).
    pub fn current_match_index(&self) -> Option<usize> {
        self.current_match
    }

    /// For each visible row, the match spans on it as `(first, last, focused)`,
    /// where `focused` marks the currently focused match.
    pub fn search_spans(&self) -> Vec<Vec<(usize, usize, bool)>> {
        let offset = self.display_offset() as i32;
        let mut rows = vec![Vec::new(); self.rows];
        for (index, m) in self.search_matches.iter().enumerate() {
            let focused = self.current_match == Some(index);
            let (start, end) = (*m.start(), *m.end());
            for (v, row) in rows.iter_mut().enumerate() {
                if let Some((first, last)) = self.row_column_span(v, offset, start, end) {
                    row.push((first, last, focused));
                }
            }
        }
        rows
    }

    /// The currently visible rows, top to bottom, at the current scroll position.
    pub fn viewport(&self) -> Vec<Vec<Cell>> {
        let grid = self.term.grid();
        let top = -(grid.display_offset() as i32);
        snapshot_rows(grid, top..=top + self.rows as i32 - 1, self.columns)
    }
}

/// Escape regex metacharacters so a find query matches literally.
fn regex_escape(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    for c in query.chars() {
        if "\\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
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
        let output = BuildOutput::from_bytes(40, b"abcdefghijkl\n".to_vec());
        let terminal = Terminal::new(&output, 6, 10);
        assert_eq!(viewport_text(&terminal), "abcdef\nghijkl");
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
    fn selection_survives_a_rebuild_with_more_output() {
        // Select "beta" on the second line of an early frame.
        let early = BuildOutput::from_bytes(40, b"alpha\nbeta\ngamma\n".to_vec());
        let mut terminal = Terminal::new(&early, 40, 10);
        terminal.begin_selection(SelectionType::Simple, 0, 1, Side::Left);
        terminal.extend_selection(3, 1, Side::Right);
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
    fn trims_trailing_punctuation_from_url() {
        let output = BuildOutput::from_bytes(80, b"go to http://example.com.\n".to_vec());
        let mut terminal = Terminal::new(&output, 80, 10);
        let url = terminal.url_at(8, 0).expect("url under the link");
        assert_eq!(url.text(), "http://example.com");
    }

    #[test]
    fn semantic_and_line_selection_expand() {
        let output = BuildOutput::from_bytes(40, b"alpha beta\n".to_vec());
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
