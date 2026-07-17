use super::*;

impl Terminal {
    /// Begin a selection of the given type anchored at a viewport cell.
    pub fn begin_selection(&mut self, ty: SelectionType, cell: CellPos, side: Side) {
        let point = self.grid_point(cell.column, cell.row);
        self.term.selection = Some(Selection::new(ty, point, side));
    }

    /// Extend the in-progress selection to a viewport cell.
    pub fn extend_selection(&mut self, cell: CellPos, side: Side) {
        let point = self.grid_point(cell.column, cell.row);
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
}
