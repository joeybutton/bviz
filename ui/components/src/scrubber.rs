//! An interactive playback scrubber element: a track with a played portion and a
//! draggable knob. It owns no timeline model — the caller passes the played
//! `fraction` to draw and receives the fraction under the cursor on press and
//! drag, mapping it to whatever position it keeps.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    Bounds, Context, Entity, MouseButton, MouseDownEvent, Pixels, Rgba, Window, canvas, div, fill,
    point, prelude::*, px, size,
};

use crate::track_drag;

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

/// A scrubber element with id `id`: a full-width track in `track`, a played
/// portion and knob in `played`/`knob` placed at `fraction`. Its geometry is
/// stashed into `layout` each layout pass so a press or drag can report the
/// fraction under the cursor to `on_scrub`; `on_release` runs when the drag ends.
/// Pass `active` while a drag is in progress to keep it live past the window edge.
pub fn scrubber<V: 'static>(
    id: &'static str,
    fraction: f32,
    track: Rgba,
    played: Rgba,
    knob: Rgba,
    layout: Rc<Cell<Option<ScrubberLayout>>>,
    entity: Entity<V>,
    active: bool,
    on_scrub: impl Fn(&mut V, f32, &mut Context<V>) + 'static,
    on_release: impl Fn(&mut V, &mut Context<V>) + 'static,
) -> impl IntoElement {
    let on_scrub = Rc::new(on_scrub);
    let on_release = Rc::new(on_release);

    let press_entity = entity.clone();
    let press_layout = layout.clone();
    let press_scrub = on_scrub.clone();

    let stash = layout.clone();

    div()
        .id(id)
        .flex_1()
        .h_full()
        .on_mouse_down(MouseButton::Left, move |event: &MouseDownEvent, _, cx| {
            if let Some(geom) = press_layout.get() {
                let fraction = geom.fraction_at(f32::from(event.position.x));
                press_entity.update(cx, |view, cx| press_scrub(view, fraction, cx));
            }
        })
        .child(
            canvas(
                move |bounds, _, _| {
                    stash.set(Some(ScrubberLayout {
                        left: f32::from(bounds.origin.x),
                        width: f32::from(bounds.size.width),
                    }));
                },
                move |bounds, _, window, _| {
                    paint_scrubber(bounds, fraction, track, played, knob, window);
                    if active {
                        let move_scrub = on_scrub.clone();
                        let move_layout = layout.clone();
                        let up_release = on_release.clone();
                        track_drag(
                            window,
                            &entity,
                            move |view, event, cx| {
                                if let Some(geom) = move_layout.get() {
                                    let fraction = geom.fraction_at(f32::from(event.position.x));
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
