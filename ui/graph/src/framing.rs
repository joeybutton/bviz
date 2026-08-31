use crate::camera::{
    Camera, Rect, Settle, clamp_pan, ease_out_cubic, fit_view, fit_zoom, settle_view,
};
use crate::{GraphPanel, MAX_ZOOM, SETTLE_DURATION, SETTLE_MIN_PAN_STEP, SETTLE_MIN_ZOOM_STEP};
use gpui::{App, Bounds, Entity, Pixels, Window};
use std::time::Instant;

/// Where the graph sits in the window, and the ease that carries it from one
/// placement to the next. Every step over it is a function of this value alone,
/// so none of them needs the entity or the window.
pub(crate) struct Framing {
    camera: Camera,
    /// The zoom the reader put the view at, by the fit to the window or by
    /// working the wheel. A settle may relax the camera below this to show a
    /// toggled box that outgrew the window, and comes back to it once a toggle
    /// leaves that box fitting again, so it is the one part of the framing a
    /// settle never writes.
    chosen_zoom: f32,
    /// Whether the fit-to-window view has been established.
    fitted: bool,
    /// Canvas bounds captured at the last prepaint, so hit-testing and the
    /// pan/zoom handlers can work in the same frame the canvas painted.
    viewport: Bounds<Pixels>,
    recenter: bool,
    /// The settle ease in flight, stepped once per frame; `None` at rest.
    anim: Option<Settle>,
}

impl Framing {
    /// A framing not yet fitted to a window.
    pub(crate) fn new() -> Self {
        Framing {
            camera: Camera {
                zoom: 1.0,
                pan: (0.0, 0.0),
            },
            chosen_zoom: 1.0,
            fitted: false,
            viewport: Bounds::default(),
            recenter: false,
            anim: None,
        }
    }

    pub(crate) fn camera(&self) -> Camera {
        self.camera
    }

    pub(crate) fn viewport(&self) -> Bounds<Pixels> {
        self.viewport
    }

    /// Moves the camera to a zoom the reader picked, abandoning any settle in
    /// flight: a manual zoom takes the view over from an ease still running. The
    /// zoom becomes the one settles come back to.
    pub(crate) fn move_to(&mut self, camera: Camera) {
        self.camera = camera;
        self.chosen_zoom = camera.zoom;
        self.anim = None;
    }

    /// Slides the camera across the graph, abandoning any settle in flight. The
    /// zoom a settle returns to is left alone: carrying the view sideways is not
    /// the reader asking to be shown the graph at a different size.
    pub(crate) fn pan_to(&mut self, pan: (f32, f32)) {
        self.camera.pan = pan;
        self.anim = None;
    }

    /// Asks the next prepaint to settle the view onto the reframed content.
    pub(crate) fn request_recenter(&mut self) {
        self.recenter = true;
    }

    /// Drops the established framing, so the next prepaint fits the content
    /// afresh. A layout of a different extent has nothing in common with the
    /// zoom and pan held over from the last one.
    pub(crate) fn refit(&mut self) {
        self.fitted = false;
        self.anim = None;
    }

    /// Arms an ease from where the camera stands onto `(to_zoom, to_pan)`. A move
    /// below the settle steps arms nothing, so a gesture that leaves the view
    /// already framed does not twitch.
    fn ease_to(&mut self, to_zoom: f32, to_pan: (f32, f32)) {
        let (from_zoom, from_pan) = (self.camera.zoom, self.camera.pan);
        let moves = (to_zoom - from_zoom).abs() > SETTLE_MIN_ZOOM_STEP
            || (to_pan.0 - from_pan.0).abs() > SETTLE_MIN_PAN_STEP
            || (to_pan.1 - from_pan.1).abs() > SETTLE_MIN_PAN_STEP;
        if moves {
            self.anim = Some(Settle {
                from_zoom,
                from_pan,
                to_zoom,
                to_pan,
                start: Instant::now(),
            });
        }
    }

    /// Records the canvas bounds and establishes the framing they call for: the
    /// first prepaint fits the graph to the window. Reports whether the camera
    /// moved.
    ///
    /// A later prepaint whose bounds changed size keeps the view stable at any
    /// zoom level instead, holding the graph point under the viewport center
    /// fixed and scaling the zoom with the fit, so the same section stays framed
    /// at the same zoom relative to the window rather than snapping to the fully
    /// fitted view. A settle in flight targeted the old size, so it is dropped
    /// rather than eased toward a stale destination.
    fn fit_or_rescale(&mut self, bounds: Bounds<Pixels>, content: (f32, f32)) -> bool {
        let prev = self.viewport;
        let before = self.camera;
        self.viewport = bounds;
        if !self.fitted {
            let (z, p) = fit_view(bounds.size, content);
            self.camera = Camera { zoom: z, pan: p };
            self.chosen_zoom = z;
            self.fitted = true;
        } else if prev.size != bounds.size {
            self.anim = None;
            let old_fit = fit_zoom(prev.size, content);
            let new_fit = fit_zoom(bounds.size, content);
            let z_old = self.camera.zoom;
            let (pan_x, pan_y) = self.camera.pan;
            let center_x = (f32::from(prev.size.width) / 2. - pan_x) / z_old;
            let center_y = (f32::from(prev.size.height) / 2. - pan_y) / z_old;
            let z = (z_old * new_fit / old_fit).clamp(new_fit, MAX_ZOOM);
            self.camera.zoom = z;
            self.chosen_zoom = (self.chosen_zoom * new_fit / old_fit).clamp(new_fit, MAX_ZOOM);
            self.camera.pan = clamp_pan(
                (
                    f32::from(bounds.size.width) / 2. - center_x * z,
                    f32::from(bounds.size.height) / 2. - center_y * z,
                ),
                z,
                bounds.size,
                content,
            );
        }
        (before.zoom, before.pan) != (self.camera.zoom, self.camera.pan)
    }

    /// Answers a pending recenter request by arming an ease onto `target`; the
    /// request is consumed whether or not one is armed.
    fn arm_settle(&mut self, content: (f32, f32), target: Option<Rect>) {
        if !self.recenter {
            return;
        }
        if let Some(target) = target {
            let (zoom, pan) = (self.camera.zoom, self.camera.pan);
            let (to_zoom, to_pan) = settle_view(
                target,
                self.viewport.size,
                content,
                zoom,
                pan,
                self.chosen_zoom,
            );
            self.ease_to(to_zoom, to_pan);
        }
        self.recenter = false;
    }

    /// Advances an in-flight settle, writing the eased zoom and pan and landing
    /// exactly on the target on the last step. Reports whether a frame is due:
    /// every step of a running ease needs the next one, and the step that lands
    /// needs one more if it moved the camera.
    fn step_settle(&mut self) -> bool {
        let Some(a) = self.anim else {
            return false;
        };
        let before = self.camera;
        let t = (a.start.elapsed().as_secs_f32() / SETTLE_DURATION.as_secs_f32()).clamp(0.0, 1.0);
        let e = ease_out_cubic(t);
        self.camera.zoom = a.from_zoom + (a.to_zoom - a.from_zoom) * e;
        self.camera.pan = (
            a.from_pan.0 + (a.to_pan.0 - a.from_pan.0) * e,
            a.from_pan.1 + (a.to_pan.1 - a.from_pan.1) * e,
        );
        if t < 1.0 {
            return true;
        }
        self.anim = None;
        (before.zoom, before.pan) != (self.camera.zoom, self.camera.pan)
    }
}

/// Steers the view's framing from the bounds the canvas was laid out at. Runs at
/// canvas prepaint because the fit needs the viewport size, known only once laid
/// out.
pub(crate) fn build_prepaint(
    parent: Entity<GraphPanel>,
    content: (f32, f32),
    recenter_target: Option<Rect>,
) -> impl FnOnce(Bounds<Pixels>, &mut Window, &mut App) {
    move |bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App| {
        let redraw = parent.update(cx, |view, _| {
            let moved = view.framing.fit_or_rescale(bounds, content);
            view.framing.arm_settle(content, recenter_target);
            // A moved camera leaves the frame that was rendered from the old one
            // (its status line, its legend) describing a view that is no longer
            // drawn, so ask for one more.
            moved | view.framing.step_settle()
        });
        if redraw {
            window.request_animation_frame();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Size, point, px, size};

    // The whole graph, large enough that fit_zoom's floor stays well below 1.0 and
    // never bites in these cases.
    const CONTENT: (f32, f32) = (4000.0, 3000.0);

    fn vp(w: f32, h: f32) -> Size<Pixels> {
        size(px(w), px(h))
    }

    fn canvas_bounds(w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(0.), px(0.)), vp(w, h))
    }

    /// A framing already fitted to `bounds`, at the given camera and at rest.
    fn framed(zoom: f32, pan: (f32, f32), bounds: Bounds<Pixels>) -> Framing {
        Framing {
            camera: Camera { zoom, pan },
            chosen_zoom: zoom,
            viewport: bounds,
            fitted: true,
            recenter: false,
            anim: None,
        }
    }

    /// The camera the settle tests ease away from: off the identity zoom and
    /// asymmetric across the axes, so a step that swaps the zoom, x and y
    /// components for one another lands outside the expected values.
    const EASE_FROM_ZOOM: f32 = 0.25;
    const EASE_FROM_PAN: (f32, f32) = (-30.0, 70.0);

    fn settle_to(zoom: f32, pan: (f32, f32), start: Instant) -> Settle {
        Settle {
            from_zoom: EASE_FROM_ZOOM,
            from_pan: EASE_FROM_PAN,
            to_zoom: zoom,
            to_pan: pan,
            start,
        }
    }

    #[test]
    fn first_prepaint_fits_and_latches() {
        let mut f = Framing::new();
        f.fit_or_rescale(canvas_bounds(800., 600.), CONTENT);
        let (z, p) = fit_view(vp(800., 600.), CONTENT);
        assert_eq!((f.camera.zoom, f.camera.pan), (z, p));
        assert!(f.fitted);
        assert_eq!(f.viewport.size, vp(800., 600.));
    }

    #[test]
    fn a_second_prepaint_at_the_same_size_holds_the_view() {
        // A hand-panned view, redrawn at the same bounds: no refit, no reframe.
        let bounds = canvas_bounds(800., 600.);
        let mut f = framed(0.4, (-123.0, 45.0), bounds);
        f.anim = Some(settle_to(0.5, (10.0, 20.0), Instant::now()));
        f.fit_or_rescale(bounds, CONTENT);
        assert_eq!((f.camera.zoom, f.camera.pan), (0.4, (-123.0, 45.0)));
        assert!(f.anim.is_some(), "an unrelated repaint dropped the settle");
    }

    #[test]
    fn a_resize_rescales_rather_than_refitting_and_drops_a_settle() {
        // Centered on the content at 3x the 800x600 fit; the viewport then doubles
        // on both axes, so the fit — and with it the zoom — doubles too, and the
        // content stays centered.
        let mut f = framed(0.4, (-400.0, -300.0), canvas_bounds(800., 600.));
        f.anim = Some(settle_to(0.5, (10.0, 20.0), Instant::now()));
        f.fit_or_rescale(canvas_bounds(1600., 1200.), CONTENT);
        assert!((f.camera.zoom - 0.8).abs() < 1e-6, "zoom {}", f.camera.zoom);
        assert!(
            f.camera.zoom > fit_zoom(vp(1600., 1200.), CONTENT),
            "refitted instead of rescaling: {}",
            f.camera.zoom
        );
        assert!(
            (f.camera.pan.0 + 800.0).abs() < 1e-3,
            "pan {:?}",
            f.camera.pan
        );
        assert!(
            (f.camera.pan.1 + 600.0).abs() < 1e-3,
            "pan {:?}",
            f.camera.pan
        );
        assert!(
            f.anim.is_none(),
            "a settle aimed at the old size survived the resize"
        );
    }

    #[test]
    fn arming_below_the_settle_steps_installs_no_ease() {
        // The target already sits comfortably framed, so the move is a no-op.
        let target = Rect {
            x: 375.0,
            y: 280.0,
            w: 50.0,
            h: 40.0,
        };
        let mut f = framed(1.0, (0.0, 0.0), canvas_bounds(800., 600.));
        f.recenter = true;
        f.arm_settle(CONTENT, Some(target));
        assert!(f.anim.is_none());
        assert!(
            !f.recenter,
            "the request outlived the prepaint that answered it"
        );
    }

    #[test]
    fn arming_measures_from_the_readers_zoom_not_from_where_a_settle_left_the_camera() {
        // A camera an earlier settle relaxed to 0.25 to take in content a toggle
        // had spread apart, under a reader's zoom of 1.0 that the settle left
        // alone. A target that fits again brings the view back up to it rather
        // than stranding it at the relaxed zoom.
        let target = Rect {
            x: 375.0,
            y: 280.0,
            w: 50.0,
            h: 40.0,
        };
        let mut f = framed(1.0, (0.0, 0.0), canvas_bounds(800., 600.));
        f.camera.zoom = 0.25;
        f.recenter = true;
        f.arm_settle(CONTENT, Some(target));
        let a = f.anim.expect("no settle armed");
        assert!(
            (a.to_zoom - 1.0).abs() < 1e-4,
            "settled to {}, not back to the reader's 1.0",
            a.to_zoom
        );
        assert_eq!(f.chosen_zoom, 1.0, "the settle wrote the reader's zoom");
    }

    #[test]
    fn arming_on_an_offscreen_target_eases_onto_it() {
        let target = Rect {
            x: 1000.0,
            y: 100.0,
            w: 50.0,
            h: 40.0,
        };
        let mut f = framed(1.0, (0.0, 0.0), canvas_bounds(800., 600.));
        f.recenter = true;
        f.arm_settle(CONTENT, Some(target));
        let (tz, tp) = settle_view(target, vp(800., 600.), CONTENT, 1.0, (0.0, 0.0), 1.0);
        let a = f.anim.expect("no settle armed");
        assert_eq!((a.from_zoom, a.from_pan), (1.0, (0.0, 0.0)));
        assert_eq!((a.to_zoom, a.to_pan), (tz, tp));
        // Arming alone leaves the camera put; the ease moves it.
        assert_eq!((f.camera.zoom, f.camera.pan), (1.0, (0.0, 0.0)));
        assert!(!f.recenter);
    }

    #[test]
    fn arming_without_a_target_consumes_the_request() {
        let mut f = framed(1.0, (0.0, 0.0), canvas_bounds(800., 600.));
        f.recenter = true;
        f.arm_settle(CONTENT, None);
        assert!(f.anim.is_none());
        assert!(!f.recenter);
    }

    #[test]
    fn a_finished_settle_lands_on_the_target_and_asks_for_the_frame_that_shows_it() {
        let mut f = framed(1.0, (0.0, 0.0), canvas_bounds(800., 600.));
        f.anim = Some(settle_to(
            2.0,
            (100.0, -50.0),
            Instant::now() - SETTLE_DURATION,
        ));
        assert!(
            f.step_settle(),
            "the step that moved the camera asked for no frame"
        );
        assert_eq!((f.camera.zoom, f.camera.pan), (2.0, (100.0, -50.0)));
        assert!(f.anim.is_none());
        // Landed and drawn: the ease is over and asks for nothing further.
        assert!(!f.step_settle());
    }

    #[test]
    fn a_settle_landing_where_the_camera_already_stands_asks_for_no_frame() {
        let (zoom, pan) = (EASE_FROM_ZOOM, EASE_FROM_PAN);
        let mut f = framed(zoom, pan, canvas_bounds(800., 600.));
        f.anim = Some(settle_to(zoom, pan, Instant::now() - SETTLE_DURATION));
        assert!(
            !f.step_settle(),
            "a landing that moved nothing asked for a frame"
        );
        assert!(f.anim.is_none());
    }

    #[test]
    fn a_settle_mid_ease_asks_for_another_frame() {
        let (to_zoom, to_pan) = (2.0, (100.0, -50.0));
        let mut f = framed(1.0, (0.0, 0.0), canvas_bounds(800., 600.));
        f.anim = Some(settle_to(
            to_zoom,
            to_pan,
            Instant::now() - SETTLE_DURATION / 2,
        ));
        assert!(f.step_settle());
        assert!(f.anim.is_some());
        // Halfway through in time, so every component sits where the ease curve
        // puts it over 0.4..=0.6 — a band loose enough to absorb the scheduling
        // jitter around that half, tight enough to exclude any other curve.
        let (lo, hi) = (ease_out_cubic(0.4), ease_out_cubic(0.6));
        let eased = |from: f32, to: f32| {
            let (a, b) = (from + (to - from) * lo, from + (to - from) * hi);
            a.min(b)..=a.max(b)
        };
        let (z, (panx, pany)) = (f.camera.zoom, f.camera.pan);
        assert!(
            eased(EASE_FROM_ZOOM, to_zoom).contains(&z),
            "zoom off the ease: {z}"
        );
        assert!(
            eased(EASE_FROM_PAN.0, to_pan.0).contains(&panx),
            "pan x off the ease: {panx}"
        );
        assert!(
            eased(EASE_FROM_PAN.1, to_pan.1).contains(&pany),
            "pan y off the ease: {pany}"
        );
    }

    #[test]
    fn stepping_a_framing_at_rest_asks_for_no_frame() {
        let mut f = framed(0.7, (12.0, -34.0), canvas_bounds(800., 600.));
        assert!(!f.step_settle());
        assert_eq!((f.camera.zoom, f.camera.pan), (0.7, (12.0, -34.0)));
    }

    #[test]
    fn a_prepaint_pass_records_each_step_in_the_framing() {
        // A resize with a recenter pending, driving all three steps: the pass
        // rescales the camera, discards the ease aimed at the old size, arms one
        // onto the target and starts it, so the framing left behind pins what
        // each step wrote.
        let bounds = canvas_bounds(800., 600.);
        let target = Rect {
            x: 2000.0,
            y: 1500.0,
            w: 200.0,
            h: 150.0,
        };
        let mut f = framed(0.6, (-100.0, -200.0), canvas_bounds(400., 300.));
        f.recenter = true;
        f.anim = Some(settle_to(4.0, (1.0, 2.0), Instant::now()));

        f.fit_or_rescale(bounds, CONTENT);
        f.arm_settle(CONTENT, Some(target));
        assert!(
            f.step_settle(),
            "the ease landed on the frame that armed it"
        );

        assert_eq!(f.viewport, bounds);
        assert!(f.fitted);
        assert!(
            !f.recenter,
            "the request outlived the prepaint that answered it"
        );
        let a = f.anim.expect("the recenter armed no ease");
        // The viewport doubles on both axes, so the fit and with it the zoom
        // double, and the point under the old center stays under the new one.
        assert!(
            (a.from_zoom - 1.2).abs() < 1e-5,
            "ease origin zoom {}",
            a.from_zoom
        );
        assert!(
            (a.from_pan.0 + 200.0).abs() < 1e-3,
            "ease origin pan {:?}",
            a.from_pan
        );
        assert!(
            (a.from_pan.1 + 400.0).abs() < 1e-3,
            "ease origin pan {:?}",
            a.from_pan
        );
        assert_eq!(
            (a.to_zoom, a.to_pan),
            settle_view(
                target,
                bounds.size,
                CONTENT,
                a.from_zoom,
                a.from_pan,
                f.chosen_zoom
            )
        );
        let travelled = |v: f32, from: f32, to: f32| (from.min(to)..=from.max(to)).contains(&v);
        assert!(
            travelled(f.camera.pan.0, a.from_pan.0, a.to_pan.0),
            "pan {:?}",
            f.camera.pan
        );
        assert!(
            travelled(f.camera.pan.1, a.from_pan.1, a.to_pan.1),
            "pan {:?}",
            f.camera.pan
        );
    }
}
