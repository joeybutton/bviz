use crate::chart::fit_px_per_us;
use crate::counters::paint_crosshair;
use crate::groups::{GroupChrome, GroupHit, RowAnchor, anchor_at, anchor_offset, chart_rows};
use crate::model::TraceModel;
use crate::regions::{Regions, flame_total_height, max_lane_scroll};
use crate::theme::Palette;
use crate::{
    BottomTab, ClearTimeSelection, DEFAULT_GUTTER_WIDTH, DEFAULT_PANEL_HEIGHT, EDGE_MARGIN_PX,
    GROUP_BAND_HEIGHT, GROUP_ICON, LINE_SCROLL_PX, MAX_PX_PER_US, METRICS, MIN_SLICE_PX,
    SLICE_TEXT_PAD, SelectAllTracks, Source, TraceView, ZOOM_PER_NOTCH, drawn_scroll,
    paint_gridlines, paint_gutter, paint_markers, paint_ruler, paint_slice, paint_tooltip,
    ruler_ticks, scroll_request, scrollbar_geom, thumb_offset,
};
use bviz_trace_layout::{
    Cull, LaneLayout, TimeRegion, TimeSelection, TimeView, Viewport, VisibleSlice, span_between,
    visible_slices,
};
use bviz_trace_profile::{Profile, Slice, SliceDetail, TimeSpan};
use bviz_ui_components::{Scrollbar, cached_layer, overlay_canvas, track_drag};
use gpui::{
    App, Bounds, ContentMask, Context, Div, Entity, FocusHandle, IntoElement, KeyBinding,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, Rgba,
    ScrollWheelEvent, Stateful, WeakEntity, Window, canvas, div,
    fill, point, prelude::*, px, size,
};
use std::rc::Rc;
use std::sync::Arc;

/// The timeline's culled draw list together with the inputs that determine it,
/// so a repaint reuses it unless one changed. The list turns only on the profile
/// (`Arc` identity), the lane layout (`Rc` identity, a fresh one on any
/// visibility, collapse, or show-all change), the time view (pan/zoom), and the
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
            Cull { view: &view, viewport: &viewport, metrics: &METRICS, min_px: MIN_SLICE_PX },
        ));
        Self { profile, lanes, view, viewport, slices }
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
        Some(HoveredSlice {
            profile: self.profile.clone(),
            slice: self.profile.slices(track)[vs.slice],
            detail: self.profile.details(track)[vs.slice],
        })
    }
}

/// The flame chart as it currently stands, read against the lane scroll's
/// standing anchor.
struct AnchoredChart {
    lanes: Rc<LaneLayout>,
    chrome: Rc<GroupChrome>,
    /// Where the anchored row sits in the chart, before the ceiling bounds it.
    held: f32,
    ceiling: f32,
}

/// How near a region's edge in the ruler a press must land to take hold of it.
const EDGE_GRAB_PX: f32 = 4.0;

/// How far a press must travel before it is read as a drag rather than a click.
const MIN_DRAG_PX: f32 = 3.0;

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
    Slide { index: usize, width_us: i64, grab_us: i64, bounds: TimeSpan },
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
    /// a slide pushed against an end stops there, keeping its width. A drag that
    /// has not left the instant it is anchored at covers no region.
    pub(crate) fn span(&self) -> Option<TimeRegion> {
        match self.edit {
            RegionEdit::Slide { width_us, grab_us, bounds, .. } => {
                let last_start_us = bounds.end_us - width_us;
                let start_us = (self.cursor_us - grab_us).min(last_start_us).max(bounds.start_us);
                span_between(start_us, start_us + width_us)
            }
            _ => span_between(self.anchor_us, self.cursor_us),
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
    Pan,
    /// A press on the lane vertical or time horizontal scroll thumb/track,
    /// holding the grab offset from the thumb's leading edge.
    VThumb(f32),
    HThumb(f32),
    /// A press on the ruler, or a shift-press on the flame lanes, selecting the
    /// time the flame graph and stats aggregate over.
    TimeRegion(RegionDrag),
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
        self.region().map_or_else(|| standing.clone(), |drag| drag.applied(standing))
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
/// axis, the counter plots (Counters tab), the ruler, gutter, phase markers, and
/// scroll bars. Owns the timeline's view state, reading the document and
/// selection from [`TraceModel`] and the panel/gutter layout from
/// [`TraceView`]. The slices are painted by [`SliceCanvas`], the time selection,
/// crosshair, and tooltip by an overlay above it.
pub(crate) struct TimelinePanel {
    /// Focus for the trace content, so Ctrl-A (select all tracks) dispatches
    /// here; a descendant of the window focus, so the window's own actions fire.
    content_focus: FocusHandle,
    /// Read for the document and lanes, written for selection and grouping.
    model: Entity<TraceModel>,
    /// The owning view, read for the panel and gutter layout the timeline shares
    /// with the bottom panel and the tab bar.
    shell: WeakEntity<TraceView>,
    /// Horizontal time-to-pixel mapping (pan and zoom). Prepaint settles it
    /// against the painted width — the fit, the resize rescale, and the zoom
    /// floor all need a width the panel learns only there — and the same frame's
    /// paint then reads the result.
    view: TimeView,
    /// Which chart row the flame region's top edge sits in, and where within it;
    /// [`Self::lane_scroll`] turns it into the pixel offset the frame draws at.
    scroll: RowAnchor,
    /// Viewport bounds captured during canvas paint, so handlers can map a window
    /// cursor to viewport space and bound the zoom to a fit.
    viewport: Bounds<Pixels>,
    /// Whether the initial fit-to-window view has been set; latched during the
    /// first paint, when the viewport size is first known.
    fitted: bool,
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
}

impl TimelinePanel {
    pub(crate) fn new(
        model: Entity<TraceModel>,
        shell: WeakEntity<TraceView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let content_focus = cx.focus_handle();
        content_focus.focus(window, cx);
        cx.bind_keys([
            KeyBinding::new("ctrl-a", SelectAllTracks, Some("TraceContent")),
            KeyBinding::new("escape", ClearTimeSelection, Some("TraceContent")),
        ]);
        let parent = cx.entity().downgrade();
        let canvas = cx.new(|_| SliceCanvas { parent });
        Self {
            content_focus,
            model,
            shell,
            view: TimeView { start_us: 0.0, px_per_us: 1.0 },
            scroll: RowAnchor::Start,
            viewport: Bounds::default(),
            fitted: false,
            slice_draw: None,
            hover: None,
            crosshair: None,
            drag: TimelineDrag::None,
            last_mouse: Point::default(),
            canvas,
        }
    }

    /// Rebuilds the slice scene along with the overlay; every change to what the
    /// slices show routes through here, and a change that leaves them alone
    /// notifies alone, so the slice scene replays from its cache.
    pub(crate) fn invalidate_canvas(&self, cx: &mut Context<Self>) {
        self.canvas.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    /// Drops the state that addresses the document being replaced, so nothing
    /// survives the swap still pointing into the old profile.
    pub(crate) fn reset_for_new_profile(&mut self) {
        self.slice_draw = None;
        self.scroll = RowAnchor::Start;
        self.drag = TimelineDrag::None;
    }

    /// The bottom-tab selection, panel height, and gutter width the timeline
    /// shares with the bottom panel, read from the owning view.
    fn shell_layout(&self, cx: &App) -> (BottomTab, f32, f32) {
        match self.shell.upgrade() {
            Some(shell) => {
                let s = shell.read(cx);
                (s.bottom_tab, s.panel_height, s.gutter_width)
            }
            None => (BottomTab::Counters, DEFAULT_PANEL_HEIGHT, DEFAULT_GUTTER_WIDTH),
        }
    }

    /// The viewport bounds last captured during paint.
    pub(crate) fn viewport(&self) -> Bounds<Pixels> {
        self.viewport
    }

    /// [`Regions`] for the current viewport, panel and gutter sizes, the loaded
    /// profile's counter count, and the flame lanes' total height.
    fn regions(&self, cx: &App) -> Regions {
        let (n_counters, flame_total) = {
            let model = self.model.read(cx);
            let n_counters = match model.source() {
                Source::Loaded(p) => p.counters().len(),
                _ => 0,
            };
            let flame_total = model
                .lane_layout()
                .map(|l| flame_total_height(&l))
                .unwrap_or(0.0);
            (n_counters, flame_total)
        };
        let (_, panel_height, gutter_width) = self.shell_layout(cx);
        Regions::new(self.viewport, n_counters, panel_height, gutter_width, flame_total)
    }

    /// The lane and time bars for the panel's view state; both `None` while no
    /// profile with a lane layout is loaded.
    fn scroll_bars(&self, cx: &App) -> (Option<Scrollbar>, Option<Scrollbar>) {
        let (span, flame_total) = {
            let model = self.model.read(cx);
            let (Source::Loaded(profile), Some(lanes)) = (model.source(), model.lane_layout())
            else {
                return (None, None);
            };
            (profile.span(), flame_total_height(&lanes))
        };
        scrollbar_geom(span, self.view, flame_total, self.regions(cx), self.lane_scroll(cx))
    }

    fn vbar(&self, cx: &App) -> Option<Scrollbar> {
        self.scroll_bars(cx).0
    }

    fn hbar(&self, cx: &App) -> Option<Scrollbar> {
        self.scroll_bars(cx).1
    }

    /// The chart the lane scroll addresses; `None` while no profile with a lane
    /// layout is loaded. Reads the shell for its layout, so it must not run while
    /// the shell is leased.
    fn anchored_chart(&self, cx: &App) -> Option<AnchoredChart> {
        let model = self.model.read(cx);
        let (lanes, chrome) = (model.lane_layout()?, model.group_chrome()?);
        let held = anchor_offset(chart_rows(&lanes, &chrome), self.scroll);
        let ceiling = max_lane_scroll(&lanes, &self.regions(cx));
        Some(AnchoredChart { lanes, chrome, held, ceiling })
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
        self.scroll = anchor_at(chart_rows(&chart.lanes, &chart.chrome), target);
    }

    /// Scrolls the lanes down by a gesture's `dy` pixels.
    fn scroll_by(&mut self, dy: f32, cx: &App) {
        self.scroll_to(self.lane_scroll(cx) - dy, cx);
    }

    /// Clears the slice tooltip and crosshair, so a tab switch drops them
    /// instead of leaving them until the next mouse move.
    pub(crate) fn clear_hover(&mut self) {
        self.hover = None;
        self.crosshair = None;
    }

    /// Whether `pos` lies over a time-axis region the crosshair shows on: the
    /// flame lanes always, and the counter panel when it is the shown tab.
    fn in_content(&self, pos: Point<Pixels>, cx: &App) -> bool {
        if self.model.read(cx).lane_layout().is_none() {
            return false;
        }
        let regions = self.regions(cx);
        let (tab, _, _) = self.shell_layout(cx);
        let x = f32::from(pos.x);
        let y = f32::from(pos.y);
        let in_counters = tab == BottomTab::Counters
            && regions.has_graph
            && x >= regions.content_left
            && x < regions.time_right
            && y >= regions.panel_top
            && y < regions.panel_bottom;
        regions.in_flame(pos) || in_counters
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
        let (_, _, gutter_width) = self.shell_layout(cx);
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
        let (_, _, gutter_width) = self.shell_layout(cx);
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

    /// Selects every currently visible track (the Ctrl-A gesture).
    fn select_all_visible(&mut self, _: &SelectAllTracks, _: &mut Window, cx: &mut Context<Self>) {
        if self.model.update(cx, |m, cx| m.select_all_visible(cx)) {
            self.invalidate_canvas(cx);
        }
    }

    /// Backs out of the innermost gesture (the Escape gesture): a region drag
    /// under way is abandoned, and failing that the aggregate widens back to the
    /// whole timeline.
    fn clear_time_selection(
        &mut self,
        _: &ClearTimeSelection,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let backed_out = self.drag.cancel_region()
            || self.model.update(cx, |m, cx| m.set_time_selection(TimeSelection::default(), cx));
        if backed_out {
            cx.notify();
        }
    }

    /// The instant a window-space cursor points at; `None` while no profile is
    /// loaded.
    fn time_at(&self, pos: Point<Pixels>, cx: &App) -> Option<i64> {
        let Source::Loaded(profile) = self.model.read(cx).source() else {
            return None;
        };
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
        if !in_ruler && !(e.modifiers.shift && regions.in_flame(e.position)) {
            return false;
        }
        let Source::Loaded(profile) = self.model.read(cx).source() else {
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

    /// Lands a finished time-region drag on the model.
    fn commit_time_region(&mut self, drag: RegionDrag, cx: &mut Context<Self>) {
        let selection = drag.released(self.model.read(cx).time_selection());
        self.model.update(cx, |m, cx| m.set_time_selection(selection, cx));
    }

    /// Begins a timeline drag at `pos`: the lane vertical or time horizontal
    /// scroll thumb/track if the press hit one, otherwise a pan.
    fn press(&mut self, pos: Point<Pixels>, cx: &App) {
        self.crosshair = None;
        let (vbar, hbar) = self.scroll_bars(cx);
        if let Some(bar) = vbar {
            if bar.track_hit(pos) {
                let grab = bar.grab(pos);
                self.drag = TimelineDrag::VThumb(grab);
                self.scroll_thumb_to(pos, grab, cx);
                return;
            }
        }
        if let Some(bar) = hbar {
            if bar.track_hit(pos) {
                let grab = bar.grab(pos);
                self.drag = TimelineDrag::HThumb(grab);
                self.pan_thumb_to(pos, grab, cx);
                return;
            }
        }
        self.drag = TimelineDrag::Pan;
        self.last_mouse = pos;
    }

    /// Maps the lane vertical thumb drag back to a scroll offset.
    fn scroll_thumb_to(&mut self, pos: Point<Pixels>, grab: f32, cx: &App) {
        if let Some(offset) = thumb_offset(self.vbar(cx), pos, grab) {
            self.scroll_to(offset, cx);
        }
    }

    /// Maps a horizontal thumb drag back to a pan start time, then clamps the view.
    fn pan_thumb_to(&mut self, pos: Point<Pixels>, grab: f32, cx: &App) {
        if self.model.read(cx).lane_layout().is_none() {
            return;
        }
        let profile = match self.model.read(cx).source() {
            Source::Loaded(p) => p.clone(),
            _ => return,
        };
        let span = profile.span();
        let regions = self.regions(cx);
        let Some(offset) = thumb_offset(self.hbar(cx), pos, grab) else {
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
        let profile = match self.model.read(cx).source() {
            Source::Loaded(p) => p.clone(),
            _ => return,
        };
        let span = profile.span();
        let regions = self.regions(cx);
        let time_w = regions.time_w;
        let content_left = regions.content_left;
        let delta = e.delta.pixel_delta(px(LINE_SCROLL_PX));
        let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        if e.modifiers.control || e.modifiers.platform {
            let rel_x = f32::from(e.position.x) - content_left;
            let dy_lines = dy / LINE_SCROLL_PX;
            self.view.zoom_at(rel_x, ZOOM_PER_NOTCH.powf(dy_lines as f64));
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
            self.scroll_by(scroll_dy, cx);
            self.view.pan_px(pan_dx);
            self.view.clamp_to(span, time_w, EDGE_MARGIN_PX);
        }
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
    /// selects time regions, anywhere else pans; the wheel zooms/pans/scrolls,
    /// and a move updates the crosshair and hover. Holds the content focus and
    /// the chart's actions, so a press here makes Ctrl-A select all tracks and
    /// Escape widen the aggregate to the whole timeline.
    fn content_handlers(&self, root: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        root.key_context("TraceContent")
            .track_focus(&self.content_focus)
            .on_action(cx.listener(Self::select_all_visible))
            .on_action(cx.listener(Self::clear_time_selection))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, window, cx| {
                    // Take content focus so Ctrl-A selects tracks after a chart
                    // interaction (a click may have moved focus to the filter).
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
                        this.model
                            .update(cx, |m, cx| m.select_track(track, e.modifiers.shift, ctrl, cx));
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

impl Render for TimelinePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (profile, lanes) = {
            let model = self.model.read(cx);
            let Source::Loaded(profile) = model.source() else {
                return div().into_any_element();
            };
            let Some(lanes) = model.lane_layout() else {
                return div().into_any_element();
            };
            (profile.clone(), lanes)
        };
        let palette = Palette::from_colors(&bviz_ui_theme::colors(window));
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
    panel: &mut TimelinePanel,
    event: &MouseMoveEvent,
    cx: &mut Context<TimelinePanel>,
) {
    let moves_slices = !matches!(panel.drag, TimelineDrag::TimeRegion(_));
    match panel.drag {
        TimelineDrag::Pan => {
            let dx = f32::from(event.position.x) - f32::from(panel.last_mouse.x);
            let dy = f32::from(event.position.y) - f32::from(panel.last_mouse.y);
            if panel.model.read(cx).lane_layout().is_none() {
                return;
            }
            let profile = match panel.model.read(cx).source() {
                Source::Loaded(p) => p.clone(),
                _ => return,
            };
            let span = profile.span();
            let regions = panel.regions(cx);
            panel.view.pan_px(dx);
            panel.view.clamp_to(span, regions.time_w, EDGE_MARGIN_PX);
            panel.scroll_by(dy, cx);
            panel.last_mouse = event.position;
        }
        TimelineDrag::VThumb(grab) => panel.scroll_thumb_to(event.position, grab, cx),
        TimelineDrag::HThumb(grab) => panel.pan_thumb_to(event.position, grab, cx),
        TimelineDrag::TimeRegion(mut drag) => {
            if let Some(at) = panel.time_at(event.position, cx) {
                drag.cursor_us = at;
                drag.moved |= (f32::from(event.position.x) - drag.press_x).abs() > MIN_DRAG_PX;
                panel.drag = TimelineDrag::TimeRegion(drag);
            }
        }
        TimelineDrag::None => {}
    }
    panel.update_hover(event.position, cx);
    if moves_slices {
        panel.invalidate_canvas(cx);
    } else {
        cx.notify();
    }
}

fn timeline_end_drag(
    panel: &mut TimelinePanel,
    _: &MouseUpEvent,
    cx: &mut Context<TimelinePanel>,
) {
    match std::mem::replace(&mut panel.drag, TimelineDrag::None).region() {
        Some(drag) => {
            panel.commit_time_region(drag, cx);
            cx.notify();
        }
        None => panel.invalidate_canvas(cx),
    }
}

/// The layer painting the timeline's slices and chrome, from the view state,
/// document, and lanes it reads through `parent`.
struct SliceCanvas {
    parent: WeakEntity<TimelinePanel>,
}

impl Render for SliceCanvas {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(parent) = self.parent.upgrade() else {
            return div().into_any_element();
        };
        let (profile, lanes, chrome, panel_height, gutter_width) = {
            let panel = parent.read(cx);
            let model = panel.model.read(cx);
            let Source::Loaded(profile) = model.source() else {
                return div().into_any_element();
            };
            let (Some(lanes), Some(chrome)) = (model.lane_layout(), model.group_chrome()) else {
                return div().into_any_element();
            };
            let (_, panel_height, gutter_width) = panel.shell_layout(cx);
            (profile.clone(), lanes, chrome, panel_height, gutter_width)
        };
        let palette = Palette::from_colors(&bviz_ui_theme::colors(window));
        let prepaint = fit_prepaint(profile.clone(), lanes.clone(), parent.clone(), panel_height, gutter_width);
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
    parent: Entity<TimelinePanel>,
    panel_height: f32,
    gutter_width: f32,
) -> impl FnOnce(Bounds<Pixels>, &mut Window, &mut App) {
    move |bounds: Bounds<Pixels>, _: &mut Window, cx: &mut App| {
        let old_bounds = parent.read(cx).viewport;
        let flame_total = flame_total_height(&lanes);
        let regions_of = |b| {
            Regions::new(b, profile.counters().len(), panel_height, gutter_width, flame_total)
        };
        let regions = regions_of(bounds);
        let time_w = regions.time_w;
        let old_time_w = regions_of(old_bounds).time_w;
        let span = profile.span();
        parent.update(cx, |panel, _| {
            panel.viewport = bounds;
            if !panel.fitted {
                panel.view = TimeView::fit(span, time_w, EDGE_MARGIN_PX);
                panel.fitted = true;
            } else if old_bounds.size.width > px(0.)
                && (old_time_w - time_w).abs() > f32::EPSILON
            {
                panel.view.rescale_to_width(span, old_time_w, time_w, EDGE_MARGIN_PX);
            }
            panel.view.px_per_us = panel
                .view
                .px_per_us
                .max(fit_px_per_us(span, time_w, EDGE_MARGIN_PX));
            panel.view.clamp_to(span, time_w, EDGE_MARGIN_PX);
        });
    }
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
    if let Some(c) = cache.as_ref() {
        if Arc::ptr_eq(&c.profile, profile)
            && Rc::ptr_eq(&c.lanes, lanes)
            && c.view == view
            && c.viewport == viewport
        {
            return c.slices.clone();
        }
    }
    cache
        .insert(SliceDrawCache::new(profile.clone(), lanes.clone(), view, viewport))
        .slices
        .clone()
}

/// The slice layer's paint closure: the flame timeline — gridlines, lanes and
/// slices, phase markers, gutter, scroll bars, ruler, and (for the Counters tab)
/// the counter plots — reading the live view state through `parent`.
fn slice_paint(
    profile: Arc<Profile>,
    lanes: Rc<LaneLayout>,
    chrome: Rc<GroupChrome>,
    parent: Entity<TimelinePanel>,
    palette: Palette,
) -> impl FnOnce(Bounds<Pixels>, (), &mut Window, &mut App) {
    move |bounds, _, window, cx| {
        let layout = &*lanes;
        let n_counters = profile.counters().len();
        let flame_total = flame_total_height(layout);
        let (tv, scroll, gutter_width, selected, regions, show_counters, vp) = {
            let panel = parent.read(cx);
            let tv = panel.view;
            let scroll = panel.lane_scroll(cx);
            let (tab, panel_height, gutter_width) = panel.shell_layout(cx);
            let selected = panel.model.read(cx).selected_tracks().to_vec();
            let regions =
                Regions::new(bounds, n_counters, panel_height, gutter_width, flame_total);
            let vp = Viewport {
                width: regions.time_w,
                height: regions.flame_h(),
                scroll_y: scroll,
            };
            let show_counters = regions.has_graph && tab == BottomTab::Counters;
            (tv, scroll, gutter_width, selected, regions, show_counters, vp)
        };
        let slices = parent.update(cx, |panel, _| {
            cached_slices(&mut panel.slice_draw, &profile, &lanes, tv, vp)
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
                regions.panel_top,
                regions.panel_bottom,
                &palette,
                window,
            );
        }

        let flame_mask = ContentMask { bounds: regions.flame_rect() };
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
            for band in &chrome.bands {
                let y = regions.flame_top + band.top - scroll;
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
            crate::counters::paint_graph(&profile, tv, regions, &palette, window);
        }
        paint_markers(&profile, tv, regions, show_counters, &palette, window, cx);
        let (vbar, hbar) = scrollbar_geom(profile.span(), tv, flame_total, regions, scroll);
        for bar in vbar.into_iter().chain(hbar) {
            bar.paint(palette.scroll_track, palette.scroll_thumb, window);
        }

        paint_gutter(
            &profile,
            layout,
            &chrome,
            regions,
            gutter_width,
            scroll,
            show_counters,
            &selected,
            &palette,
            window,
            cx,
        );

        paint_ruler(&ticks, regions, bounds, &palette, window, cx);
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
    out.extend(sel.spans().iter().filter_map(|s| RegionPixels::of(*s, tv, regions)));
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
        window.paint_quad(fill(Bounds::new(point(px(x), px(top)), size(px(w), px(h))), color));
    };

    let veil = Rgba { a: VEIL_ALPHA, ..palette.background };
    let veil_span = |from: f32, to: f32, window: &mut Window| {
        quad(from, to - from, regions.flame_top, regions.flame_h(), veil, window);
        if show_counters {
            quad(from, to - from, regions.panel_top, regions.panel_h(), veil, window);
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
    let tint = Rgba { a: RULER_BAND_ALPHA, ..palette.selection };
    for r in &painted {
        quad(r.left, r.right - r.left, band_top, band_h, tint, window);
    }

    let accent = palette.selection;
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
            quad(line_x, EDGE_LINE_PX, band_top, regions.flame_bottom - band_top, accent, window);
            if show_counters {
                quad(line_x, EDGE_LINE_PX, regions.panel_top, regions.panel_h(), accent, window);
            }
            quad(edge_x - EDGE_GRAB_PX, GRIP_WIDTH, grip_top, grip_h, accent, window);
        }
    }
}

/// The overlay's paint closure: the time selection, the crosshair, the hover
/// tooltip, and — while a drag runs — the window-global drag handlers, from the
/// selection, crosshair, hover, and drag state it reads through `parent`.
fn overlay_paint(
    profile: Arc<Profile>,
    lanes: Rc<LaneLayout>,
    parent: Entity<TimelinePanel>,
    palette: Palette,
) -> impl FnOnce(Bounds<Pixels>, &mut Window, &mut App) {
    move |bounds, window, cx| {
        let n_counters = profile.counters().len();
        let (tv, tab, panel_height, gutter_width, selection, crosshair, hovered, dragging) = {
            let panel = parent.read(cx);
            let tv = panel.view;
            let (tab, panel_height, gutter_width) = panel.shell_layout(cx);
            let hovered = panel.hover.and_then(|at| Some((at, panel.slice_at(at, cx)?)));
            let selection = panel.drag.preview(panel.model.read(cx).time_selection());
            (
                tv,
                tab,
                panel_height,
                gutter_width,
                selection,
                panel.crosshair,
                hovered,
                !matches!(panel.drag, TimelineDrag::None),
            )
        };
        let flame_total = flame_total_height(&lanes);
        let regions = Regions::new(bounds, n_counters, panel_height, gutter_width, flame_total);
        let show_counters = regions.has_graph && tab == BottomTab::Counters;
        paint_time_selection(&selection, tv, regions, show_counters, &palette, window);
        if let Some(at) = crosshair {
            if f32::from(at.x) >= regions.content_left {
                paint_crosshair(&profile, tv, regions, at, show_counters, &palette, window, cx);
            }
        }
        if let Some((at, drawn)) = hovered {
            paint_tooltip(&drawn, at, bounds, &palette, window, cx);
        }
        if dragging {
            track_drag(window, &parent, timeline_on_drag, timeline_end_drag);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_trace_layout::lay_out_lanes;

    const TEST_TIDS: [i64; 2] = [1, 2];
    const TEST_VIEW: TimeView = TimeView { start_us: 0.0, px_per_us: 0.1 };
    const TEST_VIEWPORT: Viewport = Viewport { width: 1000.0, height: 500.0, scroll_y: 0.0 };

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
        profile_of_events(thread_events(TEST_TIDS[0], [(0, sub_pixel_us), (sub_pixel_us, 1000)]))
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
            assert_eq!(hit.profile().str(hit.detail().category), profile.str(detail.category));
        }

        let below_every_box = cache.slices.iter().map(|vs| vs.y + vs.h).fold(0.0, f32::max);
        let first = cache.slices.first().expect("a culled slice");
        assert!(cache.hit(first.x + first.w / 2.0, below_every_box).is_none());
    }

    #[test]
    fn a_hit_where_boxes_overlap_names_the_one_drawn_last() {
        let profile = profile_of_overlapping_boxes();
        let cache = draw_cache(&profile);
        let [under, over] = cache.slices[..] else {
            panic!("expected the fixture's two boxes, got {}", cache.slices.len());
        };
        let overlap_end = under.x + under.w;
        assert_eq!(under.y, over.y, "the fixture's boxes must share a row");
        assert!(over.x < overlap_end, "the fixture's boxes must cover a common point");

        let hit = cache
            .hit((over.x + overlap_end) / 2.0, over.y + over.h / 2.0)
            .expect("the box drawn over the point");
        let track = &profile.threads()[over.track];
        let drawn = profile.slices(track)[over.slice];
        assert_eq!(hit.profile().str(hit.slice().name), profile.str(drawn.name));
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
        RegionDrag { edit, anchor_us, cursor_us, press_x: 0.0, moved: true }
    }

    /// The profile a slide is dragged over.
    const TEST_SPAN: TimeSpan = TimeSpan { start_us: 0, end_us: 40_000 };

    /// A slide bounded to [`TEST_SPAN`].
    fn slide(index: usize, width_us: i64, grab_us: i64) -> RegionEdit {
        RegionEdit::Slide { index, width_us, grab_us, bounds: TEST_SPAN }
    }

    #[test]
    fn a_replacing_drag_spans_its_press_and_its_cursor() {
        assert_eq!(drag(RegionEdit::Replace, 100, 400).span(), span_between(100, 400));
        // Dragged leftward, the ends arrive the other way round.
        assert_eq!(drag(RegionEdit::Union, 400, 100).span(), span_between(100, 400));
    }

    #[test]
    fn a_resize_dragged_past_its_fixed_edge_flips_the_region() {
        let edit = RegionEdit::Resize { index: 0 };
        assert_eq!(drag(edit, 400, 500).span(), span_between(400, 500));
        assert_eq!(drag(edit, 400, 250).span(), span_between(250, 400));
    }

    #[test]
    fn a_slide_carries_the_region_width_to_the_cursor() {
        assert_eq!(drag(slide(0, 100, 30), 0, 500).span(), span_between(470, 570));
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
        let held = RegionDrag { moved: false, ..drag(RegionEdit::Replace, 500, 501) };
        assert_eq!(held.applied(&base), base);
    }

    #[test]
    fn a_resize_released_under_the_click_threshold_keeps_every_region() {
        let base = selection(&[(0, 100), (200, 300), (400, 500)]);
        let held = RegionDrag { moved: false, ..drag(RegionEdit::Resize { index: 1 }, 300, 202) };
        assert_eq!(regions(&held.released(&base)), regions(&base));
    }

    #[test]
    fn a_slide_released_under_the_click_threshold_keeps_every_region() {
        let base = selection(&[(0, 100), (200, 300), (400, 500)]);
        let held = RegionDrag { moved: false, ..drag(slide(1, 100, 50), 250, 252) };
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
        assert_eq!(regions(&gesture.preview(&base)), [(500, 700)], "the drag previews a region");
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
        assert!(gesture.region().is_none(), "the release finds no drag to commit");
    }

    #[test]
    fn cancelling_with_no_drag_under_way_reports_nothing_to_abandon() {
        assert!(!TimelineDrag::None.cancel_region());
        assert!(!TimelineDrag::Pan.cancel_region());
    }

    /// A viewport split with a gutter of [`TEST_GUTTER`] and no counter lanes,
    /// whose flame lanes are short enough that no vertical-bar column is reserved.
    fn test_axis() -> Regions {
        Regions::new(
            Bounds::new(point(px(0.), px(0.)), size(px(1000.), px(800.))),
            0,
            DEFAULT_PANEL_HEIGHT,
            TEST_GUTTER,
            0.0,
        )
    }

    /// A view whose window opens well after the profile start, so a region can
    /// reach off either end of the axis.
    const TEST_SELECTION_VIEW: TimeView = TimeView { start_us: 5000.0, px_per_us: 0.1 };
    const TEST_GUTTER: f32 = 200.0;

    #[test]
    fn a_region_is_clipped_to_the_axis_and_keeps_only_the_edges_on_it() {
        let axis = test_axis();
        assert_eq!((axis.content_left, axis.time_right), (200.0, 1000.0), "the fixture's axis");
        let sel = selection(&[(4000, 6000), (6500, 8000), (12000, 20000)]);
        assert_eq!(
            selection_pixels(&sel, TEST_SELECTION_VIEW, axis),
            [
                RegionPixels { left: 200.0, right: 300.0, starts: false, ends: true },
                RegionPixels { left: 350.0, right: 500.0, starts: true, ends: true },
                RegionPixels { left: 900.0, right: 1000.0, starts: true, ends: false },
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
            [RegionPixels { left: 700.0, right: 1000.0, starts: true, ends: true }]
        );
    }

    #[test]
    fn the_ruler_band_covers_the_time_axis_down_to_the_flame_lanes() {
        let axis = test_axis();
        let band = axis.ruler_rect();
        assert_eq!(f32::from(band.origin.x), axis.content_left);
        assert_eq!(f32::from(band.size.width), axis.time_w);
        assert_eq!(f32::from(band.origin.y) + f32::from(band.size.height), axis.flame_top);
    }

    #[test]
    fn a_press_is_in_the_ruler_band_exactly_where_the_paint_fills_it() {
        let axis = test_axis();
        let band = axis.ruler_rect();
        let mid_y = px(f32::from(band.origin.y) + f32::from(band.size.height) / 2.0);
        assert!(axis.in_ruler(point(px(axis.content_left), mid_y)));
        assert!(!axis.in_ruler(point(px(axis.content_left - 1.0), mid_y)), "over the gutter");
        assert!(!axis.in_ruler(point(px(axis.time_right), mid_y)), "past the time axis");
        assert!(!axis.in_ruler(point(px(axis.content_left), px(axis.flame_top))), "in the lanes");
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
        let grabbed = region_grab(&sel, TEST_SELECTION_VIEW, axis, TEST_SPAN, press_at(x, axis), x);
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
        let grabbed = region_grab(&sel, TEST_SELECTION_VIEW, axis, TEST_SPAN, press_at(x, axis), x);
        assert_eq!(grabbed.map(|(_, anchor)| anchor), Some(8000));
        assert!(matches!(grabbed, Some((RegionEdit::Resize { index: 0 }, _))));
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
        let grab = Grab::Ruler(region_grab(sel, TEST_SELECTION_VIEW, axis, TEST_SPAN, at, x));
        region_press(sel, at, ctrl, grab)
    }

    /// A press released without ever passing the click threshold.
    fn click(edit: RegionEdit, anchor_us: i64) -> RegionDrag {
        RegionDrag { moved: false, ..drag(edit, anchor_us, anchor_us) }
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
        assert_eq!(regions(&click(edit, at).released(&base)), [(6000, 7000), (10_000, 11_000)]);
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
        assert!(matches!(ruler_press(&base, x, false).0, RegionEdit::Resize { index: 1 }), "{x}");
        let (edit, at) = ruler_press(&base, x, true);
        assert!(matches!(edit, RegionEdit::Remove { index: 1 }), "at x={x}");
        assert_eq!(regions(&click(edit, at).released(&base)), [(6000, 7000), (10_000, 11_000)]);
    }

    #[test]
    fn a_ctrl_press_in_a_grips_outer_half_removes_its_region() {
        assert_eq!((ruler_x(8000), ruler_x(9000)), REGION_1_EDGES_PX, "the fixture's drawn edges");
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
        assert_eq!(EDGE_GRAB_PX, 4.0, "the reach the probes below are placed just outside");
        for x in [left - 5.0, right + 5.0] {
            let (edit, at) = ruler_press(&base, x, true);
            assert!(matches!(edit, RegionEdit::Union), "at x={x}");
            assert_eq!(regions(&click(edit, at).released(&base)), regions(&base), "at x={x}");
        }
    }

    #[test]
    fn a_removal_that_passes_the_click_threshold_still_removes_its_region() {
        let base = three_regions();
        let dragged = drag(RegionEdit::Remove { index: 1 }, 8500, 12_000);
        assert_eq!(regions(&dragged.released(&base)), [(6000, 7000), (10_000, 11_000)]);
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
        assert!(matches!(edit, RegionEdit::Union), "it found no region to take away");
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
        assert_eq!(regions(&sel), [(6000, 7000), (7500, 7800), (8000, 9000), (10_000, 11_000)]);
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
        assert_eq!(regions(&click(edit, at).released(&base)), [(6000, 7000), (10_000, 11_000)]);
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
            time_at_x(TEST_VIEW, span, TEST_VIEW.x_of(span.end_us) + EDGE_MARGIN_PX),
            span.end_us
        );
        let inside = (span.start_us + span.end_us) / 2;
        assert_eq!(time_at_x(TEST_VIEW, span, TEST_VIEW.x_of(inside)), inside);
    }
}
