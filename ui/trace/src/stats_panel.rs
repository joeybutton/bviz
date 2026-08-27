use crate::chart::format_duration;
use crate::model::{SourceReplaced, TraceModel};
use crate::rebuild::Rebuild;
use crate::theme::Palette;
use crate::{
    STATS_CELL_PAD, STATS_HEADER_HEIGHT, STATS_NUM_COL_WIDTH, STATS_ROW_HEIGHT, STATS_TEXT_SIZE,
    panel_toolbar, profile_status, set_list_thumb, thumb_offset, time_window_chip,
};
use bviz_trace_layout::{FlameLabels, ScopeAggregate, StatRow};
use bviz_ui_components::{
    Axis, DEFAULT_MIN_THUMB, DragEnd, SCROLLBAR_THICKNESS, Scrollbar, centered_spinner,
    load_in_background, overlay_canvas, track_drag,
};
use bviz_ui_workspace::{Panel, PanelEvent, PanelKind};
use gpui::{
    App, Bounds, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    MouseButton, MouseDownEvent, Pixels, Point, Render, Rgba, SharedString, Subscription,
    UniformListScrollHandle, Window, div, point, prelude::*, px, size, uniform_list,
};

/// The key context scoping the stats table's own bindings, so a key pressed here
/// reaches this panel and no other.
const STATS_KEY_CONTEXT: &str = "TraceStats";
use std::rc::Rc;
use std::sync::Arc;

/// A sortable column of the graph-mode stats table.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum StatsColumn {
    Key,
    Count,
    SelfTime,
    Avg,
    Max,
    Percent,
}

/// One column of the stats table: which value it sorts by and its fixed width
/// (`None` for the one flexible column that takes the remainder). The header and
/// every body cell are laid out from this single spec so their widths cannot
/// disagree.
struct StatsColumnSpec {
    column: StatsColumn,
    width: Option<f32>,
}

const STATS_COLUMNS: [StatsColumnSpec; 6] = [
    StatsColumnSpec {
        column: StatsColumn::Key,
        width: None,
    },
    StatsColumnSpec {
        column: StatsColumn::Count,
        width: Some(STATS_NUM_COL_WIDTH),
    },
    StatsColumnSpec {
        column: StatsColumn::SelfTime,
        width: Some(STATS_NUM_COL_WIDTH),
    },
    StatsColumnSpec {
        column: StatsColumn::Avg,
        width: Some(STATS_NUM_COL_WIDTH),
    },
    StatsColumnSpec {
        column: StatsColumn::Max,
        width: Some(STATS_NUM_COL_WIDTH),
    },
    StatsColumnSpec {
        column: StatsColumn::Percent,
        width: Some(STATS_NUM_COL_WIDTH),
    },
];

/// The header text of `column`. Rows are keyed by whatever text labels the
/// graph's frames, so the key column is named for `labels`.
fn column_header(column: StatsColumn, labels: FlameLabels) -> &'static str {
    match column {
        StatsColumn::Key => match labels {
            FlameLabels::Mnemonic => "Mnemonic",
            FlameLabels::Name => "Name",
        },
        StatsColumn::Count => "Count",
        StatsColumn::SelfTime => "Self",
        StatsColumn::Avg => "Avg",
        StatsColumn::Max => "Max",
        StatsColumn::Percent => "%",
    }
}

/// The sort the stats table opens with. Every aggregate ships this order
/// precomputed ([`ScopeAggregate::default_order`]), so serving it costs no sort.
const DEFAULT_STATS_SORT: (StatsColumn, bool) = (StatsColumn::SelfTime, true);

/// The stats rows in sorted display order, as indices into `aggregates.stats`.
/// Holds its own `Arc<ScopeAggregate>` so the table renders consistent rows even
/// mid-reswap; the percent denominator is the aggregate's `grand_total`. Only the
/// visible rows are formatted, lazily during paint, so no per-row strings are
/// retained.
pub(crate) struct StatsOrder {
    aggregates: Arc<ScopeAggregate>,
    aggregate_version: u64,
    /// Row order for a non-default sort, built off the UI thread: sorting ~2M
    /// rows costs far more than a frame budget. `None` for the default sort,
    /// whose order ships precomputed inside `aggregates`.
    sorted: Option<Vec<u32>>,
}

impl StatsOrder {
    fn order(&self) -> &[u32] {
        self.sorted
            .as_deref()
            .unwrap_or(&self.aggregates.default_order)
    }
}

/// A row the reader has picked out, as an index into the `stats` of the
/// aggregate `aggregate_version` names. Only that aggregate can resolve the
/// index, so a rebuilt aggregate lapses the pick; a re-sort reorders the rows
/// without renumbering them, so the pick keeps its group.
#[derive(Clone, Copy)]
struct StatsPick {
    aggregate_version: u64,
    index: u32,
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
        StatsColumn::SelfTime | StatsColumn::Percent => bias(r.self_us),
        StatsColumn::Avg => bias(r.avg_us()),
        StatsColumn::Max => bias(r.max_us),
    };
    // A descending numeric sort inverts its primary; for the Key column the
    // primary IS the tie-break key, whose descending header state shows
    // ascending text.
    let invert = if column == StatsColumn::Key {
        !desc
    } else {
        desc
    };
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
fn stats_cell(spec: &StatsColumnSpec, content: impl IntoIterator<Item = impl IntoElement>) -> Div {
    let cell = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .h_full()
        .px(px(STATS_CELL_PAD))
        .overflow_hidden()
        .whitespace_nowrap()
        .children(content);
    match spec.width {
        Some(w) => cell.w(px(w)).flex_shrink_0().justify_end(),
        None => cell.flex_1().min_w_0().text_ellipsis(),
    }
}

/// What a left-button drag over the stats panel is currently moving.
#[derive(Clone, Copy)]
enum StatsDrag {
    None,
    /// A press on the scroll thumb/track, holding that grab offset from the
    /// thumb's leading edge.
    Thumb(f32),
}

/// The graph-mode stats table as a self-contained panel: a header band of
/// clickable, sortable columns over a scrollable, virtualized body of one row per
/// group, reading the aggregate from [`TraceModel`]. Owns its sort, scroll, and
/// drag state as plain fields.
pub(crate) struct StatsPanel {
    /// Focus for the panel, so the window's own actions dispatch while it is the
    /// panel on show.
    focus: FocusHandle,
    /// Read for the current selection's aggregate and the version its sort keys
    /// on.
    model: Entity<TraceModel>,
    /// Which column the table sorts by, and whether descending.
    stats_sort: StatsColumn,
    stats_desc: bool,
    /// The stats rows in sorted display order for the active key and sort, built
    /// off the UI thread (sorting ~2M rows would freeze a paint) and swapped in
    /// when ready. The previous order stays shown while a re-sort runs, so a
    /// header click doesn't blank the table.
    stats_order: Option<Rc<StatsOrder>>,
    /// Background-sort bookkeeping for `stats_order`, keyed on
    /// `(aggregate version, sort, desc)`.
    stats_build: Rebuild<(u64, StatsColumn, bool)>,
    /// Scroll position of the virtualized body.
    stats_scroll: UniformListScrollHandle,
    picked: Option<StatsPick>,
    drag: StatsDrag,
    /// Rebuilds the sort order when the model swaps in a freshly built aggregate:
    /// the order derives from the aggregate, which lands asynchronously.
    _model_observer: Subscription,
    /// Drops the order built over the document being replaced.
    _source_replaced: Subscription,
}

impl StatsPanel {
    pub(crate) fn new(model: Entity<TraceModel>, cx: &mut Context<Self>) -> Self {
        // Kick the re-sort here rather than waiting for a render, so the order
        // stays current even while the Stats tab is hidden — switching to it then
        // shows the sorted rows without a rebuild, instead of a stale flash or
        // spinner.
        let model_observer = cx.observe(&model, |this, _, cx| {
            this.ensure_stats_order(cx);
            cx.notify();
        });
        let source_replaced = cx.subscribe(&model, |this, _, _: &SourceReplaced, cx| {
            this.reset_for_new_profile();
            cx.notify();
        });
        Self {
            focus: cx.focus_handle(),
            model,
            stats_sort: DEFAULT_STATS_SORT.0,
            stats_desc: DEFAULT_STATS_SORT.1,
            stats_order: None,
            stats_build: Rebuild::new(),
            stats_scroll: UniformListScrollHandle::new(),
            picked: None,
            drag: StatsDrag::None,
            _model_observer: model_observer,
            _source_replaced: source_replaced,
        }
    }

    /// Drops the shown order and abandons any in-flight sort, so a freshly loaded
    /// profile shows the sort spinner rather than the prior selection's rows until
    /// its aggregate lands, and returns the body to the first row.
    fn reset_for_new_profile(&mut self) {
        self.stats_order = None;
        self.stats_build.invalidate();
        self.picked = None;
        set_list_thumb(&self.stats_scroll.0.borrow().base_handle, 0.0);
    }

    /// The graph-mode stats table: a header band of clickable, sortable columns
    /// over a scrollable, virtualized body of one row per group, themed like the
    /// rest of the view. The rows are aggregated over the same scope as the
    /// flame graph and sorted by the active column; the percent column is each
    /// group's share of the summed self times (so it totals 100%).
    ///
    /// Header and body cells are laid out from the one [`STATS_COLUMNS`] spec
    /// through [`stats_cell`], so their widths cannot disagree at any window
    /// width: each fixed column reserves its pixel width without shrinking, and
    /// the one flexible column takes the remainder.
    fn stats_table(&self, palette: &Palette, cx: &mut Context<Self>) -> Div {
        // The key column names the labelling the shown rows were keyed by, which
        // trails the model's mode for as long as a rebuild takes; with no rows
        // up there is nothing for the model's mode to disagree with.
        let labels = match &self.stats_order {
            Some(order) => order.aggregates.mode.labels,
            None => self.model.read(cx).flame_mode().labels,
        };
        // Reserve the scroll bar's width on the right of the header and every row
        // (the bar is painted as an overlay there), so the last column's values
        // don't run under or up against the bar.
        let header = div()
            .flex()
            .flex_row()
            .h(px(STATS_HEADER_HEIGHT))
            .w_full()
            .pr(px(SCROLLBAR_THICKNESS))
            .bg(palette.header_band)
            .border_b_1()
            .border_color(palette.border)
            .text_color(palette.label)
            .children(STATS_COLUMNS.iter().enumerate().map(|(i, spec)| {
                let column = spec.column;
                let caret = (self.stats_sort == column).then(|| {
                    let name = if self.stats_desc {
                        "caret-down"
                    } else {
                        "caret-up"
                    };
                    bviz_ui_assets::icon(name)
                        .size(px(STATS_TEXT_SIZE))
                        .flex_shrink_0()
                        .text_color(palette.label)
                        .into_any_element()
                });
                let label = column_header(column, labels).into_any_element();
                // Place the caret on the side away from the text's anchor so the
                // label can't shift as the sort indicator moves: left of a
                // right-aligned numeric value, right of the left-aligned key.
                let content = match (caret, spec.width.is_some()) {
                    (Some(caret), true) => vec![caret, label],
                    (Some(caret), false) => vec![label, caret],
                    (None, _) => vec![label],
                };
                stats_cell(spec, content)
                    .id(SharedString::from(format!("stats-h-{i}")))
                    .cursor_pointer()
                    .hover(|s| s.bg(palette.hover))
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
        let Some(order) = self.stats_order.clone() else {
            return table.child(centered_spinner("Sorting…", &palette.colors));
        };

        let row_count = order.order().len();
        let row_hover = palette.hover;
        let row_pick = palette.selection;
        let row_pick_hover = palette.picked_hover();
        let picked = self.picked_row(&order);
        let version = order.aggregate_version;
        let entity = cx.entity();
        let body = uniform_list("stats-body", row_count, move |range, _window, _cx| {
            let graph = &order.aggregates.flame;
            range
                .map(|i| {
                    // Format only the visible rows here: formatting all ~2M up
                    // front froze the UI thread for seconds and retained a huge
                    // string structure that was slow to drop.
                    let index = order.order()[i];
                    let row = order.aggregates.stats[index as usize];
                    let key = graph.str(row.key);
                    let pct =
                        100.0 * row.self_us as f64 / order.aggregates.grand_total.max(1) as f64;
                    let entity = entity.clone();
                    let is_picked = picked == Some(index);
                    let hover_fill = if is_picked { row_pick_hover } else { row_hover };
                    let cells = STATS_COLUMNS.iter().map(|spec| {
                        let text: SharedString = match spec.column {
                            StatsColumn::Key => key.to_string().into(),
                            StatsColumn::Count => row.count.to_string().into(),
                            StatsColumn::SelfTime => format_duration(row.self_us).into(),
                            StatsColumn::Avg => format_duration(row.avg_us()).into(),
                            StatsColumn::Max => format_duration(row.max_us).into(),
                            StatsColumn::Percent => format!("{pct:.1}%").into(),
                        };
                        stats_cell(spec, [text])
                    });
                    // The row's fill runs under the reserved scroll-bar strip
                    // as well as under the cells, and only the cells take a
                    // click, so a press that drives the bar picks no row. The
                    // fill's id is what gets its hover repainted.
                    div()
                        .id(("stats-row", index as usize))
                        .flex()
                        .flex_row()
                        .h(px(STATS_ROW_HEIGHT))
                        .w_full()
                        .pr(px(SCROLLBAR_THICKNESS))
                        .when(is_picked, |d| d.bg(row_pick))
                        .hover(move |s| s.bg(hover_fill))
                        .child(
                            div()
                                .id(("stats-row-cells", index as usize))
                                .flex()
                                .flex_row()
                                .items_center()
                                .size_full()
                                .cursor_pointer()
                                .on_click(move |_, _, cx: &mut App| {
                                    entity.update(cx, |panel, cx| {
                                        panel.picked = Some(StatsPick {
                                            aggregate_version: version,
                                            index,
                                        });
                                        cx.notify();
                                    });
                                })
                                .children(cells),
                        )
                })
                .collect()
        })
        .track_scroll(&self.stats_scroll);

        let bar = stats_scrollbar(
            cx.entity(),
            matches!(self.drag, StatsDrag::Thumb(_)),
            palette.scroll.track,
            palette.scroll.thumb,
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
    fn ensure_stats_order(&mut self, cx: &mut Context<Self>) {
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
            self.stats_order = Some(Rc::new(StatsOrder {
                aggregates,
                aggregate_version: version,
                sorted: None,
            }));
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
                StatsOrder {
                    aggregates,
                    aggregate_version: version,
                    sorted: Some(order),
                }
            },
            move |panel, stats_order, _cx| {
                // Discard a result the inputs have moved past since this spawn.
                if !panel.stats_build.accept(generation, key) {
                    return;
                }
                panel.stats_order = Some(Rc::new(stats_order));
            },
        );
        self.stats_build.set_task(task);
    }

    fn picked_row(&self, order: &StatsOrder) -> Option<u32> {
        let picked = self.picked?;
        (picked.aggregate_version == order.aggregate_version).then_some(picked.index)
    }

    /// Sets the sort to `column`, toggling the direction when it is already the
    /// active column.
    fn sort_by(&mut self, column: StatsColumn) {
        if self.stats_sort == column {
            self.stats_desc = !self.stats_desc;
        } else {
            self.stats_sort = column;
            self.stats_desc = true;
        }
    }

    /// The bar for the rows' content height against the body's visible height;
    /// `None` while no order exists or the rows fit.
    fn vbar(&self) -> Option<Scrollbar> {
        self.stats_order.as_ref()?;
        let state = self.stats_scroll.0.borrow();
        let body = state.base_handle.bounds();
        let visible = f32::from(body.size.height);
        Scrollbar::new(
            Axis::Vertical,
            Bounds::new(
                point(body.right() - px(SCROLLBAR_THICKNESS), body.origin.y),
                size(px(SCROLLBAR_THICKNESS), body.size.height),
            ),
            visible + f32::from(state.base_handle.max_offset().y),
            visible,
            -f32::from(state.base_handle.offset().y),
            DEFAULT_MIN_THUMB,
        )
    }

    /// Begins a scroll drag if `pos` hit the bar; otherwise leaves the press to
    /// fall through to a row.
    fn press(&mut self, pos: Point<Pixels>) {
        if let Some(bar) = self.vbar()
            && bar.track_hit(pos)
        {
            let grab = bar.grab(pos);
            self.drag = StatsDrag::Thumb(grab);
            self.thumb_to(pos, grab);
        }
    }

    /// Maps a thumb drag back to the list's scroll offset.
    fn thumb_to(&mut self, pos: Point<Pixels>, grab: f32) {
        if let Some(offset) = thumb_offset(self.vbar(), pos, grab) {
            set_list_thumb(&self.stats_scroll.0.borrow().base_handle, offset);
        }
    }
}

impl EventEmitter<PanelEvent> for StatsPanel {}

impl Focusable for StatsPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Panel for StatsPanel {
    fn kind() -> PanelKind {
        PanelKind {
            name: "trace-stats",
            key_context: STATS_KEY_CONTEXT,
        }
    }

    fn title(&self, _cx: &App) -> SharedString {
        "Stats".into()
    }

    fn status(&self, cx: &App) -> Option<SharedString> {
        Some(profile_status(&self.model, cx))
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if active {
            self.focus.focus(window, cx);
        }
    }
}

impl Render for StatsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Re-sort off the UI thread when the aggregates or sort change; sorting
        // ~2M rows on the paint thread would freeze it.
        self.ensure_stats_order(cx);
        let palette = Palette::from_theme(&bviz_ui_theme::theme(window, cx));
        let toolbar = time_window_chip(&self.model, palette, cx)
            .map(|chip| panel_toolbar(palette).child(chip));
        div()
            .key_context(STATS_KEY_CONTEXT)
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .children(toolbar)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, e: &MouseDownEvent, _, cx| {
                            // A press on the scroll thumb/track drives the bar; the
                            // canvas paint re-installs the drag handlers while the
                            // drag is set, so repaint now.
                            this.press(e.position);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .on_scroll_wheel(|_, _, cx: &mut App| cx.stop_propagation())
                    .child(self.stats_table(&palette, cx)),
            )
    }
}

/// The stats table's scroll bar, drawn over the body once the list has been
/// laid out; keeps the drag handlers installed while `dragging`.
fn stats_scrollbar(
    entity: Entity<StatsPanel>,
    dragging: bool,
    track: Rgba,
    thumb: Rgba,
) -> impl IntoElement {
    overlay_canvas(move |_, window: &mut Window, cx: &mut App| {
        if let Some(bar) = entity.read(cx).vbar() {
            bar.paint(track, thumb, window);
        }
        if dragging {
            track_drag(window, &entity, stats_on_drag, stats_end_drag);
        }
    })
}

/// Routes an in-progress thumb drag to the list's scroll offset.
fn stats_on_drag(panel: &mut StatsPanel, position: Point<Pixels>, cx: &mut Context<StatsPanel>) {
    if let StatsDrag::Thumb(grab) = panel.drag {
        panel.thumb_to(position, grab);
    }
    cx.notify();
}

fn stats_end_drag(
    panel: &mut StatsPanel,
    _: Point<Pixels>,
    _: DragEnd,
    cx: &mut Context<StatsPanel>,
) {
    panel.drag = StatsDrag::None;
    cx.notify();
}
