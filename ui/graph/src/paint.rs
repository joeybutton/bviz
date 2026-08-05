use crate::camera::{to_screen, visible_rect, Camera, Rect};
use crate::dimming::{compute_dimming, Dimming};
use crate::drag::track_gesture;
use crate::model::{Drawn, Expandable};
use crate::scroll::paint_scrollbars;
use crate::{GraphView, Palette};
use bviz_graph_layout::layout::{CARET_RESERVE, LABEL_BAND};
use bviz_graph_layout::{Connector, EdgeKind, Id, Kind};
use gpui::{
    App, BorderStyle, Bounds, Entity, Hsla, PathBuilder, Pixels, Point, Rgba, TextAlign, Window,
    point, px, quad, rgb, size,
};

/// Box label font size in layout pixels.
const LABEL_SIZE: f64 = 13.0;

/// Screen-pixel size a label is never drawn below. Zoom scales the label with
/// the box down to this floor and no further, so a box wide enough to hold its
/// name shows it rather than fading to a blank rectangle.
const LABEL_FLOOR: f32 = 11.0;

/// Screen-pixel clearance held between a label and its box's edges, so a label
/// held at the floor is not painted flush against the border.
const LABEL_PAD: f32 = 2.0;

/// Screen width a box needs to spare for its label before one is drawn: any
/// narrower and what survives truncation names nothing, so the box is left bare.
const LABEL_MIN_WIDTH: f32 = 22.0;

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

/// Accent for the just-toggled box's emphasis border and inset glow, bright
/// enough to catch the eye on either a light or dark background.
const HIGHLIGHT_ACCENT: Rgba = Rgba { r: 1.0, g: 0.819, b: 0.4, a: 1.0 };

/// Fraction a dimmed box's fill and border are pulled toward the background, so
/// the hovered node's neighborhood stands out against the receded rest.
const DIM_FADE: f32 = 0.6;

/// Alpha a dimmed connector keeps, fading edges outside the hovered node's
/// neighborhood without erasing them.
const DIM_EDGE_ALPHA: f32 = 0.2;

/// Everything the canvas draws from, owned so the `'static` paint closure holds
/// no borrow of the view.
pub(crate) struct Scene {
    pub(crate) drawn: Drawn,
    pub(crate) expandable: Expandable,
    pub(crate) palette: Palette,
    pub(crate) hovered: Option<Id>,
    pub(crate) dim_enabled: bool,
    /// The emphasized box and the alpha its highlight paints at.
    pub(crate) highlight: Option<(Id, f32)>,
}

/// The canvas paint pass: box fills parent-before-child so children sit over
/// their containers, then the connectors, then every label on top.
///
/// Labels go last, in a pass of their own, so a name wins over the routes
/// crossing its box; at the finest levels the lanes are dense enough to shred
/// one painted under them. That pass keeps the boxes' outermost-first order,
/// which is the order [`paint_box_label`] arbitrates overlapping titles in.
///
/// The camera is read here, once the prepaint has fitted it, so the level a map
/// draws and the part of it the window shows are both the frame's own. Only what
/// falls inside that part is touched, which is what keeps a level of tens of
/// thousands of boxes paintable.
pub(crate) fn canvas_body(
    scene: Scene,
    entity: Entity<GraphView>,
) -> impl FnOnce(Bounds<Pixels>, (), &mut Window, &mut App) {
    move |bounds, _, window, cx| {
        let camera = entity.read(cx).camera();
        let g = scene.drawn.view(camera.zoom);
        let visible = visible_rect(bounds, camera);
        let dimming = compute_dimming(g, scene.hovered, scene.dim_enabled);
        let drawn: Vec<(PaintBox, Bounds<Pixels>)> = g
            .boxes
            .iter()
            .filter(|b| visible.overlaps(&box_rect(b)))
            .map(|b| {
                let b = PaintBox::of(b, &scene, &dimming);
                let rect = screen_rect(&b, bounds, camera);
                paint_box(&b, rect, camera.zoom, &scene.palette, window);
                (b, rect)
            })
            .collect();
        for c in g.connectors.iter().filter(|c| visible.crossed_by(&c.points)) {
            let dim = dimming.applies && !dimming.lit_edges.contains(&(c.from, c.to));
            paint_lanes(c, dim, bounds, camera, window);
        }
        let mut floated: Vec<Bounds<Pixels>> = Vec::new();
        for (b, rect) in &drawn {
            paint_box_label(b, *rect, camera.zoom, &scene.palette, &mut floated, window, cx);
        }
        paint_scrollbars(bounds, scene.drawn.content(), camera, &scene.palette, window);
        track_gesture(&entity, window, cx);
    }
}

fn box_rect(b: &bviz_graph_layout::Box) -> Rect {
    Rect { x: b.x, y: b.y, w: b.width, h: b.height }
}

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

/// A box resolved against the theme and the frame's lit/dimmed partition, in
/// layout pixels.
struct PaintBox<'a> {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    compound: bool,
    /// Whether this box can be opened or closed, so the renderer draws a
    /// disclosure caret: down when open (compound), right when collapsed.
    expandable: bool,
    /// The themed fill, already faded if this box is dimmed, which the quad and
    /// the plate a label sits on are both painted in.
    fill: Rgba,
    label: &'a str,
    /// Highlight alpha for the just-toggled box (0.0 = none), scaling the
    /// emphasis border and inset glow as it fades.
    highlight: f32,
    /// Outside the hovered node's neighborhood, drawn faded to recede.
    dim: bool,
}

impl<'a> PaintBox<'a> {
    fn of(b: &'a bviz_graph_layout::Box, scene: &Scene, dimming: &Dimming) -> Self {
        let palette = &scene.palette;
        let dim = dimming.applies && !dimming.lit.contains(&b.id);
        let fill = match b.compound {
            true => container_fill(b.kind, palette),
            false => bviz_ui_theme::mix(kind_accent(b.kind), palette.background, KIND_BLEND),
        };
        PaintBox {
            x: b.x,
            y: b.y,
            w: b.width,
            h: b.height,
            compound: b.compound,
            expandable: scene.expandable.holds(b),
            fill: match dim {
                true => bviz_ui_theme::mix(fill, palette.background, DIM_FADE),
                false => fill,
            },
            label: &b.label,
            highlight: match scene.highlight {
                Some((id, alpha)) if id == b.id => alpha,
                _ => 0.0,
            },
            dim,
        }
    }
}

/// Paints a connector's provenances as evenly spaced parallel lanes centered on
/// its route, `dim` fading those outside the hovered node's neighborhood.
fn paint_lanes(
    c: &Connector,
    dim: bool,
    bounds: Bounds<Pixels>,
    camera: Camera,
    window: &mut Window,
) {
    let z = camera.zoom;
    let screen: Vec<(f32, f32)> = c
        .points
        .iter()
        .map(|(x, y)| {
            let s = to_screen(*x, *y, bounds, camera);
            (f32::from(s.x), f32::from(s.y))
        })
        .collect();
    let n = c.kinds.len() as f32;
    for (i, kind) in c.kinds.iter().enumerate() {
        let slot = i as f32 - (n - 1.0) / 2.0;
        let lane = lane_polyline(&screen, slot * PARALLEL_SPACING * z);
        paint_connector(&lane, *kind, dim, z, window);
    }
}

/// Where `b` falls in the window under `camera`.
fn screen_rect(b: &PaintBox, bounds: Bounds<Pixels>, camera: Camera) -> Bounds<Pixels> {
    let origin = to_screen(b.x, b.y, bounds, camera);
    let zoom = camera.zoom;
    Bounds::new(origin, size(px(b.w as f32 * zoom), px(b.h as f32 * zoom)))
}

/// Paints a box's filled quad, with an emphasis border and glow when
/// highlighted.
fn paint_box(
    b: &PaintBox,
    rect: Bounds<Pixels>,
    zoom: f32,
    palette: &Palette,
    window: &mut Window,
) {
    let corner = px(6. * zoom);
    let ((border_w, border_c), glow) = box_emphasis(b, zoom, palette, rect, corner);
    window.paint_quad(quad(rect, corner, b.fill, border_w, border_c, BorderStyle::Solid));
    glow(window);
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

/// Paints a box's label, truncated to the width there is room for, with a
/// disclosure caret for an expandable box. A box too short or too narrow for the
/// floored label is left bare. The font is the window's inherited UI text style
/// so the views stay visually coherent, and a container's title is bold.
///
/// A label normally centers in the strip its own box keeps for it — a
/// container's title band, or the whole box for anything else — which no other
/// box's label reaches into. A container whose band the zoom has shrunk below
/// the floored label instead floats its title to the top of its box, over ground
/// the boxes inside it are drawn on; `floated` collects what those titles have
/// taken, and a label that would land on any of it is dropped rather than
/// printed over it. Since the caller walks the boxes outermost first, that
/// leaves the coarser, more telling name standing.
fn paint_box_label(
    b: &PaintBox,
    rect: Bounds<Pixels>,
    zoom: f32,
    palette: &Palette,
    floated: &mut Vec<Bounds<Pixels>>,
    window: &mut Window,
    cx: &mut App,
) {
    let (w, h) = (f32::from(rect.size.width), f32::from(rect.size.height));
    let fs = (LABEL_SIZE as f32 * zoom).max(LABEL_FLOOR);
    let needed = fs + 2. * LABEL_PAD;
    if h < needed {
        return;
    }
    let band = LABEL_BAND as f32 * zoom;
    let (top_offset, floats) = match b.compound {
        true if band >= needed => ((band - fs) / 2., false),
        true => (LABEL_PAD, true),
        false => ((h - fs) / 2., false),
    };
    // A box that can be opened or closed carries a disclosure caret left of the
    // label; the two are centered together as one unit. The caret keeps the
    // proportion the layout reserved for it whatever size the label is drawn at.
    let caret = b.expandable;
    let footprint = match caret {
        true => fs * (CARET_RESERVE / LABEL_SIZE) as f32,
        false => 0.,
    };
    let avail = w - 2. * LABEL_PAD - footprint;
    if avail < LABEL_MIN_WIDTH {
        return;
    }
    let label_color = if b.dim {
        bviz_ui_theme::mix(palette.label, palette.background, DIM_FADE)
    } else {
        palette.label
    };
    let base = bviz_ui_assets::text_metrics(window).font;
    let font = if b.compound { base.bold() } else { base };
    let line = bviz_ui_components::shape_truncated(
        b.label,
        avail,
        font,
        px(fs),
        label_color.into(),
        window,
    );
    let unit_left = f32::from(rect.origin.x) + (w - footprint - f32::from(line.width())) / 2.;
    let ty = f32::from(rect.origin.y) + top_offset;
    let plate = Bounds::new(
        point(px(unit_left - LABEL_PAD), px(ty - LABEL_PAD)),
        size(px(footprint + f32::from(line.width()) + 2. * LABEL_PAD), px(needed)),
    );
    if floated.iter().any(|taken| taken.intersects(&plate)) {
        return;
    }
    if floats {
        floated.push(plate);
    }
    // The plate is painted in the box's own fill, so a name reads against its box
    // rather than against the routes crossing it. On the box's own ground it is
    // invisible.
    window.paint_quad(quad(
        plate,
        px(0.),
        b.fill,
        px(0.),
        Hsla::transparent_black(),
        BorderStyle::Solid,
    ));
    let _ = line.paint(
        point(px(unit_left + footprint), px(ty)),
        px(fs),
        TextAlign::Left,
        None,
        window,
        cx,
    );

    // An open container (compound) points the caret down; a collapsed box points
    // it right. Phosphor icons render as `svg()` elements and cannot be painted
    // inside this canvas, so the caret is a small filled triangle.
    if caret {
        paint_caret(
            point(px(unit_left), px(ty + fs / 2.)),
            px(footprint),
            px(fs),
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
