use crate::camera::{Camera, clamp_pan};
use crate::scroll::{ScrollDrag, scroll_drag_move};
use crate::{GraphView, click_box_at};
use bviz_ui_components::{DragEnd, track_drag};
use gpui::{App, Context, Entity, Pixels, Point, Window};

/// A cursor must travel at least this far from the press before the gesture
/// counts as a drag rather than a click.
const CLICK_SLOP: f32 = 4.0;

/// The pointer gesture the view is in the middle of.
#[derive(Clone, Copy, Default)]
pub(crate) enum Drag {
    #[default]
    None,
    Pan(Pan),
    Scrollbar(ScrollDrag),
}

#[derive(Clone, Copy)]
pub(crate) struct Pan {
    /// Cursor position at the press, which travel is measured from.
    press: Point<Pixels>,
    /// Cursor position the next move's pan delta is measured from.
    last: Point<Pixels>,
    /// Latched once the cursor travels past `CLICK_SLOP`.
    moved: bool,
}

/// Begin panning from a press at `position`, repainting so the canvas installs
/// the window-global handlers that carry the gesture past the window edge.
pub(crate) fn start_pan(
    view: &mut GraphView,
    position: Point<Pixels>,
    cx: &mut Context<GraphView>,
) {
    view.drag = Drag::Pan(Pan {
        press: position,
        last: position,
        moved: false,
    });
    cx.notify();
}

/// Pan the graph by the cursor delta during a drag, latching `moved` once the
/// cursor has travelled past the click threshold.
pub(crate) fn drag_pan(view: &mut GraphView, position: Point<Pixels>, cx: &mut Context<GraphView>) {
    let Drag::Pan(mut pan) = view.drag else {
        return;
    };
    let traveled = (f32::from(position.x) - f32::from(pan.press.x))
        .hypot(f32::from(position.y) - f32::from(pan.press.y));
    if traveled > CLICK_SLOP {
        pan.moved = true;
    }
    let Camera {
        zoom,
        pan: (px, py),
    } = view.camera();
    let dx = f32::from(position.x) - f32::from(pan.last.x);
    let dy = f32::from(position.y) - f32::from(pan.last.y);
    let panned = (px + dx, py + dy);
    let panned = match view.drawn().map(|d| d.content()) {
        Some(content) => clamp_pan(panned, zoom, view.framing.viewport().size, content),
        None => panned,
    };
    pan.last = position;
    view.drag = Drag::Pan(pan);
    view.framing.move_to(Camera { zoom, pan: panned });
    cx.notify();
}

/// End whatever gesture is in progress. A pan release that never travelled past
/// the click threshold is a click on the box under the cursor. Taking the gesture
/// out of the view makes a second call a no-op, as both the element-level
/// release handler and the window-global handlers installed during a drag can
/// end one gesture.
pub(crate) fn end_drag(
    view: &mut GraphView,
    position: Point<Pixels>,
    end: DragEnd,
    cx: &mut Context<GraphView>,
) {
    match std::mem::take(&mut view.drag) {
        Drag::None => {}
        Drag::Pan(Pan { moved, .. }) => {
            if !moved && matches!(end, DragEnd::Released) {
                click_box_at(view, position, cx);
            }
        }
        Drag::Scrollbar(_) => cx.notify(),
    }
}

/// Installs the window-global handlers that advance and end whichever gesture is
/// in progress.
pub(crate) fn track_gesture(entity: &Entity<GraphView>, window: &mut Window, cx: &App) {
    match entity.read(cx).drag {
        Drag::None => {}
        Drag::Pan(_) => track_drag(window, entity, drag_pan, end_drag),
        Drag::Scrollbar(_) => track_drag(window, entity, scroll_drag_move, end_drag),
    }
}
