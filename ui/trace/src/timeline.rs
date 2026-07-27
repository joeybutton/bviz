use crate::chart::fit_px_per_us;
use crate::counters::paint_crosshair;
use crate::groups::GroupHit;
use crate::model::TraceModel;
use crate::regions::{Regions, flame_total_height, max_lane_scroll};
use crate::theme::Palette;
use crate::{
    BottomTab, DEFAULT_GUTTER_WIDTH, DEFAULT_PANEL_HEIGHT, EDGE_MARGIN_PX, GROUP_BAND_HEIGHT,
    GROUP_ICON, Hover, LINE_SCROLL_PX, MAX_PX_PER_US, METRICS, MIN_SLICE_PX, SLICE_TEXT_PAD,
    SelectAllTracks, Source, TraceView, ZOOM_PER_NOTCH, paint_gridlines, paint_gutter,
    paint_markers, paint_ruler, paint_scrollbars, paint_slice, paint_tooltip, ruler_ticks,
    thumb_offset,
};
use bviz_trace_layout::{Cull, LaneLayout, TimeView, Viewport, VisibleSlice, visible_slices};
use bviz_trace_profile::Profile;
use bviz_ui_components::{Scrollbar, track_drag};
use gpui::{
    AnyView, App, Bounds, ContentMask, Context, Div, Entity, FocusHandle, IntoElement, KeyBinding,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ScrollDelta,
    ScrollWheelEvent, Stateful, StyleRefinement, WeakEntity, Window, canvas, div,
    fill, point, prelude::*, px, size,
};
use std::cell::RefCell;
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
}

/// The flame-chart timeline: time on the x axis, stacked thread lanes on the y
/// axis, the counter plots (Counters tab), the ruler, gutter, phase markers, and
/// scroll bars. Owns the timeline's view state, reading the document and
/// selection from [`TraceModel`] and the panel/gutter layout from
/// [`TraceView`]. The expensive slice paint lives in a separate cached child
/// ([`SliceCanvas`]) so a hover — which touches only the crosshair/tooltip
/// overlay — replays its last scene rather than re-emitting every slice.
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
    /// How far the lanes are scrolled up, in pixels.
    scroll_y: f32,
    /// Viewport bounds captured during canvas paint, so handlers can map a window
    /// cursor to viewport space and bound the zoom to a fit.
    viewport: Bounds<Pixels>,
    /// Whether the initial fit-to-window view has been set; latched during the
    /// first paint, when the viewport size is first known.
    fitted: bool,
    /// The lane vertical bar and time horizontal bar geometry, set during paint
    /// and read by mouse handlers; `None` when that axis fits.
    vbar: Option<Scrollbar>,
    hbar: Option<Scrollbar>,
    /// The timeline's memoized culled draw list; see [`SliceDrawCache`].
    slice_draw: RefCell<Option<SliceDrawCache>>,
    /// The slice under the cursor, if any, drawn as a tooltip.
    hover: Option<Hover>,
    /// The cursor position while it hovers the time-content column and no drag is
    /// in progress, drawn as a crosshair; `None` otherwise.
    crosshair: Option<Point<Pixels>>,
    drag: TimelineDrag,
    last_mouse: Point<Pixels>,
    /// The cached child that paints the slices, gutter, ruler, markers, scroll
    /// bars, and counter plots. A parent `cx.notify()` (the hover path) leaves it
    /// to replay; [`Self::invalidate_canvas`] notifies it to rebuild.
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
        cx.bind_keys([KeyBinding::new("ctrl-a", SelectAllTracks, Some("TraceContent"))]);
        let parent = cx.entity().downgrade();
        let canvas = cx.new(|_| SliceCanvas { parent });
        Self {
            content_focus,
            model,
            shell,
            view: TimeView { start_us: 0.0, px_per_us: 1.0 },
            scroll_y: 0.0,
            viewport: Bounds::default(),
            fitted: false,
            vbar: None,
            hbar: None,
            slice_draw: RefCell::new(None),
            hover: None,
            crosshair: None,
            drag: TimelineDrag::None,
            last_mouse: Point::default(),
            canvas,
        }
    }

    /// Rebuilds the cached slice scene along with the overlay, so a change to what
    /// the canvas paints re-emits its scene rather than replaying it. A plain
    /// `cx.notify()` (the hover path) leaves the canvas to replay.
    pub(crate) fn invalidate_canvas(&self, cx: &mut Context<Self>) {
        self.canvas.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    /// Drops the memoized draw list so it re-culls when the loaded document
    /// changes.
    pub(crate) fn reset_draw_cache(&self) {
        self.slice_draw.borrow_mut().take();
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

    /// The pixel ceiling for the lane scroll: the thread-lane height past the
    /// flame region's on-screen height.
    fn max_scroll(&self, cx: &App) -> f32 {
        match self.model.read(cx).lane_layout() {
            Some(lanes) => max_lane_scroll(&lanes, &self.regions(cx)),
            None => 0.0,
        }
    }

    /// Clamps the lane scroll to the current layout height, dropping any
    /// overscroll a visibility or collapse change left past the new bottom.
    /// Reads the shell for its layout, so it must not run while the shell is
    /// leased; a leased-shell caller uses [`TraceView::clamp_timeline_scroll`],
    /// which computes the ceiling from the shell's own reads and pushes it down
    /// through [`Self::clamp_scroll_to`].
    pub(crate) fn clamp_lane_scroll(&mut self, cx: &App) {
        self.scroll_y = self.scroll_y.min(self.max_scroll(cx));
    }

    /// Drops any lane overscroll past `max`, the scroll ceiling computed by the
    /// caller (which holds the shell's layout without leasing this panel).
    pub(crate) fn clamp_scroll_to(&mut self, max: f32) {
        self.scroll_y = self.scroll_y.min(max);
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
        if x < regions.content_left || x >= regions.time_right {
            return false;
        }
        let in_flame = y >= regions.flame_top && y < regions.flame_bottom;
        let in_counters = tab == BottomTab::Counters
            && regions.has_graph
            && y >= regions.panel_top
            && y < regions.panel_bottom;
        in_flame || in_counters
    }

    /// The slice under a window-space cursor, inverting the paint transform.
    fn slice_at(&self, pos: Point<Pixels>, cx: &App) -> Option<(usize, usize)> {
        let layout = self.model.read(cx).lane_layout()?;
        let profile = match self.model.read(cx).source() {
            Source::Loaded(p) => p.clone(),
            _ => return None,
        };
        let regions = self.regions(cx);
        let x = f32::from(pos.x);
        let y = f32::from(pos.y);
        if x < regions.content_left
            || x >= regions.time_right
            || y < regions.flame_top
            || y >= regions.flame_bottom
        {
            return None;
        }
        let content_y = y - regions.flame_top + self.scroll_y;
        let lane = layout
            .lanes
            .iter()
            .find(|l| content_y >= l.top && content_y < l.top + l.height)?;
        let rows_top = lane.top + METRICS.header_height;
        if content_y < rows_top {
            return None;
        }
        let depth = ((content_y - rows_top) / METRICS.row_height) as u32;
        let view = self.view;
        let cursor_px = x - regions.content_left;
        let slices = profile.slices(&profile.threads()[lane.track]);
        let upper = slices.partition_point(|s| view.x_of(s.start_us) <= cursor_px);
        slices[..upper]
            .iter()
            .position(|s| {
                if s.depth != depth {
                    return false;
                }
                let sx = view.x_of(s.start_us);
                let sw = (view.x_of(s.start_us + s.dur_us) - sx).max(MIN_SLICE_PX);
                cursor_px < sx + sw
            })
            .map(|slice| (lane.track, slice))
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
        let content_y = y - regions.flame_top + self.scroll_y;
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
        let content_y = y - regions.flame_top + self.scroll_y;
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

    /// Begins a timeline drag at `pos`: the lane vertical or time horizontal
    /// scroll thumb/track if the press hit one, otherwise a pan.
    fn press(&mut self, pos: Point<Pixels>, cx: &App) {
        if let Some(bar) = self.vbar {
            if bar.track_hit(pos) {
                let grab = bar.grab(pos);
                self.drag = TimelineDrag::VThumb(grab);
                self.scroll_thumb_to(pos, grab);
                return;
            }
        }
        if let Some(bar) = self.hbar {
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
    fn scroll_thumb_to(&mut self, pos: Point<Pixels>, grab: f32) {
        if let Some(offset) = thumb_offset(self.vbar, pos, grab) {
            self.scroll_y = offset;
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
        let Some(offset) = thumb_offset(self.hbar, pos, grab) else {
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
        let (dx, dy) = match e.delta {
            ScrollDelta::Lines(p) => (p.x * LINE_SCROLL_PX, p.y * LINE_SCROLL_PX),
            ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
        };
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
            let scrolled = (self.scroll_y - scroll_dy).clamp(0.0, self.max_scroll(cx));
            self.scroll_y = scrolled;
            self.view.pan_px(pan_dx);
            self.view.clamp_to(span, time_w, EDGE_MARGIN_PX);
        }
        self.invalidate_canvas(cx);
    }

    /// Tracks the slice under the cursor and the crosshair position while not
    /// dragging. Only the overlay reads these, so a change replays the cached
    /// canvas via a plain notify.
    fn on_mouse_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.drag, TimelineDrag::None) {
            if self.crosshair.take().is_some() {
                cx.notify();
            }
            return;
        }
        let in_content = self.in_content(e.position, cx);
        self.crosshair = in_content.then_some(e.position);
        let hit = self.slice_at(e.position, cx);
        let slice_unchanged = match (self.hover, hit) {
            (Some(h), Some((track, slice))) => h.track == track && h.slice == slice,
            (None, None) => true,
            _ => false,
        };
        if !slice_unchanged {
            self.hover = hit.map(|(track, slice)| Hover { track, slice, at: e.position });
        }
        if in_content || !slice_unchanged {
            cx.notify();
        }
    }

    /// The timeline's mouse/scroll/hover wiring: a press on a group header folds
    /// or shows-all it, on a track label selects that track, on a scroll
    /// thumb/track drives that bar, anywhere else pans; the wheel zooms/pans/
    /// scrolls, and a move updates the crosshair and hover. Holds the content
    /// focus and the Ctrl-A action so a press here makes Ctrl-A select all tracks.
    fn content_handlers(&self, root: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        root.key_context("TraceContent")
            .track_focus(&self.content_focus)
            .on_action(cx.listener(Self::select_all_visible))
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
                        this.clamp_lane_scroll(cx);
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
        // Embedding the canvas cached lets a hover reuse its last scene: a parent
        // `cx.notify()` does not dirty it, so gpui replays its slices.
        let slices = AnyView::from(self.canvas.clone())
            .cached(StyleRefinement::default().absolute().inset_0().size_full());
        // The crosshair, hover tooltip, and drag paint over the cached slices,
        // sharing the canvas's full bounds so the tooltip's viewport clamping is
        // unchanged. A hover repaints only this overlay.
        let overlay = canvas(
            |_, _, _| {},
            overlay_paint(profile, lanes, entity, palette),
        )
        .absolute()
        .left_0()
        .top_0()
        .size_full();
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

/// Routes an in-progress timeline drag to its target: panning the chart, or
/// moving a scroll thumb.
fn timeline_on_drag(
    panel: &mut TimelinePanel,
    event: &MouseMoveEvent,
    cx: &mut Context<TimelinePanel>,
) {
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
            let scrolled = (panel.scroll_y - dy).clamp(0.0, panel.max_scroll(cx));
            panel.scroll_y = scrolled;
            panel.last_mouse = event.position;
        }
        TimelineDrag::VThumb(grab) => panel.scroll_thumb_to(event.position, grab),
        TimelineDrag::HThumb(grab) => panel.pan_thumb_to(event.position, grab, cx),
        TimelineDrag::None => {}
    }
    panel.invalidate_canvas(cx);
}

fn timeline_end_drag(
    panel: &mut TimelinePanel,
    _: &MouseUpEvent,
    cx: &mut Context<TimelinePanel>,
) {
    panel.drag = TimelineDrag::None;
    panel.invalidate_canvas(cx);
}

/// The cached child view painting the timeline's slices and chrome. It reads the
/// timeline's view state and the document and lanes through `parent`, so a hover
/// leaves its cached scene to replay.
struct SliceCanvas {
    parent: WeakEntity<TimelinePanel>,
}

impl Render for SliceCanvas {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(parent) = self.parent.upgrade() else {
            return div().into_any_element();
        };
        let (profile, lanes, panel_height, gutter_width) = {
            let panel = parent.read(cx);
            let model = panel.model.read(cx);
            let Source::Loaded(profile) = model.source() else {
                return div().into_any_element();
            };
            let Some(lanes) = model.lane_layout() else {
                return div().into_any_element();
            };
            let (_, panel_height, gutter_width) = panel.shell_layout(cx);
            (profile.clone(), lanes, panel_height, gutter_width)
        };
        let palette = Palette::from_colors(&bviz_ui_theme::colors(window));
        let prepaint = fit_prepaint(profile.clone(), lanes.clone(), parent.clone(), panel_height, gutter_width);
        let paint = slice_paint(profile, lanes, parent, palette);
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
        let width_of = |b| {
            Regions::new(b, profile.counters().len(), panel_height, gutter_width, flame_total)
                .time_w
        };
        let time_w = width_of(bounds);
        let old_time_w = width_of(old_bounds);
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
    cache: &RefCell<Option<SliceDrawCache>>,
    profile: &Arc<Profile>,
    lanes: &Rc<LaneLayout>,
    view: TimeView,
    viewport: Viewport,
) -> Rc<Vec<VisibleSlice>> {
    if let Some(c) = cache.borrow().as_ref() {
        if Arc::ptr_eq(&c.profile, profile)
            && Rc::ptr_eq(&c.lanes, lanes)
            && c.view == view
            && c.viewport == viewport
        {
            return c.slices.clone();
        }
    }
    let slices = Rc::new(visible_slices(
        profile,
        lanes,
        Cull { view: &view, viewport: &viewport, metrics: &METRICS, min_px: MIN_SLICE_PX },
    ));
    *cache.borrow_mut() = Some(SliceDrawCache {
        profile: profile.clone(),
        lanes: lanes.clone(),
        view,
        viewport,
        slices: slices.clone(),
    });
    slices
}

/// The cached canvas's paint closure: the flame timeline — gridlines, lanes and
/// slices, phase markers, gutter, scroll bars, ruler, and (for the Counters tab)
/// the counter plots — reading the live view state through `parent`.
fn slice_paint(
    profile: Arc<Profile>,
    lanes: Rc<LaneLayout>,
    parent: Entity<TimelinePanel>,
    palette: Palette,
) -> impl FnOnce(Bounds<Pixels>, (), &mut Window, &mut App) {
    move |bounds, _, window, cx| {
        let layout = &*lanes;
        let n_counters = profile.counters().len();
        let flame_total = flame_total_height(layout);
        let (tv, scroll, gutter_width, selected, chrome, regions, show_counters, slices) = {
            let panel = parent.read(cx);
            let tv = panel.view;
            let scroll = panel.scroll_y;
            let (tab, panel_height, gutter_width) = panel.shell_layout(cx);
            let (selected, chrome) = {
                let model = panel.model.read(cx);
                (
                    model.selected_tracks().to_vec(),
                    model.group_chrome().unwrap_or_default(),
                )
            };
            let regions =
                Regions::new(bounds, n_counters, panel_height, gutter_width, flame_total);
            let vp = Viewport {
                width: regions.time_w,
                height: regions.flame_h(),
                scroll_y: scroll,
            };
            let show_counters = regions.has_graph && tab == BottomTab::Counters;
            let slices = cached_slices(&panel.slice_draw, &profile, &lanes, tv, vp);
            (tv, scroll, gutter_width, selected, chrome, regions, show_counters, slices)
        };

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

        let flame_mask = ContentMask {
            bounds: Bounds::new(
                point(px(regions.content_left), px(regions.flame_top)),
                size(px(regions.time_w), px(regions.flame_h())),
            ),
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
        let (vlayout, hlayout) = paint_scrollbars(
            profile.span(),
            tv,
            flame_total,
            regions,
            scroll,
            &palette,
            window,
        );
        parent.update(cx, |panel, _| {
            panel.vbar = vlayout;
            panel.hbar = hlayout;
        });

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

/// The overlay paint closure, layered above the cached canvas: the crosshair, the
/// hover tooltip, and — while a drag runs — the window-global drag handlers. It
/// reads the live crosshair/hover/drag state through `parent`, so it repaints on
/// a hover without disturbing the canvas's cached scene.
fn overlay_paint(
    profile: Arc<Profile>,
    lanes: Rc<LaneLayout>,
    parent: Entity<TimelinePanel>,
    palette: Palette,
) -> impl FnOnce(Bounds<Pixels>, (), &mut Window, &mut App) {
    move |bounds, _, window, cx| {
        let n_counters = profile.counters().len();
        let (tv, tab, panel_height, gutter_width, crosshair, hover, dragging) = {
            let panel = parent.read(cx);
            let tv = panel.view;
            let (tab, panel_height, gutter_width) = panel.shell_layout(cx);
            (
                tv,
                tab,
                panel_height,
                gutter_width,
                panel.crosshair,
                panel.hover,
                !matches!(panel.drag, TimelineDrag::None),
            )
        };
        let flame_total = flame_total_height(&lanes);
        let regions = Regions::new(bounds, n_counters, panel_height, gutter_width, flame_total);
        let show_counters = regions.has_graph && tab == BottomTab::Counters;
        if let Some(at) = crosshair {
            if f32::from(at.x) >= regions.content_left {
                paint_crosshair(&profile, tv, regions, at, show_counters, &palette, window, cx);
            }
        }
        if let Some(h) = hover {
            let track = &profile.threads()[h.track];
            let slice = profile.slices(track)[h.slice];
            let detail = profile.details(track)[h.slice];
            paint_tooltip(&profile, &slice, &detail, h.at, bounds, &palette, window, cx);
        }
        if dragging {
            track_drag(window, &parent, timeline_on_drag, timeline_end_drag);
        }
    }
}
