//! Loads a Bazel `aquery`, `cquery`, or `query` `--output=streamed_proto` file
//! through `bviz_graph_layout`, lays the graph out, and opens a window
//! rendering the result.

use bviz_graph_layout::graph::Graph;
use bviz_graph_layout::{EdgeKind, Id, Kind, LaidOut, Names};
use bviz_ui_components::{load_in_background, track_drag, Axis};
use bviz_ui_shell::Chrome;
use clap::Parser;
use gpui::{
    App, Bounds, Context, Div, Entity, FocusHandle, Hsla, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, Rgba, ScrollDelta,
    ScrollWheelEvent, Stateful, Task, Window, canvas, div, prelude::*, px, rems, size,
};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

mod build;
mod camera;
mod dimming;
mod paint;
mod scroll;

use build::{build_graph, relayout, Input, InputKind};
use camera::{
    clamp_pan, content_size, ease_out_cubic, enclosed_boxes, fit_view, fit_zoom, from_screen,
    recenter_rect, settle_view, to_screen, Camera, Settle,
};
use dimming::compute_dimming;
use paint::{
    collect_connectors, edge_color, kind_accent, lane_polyline, paint_box, paint_boxes,
    paint_connector,
};
use scroll::{
    end_scroll_drag, scroll_drag_move, scrollbar_geom, scrollbar_hit, start_scroll_drag, ScrollDrag,
};

/// Padding around the graph, in layout pixels.
const MARGIN: f64 = 16.0;

/// Perpendicular gap (pre-zoom px) between the parallel provenance lines drawn
/// along a node pair that several queries connect.
const PARALLEL_SPACING: f32 = 3.0;

/// Largest zoom-in factor. The smallest is computed per graph so the whole graph
/// always fits the viewport.
const MAX_ZOOM: f32 = 4.0;

/// Startup expands containment levels outward from the roots until the visible
/// view would exceed either cap, so a large graph opens to a bounded but
/// non-trivial overview rather than a single collapsed box.
const MAX_VISIBLE_NODES: usize = 400;
const MAX_VISIBLE_EDGES: usize = 600;

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

/// The loaded graph, a load in progress, or a message explaining why none is
/// shown.
enum Source {
    Loading,
    Loaded(LaidOut),
    Message(String),
}

/// A box flattened to what the canvas needs to paint it, in layout pixels.
struct PaintBox {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    compound: bool,
    /// Whether this box can be opened or closed (a container or a collapsed
    /// group), so the renderer draws a disclosure caret: down when open
    /// (compound), right when collapsed.
    expandable: bool,
    /// Fill resolved from the active theme when the box vec is built.
    fill: Rgba,
    label: String,
    /// Highlight alpha for the just-toggled box (0.0 = none), scaling the
    /// emphasis border and inset glow as it fades.
    highlight: f32,
    /// Outside the hovered node's neighborhood, drawn faded to recede.
    dim: bool,
}

/// The shared, interior-mutable view state threaded through the render tree and
/// its prepaint/paint closures: zoom factor, pan offset, whether the initial fit
/// has run, the last painted viewport, a pending recenter request, and the
/// in-flight settle animation.
#[derive(Clone)]
struct ViewState {
    zoom: Rc<Cell<f32>>,
    pan: Rc<Cell<(f32, f32)>>,
    fitted: Rc<Cell<bool>>,
    viewport: Rc<Cell<Bounds<Pixels>>>,
    recenter: Rc<Cell<bool>>,
    anim: Rc<Cell<Option<Settle>>>,
}

/// Loads, lays out, and renders a Bazel build graph.
///
/// Zoom, pan, and the captured viewport bounds are shared cells so the canvas
/// can both read them while painting and, on the first frame, set the initial
/// fit-to-window view (which needs the viewport size, known only at paint time).
struct GraphView {
    focus_handle: FocusHandle,
    source: Source,
    view_state: ViewState,
    dragging: bool,
    /// Whether the cursor moved past the click threshold since the press, which
    /// distinguishes a drag (pan) from a click (toggle).
    moved: bool,
    last_mouse: Point<Pixels>,
    /// Cursor position at the most recent left press, the click/drag origin.
    press: Point<Pixels>,
    /// The scroll-bar drag in progress, or `None` when no thumb is held.
    scroll_drag: Option<ScrollDrag>,
    /// The built (pre-quotient) graph, cached so toggling a group re-lays-out
    /// without re-reading or re-parsing the proto. `None` until the load lands.
    base: Rc<RefCell<Option<Graph>>>,
    /// The base with its transitively-redundant edges removed, cached so the
    /// reduced view's edges are a stable projection of one fixed skeleton.
    reduced_base: Rc<RefCell<Option<Graph>>>,
    /// The interned node-id table the base graph keys on, threaded into each
    /// relayout so the quotient pass can mint group ids. `None` until the load
    /// lands.
    names: Rc<RefCell<Option<Names>>>,
    /// Container and group ids currently expanded; every other container folds
    /// its subtree away so the layout only processes the visible view.
    expanded: Rc<RefCell<HashSet<Id>>>,
    /// Multi-child container ids of the base graph; only these carry an
    /// expand/collapse handle (a leaf can't open, a singleton auto-expands), so a
    /// click or caret can tell an expandable box from a plain one.
    expandable: Rc<RefCell<HashSet<Id>>>,
    /// The most-recently toggled node id: the box its relayout frames or
    /// recenters, and the box emphasized briefly afterward so the eye can find
    /// what opened or closed. Cleared when the highlight fades.
    recent: Option<Id>,
    /// When the highlight began — set as the relayout lands, so the emphasis
    /// fades over `HIGHLIGHT_LINGER` measured from when the box becomes visible.
    /// A relayout slower than the linger would otherwise outlast a fade timed
    /// from the click, surfacing the box already un-highlighted.
    recent_at: Instant,
    /// Guards the highlight-fade timer: a toggle bumps this so any in-flight fade
    /// retires at once, leaving `recent` for the new toggle rather than clearing
    /// it or repainting a stale highlight.
    highlight_seq: u64,
    /// Ids of the boxes the viewport fully enclosed when the recent toggle fired.
    /// The settling relayout keeps these — and the toggled box — in view, so the
    /// view tracks what the user was looking at rather than snapping to the
    /// toggled box alone.
    prev_visible: Vec<Id>,
    /// Whether a relayout is running, so the view shows a progress indicator
    /// while the prior layout stays interactive.
    relaying: bool,
    /// Node under the cursor, whose neighborhood stays lit while everything else
    /// dims; `None` when the cursor is over no node.
    hovered: Option<Id>,
    /// Whether hovering a node dims the rest of the graph; flipped by the `h` key
    /// or the on-screen toggle so the dimming can be turned off when it is more
    /// noise than help.
    dim_enabled: bool,
    /// Whether the view draws the transitive reduction of the edges (the default)
    /// rather than every edge; flipped by the `r` key or the on-screen toggle,
    /// which relays out. Hover lights up a node's full neighbors either way.
    reduced: bool,
    /// Holds the background load/relayout task so it isn't cancelled on drop.
    _load_task: Task<()>,
}

/// A cursor must travel at least this far from the press before the gesture
/// counts as a drag rather than a click.
const CLICK_SLOP: f32 = 4.0;

/// How long the just-toggled box stays emphasized before the highlight fades.
const HIGHLIGHT_LINGER: Duration = Duration::from_millis(1100);

impl GraphView {
    fn new(inputs: Vec<Input>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let base: Rc<RefCell<Option<Graph>>> = Rc::new(RefCell::new(None));
        let reduced_base: Rc<RefCell<Option<Graph>>> = Rc::new(RefCell::new(None));
        let names: Rc<RefCell<Option<Names>>> = Rc::new(RefCell::new(None));
        // Build and lay out the graph on the background executor so window
        // appearance and interactivity are never blocked; swap the view to the
        // result and cache the base graph when it completes.
        let load_task = load_in_background(
            cx,
            move || {
                let (graph, reduced, mut names) = build_graph(&inputs)?;
                // Open to a bounded overview instead of laying out the whole graph.
                let expanded = bviz_graph_layout::lod::default_expansion(
                    &graph,
                    MAX_VISIBLE_NODES,
                    MAX_VISIBLE_EDGES,
                );
                // Open showing the transitive reduction (the `reduced` default).
                let laid_out = relayout(&graph, &reduced, &expanded, &mut names, true);
                Ok::<_, String>((graph, reduced, names, expanded, laid_out))
            },
            |view, result, _| {
                view.source = match result {
                    Ok((graph, reduced, names, expanded, laid_out)) => {
                        *view.expandable.borrow_mut() =
                            bviz_graph_layout::lod::expandable_containers(&graph);
                        *view.expanded.borrow_mut() = expanded;
                        *view.base.borrow_mut() = Some(graph);
                        *view.reduced_base.borrow_mut() = Some(reduced);
                        *view.names.borrow_mut() = Some(names);
                        Source::Loaded(laid_out)
                    }
                    Err(err) => Source::Message(err),
                };
            },
        );
        Self {
            focus_handle,
            source: Source::Loading,
            view_state: ViewState {
                zoom: Rc::new(Cell::new(1.0)),
                pan: Rc::new(Cell::new((0.0, 0.0))),
                fitted: Rc::new(Cell::new(false)),
                viewport: Rc::new(Cell::new(Bounds::default())),
                recenter: Rc::new(Cell::new(false)),
                anim: Rc::new(Cell::new(None)),
            },
            dragging: false,
            moved: false,
            last_mouse: Point::default(),
            press: Point::default(),
            scroll_drag: None,
            base,
            reduced_base,
            names,
            expanded: Rc::new(RefCell::new(HashSet::new())),
            expandable: Rc::new(RefCell::new(HashSet::new())),
            recent: None,
            recent_at: Instant::now(),
            highlight_seq: 0,
            prev_visible: Vec::new(),
            relaying: false,
            hovered: None,
            dim_enabled: true,
            reduced: true,
            _load_task: load_task,
        }
    }

    /// Re-lays-out the cached base graph with the current expansion set off the
    /// UI thread and swaps in the result, leaving zoom, pan, and the fit flag
    /// untouched so the view stays put across a toggle.
    fn retoggle(&mut self, cx: &mut Context<Self>) {
        let Some(graph) = self.base.borrow().clone() else {
            return;
        };
        let Some(reduced_base) = self.reduced_base.borrow().clone() else {
            return;
        };
        // The background relayout mints group ids against the interned-name table,
        // so it gets a clone to extend and returns it; the view adopts that
        // accumulated table when the task lands. Cloning (rather than moving the
        // table out) keeps a superseded toggle's table from stranding the slot, so
        // a fresh toggle can still cancel and replace an in-flight relayout.
        let Some(mut names) = self.names.borrow().clone() else {
            return;
        };
        let expanded = self.expanded.borrow().clone();
        let reduced = self.reduced;
        self.relaying = true;
        self._load_task = load_in_background(
            cx,
            move || {
                let laid_out = relayout(&graph, &reduced_base, &expanded, &mut names, reduced);
                (laid_out, names)
            },
            |view, (laid_out, names), cx| {
                *view.names.borrow_mut() = Some(names);
                view.source = Source::Loaded(laid_out);
                view.view_state.recenter.set(true);
                view.relaying = false;
                view.highlight_recent(cx);
            },
        );
    }

    /// Emphasizes `recent` (the just-toggled box) for `HIGHLIGHT_LINGER`,
    /// repainting so the emphasis fades smoothly, then clears it. Begun once the
    /// relayout lands so the highlight appears on the new layout: a relayout
    /// slower than the linger would outlast a fade begun at click time, so the
    /// box would surface with no highlight at all.
    fn highlight_recent(&mut self, cx: &mut Context<Self>) {
        self.recent_at = Instant::now();
        let seq = self.highlight_seq;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(33)).await;
                let done = this.update(cx, |this, cx| {
                    if this.highlight_seq != seq {
                        return true;
                    }
                    if this.recent_at.elapsed() >= HIGHLIGHT_LINGER {
                        this.recent = None;
                        cx.notify();
                        return true;
                    }
                    cx.notify();
                    false
                });
                if done.unwrap_or(true) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Assembles the single-canvas viewport: the painted boxes and connectors,
    /// the overlay controls layered over them, and the prepaint/paint closures
    /// that fit and draw the graph under the shared pan/zoom transform.
    fn graph(
        g: &LaidOut,
        expandable: &HashSet<Id>,
        entity: Entity<GraphView>,
        view_state: ViewState,
        flags: ViewFlags,
        palette: Palette,
    ) -> Stateful<Div> {
        let ViewFlags { highlight, prev_visible, relaying, hovered, dim_enabled, reduced } = flags;
        let dimming = compute_dimming(g, hovered, dim_enabled);
        let boxes = paint_boxes(g, expandable, highlight, &dimming, &palette);
        let connectors = collect_connectors(g, &dimming);
        let legend = legend(g, &palette);
        let relaying_indicator = relaying_indicator(relaying, &palette);
        // Kept in sync with the `h` key, which flips the same field.
        let dim_toggle = bviz_ui_components::toggle_pill(
            "dim-toggle",
            rems(0.5),
            "dim",
            dim_enabled,
            bviz_ui_components::PillColors {
                accent: kind_accent(Kind::Artifact),
                off_label: palette.label,
                off_border: palette.border,
                fill: palette.container_fill,
            },
            entity.clone(),
            |this, _| this.dim_enabled = !this.dim_enabled,
        );
        // Sits below the dim pill; flipping it relays out the view with or without
        // transitive reduction.
        let reduce_toggle = bviz_ui_components::toggle_pill(
            "reduce-toggle",
            px(40.),
            "reduce",
            reduced,
            bviz_ui_components::PillColors {
                accent: kind_accent(Kind::Artifact),
                off_label: palette.label,
                off_border: palette.border,
                fill: palette.container_fill,
            },
            entity.clone(),
            |this, cx| {
                this.reduced = !this.reduced;
                this.retoggle(cx);
            },
        );
        let content = content_size(g);
        let recenter_target = recenter_rect(g, highlight.map(|h| h.id), prev_visible);
        let prepaint = build_prepaint(view_state.clone(), content, recenter_target);
        let paint = canvas_body(
            boxes,
            connectors,
            view_state.zoom.clone(),
            view_state.pan.clone(),
            palette,
            content,
            entity,
        );
        div()
            .id("graph-viewport")
            .relative()
            .size_full()
            .overflow_hidden()
            .child(
                canvas(prepaint, paint)
                    .absolute()
                    .left_0()
                    .top_0()
                    .size_full(),
            )
            .children(legend)
            .children(relaying_indicator)
            .child(dim_toggle)
            .child(reduce_toggle)
    }

    fn status(&self) -> String {
        match &self.source {
            Source::Loaded(g) => format!(
                "{} boxes · {} connectors · {}×{}",
                g.boxes.len(),
                g.connectors.len(),
                g.width.round() as i64,
                g.height.round() as i64,
            ),
            Source::Loading => "Loading…".into(),
            Source::Message(_) => "No graph".into(),
        }
    }

    fn on_mouse_down(&mut self, e: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        // A press on a scroll-bar gutter drives the thumb instead of panning the
        // content under it.
        if let Some(axis) = scrollbar_hit(self, e.position) {
            start_scroll_drag(self, axis, e.position, cx);
            return;
        }
        // Start a pan drag. The window-global move handler that pans it is
        // installed from the canvas paint while `dragging` is set (so it keeps
        // working past the window edge), hence the repaint request. The release is
        // caught both there and by `on_mouse_up`.
        self.dragging = true;
        self.moved = false;
        self.press = e.position;
        self.last_mouse = e.position;
        cx.notify();
    }

    /// Ends the drag on the release the element receives directly. The canvas-paint
    /// handler also ends it (and catches releases outside the window), but it is
    /// only installed once a frame paints with `dragging` set; a press-move-release
    /// quick enough to finish before that paint would otherwise never clear
    /// `dragging`, leaving every later move panning the view.
    fn on_mouse_up(&mut self, e: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        end_drag(self, e, cx);
        end_scroll_drag(self, cx);
    }

    fn on_mouse_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        // A drag pans rather than hovers; leave the lit set alone.
        if self.dragging {
            return;
        }
        let Source::Loaded(g) = &self.source else {
            return;
        };
        let vp = self.view_state.viewport.get();
        let (gx, gy) = from_screen(
            e.position,
            vp.origin,
            Camera { zoom: self.view_state.zoom.get(), pan: self.view_state.pan.get() },
        );
        let hit = hit_box(&g.boxes, gx, gy).map(|b| b.id);
        if hit != self.hovered {
            self.hovered = hit;
            cx.notify();
        }
    }

    fn on_scroll_wheel(&mut self, e: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        // Anchor the zoom at the cursor: keep the content point under the cursor
        // fixed as the scale changes. The minimum zoom is the fit, so the graph
        // can't be zoomed out of view.
        let content = match &self.source {
            Source::Loaded(g) => content_size(g),
            _ => return,
        };
        let vp = self.view_state.viewport.get();
        let lx = f32::from(e.position.x) - f32::from(vp.origin.x);
        let ly = f32::from(e.position.y) - f32::from(vp.origin.y);
        let dy = match e.delta {
            ScrollDelta::Lines(p) => p.y,
            ScrollDelta::Pixels(p) => f32::from(p.y) / 120.0,
        };
        let old = self.view_state.zoom.get();
        let new = (old * 1.1_f32.powf(dy)).clamp(fit_zoom(vp.size, content), MAX_ZOOM);
        let (px, py) = self.view_state.pan.get();
        let content_x = (lx - px) / old;
        let content_y = (ly - py) / old;
        let pan = (lx - content_x * new, ly - content_y * new);
        self.view_state.pan.set(clamp_pan(pan, new, vp.size, content));
        self.view_state.zoom.set(new);
        // A manual zoom overrides any settle still easing.
        self.view_state.anim.set(None);
        cx.notify();
    }

    fn on_key_down(&mut self, e: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if e.keystroke.key == "h" {
            self.dim_enabled = !self.dim_enabled;
            cx.notify();
        } else if e.keystroke.key == "r" {
            self.reduced = !self.reduced;
            self.retoggle(cx);
            cx.notify();
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
        let body = match &self.source {
            Source::Loaded(g) => div()
                .flex_1()
                .flex()
                .overflow_hidden()
                .child(
                    Self::graph(
                        g,
                        &self.expandable.borrow(),
                        cx.entity(),
                        self.view_state.clone(),
                        ViewFlags {
                            highlight: self.recent.map(|id| Highlight { id, at: self.recent_at }),
                            prev_visible: &self.prev_visible,
                            relaying: self.relaying,
                            hovered: self.hovered,
                            dim_enabled: self.dim_enabled,
                            reduced: self.reduced,
                        },
                        palette,
                    )
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
                    .on_mouse_move(cx.listener(Self::on_mouse_move))
                    .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
                    // Track the shared focus handle here too (the chrome already
                    // tracks it on an ancestor) so this deepest focused element
                    // receives key events. Events still bubble up to the chrome's
                    // own key bindings.
                    .track_focus(&self.focus_handle)
                    .on_key_down(cx.listener(Self::on_key_down)),
                )
                .into_any_element(),
            Source::Loading => bviz_ui_components::centered_spinner("Loading…", &colors)
                .into_any_element(),
            Source::Message(msg) => {
                bviz_ui_components::centered_notice(msg.clone(), &colors).into_any_element()
            }
        };
        Chrome::new(&self.focus_handle)
            .title("bazel_viz graph")
            .status(self.status())
            .child(body)
    }
}

/// The just-toggled box and the instant its highlight began, so the fade
/// emphasis and the time it is measured from travel together to the painter.
#[derive(Clone, Copy)]
struct Highlight {
    id: Id,
    at: Instant,
}

/// The per-render view state the layout reads: what to highlight, the previously
/// visible nodes a relayout animates from, whether a relayout is running, the
/// hovered node, and the dim and reduce toggle states.
struct ViewFlags<'a> {
    highlight: Option<Highlight>,
    prev_visible: &'a [Id],
    relaying: bool,
    hovered: Option<Id>,
    dim_enabled: bool,
    reduced: bool,
}

/// Overlay listing the edge provenances actually drawn, in enum order, so it
/// explains only the kinds present in this graph. `None` when none are drawn.
fn legend(g: &LaidOut, palette: &Palette) -> Option<Div> {
    let present: Vec<EdgeKind> = [EdgeKind::Query, EdgeKind::Cquery, EdgeKind::Aquery]
        .into_iter()
        .filter(|k| g.connectors.iter().any(|c| c.kinds.contains(k)))
        .collect();
    (!present.is_empty()).then(|| {
        div()
            .absolute()
            .left_2()
            .bottom_2()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_md()
            .bg(palette.container_fill)
            .children(present.into_iter().map(|k| {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(16.)).h(px(2.)).bg(edge_color(k)))
                    .child(div().text_xs().text_color(palette.label).child(k.label()))
            }))
    })
}

/// Spinner shown while a relayout runs, so the prior layout stays interactive
/// with a visible cue that a new one is in flight. `None` when not relaying.
fn relaying_indicator(relaying: bool, palette: &Palette) -> Option<Div> {
    relaying.then(|| {
        div()
            .absolute()
            .right_2()
            .top_2()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(palette.container_fill)
            .child(bviz_ui_components::spinner(
                "relaying-spinner",
                px(14.),
                palette.label,
            ))
            .child(div().text_xs().text_color(palette.label).child("laying out…"))
    })
}

/// Fits the graph to the viewport on the first frame, holds the framing stable
/// across a resize, and drives the settle animation toward `recenter_target`.
/// Runs at canvas prepaint because the fit needs the viewport size, known only
/// once laid out. Owns the shared cells so it can both read and update the view.
fn build_prepaint(
    view_state: ViewState,
    content: (f32, f32),
    recenter_target: Option<(f64, f64, f64, f64)>,
) -> impl FnOnce(Bounds<Pixels>, &mut Window, &mut App) {
    let ViewState { zoom, pan, fitted, viewport, recenter, anim } = view_state;
    move |bounds: Bounds<Pixels>, window: &mut Window, _: &mut App| {
        let prev = viewport.get();
        viewport.set(bounds);
        if !fitted.get() {
            let (z, p) = fit_view(bounds.size, content);
            zoom.set(z);
            pan.set(p);
            fitted.set(true);
        } else if prev.size != bounds.size {
            // Keep the view stable across a resize at any zoom level: hold the
            // graph point under the viewport center fixed and scale the zoom with
            // the fit, so the same section stays framed at the same zoom relative
            // to the window rather than snapping to the fully fitted view. A
            // settle in flight targeted the old size, so drop it rather than ease
            // toward a stale destination.
            anim.set(None);
            let old_fit = fit_zoom(prev.size, content);
            let new_fit = fit_zoom(bounds.size, content);
            let z_old = zoom.get();
            let (pan_x, pan_y) = pan.get();
            let center_x = (f32::from(prev.size.width) / 2. - pan_x) / z_old;
            let center_y = (f32::from(prev.size.height) / 2. - pan_y) / z_old;
            let z = (z_old * new_fit / old_fit).clamp(new_fit, MAX_ZOOM);
            zoom.set(z);
            pan.set(clamp_pan(
                (
                    f32::from(bounds.size.width) / 2. - center_x * z,
                    f32::from(bounds.size.height) / 2. - center_y * z,
                ),
                z,
                bounds.size,
                content,
            ));
        }
        // Settle the view on the toggled box and the boxes that shared the screen
        // with it. Fit that rect, but only ever zoom out: a node opening smaller
        // than the current zoom must not blow up to fill the view, the
        // disorienting case this avoids. Hold the rect's center fixed across the
        // zoom change so the content does not lurch, then pan the minimum needed
        // to seat the rect fully inside — leaving the view put when it already
        // fits. The move is eased over a few frames (the driver below) rather than
        // jumped.
        if recenter.get() {
            if let Some(target) = recenter_target {
                let (tz, (tpx, tpy)) =
                    settle_view(target, bounds.size, content, zoom.get(), pan.get());
                let (z, (px, py)) = (zoom.get(), pan.get());
                let moves = (tz - z).abs() > SETTLE_MIN_ZOOM_STEP
                    || (tpx - px).abs() > SETTLE_MIN_PAN_STEP
                    || (tpy - py).abs() > SETTLE_MIN_PAN_STEP;
                if moves {
                    anim.set(Some(Settle {
                        from_zoom: z,
                        from_pan: (px, py),
                        to_zoom: tz,
                        to_pan: (tpx, tpy),
                        start: Instant::now(),
                    }));
                }
            }
            recenter.set(false);
        }
        // Advance an in-flight settle and ask for the next frame until it lands,
        // then snap to the target and stop, so the window returns to idle.
        // request_animation_frame redraws even with no other change.
        if let Some(a) = anim.get() {
            let t =
                (a.start.elapsed().as_secs_f32() / SETTLE_DURATION.as_secs_f32()).clamp(0.0, 1.0);
            let e = ease_out_cubic(t);
            zoom.set(a.from_zoom + (a.to_zoom - a.from_zoom) * e);
            pan.set((
                a.from_pan.0 + (a.to_pan.0 - a.from_pan.0) * e,
                a.from_pan.1 + (a.to_pan.1 - a.from_pan.1) * e,
            ));
            if t >= 1.0 {
                anim.set(None);
            } else {
                window.request_animation_frame();
            }
        }
    }
}

/// The canvas paint pass: draws every box parent-before-child so children sit
/// over their containers, then the connectors over the fills (fanning each node
/// pair's provenances into nested lanes), then the scroll bars, and installs the
/// window-global drag handlers while a pan or scroll-bar drag is in progress.
/// Owns snapshots of the boxes and connectors so it need not borrow the graph.
fn canvas_body(
    boxes: Vec<PaintBox>,
    connectors: Vec<(Vec<(f64, f64)>, Vec<EdgeKind>, bool)>,
    zoom: Rc<Cell<f32>>,
    pan: Rc<Cell<(f32, f32)>>,
    palette: Palette,
    content: (f32, f32),
    entity: Entity<GraphView>,
) -> impl FnOnce(Bounds<Pixels>, (), &mut Window, &mut App) {
    move |bounds, _, window, cx| {
        let z = zoom.get();
        let p = pan.get();
        let camera = Camera { zoom: z, pan: p };
        for b in &boxes {
            paint_box(b, bounds, camera, &palette, window, cx);
        }
        for (points, kinds, dim) in &connectors {
            // ELK routes a node pair once; fan its provenances out into nested
            // lanes centered on that route — each anchored at the nodes like a
            // lone edge — so a pair several queries connect still shows each.
            let screen: Vec<(f32, f32)> = points
                .iter()
                .map(|(x, y)| {
                    let s = to_screen(*x, *y, bounds, camera);
                    (f32::from(s.x), f32::from(s.y))
                })
                .collect();
            let n = kinds.len() as f32;
            for (i, kind) in kinds.iter().enumerate() {
                let slot = i as f32 - (n - 1.0) / 2.0;
                let lane = lane_polyline(&screen, slot * PARALLEL_SPACING * z);
                paint_connector(&lane, *kind, *dim, z, window);
            }
        }
        // Scroll bars along the edges of whichever axes overflow the viewport, an
        // alternative to drag-panning.
        let track = Hsla::from(bviz_ui_theme::mix(palette.border, palette.background, 0.5));
        let thumb = Hsla::from(bviz_ui_theme::mix(palette.label, palette.background, 0.45));
        for axis in [Axis::Vertical, Axis::Horizontal] {
            if let Some(bar) = scrollbar_geom(axis, bounds, content, camera) {
                bar.paint(track, thumb, window);
            }
        }
        // While a pan or a scroll-bar drag is in progress, run it through
        // window-global handlers so it keeps working when the cursor leaves the
        // window. A pan release also toggles the group under it when it was a
        // click rather than a drag.
        let (dragging, scroll_dragging) = {
            let v = entity.read(cx);
            (v.dragging, v.scroll_drag.is_some())
        };
        if dragging {
            track_drag(window, &entity, drag_pan, end_drag);
        } else if scroll_dragging {
            track_drag(window, &entity, scroll_drag_move, |view, _, cx| {
                end_scroll_drag(view, cx)
            });
        }
    }
}

/// Pan the graph by the cursor delta during a drag, latching `moved` once the
/// cursor has travelled past the click threshold.
fn drag_pan(view: &mut GraphView, event: &MouseMoveEvent, cx: &mut Context<GraphView>) {
    let traveled = (f32::from(event.position.x) - f32::from(view.press.x))
        .hypot(f32::from(event.position.y) - f32::from(view.press.y));
    if traveled > CLICK_SLOP {
        view.moved = true;
    }
    let (px, py) = view.view_state.pan.get();
    let dx = f32::from(event.position.x) - f32::from(view.last_mouse.x);
    let dy = f32::from(event.position.y) - f32::from(view.last_mouse.y);
    let pan = (px + dx, py + dy);
    view.view_state.pan.set(match &view.source {
        Source::Loaded(g) => {
            let content = content_size(g);
            clamp_pan(pan, view.view_state.zoom.get(), view.view_state.viewport.get().size, content)
        }
        _ => pan,
    });
    view.last_mouse = event.position;
    // A manual pan overrides any settle still easing.
    view.view_state.anim.set(None);
    cx.notify();
}

/// End a drag. A release that never travelled past the click threshold is a
/// click rather than a pan.
fn end_drag(view: &mut GraphView, event: &MouseUpEvent, cx: &mut Context<GraphView>) {
    // Both the element-level release handler and the window-global one installed
    // during a drag can fire for the same release; guard so the toggle below runs
    // once. The guard also makes a stray release (no drag in progress) a no-op.
    if !view.dragging {
        return;
    }
    view.dragging = false;
    if view.moved {
        return;
    }
    toggle_box_at(view, event.position, cx);
}

/// Open or close the container (or group) box under `pos` (window px) and settle
/// the view to keep it in frame.
fn toggle_box_at(view: &mut GraphView, pos: Point<Pixels>, cx: &mut Context<GraphView>) {
    let Source::Loaded(g) = &view.source else {
        return;
    };
    let vp = view.view_state.viewport.get();
    let (gx, gy) = from_screen(
        pos,
        vp.origin,
        Camera { zoom: view.view_state.zoom.get(), pan: view.view_state.pan.get() },
    );
    let Some(hit) = hit_box(&g.boxes, gx, gy) else {
        return;
    };
    // A multi-child container or a collapsed group can be opened/closed; a leaf
    // or singleton can't.
    if !hit.group && !view.expandable.borrow().contains(&hit.id) {
        return;
    }
    let id = hit.id;
    let mut expanded = view.expanded.borrow_mut();
    let removed = expanded.remove(&id);
    if !removed {
        expanded.insert(id);
    }
    drop(expanded);
    // The boxes fully on screen now, so the relayout settles the view to keep
    // them — and the toggled box — in frame.
    view.prev_visible = enclosed_boxes(
        &g.boxes,
        vp.size,
        Camera { zoom: view.view_state.zoom.get(), pan: view.view_state.pan.get() },
    );
    // The relayout may drop the hovered node (it was collapsed away), which would
    // otherwise leave the whole graph dimmed until the next mouse move.
    view.hovered = None;
    view.recent = Some(id);
    // Retire any in-flight fade so it can't clear this fresh `recent`; the new
    // highlight starts when the relayout lands.
    view.highlight_seq = view.highlight_seq.wrapping_add(1);
    view.retoggle(cx);
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
    use bviz_graph_layout::Box;
    use gpui::{point, Size};

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
    fn assert_enclosed(target: (f64, f64, f64, f64), vw: f32, vh: f32, zoom: f32, pan: (f32, f32)) {
        let (bx, by, bw, bh) = (target.0 as f32, target.1 as f32, target.2 as f32, target.3 as f32);
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
        let target = (375.0, 280.0, 50.0, 40.0);
        let (z, p) = settle_view(target, vp(800., 600.), CONTENT, 1.0, (0.0, 0.0));
        assert!((z - 1.0).abs() < 1e-4, "zoom moved: {z}");
        assert!(p.0.abs() < 1e-3 && p.1.abs() < 1e-3, "pan moved: {p:?}");
    }

    #[test]
    fn settle_view_only_zooms_out_to_enclose_a_big_target() {
        // A rect larger than the viewport at the current zoom forces a zoom-out,
        // never below the whole-graph fit, and ends up fully framed.
        let target = (0.0, 0.0, 2000.0, 1500.0);
        let (z, p) = settle_view(target, vp(800., 600.), CONTENT, 1.0, (0.0, 0.0));
        assert!(z < 1.0, "expected zoom-out, got {z}");
        assert!(z >= fit_zoom(vp(800., 600.), CONTENT) - 1e-4, "below whole-graph fit: {z}");
        assert_enclosed(target, 800., 600., z, p);
    }

    #[test]
    fn settle_view_never_zooms_in_on_a_small_target() {
        // A node opening smaller than the current zoom keeps the zoom — the
        // disorienting blow-up-to-fill case is gone.
        let target = (100.0, 100.0, 30.0, 20.0);
        let (z, _) = settle_view(target, vp(800., 600.), CONTENT, 0.5, (0.0, 0.0));
        assert!(z <= 0.5 + 1e-4, "zoomed in: {z}");
    }

    #[test]
    fn settle_view_pans_an_offscreen_target_into_view_without_rezoom() {
        // A small rect off to the right fits at the current zoom, so the zoom
        // holds and only the pan shifts it back into frame.
        let target = (1000.0, 100.0, 50.0, 40.0);
        let (z, p) = settle_view(target, vp(800., 600.), CONTENT, 1.0, (0.0, 0.0));
        assert!((z - 1.0).abs() < 1e-4, "zoom moved: {z}");
        assert!(p.0 < 0.0, "expected a leftward pan, got {p:?}");
        assert_enclosed(target, 800., 600., z, p);
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
