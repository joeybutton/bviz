//! Reordering chips by dragging a pill within its group.

use super::GraphPanel;
use crate::drag::past_slop;
use bviz_graph_layout::query_lang::{Chip, ChipId};
use bviz_ui_components::{DragEnd, overlay_canvas, track_drag};
use gpui::{Canvas, Context, Pixels, Point, Subscription, Task, Window, point, px};
use std::time::Duration;

/// How near the row's end a dragged pill scrolls the row, how far one step
/// scrolls it, and how often a pointer resting there steps it again.
const EDGE: Pixels = px(24.);
const EDGE_STEP: Pixels = px(12.);
const EDGE_TICK: Duration = Duration::from_millis(30);

/// Where a press on a pill stands. A pill's click lands either side of the
/// release ending the press, so whether the click is the drag's own is read
/// from here whichever comes first.
#[derive(Default)]
pub(super) enum Gesture {
    #[default]
    Idle,
    Pressed(Press),
    /// A drag has been let go, and the click its release makes is yet to
    /// come.
    Dropped,
}

/// A press on a pill, and where carrying it would drop it.
pub(super) struct Press {
    chip: ChipId,
    from: Point<Pixels>,
    /// Where the pointer last stood.
    at: Point<Pixels>,
    /// Whether the press has been carried past the click slop, which makes it
    /// a drag rather than a click.
    carried: bool,
    /// Where the chip lands under the pointer; `None` where the pointer stands
    /// off the row, where letting go moves nothing.
    slot: Option<Slot>,
    /// Steps the row's scroll while the pointer rests near either of its ends.
    _scrolling: Task<()>,
    /// Lets the press go on `escape`.
    _cancel: Subscription,
}

/// Where in its group a dragged chip lands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Slot {
    Before(ChipId),
    End,
}

/// Starts a press on the pill of `chip` at `at`.
pub(super) fn press(
    view: &mut GraphPanel,
    chip: ChipId,
    at: Point<Pixels>,
    window: &Window,
    cx: &mut Context<GraphPanel>,
) {
    let panel = cx.weak_entity();
    let here = window.window_handle();
    let cancel = cx.intercept_keystrokes(move |event, window, cx| {
        // A modified escape is a keystroke of its own, left to what binds it.
        if window.window_handle() != here
            || event.keystroke.key != "escape"
            || event.keystroke.modifiers.modified()
        {
            return;
        }
        let cancelled = panel.update(cx, |view, cx| {
            let pressed = matches!(view.chips.gesture, Gesture::Pressed(_));
            if pressed {
                view.chips.gesture = Gesture::Idle;
                cx.notify();
            }
            pressed
        });
        if cancelled.unwrap_or(false) {
            cx.stop_propagation();
        }
    });
    view.chips.gesture = Gesture::Pressed(Press {
        chip,
        from: at,
        at,
        carried: false,
        slot: None,
        _scrolling: Task::ready(()),
        _cancel: cancel,
    });
    cx.notify();
}

impl Gesture {
    /// Whether a click on a pill is the release of a drag, which acts on
    /// nothing.
    fn takes_click(&mut self) -> bool {
        match self {
            Gesture::Pressed(press) => press.carried,
            Gesture::Dropped => {
                *self = Gesture::Idle;
                true
            }
            Gesture::Idle => false,
        }
    }

    /// Ends the press held, handing it back.
    fn release(&mut self) -> Option<Press> {
        let Gesture::Pressed(press) = std::mem::take(self) else {
            return None;
        };
        if press.carried {
            *self = Gesture::Dropped;
        }
        Some(press)
    }
}

/// Whether a click on a pill is the release of a drag, which acts on nothing.
pub(super) fn swallows_click(view: &mut GraphPanel) -> bool {
    view.chips.gesture.takes_click()
}

/// The press being carried, if one is.
fn carried(view: &GraphPanel) -> Option<&Press> {
    match &view.chips.gesture {
        Gesture::Pressed(press) if press.carried => Some(press),
        _ => None,
    }
}

/// Whether `chip` shows the drop mark ahead of itself, and behind itself as
/// the last of its group.
pub(super) fn marked(view: &GraphPanel, chip: ChipId) -> (bool, bool) {
    let Some(press) = carried(view) else {
        return (false, false);
    };
    let ahead = press.slot == Some(Slot::Before(chip));
    let last = group_of(view, press.chip).last().map(Chip::id);
    let behind = press.slot == Some(Slot::End) && last == Some(chip);
    (ahead, behind)
}

/// The chips of the group `chip` is in, in order.
fn group_of(view: &GraphPanel, chip: ChipId) -> &[Chip] {
    let stack = &view.chips.stack;
    match stack.sources().iter().any(|source| source.id() == chip) {
        true => stack.sources(),
        false => stack.refinements(),
    }
}

/// The window-global handlers carrying a press past its pill, installed for as
/// long as one is held.
pub(super) fn handlers(view: &GraphPanel, cx: &mut Context<GraphPanel>) -> Option<Canvas<()>> {
    if !matches!(view.chips.gesture, Gesture::Pressed(_)) {
        return None;
    }
    let panel = cx.entity();
    Some(overlay_canvas(move |_, window, _| {
        track_drag(
            window,
            &panel,
            |view, at, _, cx| moved(view, at, cx),
            |view, _, end, _, cx| ended(view, end, cx),
        );
    }))
}

fn moved(view: &mut GraphPanel, at: Point<Pixels>, cx: &mut Context<GraphPanel>) {
    let Gesture::Pressed(press) = &mut view.chips.gesture else {
        return;
    };
    press.at = at;
    if !press.carried && !past_slop(press.from, at) {
        return;
    }
    let starts_scrolling = !press.carried || edge_step(view, at).is_none();
    if let Gesture::Pressed(press) = &mut view.chips.gesture {
        press.carried = true;
    }
    if starts_scrolling && edge_step(view, at).is_some() {
        let scrolling = cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor().timer(EDGE_TICK).await;
                let stepped = view.update(cx, step_at_edge);
                if !stepped.unwrap_or(false) {
                    break;
                }
            }
        });
        if let Gesture::Pressed(press) = &mut view.chips.gesture {
            press._scrolling = scrolling;
        }
    }
    aim(view);
    cx.notify();
}

/// Aims the press at the slot under the pointer.
fn aim(view: &mut GraphPanel) {
    let Some(press) = carried(view) else {
        return;
    };
    let slot = slot_at(view, press.chip, press.at);
    if let Gesture::Pressed(press) = &mut view.chips.gesture {
        press.slot = slot;
    }
}

/// Steps the row's scroll towards the end the pointer rests near, and re-aims
/// the press; reports whether it stepped.
fn step_at_edge(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) -> bool {
    let Some(at) = carried(view).map(|press| press.at) else {
        return false;
    };
    let Some(step) = edge_step(view, at) else {
        return false;
    };
    let scroll = &view.chips.scroll;
    let offset = scroll.offset();
    let reach = scroll.max_offset().x;
    let x = (offset.x + step).clamp(-reach, px(0.));
    if x == offset.x {
        return false;
    }
    scroll.set_offset(point(x, offset.y));
    aim(view);
    cx.notify();
    true
}

/// How far a pointer at `at` steps the row's scroll: towards the start near
/// its left end, the end near its right; `None` away from both.
fn edge_step(view: &GraphPanel, at: Point<Pixels>) -> Option<Pixels> {
    let row = view.chips.scroll.bounds();
    if at.y < row.top() || at.y > row.bottom() {
        return None;
    }
    if at.x < row.left() + EDGE {
        Some(EDGE_STEP)
    } else if at.x > row.right() - EDGE {
        Some(-EDGE_STEP)
    } else {
        None
    }
}

fn ended(view: &mut GraphPanel, end: DragEnd, cx: &mut Context<GraphPanel>) {
    let Some(press) = view.chips.gesture.release() else {
        return;
    };
    if let (true, DragEnd::Released, Some(slot)) = (press.carried, end, press.slot) {
        let before = match slot {
            Slot::Before(before) => Some(before),
            Slot::End => None,
        };
        if view.chips.stack.move_before(press.chip, before) {
            view.chips.scroll_to(press.chip);
            super::changed(view, cx);
        }
    }
    cx.notify();
}

/// Where in the group of `chip` a drop at `at` lands it: ahead of the first
/// other chip of the group whose middle lies past `at`, or at the group's end.
/// `None` where `at` lies off the row, or the row has not laid the group out.
fn slot_at(view: &GraphPanel, chip: ChipId, at: Point<Pixels>) -> Option<Slot> {
    let scroll = &view.chips.scroll;
    let row = scroll.bounds();
    if !row.contains(&at) {
        return None;
    }
    let shift = scroll.offset().x;
    let stack = &view.chips.stack;
    for other in group_of(view, chip).iter().map(Chip::id) {
        if other == chip {
            continue;
        }
        let bounds = scroll.bounds_for_item(stack.position(other)?)?;
        let middle = bounds.left() + shift + bounds.size.width / 2.;
        if at.x < middle {
            return Some(Slot::Before(other));
        }
    }
    Some(Slot::End)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pressed(carried: bool) -> Gesture {
        let at = point(px(0.), px(0.));
        Gesture::Pressed(Press {
            chip: bviz_graph_layout::query_lang::Stack::default().push("//a:all"),
            from: at,
            at,
            carried,
            slot: None,
            _scrolling: Task::ready(()),
            _cancel: Subscription::new(|| {}),
        })
    }

    /// A pill's click lands either side of the release ending its drag; it is
    /// the drag's own whichever comes first, and a press carried no distance
    /// clicks.
    #[test]
    fn the_click_a_drag_ends_with_acts_on_nothing_whichever_comes_first() {
        let mut click_first = pressed(true);
        assert!(click_first.takes_click());
        assert!(click_first.release().is_some());

        let mut release_first = pressed(true);
        assert!(release_first.release().is_some());
        assert!(release_first.takes_click());
        assert!(!release_first.takes_click(), "the next click is a click");

        let mut still = pressed(false);
        assert!(!still.takes_click());
        assert!(still.release().is_some());
        assert!(!still.takes_click());
    }
}
