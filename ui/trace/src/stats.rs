use crate::chart::format_duration;
use crate::theme::Palette;
use crate::{
    BAR_WIDTH, Drag, STATS_CELL_PAD, STATS_HEADER_HEIGHT, STATS_NUM_COL_WIDTH, STATS_ROW_HEIGHT,
    STATS_TEXT_SIZE, TraceView, scrollbar_canvas, set_list_thumb, thumb_offset,
};
use bviz_trace_layout::{ScopeAggregate, StatRow};
use bviz_ui_components::{centered_spinner, load_in_background};
use gpui::{Context, Div, Pixels, Point, SharedString, div, prelude::*, px, uniform_list};
use std::rc::Rc;
use std::sync::Arc;

/// A sortable column of the graph-mode stats table.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum StatsColumn {
    Key,
    Count,
    Total,
    Avg,
    Max,
    Percent,
}

/// One column of the stats table: which value it sorts by, its header label, and
/// its fixed width (`None` for the one flexible column that takes the remainder).
/// The header and every body cell are laid out from this single spec so their
/// widths cannot disagree.
struct StatsColumnSpec {
    column: StatsColumn,
    label: &'static str,
    width: Option<f32>,
}

const STATS_COLUMNS: [StatsColumnSpec; 6] = [
    StatsColumnSpec { column: StatsColumn::Key, label: "Mnemonic", width: None },
    StatsColumnSpec { column: StatsColumn::Count, label: "Count", width: Some(STATS_NUM_COL_WIDTH) },
    StatsColumnSpec { column: StatsColumn::Total, label: "Total", width: Some(STATS_NUM_COL_WIDTH) },
    StatsColumnSpec { column: StatsColumn::Avg, label: "Avg", width: Some(STATS_NUM_COL_WIDTH) },
    StatsColumnSpec { column: StatsColumn::Max, label: "Max", width: Some(STATS_NUM_COL_WIDTH) },
    StatsColumnSpec { column: StatsColumn::Percent, label: "%", width: Some(STATS_NUM_COL_WIDTH) },
];

/// The sort the stats table opens with. Every aggregate ships this order
/// precomputed ([`ScopeAggregate::default_order`]), so serving it costs no sort.
pub(crate) const DEFAULT_STATS_SORT: (StatsColumn, bool) = (StatsColumn::Total, true);

/// The stats rows in sorted display order, as indices into `aggregates.stats`.
/// Holds its own `Arc<ScopeAggregate>` so the table renders consistent rows even
/// mid-reswap; the percent denominator is the aggregate's `grand_total`. Only the
/// visible rows are formatted, lazily during paint, so no per-row strings are
/// retained.
pub(crate) struct StatsOrder {
    aggregates: Arc<ScopeAggregate>,
    /// Row order for a non-default sort, built off the UI thread: sorting ~2M
    /// rows costs far more than a frame budget. `None` for the default sort,
    /// whose order ships precomputed inside `aggregates`.
    sorted: Option<Vec<u32>>,
}

impl StatsOrder {
    fn order(&self) -> &[u32] {
        self.sorted.as_deref().unwrap_or(&self.aggregates.default_order)
    }
}

/// Row display order for a non-default stats sort, as indices into `rows`.
///
/// Every comparison is integer-only: a row's key rank orders exactly as its key
/// text does (byte-wise), so the Key column sorts by rank and every other column
/// tie-breaks by rank — ties by key keep the order stable regardless of input
/// order. Each row is decorated into one packed key — primary above the
/// always-ascending rank above the row index (the index is never reached, ranks
/// being distinct) — so no comparison chases a row pointer.
fn sort_stats(rows: &[StatRow], column: StatsColumn, desc: bool) -> Vec<u32> {
    // Bit offsets of the fields packed into each 128-bit sort key: primary in
    // the high 64 bits, rank in the next 32, row index in the low 32.
    const PRIMARY_SHIFT: u32 = 64;
    const RANK_SHIFT: u32 = 32;
    // Order-preserving bias from i64 to u64, so a signed value packs into the
    // unsigned key.
    let bias = |v: i64| (v as u64) ^ (1 << 63);
    let primary = |r: &StatRow| match column {
        StatsColumn::Key => r.rank as u64,
        StatsColumn::Count => r.count as u64,
        StatsColumn::Total | StatsColumn::Percent => bias(r.total_us),
        StatsColumn::Avg => bias(r.avg_us()),
        StatsColumn::Max => bias(r.max_us),
    };
    // A descending numeric sort inverts its primary; for the Key column the
    // primary IS the tie-break key, whose descending header state shows
    // ascending text.
    let invert = if column == StatsColumn::Key { !desc } else { desc };
    let mut keyed: Vec<u128> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let p = if invert { !primary(r) } else { primary(r) };
            ((p as u128) << PRIMARY_SHIFT) | ((r.rank as u128) << RANK_SHIFT) | i as u128
        })
        .collect();
    keyed.sort_unstable();
    keyed.into_iter().map(|k| k as u32).collect()
}

/// One stats-table cell, header or body, laid out per its column spec. A fixed
/// column reserves its width and never shrinks (a bare `w(px)` is only a flex
/// basis and gets shrunk unevenly between header and body when a row overflows,
/// drifting the columns out of alignment) and right-aligns its value; the
/// flexible key column takes the remainder and ellipsizes a long name. Only the
/// key column ellipsizes: a right-aligned numeric cell with `text_ellipsis`
/// truncates values that already fit, so its fixed width is just sized to hold
/// the longest value instead.
fn stats_cell(spec: &StatsColumnSpec, text: impl Into<SharedString>) -> Div {
    let cell = div()
        .flex()
        .flex_row()
        .items_center()
        .h_full()
        .px(px(STATS_CELL_PAD))
        .overflow_hidden()
        .whitespace_nowrap()
        .child(text.into());
    match spec.width {
        Some(w) => cell.w(px(w)).flex_shrink_0().justify_end(),
        None => cell.flex_1().min_w_0().text_ellipsis(),
    }
}

/// Maps a stats-table thumb drag back to the list's scroll offset.
pub(crate) fn stats_thumb_to(view: &mut TraceView, pos: Point<Pixels>, grab: f32) {
    if let Some(offset) = thumb_offset(&view.stats_bar, pos, grab) {
        set_list_thumb(&view.stats_scroll.0.borrow().base_handle, offset);
    }
}

impl TraceView {
    /// The graph-mode stats table: a header band of clickable, sortable columns
    /// over a scrollable, virtualized body of one row per group, themed like the
    /// rest of the view. The rows are aggregated over the same scope as the
    /// flame graph and sorted by the active column; the percent column is each
    /// group's share of the summed group durations (so it totals 100%).
    ///
    /// Header and body cells are laid out from the one [`STATS_COLUMNS`] spec
    /// through [`stats_cell`], so their widths cannot disagree at any window
    /// width: each fixed column reserves its pixel width without shrinking, and
    /// the one flexible column takes the remainder.
    pub(crate) fn stats_table(
        &self,
        palette: &Palette,
        colors: &bviz_ui_theme::Colors,
        cx: &mut Context<Self>,
    ) -> Div {
        // Reserve the scroll bar's width on the right of the header and every row
        // (the bar is painted as an overlay there), so the last column's values
        // don't run under or up against the bar.
        let header = div()
            .flex()
            .flex_row()
            .h(px(STATS_HEADER_HEIGHT))
            .w_full()
            .pr(px(BAR_WIDTH))
            .bg(palette.header_band)
            .border_b_1()
            .border_color(palette.border)
            .text_color(palette.label)
            .children(STATS_COLUMNS.iter().map(|spec| {
                let column = spec.column;
                let glyph = if self.stats_sort != column {
                    None
                } else if self.stats_desc {
                    Some('▾')
                } else {
                    Some('▴')
                };
                let label = spec.label;
                // Place the caret on the side away from the text's anchor so the
                // label can't shift as the sort indicator moves: left of a
                // right-aligned numeric value, right of the left-aligned key.
                let text = match (glyph, spec.width.is_some()) {
                    (Some(g), true) => format!("{g} {label}"),
                    (Some(g), false) => format!("{label} {g}"),
                    (None, _) => label.to_string(),
                };
                stats_cell(spec, text)
                    .id(SharedString::from(format!("stats-h-{}", spec.label)))
                    .cursor_pointer()
                    .hover(|s| s.bg(palette.scroll_track))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.sort_by(column);
                        cx.notify();
                    }))
            }));

        let table = div()
            .flex()
            .flex_col()
            .size_full()
            .bg(palette.background)
            .text_color(palette.label)
            .text_size(px(STATS_TEXT_SIZE))
            .child(header);

        // Until the off-thread sort lands, keep the header but show a spinner in
        // place of the body rather than an empty band.
        let Some(order) = self.stats_order.borrow().clone() else {
            return table.child(centered_spinner("Sorting…", colors));
        };

        let row_count = order.order().len();
        let scroll_track = palette.scroll_track;
        let body = uniform_list(
            "stats-body",
            row_count,
            move |range, _window, _cx| {
                let graph = &order.aggregates.flame;
                range
                    .map(|i| {
                        // Format only the visible rows here: formatting all ~2M up
                        // front froze the UI thread for seconds and retained a huge
                        // string structure that was slow to drop.
                        let row = order.aggregates.stats[order.order()[i] as usize];
                        let key = graph.str(row.key);
                        let pct = 100.0 * row.total_us as f64
                            / order.aggregates.grand_total.max(1) as f64;
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .h(px(STATS_ROW_HEIGHT))
                            .w_full()
                            .pr(px(BAR_WIDTH))
                            .hover(move |s| s.bg(scroll_track))
                            .children(STATS_COLUMNS.iter().map(|spec| {
                                let text: SharedString = match spec.column {
                                    StatsColumn::Key => key.to_string().into(),
                                    StatsColumn::Count => row.count.to_string().into(),
                                    StatsColumn::Total => format_duration(row.total_us).into(),
                                    StatsColumn::Avg => format_duration(row.avg_us()).into(),
                                    StatsColumn::Max => format_duration(row.max_us).into(),
                                    StatsColumn::Percent => format!("{pct:.1}%").into(),
                                };
                                stats_cell(spec, text)
                            }))
                    })
                    .collect()
            },
        )
        .track_scroll(&self.stats_scroll);

        let scroll = self.stats_scroll.clone();
        let bar = scrollbar_canvas(
            self.stats_bar.clone(),
            cx.entity(),
            matches!(self.drag, Drag::StatsThumb(_)),
            palette.scroll_track,
            palette.scroll_thumb,
            move |bounds| {
                let content = row_count as f32 * STATS_ROW_HEIGHT;
                let visible = f32::from(bounds.size.height);
                let offset = -f32::from(scroll.0.borrow().base_handle.offset().y);
                (content, visible, offset)
            },
        );

        table.child(
            div()
                .relative()
                .flex_1()
                .w_full()
                .child(body.size_full())
                .child(bar),
        )
    }

    /// Makes the current `(scope, sort)`'s row order the shown one. The default
    /// sort's order ships precomputed inside the aggregate, so it is served
    /// with no sort at all; any other sort runs on the background executor
    /// (sorting ~2M rows is far too costly for the UI thread) and only the row
    /// indices come back, the previous order staying shown until the new one
    /// lands. An in-flight sort whose key the inputs have moved past is
    /// abandoned. The visible rows are formatted lazily at paint. Idempotent —
    /// safe to call each frame.
    pub(crate) fn ensure_stats_order(&mut self, cx: &mut Context<Self>) {
        let Some(aggregates) = self.model.read(cx).aggregates() else {
            return;
        };
        let Some(version) = self.model.read(cx).aggregate_version() else {
            return;
        };
        let key = (version, self.stats_sort, self.stats_desc);
        if self.stats_build.is_ready(key) {
            // The shown order is already current; cancel a sort for a key the
            // inputs have since left so its late result can't clobber it.
            self.stats_build.cancel_inflight();
            return;
        }
        if (self.stats_sort, self.stats_desc) == DEFAULT_STATS_SORT {
            *self.stats_order.borrow_mut() =
                Some(Rc::new(StatsOrder { aggregates, sorted: None }));
            self.stats_build.mark_ready(key);
            // The swap supersedes any background sort still running.
            self.stats_build.cancel_inflight();
            return;
        }
        if self.stats_build.is_inflight(key) {
            return;
        }
        let generation = self.stats_build.start(key);
        let column = self.stats_sort;
        let desc = self.stats_desc;
        let task = load_in_background(
            cx,
            move || {
                let order = sort_stats(&aggregates.stats, column, desc);
                StatsOrder { aggregates, sorted: Some(order) }
            },
            move |view, stats_order, _cx| {
                // Discard a result the inputs have moved past since this spawn.
                if !view.stats_build.accept(generation, key) {
                    return;
                }
                *view.stats_order.borrow_mut() = Some(Rc::new(stats_order));
            },
        );
        self.stats_build.set_task(task);
    }

    /// Sets the stats sort to `column`, toggling the direction when it is already
    /// the active column.
    fn sort_by(&mut self, column: StatsColumn) {
        if self.stats_sort == column {
            self.stats_desc = !self.stats_desc;
        } else {
            self.stats_sort = column;
            self.stats_desc = true;
        }
    }

    /// Begins a stats-table scroll drag if `pos` hit its bar; otherwise leaves
    /// the press to fall through to a row.
    pub(crate) fn press_stats(&mut self, pos: Point<Pixels>) {
        self.begin_thumb_drag(pos, self.stats_bar.get(), Drag::StatsThumb, stats_thumb_to);
    }
}
