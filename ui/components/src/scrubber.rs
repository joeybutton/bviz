//! An interactive playback scrubber element: a track with a played portion and a
//! draggable knob. It owns no timeline model — the caller passes the played
//! `fraction` to draw and receives the fraction under the cursor on press and
//! drag, mapping it to whatever position it keeps.

use std::rc::Rc;

use gpui::{
    Bounds, Context, Entity, MouseButton, MouseDownEvent, Pixels, Rgba, Window, canvas, div, fill,
    point, prelude::*, px, size,
};

use crate::track_drag;

/// The scrubber's three fills: the full-width track, the played portion up to
/// the current fraction, and the knob.
#[derive(Clone, Copy)]
pub struct ScrubberColors {
    pub track: Rgba,
    pub played: Rgba,
    pub knob: Rgba,
}

/// What a [`scrubber`] carries between frames. The element is its only writer, so
/// the hold it reports and the geometry that hold is measured against cannot
/// fall out of step.
#[derive(Default)]
pub struct ScrubberState {
    track: Option<ScrubberLayout>,
    held: bool,
}

impl ScrubberState {
    /// A press reports its scrub before taking hold, so this reads false for the
    /// grab and true for the drag moves that follow it.
    pub fn held(&self) -> bool {
        self.held
    }
}

/// Geometry of the painted scrubber track, in window pixels, kept so a click or
/// drag on it maps to a fraction along the track.
#[derive(Clone, Copy)]
struct ScrubberLayout {
    left: f32,
    width: f32,
}

impl ScrubberLayout {
    /// The fraction `0..=1` along the track at window x-coordinate `x`.
    fn fraction_at(&self, x: f32) -> f32 {
        ((x - self.left) / self.width.max(1.0)).clamp(0.0, 1.0)
    }
}

/// A scrubber element with id `id`, filled per `colors`, placed at `fraction`.
/// `slot` reaches this element's [`ScrubberState`] within the caller's own state;
/// the fraction under the cursor on a press or drag goes to `on_scrub`.
///
/// `slot` runs under a lease on `entity`, so `V` must not already be leased where
/// this element lays out or paints — placing it inside an element whose own
/// prepaint holds `V` open would panic.
pub fn scrubber<V: 'static>(
    id: &'static str,
    fraction: f32,
    colors: ScrubberColors,
    slot: impl Fn(&mut V) -> &mut ScrubberState + 'static,
    entity: Entity<V>,
    on_scrub: impl Fn(&mut V, f32, &mut Context<V>) + 'static,
) -> impl IntoElement {
    let ScrubberColors {
        track,
        played,
        knob,
    } = colors;
    let slot = Rc::new(slot);
    let on_scrub = Rc::new(on_scrub);

    let press_entity = entity.clone();
    let press_slot = slot.clone();
    let press_scrub = on_scrub.clone();

    let stash_entity = entity.clone();
    let stash_slot = slot.clone();

    div()
        .id(id)
        .flex_1()
        .h_full()
        .on_mouse_down(MouseButton::Left, move |event: &MouseDownEvent, _, cx| {
            press_entity.update(cx, |view, cx| {
                let Some(geom) = press_slot(view).track else {
                    return;
                };
                let fraction = geom.fraction_at(f32::from(event.position.x));
                press_scrub(view, fraction, cx);
                press_slot(view).held = true;
                cx.notify();
            });
        })
        .child(
            canvas(
                move |bounds, _, cx| {
                    stash_entity.update(cx, |view, _| {
                        let state = stash_slot(view);
                        state.track = Some(ScrubberLayout {
                            left: f32::from(bounds.origin.x),
                            width: f32::from(bounds.size.width),
                        });
                        state.held
                    })
                },
                move |bounds, held, window, _| {
                    paint_scrubber(bounds, fraction, track, played, knob, window);
                    if held {
                        let move_scrub = on_scrub.clone();
                        let move_slot = slot.clone();
                        let end_slot = slot.clone();
                        track_drag(
                            window,
                            &entity,
                            move |view, position, _, cx| {
                                if let Some(geom) = move_slot(view).track {
                                    let fraction = geom.fraction_at(f32::from(position.x));
                                    move_scrub(view, fraction, cx);
                                }
                            },
                            move |view, _, _, _, cx| {
                                end_slot(view).held = false;
                                cx.notify();
                            },
                        );
                    }
                },
            )
            .size_full(),
        )
}

const TRACK_HEIGHT: f32 = 4.0;
const KNOB_SIZE: f32 = 12.0;

/// The knob's left edge for a track `width` wide from `left`, played `played_w`
/// along it: centred on the played end, kept within the track, and against its
/// start when the track is narrower than the knob.
fn knob_left(left: f32, width: f32, played_w: f32) -> f32 {
    let last = (left + width - KNOB_SIZE).max(left);
    (left + played_w - KNOB_SIZE / 2.0).clamp(left, last)
}

/// Paint the scrubber into its bounds: a dim full-width track, a brighter played
/// portion up to `fraction`, and a knob marking the current position.
fn paint_scrubber(
    bounds: Bounds<Pixels>,
    fraction: f32,
    track: Rgba,
    played: Rgba,
    knob: Rgba,
    window: &mut Window,
) {
    let left = f32::from(bounds.left());
    let width = f32::from(bounds.size.width);
    let mid = f32::from(bounds.top()) + f32::from(bounds.size.height) / 2.0;
    let played_w = width * fraction.clamp(0.0, 1.0);
    let knob_x = knob_left(left, width, played_w);

    let bar = |x: f32, w: f32, color: Rgba| {
        fill(
            Bounds::new(
                point(px(x), px(mid - TRACK_HEIGHT / 2.0)),
                size(px(w), px(TRACK_HEIGHT)),
            ),
            color,
        )
    };
    window.paint_quad(bar(left, width, track));
    window.paint_quad(bar(left, played_w, played));
    window.paint_quad(fill(
        Bounds::new(
            point(px(knob_x), px(mid - KNOB_SIZE / 2.0)),
            size(px(KNOB_SIZE), px(KNOB_SIZE)),
        ),
        knob,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_knob_stays_on_its_track() {
        let width = 200.0;
        assert_eq!(knob_left(10.0, width, 0.0), 10.0);
        assert_eq!(
            knob_left(10.0, width, width / 2.0),
            10.0 + width / 2.0 - KNOB_SIZE / 2.0
        );
        assert_eq!(knob_left(10.0, width, width), 10.0 + width - KNOB_SIZE);
    }

    #[test]
    fn a_track_narrower_than_the_knob_holds_it_at_the_start() {
        let width = KNOB_SIZE / 2.0;
        assert_eq!(knob_left(10.0, width, width), 10.0);
        assert_eq!(knob_left(10.0, 0.0, 0.0), 10.0);
    }
}
