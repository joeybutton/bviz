use crate::emphasis::Emphasis;
use crate::{FIT_MARGIN, MARGIN, MAX_ZOOM, RECENTER_PAD};
use bviz_graph_layout::{Id, LaidOut};
use gpui::{Bounds, Pixels, Point, Size, point, px};
use std::time::Instant;

/// A layout-pixel rectangle by origin and size.
#[derive(Clone, Copy)]
pub(crate) struct Rect {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) w: f64,
    pub(crate) h: f64,
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
pub(crate) fn from_screen(screen: Point<Pixels>, origin: Point<Pixels>, camera: Camera) -> (f32, f32) {
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

/// Clamp the pan so the graph can never be moved out of view, the panning
/// counterpart to the fit-zoom floor that bounds zoom-out. Per axis the content
/// (its full extent times `zoom`) reaches the viewport edge — fully inside when
/// smaller, fully covering when larger — plus the same breathing room the fit zoom
/// leaves: `FIT_MARGIN` holds a zoomed-out graph off the edges, so the pan bound
/// permits an equal clearance rather than snapping the content flush. Without it a
/// settle that frames a node with room to spare jumps the first time the user pans.
pub(crate) fn clamp_pan(pan: (f32, f32), zoom: f32, viewport: Size<Pixels>, content: (f32, f32)) -> (f32, f32) {
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
pub(crate) fn enclosed_boxes(boxes: &[bviz_graph_layout::Box], viewport: Size<Pixels>, camera: Camera) -> Vec<Id> {
    let Camera { zoom, pan } = camera;
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
pub(crate) fn ease_out_cubic(t: f32) -> f32 {
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
pub(crate) fn settle_view(
    target: Rect,
    viewport: Size<Pixels>,
    content: (f32, f32),
    zoom: f32,
    pan: (f32, f32),
) -> (f32, (f32, f32)) {
    let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
    let (bx, by, bw, bh) = (target.x as f32, target.y as f32, target.w as f32, target.h as f32);
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
pub(crate) fn fit_view(viewport: Size<Pixels>, content: (f32, f32)) -> (f32, (f32, f32)) {
    let zoom = fit_zoom(viewport, content);
    let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
    let (gw, gh) = content;
    (zoom, ((vw - gw * zoom) / 2., (vh - gh * zoom) / 2.))
}

/// The graph's full extent in layout pixels: its laid-out size plus `MARGIN` on
/// every side.
pub(crate) fn content_size(g: &LaidOut) -> (f32, f32) {
    (
        g.width as f32 + 2. * MARGIN as f32,
        g.height as f32 + 2. * MARGIN as f32,
    )
}

/// Smallest layout rect covering the boxes the emphasis keeps framed that
/// survived the relayout.
pub(crate) fn recenter_rect(g: &LaidOut, emphasis: &Emphasis) -> Option<Rect> {
    let mut rect: Option<(f64, f64, f64, f64)> = None;
    for bid in emphasis.framed() {
        let Some(b) = g.boxes.iter().find(|b| b.id == bid) else {
            continue;
        };
        let (x0, y0, x1, y1) = (b.x, b.y, b.x + b.width, b.y + b.height);
        rect = Some(match rect {
            None => (x0, y0, x1, y1),
            Some((ax0, ay0, ax1, ay1)) => (ax0.min(x0), ay0.min(y0), ax1.max(x1), ay1.max(y1)),
        });
    }
    rect.map(|(x0, y0, x1, y1)| Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
}
