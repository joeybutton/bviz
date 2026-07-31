use crate::camera::{to_screen, Camera};
use crate::dimming::Dimming;
use crate::{Highlight, PaintBox, Palette, HIGHLIGHT_LINGER};
use bviz_graph_layout::layout::CARET_RESERVE;
use bviz_graph_layout::{EdgeKind, Id, Kind, LaidOut};
use gpui::{
    App, BorderStyle, Bounds, Hsla, PathBuilder, Pixels, Point, Rgba, TextAlign, Window, point, px,
    quad, rgb, size,
};
use std::collections::HashSet;

/// Height of the top band a container reserves for its title.
const LABEL_BAND: f64 = 26.0;

/// Box label font size in layout pixels.
const LABEL_SIZE: f64 = 13.0;

/// Arrowhead size along and across the edge direction, in layout pixels.
const ARROW_LENGTH: f64 = 9.0;
const ARROW_HALF_WIDTH: f64 = 4.0;
/// How far the stroke runs past the arrowhead base into the head, closing the
/// seam without letting the line broaden the visible tip.
const ARROW_OVERDRAW: f64 = 1.0;

/// Fraction by which a kind accent is blended toward the themed background; the
/// result is the leaf fill, so kinds stay recognizable yet read on light or dark.
const KIND_BLEND: f32 = 0.68;

/// Accent for the just-toggled box's emphasis border and inset glow, bright
/// enough to catch the eye on either a light or dark background.
const HIGHLIGHT_ACCENT: Rgba = Rgba { r: 1.0, g: 0.819, b: 0.4, a: 1.0 };

/// Fraction a dimmed box's fill and border are pulled toward the background, so
/// the hovered node's neighborhood stands out against the receded rest.
const DIM_FADE: f32 = 0.6;

/// Alpha a dimmed connector keeps, fading edges outside the hovered node's
/// neighborhood without erasing them.
const DIM_EDGE_ALPHA: f32 = 0.2;

/// Accent hue for a leaf box, keyed by its node kind. These are mid-saturation
/// and recognizable against either a light or dark background.
pub(crate) fn kind_accent(kind: Kind) -> Rgba {
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
pub(crate) fn edge_color(kind: EdgeKind) -> Rgba {
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

/// Flattens each laid-out box to what the canvas paints, resolving its themed
/// fill and marking those the dimming fades.
pub(crate) fn paint_boxes(
    g: &LaidOut,
    expandable: &HashSet<Id>,
    highlight: Option<Highlight>,
    dimming: &Dimming,
    palette: &Palette,
) -> Vec<PaintBox> {
    // The highlight fades linearly to nothing over the linger window.
    let (recent, highlight_alpha) = match highlight {
        Some(h) => (
            Some(h.id),
            (1.0 - h.at.elapsed().as_secs_f32() / HIGHLIGHT_LINGER.as_secs_f32()).clamp(0.0, 1.0),
        ),
        None => (None, 0.0),
    };
    g.boxes
        .iter()
        .map(|b| PaintBox {
            x: b.x,
            y: b.y,
            w: b.width,
            h: b.height,
            compound: b.compound,
            expandable: expandable.contains(&b.id) || b.group,
            fill: if b.compound {
                container_fill(b.kind, palette)
            } else {
                bviz_ui_theme::mix(kind_accent(b.kind), palette.background, KIND_BLEND)
            },
            label: b.label.clone(),
            highlight: if recent == Some(b.id) { highlight_alpha } else { 0.0 },
            dim: dimming.applies && !dimming.lit.contains(&b.id),
        })
        .collect()
}

/// A connector ready to paint, owned so the `'static` paint closure needs no
/// borrow of the graph.
pub(crate) struct PaintConnector {
    /// The routed polyline, in graph coordinates.
    pub(crate) points: Vec<(f64, f64)>,
    /// The edge kinds sharing the route, drawn as parallel lanes along it.
    pub(crate) kinds: Vec<EdgeKind>,
    /// Whether the hover dimming fades this connector.
    pub(crate) dim: bool,
}

/// Snapshots each of the graph's connectors for the painter.
pub(crate) fn collect_connectors(g: &LaidOut, dimming: &Dimming) -> Vec<PaintConnector> {
    g.connectors
        .iter()
        .map(|c| PaintConnector {
            points: c.points.clone(),
            kinds: c.kinds.clone(),
            dim: dimming.applies && !dimming.lit_edges.contains(&(c.from, c.to)),
        })
        .collect()
}

/// Paints a box: its filled quad, an emphasis border and glow when highlighted,
/// and its centered label. Off-screen boxes are culled.
pub(crate) fn paint_box(
    b: &PaintBox,
    bounds: Bounds<Pixels>,
    camera: Camera,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let Camera { zoom, pan: _ } = camera;
    let origin = to_screen(b.x, b.y, bounds, camera);
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
    let ((border_w, border_c), glow) = box_emphasis(b, zoom, palette, rect, corner);
    window.paint_quad(quad(rect, corner, fill, border_w, border_c, BorderStyle::Solid));
    glow(window);
    paint_box_label(b, rect, zoom, palette, window, cx);
}

/// A box's emphasis: the quad border to stroke, and a deferred paint of the glow
/// wash. The border fades from the themed border to the amber accent, and from a
/// normal to a thick stroke, by the highlight alpha; a dim box uses a faded
/// themed border. The returned closure paints a faint accent wash over the fill
/// so the emphasis reads as a glow, not just a brighter outline.
fn box_emphasis(
    b: &PaintBox,
    zoom: f32,
    palette: &Palette,
    rect: Bounds<Pixels>,
    corner: Pixels,
) -> ((Pixels, Hsla), impl FnOnce(&mut Window)) {
    let (border_w, border_c) = if b.highlight > 0.0 {
        let normal = px(1.);
        let thick = px((2. * zoom).max(2.));
        let mut accent = HIGHLIGHT_ACCENT;
        accent.a = b.highlight;
        (normal + (thick - normal) * b.highlight, Hsla::from(accent))
    } else if b.dim {
        (px(1.), Hsla::from(bviz_ui_theme::mix(palette.border, palette.background, DIM_FADE)))
    } else {
        (px(1.), Hsla::from(palette.border))
    };
    let highlight = b.highlight;
    let glow = move |window: &mut Window| {
        if highlight > 0.0 {
            let mut wash = HIGHLIGHT_ACCENT;
            wash.a = 0.15 * highlight;
            window.paint_quad(quad(
                rect,
                corner,
                wash,
                px(0.),
                Hsla::transparent_black(),
                BorderStyle::Solid,
            ));
        }
    };
    ((border_w, border_c), glow)
}

/// Paints a box's centered label, with a disclosure caret for a collapsed group
/// leaf. A container's title sits in its top band; other labels center in the
/// whole box. The font is the window's inherited UI text style so the views stay
/// visually coherent; a container's title is bold, and its size is the layout's
/// own, kept in step with the width the layout reserved for the label.
fn paint_box_label(
    b: &PaintBox,
    rect: Bounds<Pixels>,
    zoom: f32,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let label_color = if b.dim {
        bviz_ui_theme::mix(palette.label, palette.background, DIM_FADE)
    } else {
        palette.label
    };
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
pub(crate) fn lane_polyline(pts: &[(f32, f32)], d: f32) -> Vec<(f32, f32)> {
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
pub(crate) fn paint_connector(points: &[(f32, f32)], kind: EdgeKind, dim: bool, zoom: f32, window: &mut Window) {
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
