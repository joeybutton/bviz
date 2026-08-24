//! A geometry/hit-test/drag-mapping helper for an interactive scroll bar, shared
//! by the views that draw one. It owns no scroll model: the caller keeps its own
//! offset (in pixels, lines, microseconds, or any unit matching the extents it
//! passes) and its own reveal/auto-hide policy, and uses this only to place the
//! thumb, hit-test a press, and map a drag back to an offset.

use bviz_ui_theme::ThemeColors;
use gpui::{
    Background, BorderStyle, Bounds, Hsla, Pixels, Point, Rgba, Window, point, px, quad, size,
};

/// The axis a scroll bar runs along: its long dimension and the cursor
/// coordinate a press is read from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Vertical,
    Horizontal,
}

impl Axis {
    /// The axis at right angles to this one.
    pub fn cross(self) -> Axis {
        match self {
            Axis::Vertical => Axis::Horizontal,
            Axis::Horizontal => Axis::Vertical,
        }
    }

    /// Project a track rect onto this axis: its leading edge and length along the
    /// long axis, and its thickness across the short axis.
    fn project(self, track: Bounds<Pixels>) -> (f32, f32, f32) {
        match self {
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
        }
    }

    /// A thumb rect from its along-axis start and length plus the cross-axis
    /// thickness, positioned within `track`.
    fn thumb_bounds(
        self,
        track: Bounds<Pixels>,
        thumb_start: f32,
        thumb_len: f32,
        thickness: f32,
    ) -> Bounds<Pixels> {
        match self {
            Axis::Vertical => Bounds::new(
                point(track.origin.x, px(thumb_start)),
                size(px(thickness), px(thumb_len)),
            ),
            Axis::Horizontal => Bounds::new(
                point(px(thumb_start), track.origin.y),
                size(px(thumb_len), px(thickness)),
            ),
        }
    }
}

/// The shortest a thumb is drawn, in pixels, so it stays grabbable when the
/// visible fraction is tiny. Passed as `min_thumb` to [`Scrollbar::new`] by views
/// that have no reason to pick their own.
pub const DEFAULT_MIN_THUMB: f32 = 24.0;

/// Thickness of a scroll bar across its short axis, in pixels: the track's short
/// dimension, and so the strip a view keeps clear along the edge it puts a bar on.
pub const SCROLLBAR_THICKNESS: f32 = 10.0;

/// The fills a scroll bar paints with, so every bar reads the same whichever view
/// draws it. Both are washes, so one pair of values carries a bar over whichever
/// surface the view that owns it lays it on.
#[derive(Clone, Copy)]
pub struct ScrollbarColors {
    pub track: Rgba,
    pub thumb: Rgba,
}

impl ScrollbarColors {
    /// The bar's two parts take the palette's two neutral washes: each registers
    /// on every surface and the heavier stands off the lighter, which is what a
    /// track laid on any surface and a thumb laid on that track have to do.
    pub fn from_colors(colors: &ThemeColors) -> ScrollbarColors {
        ScrollbarColors {
            track: colors.element_hover.into(),
            thumb: colors.element_active.into(),
        }
    }
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
        let (track_start, track_len, thickness) = axis.project(track);
        let thumb_len = (track_len * (visible / content))
            .max(min_thumb)
            .min(track_len);
        let max_offset = content - visible;
        let travel = track_len - thumb_len;
        let thumb_start = track_start + (offset / max_offset).clamp(0.0, 1.0) * travel;
        let thumb = axis.thumb_bounds(track, thumb_start, thumb_len, thickness);
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
    use bviz_ui_theme::{MIN_VISIBLE_STEP, built_in_themes, level_step, over};

    /// A bar lands wherever the view that owns it puts it, so its track has to
    /// part from every surface it can lie on, its thumb from the track under it,
    /// and — for a view that draws the thumb alone — from that surface too.
    #[test]
    fn the_bar_reads_on_every_surface_it_is_drawn_over() {
        for theme in built_in_themes() {
            let colors = &theme.colors;
            let bar = ScrollbarColors::from_colors(colors);
            for surface in [colors.background, colors.surface, colors.panel] {
                let track = over(bar.track, surface);
                let laid = level_step(track, surface);
                assert!(
                    laid >= MIN_VISIBLE_STEP,
                    "{laid} levels of track on its surface"
                );
                let grabbable = level_step(over(bar.thumb, track), track);
                assert!(
                    grabbable >= MIN_VISIBLE_STEP,
                    "{grabbable} levels of thumb on its track"
                );
                let bare = level_step(over(bar.thumb, surface), surface);
                assert!(
                    bare >= MIN_VISIBLE_STEP,
                    "{bare} levels of thumb on a bare surface"
                );
            }
        }
    }

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
