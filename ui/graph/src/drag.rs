use crate::camera::{Camera, clamp_pan};
use crate::scroll::{ScrollDrag, scroll_drag_move};
use crate::{GraphPanel, click_box_at};
use bviz_ui_components::gesture::{
    Button, Clicks, Entry, Gestures, Held, Input, Motion, Place, Pointer,
};
use bviz_ui_components::{DragEnd, track_drag};
use gpui::{App, Context, Entity, MouseDownEvent, Pixels, Point, Window};

/// A cursor must travel at least this far from the press before the gesture
/// counts as a drag rather than a click.
pub(crate) const CLICK_SLOP: f32 = 4.0;

/// Whether a cursor at `at` has travelled past [`CLICK_SLOP`] from a press at
/// `from`.
pub(crate) fn past_slop(from: Point<Pixels>, at: Point<Pixels>) -> bool {
    let travelled =
        (f32::from(at.x) - f32::from(from.x)).hypot(f32::from(at.y) - f32::from(from.y));
    travelled > CLICK_SLOP
}

/// The `click_count` of the press completing a double click.
pub(crate) const DOUBLE_CLICK: usize = 2;

/// What the canvas does for a gesture in [`GESTURES`].
#[derive(Clone, Copy)]
pub(crate) enum CanvasIntent {
    ShowWhole,
    Pan,
    ToggleBox,
    Select,
    Hover,
    Zoom,
    PanAcross,
    Scroll,
    GoBack,
    GoForward,
}

const BOX: Place = Place::On("a box");

pub(crate) const GESTURES: Gestures<CanvasIntent> = Gestures::new(&[
    Entry::new(
        Input::new(Motion::Press(Button::Left, Clicks::Double)),
        Place::Empty,
        "Show whole graph",
        CanvasIntent::ShowWhole,
    ),
    Entry::new(
        Input::drag(Button::Left),
        Place::Anywhere,
        "Pan",
        CanvasIntent::Pan,
    ),
    Entry::new(
        Input::new(Motion::Click(Button::Left, Clicks::Single)),
        BOX,
        "Open or close box",
        CanvasIntent::ToggleBox,
    ),
    Entry::new(
        Input::press(Button::Right),
        BOX,
        "Select or deselect box",
        CanvasIntent::Select,
    ),
    Entry::new(
        Input::HOVER,
        BOX,
        "Describe; highlight neighbours",
        CanvasIntent::Hover,
    ),
    Entry::new(
        Input::SCROLL.holding(Held::SECONDARY),
        Place::Anywhere,
        "Zoom about pointer",
        CanvasIntent::Zoom,
    ),
    Entry::new(
        Input::PINCH,
        Place::Anywhere,
        "Zoom about pointer",
        CanvasIntent::Zoom,
    ),
    Entry::new(
        Input::SCROLL.holding(Held::SHIFT),
        Place::Anywhere,
        "Pan across",
        CanvasIntent::PanAcross,
    ),
    Entry::new(Input::SCROLL, Place::Anywhere, "Pan", CanvasIntent::Scroll),
    Entry::new(
        Input::press(Button::Back),
        Place::Anywhere,
        "Go back",
        CanvasIntent::GoBack,
    ),
    Entry::new(
        Input::press(Button::Forward),
        Place::Anywhere,
        "Go forward",
        CanvasIntent::GoForward,
    ),
]);

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
    /// The click a release short of `CLICK_SLOP` makes of the press.
    click: Pointer,
}

/// Begin panning from `press`, repainting so the frame installs the
/// window-global handlers that carry the gesture past the window edge.
pub(crate) fn start_pan(
    view: &mut GraphPanel,
    press: &MouseDownEvent,
    cx: &mut Context<GraphPanel>,
) {
    view.drag = Drag::Pan(Pan {
        press: press.position,
        last: press.position,
        moved: false,
        click: Pointer::completing(press),
    });
    cx.notify();
}

/// Pan the graph by the cursor delta during a drag, latching `moved` once the
/// cursor has travelled past the click threshold.
pub(crate) fn drag_pan(
    view: &mut GraphPanel,
    position: Point<Pixels>,
    _window: &mut Window,
    cx: &mut Context<GraphPanel>,
) {
    let Drag::Pan(mut pan) = view.drag else {
        return;
    };
    if past_slop(pan.press, position) {
        pan.moved = true;
    }
    if pan.moved {
        view.box_card.drag(position, cx);
    }
    let dx = f32::from(position.x) - f32::from(pan.last.x);
    let dy = f32::from(position.y) - f32::from(pan.last.y);
    pan.last = position;
    view.drag = Drag::Pan(pan);
    pan_by(view, (dx, dy), cx);
}

/// Slides the graph by `(dx, dy)` window px, as far as [`clamp_pan`] lets it.
pub(crate) fn pan_by(view: &mut GraphPanel, (dx, dy): (f32, f32), cx: &mut Context<GraphPanel>) {
    let Camera {
        zoom,
        pan: (px, py),
    } = view.camera();
    let panned = (px + dx, py + dy);
    let panned = match view.drawn().map(|d| d.content()) {
        Some(content) => clamp_pan(panned, zoom, view.framing.viewport().size, content),
        None => panned,
    };
    view.steer(|framing| framing.pan_to(panned), cx);
}

/// End whatever gesture is in progress. A pan release that never travelled past
/// the click threshold is a click on the box under the cursor. Taking the gesture
/// out of the view makes a second call a no-op, as both the element-level
/// release handler and the window-global handlers installed during a drag can
/// end one gesture.
pub(crate) fn end_drag(
    view: &mut GraphPanel,
    position: Point<Pixels>,
    end: DragEnd,
    _: &mut Window,
    cx: &mut Context<GraphPanel>,
) {
    let drag = std::mem::take(&mut view.drag);
    if !matches!(drag, Drag::None) {
        let over = view.framing.viewport().contains(&position);
        match matches!(end, DragEnd::Released) && over {
            true => view.hover_box_at(position, cx),
            false => view.box_card.forget(cx),
        }
    }
    match drag {
        Drag::None => {}
        Drag::Pan(Pan { moved, click, .. }) => {
            if !moved && matches!(end, DragEnd::Released) {
                GESTURES.dispatch(click, |intent| match intent {
                    CanvasIntent::ToggleBox => {
                        click_box_at(view, position, cx);
                        true
                    }
                    _ => false,
                });
            }
        }
        Drag::Scrollbar(_) => cx.notify(),
    }
}

/// Installs the window-global handlers that advance and end whichever gesture is
/// in progress.
pub(crate) fn track_gesture(entity: &Entity<GraphPanel>, window: &mut Window, cx: &App) {
    match entity.read(cx).drag {
        Drag::None => {}
        Drag::Pan(_) => track_drag(window, entity, drag_pan, end_drag),
        Drag::Scrollbar(_) => track_drag(window, entity, scroll_drag_move, end_drag),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_gesture_is_reachable() {
        assert_eq!(GESTURES.unreachable(), []);
    }

    #[test]
    fn every_gesture_fits_the_keys_help() {
        assert_eq!(GESTURES.overlong(), []);
    }

    /// Whether letting go of the `click_count`th press of a run on a box opens
    /// or closes it.
    fn toggles(click_count: usize) -> bool {
        let press = MouseDownEvent {
            button: gpui::MouseButton::Left,
            click_count,
            ..Default::default()
        };
        GESTURES.dispatch(Pointer::completing(&press), |intent| {
            matches!(intent, CanvasIntent::ToggleBox)
        })
    }

    #[test]
    fn a_double_click_on_a_box_toggles_it_once() {
        assert!(toggles(1));
        assert!(!toggles(DOUBLE_CLICK));
    }
}
