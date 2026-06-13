//! Loads a Bazel `aquery --output=streamed_proto` file through
//! `bviz_graph_layout`, lays the action graph out, and opens a window
//! summarizing the result.

use bviz_graph_layout::{aquery, layout, streamed_proto, LaidOut};
use bviz_ui_shell::Chrome;
use gpui::{
    Bounds, Context, Div, FontWeight, FocusHandle, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, Point, Render, ScrollDelta, ScrollWheelEvent,
    Stateful,
    Window, canvas, div, point, prelude::*, px, rgb,
};
use std::cell::Cell;
use std::rc::Rc;

/// Stroke color shared by connector polylines and their arrowheads.
const CONNECTOR_COLOR: u32 = 0x5a5a5a;

/// Padding around the graph, matching the SVG renderer's margin.
const MARGIN: f64 = 16.0;

/// Fill color for a leaf box, keyed by its node kind, matching the SVG palette.
fn leaf_fill(kind: &str) -> u32 {
    match kind {
        "action" => 0xfde8c8,
        "artifact" => 0xd6efd6,
        "source" => 0xe8e0f4,
        _ => 0xeef3fb,
    }
}

/// The loaded graph, or a message explaining why none is shown.
enum Source {
    Loaded(LaidOut),
    Message(String),
}

/// Loads, lays out, and summarizes an aquery action graph.
struct GraphView {
    focus_handle: FocusHandle,
    source: Source,
    /// Zoom scale baked into every coordinate (1.0 = layout pixels).
    zoom: f32,
    /// Pixel translation applied after scaling.
    pan: (f32, f32),
    dragging: bool,
    last_mouse: Point<Pixels>,
    /// Screen origin of the viewport, captured during canvas paint so the
    /// scroll handler can map a window cursor position to viewport-local space.
    viewport_origin: Rc<Cell<Point<Pixels>>>,
}

impl GraphView {
    fn new(source: Source, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        Self {
            focus_handle,
            source,
            zoom: 1.0,
            pan: (0.0, 0.0),
            dragging: false,
            last_mouse: Point::default(),
            viewport_origin: Rc::new(Cell::new(Point::default())),
        }
    }

    /// Builds the box elements at their layout coordinates with pan and zoom
    /// baked in: containers carry a top label band, leaves are filled by kind.
    /// Boxes are emitted parent-before-child so children paint over the
    /// containers that enclose them; the connectors canvas spans the whole
    /// viewport and is added last so it paints over the boxes.
    fn graph(
        g: &LaidOut,
        zoom: f32,
        pan: (f32, f32),
        viewport_origin: Rc<Cell<Point<Pixels>>>,
    ) -> Stateful<Div> {
        let mut container = div()
            .id("graph-viewport")
            .relative()
            .size_full()
            .overflow_hidden();
        for b in &g.boxes {
            let frame = div()
                .absolute()
                .left(px((b.x as f32 + MARGIN as f32) * zoom + pan.0))
                .top(px((b.y as f32 + MARGIN as f32) * zoom + pan.1))
                .w(px(b.width as f32 * zoom))
                .h(px(b.height as f32 * zoom))
                .rounded(px(6. * zoom))
                .border_1()
                .border_color(rgb(0x3a5a8c));
            let frame = if b.compound {
                frame.bg(rgb(0xdde6f2)).flex().flex_col().items_center().child(
                    div()
                        .h(px(26. * zoom))
                        .flex()
                        .items_center()
                        .text_size(px(13. * zoom))
                        .font_weight(FontWeight::BOLD)
                        .text_color(rgb(0x1f3354))
                        .child(b.label.clone()),
                )
            } else {
                frame
                    .bg(rgb(leaf_fill(&b.kind)))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(13. * zoom))
                    .text_color(rgb(0x1f3354))
                    .child(b.label.clone())
            };
            container = container.child(frame);
        }
        let connectors: Vec<Vec<(f64, f64)>> = g
            .connectors
            .iter()
            .map(|c| c.points.clone())
            .collect();
        container.child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    viewport_origin.set(bounds.origin);
                    for points in &connectors {
                        paint_connector(points, bounds, zoom, pan, window);
                    }
                },
            )
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
            Source::Message(_) => "No graph".into(),
        }
    }
}

impl Render for GraphView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = bviz_ui_theme::colors(window);
        let body = match &self.source {
            Source::Loaded(g) => div()
                .flex_1()
                .flex()
                .overflow_hidden()
                .child(
                    Self::graph(g, self.zoom, self.pan, self.viewport_origin.clone())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, e: &MouseDownEvent, _, _| {
                                this.dragging = true;
                                this.last_mouse = e.position;
                            }),
                        )
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _e: &MouseUpEvent, _, _| {
                                this.dragging = false;
                            }),
                        )
                        .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                            if this.dragging {
                                this.pan.0 += f32::from(e.position.x) - f32::from(this.last_mouse.x);
                                this.pan.1 += f32::from(e.position.y) - f32::from(this.last_mouse.y);
                                this.last_mouse = e.position;
                                cx.notify();
                            }
                        }))
                        .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, _, cx| {
                            // Anchor the zoom at the cursor: keep the content
                            // point under the cursor fixed as the scale changes.
                            let vp = this.viewport_origin.get();
                            let lx = f32::from(e.position.x) - f32::from(vp.x);
                            let ly = f32::from(e.position.y) - f32::from(vp.y);
                            let dy = match e.delta {
                                ScrollDelta::Lines(p) => p.y,
                                ScrollDelta::Pixels(p) => f32::from(p.y) / 120.0,
                            };
                            let old = this.zoom;
                            let new = (old * 1.1_f32.powf(dy)).clamp(0.2, 4.0);
                            let content_x = (lx - this.pan.0) / old;
                            let content_y = (ly - this.pan.1) / old;
                            this.pan.0 = lx - content_x * new;
                            this.pan.1 = ly - content_y * new;
                            this.zoom = new;
                            cx.notify();
                        })),
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

/// Maps a layout-space point to window space with pan and zoom baked in, atop
/// the same margin offset the boxes use and the canvas's origin within the
/// window.
fn to_screen(x: f64, y: f64, bounds: Bounds<Pixels>, zoom: f32, pan: (f32, f32)) -> Point<Pixels> {
    point(
        bounds.origin.x + px((x as f32 + MARGIN as f32) * zoom + pan.0),
        bounds.origin.y + px((y as f32 + MARGIN as f32) * zoom + pan.1),
    )
}

/// Strokes a connector polyline and caps its end with a filled arrowhead
/// pointing along the final segment.
fn paint_connector(
    points: &[(f64, f64)],
    bounds: Bounds<Pixels>,
    zoom: f32,
    pan: (f32, f32),
    window: &mut Window,
) {
    if points.len() < 2 {
        return;
    }
    let pts: Vec<Point<Pixels>> = points
        .iter()
        .map(|(x, y)| to_screen(*x, *y, bounds, zoom, pan))
        .collect();

    let mut line = PathBuilder::stroke(px(1.5 * zoom));
    for (i, p) in pts.iter().enumerate() {
        if i == 0 {
            line.move_to(*p);
        } else {
            line.line_to(*p);
        }
    }
    if let Ok(path) = line.build() {
        window.paint_path(path, rgb(CONNECTOR_COLOR));
    }

    paint_arrowhead(&pts, zoom, window);
}

/// Paints a filled triangle at the polyline's last point, aimed along the
/// direction of its final segment.
fn paint_arrowhead(pts: &[Point<Pixels>], zoom: f32, window: &mut Window) {
    let length = 9.0 * zoom;
    let half_width = 4.0 * zoom;

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
        window.paint_path(path, rgb(CONNECTOR_COLOR));
    }
}

/// Reads the aquery proto at `path` and lays out its action graph.
fn load(path: &str) -> Result<LaidOut, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let containers =
        streamed_proto::read_action_graph_containers(&bytes).map_err(|e| e.to_string())?;
    let graph = aquery::build(&containers, false);
    Ok(layout::layout(&graph))
}

fn main() {
    let source = match std::env::args().nth(1) {
        Some(path) => match load(&path) {
            Ok(laid_out) => Source::Loaded(laid_out),
            Err(err) => Source::Message(err),
        },
        None => Source::Message("Usage: graph <aquery.streamed_proto>".into()),
    };
    bviz_ui_shell::run("bazel_viz graph", move |window, cx| {
        cx.new(|cx| GraphView::new(source, window, cx))
    });
}
