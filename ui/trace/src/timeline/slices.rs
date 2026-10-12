//! The culled list of slices the last paint drew, and the slice a pointer hits in it.

use crate::groups::ChartLanes;
use crate::regions::{LaneBand, Regions};
use crate::{METRICS, MIN_SLICE_PX};
use bviz_trace_layout::{Cull, LaneLayout, TimeView, Viewport, VisibleSlice, visible_slices};
use bviz_trace_profile::{Profile, Slice, SliceDetail};
use std::rc::Rc;
use std::sync::Arc;

/// The timeline's culled draw list together with the inputs that determine it,
/// so a repaint reuses it unless one changed. The list turns only on the profile
/// (`Arc` identity), the chart's lanes (`Rc` identity, a fresh one whenever the
/// chart restacks), the time view (pan/zoom), and the viewport of each band of
/// the flame region (width, height, and `scroll_y`); `min_px` and the metrics
/// are consts, so they are not keys.
pub(crate) struct SliceDrawCache {
    profile: Arc<Profile>,
    lanes: Rc<ChartLanes>,
    view: TimeView,
    pinned: Viewport,
    scrolled: Viewport,
    pub(super) slices: Rc<DrawnSlices>,
}

/// The slices each band of the flame region draws, `y` down from that band's
/// top edge.
pub(crate) struct DrawnSlices {
    pinned: Vec<VisibleSlice>,
    scrolled: Vec<VisibleSlice>,
}

impl DrawnSlices {
    /// Each band's slices, alongside the band they are drawn in.
    pub(super) fn in_bands(
        &self,
        regions: Regions,
        scroll: f32,
    ) -> [(LaneBand, &[VisibleSlice]); 2] {
        [
            (regions.pinned_band(), &self.pinned),
            (regions.scrolled_band(scroll), &self.scrolled),
        ]
    }
}

/// A hovered slice paired with the profile its string handles resolve through.
/// Only [`SliceDrawCache::hit`] builds one, from the profile the slice was
/// culled from, so no caller can pair it with a document it never came from.
pub(crate) struct HoveredSlice {
    profile: Arc<Profile>,
    slice: Slice,
    detail: SliceDetail,
    self_us: Option<i64>,
}

impl HoveredSlice {
    pub(crate) fn profile(&self) -> &Profile {
        &self.profile
    }

    pub(crate) fn slice(&self) -> &Slice {
        &self.slice
    }

    pub(crate) fn detail(&self) -> &SliceDetail {
        &self.detail
    }

    /// The part of the span's duration its directly nested spans leave
    /// unaccounted for, floored at zero because a child may overrun its parent
    /// and siblings may overlap. `None` for a span nothing nests in, whose self
    /// time is its whole duration.
    pub(crate) fn self_us(&self) -> Option<i64> {
        self.self_us
    }
}

impl SliceDrawCache {
    /// Every [`VisibleSlice`] the cull emits addresses `profile`'s own threads
    /// and their slice spans, and the cache retains `profile`, so a hit indexes
    /// both without checking.
    fn new(
        profile: Arc<Profile>,
        lanes: Rc<ChartLanes>,
        view: TimeView,
        pinned: Viewport,
        scrolled: Viewport,
    ) -> Self {
        let cull = |layout: &LaneLayout, viewport: &Viewport| {
            visible_slices(
                &profile,
                layout,
                Cull {
                    view: &view,
                    viewport,
                    metrics: &METRICS,
                    min_px: MIN_SLICE_PX,
                },
            )
        };
        let slices = Rc::new(DrawnSlices {
            pinned: cull(&lanes.pinned, &pinned),
            scrolled: cull(&lanes.scrolled, &scrolled),
        });
        Self {
            profile,
            lanes,
            view,
            pinned,
            scrolled,
            slices,
        }
    }

    /// The slice drawn at flame-content-relative `(x, y)`, `y` down from the
    /// top of the pinned row, which the caller has already gated to the flame
    /// region — a point outside it still matches any box it falls in. Boxes may
    /// overlap, and a later one is drawn over an earlier one, so the last match
    /// wins.
    pub(super) fn hit(&self, x: f32, y: f32) -> Option<HoveredSlice> {
        let (slices, y) = match y < self.pinned.height {
            true => (&self.slices.pinned, y),
            false => (&self.slices.scrolled, y - self.pinned.height),
        };
        let vs = slices
            .iter()
            .rev()
            .find(|v| x >= v.x && x < v.x + v.w && y >= v.y && y < v.y + v.h)?;
        let track = &self.profile.threads()[vs.track];
        let slices = self.profile.slices(track);
        let slice = slices[vs.slice];
        // Lane order puts a slice's descendants in the run that follows it, so
        // the entries one level deeper in that run are its direct children.
        let mut nested_us: Option<i64> = None;
        for child in slices[vs.slice + 1..]
            .iter()
            .take_while(|s| s.depth > slice.depth)
            .filter(|s| s.depth == slice.depth + 1)
        {
            *nested_us.get_or_insert(0) += child.dur_us;
        }
        Some(HoveredSlice {
            profile: self.profile.clone(),
            slice,
            detail: self.profile.details(track)[vs.slice],
            self_us: nested_us.map(|nested| (slice.dur_us - nested).max(0)),
        })
    }
}

/// The timeline's culled draw list, reusing the [`SliceDrawCache`] in `cache`
/// unless a key changed, so a repaint that leaves the profile, lanes, time view,
/// and viewports unchanged reuses the list rather than re-culling.
pub(super) fn cached_slices(
    cache: &mut Option<SliceDrawCache>,
    profile: &Arc<Profile>,
    lanes: &Rc<ChartLanes>,
    view: TimeView,
    pinned: Viewport,
    scrolled: Viewport,
) -> Rc<DrawnSlices> {
    if let Some(c) = cache.as_ref()
        && Arc::ptr_eq(&c.profile, profile)
        && Rc::ptr_eq(&c.lanes, lanes)
        && c.view == view
        && c.pinned == pinned
        && c.scrolled == scrolled
    {
        return c.slices.clone();
    }
    cache
        .insert(SliceDrawCache::new(
            profile.clone(),
            lanes.clone(),
            view,
            pinned,
            scrolled,
        ))
        .slices
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slice_tooltip;
    use crate::timeline::tests::{
        TEST_TIDS, TEST_VIEW, profile_of, profile_of_events, thread_events,
    };
    use bviz_invocation::Session;
    use bviz_trace_layout::{Lane, lay_out_lanes};

    const TEST_VIEWPORT: Viewport = Viewport {
        width: 1000.0,
        height: 500.0,
        scroll_y: 0.0,
    };

    /// A profile of one thread whose opening slice is narrower than
    /// [`MIN_SLICE_PX`] at [`TEST_VIEW`]'s zoom: widened to that minimum, its box
    /// reaches over the start of the millisecond slice that abuts it at the same
    /// depth.
    fn profile_of_overlapping_boxes() -> Arc<Profile> {
        let sub_pixel_us = (f64::from(MIN_SLICE_PX) / TEST_VIEW.px_per_us) as i64 - 1;
        profile_of_events(thread_events(
            TEST_TIDS[0],
            [(0, sub_pixel_us), (sub_pixel_us, 1000)],
        ))
    }

    /// A draw list culled from `profile` over a viewport holding all of it,
    /// nothing pinned.
    fn draw_cache(profile: &Arc<Profile>) -> SliceDrawCache {
        let lanes = ChartLanes {
            pinned: LaneLayout {
                lanes: Vec::new(),
                total_height: 0.0,
            },
            scrolled: lay_out_lanes(profile, &METRICS),
        };
        let pinned = Viewport {
            height: 0.0,
            ..TEST_VIEWPORT
        };
        SliceDrawCache::new(
            profile.clone(),
            Rc::new(lanes),
            TEST_VIEW,
            pinned,
            TEST_VIEWPORT,
        )
    }

    #[test]
    fn a_hit_names_the_slice_drawn_there() {
        let profile = profile_of(3);
        let cache = draw_cache(&profile);
        assert_eq!(cache.slices.scrolled.len(), TEST_TIDS.len() * 3);
        for vs in cache.slices.scrolled.iter() {
            let track = &profile.threads()[vs.track];
            let drawn = profile.slices(track)[vs.slice];
            let detail = profile.details(track)[vs.slice];
            let hit = cache
                .hit(vs.x + vs.w / 2.0, vs.y + vs.h / 2.0)
                .expect("the slice at its own center");
            assert_eq!(hit.profile().str(hit.slice().name), profile.str(drawn.name));
            assert_eq!(
                hit.profile().str(hit.detail().category),
                profile.str(detail.category)
            );
        }

        let below_every_box = cache
            .slices
            .scrolled
            .iter()
            .map(|vs| vs.y + vs.h)
            .fold(0.0, f32::max);
        let first = cache.slices.scrolled.first().expect("a culled slice");
        assert!(
            cache
                .hit(first.x + first.w / 2.0, below_every_box)
                .is_none()
        );
    }

    #[test]
    fn a_hit_in_the_pinned_row_names_its_slice_and_never_one_scrolled_under_it() {
        let profile = profile_of(3);
        let full = lay_out_lanes(&profile, &METRICS);
        let stack = |track: usize| LaneLayout {
            lanes: vec![Lane {
                top: 0.0,
                ..full.lanes[track]
            }],
            total_height: full.lanes[track].height,
        };
        let lanes = ChartLanes {
            pinned: stack(0),
            scrolled: stack(1),
        };
        let pinned_h = lanes.heights().pinned;
        let pinned = Viewport {
            height: pinned_h,
            ..TEST_VIEWPORT
        };
        // Scrolled past the lane's header, so its slices reach up out of their
        // band into the pinned row's.
        let scrolled = Viewport {
            scroll_y: METRICS.header_height + 2.0,
            ..TEST_VIEWPORT
        };
        let cache =
            SliceDrawCache::new(profile.clone(), Rc::new(lanes), TEST_VIEW, pinned, scrolled);
        let named_at = |x: f32, y: f32| {
            cache
                .hit(x, y)
                .map(|hit| profile.str(hit.slice().name).to_owned())
        };
        let [pinned_tid, scrolled_tid] = TEST_TIDS;

        let top = cache.slices.pinned.first().expect("a pinned slice");
        assert_eq!(
            named_at(top.x + top.w / 2.0, top.y + top.h / 2.0),
            Some(format!("t{pinned_tid}s0"))
        );
        let under = cache.slices.scrolled.first().expect("a scrolled slice");
        assert!(under.y < 0.0, "the scrolled slice reaches above its band");
        let mid_x = under.x + under.w / 2.0;
        assert_eq!(
            named_at(mid_x, pinned_h + under.y + under.h - 0.5),
            Some(format!("t{scrolled_tid}s0")),
            "below the pinned row the scrolled slice is hit"
        );
        assert_eq!(
            named_at(mid_x, pinned_h - 0.5),
            None,
            "in the pinned row's gap nothing is, whatever is scrolled under it"
        );
    }

    const CONFIGURATION: &str = "55817e2200e2f24b506d68c96928ed7b9fa185d1fb462c4f511796269d8d06d4";

    /// The tooltip of the one slice of a profile recording an action that ran
    /// in [`CONFIGURATION`], hovered in `session`.
    fn action_tooltip(session: Option<&Session>) -> Vec<String> {
        let tid = TEST_TIDS[0];
        let profile = profile_of_events(vec![
            format!(
                r#"{{"name":"thread_name","ph":"M","tid":{tid},"args":{{"name":"skyframe-evaluator-execution-{tid}"}}}}"#,
            ),
            format!(
                r#"{{"cat":"action processing","name":"Compiling a.cc","ph":"X","ts":0,"dur":1000,"tid":{tid},"args":{{"mnemonic":"CppCompile","configuration":"{CONFIGURATION}"}}}}"#,
            ),
        ]);
        let cache = draw_cache(&profile);
        let [drawn] = cache.slices.scrolled[..] else {
            panic!("expected the fixture's one box");
        };
        let hovered = cache
            .hit(drawn.x + drawn.w / 2.0, drawn.y + drawn.h / 2.0)
            .expect("the slice at its own center");
        slice_tooltip(&hovered, session)
    }

    #[test]
    fn a_tooltip_names_the_configuration_the_session_knows() {
        let session = crate::tests::session_knowing(&[(CONFIGURATION, "k8-fastbuild")]);
        assert_eq!(
            action_tooltip(Some(&session)),
            [
                "Compiling a.cc",
                "action processing · 1.00ms",
                "CppCompile · k8-fastbuild",
            ]
        );
    }

    #[test]
    fn a_tooltip_shows_the_checksum_of_a_configuration_no_session_names() {
        let by_checksum = format!("CppCompile · {CONFIGURATION}");
        let unread = Session::default();
        let elsewhere = crate::tests::session_knowing(&[("0c3d9a7714be", "k8-opt-exec")]);
        for session in [None, Some(&unread), Some(&elsewhere)] {
            let lines = action_tooltip(session);
            assert_eq!(lines.last(), Some(&by_checksum));
        }
    }

    #[test]
    fn a_hit_where_boxes_overlap_names_the_one_drawn_last() {
        let profile = profile_of_overlapping_boxes();
        let cache = draw_cache(&profile);
        let [under, over] = cache.slices.scrolled[..] else {
            panic!(
                "expected the fixture's two boxes, got {}",
                cache.slices.scrolled.len()
            );
        };
        let overlap_end = under.x + under.w;
        assert_eq!(under.y, over.y, "the fixture's boxes must share a row");
        assert!(
            over.x < overlap_end,
            "the fixture's boxes must cover a common point"
        );

        let hit = cache
            .hit((over.x + overlap_end) / 2.0, over.y + over.h / 2.0)
            .expect("the box drawn over the point");
        let track = &profile.threads()[over.track];
        let drawn = profile.slices(track)[over.slice];
        assert_eq!(hit.profile().str(hit.slice().name), profile.str(drawn.name));
    }

    /// The self time a hover reports for the slice named `name`, hit at the
    /// center of the box drawn for it.
    fn hovered_self_us(profile: &Arc<Profile>, cache: &SliceDrawCache, name: &str) -> Option<i64> {
        let vs = cache
            .slices
            .scrolled
            .iter()
            .find(|vs| {
                let track = &profile.threads()[vs.track];
                profile.str(profile.slices(track)[vs.slice].name) == name
            })
            .unwrap_or_else(|| panic!("{name} is drawn"));
        let hit = cache
            .hit(vs.x + vs.w / 2.0, vs.y + vs.h / 2.0)
            .expect("the slice at its own center");
        assert_eq!(hit.profile().str(hit.slice().name), name);
        hit.self_us()
    }

    #[test]
    fn a_hit_reports_what_its_nested_spans_leave_unaccounted() {
        // A span, a child of it, a child of that, and a second child of the
        // span.
        let profile = profile_of_events(thread_events(
            TEST_TIDS[0],
            [(0, 1000), (100, 600), (200, 100), (800, 100)],
        ));
        let cache = draw_cache(&profile);
        let self_us = |i| hovered_self_us(&profile, &cache, &format!("t{}s{i}", TEST_TIDS[0]));
        // Only the two direct children count against the outer span; the
        // grandchild counts against the child it nests in.
        assert_eq!(self_us(0), Some(1000 - 600 - 100));
        assert_eq!(self_us(1), Some(600 - 100));
        // A span nothing nests in accounts for its whole duration.
        assert_eq!(self_us(2), None);
        assert_eq!(self_us(3), None);
    }

    #[test]
    fn a_hit_floors_self_time_at_zero_when_nested_spans_overrun() {
        // Two children of one span, overlapping each other so their durations
        // sum past the span that encloses them.
        let profile = profile_of_events(thread_events(
            TEST_TIDS[0],
            [(0, 1000), (100, 300), (200, 799)],
        ));
        let cache = draw_cache(&profile);
        let outer = format!("t{}s0", TEST_TIDS[0]);
        assert_eq!(hovered_self_us(&profile, &cache, &outer), Some(0));
    }
}
