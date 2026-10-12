use super::status::{BareBadge, STATUS_RIM, StatusBadge, status_half};
use super::{Fills, Marks, Scene, faded, receded};
use crate::Palette;
use crate::camera::{Camera, Rect, to_screen};
use crate::critical::CriticalNodes;
use crate::derive::Expandable;
use crate::status::{Status, Statuses};
use bviz_graph_layout::Id;
use gpui::{BorderStyle, Bounds, Hsla, Pixels, Rgba, Window, point, px, quad, size};

/// Width of the halo around a box standing for a step of the critical path,
/// in layout pixels: wide enough to read as a mark of its own beside the
/// accent border a selection draws.
const CRITICAL_HALO: f32 = 3.0;

/// The narrowest the critical-path halo is drawn on screen, in pixels: it
/// lies outside the box, so a box zoomed out to a few pixels is found by the
/// halo's size rather than its own.
const CRITICAL_HALO_FLOOR: f32 = 4.0;

/// Radius of a box's corners, in layout pixels.
const BOX_CORNER: f32 = 6.0;

/// A box the frame draws: the rect it occupies and the opacity it paints at,
/// both in layout pixels.
pub(super) struct Placed<'a> {
    pub(super) b: &'a bviz_graph_layout::Box,
    pub(super) rect: Rect,
    pub(super) alpha: f32,
    /// On its way out of the drawing, which only the morph still holds it for.
    pub(super) leaving: bool,
}

impl Placed<'_> {
    /// Whether this box draws a disclosure caret. A box on its way out is absent
    /// from the drawing that replaced it, so there is nothing left there to open
    /// or close and it carries none however the layout it is leaving classified
    /// it.
    pub(super) fn expandable(&self, expandable: &Expandable) -> bool {
        !self.leaving && expandable.holds(self.b)
    }
}

/// A box resolved against the theme and the hover's marked set, in layout
/// pixels.
pub(super) struct PaintBox<'a> {
    pub(super) id: Id,
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) w: f64,
    pub(super) h: f64,
    pub(super) compound: bool,
    /// Whether this box can be opened or closed, so the renderer draws a
    /// disclosure caret: down when open (compound), right when collapsed.
    pub(super) expandable: bool,
    /// The themed fill, which the quad and the plate a label sits on are both
    /// painted in.
    pub(super) fill: Rgba,
    pub(super) label: &'a str,
    /// How strongly the box wears the accent border, from 0 for none to 1: full
    /// while it is the selected box, and falling away as the emphasis on a
    /// just-toggled box fades.
    pub(super) highlight: f32,
    /// Whether a marking picks this box out, which the wash over its fill says.
    pub(super) marked: bool,
    /// Opacity the whole box paints at: a morph fades it up from zero for a box
    /// only the new drawing holds and down to zero for one only the old one
    /// held, and a standing selection holds it down for a box its marking leaves
    /// out.
    pub(super) alpha: f32,
    pub(super) status: Option<Status>,
    /// Whether the box stands for a step of the critical path, which a halo
    /// around it says.
    pub(super) critical: bool,
}

impl<'a> PaintBox<'a> {
    pub(super) fn of(
        p: &Placed<'a>,
        scene: &Scene,
        marks: &Marks,
        statuses: &Statuses,
        fills: &Fills,
        critical: Option<&CriticalNodes>,
    ) -> Self {
        let Placed { b, rect, alpha, .. } = *p;
        let marked = marks.marks_box(b.id);
        let alpha = alpha * receded(marks.strength(marked), scene.filter_strength(b.id));
        let fill = fills.fill(b, scene.at, &scene.palette);
        let toggled = match scene.highlight {
            Some((id, alpha)) if id == b.id => alpha,
            _ => 0.0,
        };
        // A selection outlasts the fade, so the box it holds keeps the border at
        // full whatever the emphasis on it has come down to.
        let accent = match scene.selected == Some(b.id) {
            true => 1.0,
            false => toggled,
        };
        PaintBox {
            id: b.id,
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: rect.h,
            compound: b.compound,
            expandable: p.expandable(&scene.expandable),
            fill: faded(fill, alpha),
            label: &b.label,
            highlight: accent,
            marked,
            alpha,
            status: statuses.get(b.id),
            critical: critical.is_some_and(|nodes| nodes.marks(b.id, !b.compound)),
        }
    }

    /// The badge of a box drawn without its label. A container's keeps to
    /// its title band, the rest being ground the boxes it holds are drawn on,
    /// and a leaf's other place is the corner a title floated over the leaf
    /// from the top of its container reaches last.
    pub(super) fn bare_badge(&self, rect: Bounds<Pixels>, zoom: f32) -> Option<BareBadge> {
        let status = self.status?;
        let half = status_half(zoom);
        let reach = px(half + STATUS_RIM);
        let (center, otherwise) = match self.compound {
            true => (rect.origin + point(reach, reach), None),
            false => {
                let corner = point(rect.left() + reach, rect.bottom() - reach);
                let holds = rect.size.width >= reach * 2. && rect.size.height >= reach * 2.;
                (rect.center(), holds.then_some(corner))
            }
        };
        Some(BareBadge {
            badge: StatusBadge {
                center,
                half,
                status,
                alpha: self.alpha,
            },
            otherwise,
        })
    }

    /// The wash a marking lays over this box's fill, transparent where the box
    /// is unmarked.
    pub(super) fn wash(&self, palette: &Palette) -> Rgba {
        palette.highlight_wash(if self.marked { self.alpha } else { 0.0 })
    }
}

/// Where `b` falls in the window under `camera`.
pub(super) fn screen_rect(b: &PaintBox, bounds: Bounds<Pixels>, camera: Camera) -> Bounds<Pixels> {
    let origin = to_screen(b.x, b.y, bounds, camera);
    let zoom = camera.zoom;
    Bounds::new(origin, size(px(b.w as f32 * zoom), px(b.h as f32 * zoom)))
}

/// Paints a box's filled quad, over which a marking's wash goes.
pub(super) fn paint_box(
    b: &PaintBox,
    rect: Bounds<Pixels>,
    zoom: f32,
    palette: &Palette,
    window: &mut Window,
) {
    let corner = px(BOX_CORNER * zoom);
    let ((border_w, border_c), mark) = box_emphasis(b, zoom, palette, rect, corner);
    window.paint_quad(quad(
        rect,
        corner,
        b.fill,
        border_w,
        border_c,
        BorderStyle::Solid,
    ));
    mark(window);
}

/// The halo around a box standing for a step of the critical path, outside
/// the box so that it takes none of the channels a box's own emphases do. It
/// is drawn in the label ink, which every fill keeps a label legible against,
/// so it parts from whatever fill a colouring gives the box. Painted over the
/// routes, which meet the box at its border, and under the labels.
pub(super) fn paint_critical_halo(
    b: &PaintBox,
    rect: Bounds<Pixels>,
    zoom: f32,
    palette: &Palette,
    window: &mut Window,
) {
    let width = px((CRITICAL_HALO * zoom).max(CRITICAL_HALO_FLOOR));
    let around = Bounds {
        origin: rect.origin - point(width, width),
        size: size(rect.size.width + width * 2., rect.size.height + width * 2.),
    };
    window.paint_quad(quad(
        around,
        px(BOX_CORNER * zoom) + width,
        Hsla::transparent_black(),
        width,
        Hsla::from(faded(palette.label, b.alpha)),
        BorderStyle::Solid,
    ));
}

/// A box's emphases, told apart by the channel each takes: the quad border to
/// stroke, which the accent carries from the themed border to itself and from a
/// normal to a thick stroke, and a deferred paint of a marking's wash, which
/// the fill alone carries.
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
        let accent = Rgba {
            a: b.highlight,
            ..palette.highlight
        };
        (
            normal + (thick - normal) * b.highlight,
            Hsla::from(faded(accent, b.alpha)),
        )
    } else {
        (px(1.), Hsla::from(faded(palette.border, b.alpha)))
    };
    let wash = b.wash(palette);
    let mark = move |window: &mut Window| {
        if wash.a > 0.0 {
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
    ((border_w, border_c), mark)
}
