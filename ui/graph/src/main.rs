//! Loads a Bazel `aquery`, `cquery`, or `query` `--output=streamed_proto` file
//! through `bviz_graph_layout`, lays the graph out, and opens a window
//! rendering the result.

use bviz_graph_layout::graph::Graph;
use bviz_graph_layout::layout::CARET_RESERVE;
use bviz_graph_layout::{layout, quotient, streamed_proto, EdgeKind, Id, Kind, LaidOut, Names};
use bviz_ui_components::{load_in_background, track_drag, Axis, Scrollbar};
use bviz_ui_shell::Chrome;
use clap::Parser;
use gpui::{
    Animation, AnimationExt, App, BorderStyle, Bounds, Context, Div, Entity, FocusHandle,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, Hsla, MouseMoveEvent, MouseUpEvent, PathBuilder,
    Pixels, Point, Render, Rgba, ScrollDelta, ScrollWheelEvent, Stateful, Task, TextAlign, Size,
    Transformation, Window, canvas, div, percentage, point, prelude::*, px, quad, rgb, size,
};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
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
/// Perpendicular gap (pre-zoom px) between the parallel provenance lines drawn
/// along a node pair that several queries connect.
const PARALLEL_SPACING: f32 = 3.0;

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

/// Breathing room held between the reframed content and the viewport edge when a
/// relayout settles the view, so a kept box never sits flush against the border.
const RECENTER_PAD: f32 = 24.0;

/// Thickness of a scroll-bar track and thumb, in px.
const SCROLLBAR_THICKNESS: f32 = 10.0;

/// Smallest a scroll thumb may shrink to, so it stays grabbable on a long track.
const MIN_THUMB: f32 = 28.0;

/// How long the view eases to its settled position after a toggle: short enough
/// to stay responsive, long enough that the eye can follow the shift instead of
/// the view jumping there in a single frame.
const SETTLE_DURATION: Duration = Duration::from_millis(220);

/// Below these per-axis deltas a settle is a no-op and is not animated, so a
/// toggle that leaves the view already framed does not twitch.
const SETTLE_MIN_ZOOM_STEP: f32 = 0.002;
const SETTLE_MIN_PAN_STEP: f32 = 0.5;

/// An in-flight ease of the view from one zoom/pan to another, so a toggle's
/// settle shifts over a few frames instead of jumping. Driven from the canvas
/// prepaint, which re-requests a frame until the ease lands.
#[derive(Clone, Copy)]
struct Settle {
    from_zoom: f32,
    from_pan: (f32, f32),
    to_zoom: f32,
    to_pan: (f32, f32),
    start: Instant,
}

/// An in-progress scroll-bar drag: which bar holds it and the grab offset down the
/// thumb at the press, so the thumb tracks the cursor from the point it was grabbed.
#[derive(Clone, Copy)]
struct ScrollDrag {
    axis: Axis,
    grab: f32,
}

/// Accent hue for a leaf box, keyed by its node kind. These are mid-saturation
/// and recognizable against either a light or dark background.
fn kind_accent(kind: Kind) -> Rgba {
    match kind {
        Kind::Action => rgb(0xe69a3c),   // orange
        Kind::Artifact => rgb(0x57b85a), // green
        Kind::Source => rgb(0x9b7fd0),   // purple
        _ => rgb(0x5b8fd0),              // blue
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
fn container_fill(kind: Kind, palette: &Palette) -> Rgba {
    match kind {
        Kind::Package => bviz_ui_theme::mix(palette.container_fill, palette.border, PACKAGE_DEEPEN),
        Kind::Group => {
            bviz_ui_theme::mix(palette.container_fill, kind_accent(Kind::Artifact), GROUP_TINT)
        }
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
    /// The scroll-bar drag in progress, or `None` when no thumb is held.
    scroll_drag: Option<ScrollDrag>,
    /// Viewport bounds captured during canvas paint, so the scroll handler can
    /// map a window cursor to viewport space and bound the zoom to a fit.
    viewport: Rc<Cell<Bounds<Pixels>>>,
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
    /// Set after a toggle so the next relayout's first paint settles the view on
    /// `recent` together with the boxes that were on screen when it was toggled.
    recenter: Rc<Cell<bool>>,
    /// Ids of the boxes the viewport fully enclosed when the recent toggle fired.
    /// The settling relayout keeps these — and the toggled box — in view, so the
    /// view tracks what the user was looking at rather than snapping to the
    /// toggled box alone.
    prev_visible: Vec<Id>,
    /// The in-flight ease toward the settled view, or `None` when the view is at
    /// rest. The canvas paint advances it; a manual pan or zoom clears it so the
    /// animation never fights the cursor.
    anim: Rc<Cell<Option<Settle>>>,
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
            zoom: Rc::new(Cell::new(1.0)),
            pan: Rc::new(Cell::new((0.0, 0.0))),
            fitted: Rc::new(Cell::new(false)),
            dragging: false,
            moved: false,
            last_mouse: Point::default(),
            press: Point::default(),
            scroll_drag: None,
            viewport: Rc::new(Cell::new(Bounds::default())),
            base,
            reduced_base,
            names,
            expanded: Rc::new(RefCell::new(HashSet::new())),
            expandable: Rc::new(RefCell::new(HashSet::new())),
            recent: None,
            recent_at: Instant::now(),
            highlight_seq: 0,
            recenter: Rc::new(Cell::new(false)),
            prev_visible: Vec::new(),
            anim: Rc::new(Cell::new(None)),
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
        expandable: &HashSet<Id>,
        entity: Entity<GraphView>,
        zoom: Rc<Cell<f32>>,
        pan: Rc<Cell<(f32, f32)>>,
        fitted: Rc<Cell<bool>>,
        viewport: Rc<Cell<Bounds<Pixels>>>,
        recent: Option<Id>,
        recent_at: Instant,
        recenter: Rc<Cell<bool>>,
        anim: Rc<Cell<Option<Settle>>>,
        prev_visible: &[Id],
        relaying: bool,
        hovered: Option<Id>,
        dim_enabled: bool,
        reduced: bool,
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
        let subtree: HashSet<Id> = hovered_box
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
                    .map(|b| b.id)
                    .collect()
            })
            .unwrap_or_default();
        // The subtree plus the far endpoint of each full-connectivity edge incident
        // to it: the direct neighbors, the only nodes a hover lights. This walks the
        // full connectivity, not the drawn connectors, so a neighbor still lights up
        // when reduction dropped the edge to it. The far endpoints are split by
        // direction so the path tracing below follows dependency edges the right way.
        // An empty active set (no incident edge) leaves dimming off.
        let mut active: HashSet<Id> = subtree.clone();
        let mut out_targets: HashSet<Id> = HashSet::new();
        let mut in_targets: HashSet<Id> = HashSet::new();
        let mut has_edge = false;
        for &(from, to) in &g.full_edges {
            let (f_in, t_in) = (subtree.contains(&from), subtree.contains(&to));
            if f_in || t_in {
                has_edge = true;
            }
            if f_in && !t_in {
                active.insert(to);
                out_targets.insert(to);
            } else if t_in && !f_in {
                active.insert(from);
                in_targets.insert(from);
            }
        }
        if !has_edge {
            active.clear();
        }
        let dim_active = has_edge;
        // Edges to light. Every drawn edge incident to the subtree is a surviving
        // direct-neighbor edge — the drawn graph is a subset of the full
        // connectivity — so light them all; a compound box thus lights every
        // member's neighbor edge at once, not one at a time. A neighbor whose direct
        // edge reduction dropped is reached by no such edge; for it alone, trace the
        // shortest drawn path so it is not left floating. A path's interior nodes are
        // conduits, not neighbors, so they stay out of `active` (the undimmed boxes
        // remain just the direct neighbors).
        let mut lit_edges: HashSet<(Id, Id)> = HashSet::new();
        if has_edge {
            let mut out_adj: HashMap<Id, Vec<Id>> = HashMap::new();
            let mut in_adj: HashMap<Id, Vec<Id>> = HashMap::new();
            for c in &g.connectors {
                out_adj.entry(c.from).or_default().push(c.to);
                in_adj.entry(c.to).or_default().push(c.from);
                if subtree.contains(&c.from) || subtree.contains(&c.to) {
                    lit_edges.insert((c.from, c.to));
                }
            }
            // Neighbors a drawn edge already reaches need no path.
            let reached: HashSet<Id> = lit_edges
                .iter()
                .flat_map(|&(f, t)| [f, t])
                .filter(|n| !subtree.contains(n))
                .collect();
            let out_missing: HashSet<Id> = out_targets.difference(&reached).copied().collect();
            let in_missing: HashSet<Id> = in_targets.difference(&reached).copied().collect();
            trace_paths(&subtree, &out_missing, &out_adj, true, &mut lit_edges);
            trace_paths(&subtree, &in_missing, &in_adj, false, &mut lit_edges);
        }
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
                expandable: expandable.contains(&b.id) || b.group,
                fill: if b.compound {
                    container_fill(b.kind, &palette)
                } else {
                    bviz_ui_theme::mix(kind_accent(b.kind), palette.background, KIND_BLEND)
                },
                label: b.label.clone(),
                highlight: if recent == Some(b.id) { highlight_alpha } else { 0.0 },
                dim: dim_active && !active.contains(&b.id),
            })
            .collect();
        // The rect to settle the view on once the relayout lands: the smallest
        // layout rect covering the just-toggled box and every box that was fully
        // on screen when it was toggled (those that survived the relayout), so
        // the user keeps sight of what they were looking at. As (x, y, w, h).
        let recenter_target = recent.and_then(|id| {
            let mut rect: Option<(f64, f64, f64, f64)> = None;
            for &bid in std::iter::once(&id).chain(prev_visible) {
                let Some(b) = g.boxes.iter().find(|b| b.id == bid) else {
                    continue;
                };
                let (x0, y0, x1, y1) = (b.x, b.y, b.x + b.width, b.y + b.height);
                rect = Some(match rect {
                    None => (x0, y0, x1, y1),
                    Some((ax0, ay0, ax1, ay1)) => {
                        (ax0.min(x0), ay0.min(y0), ax1.max(x1), ay1.max(y1))
                    }
                });
            }
            rect.map(|(x0, y0, x1, y1)| (x0, y0, x1 - x0, y1 - y0))
        });
        let connectors: Vec<(Vec<(f64, f64)>, Vec<EdgeKind>, bool)> = g
            .connectors
            .iter()
            .map(|c| {
                let dim = dim_active && !lit_edges.contains(&(c.from, c.to));
                (c.points.clone(), c.kinds.clone(), dim)
            })
            .collect();
        // Edge kinds actually drawn, in enum order, so the legend explains only
        // the provenances present in this graph.
        let present: Vec<EdgeKind> = [EdgeKind::Query, EdgeKind::Cquery, EdgeKind::Aquery]
            .into_iter()
            .filter(|k| g.connectors.iter().any(|c| c.kinds.contains(k)))
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
            let accent = kind_accent(Kind::Artifact);
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
        // Sits below the dim pill; flipping it relays out the view with or without
        // transitive reduction. On reads in the file-leaf accent, off muted.
        let reduce_toggle = {
            let entity = entity.clone();
            let accent = kind_accent(Kind::Artifact);
            let (dot, label_color, border_color) = if reduced {
                ('\u{25CF}', accent, accent)
            } else {
                ('\u{25CB}', palette.label, palette.border)
            };
            div()
                .id("reduce-toggle")
                .absolute()
                .left_2()
                .top(px(40.))
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
                .child(format!("{dot} reduce"))
                .on_click(move |_, _, cx| {
                    entity.update(cx, |this, cx| {
                        this.reduced = !this.reduced;
                        this.retoggle(cx);
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
            let (zoom, pan, fitted, viewport, recenter, anim) = (
                zoom.clone(),
                pan.clone(),
                fitted.clone(),
                viewport.clone(),
                recenter.clone(),
                anim.clone(),
            );
            move |bounds: Bounds<Pixels>, window: &mut Window, _: &mut App| {
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
                    // fully fitted view. A settle in flight targeted the old size,
                    // so drop it rather than ease toward a stale destination.
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
                // Settle the view on the toggled box and the boxes that shared
                // the screen with it. Fit that rect, but only ever zoom out: a
                // node opening smaller than the current zoom must not blow up to
                // fill the view, the disorienting case this avoids. Hold the
                // rect's center fixed across the zoom change so the content does
                // not lurch, then pan the minimum needed to seat the rect fully
                // inside — leaving the view put when it already fits. The move is
                // eased over a few frames (the driver below) rather than jumped.
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
                // Advance an in-flight settle and ask for the next frame until it
                // lands, then snap to the target and stop, so the window returns to
                // idle. request_animation_frame redraws even with no other change.
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
                    for (points, kinds, dim) in &connectors {
                        // ELK routes a node pair once; fan its provenances out into
                        // nested lanes centered on that route — each anchored at the
                        // nodes like a lone edge — so a pair several queries connect
                        // still shows each.
                        let screen: Vec<(f32, f32)> = points
                            .iter()
                            .map(|(x, y)| {
                                let s = to_screen(*x, *y, bounds, z, p);
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
                    // Scroll bars along the edges of whichever axes overflow the
                    // viewport, an alternative to drag-panning.
                    let track = Hsla::from(bviz_ui_theme::mix(palette.border, palette.background, 0.5));
                    let thumb = Hsla::from(bviz_ui_theme::mix(palette.label, palette.background, 0.45));
                    for axis in [Axis::Vertical, Axis::Horizontal] {
                        if let Some(bar) = scrollbar_geom(axis, bounds, content, z, p) {
                            bar.paint(track, thumb, window);
                        }
                    }
                    // While a pan or a scroll-bar drag is in progress, run it
                    // through window-global handlers so it keeps working when the
                    // cursor leaves the window. A pan release also toggles the group
                    // under it when it was a click rather than a drag.
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
                })
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
                        self.recent,
                        self.recent_at,
                        self.recenter.clone(),
                        self.anim.clone(),
                        &self.prev_visible,
                        self.relaying,
                        self.hovered,
                        self.dim_enabled,
                        self.reduced,
                        palette,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, e: &MouseDownEvent, _, cx| {
                            // A press on a scroll-bar gutter drives the thumb instead
                            // of panning the content under it.
                            if let Some(axis) = scrollbar_hit(this, e.position) {
                                start_scroll_drag(this, axis, e.position, cx);
                                return;
                            }
                            // Start a pan drag. The window-global move handler that
                            // pans it is installed from the canvas paint while
                            // `dragging` is set (so it keeps working past the window
                            // edge), hence the repaint request. The release is caught
                            // both there and by the element handler below.
                            this.dragging = true;
                            this.moved = false;
                            this.press = e.position;
                            this.last_mouse = e.position;
                            cx.notify();
                        }),
                    )
                    // End the drag on the release the element receives directly. The
                    // canvas-paint handler also ends it (and catches releases outside
                    // the window), but it is only installed once a frame paints with
                    // `dragging` set; a press-move-release quick enough to finish
                    // before that paint would otherwise never clear `dragging`,
                    // leaving every later move panning the view.
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, e: &MouseUpEvent, _, cx| {
                            end_drag(this, e, cx);
                            end_scroll_drag(this, cx);
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
                        let hit = hit_box(&g.boxes, gx, gy).map(|b| b.id);
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
                        let pan = (lx - content_x * new, ly - content_y * new);
                        this.pan.set(clamp_pan(pan, new, vp.size, content));
                        this.zoom.set(new);
                        // A manual zoom overrides any settle still easing.
                        this.anim.set(None);
                        cx.notify();
                    }))
                    // Track the shared focus handle here too (the chrome already
                    // tracks it on an ancestor) so this deepest focused element
                    // receives key events; `h` toggles hover-dim, `r` toggles
                    // edge reduction. Events still bubble up to the chrome's own
                    // key bindings.
                    .track_focus(&self.focus_handle)
                    .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                        if e.keystroke.key == "h" {
                            this.dim_enabled = !this.dim_enabled;
                            cx.notify();
                        } else if e.keystroke.key == "r" {
                            this.reduced = !this.reduced;
                            this.retoggle(cx);
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

/// Clamp the pan so the graph can never be moved out of view, the panning
/// counterpart to the fit-zoom floor that bounds zoom-out. Per axis the content
/// (its full extent times `zoom`) reaches the viewport edge — fully inside when
/// smaller, fully covering when larger — plus the same breathing room the fit zoom
/// leaves: `FIT_MARGIN` holds a zoomed-out graph off the edges, so the pan bound
/// permits an equal clearance rather than snapping the content flush. Without it a
/// settle that frames a node with room to spare jumps the first time the user pans.
fn clamp_pan(pan: (f32, f32), zoom: f32, viewport: Size<Pixels>, content: (f32, f32)) -> (f32, f32) {
    let axis = |p: f32, vp: f32, extent: f32| {
        let clearance = vp * (1.0 - FIT_MARGIN) / 2.0;
        p.clamp((vp - extent).min(0.0) - clearance, (vp - extent).max(0.0) + clearance)
    };
    (
        axis(pan.0, f32::from(viewport.width), content.0 * zoom),
        axis(pan.1, f32::from(viewport.height), content.1 * zoom),
    )
}

/// Ids of the boxes the viewport fully encloses at the given zoom and pan. The
/// view-settling that follows a toggle keeps these in frame; a partly-clipped
/// box (or one larger than the viewport) is left out, so a big container the
/// user only saw part of does not force the whole graph back into view.
fn enclosed_boxes(
    boxes: &[bviz_graph_layout::Box],
    viewport: Size<Pixels>,
    zoom: f32,
    pan: (f32, f32),
) -> Vec<Id> {
    let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
    boxes
        .iter()
        .filter(|b| {
            let x0 = (b.x as f32 + MARGIN as f32) * zoom + pan.0;
            let y0 = (b.y as f32 + MARGIN as f32) * zoom + pan.1;
            let x1 = ((b.x + b.width) as f32 + MARGIN as f32) * zoom + pan.0;
            let y1 = ((b.y + b.height) as f32 + MARGIN as f32) * zoom + pan.1;
            x0 >= 0. && y0 >= 0. && x1 <= vw && y1 <= vh
        })
        .map(|b| b.id)
        .collect()
}

/// Ease-out cubic over `t` in `0..=1`: quick to start, decelerating into place,
/// so a settle arrives gently rather than stopping abruptly.
fn ease_out_cubic(t: f32) -> f32 {
    let u = 1.0 - t;
    1.0 - u * u * u
}

/// The zoom and pan that keep `target` (a layout-space rect `(x, y, w, h)`) fully
/// in view after a toggle, disturbing the view as little as that allows. The zoom
/// only ever drops: it stays put when `target` already fits, and falls just far
/// enough to enclose it otherwise — never zooming in to fill the view with one
/// box, nor below the whole-graph fit. The rect's center is held on screen across
/// any zoom change so the content does not lurch, then the pan shifts the minimum
/// needed to seat the rect inside with `RECENTER_PAD` of breathing room.
fn settle_view(
    target: (f64, f64, f64, f64),
    viewport: Size<Pixels>,
    content: (f32, f32),
    zoom: f32,
    pan: (f32, f32),
) -> (f32, (f32, f32)) {
    let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
    let (bx, by, bw, bh) = (target.0 as f32, target.1 as f32, target.2 as f32, target.3 as f32);
    let z_fit = if bw > 0. && bh > 0. {
        (vw / bw).min(vh / bh) * FIT_MARGIN
    } else {
        zoom
    };
    let z = zoom.min(z_fit).clamp(fit_zoom(viewport, content), MAX_ZOOM);
    let (cx, cy) = (bx + bw / 2. + MARGIN as f32, by + bh / 2. + MARGIN as f32);
    let mut panx = cx * zoom + pan.0 - cx * z;
    let mut pany = cy * zoom + pan.1 - cy * z;
    let (sx0, sx1) = ((bx + MARGIN as f32) * z + panx, (bx + bw + MARGIN as f32) * z + panx);
    let (sy0, sy1) = ((by + MARGIN as f32) * z + pany, (by + bh + MARGIN as f32) * z + pany);
    if sx0 < RECENTER_PAD {
        panx += RECENTER_PAD - sx0;
    } else if sx1 > vw - RECENTER_PAD {
        panx -= sx1 - (vw - RECENTER_PAD);
    }
    if sy0 < RECENTER_PAD {
        pany += RECENTER_PAD - sy0;
    } else if sy1 > vh - RECENTER_PAD {
        pany -= sy1 - (vh - RECENTER_PAD);
    }
    (z, (panx, pany))
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

/// A polyline parallel to `pts` at signed perpendicular distance `d` (screen px),
/// for drawing a node pair's several provenances as evenly spaced parallel lanes.
/// Every vertex is pushed off its segments' normals — the endpoints perpendicular
/// to their one segment, the interior mitered at the corners — so a lane stays
/// parallel to the route along its whole length and meets each node offset
/// alongside its siblings rather than converging to a single point.
fn lane_polyline(pts: &[(f32, f32)], d: f32) -> Vec<(f32, f32)> {
    if d == 0.0 || pts.len() < 2 {
        return pts.to_vec();
    }
    let left_normal = |from: (f32, f32), to: (f32, f32)| {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let l = (dx * dx + dy * dy).sqrt().max(1e-3);
        (-dy / l, dx / l)
    };
    let n = pts.len();
    let mut out = Vec::with_capacity(n);
    let ns = left_normal(pts[0], pts[1]);
    out.push((pts[0].0 + ns.0 * d, pts[0].1 + ns.1 * d));
    for i in 1..n - 1 {
        out.push(miter_vertex(pts[i - 1], pts[i], pts[i + 1], d));
    }
    let ne = left_normal(pts[n - 2], pts[n - 1]);
    out.push((pts[n - 1].0 + ne.0 * d, pts[n - 1].1 + ne.1 * d));
    out
}

/// Offsets the corner `b` (between segments `a->b` and `b->c`) by `d` along the
/// miter direction, keeping it distance `d` from both segments. A near-reversal,
/// or a miter that would shoot far past a sharp corner, falls back to one
/// segment's normal.
fn miter_vertex(a: (f32, f32), b: (f32, f32), c: (f32, f32), d: f32) -> (f32, f32) {
    let unit = |(x, y): (f32, f32)| {
        let l = (x * x + y * y).sqrt().max(1e-3);
        (x / l, y / l)
    };
    let d1 = unit((b.0 - a.0, b.1 - a.1));
    let d2 = unit((c.0 - b.0, c.1 - b.1));
    let (n1, n2) = ((-d1.1, d1.0), (-d2.1, d2.0)); // left normals
    let m = (n1.0 + n2.0, n1.1 + n2.1);
    let ml = (m.0 * m.0 + m.1 * m.1).sqrt();
    if ml < 1e-3 {
        return (b.0 + n1.0 * d, b.1 + n1.1 * d);
    }
    let mdir = (m.0 / ml, m.1 / ml);
    let cos_half = (mdir.0 * n1.0 + mdir.1 * n1.1).max(0.35); // miter limit
    let scale = d / cos_half;
    (b.0 + mdir.0 * scale, b.1 + mdir.1 * scale)
}

/// Strokes a connector polyline (screen coordinates) and caps its end with a
/// filled arrowhead pointing along the final segment.
fn paint_connector(points: &[(f32, f32)], kind: EdgeKind, dim: bool, zoom: f32, window: &mut Window) {
    if points.len() < 2 {
        return;
    }
    let mut color = edge_color(kind);
    if dim {
        color.a = DIM_EDGE_ALPHA;
    }
    let pts: Vec<Point<Pixels>> = points.iter().map(|(x, y)| point(px(*x), px(*y))).collect();

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
fn build_graph(inputs: &[Input]) -> Result<(Graph, Graph, Names), String> {
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
    let (base, names) =
        bviz_graph_layout::build_joint(aq.as_deref(), cq.as_deref(), q.as_deref(), false);
    // Transitively reduce the whole base once: a collapsed box then draws the
    // reduction of its hidden subtree's edges, so its edge count is stable under
    // expansion rather than re-derived (differently) from each view.
    let reduced = bviz_graph_layout::reduce::transitive_reduction(&base);
    Ok((base, reduced, names))
}

/// Lights the shortest drawn path from `subtree` to each target. A breadth-first
/// walk over `adj` records each node's predecessor; reconstructing from a target
/// back to the subtree yields the path's edges. With `forward`, `adj` is the
/// out-adjacency and a predecessor edge `p->cur` is itself a drawn edge; otherwise
/// `adj` is the in-adjacency and the drawn edge runs the other way, `cur->p`. The
/// walk seeds the subtree in id order so a tie between equal-length paths resolves
/// the same way every paint, rather than flickering with hash iteration order.
fn trace_paths(
    subtree: &HashSet<Id>,
    targets: &HashSet<Id>,
    adj: &HashMap<Id, Vec<Id>>,
    forward: bool,
    lit: &mut HashSet<(Id, Id)>,
) {
    let mut came: HashMap<Id, Id> = HashMap::new();
    let mut seen: HashSet<Id> = subtree.clone();
    let mut seeds: Vec<Id> = subtree.iter().copied().collect();
    seeds.sort_unstable_by_key(|id| id.0);
    let mut queue: VecDeque<Id> = seeds.into();
    while let Some(u) = queue.pop_front() {
        for &w in adj.get(&u).into_iter().flatten() {
            if seen.insert(w) {
                came.insert(w, u);
                queue.push_back(w);
            }
        }
    }
    for &target in targets {
        let mut cur = target;
        while !subtree.contains(&cur) {
            let Some(&prev) = came.get(&cur) else { break };
            lit.insert(if forward { (prev, cur) } else { (cur, prev) });
            cur = prev;
        }
    }
}

/// Builds the display graph with the given ids expanded and lays it out: fold
/// every unexpanded container away, merge interchangeable leaves, then lay out
/// only the resulting visible view. When `reduced`, the view is transitively
/// reduced in two steps. First the base's reduction projects onto the view's
/// boxes — the full base and the already-reduced base collapse to the same
/// boxes, so a collapsed box stands in for its hidden subtree's reduced edges,
/// keeping its edge count stable under expansion. Then the projected box graph
/// is reduced again, because merging a subtree into one box creates box-level
/// shortcuts that no longer hold at node granularity (a box A->B edge implied by
/// a drawn A->M->B path through other visible boxes); the second pass drops them.
/// `full_edges` keeps the unreduced connectivity for hover.
fn relayout(
    base: &Graph,
    reduced_base: &Graph,
    expanded: &HashSet<Id>,
    names: &mut Names,
    reduced: bool,
) -> LaidOut {
    let visible = bviz_graph_layout::lod::collapse(base, expanded);
    let (mut view, remap) = quotient::quotient(&visible, expanded, names);
    let full_edges: Vec<(Id, Id)> =
        view.edges.iter().map(|e| (e.from, e.to)).collect::<HashSet<_>>().into_iter().collect();
    if reduced {
        // Project the reduced base's edges onto the same boxes (a collapsed group's
        // members map through `remap`, everything else keeps its id), and keep only
        // those of the view's edges.
        let skeleton = bviz_graph_layout::lod::collapse(reduced_base, expanded);
        let box_of = |id: Id| remap.get(&id).copied().unwrap_or(id);
        let kept: HashSet<(Id, Id)> = skeleton
            .edges
            .iter()
            .map(|e| (box_of(e.from), box_of(e.to)))
            .filter(|(a, b)| a != b)
            .collect();
        view.edges.retain(|e| kept.contains(&(e.from, e.to)));
        view = bviz_graph_layout::reduce::transitive_reduction(&view);
    }
    let mut laid = layout::layout(&view);
    laid.full_edges = full_edges;
    laid
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
    let pan = (px + dx, py + dy);
    view.pan.set(match &view.source {
        Source::Loaded(g) => {
            let content = (g.width as f32 + 2. * MARGIN as f32, g.height as f32 + 2. * MARGIN as f32);
            clamp_pan(pan, view.zoom.get(), view.viewport.get().size, content)
        }
        _ => pan,
    });
    view.last_mouse = event.position;
    // A manual pan overrides any settle still easing.
    view.anim.set(None);
    cx.notify();
}

/// End a drag. A release that never travelled is a click: open or close the
/// container (or group) box under the cursor.
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
    view.prev_visible = enclosed_boxes(&g.boxes, vp.size, view.zoom.get(), view.pan.get());
    // The relayout may drop the hovered node (it was collapsed away), which would
    // otherwise leave the whole graph dimmed until the next mouse move.
    view.hovered = None;
    view.recent = Some(id);
    // Retire any in-flight fade so it can't clear this fresh `recent`; the new
    // highlight starts when the relayout lands.
    view.highlight_seq = view.highlight_seq.wrapping_add(1);
    view.retoggle(cx);
}

/// The scroll bar for `axis` given the view's current zoom, pan, and `viewport`
/// (window px), or `None` when that axis fits and needs none. The bar's track,
/// content, visible extent, and offset are the axis length in window px, so a
/// press maps straight through in window coordinates.
fn scrollbar_geom(
    axis: Axis,
    viewport: Bounds<Pixels>,
    content: (f32, f32),
    zoom: f32,
    pan: (f32, f32),
) -> Option<Scrollbar> {
    let (vw, vh) = (f32::from(viewport.size.width), f32::from(viewport.size.height));
    let (cw, ch) = (content.0 * zoom, content.1 * zoom);
    let (vlen, clen, panc, other_overflows) = match axis {
        Axis::Vertical => (vh, ch, pan.1, cw > vw),
        Axis::Horizontal => (vw, cw, pan.0, ch > vh),
    };
    // Pan is the screen offset of the content's near edge: 0 at the start, down to
    // `viewport - content` at the far edge, so the distance scrolled past the near
    // edge is `-pan`, in `[0, content - viewport]`.
    let offset = (-panc).clamp(0.0, (clen - vlen).max(0.0));
    // Where the crossing axis overflows too, stop the track short of its bar so the
    // two don't overlap in the corner.
    let track_len = vlen - if other_overflows { SCROLLBAR_THICKNESS } else { 0.0 };
    let thickness = px(SCROLLBAR_THICKNESS);
    let track = match axis {
        Axis::Vertical => Bounds::new(
            point(viewport.right() - thickness, viewport.top()),
            size(thickness, px(track_len)),
        ),
        Axis::Horizontal => Bounds::new(
            point(viewport.left(), viewport.bottom() - thickness),
            size(px(track_len), thickness),
        ),
    };
    Scrollbar::new(axis, track, clen, vlen, offset, MIN_THUMB)
}

/// The scroll bar for `axis` built from the loaded graph's current view state.
fn current_scrollbar(view: &GraphView, axis: Axis) -> Option<Scrollbar> {
    let Source::Loaded(g) = &view.source else {
        return None;
    };
    let content = (g.width as f32 + 2. * MARGIN as f32, g.height as f32 + 2. * MARGIN as f32);
    scrollbar_geom(axis, view.viewport.get(), content, view.zoom.get(), view.pan.get())
}

/// The axis of the scroll bar whose track a press at `position` (window px) landed
/// on, or `None` when the press is over the content.
fn scrollbar_hit(view: &GraphView, position: Point<Pixels>) -> Option<Axis> {
    [Axis::Vertical, Axis::Horizontal].into_iter().find(|&axis| {
        current_scrollbar(view, axis).is_some_and(|bar| bar.track_hit(position))
    })
}

/// Scroll `axis` so `offset` window px of content sit past the near edge, panning
/// the content's near edge to `-offset`.
fn scroll_to(view: &mut GraphView, axis: Axis, offset: f32, cx: &mut Context<GraphView>) {
    let Source::Loaded(g) = &view.source else {
        return;
    };
    let content = (g.width as f32 + 2. * MARGIN as f32, g.height as f32 + 2. * MARGIN as f32);
    let zoom = view.zoom.get();
    let vp = view.viewport.get().size;
    let (px, py) = view.pan.get();
    let pan = match axis {
        Axis::Vertical => (px, -offset),
        Axis::Horizontal => (-offset, py),
    };
    view.pan.set(clamp_pan(pan, zoom, vp, content));
    // A manual scroll overrides any settle still easing.
    view.anim.set(None);
    cx.notify();
}

/// Begin a scroll-bar drag: record the grabbed thumb and the grab offset, then
/// scroll to the press so a track click jumps the view at once.
fn start_scroll_drag(
    view: &mut GraphView,
    axis: Axis,
    position: Point<Pixels>,
    cx: &mut Context<GraphView>,
) {
    let Some(bar) = current_scrollbar(view, axis) else {
        return;
    };
    let grab = bar.grab(position);
    view.scroll_drag = Some(ScrollDrag { axis, grab });
    scroll_to(view, axis, bar.offset_at(position, grab), cx);
}

/// Track a held scroll thumb to the cursor.
fn scroll_drag_move(view: &mut GraphView, event: &MouseMoveEvent, cx: &mut Context<GraphView>) {
    let Some(drag) = view.scroll_drag else {
        return;
    };
    let Some(bar) = current_scrollbar(view, drag.axis) else {
        return;
    };
    scroll_to(view, drag.axis, bar.offset_at(event.position, drag.grab), cx);
}

/// End a scroll-bar drag. Called both from the element release handler and the
/// window-global one, so it is a no-op when no drag is in progress.
fn end_scroll_drag(view: &mut GraphView, cx: &mut Context<GraphView>) {
    if view.scroll_drag.take().is_some() {
        cx.notify();
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
        let mut ids = enclosed_boxes(&boxes, vp(800., 600.), 1.0, (0.0, 0.0));
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
