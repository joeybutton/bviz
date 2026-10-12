//! The time view's zoom range and framing, and the lane scroll that reveals a row.

use crate::chart::fit_px_per_us;
use crate::{EDGE_MARGIN_PX, MAX_PX_PER_US, METRICS};
use bviz_trace_layout::{LaneLayout, TimeView};
use bviz_trace_profile::{Profile, SliceAt, TimeSpan};

/// The zoom-out floor, in pixels per microsecond: the fit of the profile's whole
/// `span` to a time column `width_px` wide, capped at [`MAX_PX_PER_US`] so a
/// span too short to fill the column at the deepest zoom stands narrower.
pub(crate) fn zoom_floor(span: TimeSpan, width_px: f32) -> f64 {
    fit_px_per_us(span, width_px, EDGE_MARGIN_PX).min(MAX_PX_PER_US)
}

/// `px_per_us` held between [`zoom_floor`] and [`MAX_PX_PER_US`].
pub(super) fn held_zoom(px_per_us: f64, span: TimeSpan, width_px: f32) -> f64 {
    px_per_us.clamp(zoom_floor(span, width_px), MAX_PX_PER_US)
}

/// Share of the time column left clear on each side of a revealed stretch of
/// time, so what ran around it stays in view.
const REVEAL_MARGIN_SHARE: f32 = 0.1;

/// The view showing `time` across a time column `width_px` wide, clear of its
/// sides by [`REVEAL_MARGIN_SHARE`], held within the zoom range and the pan
/// bounds of the profile's whole `span`. Where the zoom ceiling holds `time`
/// narrower than that, it stands centred.
pub(super) fn framing(time: TimeSpan, span: TimeSpan, width_px: f32) -> TimeView {
    let mut view = TimeView::fit(time, width_px, width_px * REVEAL_MARGIN_SHARE);
    let bounded = held_zoom(view.px_per_us, span, width_px);
    if bounded != view.px_per_us {
        let centre_us = (time.start_us as f64 + time.end_us as f64) / 2.0;
        view = TimeView {
            start_us: centre_us - f64::from(width_px) / 2.0 / bounded,
            px_per_us: bounded,
        };
    }
    view.clamp_to(span, width_px, EDGE_MARGIN_PX);
    view
}

/// A row of slices in the chart, down from the top of the lanes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Row {
    top: f32,
    bottom: f32,
}

impl Row {
    /// The row slices `depth` deep stand in on the lane whose top is
    /// `lane_top`.
    fn of(lane_top: f32, depth: u32) -> Row {
        let top = lane_top + METRICS.header_height + depth as f32 * METRICS.row_height;
        Row {
            top,
            bottom: top + METRICS.row_height,
        }
    }
}

/// The row of the scrolled lanes the first of `slices` drawn there stands in;
/// `None` where `scrolled` draws none of them.
pub(super) fn scrolled_row(
    profile: &Profile,
    scrolled: &LaneLayout,
    slices: &[SliceAt],
) -> Option<Row> {
    slices.iter().find_map(|&at| {
        let lane = scrolled
            .lanes
            .iter()
            .find(|lane| lane.track == at.track())?;
        Some(Row::of(lane.top, profile.slice(at).depth))
    })
}

/// Where to scroll lanes standing `scroll` pixels down the chart in a region
/// `view_h` tall so that `row` shows in full, moving them no further than that
/// takes; its top where it is taller than the region. `None` where it shows in
/// full already.
pub(super) fn scroll_revealing(row: Row, scroll: f32, view_h: f32) -> Option<f32> {
    if row.top >= scroll && row.bottom <= scroll + view_h {
        return None;
    }
    match row.top < scroll || row.bottom - row.top > view_h {
        true => Some(row.top),
        false => Some(row.bottom - view_h),
    }
}

/// Whether the time view pages to keep a playing clock's playhead in sight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Follow {
    Playhead,
    /// The reader moved the view, which stays where they put it until the
    /// playhead is in sight again or a gesture moves the clock.
    Released,
}

impl Follow {
    /// Whether a frame showing the playhead, or not as `in_sight` says, pages
    /// the view to it; a playhead in sight is followed from there on.
    pub(super) fn pages(&mut self, in_sight: bool) -> bool {
        if in_sight {
            *self = Follow::Playhead;
        }
        !in_sight && *self == Follow::Playhead
    }
}

/// Where a page brings the playhead back into view, as a share of the time
/// axis from its left edge: near the start, so it plays on across most of the
/// width before the next page.
pub(super) const PLAYHEAD_FOLLOW_INSET: f32 = 0.1;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::tests::{TEST_TIDS, profile_of, profile_of_events, thread_events};
    use bviz_trace_layout::{Lane, lay_out_lanes};

    /// A profile's whole time, long enough for every framing below to zoom in.
    const WHOLE: TimeSpan = TimeSpan {
        start_us: 0,
        end_us: 1_000_000,
    };

    #[test]
    fn a_revealed_stretch_spans_the_time_column_clear_of_its_sides() {
        let width = 1000.0;
        let time = TimeSpan {
            start_us: 400_000,
            end_us: 410_000,
        };
        let view = framing(time, WHOLE, width);
        let margin = width * REVEAL_MARGIN_SHARE;
        assert!((view.x_of(time.start_us) - margin).abs() < 0.01);
        assert!((view.x_of(time.end_us) - (width - margin)).abs() < 0.01);
    }

    #[test]
    fn a_stretch_too_short_for_the_zoom_ceiling_stands_centred_at_it() {
        let width = 1000.0;
        let instant = TimeSpan {
            start_us: 500_000,
            end_us: 500_000,
        };
        let view = framing(instant, WHOLE, width);
        assert_eq!(view.px_per_us, MAX_PX_PER_US);
        assert!((view.x_of(instant.start_us) - width / 2.0).abs() < 0.01);
    }

    #[test]
    fn an_empty_profile_holds_the_zoom_at_the_ceiling() {
        let width = 1000.0;
        let empty = TimeSpan {
            start_us: 0,
            end_us: 0,
        };
        assert_eq!(zoom_floor(empty, width), MAX_PX_PER_US);
        assert_eq!(held_zoom(0.0, empty, width), MAX_PX_PER_US);
        assert_eq!(framing(empty, empty, width).px_per_us, MAX_PX_PER_US);
    }

    #[test]
    fn a_stretch_at_the_profile_s_start_keeps_the_pan_within_bounds() {
        let width = 1000.0;
        let first = TimeSpan {
            start_us: 0,
            end_us: 10,
        };
        let view = framing(first, WHOLE, width);
        let mut bounded = view;
        bounded.clamp_to(WHOLE, width, EDGE_MARGIN_PX);
        assert_eq!(view, bounded);
    }

    #[test]
    fn lanes_scroll_only_as_far_as_showing_a_row_in_full_takes() {
        let row = |top: f32, height: f32| Row {
            top,
            bottom: top + height,
        };
        // Region 100 tall, scrolled 200 down.
        assert_eq!(scroll_revealing(row(220.0, 30.0), 200.0, 100.0), None);
        assert_eq!(
            scroll_revealing(row(150.0, 30.0), 200.0, 100.0),
            Some(150.0)
        );
        assert_eq!(
            scroll_revealing(row(290.0, 30.0), 200.0, 100.0),
            Some(220.0)
        );
        // Taller than the region, it shows from its top.
        assert_eq!(
            scroll_revealing(row(400.0, 300.0), 200.0, 100.0),
            Some(400.0)
        );
    }

    /// The handle of `profile`'s `slice`th slice on its `track`th track.
    fn slice_at(profile: &Profile, track: usize, slice: usize) -> SliceAt {
        profile.slices_at(track).nth(slice).expect("a slice")
    }

    #[test]
    fn a_reveal_scrolls_to_the_span_s_row_on_a_lane_far_down_a_tall_stack() {
        let profile = profile_of_events(
            TEST_TIDS
                .iter()
                .flat_map(|&tid| {
                    // Nested spans, so the last one stands deep in its lane.
                    thread_events(tid, (0..40).map(|depth| (depth, 1000 - 2 * depth)))
                })
                .collect(),
        );
        let scrolled = lay_out_lanes(&profile, &METRICS);
        let deep = slice_at(&profile, 1, 39);
        let lane = scrolled.lanes[deep.track()];
        let row = scrolled_row(&profile, &scrolled, &[deep]).expect("a drawn row");
        assert_eq!(row, Row::of(lane.top, 39));
        let view_h = 10.0 * METRICS.row_height;
        assert!(lane.height > view_h, "a lane taller than the region");
        let y = scroll_revealing(row, 0.0, view_h).expect("a scroll");
        assert!(row.top >= y && row.bottom <= y + view_h, "the row shows");
        assert_eq!(scroll_revealing(row, y, view_h), None, "and stays put");
    }

    #[test]
    fn a_reveal_scrolls_to_no_row_of_a_lane_the_scrolled_stack_does_not_draw() {
        let profile = profile_of(2);
        let full = lay_out_lanes(&profile, &METRICS);
        // The first track's lane pinned, as the critical path's is, the
        // second's scrolled.
        let scrolled = LaneLayout {
            lanes: vec![Lane {
                top: 0.0,
                ..full.lanes[1]
            }],
            total_height: full.lanes[1].height,
        };
        let own = slice_at(&profile, 0, 1);
        let span = slice_at(&profile, 1, 0);
        assert_eq!(scrolled_row(&profile, &scrolled, &[own]), None);
        assert_eq!(
            scrolled_row(&profile, &scrolled, &[own, span]),
            Some(Row::of(0.0, 0)),
            "the span's row, whichever slice comes first"
        );
    }

    #[test]
    fn a_released_view_follows_the_playhead_again_once_it_is_back_in_sight() {
        let mut follow = Follow::Released;
        assert!(!follow.pages(false));
        assert_eq!(follow, Follow::Released);
        assert!(!follow.pages(true));
        assert_eq!(follow, Follow::Playhead);
        assert!(follow.pages(false));
    }
}
