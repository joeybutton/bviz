use bviz_trace_profile::Profile;

/// Vertical sizing of the stacked lanes, in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LaneMetrics {
    pub header_height: f32,
    pub row_height: f32,
    pub lane_gap: f32,
    /// Height of a counter lane's chart area, below its header band.
    pub counter_height: f32,
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

/// A counter lane's vertical placement; `lane` indexes `Profile::counters`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CounterPos {
    pub lane: usize,
    pub top: f32,
    pub height: f32,
}

/// Thread lanes stacked first, then the counter lanes below them.
#[derive(Debug, Clone, PartialEq)]
pub struct LaneLayout {
    pub lanes: Vec<Lane>,
    pub counters: Vec<CounterPos>,
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
    let counter_height = metrics.header_height + metrics.counter_height;
    let counters = (0..profile.counters.len())
        .map(|lane| {
            let pos = CounterPos {
                lane,
                top,
                height: counter_height,
            };
            top += counter_height + metrics.lane_gap;
            pos
        })
        .collect::<Vec<_>>();
    let total_height = counters
        .last()
        .map(|c| c.top + c.height)
        .or_else(|| lanes.last().map(|l| l.top + l.height))
        .unwrap_or(0.0);
    LaneLayout {
        lanes,
        counters,
        total_height,
    }
}
