//! Which connector batches cover each cell of the window.

use super::stroke::Batch;
use gpui::{Pixels, Size};

/// The batches a frame has drawn over one cell of the window.
#[derive(Clone, Copy, Default)]
pub(super) struct Cell {
    /// The last batch drawn here.
    top: Option<Batch>,
    /// The last batch drawn here in an ink other than `top`'s.
    under: Option<Batch>,
}

impl Cell {
    /// The last batch drawn here in an ink other than `ink`.
    pub(super) fn last_unlike(self, ink: u32) -> Option<Batch> {
        match self.top {
            Some(top) if top.ink == ink => self.under,
            top => top,
        }
    }

    /// Records `batch` drawn here, which lies over every batch here of another
    /// ink.
    pub(super) fn draw(&mut self, batch: Batch) {
        match self.top {
            Some(top) if top.ink == batch.ink => self.top = Some(top.max(batch)),
            top => {
                self.under = top;
                self.top = Some(batch);
            }
        }
    }
}

/// Screen side of the square cells a frame tracks which batches cover.
const FOOTPRINT_CELL: f32 = 32.0;

/// The window, cut into cells that each know the batches drawn over them.
pub(super) struct Footprints {
    columns: usize,
    rows: usize,
    pub(super) cells: Vec<Cell>,
}

impl Footprints {
    pub(super) fn new(window: Size<Pixels>) -> Self {
        let count = |extent: Pixels| (f32::from(extent) / FOOTPRINT_CELL).ceil() as usize;
        let (columns, rows) = (count(window.width), count(window.height));
        Self {
            columns,
            rows,
            cells: vec![Cell::default(); columns * rows],
        }
    }

    /// Adds to `out` the cells of the window that the box bounding `corners`,
    /// grown by `reach`, covers.
    pub(super) fn cover(&self, corners: &[(f32, f32)], reach: f32, out: &mut Vec<usize>) {
        let (min, max) = corners.iter().fold(
            ((f32::MAX, f32::MAX), (f32::MIN, f32::MIN)),
            |(min, max), &(x, y)| ((min.0.min(x), min.1.min(y)), (max.0.max(x), max.1.max(y))),
        );
        let span = |lo: f32, hi: f32, cells: usize| {
            let first = ((lo - reach) / FOOTPRINT_CELL).floor().max(0.0) as usize;
            let past = (((hi + reach) / FOOTPRINT_CELL).floor() + 1.0).max(0.0) as usize;
            first..past.min(cells)
        };
        for row in span(min.1, max.1, self.rows) {
            out.extend(span(min.0, max.0, self.columns).map(|column| row * self.columns + column));
        }
    }
}
