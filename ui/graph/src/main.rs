//! Loads a Bazel `aquery`, `cquery`, or `query` `--output=streamed_proto` file
//! through `bviz_graph_layout`, lays the graph out, and opens a window
//! rendering the result.

use bviz_graph_layout::graph::Graph;
use bviz_graph_layout::layout::CARET_RESERVE;
use bviz_graph_layout::{layout, quotient, streamed_proto, EdgeKind, LaidOut};
use bviz_ui_components::{load_in_background, track_drag};
use bviz_ui_shell::Chrome;
use clap::Parser;
use gpui::{
    Animation, AnimationExt, App, BorderStyle, Bounds, Context, Div, Entity, FocusHandle,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, Hsla, MouseMoveEvent, MouseUpEvent, PathBuilder,
    Pixels, Point, Render, Rgba, ScrollDelta, ScrollWheelEvent, Stateful, Task, TextAlign, Size,
    Transformation, Window, canvas, div, percentage, point, prelude::*, px, quad, rgb, size,
};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Height of the top band a container reserves for its title.
const LABEL_BAND: f64 = 26.0;

/// Box label font size in layout pixels.
const LABEL_SIZE: f64 = 13.0;

/// Padding around the graph, in layout pixels.
const MARGIN: f64 = 16.0;

/// Arrowhead size along and across the edge direction, in layout pixels.
const ARROW_LENGTH: f64 = 9.0;
const ARROW_HALF_WIDTH: f64 = 4.0;
/// How far the stroke runs past the arrowhead base into the head, closing the
/// seam without letting the line broaden the visible tip.
const ARROW_OVERDRAW: f64 = 1.0;

/// Fraction by which a kind accent is blended toward the themed background; the
/// result is the leaf fill, so kinds stay recognizable yet read on light or dark.
const KIND_BLEND: f32 = 0.68;

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

/// Accent hue for a leaf box, keyed by its node kind. These are mid-saturation
/// and recognizable against either a light or dark background.
fn kind_accent(kind: &str) -> Rgba {
    match kind {
        "action" => rgb(0xe69a3c),   // orange
        "artifact" => rgb(0x57b85a), // green
        "source" => rgb(0x9b7fd0),   // purple
        _ => rgb(0x5b8fd0),          // blue
    }
}

/// Connector color, keyed by the query an edge came from, so static (query),
/// configured (cquery), and execution-phase (aquery) dependencies read
/// distinctly. These are mid-tone hues legible on light or dark backgrounds.
fn edge_color(kind: EdgeKind) -> Rgba {
    match kind {
        EdgeKind::Query => rgb(0x4f7fbf),  // blue
        EdgeKind::Cquery => rgb(0x3fa07a), // green-teal
        EdgeKind::Aquery => rgb(0xc77b3c), // orange
    }
}

/// Fraction by which the package container fill is pulled toward the border
/// color, deepening it so the outer package box reads as containing the lighter
/// target and configured-target boxes.
const PACKAGE_DEEPEN: f32 = 0.18;

/// Fraction by which an expanded group's fill is pulled toward the file-leaf
/// accent, tinting the box so it reads as a grouping of files and stays distinct
/// from the neutral package/target containers.
const GROUP_TINT: f32 = 0.30;

/// Fill for a container box. The outer package box is deepened toward the border
/// so nesting reads package > target > configured-target; an expanded group of
/// files is tinted toward the file leaves it holds; other inner
/// containers use the plain themed container fill.
fn container_fill(kind: &str, palette: &Palette) -> Rgba {
    match kind {
        "package" => bviz_ui_theme::mix(palette.container_fill, palette.border, PACKAGE_DEEPEN),
        "group" => bviz_ui_theme::mix(palette.container_fill, kind_accent("artifact"), GROUP_TINT),
        _ => palette.container_fill,
    }
}

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

/// Loads, lays out, and renders a Bazel build graph.
///
/// Zoom, pan, and the captured viewport bounds are shared cells so the canvas
/// can both read them while painting and, on the first frame, set the initial
/// fit-to-window view (which needs the viewport size, known only at paint time).
struct GraphView {
    focus_handle: FocusHandle,
    source: Source,
    /// Zoom scale baked into every coordinate (1.0 = layout pixels).
    zoom: Rc<Cell<f32>>,
    /// Pixel translation applied after scaling.
    pan: Rc<Cell<(f32, f32)>>,
    /// Whether the initial fit-to-window view has been applied.
    fitted: Rc<Cell<bool>>,
    dragging: bool,
    /// Whether the cursor moved past the click threshold since the press, which
    /// distinguishes a drag (pan) from a click (toggle).
    moved: bool,
    last_mouse: Point<Pixels>,
    /// Cursor position at the most recent left press, the click/drag origin.
    press: Point<Pixels>,
    /// Viewport bounds captured during canvas paint, so the scroll handler can
    /// map a window cursor to viewport space and bound the zoom to a fit.
    viewport: Rc<Cell<Bounds<Pixels>>>,
    /// The built (pre-quotient) graph, cached so toggling a group re-lays-out
    /// without re-reading or re-parsing the proto. `None` until the load lands.
    base: Rc<RefCell<Option<Graph>>>,
    /// Container and group ids currently expanded; every other container folds
    /// its subtree away so the layout only processes the visible view.
    expanded: Rc<RefCell<HashSet<String>>>,
    /// Multi-child container ids of the base graph; only these carry an
    /// expand/collapse handle (a leaf can't open, a singleton auto-expands), so a
    /// click or caret can tell an expandable box from a plain one.
    expandable: Rc<RefCell<HashSet<String>>>,
    /// The most-recently toggled node id: the box its relayout frames or
    /// recenters, and the box emphasized briefly afterward so the eye can find
    /// what opened or closed. Cleared when the highlight fades.
    recent: Option<String>,
    /// When the highlight began — set as the relayout lands, so the emphasis
    /// fades over `HIGHLIGHT_LINGER` measured from when the box becomes visible.
    /// A relayout slower than the linger would otherwise outlast a fade timed
    /// from the click, surfacing the box already un-highlighted.
    recent_at: Instant,
    /// Guards the highlight-fade timer: a toggle bumps this so any in-flight fade
    /// retires at once, leaving `recent` for the new toggle rather than clearing
    /// it or repainting a stale highlight.
    highlight_seq: u64,
    /// Set after a toggle so the next relayout's first paint frames or recenters
    /// `recent`.
    recenter: Rc<Cell<bool>>,
    /// Whether the most recent toggle was an expand (vs a collapse), so the
    /// recenter frames the opened box but only keeps a closed one in view.
    recent_expand: bool,
    /// Whether a relayout is running, so the view shows a progress indicator
    /// while the prior layout stays interactive.
    relaying: bool,
    /// Node under the cursor, whose neighborhood stays lit while everything else
    /// dims; `None` when the cursor is over no node.
    hovered: Option<String>,
    /// Whether hovering a node dims the rest of the graph; flipped by the `h` key
    /// or the on-screen toggle so the dimming can be turned off when it is more
    /// noise than help.
    dim_enabled: bool,
    /// Holds the background load/relayout task so it isn't cancelled on drop.
    _load_task: Task<()>,
}

/// A cursor must travel at least this far from the press before the gesture
/// counts as a drag rather than a click.
const CLICK_SLOP: f32 = 4.0;

/// How long the just-toggled box stays emphasized before the highlight fades.
const HIGHLIGHT_LINGER: Duration = Duration::from_millis(1100);

/// Accent for the just-toggled box's emphasis border and inset glow, bright
/// enough to catch the eye on either a light or dark background.
const HIGHLIGHT_ACCENT: Rgba = Rgba { r: 1.0, g: 0.819, b: 0.4, a: 1.0 };

/// Fraction a dimmed box's fill and border are pulled toward the background, so
/// the hovered node's neighborhood stands out against the receded rest.
const DIM_FADE: f32 = 0.6;

/// Alpha a dimmed connector keeps, fading edges outside the hovered node's
/// neighborhood without erasing them.
const DIM_EDGE_ALPHA: f32 = 0.2;

impl GraphView {
    fn new(inputs: Vec<Input>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let base: Rc<RefCell<Option<Graph>>> = Rc::new(RefCell::new(None));
        // Build and lay out the graph on the background executor so window
        // appearance and interactivity are never blocked; swap the view to the
        // result and cache the base graph when it completes.
        let load_task = load_in_background(
            cx,
            move || {
                let graph = build_graph(&inputs)?;
                // Open to a bounded overview instead of laying out the whole graph.
                let expanded = bviz_graph_layout::lod::default_expansion(
                    &graph,
                    MAX_VISIBLE_NODES,
                    MAX_VISIBLE_EDGES,
                );
                let laid_out = relayout(&graph, &expanded);
                Ok::<_, String>((graph, expanded, laid_out))
            },
            |view, result, _| {
                view.source = match result {
                    Ok((graph, expanded, laid_out)) => {
                        *view.expandable.borrow_mut() =
                            bviz_graph_layout::lod::expandable_containers(&graph);
                        *view.expanded.borrow_mut() = expanded;
                        *view.base.borrow_mut() = Some(graph);
                        Source::Loaded(laid_out)
                    }
                    Err(err) => Source::Message(err),
                };
            },
        );
        Self {
            focus_handle,
            source: Source::Loading,
            zoom: Rc::new(Cell::new(1.0)),
            pan: Rc::new(Cell::new((0.0, 0.0))),
            fitted: Rc::new(Cell::new(false)),
            dragging: false,
            moved: false,
            last_mouse: Point::default(),
            press: Point::default(),
            viewport: Rc::new(Cell::new(Bounds::default())),
            base,
            expanded: Rc::new(RefCell::new(HashSet::new())),
            expandable: Rc::new(RefCell::new(HashSet::new())),
            recent: None,
            recent_at: Instant::now(),
            highlight_seq: 0,
            recenter: Rc::new(Cell::new(false)),
            recent_expand: false,
            relaying: false,
            hovered: None,
            dim_enabled: true,
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
        let expanded = self.expanded.borrow().clone();
        self.relaying = true;
        self._load_task = load_in_background(
            cx,
            move || relayout(&graph, &expanded),
            |view, laid_out, cx| {
                view.source = Source::Loaded(laid_out);
                view.recenter.set(true);
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

    /// Builds the single-canvas viewport: every box and connector is painted in
    /// one `canvas`, with pan and zoom applied in the paint transform. The
    /// closure is `'static`, so it captures owned snapshots of the boxes and
    /// connectors rather than borrowing the graph.
    fn graph(
        g: &LaidOut,
        expandable: &HashSet<String>,
        entity: Entity<GraphView>,
        zoom: Rc<Cell<f32>>,
        pan: Rc<Cell<(f32, f32)>>,
        fitted: Rc<Cell<bool>>,
        viewport: Rc<Cell<Bounds<Pixels>>>,
        recent: Option<&str>,
        recent_at: Instant,
        recenter: Rc<Cell<bool>>,
        recent_expand: bool,
        relaying: bool,
        hovered: Option<&str>,
        dim_enabled: bool,
        palette: Palette,
    ) -> Stateful<Div> {
        // Dimming traces the hovered node's whole subtree neighborhood: the boxes
        // geometrically nested inside the hovered box (it and its descendants,
        // since the layout nests children within their parent's rect) plus the far
        // endpoint of every edge incident to that subtree. The subtree's incident
        // edges stay lit and everything else dims, but only when there is at least
        // one such edge to reveal — an edgeless subtree (or a disabled toggle)
        // would fade the graph for no signal, so those suppress dimming entirely.
        let hovered_box = hovered.and_then(|h| g.boxes.iter().find(|b| b.id == h));
        let subtree: HashSet<&str> = hovered_box
            .filter(|_| dim_enabled)
            .map(|h| {
                g.boxes
                    .iter()
                    .filter(|b| {
                        b.x >= h.x
                            && b.y >= h.y
                            && b.x + b.width <= h.x + h.width
                            && b.y + b.height <= h.y + h.height
                    })
                    .map(|b| b.id.as_str())
                    .collect()
            })
            .unwrap_or_default();
        // The subtree plus the far endpoint of each edge incident to it; an empty
        // active set means no incident edges, so dimming stays off.
        let mut active: HashSet<&str> = subtree.clone();
        let mut has_edge = false;
        for c in &g.connectors {
            let (from_in, to_in) = (subtree.contains(c.from.as_str()), subtree.contains(c.to.as_str()));
            if from_in || to_in {
                has_edge = true;
                active.insert(c.from.as_str());
                active.insert(c.to.as_str());
            }
        }
        if !has_edge {
            active.clear();
        }
        let dim_active = has_edge;
        // The highlight fades linearly to nothing over the linger window.
        let highlight_alpha =
            (1.0 - recent_at.elapsed().as_secs_f32() / HIGHLIGHT_LINGER.as_secs_f32()).clamp(0.0, 1.0);
        let boxes: Vec<PaintBox> = g
            .boxes
            .iter()
            .map(|b| PaintBox {
                x: b.x,
                y: b.y,
                w: b.width,
                h: b.height,
                compound: b.compound,
                expandable: expandable.contains(&b.id) || b.id.starts_with("group:"),
                fill: if b.compound {
                    container_fill(&b.kind, &palette)
                } else {
                    bviz_ui_theme::mix(kind_accent(&b.kind), palette.background, KIND_BLEND)
                },
                label: b.label.clone(),
                highlight: if recent == Some(b.id.as_str()) { highlight_alpha } else { 0.0 },
                dim: dim_active && !active.contains(b.id.as_str()),
            })
            .collect();
        // The just-toggled box's full rect, to frame on the next paint.
        let recenter_target = recent.and_then(|id| {
            g.boxes
                .iter()
                .find(|b| b.id == id)
                .map(|b| (b.x, b.y, b.width, b.height))
        });
        let connectors: Vec<(Vec<(f64, f64)>, EdgeKind, bool)> = g
            .connectors
            .iter()
            .map(|c| {
                let active_edge =
                    subtree.contains(c.from.as_str()) || subtree.contains(c.to.as_str());
                let dim = dim_active && !active_edge;
                (c.points.clone(), c.kind, dim)
            })
            .collect();
        // Edge kinds actually drawn, in enum order, so the legend explains only
        // the provenances present in this graph.
        let present: Vec<EdgeKind> = [EdgeKind::Query, EdgeKind::Cquery, EdgeKind::Aquery]
            .into_iter()
            .filter(|k| g.connectors.iter().any(|c| c.kind == *k))
            .collect();
        let legend = (!present.is_empty()).then(|| {
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
                        .child(
                            div()
                                .text_xs()
                                .text_color(palette.label)
                                .child(k.label()),
                        )
                }))
        });
        let relaying_indicator = relaying.then(|| {
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
                .child(
                    bviz_ui_assets::icon("circle-notch")
                        .size(px(14.))
                        .text_color(palette.label)
                        .with_animation(
                            "relaying-spinner",
                            Animation::new(Duration::from_millis(900)).repeat(),
                            |icon, delta| {
                                icon.with_transformation(Transformation::rotate(percentage(delta)))
                            },
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(palette.label)
                        .child("laying out…"),
                )
        });
        // A pill that shows and toggles hover-dim, kept in sync with the `h` key
        // since both flip the same field. On reads in the file-leaf accent with a
        // filled dot; off reads muted with a hollow dot.
        let dim_toggle = {
            let entity = entity.clone();
            let accent = kind_accent("artifact");
            let (dot, label_color, border_color) = if dim_enabled {
                ('\u{25CF}', accent, accent)
            } else {
                ('\u{25CB}', palette.label, palette.border)
            };
            div()
                .id("dim-toggle")
                .absolute()
                .left_2()
                .top_2()
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(palette.container_fill)
                .border_1()
                .border_color(border_color)
                .text_xs()
                .text_color(label_color)
                .cursor_pointer()
                .child(format!("{dot} dim"))
                .on_click(move |_, _, cx| {
                    entity.update(cx, |this, cx| {
                        this.dim_enabled = !this.dim_enabled;
                        cx.notify();
                    });
                })
        };
        let content = (
            g.width as f32 + 2. * MARGIN as f32,
            g.height as f32 + 2. * MARGIN as f32,
        );
        // Capture the viewport bounds and fit the graph to them. The fit needs
        // the viewport size, known only once laid out, so it happens here.
        let prepaint = {
            let (zoom, pan, fitted, viewport, recenter) = (
                zoom.clone(),
                pan.clone(),
                fitted.clone(),
                viewport.clone(),
                recenter.clone(),
            );
            move |bounds: Bounds<Pixels>, _: &mut Window, _: &mut App| {
                let prev = viewport.get();
                viewport.set(bounds);
                if !fitted.get() {
                    let (z, p) = fit_view(bounds.size, content);
                    zoom.set(z);
                    pan.set(p);
                    fitted.set(true);
                } else if prev.size != bounds.size {
                    // Keep the view stable across a resize at any zoom level: hold
                    // the graph point under the viewport center fixed and scale the
                    // zoom with the fit, so the same section stays framed at the
                    // same zoom relative to the window rather than snapping to the
                    // fully fitted view.
                    let old_fit = fit_zoom(prev.size, content);
                    let new_fit = fit_zoom(bounds.size, content);
                    let z_old = zoom.get();
                    let (pan_x, pan_y) = pan.get();
                    let center_x = (f32::from(prev.size.width) / 2. - pan_x) / z_old;
                    let center_y = (f32::from(prev.size.height) / 2. - pan_y) / z_old;
                    let z = (z_old * new_fit / old_fit).clamp(new_fit, MAX_ZOOM);
                    zoom.set(z);
                    pan.set((
                        f32::from(bounds.size.width) / 2. - center_x * z,
                        f32::from(bounds.size.height) / 2. - center_y * z,
                    ));
                }
                // Take the view to the just-toggled box on the next paint. An
                // expand frames the box — zoom to fit it with margin (out for a
                // big box, in for a small one, never below the whole-graph fit nor
                // above MAX_ZOOM) and center it — so the user lands on what they
                // opened. A collapse only keeps it in view: pan its center on
                // screen if the relayout pushed it off, else leave the view put.
                if recenter.get() {
                    if let Some((bx, by, bw, bh)) = recenter_target {
                        let vw = f32::from(bounds.size.width);
                        let vh = f32::from(bounds.size.height);
                        let (bw, bh) = (bw as f32, bh as f32);
                        let cx = bx as f32 + bw / 2.;
                        let cy = by as f32 + bh / 2.;
                        if recent_expand {
                            let z = ((vw / bw).min(vh / bh) * FIT_MARGIN)
                                .clamp(fit_zoom(bounds.size, content), MAX_ZOOM);
                            zoom.set(z);
                            pan.set((
                                vw / 2. - (cx + MARGIN as f32) * z,
                                vh / 2. - (cy + MARGIN as f32) * z,
                            ));
                        } else {
                            let z = zoom.get();
                            let (panx, pany) = pan.get();
                            let sx = (cx + MARGIN as f32) * z + panx;
                            let sy = (cy + MARGIN as f32) * z + pany;
                            let inside = sx >= 0. && sx <= vw && sy >= 0. && sy <= vh;
                            if !inside {
                                pan.set((
                                    vw / 2. - (cx + MARGIN as f32) * z,
                                    vh / 2. - (cy + MARGIN as f32) * z,
                                ));
                            }
                        }
                    }
                    recenter.set(false);
                }
            }
        };
        div()
            .id("graph-viewport")
            .relative()
            .size_full()
            .overflow_hidden()
            .child(
                canvas(prepaint, move |bounds, _, window, cx| {
                    let z = zoom.get();
                    let p = pan.get();
                    // Boxes parent-before-child so children paint over their
                    // containers; connectors last so edges sit over the fills.
                    for b in &boxes {
                        paint_box(b, bounds, z, p, &palette, window, cx);
                    }
                    for (points, kind, dim) in &connectors {
                        paint_connector(points, *kind, *dim, bounds, z, p, window);
                    }
                    // While a drag is in progress, pan through window-global
                    // handlers so it keeps working when the cursor leaves the
                    // window; a release anywhere ends the drag and, if it was a
                    // click rather than a drag, toggles the group under it.
                    if entity.read(cx).dragging {
                        track_drag(window, &entity, drag_pan, end_drag);
                    }
                })
                .absolute()
                .left_0()
                .top_0()
                .size_full(),
            )
            .children(legend)
            .children(relaying_indicator)
            .child(dim_toggle)
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
                        self.zoom.clone(),
                        self.pan.clone(),
                        self.fitted.clone(),
                        self.viewport.clone(),
                        self.recent.as_deref(),
                        self.recent_at,
                        self.recenter.clone(),
                        self.recent_expand,
                        self.relaying,
                        self.hovered.as_deref(),
                        self.dim_enabled,
                        palette,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, e: &MouseDownEvent, _, cx| {
                            // Start a drag; the window-global handlers that follow
                            // and end it are installed from the canvas paint while
                            // `dragging` is set, so request a repaint now.
                            this.dragging = true;
                            this.moved = false;
                            this.press = e.position;
                            this.last_mouse = e.position;
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                        // A drag pans rather than hovers; leave the lit set alone.
                        if this.dragging {
                            return;
                        }
                        let Source::Loaded(g) = &this.source else {
                            return;
                        };
                        let vp = this.viewport.get();
                        let (gx, gy) =
                            from_screen(e.position, vp.origin, this.zoom.get(), this.pan.get());
                        let hit = hit_box(&g.boxes, gx, gy).map(|b| b.id.clone());
                        if hit != this.hovered {
                            this.hovered = hit;
                            cx.notify();
                        }
                    }))
                    .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, _, cx| {
                        // Anchor the zoom at the cursor: keep the content point
                        // under the cursor fixed as the scale changes. The minimum
                        // zoom is the fit, so the graph can't be zoomed out of view.
                        let content = match &this.source {
                            Source::Loaded(g) => (
                                g.width as f32 + 2. * MARGIN as f32,
                                g.height as f32 + 2. * MARGIN as f32,
                            ),
                            _ => return,
                        };
                        let vp = this.viewport.get();
                        let lx = f32::from(e.position.x) - f32::from(vp.origin.x);
                        let ly = f32::from(e.position.y) - f32::from(vp.origin.y);
                        let dy = match e.delta {
                            ScrollDelta::Lines(p) => p.y,
                            ScrollDelta::Pixels(p) => f32::from(p.y) / 120.0,
                        };
                        let old = this.zoom.get();
                        let new = (old * 1.1_f32.powf(dy))
                            .clamp(fit_zoom(vp.size, content), MAX_ZOOM);
                        let (px, py) = this.pan.get();
                        let content_x = (lx - px) / old;
                        let content_y = (ly - py) / old;
                        this.pan.set((lx - content_x * new, ly - content_y * new));
                        this.zoom.set(new);
                        cx.notify();
                    }))
                    // Track the shared focus handle here too (the chrome already
                    // tracks it on an ancestor) so this deepest focused element
                    // receives key events; `h` toggles hover-dim. Events still
                    // bubble up to the chrome's own key bindings.
                    .track_focus(&self.focus_handle)
                    .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                        if e.keystroke.key == "h" {
                            this.dim_enabled = !this.dim_enabled;
                            cx.notify();
                        }
                    })),
                ),
            Source::Loading => div()
                .flex_1()
                .flex()
                .flex_col()
                .justify_center()
                .items_center()
                .gap_3()
                .child(
                    // `with_animation` re-renders each frame, spinning the icon.
                    bviz_ui_assets::icon("circle-notch")
                        .size(px(36.))
                        .text_color(colors.text)
                        .with_animation(
                            "spinner",
                            Animation::new(Duration::from_millis(900)).repeat(),
                            |icon, delta| {
                                icon.with_transformation(Transformation::rotate(percentage(delta)))
                            },
                        ),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(colors.disabled)
                        .child("Loading…"),
                ),
            Source::Message(msg) => div()
                .flex_1()
                .flex()
                .flex_col()
                .justify_center()
                .items_center()
                .gap_4()
                .p_4()
                .child(
                    div()
                        .max_w(px(400.))
                        .text_sm()
                        .text_color(colors.disabled)
                        .child(msg.clone()),
                ),
        };
        Chrome::new(&self.focus_handle)
            .title("bazel_viz graph")
            .status(self.status())
            .child(body)
    }
}

/// The largest zoom at which `content` (its full width×height in layout pixels)
/// still fits `viewport`, capped at 1:1 so a small graph is never blown up. This
/// is both the initial zoom and the minimum the scroll handler allows, so the
/// whole graph always fits.
fn fit_zoom(viewport: Size<Pixels>, content: (f32, f32)) -> f32 {
    let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
    let (gw, gh) = content;
    if gw <= 0. || gh <= 0. || vw <= 1. || vh <= 1. {
        return 1.0;
    }
    ((vw / gw).min(vh / gh) * FIT_MARGIN).min(1.0)
}

/// The zoom and pan that fit `content` centered in `viewport`.
fn fit_view(viewport: Size<Pixels>, content: (f32, f32)) -> (f32, (f32, f32)) {
    let zoom = fit_zoom(viewport, content);
    let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
    let (gw, gh) = content;
    (zoom, ((vw - gw * zoom) / 2., (vh - gh * zoom) / 2.))
}

/// Maps a layout-space point to window space with pan and zoom baked in, atop
/// the same margin offset the boxes use and the canvas's origin within the
/// window.
fn to_screen(x: f64, y: f64, bounds: Bounds<Pixels>, zoom: f32, pan: (f32, f32)) -> Point<Pixels> {
    point(
        bounds.origin.x + px((x as f32 + MARGIN as f32) * zoom + pan.0),
        bounds.origin.y + px((y as f32 + MARGIN as f32) * zoom + pan.1),
    )
}

/// Paints a box's quad and its centered label. A container's label sits in its
/// top band; a leaf's label is centered in the whole box. Off-screen boxes are
/// culled.
fn paint_box(
    b: &PaintBox,
    bounds: Bounds<Pixels>,
    zoom: f32,
    pan: (f32, f32),
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let origin = to_screen(b.x, b.y, bounds, zoom, pan);
    let size = size(px(b.w as f32 * zoom), px(b.h as f32 * zoom));
    let rect = Bounds::new(origin, size);
    if !rect.intersects(&bounds) {
        return;
    }
    let corner = px(6. * zoom);
    let fill = if b.dim {
        bviz_ui_theme::mix(b.fill, palette.background, DIM_FADE)
    } else {
        b.fill
    };
    let label_color = if b.dim {
        bviz_ui_theme::mix(palette.label, palette.background, DIM_FADE)
    } else {
        palette.label
    };
    let (border_w, border_c) = if b.highlight > 0.0 {
        // Fade from the themed border to the amber accent, and from a normal to a
        // thick stroke, by the highlight alpha.
        let normal = px(1.);
        let thick = px((2. * zoom).max(2.));
        let mut accent = HIGHLIGHT_ACCENT;
        accent.a = b.highlight;
        (
            normal + (thick - normal) * b.highlight,
            Hsla::from(accent),
        )
    } else if b.dim {
        (px(1.), Hsla::from(bviz_ui_theme::mix(palette.border, palette.background, DIM_FADE)))
    } else {
        (px(1.), Hsla::from(palette.border))
    };
    window.paint_quad(quad(rect, corner, fill, border_w, border_c, BorderStyle::Solid));
    if b.highlight > 0.0 {
        // A faint accent wash over the fill so the emphasis reads as a glow, not
        // just a brighter outline.
        let mut wash = HIGHLIGHT_ACCENT;
        wash.a = 0.15 * b.highlight;
        window.paint_quad(quad(
            rect,
            corner,
            wash,
            px(0.),
            Hsla::transparent_black(),
            BorderStyle::Solid,
        ));
    }

    // The label font comes from the window's inherited UI text style (the same
    // one the chrome and the other views use), so the views stay visually
    // coherent; a container's title is bold. The size is the layout's own, kept
    // in step with the width the layout reserved for the label.
    let base = bviz_ui_assets::text_metrics(window).font;
    let font = if b.compound { base.bold() } else { base };
    let fs = px(LABEL_SIZE as f32 * zoom);
    let line = bviz_ui_assets::shape_line(window, b.label.clone(), font, fs, label_color.into());
    // A collapsed group leaf carries a disclosure caret left of the label,
    // occupying the reserved width the layout sized the box to hold; the label
    // and caret are centered together as one unit. Other boxes center the label
    // alone.
    let caret = b.expandable;
    let footprint = px(CARET_RESERVE as f32 * zoom);
    let unit_width = if caret {
        footprint + line.width()
    } else {
        line.width()
    };
    let unit_left = rect.origin.x + (rect.size.width - unit_width) / 2.;
    let tx = if caret {
        unit_left + footprint
    } else {
        unit_left
    };
    let ty = if b.compound {
        let band = px(LABEL_BAND as f32 * zoom);
        rect.origin.y + (band - fs) / 2.
    } else {
        rect.origin.y + (rect.size.height - fs) / 2.
    };
    let _ = line.paint(point(tx, ty), fs, TextAlign::Left, None, window, cx);

    // An open container (compound) points the caret down; a collapsed box points
    // it right. Phosphor icons render as `svg()` elements and cannot be painted
    // inside this canvas, so the caret is a small filled triangle.
    if caret {
        let cy = ty + fs / 2.;
        paint_caret(
            point(unit_left, cy),
            footprint,
            fs,
            b.compound,
            label_color,
            window,
        );
    }
}

/// Paints a small filled-triangle disclosure caret inside the reserved
/// `footprint` whose left edge is `at.x`, centered vertically at `at.y`. The
/// triangle width fills the footprint less a trailing gap before the label; its
/// height scales with the font size `fs`. `down` points it downward (expanded),
/// otherwise rightward (collapsed).
fn paint_caret(
    at: Point<Pixels>,
    footprint: Pixels,
    fs: Pixels,
    down: bool,
    color: Rgba,
    window: &mut Window,
) {
    let s = f32::from(footprint) * 0.6; // triangle width within the footprint
    let height = f32::from(fs) * 0.5; // triangle height, scaling with the label
    let left = at.x;
    let right = left + px(s);
    let (top, bottom) = (at.y - px(height / 2.), at.y + px(height / 2.));
    let mut tri = PathBuilder::fill();
    if down {
        // Apex down, base along the top.
        tri.add_polygon(
            &[
                point(left, top),
                point(right, top),
                point(left + px(s / 2.), bottom),
            ],
            true,
        );
    } else {
        // Apex right, base along the left.
        tri.add_polygon(
            &[point(left, top), point(right, at.y), point(left, bottom)],
            true,
        );
    }
    if let Ok(path) = tri.build() {
        window.paint_path(path, color);
    }
}

/// Strokes a connector polyline and caps its end with a filled arrowhead
/// pointing along the final segment.
fn paint_connector(
    points: &[(f64, f64)],
    kind: EdgeKind,
    dim: bool,
    bounds: Bounds<Pixels>,
    zoom: f32,
    pan: (f32, f32),
    window: &mut Window,
) {
    if points.len() < 2 {
        return;
    }
    let mut color = edge_color(kind);
    if dim {
        color.a = DIM_EDGE_ALPHA;
    }
    let pts: Vec<Point<Pixels>> = points
        .iter()
        .map(|(x, y)| to_screen(*x, *y, bounds, zoom, pan))
        .collect();

    // Stop the stroke at the arrowhead's base (less a slight overdraw) so the
    // line doesn't paint through the head and broaden the visible tip.
    let n = pts.len();
    let tip = pts[n - 1];
    let prev = pts[n - 2];
    let dx = f32::from(tip.x - prev.x);
    let dy = f32::from(tip.y - prev.y);
    let seg = (dx * dx + dy * dy).sqrt().max(1e-3);
    let (ux, uy) = (dx / seg, dy / seg);
    let trim = ((ARROW_LENGTH as f32 - ARROW_OVERDRAW as f32) * zoom).min(seg);
    let line_end = point(tip.x - px(ux * trim), tip.y - px(uy * trim));

    let mut line = PathBuilder::stroke(px(1.5 * zoom));
    line.move_to(pts[0]);
    for p in &pts[1..n - 1] {
        line.line_to(*p);
    }
    line.line_to(line_end);
    if let Ok(path) = line.build() {
        window.paint_path(path, color);
    }

    paint_arrowhead(&pts, zoom, color, window);
}

/// Paints a filled triangle at the polyline's last point, aimed along the
/// direction of its final segment.
fn paint_arrowhead(pts: &[Point<Pixels>], zoom: f32, color: Rgba, window: &mut Window) {
    let length = ARROW_LENGTH as f32 * zoom;
    let half_width = ARROW_HALF_WIDTH as f32 * zoom;

    let n = pts.len();
    let tip = pts[n - 1];
    let dx = f32::from(tip.x - pts[n - 2].x);
    let dy = f32::from(tip.y - pts[n - 2].y);
    let len = (dx * dx + dy * dy).sqrt().max(1e-3);
    let (ux, uy) = (dx / len, dy / len); // forward unit vector
    let (perp_x, perp_y) = (-uy, ux); // left-perpendicular unit vector

    let base = point(tip.x - px(ux * length), tip.y - px(uy * length));
    let left = point(base.x + px(perp_x * half_width), base.y + px(perp_y * half_width));
    let right = point(base.x - px(perp_x * half_width), base.y - px(perp_y * half_width));

    let mut head = PathBuilder::fill();
    head.add_polygon(&[tip, left, right], true);
    if let Ok(path) = head.build() {
        window.paint_path(path, color);
    }
}

/// Reads every input's streamed_proto file and merges them into one joint graph,
/// before any group quotienting. Cached so toggling a group needn't re-read or
/// re-parse.
fn build_graph(inputs: &[Input]) -> Result<Graph, String> {
    let mut aq = None;
    let mut cq = None;
    let mut q = None;
    for input in inputs {
        let path = input.path.display();
        let bytes = std::fs::read(&input.path).map_err(|e| format!("{path}: {e}"))?;
        match input.kind {
            InputKind::Aquery => {
                aq = Some(
                    streamed_proto::read_action_graph_containers(&bytes)
                        .map_err(|e| format!("{path}: {e}"))?,
                );
            }
            InputKind::Cquery => {
                cq = Some(
                    streamed_proto::read_cquery_results(&bytes)
                        .map_err(|e| format!("{path}: {e}"))?,
                );
            }
            InputKind::Query => {
                q = Some(
                    streamed_proto::read_query_targets(&bytes)
                        .map_err(|e| format!("{path}: {e}"))?,
                );
            }
        }
    }
    Ok(bviz_graph_layout::build_joint(
        aq.as_deref(),
        cq.as_deref(),
        q.as_deref(),
        false,
    ))
}

/// Builds the display graph with the given ids expanded and lays it out: fold
/// every unexpanded container away, merge interchangeable leaves, then lay out
/// only the resulting visible view.
fn relayout(graph: &Graph, expanded: &HashSet<String>) -> LaidOut {
    let visible = bviz_graph_layout::lod::collapse(graph, expanded);
    layout::layout(&quotient::quotient(&visible, expanded))
}

/// Inverse of [`to_screen`]: maps a window-space point back to layout space.
fn from_screen(
    screen: Point<Pixels>,
    origin: Point<Pixels>,
    zoom: f32,
    pan: (f32, f32),
) -> (f32, f32) {
    (
        (f32::from(screen.x) - f32::from(origin.x) - pan.0) / zoom - MARGIN as f32,
        (f32::from(screen.y) - f32::from(origin.y) - pan.1) / zoom - MARGIN as f32,
    )
}

/// Pan the graph by the cursor delta during a drag, latching `moved` once the
/// cursor has travelled past the click threshold.
fn drag_pan(view: &mut GraphView, event: &MouseMoveEvent, cx: &mut Context<GraphView>) {
    let traveled = (f32::from(event.position.x) - f32::from(view.press.x))
        .hypot(f32::from(event.position.y) - f32::from(view.press.y));
    if traveled > CLICK_SLOP {
        view.moved = true;
    }
    let (px, py) = view.pan.get();
    let dx = f32::from(event.position.x) - f32::from(view.last_mouse.x);
    let dy = f32::from(event.position.y) - f32::from(view.last_mouse.y);
    view.pan.set((px + dx, py + dy));
    view.last_mouse = event.position;
    cx.notify();
}

/// End a drag. A release that never travelled is a click: open or close the
/// container (or group) box under the cursor.
fn end_drag(view: &mut GraphView, event: &MouseUpEvent, cx: &mut Context<GraphView>) {
    view.dragging = false;
    if view.moved {
        return;
    }
    let Source::Loaded(g) = &view.source else {
        return;
    };
    let vp = view.viewport.get();
    let (gx, gy) = from_screen(event.position, vp.origin, view.zoom.get(), view.pan.get());
    let Some(hit) = hit_box(&g.boxes, gx, gy) else {
        return;
    };
    // A multi-child container or a collapsed group can be opened/closed; a leaf
    // or singleton can't.
    if !hit.id.starts_with("group:") && !view.expandable.borrow().contains(&hit.id) {
        return;
    }
    let id = hit.id.clone();
    let mut expanded = view.expanded.borrow_mut();
    let removed = expanded.remove(&id);
    if !removed {
        expanded.insert(id.clone());
    }
    drop(expanded);
    view.recent_expand = !removed;
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

/// Which Bazel query produced a `--output=streamed_proto` file, selecting the
/// reader and builder used to turn it into a graph.
#[derive(Clone, Copy)]
enum InputKind {
    Aquery,
    Cquery,
    Query,
}

/// A graph input: a streamed_proto file and the query kind that produced it.
#[derive(Clone)]
struct Input {
    kind: InputKind,
    path: PathBuf,
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

fn main() {
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

    fn lbox(id: &str, x: f64, y: f64, w: f64, h: f64) -> Box {
        Box {
            id: id.into(),
            label: id.into(),
            kind: "source".into(),
            x,
            y,
            width: w,
            height: h,
            compound: false,
        }
    }

    #[test]
    fn from_screen_inverts_to_screen() {
        let bounds = Bounds::new(point(px(10.), px(20.)), size(px(800.), px(600.)));
        let zoom = 1.7_f32;
        let pan = (33.0_f32, -12.0_f32);
        for &(x, y) in &[(0.0_f64, 0.0_f64), (42.5, 7.25), (123.0, 456.0)] {
            let screen = to_screen(x, y, bounds, zoom, pan);
            let (gx, gy) = from_screen(screen, bounds.origin, zoom, pan);
            assert!((gx as f64 - x).abs() < 1e-3, "x: {gx} vs {x}");
            assert!((gy as f64 - y).abs() < 1e-3, "y: {gy} vs {y}");
        }
    }

    #[test]
    fn hit_box_picks_innermost_and_none_outside() {
        // An outer container with a small inner box nested inside it.
        let boxes = vec![
            lbox("group:source1", 0.0, 0.0, 100.0, 100.0),
            lbox("source1", 20.0, 20.0, 30.0, 30.0),
        ];
        // Inside both rects: the smaller (inner) wins.
        assert_eq!(hit_box(&boxes, 30.0, 30.0).unwrap().id, "source1");
        // Inside only the container.
        assert_eq!(hit_box(&boxes, 5.0, 5.0).unwrap().id, "group:source1");
        // Outside everything.
        assert!(hit_box(&boxes, 200.0, 200.0).is_none());
        assert!(hit_box(&boxes, -1.0, 50.0).is_none());
    }
}
