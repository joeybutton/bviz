use crate::camera::{clamp_pan, content_size, Camera};
use crate::drag::Drag;
use crate::{GraphView, Palette};
use bviz_ui_components::{Axis, Scrollbar};
use gpui::{Bounds, Context, Hsla, MouseMoveEvent, Pixels, Point, Window, point, px, size};

/// Thickness of a scroll-bar track and thumb, in px.
const SCROLLBAR_THICKNESS: f32 = 10.0;

/// Smallest a scroll thumb may shrink to, so it stays grabbable on a long track.
const MIN_THUMB: f32 = 28.0;

/// Fraction by which the border color is blended toward the background to give
/// the scroll-bar track.
const TRACK_BLEND: f32 = 0.5;

/// Fraction by which the label color is blended toward the background to give
/// the scroll thumb.
const THUMB_BLEND: f32 = 0.45;

/// An in-progress scroll-bar drag: which bar holds it and the grab offset down the
/// thumb at the press, so the thumb tracks the cursor from the point it was grabbed.
#[derive(Clone, Copy)]
pub(crate) struct ScrollDrag {
    axis: Axis,
    grab: f32,
}

/// The scroll bar for `axis` given the view's current zoom, pan, and `viewport`
/// (window px), or `None` when that axis fits and needs none. The bar's track,
/// content, visible extent, and offset are the axis length in window px, so a
/// press maps straight through in window coordinates.
fn scrollbar_geom(
    axis: Axis,
    viewport: Bounds<Pixels>,
    content: (f32, f32),
    camera: Camera,
) -> Option<Scrollbar> {
    let Camera { zoom, pan } = camera;
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

/// Paints a scroll bar along the edge of each axis that overflows the viewport,
/// an alternative to drag-panning.
pub(crate) fn paint_scrollbars(
    viewport: Bounds<Pixels>,
    content: (f32, f32),
    camera: Camera,
    palette: &Palette,
    window: &mut Window,
) {
    let track = Hsla::from(bviz_ui_theme::mix(palette.border, palette.background, TRACK_BLEND));
    let thumb = Hsla::from(bviz_ui_theme::mix(palette.label, palette.background, THUMB_BLEND));
    for axis in [Axis::Vertical, Axis::Horizontal] {
        if let Some(bar) = scrollbar_geom(axis, viewport, content, camera) {
            bar.paint(track, thumb, window);
        }
    }
}

/// The scroll bar for `axis` built from the loaded graph's current view state.
fn current_scrollbar(view: &GraphView, axis: Axis) -> Option<Scrollbar> {
    let content = content_size(view.model.laid_out()?);
    scrollbar_geom(axis, view.framing.viewport(), content, view.camera())
}

/// The axis of the scroll bar whose track a press at `position` (window px) landed
/// on, or `None` when the press is over the content.
pub(crate) fn scrollbar_hit(view: &GraphView, position: Point<Pixels>) -> Option<Axis> {
    [Axis::Vertical, Axis::Horizontal].into_iter().find(|&axis| {
        current_scrollbar(view, axis).is_some_and(|bar| bar.track_hit(position))
    })
}

/// Scroll `axis` so `offset` window px of content sit past the near edge, panning
/// the content's near edge to `-offset`.
fn scroll_to(view: &mut GraphView, axis: Axis, offset: f32, cx: &mut Context<GraphView>) {
    let Some(content) = view.model.laid_out().map(content_size) else {
        return;
    };
    let Camera { zoom, pan: (px, py) } = view.camera();
    let vp = view.framing.viewport().size;
    let pan = match axis {
        Axis::Vertical => (px, -offset),
        Axis::Horizontal => (-offset, py),
    };
    view.framing.move_to(Camera { zoom, pan: clamp_pan(pan, zoom, vp, content) });
    cx.notify();
}

/// Begin a scroll-bar drag: record the grabbed thumb and the grab offset, then
/// scroll to the press so a track click jumps the view at once.
pub(crate) fn start_scroll_drag(
    view: &mut GraphView,
    axis: Axis,
    position: Point<Pixels>,
    cx: &mut Context<GraphView>,
) {
    let Some(bar) = current_scrollbar(view, axis) else {
        return;
    };
    let grab = bar.grab(position);
    view.drag = Drag::Scrollbar(ScrollDrag { axis, grab });
    scroll_to(view, axis, bar.offset_at(position, grab), cx);
}

/// Track a held scroll thumb to the cursor.
pub(crate) fn scroll_drag_move(view: &mut GraphView, event: &MouseMoveEvent, cx: &mut Context<GraphView>) {
    let Drag::Scrollbar(drag) = view.drag else {
        return;
    };
    let Some(bar) = current_scrollbar(view, drag.axis) else {
        return;
    };
    scroll_to(view, drag.axis, bar.offset_at(event.position, drag.grab), cx);
}
