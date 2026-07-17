use super::*;

impl Terminal {
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
    pub(super) fn row_column_span(
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
}
