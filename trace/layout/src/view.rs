use bviz_trace_profile::TimeSpan;

/// Horizontal mapping between profile time (microseconds) and screen x (pixels).
/// `start_us` is the time at the left edge; `px_per_us` is the zoom factor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeView {
    pub start_us: f64,
    pub px_per_us: f64,
}

impl TimeView {
    /// Scale the whole span to `width_px` less `margin_px` of empty space at each
    /// end, so the timeline's start and end stand clear of the edges. An empty
    /// span stays unzoomable-flat.
    pub fn fit(span: TimeSpan, width_px: f32, margin_px: f32) -> TimeView {
        let span_us = (span.end_us - span.start_us).max(1) as f64;
        let content_px = (width_px - 2.0 * margin_px).max(1.0) as f64;
        let px_per_us = content_px / span_us;
        TimeView {
            start_us: span.start_us as f64 - margin_px as f64 / px_per_us,
            px_per_us,
        }
    }

    pub fn x_of(&self, t_us: i64) -> f32 {
        ((t_us as f64 - self.start_us) * self.px_per_us) as f32
    }

    pub fn t_of(&self, x_px: f32) -> f64 {
        self.start_us + x_px as f64 / self.px_per_us
    }

    /// Zoom by `factor` (>1 zooms in) while keeping the time under `x_px` fixed.
    pub fn zoom_at(&mut self, x_px: f32, factor: f64) {
        let anchor = self.t_of(x_px);
        self.px_per_us *= factor;
        self.start_us = anchor - x_px as f64 / self.px_per_us;
    }

    /// Shift the view by a pixel delta; a positive `dx_px` moves content rightward.
    pub fn pan_px(&mut self, dx_px: f32) {
        self.start_us -= dx_px as f64 / self.px_per_us;
    }

    /// Adjust the view for a content-width change so the visible time range and
    /// the zoom relative to the window are preserved: scale `px_per_us` by the
    /// ratio of the new fit to the old, holding the time at the view's center
    /// fixed. Resizing to any width and back is then a no-op, at every zoom level.
    pub fn rescale_to_width(
        &mut self,
        span: TimeSpan,
        old_width_px: f32,
        new_width_px: f32,
        margin_px: f32,
    ) {
        if old_width_px <= 0.0 || new_width_px <= 0.0 {
            return;
        }
        let old_fit = TimeView::fit(span, old_width_px, margin_px).px_per_us;
        let new_fit = TimeView::fit(span, new_width_px, margin_px).px_per_us;
        if old_fit <= 0.0 {
            return;
        }
        let center_us = self.t_of(old_width_px / 2.0);
        self.px_per_us *= new_fit / old_fit;
        self.start_us = center_us - (new_width_px as f64 / 2.0) / self.px_per_us;
    }

    /// Bound the pan so `span` can never be scrolled off either edge, while
    /// leaving `margin_px` of empty space reachable past each end so the
    /// timeline's start and end can be brought clear of the edges.
    pub fn clamp_to(&mut self, span: TimeSpan, width_px: f32, margin_px: f32) {
        let visible_us = width_px as f64 / self.px_per_us;
        let margin_us = margin_px as f64 / self.px_per_us;
        let low = span.start_us as f64 - margin_us;
        let high = (span.end_us as f64 - visible_us + margin_us).max(low);
        self.start_us = self.start_us.clamp(low, high);
    }
}

/// Horizontal view of the flame graph: a window into its normalized width
/// `[0, 1]` at a zoom factor. Unlike `TimeView` there is no time axis — `zoom`
/// `1.0` fits the whole graph to the content column, and `left` is the fraction
/// of the full width at the window's left edge. Holding the position as a
/// fraction makes a window resize inherently invertible: the same fraction stays
/// in view at any width, so growing then shrinking the window is a no-op.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlameView {
    pub left: f64,
    pub zoom: f64,
}

impl FlameView {
    /// The whole graph fitted to the content column.
    pub const FIT: FlameView = FlameView {
        left: 0.0,
        zoom: 1.0,
    };

    /// Full laid-out width in pixels at this zoom for a content column `width_px`
    /// wide. Lay the tree out at this width so sub-pixel culling tracks the zoom.
    pub fn content_px(&self, width_px: f32) -> f32 {
        (width_px as f64 * self.zoom) as f32
    }

    /// Pixel offset of the window's left edge into the laid-out width — the
    /// horizontal scroll position.
    pub fn scroll_px(&self, width_px: f32) -> f32 {
        (self.left * width_px as f64 * self.zoom) as f32
    }

    /// Laid-out width in pixels, floored at one so a collapsed column can't make
    /// the fraction conversions divide by zero.
    fn span_px(&self, width_px: f32) -> f64 {
        (self.zoom * width_px as f64).max(1.0)
    }

    /// Graph fraction at a content-relative pixel.
    pub fn frac_of(&self, x_px: f32, width_px: f32) -> f64 {
        self.left + x_px as f64 / self.span_px(width_px)
    }

    /// Zoom by `factor` (>1 zooms in) holding the graph fraction under the
    /// content-relative pixel `x_px` fixed; zoom never drops below the fit.
    pub fn zoom_at(&mut self, x_px: f32, width_px: f32, factor: f64) {
        let anchor = self.frac_of(x_px, width_px);
        self.zoom = (self.zoom * factor).max(1.0);
        self.left = anchor - x_px as f64 / self.span_px(width_px);
        self.clamp();
    }

    /// Pan by a pixel delta; a positive `dx_px` moves content rightward.
    pub fn pan_px(&mut self, dx_px: f32, width_px: f32) {
        self.left -= dx_px as f64 / self.span_px(width_px);
        self.clamp();
    }

    /// Set the scroll position from a pixel offset into the laid-out width.
    pub fn set_scroll_px(&mut self, offset_px: f32, width_px: f32) {
        self.left = offset_px as f64 / self.span_px(width_px);
        self.clamp();
    }

    /// Bound zoom to the fit and the window within the graph.
    fn clamp(&mut self) {
        self.zoom = self.zoom.max(1.0);
        let max_left = (1.0 - 1.0 / self.zoom).max(0.0);
        self.left = self.left.clamp(0.0, max_left);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPAN: TimeSpan = TimeSpan {
        start_us: 0,
        end_us: 1_000_000,
    };
    const MARGIN: f32 = 24.0;

    #[test]
    fn rescale_to_width_round_trips() {
        // A zoomed-in, panned view survives a width change and back unchanged.
        let mut v = TimeView::fit(SPAN, 800.0, MARGIN);
        v.zoom_at(400.0, 3.0);
        v.pan_px(-120.0);
        let original = v;
        v.rescale_to_width(SPAN, 800.0, 1280.0, MARGIN);
        v.rescale_to_width(SPAN, 1280.0, 800.0, MARGIN);
        assert!((v.px_per_us - original.px_per_us).abs() < 1e-9);
        assert!((v.start_us - original.start_us).abs() < 1e-6);
    }

    #[test]
    fn rescale_keeps_a_fitted_view_fitted() {
        // Fully zoomed out, a resize leaves the view fitted to the new width
        // rather than spilling over or leaving empty space.
        let mut v = TimeView::fit(SPAN, 800.0, MARGIN);
        v.rescale_to_width(SPAN, 800.0, 1280.0, MARGIN);
        let refit = TimeView::fit(SPAN, 1280.0, MARGIN);
        assert!((v.px_per_us - refit.px_per_us).abs() < 1e-9);
        assert!((v.start_us - refit.start_us).abs() < 1e-6);
    }

    #[test]
    fn rescale_holds_the_center_time() {
        let mut v = TimeView::fit(SPAN, 800.0, MARGIN);
        v.zoom_at(400.0, 4.0);
        let center_before = v.t_of(800.0 / 2.0);
        v.rescale_to_width(SPAN, 800.0, 1280.0, MARGIN);
        let center_after = v.t_of(1280.0 / 2.0);
        assert!((center_before - center_after).abs() < 1e-6);
    }

    #[test]
    fn flame_view_fit_spans_the_content_width() {
        let v = FlameView::FIT;
        assert_eq!(v.content_px(800.0), 800.0);
        assert_eq!(v.scroll_px(800.0), 0.0);
    }

    #[test]
    fn flame_view_zoom_holds_the_fraction_under_the_cursor() {
        let mut v = FlameView::FIT;
        // The fraction under x=600 of an 800px column is 0.75; it stays put.
        let before = v.frac_of(600.0, 800.0);
        v.zoom_at(600.0, 800.0, 4.0);
        let after = v.frac_of(600.0, 800.0);
        assert!((before - after).abs() < 1e-12);
        assert_eq!(v.zoom, 4.0);
        assert_eq!(v.content_px(800.0), 3200.0);
    }

    #[test]
    fn flame_view_zoom_never_drops_below_fit() {
        let mut v = FlameView::FIT;
        v.zoom_at(400.0, 800.0, 0.25);
        assert_eq!(v.zoom, 1.0);
        assert_eq!(v.left, 0.0);
    }

    #[test]
    fn flame_view_position_is_width_independent() {
        // A fraction-based position keeps the same fraction in view across a
        // width change, so a window resize is a no-op.
        let mut v = FlameView::FIT;
        v.zoom_at(600.0, 800.0, 3.0);
        let left = v.left;
        // The visible window is the same fraction whatever the column width.
        assert!((v.frac_of(0.0, 800.0) - left).abs() < 1e-12);
        assert!((v.frac_of(0.0, 1280.0) - left).abs() < 1e-12);
        assert!((v.frac_of(800.0, 800.0) - v.frac_of(1280.0, 1280.0)).abs() < 1e-12);
    }

    #[test]
    fn flame_view_pan_clamps_within_the_graph() {
        let mut v = FlameView::FIT;
        v.zoom_at(0.0, 800.0, 2.0);
        // Pan far past the right edge; left stops at 1 - 1/zoom = 0.5.
        v.pan_px(-10_000.0, 800.0);
        assert!((v.left - 0.5).abs() < 1e-12);
        // And cannot pan before the start.
        v.pan_px(10_000.0, 800.0);
        assert_eq!(v.left, 0.0);
    }

    #[test]
    fn flame_view_scroll_offset_round_trips() {
        let mut v = FlameView::FIT;
        v.zoom_at(400.0, 800.0, 4.0);
        let offset = v.scroll_px(800.0);
        let mut w = FlameView { left: 0.0, zoom: 4.0 };
        w.set_scroll_px(offset, 800.0);
        assert!((w.left - v.left).abs() < 1e-12);
    }
}
