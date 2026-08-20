use gpui::{Context, DispatchPhase, Entity, MouseMoveEvent, MouseUpEvent, Pixels, Point, Window};
use std::cell::Cell;
use std::rc::Rc;

/// Install window-global handlers that carry a drag past the dragged element's
/// bounds and past the window's, where an element's own bounds-gated handlers
/// stop: a compositor holds the pointer's implicit grab on the window a press
/// landed in until every button is released, so a position outside the window is
/// ordinary and each caller bounds it for itself. `on_move` follows the cursor while the left
/// button is held. `on_end` runs at most once, on a release or on a move
/// reporting the button let go, which is how a gesture the compositor cut short
/// without a release finishes.
///
/// Call this from an element's `paint` (or a `canvas` paint closure) while the
/// drag is active. The handlers serve every event until the next paint replaces
/// them, which may never come; a latch retires them once the drag has ended.
/// Both callbacks receive the cursor position in window coordinates.
pub fn track_drag<V: 'static>(
    window: &mut Window,
    view: &Entity<V>,
    on_move: impl Fn(&mut V, Point<Pixels>, &mut Context<V>) + 'static,
    on_end: impl Fn(&mut V, Point<Pixels>, &mut Context<V>) + 'static,
) {
    let on_end = Rc::new(on_end);
    let ended = Rc::new(Cell::new(false));

    let moving = view.clone();
    let move_end = on_end.clone();
    let move_ended = ended.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
        if phase != DispatchPhase::Bubble || move_ended.get() {
            return;
        }
        if event.dragging() {
            moving.update(cx, |view, cx| on_move(view, event.position, cx));
        } else {
            move_ended.set(true);
            moving.update(cx, |view, cx| move_end(view, event.position, cx));
        }
    });

    let ending = view.clone();
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if phase != DispatchPhase::Bubble || ended.get() {
            return;
        }
        ended.set(true);
        ending.update(cx, |view, cx| on_end(view, event.position, cx));
    });
}
