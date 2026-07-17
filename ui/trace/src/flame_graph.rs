use crate::chart::format_duration;
use crate::theme::{self, Palette};
use crate::{
    BAR_WIDTH, Drag, LABEL_MIN_PX, LINE_SCROLL_PX, METRICS, MIN_SLICE_PX, SLICE_SEAM_MIN_PX,
    SLICE_SEAM_MIX, SLICE_TEXT_PAD, STATS_TEXT_SIZE, TraceView, ZOOM_PER_NOTCH, end_drag, on_drag,
    paint_tooltip_box, thumb_offset,
};
use bviz_trace_layout::{
    FlameBox, FlameGraph, FlameMetrics, FlamePlacement, FlameView, ScopeAggregate, flame_height,
    lay_out_flame,
};
use bviz_ui_components::{Axis, DEFAULT_MIN_THUMB, Scrollbar, spinner, track_drag};
use gpui::{
    App, Bounds, ContentMask, Context, Div, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, Pixels, Point, ScrollDelta, ScrollWheelEvent, Stateful, Window, canvas, div,
    point, prelude::*, px, size,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

/// The flame graph's laid-out boxes together with the inputs that determine
/// them, so a paint reuses them unless one changed. The retained
/// `ScopeAggregate` both keeps the graph alive for the boxes' name handles and
/// serves as a cache key: swapping in another selection's aggregate changes the
/// `Arc`, so `Arc::ptr_eq` detects a stale layout. The layout is viewport-
/// relative — it depends on the zoomed width `content_w`, the horizontal scroll
/// `scroll_x`, and the viewport width `view_w` — so all three are keys. The
/// vertical `scroll_y` is not: it only offsets a box's y at paint.
pub(crate) struct FlameBoxCache {
    aggregates: Arc<ScopeAggregate>,
    content_w: f64,
    scroll_x: f64,
    view_w: f32,
    boxes: Rc<Vec<FlameBox>>,
}

/// A flame-graph box under the cursor in graph mode: the laid-out box and where
/// the cursor was, so a tooltip can be drawn for it.
#[derive(Clone)]
pub(crate) struct FlameHover {
    boxed: FlameBox,
    at: Point<Pixels>,
}

/// The flame graph's current geometry, shared by hit-testing and the
/// scroll/zoom/pan handlers so they map the cursor the same way the paint does:
/// the horizontal view, the full laid-out height, and the flame section's
/// content rect after the scroll-bar reserves.
#[derive(Clone, Copy)]
pub(crate) struct FlameLayout {
    pub(crate) view: FlameView,
    pub(crate) total_h: f32,
    pub(crate) left: f32,
    pub(crate) top: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

/// The flame metrics matching the chart's lane row height, so the graph's rows
/// align with the rest of the view.
fn flame_metrics() -> FlameMetrics {
    FlameMetrics {
        row_height: METRICS.row_height,
    }
}

/// The flame graph's content rectangle: it spans the full body with no gutter
/// and no time axis, less a reserved vertical-bar column (when the tree's rows
/// overflow the visible height) and a reserved horizontal-bar strip (when the
/// zoomed width overflows the visible width). Both reserves may apply at once,
/// leaving the bottom-right corner to neither bar. Returned as
/// `(left, top, width, height)`.
fn flame_graph_rect(
    bounds: Bounds<Pixels>,
    flame_total: f32,
    flame_view: FlameView,
) -> (f32, f32, f32, f32) {
    let left = f32::from(bounds.origin.x);
    let top = f32::from(bounds.origin.y);
    let width_full = f32::from(bounds.size.width);
    let height_full = f32::from(bounds.size.height);
    // Zoom > 1 overflows horizontally, so a bottom strip is reserved for the
    // h-bar; that shrinks the height the rows must clear, which is what decides
    // the v-bar reserve. Resolve in that order.
    let hbar_reserve = if flame_view.zoom > 1.0 {
        BAR_WIDTH
    } else {
        0.0
    };
    let height = (height_full - hbar_reserve).max(0.0);
    let vbar_reserve = if flame_total > height {
        BAR_WIDTH
    } else {
        0.0
    };
    let width = (width_full - vbar_reserve).max(0.0);
    (left, top, width, height)
}

/// Lays out the graph's visible window, reusing the [`FlameBoxCache`] in `cache`
/// unless a key changed. The retained `aggregates` keeps the graph alive for the
/// boxes' name handles and keys the cache by identity; the layout is
/// viewport-relative, so panning (a change in `scroll_x`) is a key too.
fn flame_boxes(
    cache: &Rc<RefCell<Option<FlameBoxCache>>>,
    aggregates: &Arc<ScopeAggregate>,
    content_w: f64,
    scroll_x: f64,
    view_w: f32,
    metrics: &FlameMetrics,
) -> Rc<Vec<FlameBox>> {
    if let Some(c) = cache.borrow().as_ref() {
        if Arc::ptr_eq(&c.aggregates, aggregates)
            && c.content_w == content_w
            && c.scroll_x == scroll_x
            && c.view_w == view_w
        {
            return c.boxes.clone();
        }
    }
    let boxes = Rc::new(lay_out_flame(
        &aggregates.flame,
        content_w,
        FlamePlacement { scroll_x, view_w, metrics, min_px: MIN_SLICE_PX },
    ));
    *cache.borrow_mut() = Some(FlameBoxCache {
        aggregates: aggregates.clone(),
        content_w,
        scroll_x,
        view_w,
        boxes: boxes.clone(),
    });
    boxes
}

/// Paints the aggregated flame graph filling the body: each box a work-class-colored
/// quad with a truncated label, clipped to the content rect, scrolled vertically
/// by `scroll_y` and zoomed/panned horizontally by `flame_view`. A vertical
/// scroll bar shows when the tree's rows overflow the height and a horizontal one
/// when the zoomed width overflows the width; a tooltip draws for the hovered
/// box. No time-axis chrome (ruler, gridlines, gutter, markers) applies, so none
/// is drawn.
#[allow(clippy::too_many_arguments)]
fn paint_flame_graph(
    aggregates: &Arc<ScopeAggregate>,
    cache: &Rc<RefCell<Option<FlameBoxCache>>>,
    scroll_y: f32,
    flame_view: FlameView,
    palette: &Palette,
    vbar: &Rc<Cell<Option<Scrollbar>>>,
    hbar: &Rc<Cell<Option<Scrollbar>>>,
    hover: Option<FlameHover>,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let graph = &aggregates.flame;
    let metrics = flame_metrics();
    let total_h = flame_height(graph, &metrics);
    let (left, top, width, height) = flame_graph_rect(bounds, total_h, flame_view);
    // Lay out only the visible window, viewport-relative: the tree occupies
    // `content_w` pixels at this zoom, scrolled `scroll_x` in, so a box's x is
    // already relative to the content column's left edge. Off-screen boxes are
    // culled and coalescing runs locally, matching the flame chart's slice
    // layout. Coordinates are f64 so precision holds at deep zoom.
    let content_w = width as f64 * flame_view.zoom;
    let scroll_x = flame_view.left * content_w;
    let boxes = flame_boxes(cache, aggregates, content_w, scroll_x, width, &metrics);

    let mask = ContentMask {
        bounds: Bounds::new(point(px(left), px(top)), size(px(width), px(height))),
    };
    window.with_content_mask(Some(mask), |window| {
        for b in boxes.iter() {
            let y = top + b.y - scroll_y;
            if y + b.h < top || y > top + height {
                continue;
            }
            let rect = Bounds::new(point(px(left + b.x), px(y)), size(px(b.w), px(b.h)));
            let fill = theme::span_role_color(b.role, palette.background);
            bviz_ui_components::paint_labeled_box(
                rect,
                fill,
                graph.str(b.name),
                bviz_ui_theme::label_on(fill),
                bviz_ui_components::LabelBoxStyle {
                    background: palette.background,
                    seam_min_px: SLICE_SEAM_MIN_PX,
                    seam_mix: SLICE_SEAM_MIX,
                    label_min_px: LABEL_MIN_PX,
                    text_pad: SLICE_TEXT_PAD,
                },
                window,
                cx,
            );
        }
    });

    let vlayout = Scrollbar::new(
        Axis::Vertical,
        Bounds::new(
            point(px(left + width), px(top)),
            size(px(BAR_WIDTH), px(height)),
        ),
        total_h,
        height,
        scroll_y,
        DEFAULT_MIN_THUMB,
    );
    if let Some(bar) = vlayout {
        bar.paint(palette.scroll_track, palette.scroll_thumb, window);
    }
    vbar.set(vlayout);

    // The horizontal bar runs along the bottom strip reserved beneath the content.
    let hlayout = Scrollbar::new(
        Axis::Horizontal,
        Bounds::new(
            point(px(left), px(top + height)),
            size(px(width), px(BAR_WIDTH)),
        ),
        content_w as f32,
        width,
        scroll_x as f32,
        DEFAULT_MIN_THUMB,
    );
    if let Some(bar) = hlayout {
        bar.paint(palette.scroll_track, palette.scroll_thumb, window);
    }
    hbar.set(hlayout);

    if let Some(h) = hover {
        paint_flame_tooltip(graph, &h.boxed, graph.root().total_us, h.at, bounds, palette, window, cx);
    }
}

/// Paints a tooltip near `at` with the flame node's name and, below it, its total
/// and self time, occurrence count, and share of the view's aggregated total.
#[allow(clippy::too_many_arguments)]
fn paint_flame_tooltip(
    graph: &FlameGraph,
    boxed: &FlameBox,
    root_total_us: i64,
    at: Point<Pixels>,
    bounds: Bounds<Pixels>,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let pct = if root_total_us > 0 {
        100.0 * boxed.total_us as f64 / root_total_us as f64
    } else {
        0.0
    };
    let detail = format!(
        "total {}, self {}, {}×, {:.1}% of view",
        format_duration(boxed.total_us),
        format_duration(boxed.self_us),
        boxed.count,
        pct,
    );
    let lines = [graph.str(boxed.name).to_string(), detail];
    paint_tooltip_box(&lines, at, bounds, palette, window, cx);
}

/// Maps the flame-graph panel's vertical thumb drag back to a scroll offset.
pub(crate) fn flame_scroll_thumb_to(view: &mut TraceView, pos: Point<Pixels>, grab: f32) {
    if let Some(offset) = thumb_offset(&view.flame_vbar, pos, grab) {
        view.flame_scroll_y.set(offset);
    }
}

impl TraceView {
    /// The flame-graph panel: its own canvas painting the merged boxes and the
    /// panel's horizontal/vertical scroll bars, with the panel's pan/scroll/hover
    /// handlers. The canvas closures are `'static`, so they capture owned
    /// snapshots rather than borrowing the view.
    pub(crate) fn flame_graph_panel(
        &self,
        aggregates: Arc<ScopeAggregate>,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let flame_scroll_y = self.flame_scroll_y.clone();
        let flame_view = self.flame_view.clone();
        let flame_bounds = self.flame_bounds.clone();
        let vbar = self.flame_vbar.clone();
        let hbar = self.flame_hbar.clone();
        let flame_boxes = self.flame_boxes.clone();
        let entity = cx.entity();
        let prepaint = {
            let flame_bounds = flame_bounds.clone();
            move |bounds: Bounds<Pixels>, _: &mut Window, _: &mut App| {
                flame_bounds.set(bounds);
            }
        };
        let canvas = canvas(prepaint, move |bounds, _, window, cx| {
            let (flame_hover, dragging) = {
                let this = entity.read(cx);
                (this.flame_hover.clone(), !matches!(this.drag, Drag::None))
            };
            paint_flame_graph(
                &aggregates,
                &flame_boxes,
                flame_scroll_y.get(),
                flame_view.get(),
                &palette,
                &vbar,
                &hbar,
                flame_hover,
                bounds,
                window,
                cx,
            );
            if dragging {
                track_drag(window, &entity, on_drag, end_drag);
            }
        })
        .absolute()
        .left_0()
        .top_0()
        .size_full();
        // While a rebuild for a new selection runs off the thread, the prior
        // graph stays shown and interactive; a spinner pill signals the update,
        // since aggregating a large selection can take a moment.
        let spinner = self.agg.is_building().then(|| {
            div()
                .absolute()
                .top_2()
                .left_0()
                .right_0()
                .flex()
                .flex_row()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(palette.panel)
                        .border_1()
                        .border_color(palette.border)
                        .text_color(palette.label)
                        .text_size(px(STATS_TEXT_SIZE))
                        .child(spinner("flame-rebuild", px(14.), palette.label))
                        .child("Updating…"),
                )
        });
        self.flame_panel_handlers(
            div()
                .id("flame-graph-panel")
                .relative()
                .size_full()
                .overflow_hidden()
                .child(canvas)
                .children(spinner),
            cx,
        )
    }

    /// The current flame-graph panel geometry, or `None` until the graph exists.
    pub(crate) fn flame_layout(&self) -> Option<FlameLayout> {
        let aggregates = self.aggregates.borrow().clone()?;
        let view = self.flame_view.get();
        let total_h = flame_height(&aggregates.flame, &flame_metrics());
        let (left, top, width, height) = flame_graph_rect(self.flame_bounds.get(), total_h, view);
        Some(FlameLayout {
            view,
            total_h,
            left,
            top,
            width,
            height,
        })
    }

    /// The pixel ceiling for the flame-graph panel's vertical scroll: the tree's
    /// height past the panel's on-screen height (which excludes any reserved
    /// horizontal-bar strip, so the last row clears that bar).
    pub(crate) fn flame_max_scroll(&self) -> f32 {
        self.flame_layout()
            .map(|fl| (fl.total_h - fl.height).max(0.0))
            .unwrap_or(0.0)
    }

    /// The flame-graph box under a window-space cursor: map the cursor to a depth
    /// row via the vertical scroll and to a viewport x relative to the content
    /// column, then find the laid-out box at that depth whose x-range contains it.
    /// Searching the same viewport-relative box vector the paint uses (widened
    /// and coalesced) makes the hover match what is drawn.
    fn flame_box_at(&self, pos: Point<Pixels>) -> Option<FlameBox> {
        let fl = self.flame_layout()?;
        let aggregates = self.aggregates.borrow().clone()?;
        let metrics = flame_metrics();
        let x = f32::from(pos.x);
        let y = f32::from(pos.y);
        if x < fl.left || x >= fl.left + fl.width || y < fl.top || y >= fl.top + fl.height {
            return None;
        }
        let content_y = y - fl.top + self.flame_scroll_y.get();
        let depth = (content_y / metrics.row_height) as u32;
        let content_w = fl.width as f64 * fl.view.zoom;
        let scroll_x = fl.view.left * content_w;
        let vx = x - fl.left;
        let boxes = flame_boxes(&self.flame_boxes, &aggregates, content_w, scroll_x, fl.width, &metrics);
        boxes
            .iter()
            .find(|b| b.depth == depth && vx >= b.x && vx < b.x + b.w)
            .copied()
    }

    /// Updates the flame-graph panel hover to the box under `pos`, repainting
    /// only when the hovered box changes. The tooltip anchors at the cursor
    /// position on entry into a box and stays there while the cursor moves within
    /// it, so a move that stays in one box costs no repaint.
    fn update_flame_hover(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let hit = self.flame_box_at(pos);
        let changed = match (&self.flame_hover, &hit) {
            (Some(h), Some(b)) => h.boxed != *b,
            (None, None) => false,
            _ => true,
        };
        if !changed {
            return;
        }
        self.flame_hover = hit.map(|boxed| FlameHover { boxed, at: pos });
        cx.notify();
    }

    /// Begins a flame-graph panel drag at `pos`: its vertical or horizontal
    /// scroll thumb/track if the press hit one, otherwise a pan of the panel.
    fn press_flame(&mut self, pos: Point<Pixels>) {
        if self.begin_thumb_drag(pos, self.flame_vbar.get(), Drag::FlameVThumb, flame_scroll_thumb_to)
            || self.begin_thumb_drag(
                pos,
                self.flame_hbar.get(),
                Drag::FlameHThumb,
                TraceView::flame_hthumb_to,
            )
        {
            return;
        }
        self.drag = Drag::FlamePan;
        self.last_mouse = pos;
    }

    /// Maps a flame-graph panel horizontal thumb drag to its normalized scroll.
    pub(crate) fn flame_hthumb_to(&mut self, pos: Point<Pixels>, grab: f32) {
        let (Some(offset), Some(fl)) = (thumb_offset(&self.flame_hbar, pos, grab), self.flame_layout())
        else {
            return;
        };
        let mut fv = fl.view;
        fv.set_scroll_px(offset, fl.width);
        self.flame_view.set(fv);
    }

    /// The flame-graph panel's mouse/scroll/hover wiring: a press drives a scroll
    /// bar or pans, the wheel zooms/pans/scrolls the panel, and a move updates the
    /// panel's box hover. Presses and the wheel stop propagation so they don't
    /// also reach the flame chart's handlers underneath.
    fn flame_panel_handlers(&self, root: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        root.on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, e: &MouseDownEvent, _, cx| {
                this.press_flame(e.position);
                cx.stop_propagation();
                cx.notify();
            }),
        )
        .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, _, cx| {
            this.on_flame_scroll_wheel(e, cx);
            cx.stop_propagation();
        }))
        .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
            if matches!(this.drag, Drag::None) {
                this.update_flame_hover(e.position, cx);
            }
        }))
        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
            if !*hovered && this.flame_hover.take().is_some() {
                cx.notify();
            }
        }))
    }

    /// Ctrl/Cmd+wheel zooms the flame-graph panel about the cursor, Shift+wheel
    /// pans it horizontally, plain wheel scrolls it vertically.
    fn on_flame_scroll_wheel(&mut self, e: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let Some(fl) = self.flame_layout() else {
            return;
        };
        let (dx, dy) = match e.delta {
            ScrollDelta::Lines(p) => (p.x * LINE_SCROLL_PX, p.y * LINE_SCROLL_PX),
            ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
        };
        let (mut fv, left, width) = (fl.view, fl.left, fl.width);
        if e.modifiers.control || e.modifiers.platform {
            let dy_lines = dy / LINE_SCROLL_PX;
            fv.zoom_at(
                f32::from(e.position.x) - left,
                width,
                ZOOM_PER_NOTCH.powf(dy_lines as f64),
            );
            self.flame_view.set(fv);
        } else if e.modifiers.shift {
            fv.pan_px(dx + dy, width);
            self.flame_view.set(fv);
        } else {
            self.flame_scroll_y.set(self.flame_scroll_y.get() - dy);
        }
        // Re-clamp after any wheel action: a zoom that adds or drops the
        // horizontal-bar strip changes the panel's usable height, and so the
        // vertical scroll ceiling.
        let scrolled = self.flame_scroll_y.get().clamp(0.0, self.flame_max_scroll());
        self.flame_scroll_y.set(scrolled);
        cx.notify();
    }
}
