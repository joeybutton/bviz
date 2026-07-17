use super::*;

impl Terminal {
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
