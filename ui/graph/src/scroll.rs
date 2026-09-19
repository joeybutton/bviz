use crate::camera::{Camera, clamp_pan, pan_range};
use crate::drag::Drag;
use crate::{GraphPanel, Palette};
use bviz_ui_components::{Axis, DEFAULT_MIN_THUMB, SCROLLBAR_THICKNESS, Scrollbar};
use gpui::{Bounds, Context, Pixels, Point, Size, Window, point, px, size};
use std::ops::RangeInclusive;

/// An in-progress scroll-bar drag: which bar holds it and the grab offset down the
/// thumb at the press, so the thumb tracks the cursor from the point it was grabbed.
#[derive(Clone, Copy)]
pub(crate) struct ScrollDrag {
    axis: Axis,
    grab: f32,
}

/// One axis of the view's pan, in window px, and the scroll offset that reads
/// it. The offset counts from the near-most pan [`pan_range`] admits, so it runs
/// `0..=span` as the pan runs over the whole range the bound allows.
#[derive(Clone, Copy)]
struct PanAxis {
    viewport_len: f32,
    content_len: f32,
}

impl PanAxis {
    fn of(axis: Axis, viewport: Size<Pixels>, content: (f32, f32), zoom: f32) -> Self {
        let (viewport_len, content_len) = match axis {
            Axis::Vertical => (f32::from(viewport.height), content.1 * zoom),
            Axis::Horizontal => (f32::from(viewport.width), content.0 * zoom),
        };
        PanAxis {
            viewport_len,
            content_len,
        }
    }

    fn range(&self) -> RangeInclusive<f32> {
        pan_range(self.viewport_len, self.content_len)
    }

    /// How far the pan travels from one end of its range to the other.
    fn span(&self) -> f32 {
        let range = self.range();
        range.end() - range.start()
    }

    /// The offset reading of `pan`.
    fn offset_of(&self, pan: f32) -> f32 {
        self.range().end() - pan
    }

    /// The pan `offset` reads, inverting [`PanAxis::offset_of`].
    fn pan_at(&self, offset: f32) -> f32 {
        self.range().end() - offset
    }

    /// Whether the content spills past the viewport along this axis.
    fn overflows(&self) -> bool {
        self.content_len > self.viewport_len
    }
}

/// The scroll bar for `axis` given the view's current zoom, pan, and `viewport`
/// (window px), or `None` when that axis fits and needs none — an axis whose
/// content the viewport holds carries no bar, though the pan bound still leaves
/// it play around the content.
///
/// The bar's extents are the axis's viewport length and that length plus the
/// whole pan the bound admits, and its offset the [`PanAxis`] reading of the
/// current pan. So the thumb covers exactly the reachable pan: every position it
/// can be dragged to is one the bound keeps, and every pan the bound keeps is one
/// it can be dragged to.
fn scrollbar_geom(
    axis: Axis,
    viewport: Bounds<Pixels>,
    content: (f32, f32),
    camera: Camera,
) -> Option<Scrollbar> {
    let Camera { zoom, pan } = camera;
    let along = PanAxis::of(axis, viewport.size, content, zoom);
    if !along.overflows() {
        return None;
    }
    let across = PanAxis::of(axis.cross(), viewport.size, content, zoom);
    let panc = match axis {
        Axis::Vertical => pan.1,
        Axis::Horizontal => pan.0,
    };
    // Where the crossing axis overflows too, stop the track short of its bar so the
    // two don't overlap in the corner.
    let track_len = along.viewport_len
        - if across.overflows() {
            SCROLLBAR_THICKNESS
        } else {
            0.0
        };
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
    Scrollbar::new(
        axis,
        track,
        along.viewport_len + along.span(),
        along.viewport_len,
        along.offset_of(panc),
        DEFAULT_MIN_THUMB,
    )
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
    for axis in [Axis::Vertical, Axis::Horizontal] {
        if let Some(bar) = scrollbar_geom(axis, viewport, content, camera) {
            bar.paint(palette.scroll.track, palette.scroll.thumb, window);
        }
    }
}

/// The scroll bar for `axis` built from the loaded graph's current view state.
fn current_scrollbar(view: &GraphPanel, axis: Axis) -> Option<Scrollbar> {
    let content = view.drawn()?.content();
    scrollbar_geom(axis, view.framing.viewport(), content, view.camera())
}

/// The axis of the scroll bar whose track a press at `position` (window px) landed
/// on, or `None` when the press is over the content.
pub(crate) fn scrollbar_hit(view: &GraphPanel, position: Point<Pixels>) -> Option<Axis> {
    [Axis::Vertical, Axis::Horizontal]
        .into_iter()
        .find(|&axis| current_scrollbar(view, axis).is_some_and(|bar| bar.track_hit(position)))
}

/// Pan `axis` to where its bar's thumb reports `offset`.
fn scroll_to(view: &mut GraphPanel, axis: Axis, offset: f32, cx: &mut Context<GraphPanel>) {
    let Some(content) = view.drawn().map(|d| d.content()) else {
        return;
    };
    let Camera {
        zoom,
        pan: (px, py),
    } = view.camera();
    let vp = view.framing.viewport().size;
    let panc = PanAxis::of(axis, vp, content, zoom).pan_at(offset);
    let pan = match axis {
        Axis::Vertical => (px, panc),
        Axis::Horizontal => (panc, py),
    };
    view.framing.pan_to(clamp_pan(pan, zoom, vp, content));
    view.invalidate_canvas(cx);
}

/// Begin a scroll-bar drag, recording which thumb is held and where along it the
/// press landed. A press on the bare track jumps the view to the cursor at once;
/// one on the thumb itself picks it up where it stands, leaving the view where
/// the user put it until the cursor moves.
pub(crate) fn start_scroll_drag(
    view: &mut GraphPanel,
    axis: Axis,
    position: Point<Pixels>,
    cx: &mut Context<GraphPanel>,
) {
    let Some(bar) = current_scrollbar(view, axis) else {
        return;
    };
    let grab = bar.grab(position);
    view.drag = Drag::Scrollbar(ScrollDrag { axis, grab });
    if bar.thumb_hit(position) {
        // Moving nothing, the press still needs the frame whose paint installs the
        // window-global handlers that carry the drag.
        cx.notify();
    } else {
        scroll_to(view, axis, bar.offset_at(position, grab), cx);
    }
}

/// Track a held scroll thumb to the cursor.
pub(crate) fn scroll_drag_move(
    view: &mut GraphPanel,
    position: Point<Pixels>,
    _window: &mut Window,
    cx: &mut Context<GraphPanel>,
) {
    let Drag::Scrollbar(drag) = view.drag else {
        return;
    };
    let Some(bar) = current_scrollbar(view, drag.axis) else {
        return;
    };
    scroll_to(view, drag.axis, bar.offset_at(position, drag.grab), cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::point;

    /// A graph well past the viewport on both axes, so either carries a bar.
    const CONTENT: (f32, f32) = (4000.0, 3000.0);

    /// Zooms the bar is checked at, all of them overflowing both axes.
    const ZOOMS: [f32; 4] = [0.25, 0.5, 1.0, 2.0];

    /// Slack in the pan assertions, orders of magnitude below a visible pixel.
    const TOL: f32 = 0.01;

    /// A canvas laid out away from the window origin, so a mapping that neglects
    /// to take the canvas offset off the cursor lands visibly wrong.
    fn viewport() -> Bounds<Pixels> {
        Bounds::new(point(px(10.), px(20.)), size(px(800.), px(600.)))
    }

    /// A full pan with `pan` on `axis` and the crossing axis flush at its near
    /// edge.
    fn full_pan(axis: Axis, pan: f32) -> (f32, f32) {
        match axis {
            Axis::Vertical => (0.0, pan),
            Axis::Horizontal => (pan, 0.0),
        }
    }

    fn along_axis(axis: Axis, zoom: f32) -> PanAxis {
        PanAxis::of(axis, viewport().size, CONTENT, zoom)
    }

    /// The bar `axis` draws with the pan along it at `pan`.
    fn bar(axis: Axis, zoom: f32, pan: f32) -> Scrollbar {
        let camera = Camera {
            zoom,
            pan: full_pan(axis, pan),
        };
        scrollbar_geom(axis, viewport(), CONTENT, camera).expect("an overflowing axis draws a bar")
    }

    /// A cursor `along` window px down the bar's axis, across the middle of its
    /// track.
    fn cursor(axis: Axis, bar: &Scrollbar, along: f32) -> Point<Pixels> {
        let track = bar.track();
        let across = px(SCROLLBAR_THICKNESS / 2.);
        match axis {
            Axis::Vertical => point(track.origin.x + across, px(along)),
            Axis::Horizontal => point(px(along), track.origin.y + across),
        }
    }

    /// Where `rect` starts and ends along `axis`, in window px.
    fn ends(axis: Axis, rect: Bounds<Pixels>) -> (f32, f32) {
        match axis {
            Axis::Vertical => (f32::from(rect.origin.y), f32::from(rect.bottom())),
            Axis::Horizontal => (f32::from(rect.origin.x), f32::from(rect.right())),
        }
    }

    /// The pan a drag of `bar` holding `grab` lands on with the cursor `along`
    /// window px down the axis: what [`scroll_to`] writes, less the framing it
    /// writes it to.
    fn dragged_pan(axis: Axis, zoom: f32, bar: &Scrollbar, along: f32, grab: f32) -> f32 {
        let offset = bar.offset_at(cursor(axis, bar, along), grab);
        along_axis(axis, zoom).pan_at(offset)
    }

    #[test]
    fn an_axis_the_viewport_holds_draws_no_bar() {
        let zoom = 0.1;
        for axis in [Axis::Vertical, Axis::Horizontal] {
            assert!(!along_axis(axis, zoom).overflows());
            let camera = Camera {
                zoom,
                pan: (0.0, 0.0),
            };
            assert!(scrollbar_geom(axis, viewport(), CONTENT, camera).is_none());
        }
    }

    #[test]
    fn dragging_the_thumb_to_a_track_end_reaches_that_end_of_the_pan_bound() {
        for axis in [Axis::Vertical, Axis::Horizontal] {
            for zoom in ZOOMS {
                let pans = along_axis(axis, zoom);
                let range = pans.range();
                // The bound admits pan past the flush positions, so a bar that
                // stopped at those would leave the ends below unreachable.
                let flush = pans.viewport_len - pans.content_len;
                assert!(
                    *range.end() > 0.0 && *range.start() < flush,
                    "no play to reach"
                );

                let bar = bar(axis, zoom, 0.0);
                let (start, end) = ends(axis, bar.track());
                for (along, expected) in [
                    (start - 500., *range.end()),
                    (start, *range.end()),
                    (end, *range.start()),
                    (end + 500., *range.start()),
                ] {
                    let pan = dragged_pan(axis, zoom, &bar, along, 0.0);
                    assert!(
                        (pan - expected).abs() < TOL,
                        "zoom {zoom}: a drag to {along} reached {pan}, not {expected}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_thumb_never_asks_for_a_pan_the_bound_rejects() {
        for axis in [Axis::Vertical, Axis::Horizontal] {
            for zoom in ZOOMS {
                let bar = bar(axis, zoom, 0.0);
                let (start, end) = ends(axis, bar.track());
                let (thumb_start, thumb_end) = ends(axis, bar.thumb());
                let thumb_len = thumb_end - thumb_start;
                for step in 0..=40 {
                    let along = start - 100. + (end - start + 200.) * step as f32 / 40.;
                    for grab in [0.0, thumb_len / 2., thumb_len] {
                        let pan = full_pan(axis, dragged_pan(axis, zoom, &bar, along, grab));
                        let bounded = clamp_pan(pan, zoom, viewport().size, CONTENT);
                        assert!(
                            (bounded.0 - pan.0).abs() < TOL && (bounded.1 - pan.1).abs() < TOL,
                            "zoom {zoom}: a drag to {along} asked for {pan:?}, bounded to {bounded:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn dragging_down_the_track_pans_monotonically() {
        for axis in [Axis::Vertical, Axis::Horizontal] {
            for zoom in ZOOMS {
                let bar = bar(axis, zoom, 0.0);
                let (start, end) = ends(axis, bar.track());
                let mut prev = f32::INFINITY;
                for step in 0..=40 {
                    let along = start + (end - start) * step as f32 / 40.;
                    let pan = dragged_pan(axis, zoom, &bar, along, 0.0);
                    assert!(
                        pan <= prev + TOL,
                        "zoom {zoom}: the pan turned back at {along}"
                    );
                    prev = pan;
                }
            }
        }
    }

    #[test]
    fn pressing_the_thumb_where_it_stands_holds_the_pan() {
        for axis in [Axis::Vertical, Axis::Horizontal] {
            for zoom in ZOOMS {
                let range = along_axis(axis, zoom).range();
                for step in 0..=8 {
                    let pan = range.start() + (range.end() - range.start()) * step as f32 / 8.;
                    let bar = bar(axis, zoom, pan);
                    let (thumb_start, thumb_end) = ends(axis, bar.thumb());
                    // A rect covers its leading edge but not its trailing one, so
                    // the last grip is the pixel inside that edge.
                    for along in [thumb_start, (thumb_start + thumb_end) / 2., thumb_end - 1.] {
                        let at = cursor(axis, &bar, along);
                        assert!(
                            bar.thumb_hit(at),
                            "zoom {zoom}: {along} missed its own thumb"
                        );
                        let held = dragged_pan(axis, zoom, &bar, along, bar.grab(at));
                        assert!(
                            (held - pan).abs() < TOL,
                            "zoom {zoom}: a press at {along} moved the pan {pan} to {held}"
                        );
                    }
                }
            }
        }
    }
}
