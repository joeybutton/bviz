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
