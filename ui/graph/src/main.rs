//! Loads a Bazel `aquery --output=streamed_proto` file through
//! `bviz_graph_layout`, lays the action graph out, and opens a window
//! summarizing the result.

use bviz_graph_layout::graph::Graph;
use bviz_graph_layout::layout::CARET_RESERVE;
use bviz_graph_layout::{aquery, layout, quotient, streamed_proto, LaidOut};
use bviz_ui_components::{load_in_background, track_drag};
use bviz_ui_shell::Chrome;
use gpui::{
    Animation, AnimationExt, App, BorderStyle, Bounds, Context, Div, Entity, FocusHandle,
    IntoElement, MouseButton, MouseDownEvent, Hsla, MouseMoveEvent, MouseUpEvent, PathBuilder,
    Pixels, Point, Render, Rgba, ScrollDelta, ScrollWheelEvent, Stateful, Task, TextAlign, Size,
    Transformation, Window, canvas, div, percentage, point, prelude::*, px, quad, rgb, size,
};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

/// Height of the top band a container reserves for its title.
const LABEL_BAND: f64 = 26.0;

/// Box label font size in layout pixels.
const LABEL_SIZE: f64 = 13.0;

/// Padding around the graph, matching the SVG renderer's margin.
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
    connector: Rgba,
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
    /// The graph node id, so a click can be mapped back to its node.
    id: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    compound: bool,
    /// Whether this box's id is in the expansion set, which points its
    /// disclosure caret down rather than right.
    expanded: bool,
    /// Fill resolved from the active theme when the box vec is built.
    fill: Rgba,
    label: String,
}

/// Loads, lays out, and summarizes an aquery action graph.
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
    /// Group ids currently expanded into their member files.
    expanded: Rc<RefCell<HashSet<String>>>,
    /// Holds the background load/relayout task so it isn't cancelled on drop.
    _load_task: Task<()>,
}

/// A cursor must travel at least this far from the press before the gesture
/// counts as a drag rather than a click.
const CLICK_SLOP: f32 = 4.0;

impl GraphView {
    fn new(path: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let base: Rc<RefCell<Option<Graph>>> = Rc::new(RefCell::new(None));
        let (source, load_task) = match path {
            // Build and lay out the graph on the background executor so window
            // appearance and interactivity are never blocked; swap the view to
            // the result and cache the base graph when it completes.
            Some(path) => {
                let task = load_in_background(
                    cx,
                    move || {
                        let graph = build_graph(&path)?;
                        let laid_out = relayout(&graph, &HashSet::new());
                        Ok::<_, String>((graph, laid_out))
                    },
                    |view, result, _| {
                        view.source = match result {
                            Ok((graph, laid_out)) => {
                                *view.base.borrow_mut() = Some(graph);
                                Source::Loaded(laid_out)
                            }
                            Err(err) => Source::Message(err),
                        };
                    },
                );
                (Source::Loading, task)
            }
            None => (
                Source::Message("Usage: graph <aquery.streamed_proto>".into()),
                Task::ready(()),
            ),
        };
        Self {
            focus_handle,
            source,
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
        self._load_task = load_in_background(
            cx,
            move || relayout(&graph, &expanded),
            |view, laid_out, _| view.source = Source::Loaded(laid_out),
        );
    }

    /// Builds the single-canvas viewport: every box and connector is painted in
    /// one `canvas`, with pan and zoom applied in the paint transform. The
    /// closure is `'static`, so it captures owned snapshots of the boxes and
    /// connectors rather than borrowing the graph.
    fn graph(
        g: &LaidOut,
        expanded: &HashSet<String>,
        entity: Entity<GraphView>,
        zoom: Rc<Cell<f32>>,
        pan: Rc<Cell<(f32, f32)>>,
        fitted: Rc<Cell<bool>>,
        viewport: Rc<Cell<Bounds<Pixels>>>,
        palette: Palette,
    ) -> Stateful<Div> {
        let boxes: Vec<PaintBox> = g
            .boxes
            .iter()
            .map(|b| PaintBox {
                id: b.id.clone(),
                x: b.x,
                y: b.y,
                w: b.width,
                h: b.height,
                compound: b.compound,
                expanded: expanded.contains(&b.id),
                fill: if b.compound {
                    container_fill(&b.kind, &palette)
                } else {
                    bviz_ui_theme::mix(kind_accent(&b.kind), palette.background, KIND_BLEND)
                },
                label: b.label.clone(),
            })
            .collect();
        let connectors: Vec<Vec<(f64, f64)>> =
            g.connectors.iter().map(|c| c.points.clone()).collect();
        let content = (
            g.width as f32 + 2. * MARGIN as f32,
            g.height as f32 + 2. * MARGIN as f32,
        );
        // Capture the viewport bounds and fit the graph to them. The fit needs
        // the viewport size, known only once laid out, so it happens here.
        let prepaint = {
            let (zoom, pan, fitted, viewport) =
                (zoom.clone(), pan.clone(), fitted.clone(), viewport.clone());
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
                    for points in &connectors {
                        paint_connector(points, bounds, z, p, &palette, window);
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
            connector: bviz_ui_theme::mix(colors.text, colors.background, 0.45),
        };
        let body = match &self.source {
            Source::Loaded(g) => div()
                .flex_1()
                .flex()
                .overflow_hidden()
                .child(
                    Self::graph(
                        g,
                        &self.expanded.borrow(),
                        cx.entity(),
                        self.zoom.clone(),
                        self.pan.clone(),
                        self.fitted.clone(),
                        self.viewport.clone(),
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
    window.paint_quad(quad(
        rect,
        px(6. * zoom),
        b.fill,
        px(1.),
        Hsla::from(palette.border),
        BorderStyle::Solid,
    ));

    // The label font comes from the window's inherited UI text style (the same
    // one the chrome and the other views use), so the views stay visually
    // coherent; a container's title is bold. The size is the layout's own, kept
    // in step with the width the layout reserved for the label.
    let base = bviz_ui_assets::text_metrics(window).font;
    let font = if b.compound { base.bold() } else { base };
    let fs = px(LABEL_SIZE as f32 * zoom);
    let line = bviz_ui_assets::shape_line(window, b.label.clone(), font, fs, palette.label.into());
    // A collapsed group leaf carries a disclosure caret left of the label,
    // occupying the reserved width the layout sized the box to hold; the label
    // and caret are centered together as one unit. Other boxes center the label
    // alone.
    let caret = b.id.starts_with("group:");
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

    // A collapsed box points the caret right, an expanded one (its id in the
    // expansion set) points it down. Phosphor icons render as `svg()` elements
    // and cannot be painted inside this canvas, so the caret is a small filled
    // triangle.
    if caret {
        let cy = ty + fs / 2.;
        paint_caret(
            point(unit_left, cy),
            footprint,
            fs,
            b.expanded,
            palette.label,
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
    bounds: Bounds<Pixels>,
    zoom: f32,
    pan: (f32, f32),
    palette: &Palette,
    window: &mut Window,
) {
    if points.len() < 2 {
        return;
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
        window.paint_path(path, palette.connector);
    }

    paint_arrowhead(&pts, zoom, palette.connector, window);
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

/// Reads the aquery proto at `path` and builds its action graph, before any
/// group quotienting. Cached so toggling a group needn't re-read or re-parse.
fn build_graph(path: &str) -> Result<Graph, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let containers =
        streamed_proto::read_action_graph_containers(&bytes).map_err(|e| e.to_string())?;
    Ok(aquery::build(&containers, false))
}

/// Builds the display graph with the given ids expanded and lays it out.
fn relayout(graph: &Graph, expanded: &HashSet<String>) -> LaidOut {
    layout::layout(&quotient::quotient(graph, expanded))
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

/// End a drag. A release that never travelled is a click: toggle the group box
/// under the cursor.
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
    if !hit.id.starts_with("group:") {
        return;
    }
    let id = hit.id.clone();
    let mut expanded = view.expanded.borrow_mut();
    if !expanded.remove(&id) {
        expanded.insert(id);
    }
    drop(expanded);
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

fn main() {
    let path = std::env::args().nth(1);
    bviz_ui_shell::run(
        "bazel_viz graph",
        size(px(1280.), px(820.)),
        move |window, cx| cx.new(|cx| GraphView::new(path, window, cx)),
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
