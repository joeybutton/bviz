//! Editing the time selection: what a press takes hold of and what its drag makes of it.

use crate::regions::Regions;
use bviz_trace_layout::{TimeRegion, TimeSelection, TimeView, span_between};
use bviz_trace_profile::TimeSpan;

/// How near a region's edge in the ruler a press must land to take hold of it.
pub(super) const EDGE_GRAB_PX: f32 = 4.0;

/// What a time-region drag makes of the selection.
#[derive(Clone, Copy)]
pub(crate) enum RegionEdit {
    /// Restrict the selection to the dragged region alone.
    Replace,
    /// Union the dragged region into the selection.
    Union,
    /// Move the grabbed edge of the region at `index`; dragging it past the
    /// anchor flips the region.
    Resize { index: usize },
    /// Move the region at `index` whole, keeping `width_us`; the press took hold
    /// of it `grab_us` past its start, within the profile span `bounds`.
    Slide {
        index: usize,
        width_us: i64,
        grab_us: i64,
        bounds: TimeSpan,
    },
    /// Drop the region at `index`, keeping the rest of the selection.
    Remove { index: usize },
}

impl RegionEdit {
    /// Whether a press that never passed the click threshold drops the whole
    /// selection. Only a plain press on empty time does, naming no region and
    /// asking for no set operation, so it reads as "select nothing". A secondary
    /// press asks to toggle one region's membership and leaves the rest of the
    /// set standing, even where it found no region to toggle — the gap it
    /// misses by is a few pixels, and the selection it would drop is
    /// unrecoverable.
    fn clears_on_a_click(self) -> bool {
        match self {
            Self::Replace => true,
            Self::Union | Self::Resize { .. } | Self::Slide { .. } | Self::Remove { .. } => false,
        }
    }

    /// The region the edit acts on, and `None` for one that names no region of
    /// its own and covers whatever its drag reaches.
    fn region(self) -> Option<usize> {
        match self {
            Self::Replace | Self::Union => None,
            Self::Resize { index } | Self::Slide { index, .. } | Self::Remove { index } => {
                Some(index)
            }
        }
    }
}

/// A time-region drag in progress. Its region reaches the model on release, so
/// a whole drag costs one aggregate rebuild.
#[derive(Clone, Copy)]
pub(crate) struct RegionDrag {
    pub(super) edit: RegionEdit,
    /// The instant the drag is anchored at: a resize's fixed edge, otherwise
    /// where the press landed.
    pub(super) anchor_us: i64,
    pub(super) cursor_us: i64,
    /// Window x of the press, which the click threshold measures from.
    pub(super) press_x: f32,
    /// Whether the cursor has passed [`MIN_DRAG_PX`], latched so a drag that
    /// returns to the press point stays a drag.
    pub(super) moved: bool,
}

impl RegionDrag {
    /// The region the drag currently covers, never reaching outside the profile:
    /// a slide pushed against an end stops there, keeping its width. `None`
    /// where the drag covers no region — one still resting on the instant it is
    /// anchored at, and a removal, which names a region rather than sweeping one
    /// out.
    pub(crate) fn span(&self) -> Option<TimeRegion> {
        match self.edit {
            RegionEdit::Slide {
                width_us,
                grab_us,
                bounds,
                ..
            } => {
                let last_start_us = bounds.end_us - width_us;
                let start_us = (self.cursor_us - grab_us)
                    .min(last_start_us)
                    .max(bounds.start_us);
                span_between(start_us, start_us + width_us)
            }
            RegionEdit::Replace | RegionEdit::Union | RegionEdit::Resize { .. } => {
                span_between(self.anchor_us, self.cursor_us)
            }
            RegionEdit::Remove { .. } => None,
        }
    }

    /// The selection the drag would leave when applied over `base`. A removal
    /// names its region outright and takes it away from the press onward; every
    /// other edit applies the region the drag covers, which it has only past the
    /// click threshold, so short of that those yield `base` itself.
    pub(crate) fn applied(&self, base: &TimeSelection) -> TimeSelection {
        let mut sel = base.clone();
        let dragged = if self.moved { self.span() } else { None };
        match (self.edit, dragged) {
            (RegionEdit::Remove { index }, _) => {
                sel.remove(index);
            }
            (RegionEdit::Replace, Some(span)) => {
                sel.set(span);
            }
            (RegionEdit::Union, Some(span)) => {
                sel.insert(span);
            }
            (RegionEdit::Resize { index } | RegionEdit::Slide { index, .. }, Some(span)) => {
                sel.replace(index, span);
            }
            _ => {}
        }
        sel
    }

    /// The selection a release of the drag leaves over `base`.
    pub(crate) fn released(&self, base: &TimeSelection) -> TimeSelection {
        if !self.moved && self.edit.clears_on_a_click() {
            TimeSelection::default()
        } else {
            self.applied(base)
        }
    }
}

/// The instant `x_px` right of the time content's left edge points at, bounded
/// to `span` — the timeline's edge margin reaches beyond the profile. The sole
/// rounding of a mapped time to a profile instant.
pub(super) fn time_at_x(view: TimeView, span: TimeSpan, x_px: f32) -> i64 {
    (view.t_of(x_px).round() as i64).clamp(span.start_us, span.end_us)
}

/// A selected region's slice of the time axis, in window x.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct RegionPixels {
    pub(super) left: f32,
    pub(super) right: f32,
    pub(super) starts: bool,
    pub(super) ends: bool,
}

impl RegionPixels {
    /// Where the region `span` lands on the time axis, or `None` when it falls
    /// entirely off it. `left`/`right` are bounded to the axis; `starts`/`ends`
    /// mark the sides whose own edge lands on it, so an edge is drawn and live to
    /// a press together. The sole derivation of selection geometry, so what is
    /// painted and what a press takes hold of agree.
    fn of(span: TimeRegion, tv: TimeView, regions: Regions) -> Option<RegionPixels> {
        let start = regions.content_left + tv.x_of(span.start_us);
        let end = regions.content_left + tv.x_of(span.end_us);
        if end <= regions.content_left || start >= regions.time_right {
            return None;
        }
        Some(RegionPixels {
            left: start.max(regions.content_left),
            right: end.min(regions.time_right),
            starts: start >= regions.content_left,
            // An end edge is drawn inside the region it bounds, so an end
            // resting on the axis's far edge still lands on the axis.
            ends: end <= regions.time_right,
        })
    }
}

/// Where `sel`'s regions land on the time axis, dropping any that falls entirely
/// outside it.
pub(super) fn selection_pixels(
    sel: &TimeSelection,
    tv: TimeView,
    regions: Regions,
) -> Vec<RegionPixels> {
    let mut out = Vec::with_capacity(sel.len());
    out.extend(
        sel.spans()
            .iter()
            .filter_map(|s| RegionPixels::of(*s, tv, regions)),
    );
    out
}

/// What a press could take hold of, which is a property of the band it landed
/// in: the ruler draws edge grips and hit-tests them in pixels, the flame lanes
/// draw neither and leave the pressed instant to say where the press is.
#[derive(Clone, Copy)]
pub(super) enum Grab {
    /// A press in the ruler band, holding what [`region_grab`] found for it.
    Ruler(Option<(RegionEdit, i64)>),
    /// A press over the flame lanes.
    Lanes,
}

/// The edit a press on the instant `at` resolves to, with the instant to anchor
/// its drag at. Holding secondary toggles the pressed region's membership: it
/// takes away whichever region the press took hold of, and where it held none
/// it unions the drag in. Without secondary the press keeps what it took hold
/// of, and failing that replaces the selection.
pub(super) fn region_press(
    sel: &TimeSelection,
    at: i64,
    toggles: bool,
    grab: Grab,
) -> (RegionEdit, i64) {
    if toggles {
        let held = match grab {
            Grab::Ruler(held) => held.and_then(|(edit, _)| edit.region()),
            Grab::Lanes => sel.region_at(at),
        };
        let removal = held.map(|index| (RegionEdit::Remove { index }, at));
        return removal.unwrap_or((RegionEdit::Union, at));
    }
    match grab {
        Grab::Ruler(Some(held)) => held,
        Grab::Ruler(None) | Grab::Lanes => (RegionEdit::Replace, at),
    }
}

/// The edit a press at window `x`, on the instant `at`, takes hold of in `sel` —
/// a resize of a drawn region edge within [`EDGE_GRAB_PX`] of the press, else a
/// slide of the region it landed in — with the instant to anchor the drag at.
pub(super) fn region_grab(
    sel: &TimeSelection,
    tv: TimeView,
    regions: Regions,
    bounds: TimeSpan,
    at: i64,
    x: f32,
) -> Option<(RegionEdit, i64)> {
    sel.spans().iter().enumerate().find_map(|(index, s)| {
        let drawn = RegionPixels::of(*s, tv, regions)?;
        if drawn.starts && (drawn.left - x).abs() <= EDGE_GRAB_PX {
            Some((RegionEdit::Resize { index }, s.end_us))
        } else if drawn.ends && (drawn.right - x).abs() <= EDGE_GRAB_PX {
            Some((RegionEdit::Resize { index }, s.start_us))
        } else if at >= s.start_us && at < s.end_us {
            let edit = RegionEdit::Slide {
                index,
                width_us: s.end_us - s.start_us,
                grab_us: at - s.start_us,
                bounds,
            };
            Some((edit, at))
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DEFAULT_COUNTERS_HEIGHT;
    use crate::EDGE_MARGIN_PX;
    use crate::regions::CountersStrip;
    use crate::timeline::input::TimelineDrag;
    use crate::timeline::tests::{TEST_VIEW, profile_of};
    use gpui::{Bounds, point, px, size};

    /// A selection of the given regions, built the way a sequence of union drags
    /// would leave it.
    fn selection(regions: &[(i64, i64)]) -> TimeSelection {
        let mut sel = TimeSelection::default();
        for &(a, b) in regions {
            sel.insert(span_between(a, b).unwrap_or_else(|| panic!("({a}, {b}) spans no time")));
        }
        sel
    }

    fn regions(sel: &TimeSelection) -> Vec<(i64, i64)> {
        sel.spans().iter().map(|s| (s.start_us, s.end_us)).collect()
    }

    /// A drag that has passed the click threshold, anchored at `anchor_us` with
    /// its cursor on `cursor_us`.
    fn drag(edit: RegionEdit, anchor_us: i64, cursor_us: i64) -> RegionDrag {
        RegionDrag {
            edit,
            anchor_us,
            cursor_us,
            press_x: 0.0,
            moved: true,
        }
    }

    /// The profile a slide is dragged over.
    const TEST_SPAN: TimeSpan = TimeSpan {
        start_us: 0,
        end_us: 40_000,
    };

    /// A slide bounded to [`TEST_SPAN`].
    fn slide(index: usize, width_us: i64, grab_us: i64) -> RegionEdit {
        RegionEdit::Slide {
            index,
            width_us,
            grab_us,
            bounds: TEST_SPAN,
        }
    }

    #[test]
    fn a_replacing_drag_spans_its_press_and_its_cursor() {
        assert_eq!(
            drag(RegionEdit::Replace, 100, 400).span(),
            span_between(100, 400)
        );
        // Dragged leftward, the ends arrive the other way round.
        assert_eq!(
            drag(RegionEdit::Union, 400, 100).span(),
            span_between(100, 400)
        );
    }

    #[test]
    fn a_resize_dragged_past_its_fixed_edge_flips_the_region() {
        let edit = RegionEdit::Resize { index: 0 };
        assert_eq!(drag(edit, 400, 500).span(), span_between(400, 500));
        assert_eq!(drag(edit, 400, 250).span(), span_between(250, 400));
    }

    #[test]
    fn a_slide_carries_the_region_width_to_the_cursor() {
        assert_eq!(
            drag(slide(0, 100, 30), 0, 500).span(),
            span_between(470, 570)
        );
    }

    #[test]
    fn a_slide_pushed_past_the_profile_end_stops_at_it_keeping_its_width() {
        let held = drag(slide(0, 5_000, 2_000), 12_000, TEST_SPAN.end_us);
        assert_eq!(held.span(), span_between(35_000, 40_000));
    }

    #[test]
    fn a_slide_pushed_past_the_profile_start_stops_at_it_keeping_its_width() {
        let held = drag(slide(0, 5_000, 2_000), 12_000, TEST_SPAN.start_us);
        assert_eq!(held.span(), span_between(0, 5_000));
    }

    #[test]
    fn a_slide_clear_of_the_profile_ends_follows_the_cursor() {
        let held = drag(slide(0, 5_000, 2_000), 12_000, 20_000);
        assert_eq!(held.span(), span_between(18_000, 23_000));
    }

    #[test]
    fn a_replacing_drag_restricts_the_selection_to_its_own_region() {
        let base = selection(&[(0, 100), (200, 300)]);
        let sel = drag(RegionEdit::Replace, 500, 700).applied(&base);
        assert_eq!(regions(&sel), [(500, 700)]);
    }

    #[test]
    fn a_union_drag_over_an_existing_region_merges_with_it() {
        let base = selection(&[(0, 100), (200, 300)]);
        let sel = drag(RegionEdit::Union, 250, 400).applied(&base);
        assert_eq!(regions(&sel), [(0, 100), (200, 400)]);
    }

    #[test]
    fn a_resize_moves_only_the_edge_it_took_hold_of() {
        let base = selection(&[(0, 100), (200, 300)]);
        // Anchored at the second region's end, so its start follows the cursor.
        let sel = drag(RegionEdit::Resize { index: 1 }, 300, 150).applied(&base);
        assert_eq!(regions(&sel), [(0, 100), (150, 300)]);
    }

    #[test]
    fn a_slide_keeps_its_region_width_and_leaves_the_others() {
        let base = selection(&[(0, 100), (200, 300)]);
        let sel = drag(slide(1, 100, 50), 250, 550).applied(&base);
        assert_eq!(regions(&sel), [(0, 100), (500, 600)]);
    }

    #[test]
    fn a_drag_under_the_click_threshold_leaves_the_selection_alone() {
        let base = selection(&[(0, 100)]);
        let held = RegionDrag {
            moved: false,
            ..drag(RegionEdit::Replace, 500, 501)
        };
        assert_eq!(held.applied(&base), base);
    }

    #[test]
    fn a_resize_released_under_the_click_threshold_keeps_every_region() {
        let base = selection(&[(0, 100), (200, 300), (400, 500)]);
        let held = RegionDrag {
            moved: false,
            ..drag(RegionEdit::Resize { index: 1 }, 300, 202)
        };
        assert_eq!(regions(&held.released(&base)), regions(&base));
    }

    #[test]
    fn a_slide_released_under_the_click_threshold_keeps_every_region() {
        let base = selection(&[(0, 100), (200, 300), (400, 500)]);
        let held = RegionDrag {
            moved: false,
            ..drag(slide(1, 100, 50), 250, 252)
        };
        assert_eq!(regions(&held.released(&base)), regions(&base));
    }

    /// A region drag under way over the timeline.
    fn live(edit: RegionEdit, anchor_us: i64, cursor_us: i64) -> TimelineDrag {
        TimelineDrag::TimeRegion(drag(edit, anchor_us, cursor_us))
    }

    #[test]
    fn cancelling_a_replacing_drag_leaves_the_standing_selection() {
        let base = selection(&[(0, 100), (200, 300)]);
        let mut gesture = live(RegionEdit::Replace, 500, 700);
        assert_eq!(
            regions(&gesture.preview(&base)),
            [(500, 700)],
            "the drag previews a region"
        );
        assert!(gesture.cancel_region());
        assert_eq!(regions(&gesture.preview(&base)), regions(&base));
    }

    #[test]
    fn cancelling_a_resize_drag_leaves_its_region_at_its_own_extent() {
        let base = selection(&[(0, 100), (200, 300)]);
        let mut gesture = live(RegionEdit::Resize { index: 1 }, 300, 150);
        assert!(gesture.cancel_region());
        assert_eq!(regions(&gesture.preview(&base)), [(0, 100), (200, 300)]);
    }

    #[test]
    fn a_release_after_a_cancel_commits_nothing() {
        let mut gesture = live(RegionEdit::Replace, 500, 700);
        gesture.cancel_region();
        assert!(
            gesture.region().is_none(),
            "the release finds no drag to commit"
        );
    }

    /// A viewport split with a gutter of [`TEST_GUTTER`] and no counter lanes,
    /// whose flame lanes are short enough that no vertical-bar column is reserved.
    fn test_axis() -> Regions {
        Regions::new(
            Bounds::new(point(px(0.), px(0.)), size(px(1000.), px(800.))),
            0,
            CountersStrip::Open(DEFAULT_COUNTERS_HEIGHT),
            TEST_GUTTER,
            crate::regions::LaneHeights::default(),
        )
    }

    /// A view whose window opens well after the profile start, so a region can
    /// reach off either end of the axis.
    const TEST_SELECTION_VIEW: TimeView = TimeView {
        start_us: 5000.0,
        px_per_us: 0.1,
    };
    const TEST_GUTTER: f32 = 200.0;

    #[test]
    fn a_region_is_clipped_to_the_axis_and_keeps_only_the_edges_on_it() {
        let axis = test_axis();
        assert_eq!(
            (axis.content_left, axis.time_right),
            (200.0, 1000.0),
            "the fixture's axis"
        );
        let sel = selection(&[(4000, 6000), (6500, 8000), (12000, 20000)]);
        assert_eq!(
            selection_pixels(&sel, TEST_SELECTION_VIEW, axis),
            [
                RegionPixels {
                    left: 200.0,
                    right: 300.0,
                    starts: false,
                    ends: true
                },
                RegionPixels {
                    left: 350.0,
                    right: 500.0,
                    starts: true,
                    ends: true
                },
                RegionPixels {
                    left: 900.0,
                    right: 1000.0,
                    starts: true,
                    ends: false
                },
            ]
        );
    }

    #[test]
    fn a_region_entirely_off_the_axis_is_not_painted() {
        let axis = test_axis();
        let sel = selection(&[(0, 3000), (25000, 26000)]);
        assert_eq!(selection_pixels(&sel, TEST_SELECTION_VIEW, axis), []);
    }

    #[test]
    fn a_region_ending_on_the_axis_edge_keeps_its_end_hairline() {
        let axis = test_axis();
        let sel = selection(&[(10_000, 13_000)]);
        assert_eq!(
            selection_pixels(&sel, TEST_SELECTION_VIEW, axis),
            [RegionPixels {
                left: 700.0,
                right: 1000.0,
                starts: true,
                ends: true
            }]
        );
    }

    #[test]
    fn the_ruler_band_covers_the_time_axis_down_to_the_flame_lanes() {
        let axis = test_axis();
        let band = axis.ruler_rect();
        assert_eq!(f32::from(band.origin.x), axis.content_left);
        assert_eq!(f32::from(band.size.width), axis.time_w);
        assert_eq!(
            f32::from(band.origin.y) + f32::from(band.size.height),
            axis.flame_top
        );
    }

    #[test]
    fn a_press_is_in_the_ruler_band_exactly_where_the_paint_fills_it() {
        let axis = test_axis();
        let band = axis.ruler_rect();
        let mid_y = px(f32::from(band.origin.y) + f32::from(band.size.height) / 2.0);
        assert!(axis.in_ruler(point(px(axis.content_left), mid_y)));
        assert!(
            !axis.in_ruler(point(px(axis.content_left - 1.0), mid_y)),
            "over the gutter"
        );
        assert!(
            !axis.in_ruler(point(px(axis.time_right), mid_y)),
            "past the time axis"
        );
        assert!(
            !axis.in_ruler(point(px(axis.content_left), px(axis.flame_top))),
            "in the lanes"
        );
    }

    /// The press instant `x` maps to, as the ruler's own handler derives it.
    fn press_at(x: f32, axis: Regions) -> i64 {
        time_at_x(TEST_SELECTION_VIEW, TEST_SPAN, x - axis.content_left)
    }

    #[test]
    fn an_edge_clipped_off_the_axis_is_not_grabbable() {
        let axis = test_axis();
        // The region's start maps just left of the axis, within a grip's reach of
        // its leftmost pixel.
        let sel = selection(&[(4970, 8000)]);
        let drawn = selection_pixels(&sel, TEST_SELECTION_VIEW, axis);
        assert!(!drawn[0].starts, "the fixture's start edge is off the axis");
        let x = axis.content_left;
        let grabbed = region_grab(
            &sel,
            TEST_SELECTION_VIEW,
            axis,
            TEST_SPAN,
            press_at(x, axis),
            x,
        );
        assert!(
            matches!(grabbed, Some((RegionEdit::Slide { .. }, _))),
            "the press takes hold of the drawn body, not the hidden edge"
        );
    }

    #[test]
    fn a_press_on_a_drawn_edge_resizes_from_the_regions_far_side() {
        let axis = test_axis();
        let sel = selection(&[(6500, 8000)]);
        let x = axis.content_left + TEST_SELECTION_VIEW.x_of(6500);
        let grabbed = region_grab(
            &sel,
            TEST_SELECTION_VIEW,
            axis,
            TEST_SPAN,
            press_at(x, axis),
            x,
        );
        assert_eq!(grabbed.map(|(_, anchor)| anchor), Some(8000));
        assert!(matches!(
            grabbed,
            Some((RegionEdit::Resize { index: 0 }, _))
        ));
    }

    /// Window x of the instant `t_us` on the test axis.
    fn ruler_x(t_us: i64) -> f32 {
        test_axis().content_left + TEST_SELECTION_VIEW.x_of(t_us)
    }

    /// A press at window `x` over the ruler band, resolved as the ruler's own
    /// handler resolves it.
    fn ruler_press(sel: &TimeSelection, x: f32, toggles: bool) -> (RegionEdit, i64) {
        let axis = test_axis();
        let at = press_at(x, axis);
        let grab = Grab::Ruler(region_grab(
            sel,
            TEST_SELECTION_VIEW,
            axis,
            TEST_SPAN,
            at,
            x,
        ));
        region_press(sel, at, toggles, grab)
    }

    /// A press released without ever passing the click threshold.
    fn click(edit: RegionEdit, anchor_us: i64) -> RegionDrag {
        RegionDrag {
            moved: false,
            ..drag(edit, anchor_us, anchor_us)
        }
    }

    /// Three regions the removal gestures pick from, each wide enough that a
    /// press in its middle is clear of both edge zones.
    fn three_regions() -> TimeSelection {
        selection(&[(6000, 7000), (8000, 9000), (10_000, 11_000)])
    }

    #[test]
    fn a_secondary_click_inside_a_region_takes_that_region_away() {
        let base = three_regions();
        let (edit, at) = ruler_press(&base, ruler_x(8500), true);
        assert!(matches!(edit, RegionEdit::Remove { index: 1 }));
        assert_eq!(
            regions(&click(edit, at).released(&base)),
            [(6000, 7000), (10_000, 11_000)]
        );
    }

    #[test]
    fn secondary_removes_exactly_the_region_a_plain_press_would_take_hold_of() {
        let base = three_regions();
        let axis = test_axis();
        let mut x = axis.content_left;
        while x <= axis.time_right {
            let plain = ruler_press(&base, x, false).0;
            let toggled = ruler_press(&base, x, true).0;
            assert_eq!(plain.region(), toggled.region(), "at x={x}");
            x += 1.0;
        }
    }

    /// Region 1 of [`three_regions`] as the fixture draws it, in window x. Its
    /// edges carry the grips the removal gestures below aim at.
    const REGION_1_EDGES_PX: (f32, f32) = (500.0, 600.0);

    /// Asserts a secondary press at window `x` takes region 1 away and leaves
    /// the other two, and that a plain press there resizes the same region —
    /// the grip is grabbable at `x`, and a secondary press agrees on which
    /// region it belongs to.
    fn assert_grip_removes_region_1(x: f32) {
        let base = three_regions();
        assert!(
            matches!(
                ruler_press(&base, x, false).0,
                RegionEdit::Resize { index: 1 }
            ),
            "{x}"
        );
        let (edit, at) = ruler_press(&base, x, true);
        assert!(matches!(edit, RegionEdit::Remove { index: 1 }), "at x={x}");
        assert_eq!(
            regions(&click(edit, at).released(&base)),
            [(6000, 7000), (10_000, 11_000)]
        );
    }

    #[test]
    fn a_secondary_press_in_a_grips_outer_half_removes_its_region() {
        assert_eq!(
            (ruler_x(8000), ruler_x(9000)),
            REGION_1_EDGES_PX,
            "the fixture's drawn edges"
        );
        let (left, right) = REGION_1_EDGES_PX;
        // Outside the region's own stretch of time, so only the grip holds them.
        assert_grip_removes_region_1(left - 2.0);
        assert_grip_removes_region_1(right + 2.0);
    }

    #[test]
    fn a_secondary_press_on_a_drawn_end_edge_removes_its_region() {
        // A region ends on the instant past its last, so its end edge stands on
        // time the region itself does not cover.
        assert_grip_removes_region_1(REGION_1_EDGES_PX.1);
    }

    #[test]
    fn a_secondary_press_in_a_grips_inner_half_removes_its_region() {
        let (left, right) = REGION_1_EDGES_PX;
        assert_grip_removes_region_1(left);
        assert_grip_removes_region_1(right - 2.0);
    }

    #[test]
    fn a_secondary_press_beyond_a_grips_reach_leaves_the_selection_untouched() {
        let base = three_regions();
        let (left, right) = REGION_1_EDGES_PX;
        assert_eq!(
            EDGE_GRAB_PX, 4.0,
            "the reach the probes below are placed just outside"
        );
        for x in [left - 5.0, right + 5.0] {
            let (edit, at) = ruler_press(&base, x, true);
            assert!(matches!(edit, RegionEdit::Union), "at x={x}");
            assert_eq!(
                regions(&click(edit, at).released(&base)),
                regions(&base),
                "at x={x}"
            );
        }
    }

    #[test]
    fn a_removal_that_passes_the_click_threshold_still_removes_its_region() {
        let base = three_regions();
        let dragged = drag(RegionEdit::Remove { index: 1 }, 8500, 12_000);
        assert_eq!(
            regions(&dragged.released(&base)),
            [(6000, 7000), (10_000, 11_000)]
        );
    }

    /// The instant of the gap between the first two of [`three_regions`], far
    /// enough from both edges that a press there takes hold of nothing.
    const IN_THE_GAP_US: i64 = 7500;

    #[test]
    fn a_plain_click_on_empty_ruler_drops_the_whole_selection() {
        let base = three_regions();
        let (edit, at) = ruler_press(&base, ruler_x(IN_THE_GAP_US), false);
        assert!(matches!(edit, RegionEdit::Replace));
        assert!(click(edit, at).released(&base).is_empty());
    }

    #[test]
    fn a_secondary_click_outside_every_region_leaves_the_selection_untouched() {
        let base = three_regions();
        let (edit, at) = ruler_press(&base, ruler_x(IN_THE_GAP_US), true);
        assert!(
            matches!(edit, RegionEdit::Union),
            "it found no region to take away"
        );
        assert_eq!(regions(&click(edit, at).released(&base)), regions(&base));
    }

    #[test]
    fn a_shift_secondary_click_over_the_lanes_holding_no_region_keeps_the_selection() {
        let base = three_regions();
        let (edit, at) = region_press(&base, IN_THE_GAP_US, true, Grab::Lanes);
        assert!(matches!(edit, RegionEdit::Union));
        assert_eq!(regions(&click(edit, at).released(&base)), regions(&base));
    }

    #[test]
    fn a_secondary_press_outside_every_region_unions_a_new_one() {
        let base = three_regions();
        let (edit, at) = ruler_press(&base, ruler_x(IN_THE_GAP_US), true);
        assert!(matches!(edit, RegionEdit::Union));
        let sel = drag(edit, at, 7800).released(&base);
        assert_eq!(
            regions(&sel),
            [(6000, 7000), (7500, 7800), (8000, 9000), (10_000, 11_000)]
        );
    }

    #[test]
    fn a_plain_press_inside_a_region_slides_it_and_removes_nothing() {
        let base = three_regions();
        let (edit, at) = ruler_press(&base, ruler_x(8500), false);
        assert!(matches!(edit, RegionEdit::Slide { index: 1, .. }));
        assert_eq!(regions(&click(edit, at).released(&base)), regions(&base));
    }

    #[test]
    fn removing_the_only_region_leaves_the_selection_unrestricted() {
        let base = selection(&[(6000, 7000)]);
        let (edit, at) = ruler_press(&base, ruler_x(6500), true);
        assert!(matches!(edit, RegionEdit::Remove { index: 0 }));
        assert!(click(edit, at).released(&base).is_empty());
    }

    #[test]
    fn a_shift_secondary_press_over_the_lanes_removes_the_region_it_lands_in() {
        let base = three_regions();
        let (edit, at) = region_press(&base, 8500, true, Grab::Lanes);
        assert!(matches!(edit, RegionEdit::Remove { index: 1 }));
        assert_eq!(
            regions(&click(edit, at).released(&base)),
            [(6000, 7000), (10_000, 11_000)]
        );
    }

    #[test]
    fn a_removal_previews_its_region_gone_from_the_press_onward() {
        let base = selection(&[(6000, 7000), (8000, 9000)]);
        let gesture = TimelineDrag::TimeRegion(click(RegionEdit::Remove { index: 0 }, 6500));
        assert_eq!(regions(&gesture.preview(&base)), [(8000, 9000)]);
    }

    #[test]
    fn a_cursor_outside_the_profile_span_maps_back_onto_it() {
        let profile = profile_of(3);
        let span = profile.span();
        // The edge margin the pan reaches into lies outside the profile.
        assert_eq!(time_at_x(TEST_VIEW, span, -EDGE_MARGIN_PX), span.start_us);
        assert_eq!(
            time_at_x(
                TEST_VIEW,
                span,
                TEST_VIEW.x_of(span.end_us) + EDGE_MARGIN_PX
            ),
            span.end_us
        );
        let inside = (span.start_us + span.end_us) / 2;
        assert_eq!(time_at_x(TEST_VIEW, span, TEST_VIEW.x_of(inside)), inside);
    }
}
