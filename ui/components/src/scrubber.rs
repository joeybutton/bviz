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

/// Geometry of the painted scrubber track, in window pixels, kept so a click or
/// drag on it maps to a fraction along the track.
#[derive(Clone, Copy)]
pub struct ScrubberLayout {
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
/// `slot` reaches the [`ScrubberLayout`] the caller keeps in its own state: the
/// painted geometry is written there each layout pass and read back on a press or
/// drag to report the fraction under the cursor to `on_scrub`; `on_release` runs
/// when the drag ends. Pass `active` while a drag is in progress to keep it live
/// past the window edge.
///
/// `slot` runs under a lease on `entity`, so `V` must not already be leased where
/// this element lays out or paints — placing it inside an element whose own
/// prepaint holds `V` open would panic.
pub fn scrubber<V: 'static>(
    id: &'static str,
    fraction: f32,
    colors: ScrubberColors,
    slot: impl Fn(&mut V) -> &mut Option<ScrubberLayout> + 'static,
    entity: Entity<V>,
    active: bool,
    on_scrub: impl Fn(&mut V, f32, &mut Context<V>) + 'static,
    on_release: impl Fn(&mut V, &mut Context<V>) + 'static,
) -> impl IntoElement {
    let ScrubberColors { track, played, knob } = colors;
    let slot = Rc::new(slot);
    let on_scrub = Rc::new(on_scrub);
    let on_release = Rc::new(on_release);

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
                if let Some(geom) = *press_slot(view) {
                    let fraction = geom.fraction_at(f32::from(event.position.x));
                    press_scrub(view, fraction, cx);
                }
            });
        })
        .child(
            canvas(
                move |bounds, _, cx| {
                    stash_entity.update(cx, |view, _| {
                        *stash_slot(view) = Some(ScrubberLayout {
                            left: f32::from(bounds.origin.x),
                            width: f32::from(bounds.size.width),
                        });
                    });
                },
                move |bounds, _, window, _| {
                    paint_scrubber(bounds, fraction, track, played, knob, window);
                    if active {
                        let move_scrub = on_scrub.clone();
                        let move_slot = slot.clone();
                        let up_release = on_release.clone();
                        track_drag(
                            window,
                            &entity,
                            move |view, position, cx| {
                                if let Some(geom) = *move_slot(view) {
                                    let fraction = geom.fraction_at(f32::from(position.x));
                                    move_scrub(view, fraction, cx);
                                }
                            },
                            move |view, _, cx| up_release(view, cx),
                        );
                    }
                },
            )
            .size_full(),
        )
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
    let track_h = 4.0;
    let knob_size = 12.0;
    let played_w = width * fraction.clamp(0.0, 1.0);
    let knob_x = (left + played_w - knob_size / 2.0).clamp(left, left + width - knob_size);

    let bar = |x: f32, w: f32, color: Rgba| {
        fill(
            Bounds::new(
                point(px(x), px(mid - track_h / 2.0)),
                size(px(w), px(track_h)),
            ),
            color,
        )
    };
    window.paint_quad(bar(left, width, track));
    window.paint_quad(bar(left, played_w, played));
    window.paint_quad(fill(
        Bounds::new(
            point(px(knob_x), px(mid - knob_size / 2.0)),
            size(px(knob_size), px(knob_size)),
        ),
        knob,
    ));
}
