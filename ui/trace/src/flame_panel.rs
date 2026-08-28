use crate::chart::format_duration;
use crate::model::{AggregateBuildStarted, SourceReplaced, TraceModel};
use crate::theme::{self, Palette};
use crate::{
    LINE_SCROLL_PX, METRICS, MIN_SLICE_PX, STATS_TEXT_SIZE, ZOOM_PER_NOTCH, drawn_scroll,
    panel_toolbar, profile_status, scroll_request, thumb_offset, time_window_chip,
};
use bviz_trace_layout::{
    FlameBox, FlameDirection, FlameLabels, FlameMetrics, FlameMode, FlamePlacement, FlameView,
    ScopeAggregate, flame_height, lay_out_flame,
};
use bviz_ui_components::{
    Axis, Button, DEFAULT_MIN_THUMB, DragEnd, SCROLLBAR_THICKNESS, Scrollbar, cached_layer,
    centered_spinner, overlay_canvas, paint_labeled_box, paint_tooltip_box, spinner, track_drag,
};
use bviz_ui_workspace::{Panel, PanelEvent, PanelKind, ViewKind};
use gpui::{
    AnyElement, App, Bounds, ContentMask, Context, Div, Entity, EventEmitter, FocusHandle,
    Focusable, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Point, Render,
    ScrollWheelEvent, SharedString, Stateful, Subscription, WeakEntity, Window, canvas, div, point,
    prelude::*, px, size,
};
use std::rc::Rc;
use std::sync::Arc;

/// The key context scoping the flame graph's own bindings, so a key pressed here
/// reaches this panel and no other.
const FLAME_KEY_CONTEXT: &str = "TraceFlameGraph";

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

impl FlameBoxCache {
    /// Lays the boxes out from `aggregates`'s own graph, so a hit's box and the
    /// graph it names that box through are the same aggregate's.
    fn new(
        aggregates: Arc<ScopeAggregate>,
        content_w: f64,
        scroll_x: f64,
        view_w: f32,
        metrics: &FlameMetrics,
    ) -> Self {
        let boxes = Rc::new(lay_out_flame(
            &aggregates.flame,
            content_w,
            FlamePlacement {
                scroll_x,
                view_w,
                metrics,
                min_px: MIN_SLICE_PX,
            },
        ));
        Self {
            aggregates,
            content_w,
            scroll_x,
            view_w,
            boxes,
        }
    }

    /// The memoized boxes when every key matches, `None` otherwise.
    fn matching(
        &self,
        aggregates: &Arc<ScopeAggregate>,
        content_w: f64,
        scroll_x: f64,
        view_w: f32,
    ) -> Option<&Rc<Vec<FlameBox>>> {
        (Arc::ptr_eq(&self.aggregates, aggregates)
            && self.content_w == content_w
            && self.scroll_x == scroll_x
            && self.view_w == view_w)
            .then_some(&self.boxes)
    }

    /// The box under a window-space `pos`, which maps to a depth row via the
    /// vertical scroll `scroll_y` and to an x relative to `fl`'s content column.
    /// `None` when the cursor lies off that rect, the row carries no box there, or
    /// a key no longer matches. Every change to the panel's view, scroll, or
    /// bounds rebuilds the canvas scene, whose paint re-memoizes ahead of the
    /// overlay that reads this, so a hit test taken during paint names a box of
    /// the layout on screen.
    fn hit(
        &self,
        aggregates: &Arc<ScopeAggregate>,
        fl: FlameLayout,
        scroll_y: f32,
        pos: Point<Pixels>,
    ) -> Option<HoveredBox> {
        if !fl.rect().contains(&pos) {
            return None;
        }
        let boxes = self.matching(
            aggregates,
            fl.view.content_px(fl.width),
            fl.view.scroll_px(fl.width),
            fl.width,
        )?;
        let content_y = f32::from(pos.y) - fl.top + scroll_y;
        let depth = (content_y / flame_metrics().row_height) as u32;
        let vx = f32::from(pos.x) - fl.left;
        let boxed = *boxes
            .iter()
            .find(|b| b.depth == depth && vx >= b.x && vx < b.x + b.w)?;
        Some(HoveredBox {
            aggregates: self.aggregates.clone(),
            boxed,
        })
    }
}

/// A hovered flame box paired with the aggregate its name handle resolves
/// through.
struct HoveredBox {
    aggregates: Arc<ScopeAggregate>,
    boxed: FlameBox,
}

/// The flame graph's current geometry, shared by hit-testing and the
/// scroll/zoom/pan handlers so they map the cursor the same way the paint does.
#[derive(Clone, Copy)]
struct FlameLayout {
    view: FlameView,
    total_h: f32,
    left: f32,
    top: f32,
    width: f32,
    height: f32,
}

impl FlameLayout {
    /// Places a graph `total_h` tall, viewed through `view`, in `bounds`: the
    /// content rect spans the full body with no gutter and no time axis, less a
    /// reserved vertical-bar column (when the tree's rows overflow the visible
    /// height) and a reserved horizontal-bar strip (when the zoomed width
    /// overflows the visible width). Both reserves may apply at once, leaving the
    /// bottom-right corner to neither bar.
    fn new(bounds: Bounds<Pixels>, total_h: f32, view: FlameView) -> Self {
        // Zoom > 1 overflows horizontally, so a bottom strip is reserved for the
        // h-bar; that shrinks the height the rows must clear, which is what decides
        // the v-bar reserve. Resolve in that order.
        let hbar_reserve = if view.zoom > 1.0 {
            SCROLLBAR_THICKNESS
        } else {
            0.0
        };
        let height = (f32::from(bounds.size.height) - hbar_reserve).max(0.0);
        let vbar_reserve = if total_h > height {
            SCROLLBAR_THICKNESS
        } else {
            0.0
        };
        let width = (f32::from(bounds.size.width) - vbar_reserve).max(0.0);
        Self {
            view,
            total_h,
            left: f32::from(bounds.origin.x),
            top: f32::from(bounds.origin.y),
            width,
            height,
        }
    }

    fn rect(&self) -> Bounds<Pixels> {
        Bounds::new(
            point(px(self.left), px(self.top)),
            size(px(self.width), px(self.height)),
        )
    }
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

/// Lays out the graph's visible window, reusing the [`FlameBoxCache`] in `cache`
/// unless a key changed.
fn flame_boxes(
    cache: &mut Option<FlameBoxCache>,
    aggregates: &Arc<ScopeAggregate>,
    content_w: f64,
    scroll_x: f64,
    view_w: f32,
    metrics: &FlameMetrics,
) -> Rc<Vec<FlameBox>> {
    if let Some(boxes) = cache
        .as_ref()
        .and_then(|c| c.matching(aggregates, content_w, scroll_x, view_w))
    {
        return boxes.clone();
    }
    cache
        .insert(FlameBoxCache::new(
            aggregates.clone(),
            content_w,
            scroll_x,
            view_w,
            metrics,
        ))
        .boxes
        .clone()
}

/// Paints a tooltip near `at` with the flame node's name and, below it, its total
/// and self time, the number of spans whose time it accounts for, and its share
/// of the view's aggregated total.
fn paint_flame_tooltip(
    hovered: &HoveredBox,
    at: Point<Pixels>,
    bounds: Bounds<Pixels>,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let (graph, boxed) = (&hovered.aggregates.flame, &hovered.boxed);
    let root_total_us = graph.root_total_us();
    let pct = if root_total_us > 0 {
        100.0 * boxed.total_us as f64 / root_total_us as f64
    } else {
        0.0
    };
    let detail = format!(
        "total {}, self {}, {} span{}, {:.1}% of view",
        format_duration(boxed.total_us),
        format_duration(boxed.self_us),
        boxed.count,
        if boxed.count == 1 { "" } else { "s" },
        pct,
    );
    let lines = [graph.str(boxed.name).to_string(), detail];
    paint_tooltip_box(&lines, at, bounds, palette.tooltip, window, cx);
}

/// The aggregated flame-graph panel: time-free work-class-colored boxes filling
/// the body, zoom/pan on the x axis and vertical scroll on the y, with its own
/// scroll bars and a hover tooltip. Owns its view state as plain fields,
/// reading the aggregate from [`TraceModel`]. The boxes are painted by
/// [`FlameCanvas`], the tooltip by an overlay above it.
pub struct FlamePanel {
    /// Focus for the panel, so the window's own actions dispatch while it is the
    /// panel on show.
    focus: FocusHandle,
    /// Read for the current selection's aggregate and whether a rebuild is
    /// running, and written by the mode controls on its toolbar.
    model: Entity<TraceModel>,
    /// Horizontal window into the aggregated flame graph's normalized width (the
    /// panel's zoom and pan); a fraction so a resize stays invertible.
    flame_view: FlameView,
    /// Where the panel's top edge sits in the tree, in pixels from its top. Held
    /// unbounded, so a position past the end of a graph that is momentarily
    /// shorter returns the view once the graph grows back.
    flame_scroll_y: f32,
    /// The panel's own bounds, captured during its canvas prepaint, so handlers
    /// map the cursor within the panel rather than the whole window; `None`
    /// until the panel has been laid out once.
    flame_bounds: Option<Bounds<Pixels>>,
    /// The panel's memoized box layout; see [`FlameBoxCache`].
    flame_boxes: Option<FlameBoxCache>,
    /// The cursor position while it hovers the graph, anchoring the box tooltip;
    /// `None` otherwise.
    flame_hover: Option<Point<Pixels>>,
    drag: FlameDrag,
    last_mouse: Point<Pixels>,
    /// The layer painting the boxes and scroll bars; notified through
    /// [`Self::invalidate_canvas`].
    canvas: Entity<FlameCanvas>,
    /// Re-syncs the panel with the model's aggregate, which lands asynchronously.
    _model_observer: Subscription,
    /// Raises the rebuild pill when a build starts, which no state the panel
    /// renders from would otherwise reveal.
    _build_start: Subscription,
    /// Returns the panel to the fit-to-window view when a different document
    /// loads, whose graph a zoom held over the old one addresses nothing in.
    _source_replaced: Subscription,
}

impl FlamePanel {
    pub(crate) fn new(model: Entity<TraceModel>, cx: &mut Context<Self>) -> Self {
        let parent = cx.entity().downgrade();
        let canvas = cx.new(|_| FlameCanvas { parent });
        let model_observer = cx.observe(&model, |this, _, cx| {
            this.reset_aggregate_derived();
            this.invalidate_canvas(cx);
        });
        // The boxes still address the shown graph; only the pill above them needs
        // the frame.
        let build_start = cx.subscribe(&model, |_, _, _: &AggregateBuildStarted, cx| cx.notify());
        let source_replaced = cx.subscribe(&model, |this, _, _: &SourceReplaced, cx| {
            this.reset_view();
            this.invalidate_canvas(cx);
        });
        Self {
            focus: cx.focus_handle(),
            model,
            flame_view: FlameView::FIT,
            flame_scroll_y: 0.0,
            flame_bounds: None,
            flame_boxes: None,
            flame_hover: None,
            drag: FlameDrag::None,
            last_mouse: Point::default(),
            canvas,
            _model_observer: model_observer,
            _build_start: build_start,
            _source_replaced: source_replaced,
        }
    }

    /// Rebuilds the box scene along with the overlay; every change to what the
    /// boxes show routes through here.
    pub(crate) fn invalidate_canvas(&self, cx: &mut Context<Self>) {
        self.canvas.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    /// Drops the memoized box layout, which addresses the graph that produced it.
    fn reset_aggregate_derived(&mut self) {
        self.flame_boxes = None;
    }

    /// Resets the panel to the fit-to-window view at the top of the tree: a zoom,
    /// pan, and scroll held for one graph address nothing in a graph whose
    /// geometry is unrelated to it.
    pub(crate) fn reset_view(&mut self) {
        self.flame_view = FlameView::FIT;
        self.flame_scroll_y = 0.0;
        self.reset_aggregate_derived();
    }

    /// Clears the box tooltip, so a pane bringing another panel forward drops it
    /// instead of leaving it until the next mouse move.
    fn clear_hover(&mut self) {
        self.flame_hover = None;
    }

    /// The panel's toolbar: the window the graph covers, then the controls that
    /// choose which graph is aggregated and the legend for the colours it is
    /// drawn in.
    fn toolbar(&self, palette: Palette, cx: &mut Context<Self>) -> Div {
        panel_toolbar(palette)
            .children(time_window_chip(&self.model, palette, cx))
            .child(div().flex_1())
            .child(self.mode_controls(cx))
            .child(theme::role_legend(&palette, STATS_TEXT_SIZE))
    }

    /// The graph's mode controls: the two directions as an exclusive pair, and
    /// the labelling as a toggle that holds across both.
    fn mode_controls(&self, cx: &mut Context<Self>) -> Div {
        let mode = self.model.read(cx).flame_mode();
        let mnemonics = mode.labels == FlameLabels::Mnemonic;
        let button = |id: &'static str, label: &'static str, on: bool, to: FlameMode| {
            let view = cx.entity();
            Button::new(id, move |_, _, cx| {
                view.update(cx, |this, cx| this.set_mode(to, cx));
            })
            .ghost()
            .compact()
            .selected(on)
            .label(label)
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .gap_1()
            .text_size(px(STATS_TEXT_SIZE))
            .child(button(
                "flame-top-down",
                "Top-down",
                mode.direction == FlameDirection::TopDown,
                FlameMode {
                    direction: FlameDirection::TopDown,
                    ..mode
                },
            ))
            .child(button(
                "flame-bottom-up",
                "Bottom-up",
                mode.direction == FlameDirection::BottomUp,
                FlameMode {
                    direction: FlameDirection::BottomUp,
                    ..mode
                },
            ))
            .child(button(
                "flame-mnemonics",
                "Mnemonics",
                mnemonics,
                FlameMode {
                    labels: if mnemonics {
                        FlameLabels::Name
                    } else {
                        FlameLabels::Mnemonic
                    },
                    ..mode
                },
            ))
    }

    /// Applies a mode chosen on the toolbar, leaving the view alone for a click
    /// that re-chooses the standing mode. The graph the next aggregate lays out
    /// has a geometry unrelated to the one a zoom and scroll were held over, so
    /// they return to the fit.
    fn set_mode(&mut self, mode: FlameMode, cx: &mut Context<Self>) {
        if !self.model.update(cx, |m, cx| m.set_flame_mode(mode, cx)) {
            return;
        }
        self.reset_view();
        self.invalidate_canvas(cx);
    }

    /// The current panel geometry, or `None` until the graph exists and the panel
    /// has been laid out.
    fn layout(&self, cx: &App) -> Option<FlameLayout> {
        let aggregates = self.model.read(cx).aggregates()?;
        let total_h = flame_height(&aggregates.flame, &flame_metrics());
        Some(FlameLayout::new(
            self.flame_bounds?,
            total_h,
            self.flame_view,
        ))
    }

    /// The bar for the tree's rows against the visible height; none while the
    /// layout is unknown.
    fn vbar(&self, cx: &App) -> Option<Scrollbar> {
        let fl = self.layout(cx)?;
        Scrollbar::new(
            Axis::Vertical,
            Bounds::new(
                point(px(fl.left + fl.width), px(fl.top)),
                size(px(SCROLLBAR_THICKNESS), px(fl.height)),
            ),
            fl.total_h,
            fl.height,
            self.flame_scroll(cx),
            DEFAULT_MIN_THUMB,
        )
    }

    /// The bar for the zoomed width against the visible width; none while the
    /// layout is unknown.
    fn hbar(&self, cx: &App) -> Option<Scrollbar> {
        let fl = self.layout(cx)?;
        Scrollbar::new(
            Axis::Horizontal,
            Bounds::new(
                point(px(fl.left), px(fl.top + fl.height)),
                size(px(fl.width), px(SCROLLBAR_THICKNESS)),
            ),
            fl.view.content_px(fl.width) as f32,
            fl.width,
            fl.view.scroll_px(fl.width) as f32,
            DEFAULT_MIN_THUMB,
        )
    }

    /// The pixel ceiling for the panel's vertical scroll: the tree's height past
    /// the panel's on-screen height (which excludes any reserved horizontal-bar
    /// strip, so the last row clears that bar), or `None` until the layout is
    /// known.
    fn max_scroll(&self, cx: &App) -> Option<f32> {
        self.layout(cx).map(|fl| (fl.total_h - fl.height).max(0.0))
    }

    /// How far down the tree the panel draws, in pixels; see [`drawn_scroll`].
    /// The sole reading of the vertical offset, so what is painted and what is
    /// hit-tested agree.
    fn flame_scroll(&self, cx: &App) -> f32 {
        let Some(max) = self.max_scroll(cx) else {
            return self.flame_scroll_y;
        };
        drawn_scroll(self.flame_scroll_y, max)
    }

    /// Moves the panel's top edge to `y` pixels down the tree; see
    /// [`scroll_request`]. This is a gesture entry point rather than an absolute
    /// seek: while a position past the tree's end is held, `scroll_to(0.0)` is a
    /// no-op, so a caller that means "go to the top" assigns the offset instead.
    fn scroll_to(&mut self, y: f32, cx: &App) {
        let Some(max) = self.max_scroll(cx) else {
            return;
        };
        if let Some(y) = scroll_request(self.flame_scroll_y, max, y) {
            self.flame_scroll_y = y;
        }
    }

    /// Moves the panel's top edge up by a gesture's `dy` pixels.
    fn scroll_by(&mut self, dy: f32, cx: &App) {
        self.scroll_to(self.flame_scroll(cx) - dy, cx);
    }

    /// The box under a window-space cursor, resolved against the memoized layout;
    /// see [`FlameBoxCache::hit`]. `None` until the graph exists and the panel
    /// has painted once.
    fn box_at(&self, pos: Point<Pixels>, cx: &App) -> Option<HoveredBox> {
        let fl = self.layout(cx)?;
        let aggregates = self.model.read(cx).aggregates()?;
        self.flame_boxes
            .as_ref()?
            .hit(&aggregates, fl, self.flame_scroll(cx), pos)
    }

    /// Tracks the cursor for the box tooltip: the position is recorded across the
    /// whole graph rect so the tooltip follows the cursor, while only a move that
    /// draws or erases one repaints.
    fn update_hover(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let drawn_before = self.hovered_box(cx).is_some();
        self.flame_hover = self
            .layout(cx)
            .filter(|fl| fl.rect().contains(&pos))
            .map(|_| pos);
        if drawn_before || self.hovered_box(cx).is_some() {
            cx.notify();
        }
    }

    /// The hovered box, with the graph the tooltip names it through, and the
    /// position the tooltip anchors at, when the hover lands on one.
    fn hovered_box(&self, cx: &App) -> Option<(Point<Pixels>, HoveredBox)> {
        let at = self.flame_hover?;
        Some((at, self.box_at(at, cx)?))
    }

    /// Begins a drag at `pos`: the vertical or horizontal scroll thumb/track if the
    /// press hit one, otherwise a pan of the panel.
    fn press(&mut self, pos: Point<Pixels>, cx: &App) {
        if let Some(bar) = self.vbar(cx)
            && bar.track_hit(pos)
        {
            let grab = bar.grab(pos);
            self.drag = FlameDrag::VThumb(grab);
            self.scroll_thumb_to(pos, grab, cx);
            return;
        }
        if let Some(bar) = self.hbar(cx)
            && bar.track_hit(pos)
        {
            let grab = bar.grab(pos);
            self.drag = FlameDrag::HThumb(grab);
            self.hthumb_to(pos, grab, cx);
            return;
        }
        self.drag = FlameDrag::Pan;
        self.last_mouse = pos;
    }

    /// Maps the vertical thumb drag back to a scroll offset.
    fn scroll_thumb_to(&mut self, pos: Point<Pixels>, grab: f32, cx: &App) {
        if let Some(offset) = thumb_offset(self.vbar(cx), pos, grab) {
            self.scroll_to(offset, cx);
        }
    }

    /// Maps a horizontal thumb drag to its normalized scroll.
    fn hthumb_to(&mut self, pos: Point<Pixels>, grab: f32, cx: &App) {
        let (Some(offset), Some(fl)) = (thumb_offset(self.hbar(cx), pos, grab), self.layout(cx))
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
        let delta = e.delta.pixel_delta(px(LINE_SCROLL_PX));
        let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
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
            self.scroll_by(dy, cx);
        }
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

impl EventEmitter<PanelEvent> for FlamePanel {}

impl Focusable for FlamePanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Panel for FlamePanel {
    fn kind() -> PanelKind {
        PanelKind {
            view: ViewKind::new("trace-flame-graph"),
            key_context: FLAME_KEY_CONTEXT,
        }
    }

    fn title(&self, _cx: &App) -> SharedString {
        "Flame Graph".into()
    }

    fn status(&self, cx: &App) -> Option<SharedString> {
        Some(profile_status(&self.model, cx))
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        match active {
            true => self.focus.focus(window, cx),
            // The tooltip anchors to a cursor that is no longer over the graph.
            false => self.clear_hover(),
        }
    }
}

impl Render for FlamePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::from_theme(&bviz_ui_theme::theme(window, cx));
        let toolbar = self.toolbar(palette, cx);
        let body = self.graph_body(palette, cx);
        div()
            .key_context(Self::kind().key_context)
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .child(toolbar)
            .child(body)
    }
}

impl FlamePanel {
    /// The graph itself, or the spinner standing in for it while the first
    /// aggregate for the loaded document is still being built.
    fn graph_body(&mut self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        if self.model.read(cx).aggregates().is_none() {
            return centered_spinner("Building flame graph…", &palette.colors).into_any_element();
        }
        let entity = cx.entity();
        let boxes = cached_layer(self.canvas.clone());
        let overlay = overlay_canvas(move |bounds, window, cx| {
            let (hovered, dragging) = {
                let this = entity.read(cx);
                (this.hovered_box(cx), !matches!(this.drag, FlameDrag::None))
            };
            if let Some((at, drawn)) = hovered {
                paint_flame_tooltip(&drawn, at, bounds, &palette, window, cx);
            }
            if dragging {
                track_drag(window, &entity, flame_on_drag, flame_end_drag);
            }
        });
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
                .flex_1()
                .min_h(px(0.))
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
fn flame_on_drag(panel: &mut FlamePanel, position: Point<Pixels>, cx: &mut Context<FlamePanel>) {
    match panel.drag {
        FlameDrag::Pan => {
            let dx = f32::from(position.x) - f32::from(panel.last_mouse.x);
            let dy = f32::from(position.y) - f32::from(panel.last_mouse.y);
            if let Some(fl) = panel.layout(cx) {
                let mut fv = fl.view;
                fv.pan_px(dx, fl.width);
                panel.flame_view = fv;
            }
            panel.scroll_by(dy, cx);
            panel.last_mouse = position;
        }
        FlameDrag::VThumb(grab) => panel.scroll_thumb_to(position, grab, cx),
        FlameDrag::HThumb(grab) => panel.hthumb_to(position, grab, cx),
        FlameDrag::None => {}
    }
    panel.update_hover(position, cx);
    panel.invalidate_canvas(cx);
}

fn flame_end_drag(
    panel: &mut FlamePanel,
    _: Point<Pixels>,
    _: DragEnd,
    cx: &mut Context<FlamePanel>,
) {
    panel.drag = FlameDrag::None;
    panel.invalidate_canvas(cx);
}

/// The layer painting the flame-graph panel's boxes and scroll bars, from the
/// view state and aggregate it reads through `parent`.
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
        let palette = Palette::from_theme(&bviz_ui_theme::theme(window, cx));
        let prepaint = {
            let parent = parent.clone();
            move |bounds: Bounds<Pixels>, _: &mut Window, cx: &mut App| {
                parent.update(cx, |panel, _| {
                    panel.flame_bounds = Some(bounds);
                });
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

/// The box layer's paint closure: the aggregated flame graph filling the body
/// — each box a work-class-colored quad with a truncated label, clipped to the
/// content rect, scrolled vertically by the panel's scroll and zoomed/panned
/// horizontally by its view — plus the vertical and horizontal scroll bars for
/// whichever axis overflows. No time-axis chrome applies, so none is drawn. It
/// reads the live view state through `parent`.
fn flame_paint(
    aggregates: Arc<ScopeAggregate>,
    parent: Entity<FlamePanel>,
    palette: Palette,
) -> impl FnOnce(Bounds<Pixels>, (), &mut Window, &mut App) {
    move |_, _, window, cx| {
        let graph = &aggregates.flame;
        let metrics = flame_metrics();
        // Read the panel's view state and lay out the visible window, then drop the
        // borrow before the box paint uses `cx` mutably.
        let (fl, scroll_y, content_w, scroll_x, vbar, hbar) = {
            let panel = parent.read(cx);
            let Some(fl) = panel.layout(cx) else {
                return;
            };
            // Lay out only the visible window, viewport-relative: the tree occupies
            // `content_w` pixels at this zoom, scrolled `scroll_x` in, so a box's x
            // is already relative to the content column's left edge. Coordinates are
            // f64 so precision holds at deep zoom.
            let content_w = fl.view.content_px(fl.width);
            let scroll_x = fl.view.scroll_px(fl.width);
            (
                fl,
                panel.flame_scroll(cx),
                content_w,
                scroll_x,
                panel.vbar(cx),
                panel.hbar(cx),
            )
        };
        let boxes = parent.update(cx, |panel, _| {
            flame_boxes(
                &mut panel.flame_boxes,
                &aggregates,
                content_w,
                scroll_x,
                fl.width,
                &metrics,
            )
        });
        let FlameLayout {
            left, top, height, ..
        } = fl;

        let mask = ContentMask { bounds: fl.rect() };
        window.with_content_mask(Some(mask), |window| {
            for b in boxes.iter() {
                let y = top + b.y - scroll_y;
                if y + b.h < top || y > top + height {
                    continue;
                }
                let rect = Bounds::new(point(px(left + b.x), px(y)), size(px(b.w), px(b.h)));
                let fill = palette.role_fill(b.role);
                paint_labeled_box(
                    rect,
                    fill,
                    graph.str(b.name),
                    bviz_ui_theme::label_on(fill),
                    palette.label_box(),
                    window,
                    cx,
                );
            }
        });

        for bar in vbar.into_iter().chain(hbar) {
            bar.paint(palette.scroll.track, palette.scroll.thumb, window);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_trace_layout::{FlameDirection, FlameLabels, FlameMode, TimeSelection};
    use bviz_trace_profile::Profile;

    /// Width of the content column the fixture layouts are placed for, wide
    /// enough that every fixture frame clears [`MIN_SLICE_PX`].
    const VIEW_W: f32 = 800.0;

    /// A profile of one execution lane whose spans nest two deep, so its graph
    /// has an interior frame with two siblings under it.
    fn fixture() -> Arc<Profile> {
        let mut events = vec![
            r#"{"name":"thread_name","ph":"M","tid":1,"args":{"name":"skyframe-evaluator-execution-1"}}"#
                .to_string(),
        ];
        for (name, ts, dur) in [("build", 0, 1000), ("compile", 0, 400), ("link", 400, 600)] {
            events.push(format!(
                r#"{{"cat":"general information","name":"{name}","ph":"X","ts":{ts},"dur":{dur},"tid":1}}"#,
            ));
        }
        let json = format!(r#"{{"traceEvents":[{}]}}"#, events.join(","));
        Arc::new(Profile::from_bytes(json.as_bytes()).expect("parse test profile"))
    }

    /// The aggregate over every track of `profile`, each frame named by its own
    /// span's name so a box's text identifies which span it stands for.
    fn aggregate(profile: &Arc<Profile>) -> Arc<ScopeAggregate> {
        Arc::new(ScopeAggregate::build_for_tracks(
            profile,
            FlameMode {
                direction: FlameDirection::TopDown,
                labels: FlameLabels::Name,
            },
            &TimeSelection::default(),
            |_| true,
        ))
    }

    /// The layout of `aggregates` fitted to a [`VIEW_W`]-wide column, memoized as
    /// the panel would, with the geometry it was placed for. The rect runs one
    /// row past the graph, so a probe on an empty row still lands inside it.
    fn laid_out(aggregates: &Arc<ScopeAggregate>) -> (FlameBoxCache, FlameLayout) {
        let metrics = flame_metrics();
        let total_h = flame_height(&aggregates.flame, &metrics);
        let fl = FlameLayout {
            view: FlameView::FIT,
            total_h,
            left: 40.0,
            top: 30.0,
            width: VIEW_W,
            height: total_h + metrics.row_height,
        };
        let cache = FlameBoxCache::new(
            aggregates.clone(),
            fl.view.content_px(fl.width),
            fl.view.scroll_px(fl.width),
            fl.width,
            &metrics,
        );
        (cache, fl)
    }

    /// Where the unscrolled panel draws `b`'s center, in window space.
    fn center_of(fl: FlameLayout, b: &FlameBox) -> Point<Pixels> {
        point(px(fl.left + b.x + b.w / 2.0), px(fl.top + b.y + b.h / 2.0))
    }

    #[test]
    fn a_hit_names_the_box_drawn_there() {
        let profile = fixture();
        let aggregates = aggregate(&profile);
        let (cache, fl) = laid_out(&aggregates);
        let names: Vec<&str> = cache
            .boxes
            .iter()
            .map(|b| aggregates.flame.str(b.name))
            .collect();
        assert!(
            names.contains(&"compile") && names.contains(&"link"),
            "{names:?}"
        );
        for b in cache.boxes.iter() {
            let hovered = cache
                .hit(&aggregates, fl, 0.0, center_of(fl, b))
                .expect("the box at its own center");
            assert_eq!(&hovered.boxed, b);
        }
    }

    #[test]
    fn a_hit_off_the_boxes_names_nothing() {
        let profile = fixture();
        let aggregates = aggregate(&profile);
        let (cache, fl) = laid_out(&aggregates);
        let hit = |x, y| {
            cache
                .hit(&aggregates, fl, 0.0, point(px(x), px(y)))
                .is_some()
        };
        let mid_x = fl.left + fl.width / 2.0;
        // The row past the deepest one the graph reaches carries no box.
        assert!(!hit(
            mid_x,
            fl.top + fl.total_h + flame_metrics().row_height / 2.0
        ));
        // Clear of the content rect on either axis.
        assert!(!hit(fl.left - 1.0, fl.top + 1.0));
        assert!(!hit(mid_x, fl.top - 1.0));
    }

    #[test]
    fn a_layout_keyed_to_another_view_names_nothing() {
        let profile = fixture();
        let aggregates = aggregate(&profile);
        let (cache, fl) = laid_out(&aggregates);
        let at = center_of(fl, cache.boxes.first().expect("the root frame"));
        assert!(cache.hit(&aggregates, fl, 0.0, at).is_some());
        // A rebuild yields a distinct allocation, so the boxes address a graph
        // that is no longer the one on screen.
        assert!(cache.hit(&aggregate(&profile), fl, 0.0, at).is_none());
        // A narrower content column is one the boxes were not placed for.
        let resized = FlameLayout {
            width: fl.width - 1.0,
            ..fl
        };
        assert!(cache.hit(&aggregates, resized, 0.0, at).is_none());
    }

    #[test]
    fn the_content_rect_reserves_a_column_and_a_strip_for_the_bars_shown() {
        let bounds = Bounds::new(point(px(40.0), px(30.0)), size(px(800.0), px(400.0)));
        let full_w = f32::from(bounds.size.width);
        let full_h = f32::from(bounds.size.height);
        let zoomed = FlameView {
            left: 0.0,
            zoom: 2.0,
        };
        let rect = |total_h, view| FlameLayout::new(bounds, total_h, view).rect();
        // A graph that fits, at the zoom that fits it: neither bar shows, so the
        // rect is the whole body.
        assert_eq!(rect(full_h, FlameView::FIT), bounds);
        // Zoomed in, the h-bar takes its strip; the rows still clear what is left
        // of the height, so no column goes to the v-bar.
        assert_eq!(
            rect(full_h - SCROLLBAR_THICKNESS, zoomed),
            Bounds::new(
                bounds.origin,
                size(px(full_w), px(full_h - SCROLLBAR_THICKNESS))
            )
        );
        // Rows that fit the body but not the height left by the h-bar's strip
        // overflow, so both bars show and each takes its reserve.
        assert_eq!(
            rect(full_h, zoomed),
            Bounds::new(
                bounds.origin,
                size(
                    px(full_w - SCROLLBAR_THICKNESS),
                    px(full_h - SCROLLBAR_THICKNESS)
                )
            )
        );
    }
}
