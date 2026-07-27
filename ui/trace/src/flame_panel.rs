use crate::chart::format_duration;
use crate::model::{AggregateBuildStarted, TraceModel};
use crate::theme::{self, Palette};
use crate::{
    BAR_WIDTH, LABEL_MIN_PX, LINE_SCROLL_PX, METRICS, MIN_SLICE_PX, SLICE_SEAM_MIN_PX,
    SLICE_SEAM_MIX, SLICE_TEXT_PAD, STATS_TEXT_SIZE, ZOOM_PER_NOTCH, paint_tooltip_box,
    thumb_offset,
};
use bviz_trace_layout::{
    FlameBox, FlameGraph, FlameMetrics, FlamePlacement, FlameView, ScopeAggregate, flame_height,
    lay_out_flame,
};
use bviz_ui_components::{Axis, DEFAULT_MIN_THUMB, LabelBoxStyle, Scrollbar, paint_labeled_box, spinner, track_drag};
use gpui::{
    AnyView, App, Bounds, ContentMask, Context, Div, Entity, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ScrollDelta,
    ScrollWheelEvent, Stateful, StyleRefinement, Subscription, WeakEntity, Window, canvas, div,
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

/// A flame-graph box under the cursor: the laid-out box and where the cursor was,
/// so a tooltip can be drawn for it.
#[derive(Clone)]
struct FlameHover {
    boxed: FlameBox,
    at: Point<Pixels>,
}

/// The flame graph's current geometry, shared by hit-testing and the
/// scroll/zoom/pan handlers so they map the cursor the same way the paint does:
/// the horizontal view, the full laid-out height, and the flame section's
/// content rect after the scroll-bar reserves.
#[derive(Clone, Copy)]
struct FlameLayout {
    view: FlameView,
    total_h: f32,
    left: f32,
    top: f32,
    width: f32,
    height: f32,
}

/// What a left-button drag over the flame-graph panel is currently moving.
#[derive(Clone, Copy)]
enum FlameDrag {
    None,
    /// A press inside the panel pans it.
    Pan,
    /// A press on the panel's vertical or horizontal scroll thumb/track, holding
    /// that grab offset from the thumb's leading edge.
    VThumb(f32),
    HThumb(f32),
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
/// leaving the bottom-right corner to neither bar.
struct Rect {
    left: f32,
    top: f32,
    width: f32,
    height: f32,
}

fn flame_graph_rect(bounds: Bounds<Pixels>, flame_total: f32, flame_view: FlameView) -> Rect {
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
    Rect { left, top, width, height }
}

/// Lays out the graph's visible window, reusing the [`FlameBoxCache`] in `cache`
/// unless a key changed. The retained `aggregates` keeps the graph alive for the
/// boxes' name handles and keys the cache by identity; the layout is
/// viewport-relative, so panning (a change in `scroll_x`) is a key too.
fn flame_boxes(
    cache: &RefCell<Option<FlameBoxCache>>,
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

/// The aggregated flame-graph panel: time-free work-class-colored boxes filling
/// the body, zoom/pan on the x axis and vertical scroll on the y, with its own
/// scroll bars and a hover tooltip. Owns its view state as plain and
/// interior-mutable fields, reading the aggregate from [`TraceModel`]. The
/// expensive box paint lives in a separate cached child ([`FlameCanvas`]) so a
/// hover — which touches only the tooltip overlay — replays its last scene rather
/// than re-emitting every box.
pub(crate) struct FlamePanel {
    /// Read for the current selection's aggregate and whether a rebuild is
    /// running.
    model: Entity<TraceModel>,
    /// Horizontal window into the aggregated flame graph's normalized width (the
    /// panel's zoom and pan); a fraction so a resize stays invertible.
    flame_view: FlameView,
    /// How far the panel is scrolled down, in pixels.
    flame_scroll_y: f32,
    /// The panel's own bounds, captured during its canvas paint, so handlers map
    /// the cursor within the panel rather than the whole window.
    flame_bounds: Cell<Bounds<Pixels>>,
    /// The panel's vertical and horizontal bar geometry, set during paint and read
    /// by mouse handlers; `None` when that axis fits.
    flame_vbar: Cell<Option<Scrollbar>>,
    flame_hbar: Cell<Option<Scrollbar>>,
    /// The panel's memoized box layout; see [`FlameBoxCache`].
    flame_boxes: RefCell<Option<FlameBoxCache>>,
    /// The box under the cursor, if any, drawn as a tooltip.
    flame_hover: Option<FlameHover>,
    drag: FlameDrag,
    last_mouse: Point<Pixels>,
    /// The cached child that paints the boxes and scroll bars. A parent
    /// `cx.notify()` (the hover path) leaves it to replay; [`Self::invalidate_canvas`]
    /// notifies it to rebuild.
    canvas: Entity<FlameCanvas>,
    /// Re-syncs the panel with the model's aggregate, which lands asynchronously.
    _model_observer: Subscription,
    /// Raises the rebuild pill when a build starts, which no state the panel
    /// renders from would otherwise reveal.
    _build_start: Subscription,
}

impl FlamePanel {
    pub(crate) fn new(model: Entity<TraceModel>, cx: &mut Context<Self>) -> Self {
        let parent = cx.entity().downgrade();
        let canvas = cx.new(|_| FlameCanvas { parent });
        let model_observer = cx.observe(&model, |this, _, cx| {
            this.reset_aggregate_derived();
            this.invalidate_canvas(cx);
        });
        // The graph the boxes and hover address is still the shown one, so the
        // cached scene keeps replaying; only the pill outside it needs the frame.
        let build_start =
            cx.subscribe(&model, |_, _, _: &AggregateBuildStarted, cx| cx.notify());
        Self {
            model,
            flame_view: FlameView::FIT,
            flame_scroll_y: 0.0,
            flame_bounds: Cell::new(Bounds::default()),
            flame_vbar: Cell::new(None),
            flame_hbar: Cell::new(None),
            flame_boxes: RefCell::new(None),
            flame_hover: None,
            drag: FlameDrag::None,
            last_mouse: Point::default(),
            canvas,
            _model_observer: model_observer,
            _build_start: build_start,
        }
    }

    /// Rebuilds the cached box scene along with the overlay, so a change to what
    /// the canvas paints re-emits its scene rather than replaying it. A plain
    /// `cx.notify()` (the hover path) leaves the canvas to replay.
    pub(crate) fn invalidate_canvas(&self, cx: &mut Context<Self>) {
        self.canvas.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    /// Drops the state that identifies a node of the shown graph: the memoized
    /// box layout and the hovered box, whose fields address the graph that
    /// produced them. Both are re-acquired against whichever graph replaces it —
    /// the boxes on the next paint, the hover on the next mouse move.
    fn reset_aggregate_derived(&mut self) {
        self.flame_boxes.borrow_mut().take();
        self.flame_hover = None;
    }

    /// Resets the panel to its fit-to-window view, so a freshly loaded profile
    /// starts from the fit rather than the prior graph's zoom.
    pub(crate) fn reset_for_new_profile(&mut self) {
        self.flame_view = FlameView::FIT;
        self.reset_aggregate_derived();
    }

    /// Clears the box tooltip, so a tab switch drops it instead of leaving it until
    /// the next mouse move.
    pub(crate) fn clear_hover(&mut self) {
        self.flame_hover = None;
    }

    /// The current panel geometry, or `None` until the graph exists.
    fn layout(&self, cx: &App) -> Option<FlameLayout> {
        let aggregates = self.model.read(cx).aggregates()?;
        let view = self.flame_view;
        let total_h = flame_height(&aggregates.flame, &flame_metrics());
        let Rect { left, top, width, height } =
            flame_graph_rect(self.flame_bounds.get(), total_h, view);
        Some(FlameLayout {
            view,
            total_h,
            left,
            top,
            width,
            height,
        })
    }

    /// The pixel ceiling for the panel's vertical scroll: the tree's height past
    /// the panel's on-screen height (which excludes any reserved horizontal-bar
    /// strip, so the last row clears that bar).
    fn max_scroll(&self, cx: &App) -> f32 {
        self.layout(cx)
            .map(|fl| (fl.total_h - fl.height).max(0.0))
            .unwrap_or(0.0)
    }

    /// The box under a window-space cursor: map the cursor to a depth row via the
    /// vertical scroll and to a viewport x relative to the content column, then
    /// find the laid-out box at that depth whose x-range contains it. Searching the
    /// same viewport-relative box vector the paint uses (widened and coalesced)
    /// makes the hover match what is drawn.
    fn box_at(&self, pos: Point<Pixels>, cx: &App) -> Option<FlameBox> {
        let fl = self.layout(cx)?;
        let aggregates = self.model.read(cx).aggregates()?;
        let metrics = flame_metrics();
        let x = f32::from(pos.x);
        let y = f32::from(pos.y);
        if x < fl.left || x >= fl.left + fl.width || y < fl.top || y >= fl.top + fl.height {
            return None;
        }
        let content_y = y - fl.top + self.flame_scroll_y;
        let depth = (content_y / metrics.row_height) as u32;
        let content_w = fl.view.content_px(fl.width);
        let scroll_x = fl.view.scroll_px(fl.width);
        let vx = x - fl.left;
        let boxes = flame_boxes(&self.flame_boxes, &aggregates, content_w, scroll_x, fl.width, &metrics);
        boxes
            .iter()
            .find(|b| b.depth == depth && vx >= b.x && vx < b.x + b.w)
            .copied()
    }

    /// Updates the box hover to the box under `pos`, repainting only when the
    /// hovered box changes. The tooltip anchors at the cursor position on entry
    /// into a box and stays there while the cursor moves within it, so a move that
    /// stays in one box costs no repaint.
    fn update_hover(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let hit = self.box_at(pos, cx);
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

    /// Begins a drag at `pos`: the vertical or horizontal scroll thumb/track if the
    /// press hit one, otherwise a pan of the panel.
    fn press(&mut self, pos: Point<Pixels>, cx: &App) {
        if let Some(bar) = self.flame_vbar.get() {
            if bar.track_hit(pos) {
                let grab = bar.grab(pos);
                self.drag = FlameDrag::VThumb(grab);
                self.scroll_thumb_to(pos, grab);
                return;
            }
        }
        if let Some(bar) = self.flame_hbar.get() {
            if bar.track_hit(pos) {
                let grab = bar.grab(pos);
                self.drag = FlameDrag::HThumb(grab);
                self.hthumb_to(pos, grab, cx);
                return;
            }
        }
        self.drag = FlameDrag::Pan;
        self.last_mouse = pos;
    }

    /// Maps the vertical thumb drag back to a scroll offset.
    fn scroll_thumb_to(&mut self, pos: Point<Pixels>, grab: f32) {
        if let Some(offset) = thumb_offset(self.flame_vbar.get(), pos, grab) {
            self.flame_scroll_y = offset;
        }
    }

    /// Maps a horizontal thumb drag to its normalized scroll.
    fn hthumb_to(&mut self, pos: Point<Pixels>, grab: f32, cx: &App) {
        let (Some(offset), Some(fl)) =
            (thumb_offset(self.flame_hbar.get(), pos, grab), self.layout(cx))
        else {
            return;
        };
        let mut fv = fl.view;
        fv.set_scroll_px(offset, fl.width);
        self.flame_view = fv;
    }

    /// Ctrl/Cmd+wheel zooms about the cursor, Shift+wheel pans horizontally, plain
    /// wheel scrolls vertically.
    fn on_scroll_wheel(&mut self, e: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let Some(fl) = self.layout(cx) else {
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
            self.flame_view = fv;
        } else if e.modifiers.shift {
            fv.pan_px(dx + dy, width);
            self.flame_view = fv;
        } else {
            self.flame_scroll_y -= dy;
        }
        // Re-clamp after any wheel action: a zoom that adds or drops the
        // horizontal-bar strip changes the panel's usable height, and so the
        // vertical scroll ceiling.
        self.flame_scroll_y = self.flame_scroll_y.clamp(0.0, self.max_scroll(cx));
        self.invalidate_canvas(cx);
    }

    /// The panel's mouse/scroll/hover wiring: a press drives a scroll bar or pans,
    /// the wheel zooms/pans/scrolls, and a move updates the box hover. Presses and
    /// the wheel stop propagation so they don't also reach the flame chart's
    /// handlers underneath.
    fn panel_handlers(&self, root: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        root.on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, e: &MouseDownEvent, _, cx| {
                this.press(e.position, cx);
                cx.stop_propagation();
                this.invalidate_canvas(cx);
            }),
        )
        .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, _, cx| {
            this.on_scroll_wheel(e, cx);
            cx.stop_propagation();
        }))
        .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
            if matches!(this.drag, FlameDrag::None) {
                this.update_hover(e.position, cx);
            }
        }))
        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
            if !*hovered && this.flame_hover.take().is_some() {
                cx.notify();
            }
        }))
    }
}

impl Render for FlamePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(aggregates) = self.model.read(cx).aggregates() else {
            return div().into_any_element();
        };
        let palette = Palette::from_colors(&bviz_ui_theme::colors(window));
        let entity = cx.entity();
        // Embedding the canvas cached lets a hover reuse its last scene: a parent
        // `cx.notify()` does not dirty it, so gpui replays its boxes.
        let boxes = AnyView::from(self.canvas.clone())
            .cached(StyleRefinement::default().absolute().inset_0().size_full());
        // The tooltip and drag paint over the cached boxes, sharing the canvas's
        // full bounds so the tooltip's viewport clamping is unchanged. A hover
        // repaints only this overlay.
        let overlay = {
            let aggregates = aggregates.clone();
            canvas(
                |_, _, _| {},
                move |bounds, _, window, cx| {
                    let (flame_hover, dragging) = {
                        let this = entity.read(cx);
                        (
                            this.flame_hover.clone(),
                            !matches!(this.drag, FlameDrag::None),
                        )
                    };
                    if let Some(h) = flame_hover {
                        let graph = &aggregates.flame;
                        paint_flame_tooltip(
                            graph,
                            &h.boxed,
                            graph.root_total_us(),
                            h.at,
                            bounds,
                            &palette,
                            window,
                            cx,
                        );
                    }
                    if dragging {
                        track_drag(window, &entity, flame_on_drag, flame_end_drag);
                    }
                },
            )
            .absolute()
            .left_0()
            .top_0()
            .size_full()
        };
        // While a rebuild for a new selection runs off the thread, the prior graph
        // stays shown and interactive; a spinner pill signals the update, since
        // aggregating a large selection can take a moment.
        let spinner = self.model.read(cx).aggregate_building().then(|| {
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
        self.panel_handlers(
            div()
                .id("flame-graph-panel")
                .relative()
                .size_full()
                .overflow_hidden()
                .child(boxes)
                .child(overlay)
                .children(spinner),
            cx,
        )
        .into_any_element()
    }
}

/// Routes an in-progress panel drag to its target: panning the panel, or moving a
/// scroll thumb.
fn flame_on_drag(panel: &mut FlamePanel, event: &MouseMoveEvent, cx: &mut Context<FlamePanel>) {
    match panel.drag {
        FlameDrag::Pan => {
            let dx = f32::from(event.position.x) - f32::from(panel.last_mouse.x);
            let dy = f32::from(event.position.y) - f32::from(panel.last_mouse.y);
            if let Some(fl) = panel.layout(cx) {
                let mut fv = fl.view;
                fv.pan_px(dx, fl.width);
                panel.flame_view = fv;
            }
            let scrolled = (panel.flame_scroll_y - dy).clamp(0.0, panel.max_scroll(cx));
            panel.flame_scroll_y = scrolled;
            panel.last_mouse = event.position;
        }
        FlameDrag::VThumb(grab) => panel.scroll_thumb_to(event.position, grab),
        FlameDrag::HThumb(grab) => panel.hthumb_to(event.position, grab, cx),
        FlameDrag::None => {}
    }
    panel.invalidate_canvas(cx);
}

fn flame_end_drag(panel: &mut FlamePanel, _: &MouseUpEvent, cx: &mut Context<FlamePanel>) {
    panel.drag = FlameDrag::None;
    panel.invalidate_canvas(cx);
}

/// The cached child view painting the flame-graph panel's boxes and scroll bars.
/// It reads the panel's view state and the aggregate through `parent`, so a
/// hover leaves its cached scene to replay.
struct FlameCanvas {
    parent: WeakEntity<FlamePanel>,
}

impl Render for FlameCanvas {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(parent) = self.parent.upgrade() else {
            return div().into_any_element();
        };
        let Some(aggregates) = parent.read(cx).model.read(cx).aggregates() else {
            return div().into_any_element();
        };
        let palette = Palette::from_colors(&bviz_ui_theme::colors(window));
        let prepaint = {
            let parent = parent.clone();
            move |bounds: Bounds<Pixels>, _: &mut Window, cx: &mut App| {
                parent.read(cx).flame_bounds.set(bounds);
            }
        };
        let paint = flame_paint(aggregates, parent, palette);
        canvas(prepaint, paint)
            .absolute()
            .inset_0()
            .size_full()
            .into_any_element()
    }
}

/// The cached canvas's paint closure: the aggregated flame graph filling the body
/// — each box a work-class-colored quad with a truncated label, clipped to the
/// content rect, scrolled vertically by the panel's scroll and zoomed/panned
/// horizontally by its view — plus the vertical and horizontal scroll bars for
/// whichever axis overflows. No time-axis chrome applies, so none is drawn. It
/// reads the live view state through `parent`; the hover tooltip paints from a
/// separate overlay above, so this scene replays on a hover.
fn flame_paint(
    aggregates: Arc<ScopeAggregate>,
    parent: Entity<FlamePanel>,
    palette: Palette,
) -> impl FnOnce(Bounds<Pixels>, (), &mut Window, &mut App) {
    move |bounds, _, window, cx| {
        let graph = &aggregates.flame;
        let metrics = flame_metrics();
        let total_h = flame_height(graph, &metrics);
        // Read the panel's view state and lay out the visible window, then drop the
        // borrow before the box paint uses `cx` mutably.
        let (scroll_y, boxes, rect, content_w, scroll_x) = {
            let panel = parent.read(cx);
            let flame_view = panel.flame_view;
            let scroll_y = panel.flame_scroll_y;
            let rect = flame_graph_rect(bounds, total_h, flame_view);
            // Lay out only the visible window, viewport-relative: the tree occupies
            // `content_w` pixels at this zoom, scrolled `scroll_x` in, so a box's x
            // is already relative to the content column's left edge. Coordinates are
            // f64 so precision holds at deep zoom.
            let content_w = flame_view.content_px(rect.width);
            let scroll_x = flame_view.scroll_px(rect.width);
            let boxes =
                flame_boxes(&panel.flame_boxes, &aggregates, content_w, scroll_x, rect.width, &metrics);
            (scroll_y, boxes, rect, content_w, scroll_x)
        };
        let Rect { left, top, width, height } = rect;

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
                paint_labeled_box(
                    rect,
                    fill,
                    graph.str(b.name),
                    bviz_ui_theme::label_on(fill),
                    LabelBoxStyle {
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
        let panel = parent.read(cx);
        panel.flame_vbar.set(vlayout);
        panel.flame_hbar.set(hlayout);
    }
}
