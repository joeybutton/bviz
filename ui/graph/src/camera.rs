use crate::emphasis::Emphasis;
use crate::{FIT_MARGIN, MARGIN, MAX_ZOOM, RECENTER_PAD, ZOOM_PER_NOTCH};
use bviz_graph_layout::LaidOut;
use gpui::{Bounds, Pixels, Point, Size, point, px};
use std::ops::RangeInclusive;
use std::time::Instant;

/// A layout-pixel rectangle by origin and size.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Rect {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) w: f64,
    pub(crate) h: f64,
}

impl Rect {
    /// The rect a laid-out box spans.
    pub(crate) fn of(b: &bviz_graph_layout::Box) -> Rect {
        Rect {
            x: b.x,
            y: b.y,
            w: b.width,
            h: b.height,
        }
    }

    /// Whether the two rectangles meet, counting a shared edge.
    pub(crate) fn overlaps(&self, other: &Rect) -> bool {
        self.x <= other.x + other.w
            && other.x <= self.x + self.w
            && self.y <= other.y + other.h
            && other.y <= self.y + self.h
    }

    /// Whether this rectangle covers `(x, y)`, counting its edges.
    pub(crate) fn covers(&self, x: f32, y: f32) -> bool {
        let (x, y) = (x as f64, y as f64);
        self.x <= x && x <= self.x + self.w && self.y <= y && y <= self.y + self.h
    }

    /// Whether a view this size could hold the whole of the polyline `points`,
    /// wherever it happens to be panned.
    ///
    /// The test is against the view's extent rather than its position, so what a
    /// zoom admits does not then come and go as the reader pans across it.
    pub(crate) fn could_hold(&self, points: &[(f64, f64)]) -> bool {
        let (mut left, mut right) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut top, mut bottom) = (f64::INFINITY, f64::NEG_INFINITY);
        for &(x, y) in points {
            left = left.min(x);
            right = right.max(x);
            top = top.min(y);
            bottom = bottom.max(y);
        }
        right - left <= self.w && bottom - top <= self.h
    }

    /// Whether `other` lies wholly within this rectangle, counting a shared edge.
    pub(crate) fn encloses(&self, other: &Rect) -> bool {
        self.x <= other.x
            && self.y <= other.y
            && other.x + other.w <= self.x + self.w
            && other.y + other.h <= self.y + self.h
    }

    /// Whether any leg of the polyline `points` runs through this rectangle.
    pub(crate) fn crossed_by(&self, points: &[(f64, f64)]) -> bool {
        points
            .windows(2)
            .any(|leg| self.crossed_by_leg(leg[0], leg[1]))
    }

    /// Liang–Barsky: the segment reaches the rectangle while the stretch of it
    /// all four edges admit is still non-empty.
    fn crossed_by_leg(&self, from: (f64, f64), to: (f64, f64)) -> bool {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let (mut enter, mut leave) = (0.0f64, 1.0f64);
        let mut admits = |along: f64, room: f64| {
            if along == 0.0 {
                return room >= 0.0;
            }
            let at = room / along;
            if along < 0.0 {
                if at > leave {
                    return false;
                }
                enter = enter.max(at);
            } else {
                if at < enter {
                    return false;
                }
                leave = leave.min(at);
            }
            true
        };
        admits(-dx, from.0 - self.x)
            && admits(dx, self.x + self.w - from.0)
            && admits(-dy, from.1 - self.y)
            && admits(dy, self.y + self.h - from.1)
    }
}

/// The part of layout space `bounds` shows under `camera`, so a paint reaches
/// only the boxes and connectors that fall inside the window.
pub(crate) fn visible_rect(bounds: Bounds<Pixels>, camera: Camera) -> Rect {
    let (x0, y0) = from_screen(bounds.origin, bounds.origin, camera);
    let (x1, y1) = from_screen(bounds.bottom_right(), bounds.origin, camera);
    Rect {
        x: x0 as f64,
        y: y0 as f64,
        w: (x1 - x0) as f64,
        h: (y1 - y0) as f64,
    }
}

/// An in-flight ease of the view from one zoom/pan to another, so a toggle's
/// settle shifts over a few frames instead of jumping. Driven from the canvas
/// prepaint, which re-requests a frame until the ease lands.
#[derive(Clone, Copy)]
pub(crate) struct Settle {
    pub(crate) from_zoom: f32,
    pub(crate) from_pan: (f32, f32),
    pub(crate) to_zoom: f32,
    pub(crate) to_pan: (f32, f32),
    pub(crate) start: Instant,
}

/// The pan/zoom viewport transform: `zoom` scales layout units to pixels and
/// `pan` is the screen offset of the content's near edges.
#[derive(Clone, Copy)]
pub(crate) struct Camera {
    pub(crate) zoom: f32,
    pub(crate) pan: (f32, f32),
}

/// Maps a layout-space point to window space with pan and zoom baked in, atop
/// the same margin offset the boxes use and the canvas's origin within the
/// window.
pub(crate) fn to_screen(x: f64, y: f64, bounds: Bounds<Pixels>, camera: Camera) -> Point<Pixels> {
    let Camera { zoom, pan } = camera;
    point(
        bounds.origin.x + px((x as f32 + MARGIN as f32) * zoom + pan.0),
        bounds.origin.y + px((y as f32 + MARGIN as f32) * zoom + pan.1),
    )
}

/// Inverse of [`to_screen`]: maps a window-space point back to layout space.
pub(crate) fn from_screen(
    screen: Point<Pixels>,
    origin: Point<Pixels>,
    camera: Camera,
) -> (f32, f32) {
    let Camera { zoom, pan } = camera;
    (
        (f32::from(screen.x) - f32::from(origin.x) - pan.0) / zoom - MARGIN as f32,
        (f32::from(screen.y) - f32::from(origin.y) - pan.1) / zoom - MARGIN as f32,
    )
}

/// The largest zoom at which `content` (its full width×height in layout pixels)
/// still fits `viewport`, capped at 1:1 so a small graph is never blown up. This
/// is both the initial zoom and the minimum the scroll handler allows, so the
/// whole graph always fits.
pub(crate) fn fit_zoom(viewport: Size<Pixels>, content: (f32, f32)) -> f32 {
    let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
    let (gw, gh) = content;
    if gw <= 0. || gh <= 0. || vw <= 1. || vh <= 1. {
        return 1.0;
    }
    ((vw / gw).min(vh / gh) * FIT_MARGIN).min(1.0)
}

/// The pan positions one axis admits, from the viewport length `vp` and the
/// content's `extent` along it, both in window px. The content reaches the flush
/// position — fully inside the viewport when smaller than it, fully covering it
/// when larger — and may be carried one clearance past either end. The clearance
/// keeps `FIT_MARGIN` of the shorter of the viewport and the extent on screen:
/// the share a fitted graph itself spans, so a fit or a settle is a pan position
/// in its own right; measured against the extent, so a graph smaller than the
/// viewport keeps that share of itself visible however far it is pushed, and a
/// cursor-anchored [`zoom_step`] keeps play to hold its anchor where the flush
/// interval collapses to a point.
///
/// This is the whole of where the view can pan: a gesture is bounded by it, and a
/// scroll bar's thumb travels over it.
pub(crate) fn pan_range(vp: f32, extent: f32) -> RangeInclusive<f32> {
    let clearance = (1.0 - FIT_MARGIN) * vp.min(extent);
    ((vp - extent).min(0.0) - clearance)..=((vp - extent).max(0.0) + clearance)
}

/// Clamps `pan` into the range each axis admits, so the graph can never be moved
/// out of view — the panning counterpart to the fit-zoom floor that bounds
/// zoom-out.
pub(crate) fn clamp_pan(
    pan: (f32, f32),
    zoom: f32,
    viewport: Size<Pixels>,
    content: (f32, f32),
) -> (f32, f32) {
    let axis = |p: f32, vp: f32, extent: f32| {
        let range = pan_range(vp, extent);
        p.clamp(*range.start(), *range.end())
    };
    (
        axis(pan.0, f32::from(viewport.width), content.0 * zoom),
        axis(pan.1, f32::from(viewport.height), content.1 * zoom),
    )
}

/// The camera a wheel step of `notches` lands on. The zoom scales about
/// `cursor` (a window-space position), holding the layout point under it in
/// place, and spans the whole-graph fit up to [`MAX_ZOOM`]. Where the anchored
/// pan would carry the graph past what [`clamp_pan`] permits, the bound takes
/// over and the anchor slips by exactly the amount it holds back.
pub(crate) fn zoom_step(
    cursor: Point<Pixels>,
    camera: Camera,
    notches: f32,
    viewport: Bounds<Pixels>,
    content: (f32, f32),
) -> Camera {
    let Camera { zoom, pan } = camera;
    let lx = f32::from(cursor.x) - f32::from(viewport.origin.x);
    let ly = f32::from(cursor.y) - f32::from(viewport.origin.y);
    let scaled =
        (zoom * ZOOM_PER_NOTCH.powf(notches)).clamp(fit_zoom(viewport.size, content), MAX_ZOOM);
    let anchored = (
        lx - (lx - pan.0) / zoom * scaled,
        ly - (ly - pan.1) / zoom * scaled,
    );
    Camera {
        zoom: scaled,
        pan: clamp_pan(anchored, scaled, viewport.size, content),
    }
}

/// Ease-out cubic over `t` in `0..=1`: quick to start, decelerating into place,
/// so a settle arrives gently rather than stopping abruptly.
pub(crate) fn ease_out_cubic(t: f32) -> f32 {
    let u = 1.0 - t;
    1.0 - u * u * u
}

/// Smoothstep over `t` in `0..=1`: flat at both ends. What is eased through it
/// leaves and reaches its endpoints gradually rather than at full rate, which a
/// value crossing the threshold of visibility needs at its start as much as at
/// its end.
pub(crate) fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// The zoom and pan that bring `target` — the layout-space rect of the box a
/// toggle just opened or closed — fully into view, disturbing the view as little
/// as that allows.
pub(crate) fn settle_view(
    target: Rect,
    viewport: Size<Pixels>,
    content: (f32, f32),
    zoom: f32,
    pan: (f32, f32),
    ceiling: f32,
) -> (f32, (f32, f32)) {
    let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
    let (bx, by, bw, bh) = (
        target.x as f32,
        target.y as f32,
        target.w as f32,
        target.h as f32,
    );
    let z = ceiling.min(fit_rect_zoom(target, viewport).unwrap_or(ceiling));
    let z = z.clamp(fit_zoom(viewport, content), MAX_ZOOM);
    let (cx, cy) = (bx + bw / 2. + MARGIN as f32, by + bh / 2. + MARGIN as f32);
    let mut panx = cx * zoom + pan.0 - cx * z;
    let mut pany = cy * zoom + pan.1 - cy * z;
    let (sx0, sx1) = (
        (bx + MARGIN as f32) * z + panx,
        (bx + bw + MARGIN as f32) * z + panx,
    );
    let (sy0, sy1) = (
        (by + MARGIN as f32) * z + pany,
        (by + bh + MARGIN as f32) * z + pany,
    );
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

/// The zoom at which `target` spans `viewport` with the fit margin's breathing
/// room around it; `None` for a rect with no extent, which no zoom fills.
fn fit_rect_zoom(target: Rect, viewport: Size<Pixels>) -> Option<f32> {
    let (bw, bh) = (target.w as f32, target.h as f32);
    (bw > 0. && bh > 0.).then(|| {
        let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
        (vw / bw).min(vh / bh) * FIT_MARGIN
    })
}

/// The zoom and pan that fit `content` centered in `viewport`.
pub(crate) fn fit_view(viewport: Size<Pixels>, content: (f32, f32)) -> (f32, (f32, f32)) {
    let zoom = fit_zoom(viewport, content);
    let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
    let (gw, gh) = content;
    (zoom, ((vw - gw * zoom) / 2., (vh - gh * zoom) / 2.))
}

/// Where the emphasized box is drawn in `g`; `None` when nothing is emphasized
/// or the relayout dropped the box.
pub(crate) fn recenter_rect(g: &LaidOut, emphasis: &Emphasis) -> Option<Rect> {
    let id = emphasis.framed()?;
    g.boxes.iter().find(|b| b.id == id).map(Rect::of)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A route reaching further than the view can show cannot be followed to its
    /// far end, and a zoom deep into a dense graph draws mostly those.
    #[test]
    fn a_view_holds_only_a_route_no_larger_than_itself() {
        let view = Rect {
            x: 100.0,
            y: 100.0,
            w: 50.0,
            h: 20.0,
        };
        assert!(view.could_hold(&[(0.0, 0.0), (40.0, 15.0)]));
        assert!(!view.could_hold(&[(0.0, 0.0), (60.0, 15.0)]), "too wide");
        assert!(!view.could_hold(&[(0.0, 0.0), (40.0, 25.0)]), "too tall");
        // A route of exactly the view's own extent still fits it.
        assert!(view.could_hold(&[(0.0, 0.0), (50.0, 20.0)]));
        // What is judged is the route's extent, not where it sits: the same
        // route far outside the view reads the same, since a pan brings it in
        // without the zoom changing.
        assert!(view.could_hold(&[(9000.0, 9000.0), (9040.0, 9015.0)]));
        // A bend counts toward the extent.
        assert!(
            !view.could_hold(&[(0.0, 0.0), (0.0, 30.0), (10.0, 30.0)]),
            "the bend reaches past the view's height",
        );
    }
}
