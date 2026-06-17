//! A geometry/hit-test/drag-mapping helper for an interactive scroll bar, shared
//! by the views that draw one. It owns no scroll model: the caller keeps its own
//! offset (in pixels, lines, microseconds, or any unit matching the extents it
//! passes) and its own reveal/auto-hide policy, and uses this only to place the
//! thumb, hit-test a press, and map a drag back to an offset.

use gpui::{Background, BorderStyle, Bounds, Hsla, Pixels, Point, Window, point, px, quad, size};

/// The axis a scroll bar runs along: its long dimension and the cursor
/// coordinate a press is read from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Vertical,
    Horizontal,
}

/// A scroll bar's geometry in window coordinates, derived from a track rect and
/// the content/visible extents. Scalars run along the bar's long axis (y for
/// [`Axis::Vertical`], x for [`Axis::Horizontal`]).
#[derive(Clone, Copy)]
pub struct Scrollbar {
    axis: Axis,
    track: Bounds<Pixels>,
    thumb: Bounds<Pixels>,
    /// The bar's short dimension, the thumb's pill corner radius is half this.
    thickness: f32,
    track_start: f32,
    track_len: f32,
    thumb_start: f32,
    thumb_len: f32,
    /// The offset that scrolls to the far end: `content - visible`.
    max_offset: f32,
}

impl Scrollbar {
    /// Build from the `track` rect (window coords) and the content/visible
    /// extents in arbitrary matching units, with the current `offset` in
    /// `[0, content - visible]`. `min_thumb` is the shortest thumb in pixels.
    /// Returns `None` when `content <= visible` (nothing to scroll → no bar).
    pub fn new(
        axis: Axis,
        track: Bounds<Pixels>,
        content: f32,
        visible: f32,
        offset: f32,
        min_thumb: f32,
    ) -> Option<Scrollbar> {
        if content <= visible {
            return None;
        }
        let (track_start, track_len, thickness) = match axis {
            Axis::Vertical => (
                f32::from(track.origin.y),
                f32::from(track.size.height),
                f32::from(track.size.width),
            ),
            Axis::Horizontal => (
                f32::from(track.origin.x),
                f32::from(track.size.width),
                f32::from(track.size.height),
            ),
        };
        let thumb_len = (track_len * (visible / content))
            .max(min_thumb)
            .min(track_len);
        let max_offset = content - visible;
        let travel = track_len - thumb_len;
        let thumb_start = track_start + (offset / max_offset).clamp(0.0, 1.0) * travel;
        let thumb = match axis {
            Axis::Vertical => Bounds::new(
                point(track.origin.x, px(thumb_start)),
                size(px(thickness), px(thumb_len)),
            ),
            Axis::Horizontal => Bounds::new(
                point(px(thumb_start), track.origin.y),
                size(px(thumb_len), px(thickness)),
            ),
        };
        Some(Scrollbar {
            axis,
            track,
            thumb,
            thickness,
            track_start,
            track_len,
            thumb_start,
            thumb_len,
            max_offset,
        })
    }

    pub fn track(&self) -> Bounds<Pixels> {
        self.track
    }

    pub fn thumb(&self) -> Bounds<Pixels> {
        self.thumb
    }

    pub fn track_hit(&self, pos: Point<Pixels>) -> bool {
        self.track.contains(&pos)
    }

    pub fn thumb_hit(&self, pos: Point<Pixels>) -> bool {
        self.thumb.contains(&pos)
    }

    /// Grab offset for a press: the distance from the thumb's leading edge when
    /// the press lands on the thumb, else half the thumb so a track click
    /// centers it on the cursor.
    pub fn grab(&self, pos: Point<Pixels>) -> f32 {
        let along = self.along(pos);
        if along >= self.thumb_start && along <= self.thumb_start + self.thumb_len {
            along - self.thumb_start
        } else {
            self.thumb_len / 2.0
        }
    }

    /// Map a dragged cursor `pos` (with its `grab`) back to an offset, clamped to
    /// `[0, content - visible]`: the inverse of the thumb placement.
    pub fn offset_at(&self, pos: Point<Pixels>, grab: f32) -> f32 {
        let travel = (self.track_len - self.thumb_len).max(1.0);
        let thumb_start = (self.along(pos) - grab - self.track_start).clamp(0.0, travel);
        (thumb_start / travel * self.max_offset).clamp(0.0, self.max_offset)
    }

    /// Paint the track then the rounded thumb with the given colors.
    pub fn paint(
        &self,
        track: impl Into<Background>,
        thumb: impl Into<Background>,
        window: &mut Window,
    ) {
        window.paint_quad(quad(
            self.track,
            px(0.),
            track,
            px(0.),
            Hsla::transparent_black(),
            BorderStyle::Solid,
        ));
        window.paint_quad(quad(
            self.thumb,
            px(self.thickness / 2.0),
            thumb,
            px(0.),
            Hsla::transparent_black(),
            BorderStyle::Solid,
        ));
    }

    /// The along-axis coordinate of `pos` (y for vertical, x for horizontal).
    fn along(&self, pos: Point<Pixels>) -> f32 {
        match self.axis {
            Axis::Vertical => f32::from(pos.y),
            Axis::Horizontal => f32::from(pos.x),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vtrack() -> Bounds<Pixels> {
        Bounds::new(point(px(100.), px(20.)), size(px(10.), px(200.)))
    }

    #[test]
    fn none_when_content_fits() {
        assert!(Scrollbar::new(Axis::Vertical, vtrack(), 100.0, 200.0, 0.0, 24.0).is_none());
        assert!(Scrollbar::new(Axis::Vertical, vtrack(), 200.0, 200.0, 0.0, 24.0).is_none());
    }

    #[test]
    fn thumb_length_and_position() {
        // Half the content is visible, so the thumb is half the 200px track.
        let bar = Scrollbar::new(Axis::Vertical, vtrack(), 400.0, 200.0, 0.0, 24.0).unwrap();
        assert_eq!(f32::from(bar.thumb().size.height), 100.0);
        // At offset 0 the thumb sits at the track top.
        assert_eq!(f32::from(bar.thumb().origin.y), 20.0);
        // At the far end the thumb's bottom meets the track's bottom.
        let bar = Scrollbar::new(Axis::Vertical, vtrack(), 400.0, 200.0, 200.0, 24.0).unwrap();
        assert_eq!(f32::from(bar.thumb().origin.y), 120.0);
    }

    #[test]
    fn min_thumb_floors_a_tiny_fraction() {
        let bar = Scrollbar::new(Axis::Vertical, vtrack(), 10_000.0, 200.0, 0.0, 24.0).unwrap();
        assert_eq!(f32::from(bar.thumb().size.height), 24.0);
    }

    #[test]
    fn offset_at_inverts_the_placement() {
        let max = 200.0_f32;
        for &offset in &[0.0, 37.0, 100.0, max] {
            let bar = Scrollbar::new(Axis::Vertical, vtrack(), 400.0, 200.0, offset, 24.0).unwrap();
            // Pressing the thumb's leading edge (grab 0) then holding it there
            // round-trips the offset through the placement.
            let pos = point(px(105.), bar.thumb().origin.y);
            assert!((bar.offset_at(pos, 0.0) - offset).abs() < 0.01);
        }
    }

    #[test]
    fn horizontal_reads_the_x_coordinate() {
        let track = Bounds::new(point(px(50.), px(300.)), size(px(400.), px(10.)));
        let bar = Scrollbar::new(Axis::Horizontal, track, 800.0, 400.0, 0.0, 24.0).unwrap();
        assert_eq!(f32::from(bar.thumb().size.width), 200.0);
        assert_eq!(f32::from(bar.thumb().origin.x), 50.0);
        let bar = Scrollbar::new(Axis::Horizontal, track, 800.0, 400.0, 400.0, 24.0).unwrap();
        assert_eq!(f32::from(bar.thumb().origin.x), 250.0);
    }

    #[test]
    fn track_and_thumb_hit() {
        let bar = Scrollbar::new(Axis::Vertical, vtrack(), 400.0, 200.0, 0.0, 24.0).unwrap();
        // Inside the track and on the thumb (top half).
        assert!(bar.track_hit(point(px(105.), px(50.))));
        assert!(bar.thumb_hit(point(px(105.), px(50.))));
        // Inside the track but below the thumb.
        assert!(bar.track_hit(point(px(105.), px(180.))));
        assert!(!bar.thumb_hit(point(px(105.), px(180.))));
        // Outside the track entirely.
        assert!(!bar.track_hit(point(px(0.), px(50.))));
    }

    #[test]
    fn track_click_grab_centers_the_thumb() {
        let bar = Scrollbar::new(Axis::Vertical, vtrack(), 400.0, 200.0, 0.0, 24.0).unwrap();
        // A press below the thumb (which spans 20..120 at offset 0) grabs half the
        // thumb so it centers there. y=150 leaves a 100px thumb clear of both track
        // ends (20..220), so the centering is not clamped.
        let pos = point(px(105.), px(150.));
        let grab = bar.grab(pos);
        assert_eq!(grab, f32::from(bar.thumb().size.height) / 2.0);
        // The resulting thumb is centered on the cursor: its center is the press y.
        let offset = bar.offset_at(pos, grab);
        let recentered =
            Scrollbar::new(Axis::Vertical, vtrack(), 400.0, 200.0, offset, 24.0).unwrap();
        let center = f32::from(recentered.thumb().origin.y)
            + f32::from(recentered.thumb().size.height) / 2.0;
        assert!((center - 150.0).abs() < 0.01);
    }
}
