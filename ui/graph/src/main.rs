//! Loads a Bazel `aquery --output=streamed_proto` file through
//! `bviz_graph_layout`, lays the action graph out, and opens a window
//! summarizing the result.

use bviz_graph_layout::{aquery, layout, streamed_proto, LaidOut};
use bviz_ui_shell::Chrome;
use gpui::{
    App, BorderStyle, Bounds, Context, Div, FocusHandle, IntoElement, MouseButton, MouseDownEvent,
    Hsla, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, Point, Render, Rgba, ScrollDelta,
    ScrollWheelEvent, Stateful, TextAlign, TextRun, Window, canvas, div, font, point, prelude::*,
    px, quad, rgb, size,
};
use std::cell::Cell;
use std::rc::Rc;

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

/// The loaded graph, or a message explaining why none is shown.
enum Source {
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
    /// Fill resolved from the active theme when the box vec is built.
    fill: Rgba,
    label: String,
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

    /// Builds the single-canvas viewport: every box and connector is painted in
    /// one `canvas`, with pan and zoom applied in the paint transform. The
    /// closure is `'static`, so it captures owned snapshots of the boxes and
    /// connectors rather than borrowing the graph.
    fn graph(
        g: &LaidOut,
        zoom: f32,
        pan: (f32, f32),
        palette: Palette,
        viewport_origin: Rc<Cell<Point<Pixels>>>,
    ) -> Stateful<Div> {
        let boxes: Vec<PaintBox> = g
            .boxes
            .iter()
            .map(|b| PaintBox {
                x: b.x,
                y: b.y,
                w: b.width,
                h: b.height,
                compound: b.compound,
                fill: if b.compound {
                    palette.container_fill
                } else {
                    bviz_ui_theme::mix(kind_accent(&b.kind), palette.background, KIND_BLEND)
                },
                label: b.label.clone(),
            })
            .collect();
        let connectors: Vec<Vec<(f64, f64)>> =
            g.connectors.iter().map(|c| c.points.clone()).collect();
        div()
            .id("graph-viewport")
            .relative()
            .size_full()
            .overflow_hidden()
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, cx| {
                        viewport_origin.set(bounds.origin);
                        // Boxes parent-before-child so children paint over their
                        // containers; connectors last so edges sit over the fills.
                        for b in &boxes {
                            paint_box(b, bounds, zoom, pan, &palette, window, cx);
                        }
                        for points in &connectors {
                            paint_connector(points, bounds, zoom, pan, &palette, window);
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
                    Self::graph(g, self.zoom, self.pan, palette, self.viewport_origin.clone())
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

    let font = if b.compound {
        font("Inter").bold()
    } else {
        font("Inter")
    };
    let run = TextRun {
        len: b.label.len(),
        font,
        color: palette.label.into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let fs = px(LABEL_SIZE as f32 * zoom);
    let line = window
        .text_system()
        .shape_line(b.label.clone().into(), fs, &[run], None);
    let tx = rect.origin.x + (rect.size.width - line.width()) / 2.;
    let ty = if b.compound {
        let band = px(LABEL_BAND as f32 * zoom);
        rect.origin.y + (band - fs) / 2.
    } else {
        rect.origin.y + (rect.size.height - fs) / 2.
    };
    let _ = line.paint(point(tx, ty), fs, TextAlign::Left, None, window, cx);
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
