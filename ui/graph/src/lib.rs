//! Loads a Bazel `aquery`, `cquery`, or `query` `--output=streamed_proto` file
//! through `bviz_graph_layout`, lays the graph out, and opens a window
//! rendering the result.

use bviz_graph_layout::Id;
use bviz_ui_components::{Dropdown, DropdownEvent};
use bviz_ui_shell::Chrome;
use bviz_ui_theme::Colors;
use clap::Parser;
use gpui::{
    AnyElement, Context, Entity, FocusHandle, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, Rgba, ScrollDelta,
    ScrollWheelEvent, Subscription, Window, canvas, div, prelude::*, px, size,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

mod build;
mod camera;
mod dimming;
mod drag;
mod emphasis;
mod framing;
mod legend;
mod mode;
mod model;
mod paint;
mod scroll;
mod toolbar;

use build::{Input, InputKind};
use camera::{clamp_pan, enclosed_boxes, fit_zoom, from_screen, recenter_rect, Camera, Rect};
use drag::{end_drag, start_pan, Drag};
use emphasis::Emphasis;
use framing::{build_prepaint, Framing};
use legend::legend;
use mode::LayoutMode;
use model::{Drawn, Model, Source};
use paint::{canvas_body, Scene};
use scroll::{scrollbar_hit, start_scroll_drag};

/// Padding around the graph, in layout pixels.
const MARGIN: f64 = 16.0;

/// Largest zoom-in factor. The smallest is computed per graph so the whole graph
/// always fits the viewport.
const MAX_ZOOM: f32 = 4.0;

/// Fraction of the viewport the fitted graph spans, leaving a margin so the whole
/// graph sits within the inner part of the window rather than edge to edge.
const FIT_MARGIN: f32 = 2.0 / 3.0;

/// Breathing room held between the reframed content and the viewport edge when a
/// relayout settles the view, so a kept box never sits flush against the border.
const RECENTER_PAD: f32 = 24.0;

/// How long the view eases to its settled position after a toggle: short enough
/// to stay responsive, long enough that the eye can follow the shift instead of
/// the view jumping there in a single frame.
const SETTLE_DURATION: Duration = Duration::from_millis(220);

/// Below these per-axis deltas a settle is a no-op and is not animated, so a
/// toggle that leaves the view already framed does not twitch.
const SETTLE_MIN_ZOOM_STEP: f32 = 0.002;
const SETTLE_MIN_PAN_STEP: f32 = 0.5;

/// Theme-resolved colors the canvas paints with, owned so the `'static` paint
/// closure can capture them.
#[derive(Clone, Copy)]
struct Palette {
    border: Rgba,
    label: Rgba,
    container_fill: Rgba,
    /// Background the kind accents are blended toward to form leaf fills.
    background: Rgba,
}

/// Loads, lays out, and renders a Bazel build graph.
struct GraphView {
    focus_handle: FocusHandle,
    model: Model,
    framing: Framing,
    drag: Drag,
    emphasis: Emphasis,
    mode: LayoutMode,
    mode_picker: Entity<Dropdown>,
    /// Carries the picker's selections into [`GraphView::set_mode`].
    _mode_choices: Subscription,
    /// Node under the cursor, whose neighborhood stays lit while everything else
    /// dims; `None` when the cursor is over no node.
    hovered: Option<Id>,
    /// Whether hovering a node dims the rest of the graph; flipped by the `h` key
    /// or the toolbar toggle so the dimming can be turned off when it is more
    /// noise than help.
    dim_enabled: bool,
    /// Whether the layered layout draws the transitive reduction of the edges
    /// (the default) rather than every edge; flipped by the `r` key or the
    /// toolbar toggle, which relays out. Hover lights up a node's full neighbors
    /// either way.
    reduced: bool,
}

impl GraphView {
    fn camera(&self) -> Camera {
        self.framing.camera()
    }

    /// The layout the selected mode draws, or `None` while it is being computed.
    fn drawn(&self) -> Option<Drawn> {
        self.model.drawn(self.mode)
    }

    fn new(inputs: Vec<Input>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let mode = LayoutMode::default();
        let mode_picker = cx.new(|cx| {
            Dropdown::new(
                "layout-mode",
                LayoutMode::ALL.map(LayoutMode::label),
                mode.index(),
                cx,
            )
        });
        // The picker notifies on every repaint it makes, and only a selection
        // should move the view off its layout.
        let choices = cx.subscribe(&mode_picker, |view, _, event, cx| {
            let DropdownEvent::Changed(index) = event;
            if let Some(mode) = LayoutMode::at(*index) {
                view.set_mode(mode, cx);
            }
        });
        Self {
            focus_handle,
            model: Model::loading(inputs, cx),
            framing: Framing::new(),
            drag: Drag::None,
            emphasis: Emphasis::none(),
            mode,
            mode_picker,
            _mode_choices: choices,
            hovered: None,
            dim_enabled: true,
            reduced: true,
        }
    }

    /// Draws the selected mode's layout, or a spinner while it is still being
    /// computed — the toolbar stays above either, so the picker remains reachable.
    fn viewport(&mut self, palette: Palette, colors: &Colors, cx: &mut Context<Self>) -> AnyElement {
        let Some(drawn) = self.drawn() else {
            return bviz_ui_components::centered_spinner("Laying out…", colors).into_any_element();
        };
        let g = drawn.view(self.camera().zoom);
        let edge_legend = legend(g, &palette);
        let content = drawn.content();
        let recenter_target = recenter_rect(g, &self.emphasis);
        let scene = Scene {
            expandable: Arc::clone(self.model.expandable()),
            palette,
            hovered: self.hovered,
            dim_enabled: self.dim_enabled,
            highlight: self.emphasis.highlight(),
            drawn,
        };
        let entity = cx.entity();
        let prepaint = build_prepaint(entity.clone(), content, recenter_target);
        div()
            .id("graph-viewport")
            .relative()
            .size_full()
            .overflow_hidden()
            .child(
                canvas(prepaint, canvas_body(scene, entity))
                    .absolute()
                    .left_0()
                    .top_0()
                    .size_full(),
            )
            .children(edge_legend)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_hover(cx.listener(Self::on_hover))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            // Track the shared focus handle here too (the chrome already tracks
            // it on an ancestor) so this deepest focused element receives key
            // events. Events still bubble up to the chrome's own key bindings.
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .into_any_element()
    }

    /// Switches which layout is drawn, starting the newly selected one's if it
    /// has not been computed yet. The two span different extents, so the view
    /// refits.
    ///
    /// The hover and the emphasis both describe the layout being left: an
    /// emphasis carried across would let a relayout still in flight settle the
    /// incoming view onto a box picked out in the other one, and highlight it.
    fn set_mode(&mut self, mode: LayoutMode, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        self.hovered = None;
        self.emphasis.clear();
        self.framing.refit();
        if self.drawn().is_none() && !self.model.relaying() {
            model::relayout(self, cx);
        }
        cx.notify();
    }

    fn on_mouse_down(&mut self, e: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        // A press on a scroll-bar gutter drives the thumb instead of panning the
        // content under it.
        if let Some(axis) = scrollbar_hit(self, e.position) {
            start_scroll_drag(self, axis, e.position, cx);
            return;
        }
        start_pan(self, e.position, cx);
    }

    /// Ends the drag on the release the element receives directly, which the
    /// window-global handlers can miss: they are installed only once a frame
    /// paints with a drag in progress, so a press-release quick enough to finish
    /// before that paint would leave the drag running.
    fn on_mouse_up(&mut self, e: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        end_drag(self, e, cx);
    }

    fn on_mouse_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        // A pan moves the view rather than the hover; leave the lit set alone.
        if matches!(self.drag, Drag::Pan(_)) {
            return;
        }
        let Some(drawn) = self.drawn() else {
            return;
        };
        let camera = self.camera();
        let (gx, gy) = from_screen(e.position, self.framing.viewport().origin, camera);
        let hit = hit_box(&drawn.view(camera.zoom).boxes, gx, gy).map(|b| b.id);
        if hit != self.hovered {
            self.hovered = hit;
            cx.notify();
        }
    }

    /// Drops the hover when the pointer leaves the canvas, so a node's
    /// neighborhood does not stay lit while the cursor is over the toolbar or the
    /// window chrome — the move handler is bounds-gated and never fires there.
    /// gpui reports the canvas unhovered while a button is held, so the drag guard
    /// keeps a pan from dropping the lit set it started with.
    fn on_hover(&mut self, over: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if !over && matches!(self.drag, Drag::None) && self.hovered.take().is_some() {
            cx.notify();
        }
    }

    fn on_scroll_wheel(&mut self, e: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        // Anchor the zoom at the cursor: keep the content point under the cursor
        // fixed as the scale changes. The minimum zoom is the fit, so the graph
        // can't be zoomed out of view.
        let Some(content) = self.drawn().map(|d| d.content()) else {
            return;
        };
        let vp = self.framing.viewport();
        let lx = f32::from(e.position.x) - f32::from(vp.origin.x);
        let ly = f32::from(e.position.y) - f32::from(vp.origin.y);
        let dy = match e.delta {
            ScrollDelta::Lines(p) => p.y,
            ScrollDelta::Pixels(p) => f32::from(p.y) / 120.0,
        };
        let Camera { zoom: old, pan: (px, py) } = self.camera();
        let new = (old * 1.1_f32.powf(dy)).clamp(fit_zoom(vp.size, content), MAX_ZOOM);
        let content_x = (lx - px) / old;
        let content_y = (ly - py) / old;
        let pan = (lx - content_x * new, ly - content_y * new);
        self.framing.move_to(Camera { zoom: new, pan: clamp_pan(pan, new, vp.size, content) });
        cx.notify();
    }

    fn toggle_dim(&mut self, cx: &mut Context<Self>) {
        self.dim_enabled = !self.dim_enabled;
        cx.notify();
    }

    fn toggle_reduce(&mut self, cx: &mut Context<Self>) {
        if !self.mode.honors_reduce() {
            return;
        }
        self.reduced = !self.reduced;
        model::relayout(self, cx);
        cx.notify();
    }

    fn on_key_down(&mut self, e: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if e.keystroke.key == "h" {
            self.toggle_dim(cx);
        } else if e.keystroke.key == "r" {
            self.toggle_reduce(cx);
        }
    }
}

impl Render for GraphView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = bviz_ui_theme::colors(window);
        let palette = Palette {
            border: colors.border,
            label: colors.text,
            container_fill: colors.container,
            background: colors.background,
        };
        let body = match self.model.source() {
            Source::Loaded(_) => {
                let controls = Controls {
                    mode: self.mode,
                    picker: self.mode_picker.clone(),
                    dim_enabled: self.dim_enabled,
                    reduced: self.reduced,
                    relaying: self.model.relaying(),
                };
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(toolbar::toolbar(controls, &palette, cx))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .overflow_hidden()
                            .child(self.viewport(palette, &colors, cx)),
                    )
                    .into_any_element()
            }
            Source::Loading => bviz_ui_components::centered_spinner("Loading…", &colors)
                .into_any_element(),
            Source::Message(msg) => {
                bviz_ui_components::centered_notice(msg.clone(), &colors).into_any_element()
            }
        };
        Chrome::new(&self.focus_handle)
            .title("bazel_viz graph")
            .status(self.model.status(self.mode, self.camera().zoom))
            .child(body)
    }
}

/// What the toolbar's controls show.
struct Controls {
    mode: LayoutMode,
    picker: Entity<Dropdown>,
    dim_enabled: bool,
    reduced: bool,
    relaying: bool,
}

/// Acts on the box under `pos` (window px). The layered layout opens or closes
/// the box and settles the view to keep it in frame; a map is opened by zoom
/// alone, so there the box becomes the frame and the level rule reveals what is
/// inside it.
fn click_box_at(view: &mut GraphView, pos: Point<Pixels>, cx: &mut Context<GraphView>) {
    let vp = view.framing.viewport();
    let camera = view.camera();
    let Some(drawn) = view.drawn() else {
        return;
    };
    let g = drawn.view(camera.zoom);
    let (gx, gy) = from_screen(pos, vp.origin, camera);
    let Some(hit) = hit_box(&g.boxes, gx, gy) else {
        return;
    };
    match view.mode {
        LayoutMode::Map => {
            if !view.model.expandable().contains(&hit.id) {
                return;
            }
            let target = Rect { x: hit.x, y: hit.y, w: hit.width, h: hit.height };
            view.framing.frame(target, drawn.content());
            cx.notify();
        }
        LayoutMode::Layered => {
            // A multi-child container or a collapsed group can be opened/closed;
            // a leaf or singleton can't.
            if !hit.group && !view.model.expandable().contains(&hit.id) {
                return;
            }
            let id = hit.id;
            let enclosed = enclosed_boxes(&g.boxes, vp.size, camera);
            // The relayout may drop the hovered node (it was collapsed away),
            // which would otherwise leave the whole graph dimmed until the next
            // mouse move.
            view.hovered = None;
            model::toggle_expansion(view, id, cx);
            view.emphasis.toggled(id, enclosed);
        }
    }
}

/// The smallest-area box (in layout space) containing `(gx, gy)`, so an inner
/// member or header wins over its enclosing container; `None` if outside all.
fn hit_box(boxes: &[bviz_graph_layout::Box], gx: f32, gy: f32) -> Option<&bviz_graph_layout::Box> {
    boxes
        .iter()
        .filter(|b| {
            let (x, y) = (b.x as f32, b.y as f32);
            gx >= x && gx <= x + b.width as f32 && gy >= y && gy <= y + b.height as f32
        })
        .min_by(|a, b| (a.width * a.height).total_cmp(&(b.width * b.height)))
}

/// Renders a Bazel build graph from one or more `--output=streamed_proto`
/// files. Each flag selects a file and the query that produced it; passing
/// several merges them into one joint graph, with shared entities collapsed.
#[derive(Parser)]
#[command(group(
    clap::ArgGroup::new("input").required(true).multiple(true).args(["aquery", "cquery", "query"])
))]
struct Cli {
    /// `bazel aquery --output=streamed_proto` file.
    #[arg(long, value_name = "FILE")]
    aquery: Option<PathBuf>,
    /// `bazel cquery --output=streamed_proto` file (run cquery with
    /// `--transitions=lite` for configuration-resolved edges).
    #[arg(long, value_name = "FILE")]
    cquery: Option<PathBuf>,
    /// `bazel query --output=streamed_proto` file.
    #[arg(long, value_name = "FILE")]
    query: Option<PathBuf>,
}

impl Cli {
    /// Every provided input flag, in a stable order: aquery, cquery, query.
    fn inputs(self) -> Vec<Input> {
        let mut inputs = Vec::new();
        if let Some(path) = self.aquery {
            inputs.push(Input { kind: InputKind::Aquery, path });
        }
        if let Some(path) = self.cquery {
            inputs.push(Input { kind: InputKind::Cquery, path });
        }
        if let Some(path) = self.query {
            inputs.push(Input { kind: InputKind::Query, path });
        }
        inputs
    }
}

/// Parses the query inputs and opens the graph-viewer window.
pub fn run() {
    let inputs = Cli::parse().inputs();
    bviz_ui_shell::run(
        "bazel_viz graph",
        size(px(1280.), px(820.)),
        move |window, cx| cx.new(|cx| GraphView::new(inputs, window, cx)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::{ease_out_cubic, frame_view, settle_view, to_screen, Rect};
    use crate::paint::lane_polyline;
    use bviz_graph_layout::{Box, Kind};
    use gpui::{point, Bounds, Size};

    fn lbox(id: u32, x: f64, y: f64, w: f64, h: f64) -> Box {
        Box {
            id: bviz_graph_layout::Id(id),
            label: String::new(),
            kind: Kind::Source,
            x,
            y,
            width: w,
            height: h,
            compound: false,
            group: false,
        }
    }

    #[test]
    fn from_screen_inverts_to_screen() {
        let bounds = Bounds::new(point(px(10.), px(20.)), size(px(800.), px(600.)));
        let camera = Camera { zoom: 1.7, pan: (33.0, -12.0) };
        for &(x, y) in &[(0.0_f64, 0.0_f64), (42.5, 7.25), (123.0, 456.0)] {
            let screen = to_screen(x, y, bounds, camera);
            let (gx, gy) = from_screen(screen, bounds.origin, camera);
            assert!((gx as f64 - x).abs() < 1e-3, "x: {gx} vs {x}");
            assert!((gy as f64 - y).abs() < 1e-3, "y: {gy} vs {y}");
        }
    }

    #[test]
    fn an_emphasis_holds_a_target_in_whichever_layout_is_drawn() {
        // A box the emphasis names is named by graph id, which every layout
        // shares, so one left over from the layout that produced it still
        // resolves against the one switched to — and would settle the fresh view
        // onto it. Clearing it, as a mode switch does, leaves nothing to settle.
        let mut emphasis = Emphasis::none();
        emphasis.toggled(bviz_graph_layout::Id(7), vec![bviz_graph_layout::Id(1)]);
        let g = bviz_graph_layout::LaidOut {
            boxes: vec![lbox(1, 10.0, 20.0, 30.0, 40.0)],
            ..Default::default()
        };
        assert!(recenter_rect(&g, &emphasis).is_some());
        emphasis.clear();
        assert!(recenter_rect(&g, &emphasis).is_none());
    }

    #[test]
    fn hit_box_picks_innermost_and_none_outside() {
        // An outer container (id 0) with a small inner box (id 1) nested in it.
        let boxes = vec![lbox(0, 0.0, 0.0, 100.0, 100.0), lbox(1, 20.0, 20.0, 30.0, 30.0)];
        // Inside both rects: the smaller (inner) wins.
        assert_eq!(hit_box(&boxes, 30.0, 30.0).unwrap().id, bviz_graph_layout::Id(1));
        // Inside only the container.
        assert_eq!(hit_box(&boxes, 5.0, 5.0).unwrap().id, bviz_graph_layout::Id(0));
        // Outside everything.
        assert!(hit_box(&boxes, 200.0, 200.0).is_none());
        assert!(hit_box(&boxes, -1.0, 50.0).is_none());
    }

    #[test]
    fn lane_polyline_offsets_every_vertex_in_parallel() {
        // An L route: right to (100,0), then down to (100,100).
        let lane = lane_polyline(&[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)], 4.0);
        // Endpoints are pushed perpendicular too (not anchored), so the first and
        // last segments stay parallel to the route and meet the node offset.
        assert!((lane[0].0).abs() < 0.01 && (lane[0].1 - 4.0).abs() < 0.5, "start {:?}", lane[0]);
        assert!((lane[2].0 - 96.0).abs() < 0.5 && (lane[2].1 - 100.0).abs() < 0.01, "end {:?}", lane[2]);
        // First segment horizontal like the route's (no fanning in at the node).
        assert!((lane[0].1 - lane[1].1).abs() < 0.01, "first seg not parallel: {lane:?}");
        // Last segment vertical like the route's.
        assert!((lane[1].0 - lane[2].0).abs() < 0.01, "last seg not parallel: {lane:?}");
    }

    #[test]
    fn lane_polyline_zero_offset_is_identity() {
        let route = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)];
        assert_eq!(lane_polyline(&route, 0.0), route);
    }

    #[test]
    fn lane_polyline_opposite_signs_nest_on_opposite_sides() {
        let route = vec![(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)];
        let plus = lane_polyline(&route, 4.0)[1];
        let minus = lane_polyline(&route, -4.0)[1];
        // The two lanes never cross: their corners fall on opposite sides.
        assert_ne!(plus.1.signum(), minus.1.signum(), "{plus:?} {minus:?}");
    }

    #[test]
    fn lane_polyline_parallel_shifts_a_straight_run() {
        // A straight two-point edge shifts whole, both ends offset perpendicular.
        let lane = lane_polyline(&[(0.0, 0.0), (100.0, 0.0)], 4.0);
        assert_eq!(lane.len(), 2);
        assert!((lane[0].0).abs() < 0.01 && (lane[0].1 - 4.0).abs() < 0.5, "{lane:?}");
        assert!((lane[1].0 - 100.0).abs() < 0.01 && (lane[1].1 - 4.0).abs() < 0.5, "{lane:?}");
    }

    // The whole graph, large enough that fit_zoom's floor stays well below 1.0 and
    // never bites in these cases.
    const CONTENT: (f32, f32) = (4000.0, 3000.0);

    fn vp(w: f32, h: f32) -> Size<Pixels> {
        size(px(w), px(h))
    }

    /// Asserts `target`'s screen rect under `(zoom, pan)` lies fully within the
    /// viewport, inset by the breathing-room pad (with a small tolerance).
    fn assert_enclosed(target: Rect, vw: f32, vh: f32, zoom: f32, pan: (f32, f32)) {
        let (bx, by, bw, bh) = (target.x as f32, target.y as f32, target.w as f32, target.h as f32);
        let x0 = (bx + MARGIN as f32) * zoom + pan.0;
        let y0 = (by + MARGIN as f32) * zoom + pan.1;
        let x1 = (bx + bw + MARGIN as f32) * zoom + pan.0;
        let y1 = (by + bh + MARGIN as f32) * zoom + pan.1;
        let eps = 0.5;
        let (lo, hix, hiy) = (RECENTER_PAD - eps, vw - RECENTER_PAD + eps, vh - RECENTER_PAD + eps);
        assert!(x0 >= lo && x1 <= hix, "x out of view: {x0}..{x1} in 0..{vw}");
        assert!(y0 >= lo && y1 <= hiy, "y out of view: {y0}..{y1} in 0..{vh}");
    }

    #[test]
    fn settle_view_leaves_a_fitting_centered_target_put() {
        // A small rect already comfortably inside: neither zoom nor pan moves.
        let target = Rect { x: 375.0, y: 280.0, w: 50.0, h: 40.0 };
        let (z, p) = settle_view(target, vp(800., 600.), CONTENT, 1.0, (0.0, 0.0));
        assert!((z - 1.0).abs() < 1e-4, "zoom moved: {z}");
        assert!(p.0.abs() < 1e-3 && p.1.abs() < 1e-3, "pan moved: {p:?}");
    }

    #[test]
    fn settle_view_only_zooms_out_to_enclose_a_big_target() {
        // A rect larger than the viewport at the current zoom forces a zoom-out,
        // never below the whole-graph fit, and ends up fully framed.
        let target = Rect { x: 0.0, y: 0.0, w: 2000.0, h: 1500.0 };
        let (z, p) = settle_view(target, vp(800., 600.), CONTENT, 1.0, (0.0, 0.0));
        assert!(z < 1.0, "expected zoom-out, got {z}");
        assert!(z >= fit_zoom(vp(800., 600.), CONTENT) - 1e-4, "below whole-graph fit: {z}");
        assert_enclosed(target, 800., 600., z, p);
    }

    #[test]
    fn settle_view_never_zooms_in_on_a_small_target() {
        // A node opening smaller than the current zoom keeps the zoom — the
        // disorienting blow-up-to-fill case is gone.
        let target = Rect { x: 100.0, y: 100.0, w: 30.0, h: 20.0 };
        let (z, _) = settle_view(target, vp(800., 600.), CONTENT, 0.5, (0.0, 0.0));
        assert!(z <= 0.5 + 1e-4, "zoomed in: {z}");
    }

    #[test]
    fn settle_view_pans_an_offscreen_target_into_view_without_rezoom() {
        // A small rect off to the right fits at the current zoom, so the zoom
        // holds and only the pan shifts it back into frame.
        let target = Rect { x: 1000.0, y: 100.0, w: 50.0, h: 40.0 };
        let (z, p) = settle_view(target, vp(800., 600.), CONTENT, 1.0, (0.0, 0.0));
        assert!((z - 1.0).abs() < 1e-4, "zoom moved: {z}");
        assert!(p.0 < 0.0, "expected a leftward pan, got {p:?}");
        assert_enclosed(target, 800., 600., z, p);
    }

    #[test]
    fn frame_view_zooms_in_on_a_small_target_and_centers_it() {
        // The rect a map click hands over is far smaller than the viewport, and
        // framing it magnifies — the direction settle_view never takes.
        let target = Rect { x: 1000.0, y: 800.0, w: 200.0, h: 150.0 };
        let (z, p) = frame_view(target, vp(800., 600.), CONTENT);
        assert!(z > 1.0, "expected a zoom-in, got {z}");
        let center = |lo: f64, extent: f64, pan: f32| {
            (lo as f32 + extent as f32 / 2. + MARGIN as f32) * z + pan
        };
        assert!((center(target.x, target.w, p.0) - 400.).abs() < 0.5, "x off center: {p:?}");
        assert!((center(target.y, target.h, p.1) - 300.).abs() < 0.5, "y off center: {p:?}");
        assert_enclosed(target, 800., 600., z, p);
    }

    #[test]
    fn frame_view_stays_between_the_whole_graph_fit_and_the_zoom_ceiling() {
        let viewport = vp(800., 600.);
        let pinhead = Rect { x: 10.0, y: 10.0, w: 0.5, h: 0.5 };
        let (z, _) = frame_view(pinhead, viewport, CONTENT);
        assert!((z - MAX_ZOOM).abs() < 1e-4, "a tiny target passed the ceiling: {z}");
        let whole = Rect { x: 0.0, y: 0.0, w: CONTENT.0 as f64, h: CONTENT.1 as f64 };
        let (z, _) = frame_view(whole, viewport, CONTENT);
        assert!((z - fit_zoom(viewport, CONTENT)).abs() < 1e-4, "framing everything is the fit: {z}");
    }

    /// The window in layout space that the culling tests measure against.
    const WINDOW: Rect = Rect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 };

    #[test]
    fn a_route_is_kept_only_where_it_runs_through_the_window() {
        // A long diagonal whose bounding box swallows the window, but which
        // passes well clear of it: the bounds alone would keep it.
        let across = [(0.0, 1000.0), (1000.0, 0.0)];
        assert!(WINDOW.overlaps(&Rect { x: 0.0, y: 0.0, w: 1000.0, h: 1000.0 }));
        assert!(!WINDOW.crossed_by(&across));
        assert!(WINDOW.crossed_by(&[(-100.0, 5.0), (100.0, 5.0)]));
        // A route entirely inside, and one that only clips a corner leg.
        assert!(WINDOW.crossed_by(&[(2.0, 2.0), (8.0, 8.0)]));
        assert!(WINDOW.crossed_by(&[(-5.0, -5.0), (-5.0, 5.0), (5.0, 5.0)]));
        assert!(!WINDOW.crossed_by(&[(-5.0, -5.0), (-5.0, 50.0), (-1.0, 50.0)]));
    }

    #[test]
    fn a_straight_run_along_an_axis_still_registers() {
        // A vertical or horizontal leg spans no area, so an area test alone
        // would drop it.
        assert!(WINDOW.crossed_by(&[(5.0, -50.0), (5.0, 50.0)]));
        assert!(WINDOW.crossed_by(&[(-50.0, 5.0), (50.0, 5.0)]));
        assert!(!WINDOW.crossed_by(&[(50.0, -50.0), (50.0, 50.0)]));
    }

    #[test]
    fn ease_out_cubic_endpoints_and_deceleration() {
        assert!(ease_out_cubic(0.0).abs() < 1e-6, "{}", ease_out_cubic(0.0));
        assert!((ease_out_cubic(1.0) - 1.0).abs() < 1e-6, "{}", ease_out_cubic(1.0));
        // Ease-out: past the halfway mark in eased progress before halfway in time.
        assert!(ease_out_cubic(0.5) > 0.5, "{}", ease_out_cubic(0.5));
        // Monotonically increasing.
        let mut prev = ease_out_cubic(0.0);
        for i in 1..=10 {
            let e = ease_out_cubic(i as f32 / 10.0);
            assert!(e >= prev, "not monotonic at {i}: {e} < {prev}");
            prev = e;
        }
    }

    #[test]
    fn enclosed_boxes_keeps_only_fully_visible() {
        let boxes = vec![
            lbox(0, 100.0, 100.0, 50.0, 40.0),  // fully inside
            lbox(1, 760.0, 100.0, 100.0, 40.0), // spills past the right edge
            lbox(2, 100.0, 580.0, 50.0, 40.0),  // spills past the bottom edge
            lbox(3, 200.0, 200.0, 30.0, 30.0),  // fully inside
        ];
        let mut ids = enclosed_boxes(&boxes, vp(800., 600.), Camera { zoom: 1.0, pan: (0.0, 0.0) });
        ids.sort_by_key(|id| id.0);
        assert_eq!(ids, vec![bviz_graph_layout::Id(0), bviz_graph_layout::Id(3)]);
    }

    /// The breathing room `clamp_pan` allows past the flush edge on an axis, matching
    /// the fit zoom's margin.
    fn clearance(vp_axis: f32) -> f32 {
        vp_axis * (1.0 - FIT_MARGIN) / 2.0
    }

    #[test]
    fn clamp_pan_keeps_smaller_content_within_the_viewport() {
        // Content smaller than the viewport (zoom 0.1) stays inside, give or take the
        // edge clearance, so the pan stays in -clearance..=(viewport - content + clearance).
        let viewport = vp(800., 600.);
        let zoom = 0.1;
        let (cw, ch) = (CONTENT.0 * zoom, CONTENT.1 * zoom);
        let (mx, my) = (clearance(800.), clearance(600.));
        assert_eq!(clamp_pan((-9999.0, -9999.0), zoom, viewport, CONTENT), (-mx, -my));
        assert_eq!(
            clamp_pan((9999.0, 9999.0), zoom, viewport, CONTENT),
            (800.0 - cw + mx, 600.0 - ch + my)
        );
        // A pan within the bound is left untouched, including a small overscroll into
        // the clearance the strict edge would have snapped away.
        assert_eq!(clamp_pan((-50.0, -50.0), zoom, viewport, CONTENT), (-50.0, -50.0));
        assert_eq!(clamp_pan((100.0, 80.0), zoom, viewport, CONTENT), (100.0, 80.0));
    }

    #[test]
    fn clamp_pan_keeps_larger_content_covering_the_viewport() {
        // Content larger than the viewport (zoom 1.0) may pull a clearance-sized gutter
        // past an edge but no further, so the pan stays in
        // (viewport - content - clearance)..=clearance.
        let viewport = vp(800., 600.);
        let (cw, ch) = (CONTENT.0, CONTENT.1);
        let (mx, my) = (clearance(800.), clearance(600.));
        assert_eq!(clamp_pan((9999.0, 9999.0), 1.0, viewport, CONTENT), (mx, my));
        assert_eq!(
            clamp_pan((-9999.0, -9999.0), 1.0, viewport, CONTENT),
            (800.0 - cw - mx, 600.0 - ch - my)
        );
        // The flush-cover position the strict bound enforced is still permitted.
        assert_eq!(clamp_pan((0.0, 0.0), 1.0, viewport, CONTENT), (0.0, 0.0));
    }
}
