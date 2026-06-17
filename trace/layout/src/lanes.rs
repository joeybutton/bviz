use bviz_trace_profile::Profile;

/// Vertical sizing of the stacked lanes, in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LaneMetrics {
    pub header_height: f32,
    pub row_height: f32,
    pub lane_gap: f32,
}

/// A thread track's vertical placement. `depth_rows` slice rows sit below the
/// header band, so the lane spans `[top, top + height)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lane {
    pub track: usize,
    pub top: f32,
    pub height: f32,
    pub depth_rows: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LaneLayout {
    pub lanes: Vec<Lane>,
    pub total_height: f32,
}

pub fn lay_out_lanes(profile: &Profile, metrics: &LaneMetrics) -> LaneLayout {
    let mut lanes = Vec::with_capacity(profile.threads.len());
    let mut top = 0.0;
    for (track, thread) in profile.threads.iter().enumerate() {
        let depth_rows = thread.max_depth.max(1);
        let height = metrics.header_height + depth_rows as f32 * metrics.row_height;
        lanes.push(Lane {
            track,
            top,
            height,
            depth_rows,
        });
        top += height + metrics.lane_gap;
    }
    let total_height = lanes.last().map_or(0.0, |l| l.top + l.height);
    LaneLayout {
        lanes,
        total_height,
    }
}
