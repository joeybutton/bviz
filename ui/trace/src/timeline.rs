use crate::chart::fit_px_per_us;
use crate::counters::{StripCounters, counter_readouts, paint_crosshair, track_count};
use crate::groups::{GroupChrome, GroupHit, RowAnchor, anchor_at, anchor_offset, chart_rows};
use crate::model::{ChoiceChanged, TraceModel, WorkingLanesDerived};
use crate::regions::{CountersStrip, Regions, max_lane_scroll};
use crate::theme::Palette;
use crate::{
    ClearTimeSelection, DEFAULT_COUNTERS_HEIGHT, DEFAULT_GUTTER_WIDTH, EDGE_MARGIN_PX,
    GROUP_BAND_HEIGHT, GROUP_ICON, LINE_SCROLL_PX, MAX_PX_PER_US, METRICS, MIN_DRAG_PX,
    MIN_SLICE_PX, SLICE_TEXT_PAD, SelectAllTracks, TIMELINE_KEY_CONTEXT, TimelineBars, TracePanel,
    ZOOM_PER_NOTCH, drawn_scroll, paint_gridlines, paint_gutter, paint_marker_band, paint_markers,
    paint_ruler, paint_slice, ruler_ticks, scroll_request, scrollbar_geom, slice_tooltip,
    thumb_offset,
};
use bviz_trace_layout::{
    Cull, LaneLayout, TimeRegion, TimeSelection, TimeView, Viewport, VisibleSlice, span_between,
    visible_slices,
};
use bviz_trace_profile::{Profile, Slice, SliceAt, SliceDetail, TimeSpan};
use bviz_ui_components::{DragEnd, cached_layer, overlay_canvas, paint_tooltip_box, track_drag};
use bviz_ui_playback::Clock;
use gpui::{
    App, Bounds, ContentMask, Context, Div, Entity, FocusHandle, Focusable, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Point, Render, Rgba, ScrollWheelEvent,
    Stateful, Subscription, WeakEntity, Window, canvas, div, fill, point, prelude::*, px, size,
};
use std::rc::Rc;
use std::sync::Arc;

/// The timeline's culled draw list together with the inputs that determine it,
/// so a repaint reuses it unless one changed. The list turns only on the profile
/// (`Arc` identity), the lane layout (`Rc` identity, a fresh one whenever the
/// chart restacks), the time view (pan/zoom), and the
/// viewport (width, height, and `scroll_y`); `min_px` and the metrics are consts,
/// so they are not keys.
pub(crate) struct SliceDrawCache {
    profile: Arc<Profile>,
    lanes: Rc<LaneLayout>,
    view: TimeView,
    viewport: Viewport,
    slices: Rc<Vec<VisibleSlice>>,
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
        lanes: Rc<LaneLayout>,
        view: TimeView,
        viewport: Viewport,
    ) -> Self {
        let slices = Rc::new(visible_slices(
            &profile,
            &lanes,
            Cull {
                view: &view,
                viewport: &viewport,
                metrics: &METRICS,
                min_px: MIN_SLICE_PX,
            },
        ));
        Self {
            profile,
            lanes,
            view,
            viewport,
            slices,
        }
    }

    /// The slice drawn at flame-content-relative `(x, y)`, which the caller has
    /// already gated to the flame region — a point outside it still matches any
    /// box it falls in. Boxes may overlap, and a later one is drawn over an
    /// earlier one, so the last match wins.
    fn hit(&self, x: f32, y: f32) -> Option<HoveredSlice> {
        let vs = self
            .slices
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

/// The flame chart as it currently stands, read against the lane scroll's
/// standing anchor.
struct AnchoredChart {
    chrome: Rc<GroupChrome>,
    /// Where the anchored row sits in the chart, before the ceiling bounds it.
    held: f32,
    ceiling: f32,
}

/// How near a region's edge in the ruler a press must land to take hold of it.
const EDGE_GRAB_PX: f32 = 4.0;

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
    /// asking for no set operation, so it reads as "select nothing". A ctrl
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
    edit: RegionEdit,
    /// The instant the drag is anchored at: a resize's fixed edge, otherwise
    /// where the press landed.
    anchor_us: i64,
    cursor_us: i64,
    /// Window x of the press, which the click threshold measures from.
    press_x: f32,
    /// Whether the cursor has passed [`MIN_DRAG_PX`], latched so a drag that
    /// returns to the press point stays a drag.
    moved: bool,
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
fn time_at_x(view: TimeView, span: TimeSpan, x_px: f32) -> i64 {
    (view.t_of(x_px).round() as i64).clamp(span.start_us, span.end_us)
}

/// What a left-button drag over the timeline is currently moving.
#[derive(Clone, Copy)]
enum TimelineDrag {
    None,
    /// A press on the flame chart pans the timeline (and scrolls its lanes).
    Pan(PanDrag),
    /// A press on the lane vertical, time horizontal, or counters vertical
    /// scroll thumb/track, holding the grab offset from the thumb's leading edge.
    VThumb(f32),
    HThumb(f32),
    CountersThumb(f32),
    /// A press on the ruler, or a shift-press on the flame lanes, selecting the
    /// time the flame graph and stats aggregate over.
    TimeRegion(RegionDrag),
}

/// A pan in progress.
#[derive(Clone, Copy)]
struct PanDrag {
    /// Window position of the press, which the click threshold measures from.
    press: Point<Pixels>,
    /// Whether the cursor has passed [`MIN_DRAG_PX`] from the press, latched
    /// so a pan that returns to the press point stays a pan.
    moved: bool,
}

impl PanDrag {
    fn at(press: Point<Pixels>) -> PanDrag {
        PanDrag {
            press,
            moved: false,
        }
    }

    fn follow(&mut self, to: Point<Pixels>) {
        let dx = f32::from(to.x - self.press.x).abs();
        let dy = f32::from(to.y - self.press.y).abs();
        self.moved |= dx.max(dy) > MIN_DRAG_PX;
    }

    /// Whether the pan, ended as `end`, was a click: let go of where it was
    /// pressed, give or take the click threshold.
    fn clicked(self, end: DragEnd) -> bool {
        !self.moved && matches!(end, DragEnd::Released)
    }
}

impl TimelineDrag {
    /// The region drag under way, which the overlay previews and a release
    /// commits, so what is drawn during a drag is what lands.
    fn region(&self) -> Option<RegionDrag> {
        match *self {
            TimelineDrag::TimeRegion(drag) => Some(drag),
            _ => None,
        }
    }

    /// The selection to paint over `standing` while the drag runs.
    fn preview(&self, standing: &TimeSelection) -> TimeSelection {
        self.region()
            .map_or_else(|| standing.clone(), |drag| drag.applied(standing))
    }

    /// Abandons a region drag under way, reporting whether there was one. It
    /// commits nothing, so the selection stands as the press found it.
    fn cancel_region(&mut self) -> bool {
        let live = self.region().is_some();
        if live {
            *self = TimelineDrag::None;
        }
        live
    }
}

/// The flame-chart timeline: time on the x axis, stacked thread lanes on the y
/// axis, the counter plots, the ruler, gutter, phase markers, and scroll bars.
/// Owns the timeline's view state, reading the document and selection from
/// [`TraceModel`] and the strip/gutter layout from [`TracePanel`]. The slices are
/// painted by [`SliceCanvas`], the time selection, crosshair, and tooltip by an
/// overlay above it.
pub(crate) struct Timeline {
    /// Focus for the trace content, so [`SelectAllTracks`] dispatches here; a
    /// descendant of the window focus, so the window's own actions fire.
    content_focus: FocusHandle,
    /// Read for the document and lanes, written for selection and grouping.
    model: Entity<TraceModel>,
    /// The owning view, read for the counters-strip and gutter layout its
    /// divider and gutter handle drag.
    shell: WeakEntity<TracePanel>,
    /// Horizontal time-to-pixel mapping (pan and zoom). Prepaint settles it
    /// against the painted width — the fit, the resize rescale, and the zoom
    /// floor all need a width the timeline learns only there — and the same
    /// frame's paint then reads the result.
    view: TimeView,
    /// Which chart row the flame region's top edge sits in, and where within it;
    /// [`Self::lane_scroll`] turns it into the pixel offset the frame draws at.
    scroll: RowAnchor,
    /// How far the counters strip is scrolled up, in pixels, held past its
    /// ceiling so a strip resized shorter and back returns to where it stood;
    /// [`Self::counter_scroll`] is the offset the frame draws at.
    counter_scroll_held: f32,
    /// Viewport bounds captured during canvas paint, so handlers can map a window
    /// cursor to viewport space and bound the zoom to a fit.
    viewport: Bounds<Pixels>,
    /// Whether the initial fit-to-window view has been set; latched during the
    /// first paint, when the viewport size is first known.
    fitted: bool,
    follow: Follow,
    /// A stretch of time asked to be shown before the first paint, which that
    /// paint's fit frames in place of the whole profile.
    framed_on_fit: Option<TimeSpan>,
    /// The timeline's memoized culled draw list; see [`SliceDrawCache`].
    slice_draw: Option<SliceDrawCache>,
    /// The cursor position while it hovers the flame lanes, anchoring the slice
    /// tooltip; `None` otherwise.
    hover: Option<Point<Pixels>>,
    /// The cursor position while it hovers the time-content column and no drag is
    /// in progress, drawn as a crosshair; `None` otherwise.
    crosshair: Option<Point<Pixels>>,
    drag: TimelineDrag,
    last_mouse: Point<Pixels>,
    /// The layer painting the slices, gutter, ruler, markers, scroll bars, and
    /// counter plots; notified through [`Self::invalidate_canvas`].
    canvas: Entity<SliceCanvas>,
    /// Repaints the overlay for a selection change made in another pane. Only
    /// the overlay draws from the model's selection state that the timeline does
    /// not itself move, so the slice scene replays rather than rebuilding.
    _model_observer: Subscription,
    /// Repaints the overlay when the session learns something of the
    /// invocation, which the hover tooltip names a slice's configuration by.
    _session_observer: Option<Subscription>,
    /// Redraws the counter plots once the working lanes are there to plot.
    _working_lanes_derived: Subscription,
    /// Repaints the overlay, which draws the playhead, as a gesture moves the
    /// document's clock, and follows the playhead again from there.
    _clock_changed: Option<Subscription>,
    _choice_changed: Subscription,
}

impl Timeline {
    pub(crate) fn new(
        model: Entity<TraceModel>,
        shell: WeakEntity<TracePanel>,
        cx: &mut Context<Self>,
    ) -> Self {
        let content_focus = cx.focus_handle();
        let parent = cx.entity().downgrade();
        let canvas = cx.new(|_| SliceCanvas { parent });
        let model_observer = cx.observe(&model, |_, _, cx| cx.notify());
        let session = model.read(cx).session().cloned();
        let session_observer = session.map(|session| cx.observe(&session, |_, _, cx| cx.notify()));
        let clock = model.read(cx).clock().cloned();
        let clock_changed = clock.map(|clock| {
            cx.observe(&clock, |timeline, _, cx| {
                timeline.follow = Follow::Playhead;
                cx.notify();
            })
        });
        let working_lanes_derived = cx
            .subscribe(&model, |timeline, _, _: &WorkingLanesDerived, cx| {
                timeline.invalidate_canvas(cx)
            });
        let choice_changed = cx.subscribe(&model, |timeline, _, _: &ChoiceChanged, cx| {
            timeline.reveal_chosen(cx);
            timeline.invalidate_canvas(cx);
        });
        Self {
            content_focus,
            model,
            shell,
            view: TimeView {
                start_us: 0.0,
                px_per_us: 1.0,
            },
            scroll: RowAnchor::Start,
            counter_scroll_held: 0.0,
            viewport: Bounds::default(),
            fitted: false,
            follow: Follow::Playhead,
            framed_on_fit: None,
            slice_draw: None,
            hover: None,
            crosshair: None,
            drag: TimelineDrag::None,
            last_mouse: Point::default(),
            canvas,
            _model_observer: model_observer,
            _session_observer: session_observer,
            _working_lanes_derived: working_lanes_derived,
            _clock_changed: clock_changed,
            _choice_changed: choice_changed,
        }
    }

    /// Frames the time of what is chosen of the critical path and scrolls
    /// the lanes to the rows its drawn slices stand in, the action span's
    /// ahead of a step's own.
    fn reveal_chosen(&mut self, cx: &mut Context<Self>) {
        let (time, span, rows) = {
            let model = self.model.read(cx);
            let (Some(profile), Some(layout), Some((_, item))) =
                (model.profile(), model.lane_layout(), model.chosen(cx))
            else {
                return;
            };
            let rows: Vec<Row> = item
                .slices()
                .into_iter()
                .filter_map(|at| {
                    let lane = layout.lanes.iter().find(|lane| lane.track == at.track())?;
                    Some(Row::of(lane.top, profile.slice(at).depth))
                })
                .collect();
            (item.time(profile), profile.span(), rows)
        };
        let regions = self.regions(cx);
        match self.fitted {
            true => self.view = framing(time, span, regions.time_w),
            false => self.framed_on_fit = Some(time),
        }
        self.follow = Follow::Released;
        if let Some(y) = scroll_revealing_rows(&rows, self.lane_scroll(cx), regions.flame_h()) {
            self.scroll_to(y, cx);
        }
    }

    /// Rebuilds the slice scene along with the overlay; every change to what the
    /// slices show routes through here, and a change that leaves them alone
    /// notifies alone, so the slice scene replays from its cache.
    pub(crate) fn invalidate_canvas(&self, cx: &mut Context<Self>) {
        self.canvas.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    /// Pages the time view to show the playhead again where it has played out
    /// of sight. Only a playing clock moves the view, and neither while a drag
    /// holds it nor once the reader has moved it ([`Follow::Released`]), so a
    /// reader exploring the timeline keeps where they are.
    fn follow_playhead(&mut self, cx: &mut Context<Self>) {
        if !self.fitted || !matches!(self.drag, TimelineDrag::None) {
            return;
        }
        let (span, at_us) = {
            let model = self.model.read(cx);
            let playing = model.clock().is_some_and(|clock| {
                clock
                    .read(cx)
                    .timeline()
                    .is_some_and(|timeline| timeline.is_playing())
            });
            if !playing {
                return;
            }
            let (Some(profile), Some(at_us)) = (model.profile(), model.playhead_us(cx)) else {
                return;
            };
            (profile.span(), at_us)
        };
        if !(span.start_us..=span.end_us).contains(&at_us) {
            return;
        }
        let time_w = self.regions(cx).time_w;
        let x = self.view.x_of(at_us);
        if !self.follow.pages((0.0..=time_w).contains(&x)) {
            return;
        }
        self.view.pan_px(time_w * PLAYHEAD_FOLLOW_INSET - x);
        self.view.clamp_to(span, time_w, EDGE_MARGIN_PX);
        self.invalidate_canvas(cx);
    }

    /// Drops the state that addresses the document being replaced, so nothing
    /// survives the swap still pointing into the old profile.
    pub(crate) fn reset_for_new_profile(&mut self) {
        self.slice_draw = None;
        self.framed_on_fit = None;
        self.scroll = RowAnchor::Start;
        self.counter_scroll_held = 0.0;
        self.drag = TimelineDrag::None;
        self.follow = Follow::Playhead;
    }

    /// Releases the time view from following the playhead where the reader
    /// moved it from `before`.
    fn release_if_moved(&mut self, before: TimeView) {
        if self.view != before {
            self.follow = Follow::Released;
        }
    }

    /// The counters strip and gutter width the timeline is split by, held by the
    /// owning view because its divider and gutter handle drag them.
    fn shell_layout(&self, cx: &App) -> (CountersStrip, f32) {
        match self.shell.upgrade() {
            Some(shell) => {
                let s = shell.read(cx);
                (s.counters, s.gutter_width)
            }
            None => (
                CountersStrip::Open(DEFAULT_COUNTERS_HEIGHT),
                DEFAULT_GUTTER_WIDTH,
            ),
        }
    }

    /// The viewport bounds last captured during paint.
    pub(crate) fn viewport(&self) -> Bounds<Pixels> {
        self.viewport
    }

    /// [`Regions`] for the current viewport, strip and gutter sizes, the loaded
    /// profile's counter track count, and the flame lanes' total height.
    fn regions(&self, cx: &App) -> Regions {
        let (n_counters, flame_total) = {
            let model = self.model.read(cx);
            let n_counters = model.profile().map_or(0, |p| track_count(p));
            let flame_total = model.lane_layout().map(|l| l.total_height).unwrap_or(0.0);
            (n_counters, flame_total)
        };
        let (counters, gutter_width) = self.shell_layout(cx);
        Regions::new(
            self.viewport,
            n_counters,
            counters,
            gutter_width,
            flame_total,
        )
    }

    /// The bars for the timeline's view state; all `None` while no profile with a
    /// lane layout is loaded.
    fn scroll_bars(&self, cx: &App) -> TimelineBars {
        let (span, flame_total) = {
            let model = self.model.read(cx);
            let (Some(profile), Some(lanes)) = (model.profile(), model.lane_layout()) else {
                return TimelineBars::default();
            };
            (profile.span(), lanes.total_height)
        };
        scrollbar_geom(
            span,
            self.view,
            flame_total,
            self.regions(cx),
            self.lane_scroll(cx),
            self.counter_scroll(cx),
        )
    }

    /// The chart the lane scroll addresses; `None` while no profile with a lane
    /// layout is loaded. Reads the shell for its layout, so it must not run while
    /// the shell is leased.
    fn anchored_chart(&self, cx: &App) -> Option<AnchoredChart> {
        let model = self.model.read(cx);
        let (lanes, chrome) = (model.lane_layout()?, model.group_chrome()?);
        let held = anchor_offset(chart_rows(&chrome), self.scroll);
        let ceiling = max_lane_scroll(&lanes, &self.regions(cx));
        Some(AnchoredChart {
            chrome,
            held,
            ceiling,
        })
    }

    /// How far the lanes are scrolled up, in pixels. The sole reading of the
    /// vertical offset, so what is painted, what is culled, and what is
    /// hit-tested agree.
    fn lane_scroll(&self, cx: &App) -> f32 {
        self.anchored_chart(cx)
            .map_or(0.0, |chart| drawn_scroll(chart.held, chart.ceiling))
    }

    /// Re-anchors the lane scroll to the row a gesture asking for `y` pixels down
    /// the chart lands in.
    fn scroll_to(&mut self, y: f32, cx: &App) {
        let Some(chart) = self.anchored_chart(cx) else {
            return;
        };
        let Some(target) = scroll_request(chart.held, chart.ceiling, y) else {
            return;
        };
        self.scroll = anchor_at(chart_rows(&chart.chrome), target);
    }

    /// Scrolls the lanes down by a gesture's `dy` pixels.
    fn scroll_by(&mut self, dy: f32, cx: &App) {
        self.scroll_to(self.lane_scroll(cx) - dy, cx);
    }

    /// How far the counters strip is scrolled up, in pixels. The sole reading of
    /// the strip's offset, so its plots, their gutter names, and its bar agree.
    fn counter_scroll(&self, cx: &App) -> f32 {
        drawn_scroll(
            self.counter_scroll_held,
            self.regions(cx).max_counter_scroll(),
        )
    }

    /// Moves the counters strip to a gesture asking for `y` pixels down its lanes.
    fn scroll_counters_to(&mut self, y: f32, cx: &App) {
        let ceiling = self.regions(cx).max_counter_scroll();
        if let Some(target) = scroll_request(self.counter_scroll_held, ceiling, y) {
            self.counter_scroll_held = target;
        }
    }

    /// Scrolls the counters strip down by a gesture's `dy` pixels.
    fn scroll_counters_by(&mut self, dy: f32, cx: &App) {
        self.scroll_counters_to(self.counter_scroll(cx) - dy, cx);
    }

    /// Clears the slice tooltip and crosshair, so a tab switch drops them
    /// instead of leaving them until the next mouse move.
    pub(crate) fn clear_hover(&mut self) {
        self.hover = None;
        self.crosshair = None;
    }

    /// Whether `pos` lies over a time-axis region the crosshair shows on: the
    /// flame lanes, and the counters strip where the profile has counters.
    fn in_content(&self, pos: Point<Pixels>, cx: &App) -> bool {
        if self.model.read(cx).lane_layout().is_none() {
            return false;
        }
        let regions = self.regions(cx);
        regions.in_flame(pos) || regions.in_counters(pos)
    }

    /// The slice under a window-space cursor: the box the last paint drew there,
    /// found in the memoized draw list, so a hover names what is on screen, and
    /// the document it was drawn from. `None` until the first paint has culled a
    /// draw list.
    fn slice_at(&self, pos: Point<Pixels>, cx: &App) -> Option<HoveredSlice> {
        let regions = self.regions(cx);
        if !regions.in_flame(pos) {
            return None;
        }
        self.slice_draw.as_ref()?.hit(
            f32::from(pos.x) - regions.content_left,
            f32::from(pos.y) - regions.flame_top,
        )
    }

    /// The group header band or show-more row at `pos`, if the press lands on one
    /// (across the full chart width, gutter included).
    fn group_row_at(&self, pos: Point<Pixels>, cx: &App) -> Option<GroupHit> {
        let model = self.model.read(cx);
        model.lane_layout()?;
        let chrome = model.group_chrome()?;
        let regions = self.regions(cx);
        let (_, gutter_width) = self.shell_layout(cx);
        let x = f32::from(pos.x);
        let y = f32::from(pos.y);
        let gutter_left = regions.content_left - gutter_width;
        if x < gutter_left
            || x >= regions.content_right
            || y < regions.flame_top
            || y >= regions.flame_bottom
        {
            return None;
        }
        let content_y = y - regions.flame_top + self.lane_scroll(cx);
        let hits = |top: f32| content_y >= top && content_y < top + GROUP_BAND_HEIGHT;
        let band = chrome.bands.iter().find(|b| hits(b.top))?;
        let icon_left = regions.content_left - GROUP_ICON - 2.0 * SLICE_TEXT_PAD;
        if band.capped && !band.collapsed && x >= icon_left && x < regions.content_left {
            Some(GroupHit::ShowAll(band.group))
        } else {
            Some(GroupHit::Collapse(band.group))
        }
    }

    /// The track whose gutter label lies under `pos`, if the press hit the
    /// track-label column. `None` elsewhere or over a lane's empty tail.
    fn track_label_at(&self, pos: Point<Pixels>, cx: &App) -> Option<usize> {
        let layout = self.model.read(cx).lane_layout()?;
        let regions = self.regions(cx);
        let (_, gutter_width) = self.shell_layout(cx);
        let x = f32::from(pos.x);
        let y = f32::from(pos.y);
        let gutter_left = regions.content_left - gutter_width;
        if x < gutter_left
            || x >= regions.content_left
            || y < regions.flame_top
            || y >= regions.flame_bottom
        {
            return None;
        }
        let content_y = y - regions.flame_top + self.lane_scroll(cx);
        layout
            .lanes
            .iter()
            .find(|l| content_y >= l.top && content_y < l.top + l.height)
            .map(|l| l.track)
    }

    /// Answers [`SelectAllTracks`] by selecting every currently visible track.
    fn select_all_visible(&mut self, _: &SelectAllTracks, _: &mut Window, cx: &mut Context<Self>) {
        if self.model.update(cx, |m, cx| m.select_all_visible(cx)) {
            self.invalidate_canvas(cx);
        }
    }

    /// Answers [`ClearTimeSelection`] by backing out of the innermost gesture: a
    /// region drag under way is abandoned, failing that what is chosen of the
    /// critical path is let go of, and failing that the aggregate widens back
    /// to the whole timeline.
    fn clear_time_selection(
        &mut self,
        _: &ClearTimeSelection,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let backed_out = self.drag.cancel_region()
            || self.model.update(cx, |m, cx| {
                m.unchoose(cx) || m.set_time_selection(TimeSelection::default(), cx)
            });
        if backed_out {
            cx.notify();
        }
    }

    /// The instant a window-space cursor points at; `None` while no profile is
    /// loaded.
    fn time_at(&self, pos: Point<Pixels>, cx: &App) -> Option<i64> {
        let profile = self.model.read(cx).profile()?;
        let x_px = f32::from(pos.x) - self.regions(cx).content_left;
        Some(time_at_x(self.view, profile.span(), x_px))
    }

    /// Begins a time-region drag if the press asks for one — anywhere in the
    /// ruler band, or a shift-press over the flame lanes — reporting whether it
    /// took the press. [`region_press`] resolves what it edits.
    fn press_time_region(&mut self, e: &MouseDownEvent, cx: &App) -> bool {
        let regions = self.regions(cx);
        let x = f32::from(e.position.x);
        let in_ruler = regions.in_ruler(e.position);
        if !(in_ruler || (e.modifiers.shift && regions.in_flame(e.position))) {
            return false;
        }
        let Some(profile) = self.model.read(cx).profile() else {
            return false;
        };
        let bounds = profile.span();
        let at = time_at_x(self.view, bounds, x - regions.content_left);
        let (edit, anchor_us) = {
            let sel = self.model.read(cx).time_selection();
            let grab = if in_ruler {
                Grab::Ruler(region_grab(sel, self.view, regions, bounds, at, x))
            } else {
                Grab::Lanes
            };
            let ctrl = e.modifiers.control || e.modifiers.platform;
            region_press(sel, at, ctrl, grab)
        };
        self.crosshair = None;
        self.drag = TimelineDrag::TimeRegion(RegionDrag {
            edit,
            anchor_us,
            cursor_us: at,
            press_x: x,
            moved: false,
        });
        true
    }

    /// Lands a finished time-region drag on the model. Only a release carries
    /// the click that clears the selection; an abandoned drag lands what it was
    /// already showing.
    fn commit_time_region(&mut self, drag: RegionDrag, end: DragEnd, cx: &mut Context<Self>) {
        let standing = self.model.read(cx).time_selection();
        let selection = match end {
            DragEnd::Released => drag.released(standing),
            DragEnd::Abandoned => drag.applied(standing),
        };
        self.model
            .update(cx, |m, cx| m.set_time_selection(selection, cx));
    }

    /// Begins a timeline drag at `pos`: the lane vertical or time horizontal
    /// scroll thumb/track if the press hit one, otherwise a pan.
    fn press(&mut self, pos: Point<Pixels>, cx: &App) {
        self.crosshair = None;
        let bars = self.scroll_bars(cx);
        if let Some(bar) = bars.flame
            && bar.track_hit(pos)
        {
            let grab = bar.grab(pos);
            self.drag = TimelineDrag::VThumb(grab);
            self.scroll_thumb_to(pos, grab, cx);
            return;
        }
        if let Some(bar) = bars.time
            && bar.track_hit(pos)
        {
            let grab = bar.grab(pos);
            self.drag = TimelineDrag::HThumb(grab);
            self.pan_thumb_to(pos, grab, cx);
            return;
        }
        if let Some(bar) = bars.counters
            && bar.track_hit(pos)
        {
            let grab = bar.grab(pos);
            self.drag = TimelineDrag::CountersThumb(grab);
            self.counters_thumb_to(pos, grab, cx);
            return;
        }
        self.drag = TimelineDrag::Pan(PanDrag::at(pos));
        self.last_mouse = pos;
    }

    /// Maps the lane vertical thumb drag back to a scroll offset.
    fn scroll_thumb_to(&mut self, pos: Point<Pixels>, grab: f32, cx: &App) {
        if let Some(offset) = thumb_offset(self.scroll_bars(cx).flame, pos, grab) {
            self.scroll_to(offset, cx);
        }
    }

    /// Maps the counters vertical thumb drag back to a scroll offset.
    fn counters_thumb_to(&mut self, pos: Point<Pixels>, grab: f32, cx: &App) {
        if let Some(offset) = thumb_offset(self.scroll_bars(cx).counters, pos, grab) {
            self.scroll_counters_to(offset, cx);
        }
    }

    /// Maps a horizontal thumb drag back to a pan start time, then clamps the view.
    fn pan_thumb_to(&mut self, pos: Point<Pixels>, grab: f32, cx: &App) {
        if self.model.read(cx).lane_layout().is_none() {
            return;
        }
        let Some(profile) = self.model.read(cx).profile().cloned() else {
            return;
        };
        let span = profile.span();
        let regions = self.regions(cx);
        let Some(offset) = thumb_offset(self.scroll_bars(cx).time, pos, grab) else {
            return;
        };
        // The bar's offset is measured from the span start less the reachable
        // margin, so map it back through the same shift before clamping.
        let margin_us = EDGE_MARGIN_PX / self.view.px_per_us as f32;
        self.view.start_us = (span.start_us as f32 - margin_us) as f64 + offset as f64;
        self.view.clamp_to(span, regions.time_w, EDGE_MARGIN_PX);
    }

    fn on_scroll_wheel(&mut self, e: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.model.read(cx).lane_layout().is_none() {
            return;
        }
        let Some(profile) = self.model.read(cx).profile().cloned() else {
            return;
        };
        let span = profile.span();
        let regions = self.regions(cx);
        let time_w = regions.time_w;
        let content_left = regions.content_left;
        let before = self.view;
        let delta = e.delta.pixel_delta(px(LINE_SCROLL_PX));
        let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        if e.modifiers.control || e.modifiers.platform {
            let rel_x = f32::from(e.position.x) - content_left;
            let dy_lines = dy / LINE_SCROLL_PX;
            self.view
                .zoom_at(rel_x, ZOOM_PER_NOTCH.powf(dy_lines as f64));
            self.view.px_per_us = self
                .view
                .px_per_us
                .clamp(fit_px_per_us(span, time_w, EDGE_MARGIN_PX), MAX_PX_PER_US);
            self.view.clamp_to(span, time_w, EDGE_MARGIN_PX);
        } else {
            let (pan_dx, scroll_dy) = if e.modifiers.shift {
                (dx + dy, 0.0)
            } else {
                (dx, dy)
            };
            // Both sections share the time axis, so a wheel pans either; the
            // vertical half goes to whichever the cursor is over.
            match regions.in_counters(e.position) {
                true => self.scroll_counters_by(scroll_dy, cx),
                false => self.scroll_by(scroll_dy, cx),
            }
            self.view.pan_px(pan_dx);
            self.view.clamp_to(span, time_w, EDGE_MARGIN_PX);
        }
        self.release_if_moved(before);
        self.invalidate_canvas(cx);
    }

    /// Tracks the cursor for the slice tooltip. A move outside the lanes that
    /// leaves nothing to clear costs no repaint.
    fn update_hover(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let was_hovering = self.hover.is_some();
        self.hover = self.regions(cx).in_flame(pos).then_some(pos);
        if was_hovering || self.hover.is_some() {
            cx.notify();
        }
    }

    /// Tracks the cursor for the tooltip and the crosshair while not dragging.
    /// Only the overlay reads these, so the slices need no rebuild.
    fn on_mouse_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.drag, TimelineDrag::None) {
            return;
        }
        let in_content = self.in_content(e.position, cx);
        let was_crosshair = self.crosshair.is_some();
        self.crosshair = in_content.then_some(e.position);
        self.update_hover(e.position, cx);
        if in_content || was_crosshair {
            cx.notify();
        }
    }

    /// The timeline's mouse/scroll/hover wiring: a press on a group header folds
    /// or shows-all it, on a track label selects that track, on a scroll
    /// thumb/track drives that bar, on the ruler (or with shift on the lanes)
    /// selects time regions, anywhere else pans, a click there letting go of
    /// what is chosen of the critical path; the wheel zooms/pans/scrolls,
    /// and a move updates the crosshair and hover. Holds the content focus and
    /// the chart's actions, so after a press here [`SelectAllTracks`] selects all
    /// tracks and [`ClearTimeSelection`] widens the aggregate to the whole
    /// timeline.
    fn content_handlers(&self, root: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        root.key_context(TIMELINE_KEY_CONTEXT)
            .track_focus(&self.content_focus)
            .on_action(cx.listener(Self::select_all_visible))
            .on_action(cx.listener(Self::clear_time_selection))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, window, cx| {
                    // Take content focus so `SelectAllTracks` dispatches here after
                    // a chart interaction (a click may have moved focus to the filter).
                    this.content_focus.focus(window, cx);
                    if let Some(hit) = this.group_row_at(e.position, cx) {
                        this.model.update(cx, |m, _| match hit {
                            GroupHit::Collapse(g) => m.toggle_group_collapsed(g),
                            GroupHit::ShowAll(g) => m.toggle_group_show_all(g),
                        });
                        this.invalidate_canvas(cx);
                        return;
                    }
                    if let Some(track) = this.track_label_at(e.position, cx) {
                        let ctrl = e.modifiers.control || e.modifiers.platform;
                        this.model.update(cx, |m, cx| {
                            m.select_track(track, e.modifiers.shift, ctrl, cx)
                        });
                        this.invalidate_canvas(cx);
                        return;
                    }
                    if this.press_time_region(e, cx) {
                        cx.notify();
                        return;
                    }
                    this.press(e.position, cx);
                    this.invalidate_canvas(cx);
                }),
            )
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered {
                    let cleared = this.crosshair.take().is_some() | this.hover.take().is_some();
                    if cleared {
                        cx.notify();
                    }
                }
            }))
    }
}

impl Focusable for Timeline {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.content_focus.clone()
    }
}

impl Render for Timeline {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.follow_playhead(cx);
        let (profile, lanes) = {
            let model = self.model.read(cx);
            Clock::keep_pace(model.clock(), window, cx);
            let Some(profile) = model.profile() else {
                return div().into_any_element();
            };
            let Some(lanes) = model.lane_layout() else {
                return div().into_any_element();
            };
            (profile.clone(), lanes)
        };
        let palette = Palette::from_theme(&bviz_ui_theme::theme(window, cx));
        let entity = cx.entity();
        let slices = cached_layer(self.canvas.clone());
        let overlay = overlay_canvas(overlay_paint(profile, lanes, entity, palette));
        let root = div()
            .id("trace-content")
            .relative()
            .size_full()
            .overflow_hidden()
            .child(slices)
            .child(overlay);
        self.content_handlers(root, cx).into_any_element()
    }
}

/// Routes an in-progress timeline drag to its target: panning the chart, moving
/// a scroll thumb, or stretching a time region.
fn timeline_on_drag(
    timeline: &mut Timeline,
    position: Point<Pixels>,
    _window: &mut Window,
    cx: &mut Context<Timeline>,
) {
    let moves_slices = !matches!(timeline.drag, TimelineDrag::TimeRegion(_));
    let before = timeline.view;
    match timeline.drag {
        TimelineDrag::Pan(mut pan) => {
            pan.follow(position);
            timeline.drag = TimelineDrag::Pan(pan);
            let dx = f32::from(position.x) - f32::from(timeline.last_mouse.x);
            let dy = f32::from(position.y) - f32::from(timeline.last_mouse.y);
            if timeline.model.read(cx).lane_layout().is_none() {
                return;
            }
            let Some(profile) = timeline.model.read(cx).profile().cloned() else {
                return;
            };
            let span = profile.span();
            let regions = timeline.regions(cx);
            timeline.view.pan_px(dx);
            timeline.view.clamp_to(span, regions.time_w, EDGE_MARGIN_PX);
            timeline.scroll_by(dy, cx);
            timeline.last_mouse = position;
        }
        TimelineDrag::VThumb(grab) => timeline.scroll_thumb_to(position, grab, cx),
        TimelineDrag::HThumb(grab) => timeline.pan_thumb_to(position, grab, cx),
        TimelineDrag::CountersThumb(grab) => timeline.counters_thumb_to(position, grab, cx),
        TimelineDrag::TimeRegion(mut drag) => {
            if let Some(at) = timeline.time_at(position, cx) {
                drag.cursor_us = at;
                drag.moved |= (f32::from(position.x) - drag.press_x).abs() > MIN_DRAG_PX;
                timeline.drag = TimelineDrag::TimeRegion(drag);
            }
        }
        TimelineDrag::None => {}
    }
    timeline.release_if_moved(before);
    timeline.update_hover(position, cx);
    if moves_slices {
        timeline.invalidate_canvas(cx);
    } else {
        cx.notify();
    }
}

fn timeline_end_drag(
    timeline: &mut Timeline,
    _: Point<Pixels>,
    end: DragEnd,
    _: &mut Window,
    cx: &mut Context<Timeline>,
) {
    match std::mem::replace(&mut timeline.drag, TimelineDrag::None) {
        TimelineDrag::TimeRegion(drag) => {
            timeline.commit_time_region(drag, end, cx);
            cx.notify();
        }
        TimelineDrag::Pan(pan) if pan.clicked(end) => {
            timeline.model.update(cx, |model, cx| model.unchoose(cx));
            timeline.invalidate_canvas(cx);
        }
        _ => timeline.invalidate_canvas(cx),
    }
}

/// The layer painting the timeline's slices and chrome, from the view state,
/// document, and lanes it reads through `parent`.
struct SliceCanvas {
    parent: WeakEntity<Timeline>,
}

impl Render for SliceCanvas {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(parent) = self.parent.upgrade() else {
            return div().into_any_element();
        };
        let (profile, lanes, chrome, counters, gutter_width) = {
            let timeline = parent.read(cx);
            let model = timeline.model.read(cx);
            let Some(profile) = model.profile() else {
                return div().into_any_element();
            };
            let (Some(lanes), Some(chrome)) = (model.lane_layout(), model.group_chrome()) else {
                return div().into_any_element();
            };
            let (counters, gutter_width) = timeline.shell_layout(cx);
            (profile.clone(), lanes, chrome, counters, gutter_width)
        };
        let palette = Palette::from_theme(&bviz_ui_theme::theme(window, cx));
        let prepaint = fit_prepaint(
            profile.clone(),
            lanes.clone(),
            parent.clone(),
            counters,
            gutter_width,
        );
        let paint = slice_paint(profile, lanes, chrome, parent, palette);
        canvas(prepaint, paint)
            .absolute()
            .inset_0()
            .size_full()
            .into_any_element()
    }
}

/// Prepaint fit: capture the painted viewport bounds, then fit the time view to
/// the span before the first paint and, on a later width change, rescale the zoom
/// relative to the window so a resize stays invertible; finally hold the fit as
/// the zoom floor and clamp the pan to the span.
fn fit_prepaint(
    profile: Arc<Profile>,
    lanes: Rc<LaneLayout>,
    parent: Entity<Timeline>,
    counters: CountersStrip,
    gutter_width: f32,
) -> impl FnOnce(Bounds<Pixels>, &mut Window, &mut App) {
    move |bounds: Bounds<Pixels>, _: &mut Window, cx: &mut App| {
        let old_bounds = parent.read(cx).viewport;
        let flame_total = lanes.total_height;
        let regions_of = |b| {
            Regions::new(
                b,
                track_count(&profile),
                counters,
                gutter_width,
                flame_total,
            )
        };
        let regions = regions_of(bounds);
        let time_w = regions.time_w;
        let old_time_w = regions_of(old_bounds).time_w;
        let span = profile.span();
        parent.update(cx, |timeline, _| {
            timeline.viewport = bounds;
            if !timeline.fitted {
                timeline.view = match timeline.framed_on_fit.take() {
                    Some(time) => framing(time, span, time_w),
                    None => TimeView::fit(span, time_w, EDGE_MARGIN_PX),
                };
                timeline.fitted = true;
            } else if old_bounds.size.width > px(0.) && (old_time_w - time_w).abs() > f32::EPSILON {
                timeline
                    .view
                    .rescale_to_width(span, old_time_w, time_w, EDGE_MARGIN_PX);
            }
            timeline.view.px_per_us =
                timeline
                    .view
                    .px_per_us
                    .max(fit_px_per_us(span, time_w, EDGE_MARGIN_PX));
            timeline.view.clamp_to(span, time_w, EDGE_MARGIN_PX);
        });
    }
}

/// Share of the time column left clear on each side of a revealed stretch of
/// time, so what ran around it stays in view.
const REVEAL_MARGIN_SHARE: f32 = 0.1;

/// The view showing `time` across a time column `width_px` wide, clear of its
/// sides by [`REVEAL_MARGIN_SHARE`], held within the zoom range and the pan
/// bounds of the profile's whole `span`. Where the zoom ceiling holds `time`
/// narrower than that, it stands centred.
fn framing(time: TimeSpan, span: TimeSpan, width_px: f32) -> TimeView {
    let mut view = TimeView::fit(time, width_px, width_px * REVEAL_MARGIN_SHARE);
    let bounded = view
        .px_per_us
        .clamp(fit_px_per_us(span, width_px, EDGE_MARGIN_PX), MAX_PX_PER_US);
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
struct Row {
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

/// Where to scroll lanes standing `scroll` pixels down the chart in a region
/// `view_h` tall to show `rows`: every one where the band they span fits in
/// the region, else the first. `None` where that shows already.
fn scroll_revealing_rows(rows: &[Row], scroll: f32, view_h: f32) -> Option<f32> {
    let first = rows.first()?;
    let top = rows.iter().map(|row| row.top).fold(first.top, f32::min);
    let bottom = rows
        .iter()
        .map(|row| row.bottom)
        .fold(first.bottom, f32::max);
    match bottom - top <= view_h {
        true => scroll_revealing(top, bottom - top, scroll, view_h),
        false => scroll_revealing(first.top, first.bottom - first.top, scroll, view_h),
    }
}

/// Where to scroll lanes standing `scroll` pixels down the chart in a region
/// `view_h` tall so that the band `height` tall at `top` shows in full, moving
/// them no further than that takes; its top where it is taller than the
/// region. `None` where it shows in full already.
fn scroll_revealing(top: f32, height: f32, scroll: f32, view_h: f32) -> Option<f32> {
    let bottom = top + height;
    if top >= scroll && bottom <= scroll + view_h {
        return None;
    }
    match top < scroll || height > view_h {
        true => Some(top),
        false => Some(bottom - view_h),
    }
}

/// Width of the line around each slice standing for what is chosen of the
/// critical path, drawn outside the slice so its fill and label stay whole.
const CHOSEN_OUTLINE_PX: f32 = 2.0;

/// Outlines the slices standing for what is chosen of the critical path that
/// `drawn` holds, within the flame region of `regions`.
fn paint_chosen_slices(
    chosen: &[SliceAt],
    drawn: &[VisibleSlice],
    regions: Regions,
    palette: &Palette,
    window: &mut Window,
) {
    let flame = Bounds::new(
        point(px(regions.content_left), px(regions.flame_top)),
        size(
            px(regions.time_right - regions.content_left),
            px(regions.flame_h()),
        ),
    );
    window.with_content_mask(Some(ContentMask { bounds: flame }), |window| {
        for v in drawn {
            if !chosen
                .iter()
                .any(|at| at.track() == v.track && at.slice() == v.slice)
            {
                continue;
            }
            let around = Bounds::new(
                point(
                    px(regions.content_left + v.x - CHOSEN_OUTLINE_PX),
                    px(regions.flame_top + v.y - CHOSEN_OUTLINE_PX),
                ),
                size(
                    px(v.w + 2.0 * CHOSEN_OUTLINE_PX),
                    px(v.h + 2.0 * CHOSEN_OUTLINE_PX),
                ),
            );
            window.paint_quad(
                gpui::outline(around, palette.label, gpui::BorderStyle::Solid)
                    .border_widths(px(CHOSEN_OUTLINE_PX)),
            );
        }
    });
}

/// The timeline's culled draw list, reusing the [`SliceDrawCache`] in `cache`
/// unless a key changed, so a repaint that leaves the profile, lane layout, time
/// view, and viewport unchanged reuses the list rather than re-culling.
fn cached_slices(
    cache: &mut Option<SliceDrawCache>,
    profile: &Arc<Profile>,
    lanes: &Rc<LaneLayout>,
    view: TimeView,
    viewport: Viewport,
) -> Rc<Vec<VisibleSlice>> {
    if let Some(c) = cache.as_ref()
        && Arc::ptr_eq(&c.profile, profile)
        && Rc::ptr_eq(&c.lanes, lanes)
        && c.view == view
        && c.viewport == viewport
    {
        return c.slices.clone();
    }
    cache
        .insert(SliceDrawCache::new(
            profile.clone(),
            lanes.clone(),
            view,
            viewport,
        ))
        .slices
        .clone()
}

/// The slice layer's paint closure: the flame timeline — gridlines, lanes and
/// slices, phase markers, gutter, scroll bars, ruler, and the counter plots —
/// reading the live view state through `parent`.
fn slice_paint(
    profile: Arc<Profile>,
    lanes: Rc<LaneLayout>,
    chrome: Rc<GroupChrome>,
    parent: Entity<Timeline>,
    palette: Palette,
) -> impl FnOnce(Bounds<Pixels>, (), &mut Window, &mut App) {
    move |bounds, _, window, cx| {
        let layout = &*lanes;
        let n_counters = track_count(&profile);
        let flame_total = layout.total_height;
        let (
            tv,
            scroll,
            counter_scroll,
            gutter_width,
            selected,
            shared_names,
            regions,
            show_counters,
            vp,
        ) = {
            let timeline = parent.read(cx);
            let tv = timeline.view;
            let scroll = timeline.lane_scroll(cx);
            let counter_scroll = timeline.counter_scroll(cx);
            let (counters, gutter_width) = timeline.shell_layout(cx);
            let selected = timeline.model.read(cx).selected_tracks().to_vec();
            let shared_names = timeline.model.read(cx).shared_track_names().to_vec();
            let regions = Regions::new(bounds, n_counters, counters, gutter_width, flame_total);
            let vp = Viewport {
                width: regions.time_w,
                height: regions.flame_h(),
                scroll_y: scroll,
            };
            let show_counters = regions.shows_counters();
            (
                tv,
                scroll,
                counter_scroll,
                gutter_width,
                selected,
                shared_names,
                regions,
                show_counters,
                vp,
            )
        };
        let slices = parent.update(cx, |timeline, _| {
            cached_slices(&mut timeline.slice_draw, &profile, &lanes, tv, vp)
        });

        let ticks = ruler_ticks(tv, regions.time_w);
        paint_gridlines(
            &ticks,
            regions.content_left,
            regions.flame_top,
            regions.flame_bottom,
            &palette,
            window,
        );
        if show_counters {
            paint_gridlines(
                &ticks,
                regions.content_left,
                regions.counters_top,
                regions.counters_top + regions.counters_h(),
                &palette,
                window,
            );
        }

        let flame_mask = ContentMask {
            bounds: regions.flame_rect(),
        };
        window.with_content_mask(Some(flame_mask), |window| {
            for lane in layout.lanes.iter().skip(1) {
                let ry = regions.flame_top + lane.top - METRICS.lane_gap / 2.0 - scroll;
                window.paint_quad(fill(
                    Bounds::new(
                        point(px(regions.content_left), px(ry)),
                        size(px(regions.time_w), px(1.0)),
                    ),
                    palette.gridline,
                ));
            }
            for header in chart_rows(&chrome).filter(|row| row.kind.is_header()) {
                let y = regions.flame_top + header.top - scroll;
                window.paint_quad(fill(
                    Bounds::new(
                        point(px(regions.content_left), px(y)),
                        size(px(regions.time_w), px(GROUP_BAND_HEIGHT)),
                    ),
                    palette.header_band,
                ));
            }
            for vs in slices.iter() {
                let track = &profile.threads()[vs.track];
                let slice = profile.slices(track)[vs.slice];
                paint_slice(
                    &profile,
                    &slice,
                    vs.x,
                    vs.y + regions.flame_top,
                    vs.w,
                    vs.h,
                    regions.content_left,
                    &palette,
                    window,
                    cx,
                );
            }
        });

        if show_counters {
            let working_lanes_pending = {
                let working_lanes = parent.read(cx).model.read(cx).working_lanes();
                let strip = StripCounters::new(&profile, working_lanes);
                crate::counters::paint_graph(strip, tv, regions, counter_scroll, &palette, window);
                working_lanes.is_none()
            };
            if working_lanes_pending {
                crate::counters::paint_working_lanes_pending(
                    regions,
                    counter_scroll,
                    &palette,
                    window,
                    cx,
                );
            }
        }
        paint_markers(&profile, tv, regions, show_counters, &palette, window);
        scrollbar_geom(
            profile.span(),
            tv,
            flame_total,
            regions,
            scroll,
            counter_scroll,
        )
        .paint(&palette, window);

        paint_gutter(
            &profile,
            layout,
            &chrome,
            regions,
            gutter_width,
            scroll,
            counter_scroll,
            show_counters,
            &selected,
            &shared_names,
            &palette,
            window,
            cx,
        );

        paint_ruler(&ticks, regions, bounds, &palette, window, cx);
        paint_marker_band(&profile, tv, regions, &palette, window, cx);
    }
}

/// A selected region's slice of the time axis, in window x.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RegionPixels {
    left: f32,
    right: f32,
    starts: bool,
    ends: bool,
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
fn selection_pixels(sel: &TimeSelection, tv: TimeView, regions: Regions) -> Vec<RegionPixels> {
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
enum Grab {
    /// A press in the ruler band, holding what [`region_grab`] found for it.
    Ruler(Option<(RegionEdit, i64)>),
    /// A press over the flame lanes.
    Lanes,
}

/// The edit a press on the instant `at` resolves to, with the instant to anchor
/// its drag at. Ctrl toggles the pressed region's membership: it takes away
/// whichever region the press took hold of, and where it held none it unions the
/// drag in. Without ctrl the press keeps what it took hold of, and failing that
/// replaces the selection.
fn region_press(sel: &TimeSelection, at: i64, ctrl: bool, grab: Grab) -> (RegionEdit, i64) {
    if ctrl {
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
fn region_grab(
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

/// Alpha of the wash laid over unselected time: heavy enough that the selection
/// reads at a glance, light enough that the chart under it stays legible.
const VEIL_ALPHA: f32 = 0.62;

/// Alpha of the selection accent tinting a selected region's stretch of the
/// ruler band. The ruler's ticks and time labels are painted beneath it, so the
/// tint stays translucent enough to read them through while still marking the
/// stretch the regions are dragged in.
const RULER_BAND_ALPHA: f32 = 0.5;

/// Width of a region edge's hairline.
const EDGE_LINE_PX: f32 = 1.0;

/// Width of an edge's grip, spanning exactly the horizontal zone
/// [`EDGE_GRAB_PX`] gives a press either side of the edge to take hold of it.
const GRIP_WIDTH: f32 = 2.0 * EDGE_GRAB_PX;

/// Share of the ruler band's height a grip stands in, centred, so it reads as a
/// handle rather than a bar filling the band.
const GRIP_BAND_FRACTION: f32 = 0.55;

/// Paints `sel` over the timeline: a wash over the time outside it, an accent
/// fill and edge grips in the ruler band, and a hairline down each region edge.
/// Covers the time-axis sections the crosshair does, so a selection reads across
/// the lanes and the counter plots alike.
fn paint_time_selection(
    sel: &TimeSelection,
    tv: TimeView,
    regions: Regions,
    show_counters: bool,
    palette: &Palette,
    window: &mut Window,
) {
    if sel.is_empty() {
        return;
    }
    let painted = selection_pixels(sel, tv, regions);
    let quad = |x: f32, w: f32, top: f32, h: f32, color: Rgba, window: &mut Window| {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        window.paint_quad(fill(
            Bounds::new(point(px(x), px(top)), size(px(w), px(h))),
            color,
        ));
    };

    let veil = Rgba {
        a: VEIL_ALPHA,
        ..palette.background
    };
    let veil_span = |from: f32, to: f32, window: &mut Window| {
        quad(
            from,
            to - from,
            regions.flame_top,
            regions.flame_h(),
            veil,
            window,
        );
        if show_counters {
            quad(
                from,
                to - from,
                regions.counters_top,
                regions.counters_h(),
                veil,
                window,
            );
        }
    };
    let mut unselected_from = regions.content_left;
    for r in &painted {
        veil_span(unselected_from, r.left, window);
        unselected_from = r.right;
    }
    veil_span(unselected_from, regions.time_right, window);

    let band = regions.ruler_rect();
    let band_top = f32::from(band.origin.y);
    let band_h = f32::from(band.size.height);
    let tint = Rgba {
        a: RULER_BAND_ALPHA,
        ..palette.accent
    };
    for r in &painted {
        quad(r.left, r.right - r.left, band_top, band_h, tint, window);
    }

    let accent = palette.accent;
    let grip_h = band_h * GRIP_BAND_FRACTION;
    let grip_top = band_top + (band_h - grip_h) / 2.0;
    for r in &painted {
        // An end edge's line is inset by its own width, so both hairlines fall
        // inside the region they bound.
        for (on_axis, edge_x, line_inset) in
            [(r.starts, r.left, 0.0), (r.ends, r.right, EDGE_LINE_PX)]
        {
            if !on_axis {
                continue;
            }
            let line_x = edge_x - line_inset;
            quad(
                line_x,
                EDGE_LINE_PX,
                band_top,
                regions.flame_bottom - band_top,
                accent,
                window,
            );
            if show_counters {
                quad(
                    line_x,
                    EDGE_LINE_PX,
                    regions.counters_top,
                    regions.counters_h(),
                    accent,
                    window,
                );
            }
            quad(
                edge_x - EDGE_GRAB_PX,
                GRIP_WIDTH,
                grip_top,
                grip_h,
                accent,
                window,
            );
        }
    }
}

/// Whether the time view pages to keep a playing clock's playhead in sight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Follow {
    Playhead,
    /// The reader moved the view, which stays where they put it until the
    /// playhead is in sight again or a gesture moves the clock.
    Released,
}

impl Follow {
    /// Whether a frame showing the playhead, or not as `in_sight` says, pages
    /// the view to it; a playhead in sight is followed from there on.
    fn pages(&mut self, in_sight: bool) -> bool {
        if in_sight {
            *self = Follow::Playhead;
        }
        !in_sight && *self == Follow::Playhead
    }
}

/// Where a page brings the playhead back into view, as a share of the time
/// axis from its left edge: near the start, so it plays on across most of the
/// width before the next page.
const PLAYHEAD_FOLLOW_INSET: f32 = 0.1;

/// Width of the playhead's rule, broader than the crosshair's hairline so the
/// two read apart where they cross.
const PLAYHEAD_WIDTH_PX: f32 = 2.0;

/// Paints the playback time, profile instant `at_us`, as a rule down the
/// time-axis sections the crosshair covers, where it falls within the shown
/// time.
fn paint_playhead(
    at_us: i64,
    tv: TimeView,
    regions: Regions,
    show_counters: bool,
    palette: &Palette,
    window: &mut Window,
) {
    let x = regions.content_left + tv.x_of(at_us);
    if !(regions.content_left..=regions.time_right).contains(&x) {
        return;
    }
    let rule = |top: f32, height: f32, window: &mut Window| {
        window.paint_quad(fill(
            Bounds::new(
                point(px(x - PLAYHEAD_WIDTH_PX / 2.0), px(top)),
                size(px(PLAYHEAD_WIDTH_PX), px(height)),
            ),
            palette.playhead,
        ));
    };
    rule(regions.flame_top, regions.flame_h(), window);
    if show_counters {
        rule(regions.counters_top, regions.counters_h(), window);
    }
}

/// The overlay's paint closure: the time selection, the chosen step's
/// outlines, the playhead, the crosshair, the hover tooltip, and — while a drag
/// runs — the window-global drag handlers, from the selection, choice, clock,
/// crosshair, hover, and drag state it reads through `parent`.
fn overlay_paint(
    profile: Arc<Profile>,
    lanes: Rc<LaneLayout>,
    parent: Entity<Timeline>,
    palette: Palette,
) -> impl FnOnce(Bounds<Pixels>, &mut Window, &mut App) {
    move |bounds, window, cx| {
        let n_counters = track_count(&profile);
        let chosen_drawn = {
            let timeline = parent.read(cx);
            let chosen: Vec<SliceAt> = timeline
                .model
                .read(cx)
                .chosen(cx)
                .map(|(_, item)| item.slices())
                .unwrap_or_default();
            match (&timeline.slice_draw, chosen.is_empty()) {
                (Some(cache), false) => Some((chosen, cache.slices.clone())),
                _ => None,
            }
        };
        let (
            tv,
            counters,
            counter_scroll,
            gutter_width,
            selection,
            playhead,
            crosshair,
            hovered,
            dragging,
        ) = {
            let timeline = parent.read(cx);
            let tv = timeline.view;
            let counter_scroll = timeline.counter_scroll(cx);
            let (counters, gutter_width) = timeline.shell_layout(cx);
            let model = timeline.model.read(cx);
            let hovered = timeline.hover.and_then(|at| {
                let session = model.session().map(|session| session.read(cx));
                Some((at, slice_tooltip(&timeline.slice_at(at, cx)?, session)))
            });
            let selection = timeline.drag.preview(model.time_selection());
            (
                tv,
                counters,
                counter_scroll,
                gutter_width,
                selection,
                model.playhead_us(cx),
                timeline.crosshair,
                hovered,
                !matches!(timeline.drag, TimelineDrag::None),
            )
        };
        let flame_total = lanes.total_height;
        let regions = Regions::new(bounds, n_counters, counters, gutter_width, flame_total);
        let show_counters = regions.shows_counters();
        paint_time_selection(&selection, tv, regions, show_counters, &palette, window);
        if let Some((chosen, drawn)) = chosen_drawn {
            paint_chosen_slices(&chosen, &drawn, regions, &palette, window);
        }
        if let Some(at_us) = playhead {
            paint_playhead(at_us, tv, regions, show_counters, &palette, window);
        }
        if let Some(at) = crosshair
            && f32::from(at.x) >= regions.content_left
        {
            let readouts = match show_counters {
                true => {
                    let working_lanes = parent.read(cx).model.read(cx).working_lanes();
                    let strip = StripCounters::new(&profile, working_lanes);
                    counter_readouts(strip, tv, regions, at, counter_scroll)
                }
                false => Vec::new(),
            };
            paint_crosshair(
                &readouts,
                tv,
                regions,
                at,
                show_counters,
                &palette,
                window,
                cx,
            );
        }
        if let Some((at, lines)) = hovered {
            paint_tooltip_box(&lines, at, bounds, palette.tooltip, window, cx);
        }
        if dragging {
            track_drag(window, &parent, timeline_on_drag, timeline_end_drag);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn lanes_scroll_only_as_far_as_showing_a_lane_in_full_takes() {
        // Region 100 tall, scrolled 200 down.
        assert_eq!(scroll_revealing(220.0, 30.0, 200.0, 100.0), None);
        assert_eq!(scroll_revealing(150.0, 30.0, 200.0, 100.0), Some(150.0));
        assert_eq!(scroll_revealing(290.0, 30.0, 200.0, 100.0), Some(220.0));
        assert_eq!(scroll_revealing(400.0, 300.0, 200.0, 100.0), Some(400.0));
    }

    #[test]
    fn a_step_s_rows_show_together_where_they_fit_and_else_its_span_s_row() {
        let view_h = 10.0 * METRICS.row_height;
        let step = Row::of(0.0, 0);
        // On a lane far taller than the region, its slice deep in it.
        let span = Row::of(5.0 * view_h, 40);
        let y = scroll_revealing_rows(&[span, step], 0.0, view_h).expect("a scroll");
        assert!(
            span.top >= y && span.bottom <= y + view_h,
            "the span's row shows"
        );

        let near = Row::of(step.bottom + METRICS.lane_gap, 2);
        let y = scroll_revealing_rows(&[near, step], 5.0 * view_h, view_h).expect("a scroll");
        for row in [near, step] {
            assert!(row.top >= y && row.bottom <= y + view_h, "both rows show");
        }
        assert_eq!(scroll_revealing_rows(&[near, step], 0.0, view_h), None);
        assert_eq!(scroll_revealing_rows(&[], 0.0, view_h), None);
    }
    use bviz_invocation::Session;
    use bviz_trace_layout::lay_out_lanes;

    const TEST_TIDS: [i64; 2] = [1, 2];
    const TEST_VIEW: TimeView = TimeView {
        start_us: 0.0,
        px_per_us: 0.1,
    };
    const TEST_VIEWPORT: Viewport = Viewport {
        width: 1000.0,
        height: 500.0,
        scroll_y: 0.0,
    };

    /// One thread's name metadata followed by its `spans`, each `(start_us,
    /// dur_us)`, as `X` events named `t{tid}s{i}` in category `c{tid}s{i}` —
    /// unique across a profile of distinct tids, so either identifies both the
    /// thread and the slice within it.
    fn thread_events(tid: i64, spans: impl IntoIterator<Item = (i64, i64)>) -> Vec<String> {
        let mut events = vec![format!(
            r#"{{"name":"thread_name","ph":"M","tid":{tid},"args":{{"name":"skyframe-evaluator-execution-{tid}"}}}}"#,
        )];
        events.extend(spans.into_iter().enumerate().map(|(i, (start_us, dur_us))| {
            format!(
                r#"{{"cat":"c{tid}s{i}","name":"t{tid}s{i}","ph":"X","ts":{start_us},"dur":{dur_us},"tid":{tid}}}"#,
            )
        }));
        events
    }

    fn profile_of_events(events: Vec<String>) -> Arc<Profile> {
        let json = format!(r#"{{"traceEvents":[{}]}}"#, events.join(","));
        Arc::new(Profile::from_bytes(json.as_bytes()).expect("parse test profile"))
    }

    /// A profile of two threads, each carrying `per_thread` back-to-back
    /// millisecond slices.
    fn profile_of(per_thread: usize) -> Arc<Profile> {
        profile_of_events(
            TEST_TIDS
                .iter()
                .flat_map(|&tid| {
                    thread_events(tid, (0..per_thread as i64).map(|i| (i * 1000, 1000)))
                })
                .collect(),
        )
    }

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

    /// A draw list culled from `profile` over a viewport holding all of it.
    fn draw_cache(profile: &Arc<Profile>) -> SliceDrawCache {
        SliceDrawCache::new(
            profile.clone(),
            Rc::new(lay_out_lanes(profile, &METRICS)),
            TEST_VIEW,
            TEST_VIEWPORT,
        )
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

    #[test]
    fn a_hit_names_the_slice_drawn_there() {
        let profile = profile_of(3);
        let cache = draw_cache(&profile);
        assert_eq!(cache.slices.len(), TEST_TIDS.len() * 3);
        for vs in cache.slices.iter() {
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
            .iter()
            .map(|vs| vs.y + vs.h)
            .fold(0.0, f32::max);
        let first = cache.slices.first().expect("a culled slice");
        assert!(
            cache
                .hit(first.x + first.w / 2.0, below_every_box)
                .is_none()
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
        let [drawn] = cache.slices[..] else {
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
                "mnemonic: CppCompile",
                "configuration: k8-fastbuild",
            ]
        );
    }

    #[test]
    fn a_tooltip_shows_the_checksum_of_a_configuration_no_session_names() {
        let by_checksum = format!("configuration: {CONFIGURATION}");
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
        let [under, over] = cache.slices[..] else {
            panic!(
                "expected the fixture's two boxes, got {}",
                cache.slices.len()
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

    #[test]
    fn a_pan_let_go_of_within_the_click_threshold_is_a_click() {
        let press = point(px(100.), px(100.));
        let mut pan = PanDrag::at(press);
        pan.follow(point(px(100. + MIN_DRAG_PX), px(100. - MIN_DRAG_PX)));
        assert!(pan.clicked(DragEnd::Released));
        assert!(!pan.clicked(DragEnd::Abandoned), "no release, no click");

        pan.follow(point(px(100.), px(101. + MIN_DRAG_PX)));
        pan.follow(press);
        assert!(!pan.clicked(DragEnd::Released), "moved, then came back");
    }

    #[test]
    fn cancelling_with_no_drag_under_way_reports_nothing_to_abandon() {
        assert!(!TimelineDrag::None.cancel_region());
        assert!(!TimelineDrag::Pan(PanDrag::at(Point::default())).cancel_region());
    }

    /// A viewport split with a gutter of [`TEST_GUTTER`] and no counter lanes,
    /// whose flame lanes are short enough that no vertical-bar column is reserved.
    fn test_axis() -> Regions {
        Regions::new(
            Bounds::new(point(px(0.), px(0.)), size(px(1000.), px(800.))),
            0,
            CountersStrip::Open(DEFAULT_COUNTERS_HEIGHT),
            TEST_GUTTER,
            0.0,
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
    fn ruler_press(sel: &TimeSelection, x: f32, ctrl: bool) -> (RegionEdit, i64) {
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
        region_press(sel, at, ctrl, grab)
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
    fn a_ctrl_click_inside_a_region_takes_that_region_away() {
        let base = three_regions();
        let (edit, at) = ruler_press(&base, ruler_x(8500), true);
        assert!(matches!(edit, RegionEdit::Remove { index: 1 }));
        assert_eq!(
            regions(&click(edit, at).released(&base)),
            [(6000, 7000), (10_000, 11_000)]
        );
    }

    #[test]
    fn ctrl_removes_exactly_the_region_a_plain_press_would_take_hold_of() {
        let base = three_regions();
        let axis = test_axis();
        let mut x = axis.content_left;
        while x <= axis.time_right {
            let plain = ruler_press(&base, x, false).0;
            let ctrl = ruler_press(&base, x, true).0;
            assert_eq!(plain.region(), ctrl.region(), "at x={x}");
            x += 1.0;
        }
    }

    /// Region 1 of [`three_regions`] as the fixture draws it, in window x. Its
    /// edges carry the grips the removal gestures below aim at.
    const REGION_1_EDGES_PX: (f32, f32) = (500.0, 600.0);

    /// Asserts a ctrl press at window `x` takes region 1 away and leaves the
    /// other two, and that a plain press there resizes the same region — the
    /// grip is grabbable at `x`, and ctrl agrees on which region it belongs to.
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
    fn a_ctrl_press_in_a_grips_outer_half_removes_its_region() {
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
    fn a_ctrl_press_on_a_drawn_end_edge_removes_its_region() {
        // A region ends on the instant past its last, so its end edge stands on
        // time the region itself does not cover.
        assert_grip_removes_region_1(REGION_1_EDGES_PX.1);
    }

    #[test]
    fn a_ctrl_press_in_a_grips_inner_half_removes_its_region() {
        let (left, right) = REGION_1_EDGES_PX;
        assert_grip_removes_region_1(left);
        assert_grip_removes_region_1(right - 2.0);
    }

    #[test]
    fn a_ctrl_press_beyond_a_grips_reach_leaves_the_selection_untouched() {
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
    fn a_ctrl_click_outside_every_region_leaves_the_selection_untouched() {
        let base = three_regions();
        let (edit, at) = ruler_press(&base, ruler_x(IN_THE_GAP_US), true);
        assert!(
            matches!(edit, RegionEdit::Union),
            "it found no region to take away"
        );
        assert_eq!(regions(&click(edit, at).released(&base)), regions(&base));
    }

    #[test]
    fn a_shift_ctrl_click_over_the_lanes_holding_no_region_keeps_the_selection() {
        let base = three_regions();
        let (edit, at) = region_press(&base, IN_THE_GAP_US, true, Grab::Lanes);
        assert!(matches!(edit, RegionEdit::Union));
        assert_eq!(regions(&click(edit, at).released(&base)), regions(&base));
    }

    #[test]
    fn a_ctrl_press_outside_every_region_unions_a_new_one() {
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
    fn a_shift_ctrl_press_over_the_lanes_removes_the_region_it_lands_in() {
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
