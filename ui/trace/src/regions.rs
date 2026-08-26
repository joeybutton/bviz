use crate::{
    COUNTERS_COLLAPSED_HEIGHT, COUNTERS_DIVIDER_HEIGHT, COUNTERS_SNAP_SLOP,
    DEFAULT_COUNTERS_HEIGHT, FLAME_MIN_HEIGHT, METRICS, MIN_CHART_HEIGHT, MIN_DRAG_PX, MIN_GUTTER,
    RULER_HEIGHT,
};
use bviz_trace_layout::LaneLayout;
use bviz_ui_components::SCROLLBAR_THICKNESS;
use gpui::{Bounds, Pixels, Point, point, px, size};

/// How tall the counters strip stands. A strip dragged shut takes no room at
/// all and keeps the height it stood at, so reopening it brings back the strip
/// the user last saw.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum CountersStrip {
    Open(f32),
    Collapsed { restore: f32 },
}

impl CountersStrip {
    pub(crate) fn opened(self) -> Self {
        match self {
            CountersStrip::Open(_) => self,
            CountersStrip::Collapsed { restore } => CountersStrip::Open(restore),
        }
    }

    /// What a press with `click_count` at window y `press_y` begins on a divider
    /// whose strip stands with its top edge at `counters_top`.
    pub(crate) fn press(self, click_count: usize, press_y: f32, counters_top: f32) -> DividerPress {
        match click_count {
            2 => DividerPress::Reset(CountersStrip::Open(DEFAULT_COUNTERS_HEIGHT)),
            _ => DividerPress::Grab(DividerGrab {
                pressed: self,
                press_y,
                grab: counters_top - press_y,
                moved: false,
            }),
        }
    }

    /// Where a drag asking for a strip `height` tall leaves the strip that stood
    /// as `self` when the divider was pressed, in a window `vh` tall showing
    /// `n_counters` counters. An open strip shuts once the drag is pulled
    /// [`COUNTERS_SNAP_SLOP`] below the floor its lanes need; a shut one opens
    /// once pulled that far above where it sits shut, and from there follows the
    /// cursor within the heights the strip is held between.
    pub(crate) fn dragged_to(self, height: f32, n_counters: usize, vh: f32) -> Self {
        let (min, max) = counters_bounds(n_counters, vh);
        match self {
            CountersStrip::Open(standing) => match height < min - COUNTERS_SNAP_SLOP {
                true => CountersStrip::Collapsed { restore: standing },
                false => CountersStrip::Open(height.clamp(min, max)),
            },
            CountersStrip::Collapsed { .. } => match height > COUNTERS_SNAP_SLOP {
                true => CountersStrip::Open(height.clamp(min, max)),
                false => self,
            },
        }
    }
}

/// What a press on the counters divider begins.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum DividerPress {
    /// A double-click sets the strip back to a default height and takes hold of
    /// nothing.
    Reset(CountersStrip),
    /// A single press takes hold of the band.
    Grab(DividerGrab),
}

/// A press holding the counters divider, and the drag it may yet become.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct DividerGrab {
    /// The strip as it stood when the band was taken hold of, which the drag
    /// resizes from.
    pressed: CountersStrip,
    /// Window y of the press, which the click threshold measures from.
    press_y: f32,
    /// Distance from the press up to the strip's top edge. Held for the whole
    /// gesture, so the band stays under the cursor across a change of band
    /// height.
    grab: f32,
    /// Whether the cursor has passed [`MIN_DRAG_PX`], latched so a drag that
    /// returns to the press point stays a drag.
    moved: bool,
}

impl DividerGrab {
    /// Where the strip stands with the cursor at window y `y`, the timeline
    /// ending at `counters_bottom` in a window `vh` tall showing `n_counters`
    /// counters. A press that has not travelled far enough to be a drag is still
    /// a click, and leaves the strip as it was pressed.
    pub(crate) fn dragged_to(
        &mut self,
        y: f32,
        counters_bottom: f32,
        n_counters: usize,
        vh: f32,
    ) -> CountersStrip {
        self.moved |= (y - self.press_y).abs() > MIN_DRAG_PX;
        if !self.moved {
            return self.pressed;
        }
        self.pressed
            .dragged_to(counters_bottom - y - self.grab, n_counters, vh)
    }

    /// Where `strip` stands once the press is let go: a drag leaves it where it
    /// was dragged to, while a click opens a shut strip at the height it holds.
    pub(crate) fn released(self, strip: CountersStrip) -> CountersStrip {
        match self.moved {
            true => strip,
            false => strip.opened(),
        }
    }
}

/// The timeline split into the flame chart region on top, the divider, and the
/// counters strip, plus the gutter/time-axis columns shared across them. Derived
/// once per frame, by both paint and the mouse handlers, so hit-testing matches
/// what was drawn.
#[derive(Clone, Copy)]
pub(crate) struct Regions {
    /// Left edge of the time content, past the gutter.
    pub(crate) content_left: f32,
    /// Right edge of the content column (where the vertical bar/gutter end).
    pub(crate) content_right: f32,
    /// Right edge of the time axis, left of the reserved vertical-bar column.
    /// Everything mapped to time ends here so nothing underlaps the bar.
    pub(crate) time_right: f32,
    /// Width of the time axis: the content column less the reserved vertical-bar
    /// column.
    pub(crate) time_w: f32,
    /// Top of the ruler band (the viewport's top edge).
    ruler_top: f32,
    /// Top of the flame lanes (ruler bottom).
    pub(crate) flame_top: f32,
    /// Bottom of the scrollable flame lanes; the time horizontal bar sits below.
    pub(crate) flame_bottom: f32,
    /// Top of the divider (below the flame region and its horizontal bar).
    divider_top: f32,
    /// Top of the counters strip, which is the divider's bottom.
    pub(crate) counters_top: f32,
    /// Bottom of the counters strip, which is the viewport's bottom edge and so
    /// the bottom of the gutter column running the whole height.
    pub(crate) counters_bottom: f32,
}

impl Regions {
    /// Splits `bounds` given the counter count, the counters strip, the gutter
    /// width, and the flame lanes' total height (to decide whether a vertical bar
    /// shows and so a column must be reserved for it).
    pub(crate) fn new(
        bounds: Bounds<Pixels>,
        n_counters: usize,
        strip: CountersStrip,
        gutter_width: f32,
        flame_total: f32,
    ) -> Self {
        let ox = f32::from(bounds.origin.x);
        let oy = f32::from(bounds.origin.y);
        let w = f32::from(bounds.size.width);
        let vh = f32::from(bounds.size.height);
        let counters_bottom = oy + vh;
        let counters_h = clamp_counters_height(strip, n_counters, vh);
        let counters_top = (counters_bottom - counters_h).max(oy);
        let band = match counters_h > 0.0 {
            true => COUNTERS_DIVIDER_HEIGHT,
            false => COUNTERS_COLLAPSED_HEIGHT,
        };
        let divider_top = match n_counters > 0 {
            true => (counters_top - band).max(oy),
            false => counters_top,
        };
        // The flame region: ruler at the top, scrollable lanes, then a strip for
        // the time horizontal bar always reserved above the divider.
        let flame_top = oy + RULER_HEIGHT;
        let flame_bottom = (divider_top - SCROLLBAR_THICKNESS).max(flame_top);
        // Clamp the stored gutter width to the current window: a widened gutter
        // shrunk under by a later window resize must still leave the time axis
        // its share rather than collapsing it.
        let gutter_width = clamp_gutter_width(gutter_width, w);
        let content_left = ox + gutter_width;
        let content_w = (w - gutter_width).max(0.0);
        let content_right = ox + w;
        let flame_h = flame_bottom - flame_top;
        // Reserve a column for the vertical bar when the flame lanes overflow it,
        // so slices and charts end left of the bar rather than under it.
        let vbar_reserve = if flame_total > flame_h.max(0.0) {
            SCROLLBAR_THICKNESS
        } else {
            0.0
        };
        Regions {
            content_left,
            content_right,
            time_right: content_right - vbar_reserve,
            time_w: (content_w - vbar_reserve).max(0.0),
            ruler_top: oy,
            flame_top,
            flame_bottom,
            divider_top,
            counters_top,
            counters_bottom,
        }
    }

    /// The band between the flame lanes and the counters strip that a press
    /// takes hold of to resize the strip.
    pub(crate) fn divider_rect(&self) -> Bounds<Pixels> {
        Bounds::new(
            point(px(self.content_left), px(self.divider_top)),
            size(
                px((self.content_right - self.content_left).max(0.0)),
                px((self.counters_top - self.divider_top).max(0.0)),
            ),
        )
    }

    /// The ruler band over the time axis: the strip the selection's regions are
    /// tinted and gripped in, and that a press takes hold of them from.
    pub(crate) fn ruler_rect(&self) -> Bounds<Pixels> {
        Bounds::new(
            point(px(self.content_left), px(self.ruler_top)),
            size(px(self.time_w), px(self.flame_top - self.ruler_top)),
        )
    }

    pub(crate) fn in_ruler(&self, pos: Point<Pixels>) -> bool {
        self.ruler_rect().contains(&pos)
    }

    /// The flame lanes' time content, which their paint is masked to and their
    /// hover is tested against, so a tooltip is offered exactly where a slice
    /// can be drawn.
    pub(crate) fn flame_rect(&self) -> Bounds<Pixels> {
        Bounds::new(
            point(px(self.content_left), px(self.flame_top)),
            size(px(self.time_w), px(self.flame_h())),
        )
    }

    pub(crate) fn in_flame(&self, pos: Point<Pixels>) -> bool {
        self.flame_rect().contains(&pos)
    }

    /// Height the flame lanes scroll within.
    pub(crate) fn flame_h(&self) -> f32 {
        (self.flame_bottom - self.flame_top).max(0.0)
    }

    /// Height of the counters strip below the divider.
    pub(crate) fn counters_h(&self) -> f32 {
        (self.counters_bottom - self.counters_top).max(0.0)
    }

    /// Whether the counters strip stands open, and so whether its lanes, their
    /// gutter labels, and the time-axis furniture crossing them are drawn.
    pub(crate) fn shows_counters(&self) -> bool {
        self.counters_h() > 0.0
    }
}

/// The heights an open counters strip is held between: enough for every counter
/// lane to keep a header band plus `MIN_CHART_HEIGHT` of chart, so no lane
/// collapses to nothing, and never more than leaves the flame region
/// `FLAME_MIN_HEIGHT`. A window too short for both floors squeezes the strip:
/// the flame chart is the timeline's subject and keeps its own.
fn counters_bounds(n_counters: usize, vh: f32) -> (f32, f32) {
    let max =
        (vh - RULER_HEIGHT - SCROLLBAR_THICKNESS - COUNTERS_DIVIDER_HEIGHT - FLAME_MIN_HEIGHT)
            .max(0.0);
    let min = (n_counters as f32 * (METRICS.header_height + MIN_CHART_HEIGHT)).min(max);
    (min, max)
}

/// The height the counters strip stands at, bounded by [`counters_bounds`]. It
/// is nothing while the strip is shut, and nothing at all for a profile with no
/// counters, whatever height the divider was last dragged to for one that had
/// them.
fn clamp_counters_height(strip: CountersStrip, n_counters: usize, vh: f32) -> f32 {
    if n_counters == 0 {
        return 0.0;
    }
    match strip {
        CountersStrip::Collapsed { .. } => 0.0,
        CountersStrip::Open(height) => {
            let (min, max) = counters_bounds(n_counters, vh);
            height.clamp(min, max)
        }
    }
}

/// The gutter width bounded to stay legible yet never claim more than half the
/// window, so the time axis keeps the larger share.
pub(crate) fn clamp_gutter_width(gutter_width: f32, window_w: f32) -> f32 {
    let max = (window_w * 0.5).max(MIN_GUTTER);
    gutter_width.clamp(MIN_GUTTER, max)
}

/// Total height the flame section's rows occupy — thread lanes and the group
/// bands they hang under, which share one coordinate space — ignoring the
/// layout's counter lanes (the counters strip places those itself).
pub(crate) fn flame_total_height(layout: &LaneLayout) -> f32 {
    layout.total_height
}

/// The pixel ceiling for the lane scroll: the thread-lane height past the flame
/// region's on-screen height.
pub(crate) fn max_lane_scroll(layout: &LaneLayout, regions: &Regions) -> f32 {
    (flame_total_height(layout) - regions.flame_h()).max(0.0)
}

/// One fitted counter lane in the strip: its top (window y) and the height of
/// its chart area below the header band.
#[derive(Clone, Copy)]
pub(crate) struct GraphLane {
    pub(crate) lane: usize,
    pub(crate) top: f32,
    pub(crate) chart_h: f32,
}

/// Distributes the counters strip's height evenly across its `n` counter lanes:
/// each lane is a header band plus a chart area that shrinks to zero as the
/// section collapses toward the label-only minimum.
pub(crate) fn graph_lanes(n: usize, graph_top: f32, graph_h: f32) -> Vec<GraphLane> {
    if n == 0 {
        return Vec::new();
    }
    let chart_h = ((graph_h - n as f32 * METRICS.header_height) / n as f32).max(0.0);
    let lane_h = METRICS.header_height + chart_h;
    (0..n)
        .map(|lane| GraphLane {
            lane,
            top: graph_top + lane as f32 * lane_h,
            chart_h,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window the tests split: at the origin and tall enough that the strip
    /// is held by its own floor rather than by the room the flame region keeps.
    /// Its bottom edge, which the strip stands up from, is at `TALL`.
    const TALL: f32 = 800.0;
    /// Counters in the profile the gesture tests take hold of the divider of.
    const COUNTERS: usize = 3;

    fn window(vh: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(0.0), px(0.0)), size(px(1000.0), px(vh)))
    }

    fn split(n_counters: usize, strip: CountersStrip, vh: f32) -> Regions {
        Regions::new(window(vh), n_counters, strip, MIN_GUTTER, 0.0)
    }

    /// A press on the divider of a strip standing with its top edge at
    /// `counters_top`, taking hold of the band.
    fn grabbed(strip: CountersStrip, press_y: f32, counters_top: f32) -> DividerGrab {
        match strip.press(1, press_y, counters_top) {
            DividerPress::Grab(grab) => grab,
            DividerPress::Reset(_) => panic!("a single press takes hold of the band"),
        }
    }

    /// Where the strip stands with the drag's cursor at window y `y`.
    fn dragged(grab: &mut DividerGrab, y: f32) -> CountersStrip {
        grab.dragged_to(y, TALL, COUNTERS, TALL)
    }

    #[test]
    fn an_open_strip_stands_at_the_height_it_was_left_at() {
        assert_eq!(
            clamp_counters_height(CountersStrip::Open(220.0), COUNTERS, TALL),
            220.0
        );
    }

    /// The strip's floor is the room its lanes need: every chart keeps a
    /// readable sliver below its header band rather than collapsing to nothing.
    #[test]
    fn the_strip_floor_keeps_every_chart_readable() {
        let floor = clamp_counters_height(CountersStrip::Open(0.0), COUNTERS, TALL);
        for lane in graph_lanes(COUNTERS, 0.0, floor) {
            assert_eq!(lane.chart_h, MIN_CHART_HEIGHT);
        }
    }

    /// A strip squeezed to nothing by a short window stands under a collapsed
    /// band, so the caret that brings it back is drawn across the band that is
    /// all the room it has left.
    #[test]
    fn a_strip_squeezed_to_nothing_stands_under_a_collapsed_band() {
        let strip = CountersStrip::Open(DEFAULT_COUNTERS_HEIGHT);
        // The height that floors the ruler, the time bar, the divider band and
        // the flame region together, leaving the strip nothing.
        let floored =
            RULER_HEIGHT + SCROLLBAR_THICKNESS + COUNTERS_DIVIDER_HEIGHT + FLAME_MIN_HEIGHT;
        for vh in [TALL, floored + 1.0] {
            let regions = split(COUNTERS, strip, vh);
            assert!(regions.shows_counters());
            assert_eq!(
                regions.divider_rect().size.height,
                px(COUNTERS_DIVIDER_HEIGHT)
            );
        }
        for vh in [floored, floored - 20.0, COUNTERS_COLLAPSED_HEIGHT] {
            let regions = split(COUNTERS, strip, vh);
            assert!(!regions.shows_counters());
            assert_eq!(
                regions.divider_rect().size.height,
                px(COUNTERS_COLLAPSED_HEIGHT)
            );
        }
    }

    /// However far the strip is opened, the flame chart keeps its own floor.
    #[test]
    fn the_strip_never_takes_the_flame_region_below_its_floor() {
        let regions = split(1, CountersStrip::Open(10_000.0), 260.0);
        assert_eq!(regions.flame_h(), FLAME_MIN_HEIGHT);
        assert!(regions.shows_counters());
    }

    /// The flame chart is the timeline's subject, so where the window is too
    /// short to floor both sections it is the counters strip that gives way.
    #[test]
    fn a_window_too_short_for_both_floors_squeezes_the_counters() {
        let n = 10;
        let regions = split(n, CountersStrip::Open(220.0), 120.0);
        assert_eq!(regions.flame_h(), FLAME_MIN_HEIGHT);
        let squeezed = graph_lanes(n, regions.counters_top, regions.counters_h());
        assert!(squeezed[0].chart_h < MIN_CHART_HEIGHT);
    }

    /// A profile with no counters has no strip, so no height is kept for one
    /// however the divider was last dragged for a profile that had them.
    #[test]
    fn a_profile_with_no_counters_has_no_strip() {
        assert_eq!(
            clamp_counters_height(CountersStrip::Open(220.0), 0, TALL),
            0.0
        );
    }

    /// The strip a drag has shut takes no room at all, whatever height it holds
    /// to spring back to.
    #[test]
    fn a_shut_counters_strip_stands_at_nothing() {
        let shut = CountersStrip::Collapsed { restore: 220.0 };
        assert_eq!(clamp_counters_height(shut, COUNTERS, TALL), 0.0);
        assert_eq!(shut.opened(), CountersStrip::Open(220.0));
    }

    /// The pull that shuts the strip is a fixed distance past its floor, so it
    /// asks the same of a profile with one counter and of one with twenty.
    #[test]
    fn the_same_pull_past_the_floor_shuts_the_strip_whatever_its_floor() {
        for n in [1, 20] {
            let (floor, _) = counters_bounds(n, TALL);
            let open = CountersStrip::Open(300.0);
            assert_eq!(
                open.dragged_to(floor - COUNTERS_SNAP_SLOP + 1.0, n, TALL),
                CountersStrip::Open(floor)
            );
            assert_eq!(
                open.dragged_to(floor - COUNTERS_SNAP_SLOP - 1.0, n, TALL),
                CountersStrip::Collapsed { restore: 300.0 }
            );
        }
    }

    /// A press that never travels is a click: it leaves the strip standing where
    /// it is while it is held, and brings a shut one back at the height it kept.
    #[test]
    fn a_click_on_the_shut_band_brings_the_strip_back() {
        let shut = CountersStrip::Collapsed { restore: 220.0 };
        let mut grab = grabbed(shut, TALL - 8.0, TALL);
        assert_eq!(dragged(&mut grab, TALL - 9.0), shut);
        assert_eq!(grab.released(shut), CountersStrip::Open(220.0));
    }

    /// A press that travels is a drag, and answers where it is let go rather
    /// than springing the strip open behind it.
    #[test]
    fn a_drag_down_the_shut_band_leaves_the_strip_shut() {
        let shut = CountersStrip::Collapsed { restore: 220.0 };
        let mut grab = grabbed(shut, TALL - 8.0, TALL);
        assert_eq!(dragged(&mut grab, TALL + 200.0), shut);
        assert_eq!(grab.released(shut), shut);
    }

    /// A drag opens the shut strip as soon as it is pulled clear of the band,
    /// and from there gives the height the cursor asks for.
    #[test]
    fn a_drag_up_the_shut_band_opens_the_strip_where_the_cursor_asks() {
        let shut = CountersStrip::Collapsed { restore: 220.0 };
        let (floor, _) = counters_bounds(COUNTERS, TALL);
        let held = 7.0;
        let mut grab = grabbed(shut, TALL - held, TALL);
        // Short of the slop the strip stays shut; a pixel past it the strip
        // opens, at the floor its lanes need.
        assert_eq!(dragged(&mut grab, TALL - held - COUNTERS_SNAP_SLOP), shut);
        assert_eq!(
            dragged(&mut grab, TALL - held - COUNTERS_SNAP_SLOP - 1.0),
            CountersStrip::Open(floor)
        );
        // Well clear of the band it is the cursor, not the height the strip was
        // shut at, that says how tall the strip stands.
        assert_eq!(
            dragged(&mut grab, TALL - 300.0),
            CountersStrip::Open(300.0 - held)
        );
    }

    /// The band cannot slip out from under the cursor: the strip's top edge
    /// keeps the distance it was grabbed at, whatever height the band itself
    /// changes to as the strip opens.
    #[test]
    fn a_drag_holds_the_band_where_it_was_grabbed() {
        let shut = CountersStrip::Collapsed { restore: 220.0 };
        let held = 10.0;
        let mut grab = grabbed(shut, TALL - held, TALL);
        for y in [700.0, 600.0, 500.0] {
            let CountersStrip::Open(height) = dragged(&mut grab, y) else {
                panic!("a drag clear of the band opens the strip")
            };
            assert_eq!(TALL - height, y + held);
        }
    }

    /// A double-click resets the strip to its default height from either state,
    /// and takes hold of nothing.
    #[test]
    fn a_double_click_resets_the_strip_from_either_state() {
        for strip in [
            CountersStrip::Open(600.0),
            CountersStrip::Collapsed { restore: 220.0 },
        ] {
            assert_eq!(
                strip.press(2, TALL - 4.0, TALL),
                DividerPress::Reset(CountersStrip::Open(DEFAULT_COUNTERS_HEIGHT))
            );
        }
    }
}
