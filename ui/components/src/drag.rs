//! A helper for drags that must keep running while the cursor is outside the
//! window.

use gpui::{Context, DispatchPhase, Entity, MouseMoveEvent, MouseUpEvent, Window};

/// Install window-global handlers so an in-progress drag keeps receiving mouse
/// moves and the release even after the cursor leaves the window: Wayland keeps
/// delivering the held button's events to the focused window, so an element's
/// own bounds-gated handlers would otherwise stop at the window edge. Call this
/// each frame from an element's `paint` (or a `canvas` paint closure) while the
/// drag is active; the handlers last for the current frame and are re-installed
/// on the next.
pub fn track_drag<V: 'static>(
    window: &mut Window,
    view: &Entity<V>,
    on_move: impl Fn(&mut V, &MouseMoveEvent, &mut Context<V>) + 'static,
    on_up: impl Fn(&mut V, &MouseUpEvent, &mut Context<V>) + 'static,
) {
    let moving = view.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble {
            moving.update(cx, |view, cx| on_move(view, event, cx));
        }
    });
    let ending = view.clone();
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble {
            ending.update(cx, |view, cx| on_up(view, event, cx));
        }
    });
}
