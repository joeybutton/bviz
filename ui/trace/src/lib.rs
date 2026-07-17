//! Loads a Bazel `--profile` trace (`.profile.gz` or `.json`) into
//! `bviz_trace_profile` and opens a window rendering its thread tracks as a
//! flame-chart timeline: time on the x axis, stacked call depth on the y axis,
//! with horizontal zoom/pan and vertical scroll.

#[global_allocator]
static GLOBAL: bviz_alloc::MiMalloc = bviz_alloc::MiMalloc;

mod chart;
mod counters;
mod flame_graph;
mod groups;
mod rebuild;
mod regions;
mod stats;
mod theme;
mod tracks_menu;

use chart::{fit_px_per_us, format_duration, format_time, nice_step, span_seconds};

use counters::{paint_crosshair, paint_graph};
use flame_graph::{FlameBoxCache, FlameHover, flame_scroll_thumb_to};
use groups::{
    GROUP_ORDER, GroupChrome, GroupHit, build_grouped_layout, default_collapsed_groups, group_label,
};
use rebuild::Rebuild;
use regions::{Regions, clamp_gutter_width, clamp_panel_height, flame_total_height, graph_lanes};
use stats::{DEFAULT_STATS_SORT, StatsColumn, StatsOrder, stats_thumb_to};
use theme::Palette;

use bviz_trace_layout::{
    Cull, FlameView, LaneLayout, LaneMetrics, ScopeAggregate, TimeView, Viewport, lay_out_lanes,
    visible_slices,
};
use bviz_trace_profile::{LaneClass, Profile, Slice, SliceDetail, TimeSpan};
use bviz_ui_components::{
    Axis, DEFAULT_MIN_THUMB, Scrollbar, TextInput, centered_notice, centered_spinner,
    load_in_background, track_drag,
};
use bviz_ui_shell::Chrome;
use clap::Parser;
use gpui::{
    AnyElement, App, BorderStyle, Bounds, ContentMask, Context, Div,
    Entity, FocusHandle, Hsla, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, Render, Rgba, ScrollDelta, ScrollWheelEvent,
    ScrollHandle, SharedString, Stateful, Subscription, Task, TextAlign,
    TransformationMatrix, UniformListScrollHandle,
    Window, canvas, div, fill, point, prelude::*, px, quad, size,
};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

gpui::actions!(bviz_trace, [SelectAllTracks]);

/// Vertical sizing of the stacked thread and counter lanes, in pixels. The lane
/// header is a thin top margin above the depth rows — the names live in the
/// gutter — and adjacent lanes are parted by a hairline rule rather than a wide
/// gap.
const METRICS: LaneMetrics = LaneMetrics {
    header_height: 10.0,
    row_height: 18.0,
    lane_gap: 2.0,
    counter_height: 36.0,
};

/// Font size of a gutter lane or counter name, fixed rather than derived from the
/// thin lane header so the names stay legible.
const GUTTER_LABEL_SIZE: f32 = 11.0;

/// Slices rendering narrower than this are culled — too thin to see or label.
const MIN_SLICE_PX: f32 = 1.0;

/// Only slices at least this wide carry a text label; narrower ones would have no
/// room for legible text.
const LABEL_MIN_PX: f32 = 28.0;

/// Inset of a slice's label from its left and right edges.
const SLICE_TEXT_PAD: f32 = 3.0;

/// Deepest horizontal zoom, in pixels per microsecond.
const MAX_PX_PER_US: f64 = 50.0;

/// Horizontal zoom multiplier per wheel notch, shared by the chart's time axis
/// and the graph's width so a notch feels the same in both views.
const ZOOM_PER_NOTCH: f64 = 1.1;

/// Empty space kept reachable past each end of the timeline so its start and end
/// stand clear of the window edges.
const EDGE_MARGIN_PX: f32 = 24.0;

/// Pixels scrolled per wheel line.
const LINE_SCROLL_PX: f32 = 24.0;

/// A box wider than this gets a 1px muted seam on its left edge to divide it
/// from its neighbor; a narrower box keeps its whole fill so it still reads as
/// its color rather than losing a column to the seam.
const SLICE_SEAM_MIN_PX: f32 = 3.0;

/// How far the left-edge seam is mixed from a box's fill toward the background:
/// enough to read as a muted divider against any fill on either theme.
const SLICE_SEAM_MIX: f32 = 0.35;

/// Initial width of the left label gutter, holding lane and counter names clear
/// of the time content so phase-marker lines never cross them; resizable by its
/// drag handle.
const DEFAULT_GUTTER_WIDTH: f32 = 170.0;
/// Narrowest the gutter drags to, keeping room for a truncated name.
const MIN_GUTTER: f32 = 80.0;
/// Width of the vertical drag handle at the gutter's right edge.
const GUTTER_HANDLE_WIDTH: f32 = 6.0;

/// Smallest height the flame section keeps when the splitter is dragged down.
const FLAME_MIN_HEIGHT: f32 = 60.0;

/// Initial height of the bottom panel before the tab-bar splitter is dragged.
const DEFAULT_PANEL_HEIGHT: f32 = 280.0;

/// Smallest chart area kept below a counter lane's header, so dragging the
/// splitter down squishes the graphs to a readable sliver rather than to nothing.
const MIN_CHART_HEIGHT: f32 = 12.0;

/// Height of the time ruler band at the top of the timeline.
const RULER_HEIGHT: f32 = 20.0;
/// Rough pixel spacing aimed for between ruler ticks; the actual step is rounded
/// to a 1/2/5 multiple near this.
const RULER_TARGET_TICK_PX: f64 = 120.0;

/// Thickness of a scroll bar across its short axis.
const BAR_WIDTH: f32 = 10.0;

/// Font size of a phase-marker label.
const MARKER_LABEL_SIZE: f32 = 11.0;

/// Height of the tab bar between the flame chart and the bottom panel, which
/// doubles as the resize splitter for the panel.
const TAB_BAR_HEIGHT: f32 = 28.0;

/// Width of the open tracks visibility menu and the tallest its track list grows
/// before it scrolls.
const TRACKS_MENU_WIDTH: f32 = 280.0;
const TRACKS_MENU_MAX_LIST: f32 = 320.0;

/// Heights of a stats-table header row and a body row.
const STATS_HEADER_HEIGHT: f32 = 24.0;
const STATS_ROW_HEIGHT: f32 = 22.0;
/// Font size of stats-table cells and headers.
const STATS_TEXT_SIZE: f32 = 12.0;
/// Width of each numeric stats column; the key column takes the remainder.
/// Wide enough for the longest formatted value (e.g. "3155.54s", "871.64ms")
/// plus padding, since cells clip rather than grow.
const STATS_NUM_COL_WIDTH: f32 = 104.0;
/// Horizontal padding inside a stats cell.
const STATS_CELL_PAD: f32 = 8.0;

/// Hover-tooltip text size, inner padding, gap between lines, and offset from the
/// cursor.
const TOOLTIP_TEXT_SIZE: f32 = 12.0;
const TOOLTIP_PAD: f32 = 6.0;
const TOOLTIP_LINE_GAP: f32 = 4.0;
const TOOLTIP_OFFSET: f32 = 12.0;

/// Radius of a crosshair value dot, font size of its value labels, and the gap
/// between the crosshair line and a value label.
const CROSSHAIR_DOT_RADIUS: f32 = 3.0;
const CROSSHAIR_VALUE_SIZE: f32 = 11.0;
const CROSSHAIR_LABEL_GAP: f32 = 5.0;

/// Which panel the tab bar below the flame chart shows. The counter plots share
/// the flame chart's time axis; the flame graph and stats aggregate the selected
/// tracks.
#[derive(Clone, Copy, PartialEq)]
enum BottomTab {
    Counters,
    FlameGraph,
    Stats,
}

impl BottomTab {
    /// The tabs in bar order, with their labels.
    const ALL: [(BottomTab, &'static str); 3] = [
        (BottomTab::Counters, "Counters"),
        (BottomTab::FlameGraph, "Flame Graph"),
        (BottomTab::Stats, "Stats"),
    ];
}

/// What a left-button drag is currently moving.
#[derive(Clone, Copy)]
enum Drag {
    None,
    /// A press on the flame chart pans the timeline (and scrolls its lanes).
    Pan,
    /// A press on the flame chart's lane vertical or time horizontal scroll
    /// thumb/track, holding the grab offset from the thumb's leading edge.
    VThumb(f32),
    HThumb(f32),
    /// A press inside the flame-graph panel pans it.
    FlamePan,
    /// A press on the flame-graph panel's vertical or horizontal scroll
    /// thumb/track, holding that grab offset.
    FlameVThumb(f32),
    FlameHThumb(f32),
    /// A press on the stats-table scroll thumb/track, holding that grab offset.
    StatsThumb(f32),
    /// A press on the tracks-menu scroll thumb/track, holding that grab offset.
    MenuThumb(f32),
    /// A press on the tab bar resizes the bottom panel.
    Splitter,
    /// A press on the gutter handle resizes the gutter.
    Gutter,
}

/// The slice under the cursor and where the cursor was when it was picked, so a
/// tooltip can be drawn for it.
#[derive(Clone, Copy)]
struct Hover {
    track: usize,
    slice: usize,
    at: Point<Pixels>,
}

/// Height of a group header band and of a group's "show more" row.
const GROUP_BAND_HEIGHT: f32 = 18.0;
/// Side of the show-all icon at a group band's gutter edge.
const GROUP_ICON: f32 = 12.0;
/// Lanes shown per group before the rest fold behind the band's show-all toggle.
const TOP_N_PER_GROUP: usize = 12;

/// The loaded profile, a load in progress, or a message explaining why none is
/// shown.
enum Source {
    Loading,
    Loaded(Arc<Profile>),
    Message(String),
}

/// Loads and renders a Bazel trace profile as a flame chart.
///
/// The time view, vertical scroll, and captured viewport bounds are shared cells
/// so the canvas can both read them while painting and, on the first frame, set
/// the initial fit-to-window view (which needs the viewport size, known only at
/// paint time).
struct TraceView {
    focus_handle: FocusHandle,
    /// Focus for the trace content, held by the timeline so Ctrl-A (select all
    /// tracks) dispatches to it; a descendant of the window's focus, so the
    /// window's own actions (quit, tab) still fire.
    content_focus: FocusHandle,
    source: Source,
    /// Horizontal time-to-pixel mapping (pan and zoom).
    view: Rc<Cell<TimeView>>,
    /// How far the lanes are scrolled up, in pixels.
    scroll_y: Rc<Cell<f32>>,
    /// Horizontal window into the aggregated flame graph's normalized width (the
    /// flame-graph panel's zoom and pan); held as a fraction so a resize stays
    /// invertible.
    flame_view: Rc<Cell<FlameView>>,
    /// How far the flame-graph panel is scrolled down, in pixels.
    flame_scroll_y: Rc<Cell<f32>>,
    /// Whether the initial fit-to-window view has been applied.
    fitted: Rc<Cell<bool>>,
    /// Viewport bounds captured during canvas paint, so handlers can map a window
    /// cursor to viewport space and bound the zoom to a fit.
    viewport: Rc<Cell<Bounds<Pixels>>>,
    /// The flame-graph panel's own bounds, captured during its paint, so its
    /// handlers map the cursor within the panel rather than the whole window.
    flame_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The flame chart's lane vertical bar and time horizontal bar geometry, set
    /// during paint and read by mouse handlers; `None` when that axis fits.
    vbar: Rc<Cell<Option<Scrollbar>>>,
    hbar: Rc<Cell<Option<Scrollbar>>>,
    /// The flame-graph panel's own vertical and horizontal bar geometry.
    flame_vbar: Rc<Cell<Option<Scrollbar>>>,
    flame_hbar: Rc<Cell<Option<Scrollbar>>>,
    drag: Drag,
    last_mouse: Point<Pixels>,
    /// Height of the bottom panel, set by the tab-bar splitter; clamped on use.
    panel_height: f32,
    /// Width of the left label gutter, set by its drag handle; clamped on use.
    gutter_width: f32,
    /// Which panel the tab bar shows.
    bottom_tab: BottomTab,
    /// Which tracks the flame chart draws, indexed by `profile.threads()` order.
    visible_tracks: Vec<bool>,
    /// Which tracks the flame graph and stats aggregate over, indexed likewise.
    selected_tracks: Vec<bool>,
    /// Which lane-class groups are collapsed in the chart, indexed by
    /// [`GROUP_ORDER`]. Loading starts collapsed — it holds one lane per external
    /// repository, hundreds on a large build.
    collapsed_groups: [bool; GROUP_ORDER.len()],
    /// Which lane-class groups are folded in the tracks menu, indexed likewise;
    /// independent of the chart's collapse so the menu can manage a group's
    /// visibility while its lanes are collapsed in the chart.
    menu_collapsed_groups: [bool; GROUP_ORDER.len()],
    /// Which groups show every lane rather than the [`TOP_N_PER_GROUP`] busiest,
    /// indexed by [`GROUP_ORDER`].
    group_show_all: [bool; GROUP_ORDER.len()],
    /// Bumped whenever `selected_tracks` changes, keying the aggregate build so a
    /// stale build's result is discarded when the selection moves on.
    selection_version: u64,
    /// The track a shift-click range-selection extends from, if any.
    select_anchor: Option<usize>,
    /// Whether the tracks visibility menu is open.
    tracks_menu_open: bool,
    /// The tracks menu's filter field; its text narrows the listed tracks.
    track_filter: Entity<TextInput>,
    _filter_observer: Subscription,
    /// The full thread/counter lane layout for the loaded profile, built on first
    /// use. It depends only on the immutable profile, so it is cleared only when a
    /// new profile loads.
    lanes: Rc<RefCell<Option<Rc<LaneLayout>>>>,
    /// The lane layout restricted to the visible tracks and restacked into
    /// lane-class groups, derived from `lanes` and cleared whenever the
    /// visibility, collapse, or show-all state changes.
    visible_lanes: Rc<RefCell<Option<Rc<LaneLayout>>>>,
    /// The group headers and show-more rows over `visible_lanes`, built and
    /// cleared alongside it.
    group_chrome: Rc<RefCell<Option<Rc<GroupChrome>>>>,
    /// The selected tracks' aggregate — its flame graph and stats rows, held
    /// together so the rows' and laid-out boxes' name handles resolve against
    /// the graph that produced them. Built on demand off the UI thread (walking
    /// every slice would freeze a paint) and swapped in when ready; the previous
    /// value stays shown while a rebuild runs, so a selection change doesn't
    /// blank the view.
    aggregates: Rc<RefCell<Option<Arc<ScopeAggregate>>>>,
    /// The flame graph's memoized box layout; see [`FlameBoxCache`].
    flame_boxes: Rc<RefCell<Option<FlameBoxCache>>>,
    /// Background-build bookkeeping for `aggregates`/`flame_boxes`, keyed on
    /// `selection_version`.
    agg: Rebuild<u64>,
    /// Which column the stats table sorts by, and whether descending.
    stats_sort: StatsColumn,
    stats_desc: bool,
    /// The stats rows in sorted display order for the active key and sort, built
    /// off the UI thread (sorting ~2M rows would freeze a paint) and swapped in
    /// when ready. The previous order stays shown while a re-sort runs, so a
    /// header click doesn't blank the table.
    stats_order: Rc<RefCell<Option<Rc<StatsOrder>>>>,
    /// Background-sort bookkeeping for `stats_order`, keyed on
    /// `(selection_version, sort, desc)`.
    stats_build: Rebuild<(u64, StatsColumn, bool)>,
    /// Scroll position of the virtualized stats table body.
    stats_scroll: UniformListScrollHandle,
    /// Stats-table scroll-bar geometry set during paint, read by mouse handlers;
    /// `None` when the body fits and so shows no bar.
    stats_bar: Rc<Cell<Option<Scrollbar>>>,
    /// The tracks menu's scroll handle and its scroll-bar geometry (stashed each
    /// frame for hit-testing), mirroring the stats table's bar.
    menu_scroll: ScrollHandle,
    menu_bar: Rc<Cell<Option<Scrollbar>>>,
    /// The slice under the cursor, if any, drawn as a tooltip.
    hover: Option<Hover>,
    /// The flame-graph box under the cursor in graph mode, drawn as a tooltip.
    flame_hover: Option<FlameHover>,
    /// The cursor position while it hovers the time-content column and no drag is
    /// in progress, drawn as a crosshair; `None` otherwise.
    crosshair: Option<Point<Pixels>>,
    /// Holds the background load task so it isn't cancelled on drop.
    _load_task: Task<()>,
}

impl TraceView {
    fn new(
        load: std::thread::JoinHandle<Result<Profile, String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        // Focus the content so Ctrl-A reaches it; the window's actions still fire
        // since the content focus is a descendant of the window focus.
        let content_focus = cx.focus_handle();
        content_focus.focus(window, cx);
        cx.bind_keys([gpui::KeyBinding::new(
            "ctrl-a",
            SelectAllTracks,
            Some("TraceContent"),
        )]);
        let track_filter = cx.new(|cx| TextInput::new(cx, "Filter tracks…", 0));
        // Re-render the menu list as the filter text changes; TextInput notifies
        // on every edit.
        let filter_observer = cx.observe(&track_filter, |_, _, cx| cx.notify());
        let load_task = load_in_background(
            cx,
            move || load.join().unwrap_or_else(|_| Err("profile loader thread panicked".into())),
            |view, result, cx| view.on_profile_loaded(result, cx),
        );
        Self {
            focus_handle,
            content_focus,
            source: Source::Loading,
            view: Rc::new(Cell::new(TimeView {
                start_us: 0.0,
                px_per_us: 1.0,
            })),
            scroll_y: Rc::new(Cell::new(0.0)),
            flame_view: Rc::new(Cell::new(FlameView::FIT)),
            flame_scroll_y: Rc::new(Cell::new(0.0)),
            fitted: Rc::new(Cell::new(false)),
            viewport: Rc::new(Cell::new(Bounds::default())),
            flame_bounds: Rc::new(Cell::new(Bounds::default())),
            vbar: Rc::new(Cell::new(None)),
            hbar: Rc::new(Cell::new(None)),
            flame_vbar: Rc::new(Cell::new(None)),
            flame_hbar: Rc::new(Cell::new(None)),
            drag: Drag::None,
            last_mouse: Point::default(),
            panel_height: DEFAULT_PANEL_HEIGHT,
            gutter_width: DEFAULT_GUTTER_WIDTH,
            bottom_tab: BottomTab::Counters,
            visible_tracks: Vec::new(),
            selected_tracks: Vec::new(),
            collapsed_groups: default_collapsed_groups(),
            menu_collapsed_groups: default_collapsed_groups(),
            group_show_all: [false; GROUP_ORDER.len()],
            selection_version: 0,
            select_anchor: None,
            tracks_menu_open: false,
            track_filter,
            _filter_observer: filter_observer,
            lanes: Rc::new(RefCell::new(None)),
            visible_lanes: Rc::new(RefCell::new(None)),
            group_chrome: Rc::new(RefCell::new(None)),
            aggregates: Rc::new(RefCell::new(None)),
            flame_boxes: Rc::new(RefCell::new(None)),
            agg: Rebuild::new(),
            stats_sort: DEFAULT_STATS_SORT.0,
            stats_desc: DEFAULT_STATS_SORT.1,
            stats_order: Rc::new(RefCell::new(None)),
            stats_build: Rebuild::new(),
            stats_scroll: UniformListScrollHandle::new(),
            stats_bar: Rc::new(Cell::new(None)),
            menu_scroll: ScrollHandle::new(),
            menu_bar: Rc::new(Cell::new(None)),
            hover: None,
            flame_hover: None,
            crosshair: None,
            _load_task: load_task,
        }
    }

    fn on_profile_loaded(&mut self, result: Result<Profile, String>, cx: &mut Context<Self>) {
        self.invalidate_aggregates();
        // The lane layouts depend only on the profile, so they must be dropped
        // when a new profile loads.
        self.lanes.borrow_mut().take();
        self.visible_lanes.borrow_mut().take();
        self.flame_view.set(FlameView::FIT);
        self.source = match result {
            Ok(profile) => Source::Loaded(Arc::new(profile)),
            Err(err) => Source::Message(err),
        };
        if let Source::Loaded(profile) = &self.source {
            let profile = profile.clone();
            self.init_tracks(&profile);
        }
        cx.notify();
    }

    /// The default track selection for a freshly loaded profile: every
    /// non-critical-path lane selected. The critical-path lane holds redirected
    /// copies of worker time, so leaving it out of the default selection keeps
    /// the aggregate from double-counting that time.
    fn init_tracks(&mut self, profile: &Profile) {
        self.visible_tracks = vec![true; profile.threads().len()];
        self.collapsed_groups = default_collapsed_groups();
        self.menu_collapsed_groups = default_collapsed_groups();
        self.group_show_all = [false; GROUP_ORDER.len()];
        self.selected_tracks = profile
            .threads()
            .iter()
            .map(|t| t.lane_class != LaneClass::CriticalPath)
            .collect();
        self.select_anchor = None;
        self.selection_version += 1;
    }

    /// The flame chart's mouse/scroll/hover wiring: a press on a track label
    /// selects that track, a press on a scroll thumb/track drives that bar,
    /// anywhere else pans; the wheel zooms/pans/scrolls, and a move updates the
    /// hover (clearing it when the cursor leaves). Holds the content focus and
    /// the Ctrl-A action so a press here makes Ctrl-A select all visible tracks.
    fn body_handlers(&self, root: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        root.key_context("TraceContent")
            .track_focus(&self.content_focus)
            .on_action(cx.listener(Self::select_all_visible))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, window, cx| {
                    // Take content focus so Ctrl-A selects tracks after a chart
                    // interaction (a click may have moved focus to the filter).
                    this.content_focus.focus(window, cx);
                    // A press on a group header collapses or expands it; on a
                    // show-more row, toggles whether the group shows every lane.
                    if let Some(hit) = this.group_row_at(e.position) {
                        match hit {
                            GroupHit::Collapse(g) => this.toggle_group_collapsed(g),
                            GroupHit::ShowAll(g) => this.toggle_group_show_all(g),
                        }
                        cx.notify();
                        return;
                    }
                    // A press on a track label selects it rather than panning;
                    // ctrl/cmd toggles it, shift extends a range.
                    if let Some(track) = this.track_label_at(e.position) {
                        let ctrl = e.modifiers.control || e.modifiers.platform;
                        this.select_track(track, e.modifiers.shift, ctrl);
                        cx.notify();
                        return;
                    }
                    // A press on a scroll thumb/track drives that bar; anywhere
                    // else pans. The window-global drag handlers are installed
                    // from the canvas paint while a drag is set, so repaint now.
                    this.press(e.position);
                    cx.notify();
                }),
            )
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
            // Drop the crosshair and hover tooltips when the cursor leaves.
            if !*hovered {
                let cleared = this.crosshair.take().is_some()
                    | this.hover.take().is_some()
                    | this.flame_hover.take().is_some();
                if cleared {
                    cx.notify();
                }
            }
        }))
    }

    /// The whole trace body: the flame chart drawn by one full-window canvas
    /// (ruler, gridlines, lanes, slices, phase markers, gutter, scroll bars, and
    /// — for the Counters tab — the counter plots), with the tab bar, the
    /// selected bottom panel, and the gutter-handle drag as overlays. The canvas
    /// prepaint and paint closures are `'static`, so the builders take owned
    /// snapshots (`Rc`s and the const metrics) to capture rather than borrowing
    /// the view.
    fn trace_body(
        &self,
        profile: Arc<Profile>,
        lanes: Rc<LaneLayout>,
        palette: Palette,
        colors: &bviz_ui_theme::Colors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = self.view.clone();
        let scroll_y = self.scroll_y.clone();
        let fitted = self.fitted.clone();
        let viewport = self.viewport.clone();
        let vbar = self.vbar.clone();
        let hbar = self.hbar.clone();
        let entity = cx.entity();
        let panel_height = self.panel_height;
        let gutter_width = self.gutter_width;
        let prepaint = Self::fit_prepaint(
            profile.clone(),
            lanes.clone(),
            view.clone(),
            fitted.clone(),
            viewport.clone(),
            panel_height,
            gutter_width,
        );
        let regions = self.regions();
        let origin = viewport.get().origin;
        let canvas = canvas(
            prepaint,
            Self::chart_paint(
                profile.clone(),
                lanes.clone(),
                view,
                scroll_y,
                vbar,
                hbar,
                entity,
                palette,
            ),
        )
        .absolute()
        .left_0()
        .top_0()
        .size_full();

        // The tab bar spans the content column between the flame chart and the
        // bottom panel, doubling as the panel's resize splitter.
        let tab_bar = self.tab_bar(regions, origin, palette, colors, cx);
        // The selected panel sits over the content column of the bottom region;
        // the counter plots are drawn by the canvas above instead.
        let panel = self.bottom_panel(regions, origin, palette, colors, cx);
        let gutter_handle = self.gutter_handle(regions, origin, cx);
        let tracks_menu = self.tracks_menu(palette, colors, cx);
        let viewport = div()
            .id("trace-viewport")
            .relative()
            .size_full()
            .overflow_hidden()
            .child(canvas)
            .children(panel)
            .child(tab_bar)
            .child(gutter_handle)
            .child(tracks_menu);
        self.body_handlers(viewport, cx)
    }

    /// Prepaint fit: capture the painted viewport bounds, then fit the time view
    /// to the span before the first paint and, on a later width change, rescale
    /// the zoom relative to the window so a resize stays invertible; finally hold
    /// the fit as the zoom floor and clamp the pan to the span.
    fn fit_prepaint(
        profile: Arc<Profile>,
        lanes: Rc<LaneLayout>,
        view: Rc<Cell<TimeView>>,
        fitted: Rc<Cell<bool>>,
        viewport: Rc<Cell<Bounds<Pixels>>>,
        panel_height: f32,
        gutter_width: f32,
    ) -> impl FnOnce(Bounds<Pixels>, &mut Window, &mut App) {
        move |bounds: Bounds<Pixels>, _: &mut Window, _: &mut App| {
            let old_bounds = viewport.get();
            viewport.set(bounds);
            // The time axis maps to the time column, past the gutter and clear
            // of the reserved vertical-bar column.
            let flame_total = flame_total_height(&lanes);
            let regions = Regions::new(
                bounds,
                profile.counters().len(),
                panel_height,
                gutter_width,
                flame_total,
            );
            let time_w = regions.time_w;
            let span = profile.span();
            if !fitted.get() {
                view.set(TimeView::fit(span, time_w, EDGE_MARGIN_PX));
                fitted.set(true);
            } else {
                // On a width change, keep the zoom relative to the window so a
                // resize stays invertible — without this the view spills over
                // (and a scroll bar appears) after a grow-then-shrink cycle.
                let old_time_w = Regions::new(
                    old_bounds,
                    profile.counters().len(),
                    panel_height,
                    gutter_width,
                    flame_total,
                )
                .time_w;
                if old_bounds.size.width > px(0.) && (old_time_w - time_w).abs() > f32::EPSILON
                {
                    let mut v = view.get();
                    v.rescale_to_width(span, old_time_w, time_w, EDGE_MARGIN_PX);
                    view.set(v);
                }
            }
            // Keep the fit as the zoom floor and the pan within the span for
            // the current width.
            let mut v = view.get();
            v.px_per_us = v
                .px_per_us
                .max(fit_px_per_us(span, time_w, EDGE_MARGIN_PX));
            v.clamp_to(span, time_w, EDGE_MARGIN_PX);
            view.set(v);
        }
    }

    /// The chart paint body: the canvas paint closure that draws the flame
    /// timeline — gridlines, lanes and slices, phase markers, gutter, scroll
    /// bars, and (for the Counters tab) the counter plots — reading the live view
    /// state through the entity, then the ruler and hover/crosshair overlays on
    /// top.
    fn chart_paint(
        profile: Arc<Profile>,
        lanes: Rc<LaneLayout>,
        view: Rc<Cell<TimeView>>,
        scroll_y: Rc<Cell<f32>>,
        vbar: Rc<Cell<Option<Scrollbar>>>,
        hbar: Rc<Cell<Option<Scrollbar>>>,
        entity: Entity<TraceView>,
        palette: Palette,
    ) -> impl FnOnce(Bounds<Pixels>, (), &mut Window, &mut App) {
        move |bounds, _, window, cx| {
            let layout = &*lanes;
            let tv = view.get();
            let n_counters = profile.counters().len();
            let (tab, panel_height, gutter_width, selected, crosshair, hover, dragging, chrome) = {
                let this = entity.read(cx);
                (
                    this.bottom_tab,
                    this.panel_height,
                    this.gutter_width,
                    this.selected_tracks.clone(),
                    this.crosshair,
                    this.hover,
                    !matches!(this.drag, Drag::None),
                    this.group_chrome().unwrap_or_default(),
                )
            };
            let flame_total = flame_total_height(layout);
            let regions =
                Regions::new(bounds, n_counters, panel_height, gutter_width, flame_total);
            let scroll = scroll_y.get();
            let vp = Viewport {
                width: regions.time_w,
                height: regions.flame_h(),
                scroll_y: scroll,
            };
            // The counter plots share the flame chart's time axis, so they
            // paint on this canvas below the tab bar when their tab is up.
            let show_counters = regions.has_graph && tab == BottomTab::Counters;

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

            // The flame lanes clip to their band so rows panned past the
            // bottom don't bleed into the tab bar or panel.
            let flame_mask = ContentMask {
                bounds: Bounds::new(
                    point(px(regions.content_left), px(regions.flame_top)),
                    size(px(regions.time_w), px(regions.flame_h())),
                ),
            };
            window.with_content_mask(Some(flame_mask), |window| {
                // A hairline parts adjacent lanes, drawn first so slices
                // paint over it and it shows only in the gaps.
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
                // Group header bands read across the full width as section
                // dividers; their gutter labels and show-all icon paint later,
                // over the opaque gutter.
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
                for vs in visible_slices(
                    &profile,
                    layout,
                    Cull { view: &tv, viewport: &vp, metrics: &METRICS, min_px: MIN_SLICE_PX },
                ) {
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
                paint_graph(&profile, tv, regions, &palette, window);
            }
            paint_markers(&profile, tv, regions, show_counters, &palette, window, cx);
            paint_scrollbars(
                profile.span(),
                tv,
                flame_total,
                regions,
                scroll,
                &palette,
                &vbar,
                &hbar,
                window,
            );

            // The opaque gutter covers any lane content that panned under
            // it, then carries the lane and (for the Counters tab) counter
            // names.
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

            // The ruler paints over scrolled-up rows; the tooltip paints
            // over everything.
            paint_ruler(&ticks, regions, bounds, &palette, window, cx);
            if let Some(at) = crosshair {
                if f32::from(at.x) >= regions.content_left {
                    paint_crosshair(
                        &profile,
                        tv,
                        regions,
                        at,
                        show_counters,
                        &palette,
                        window,
                        cx,
                    );
                }
            }
            if let Some(h) = hover {
                let track = &profile.threads()[h.track];
                let slice = profile.slices(track)[h.slice];
                let detail = profile.details(track)[h.slice];
                paint_tooltip(&profile, &slice, &detail, h.at, bounds, &palette, window, cx);
            }
            if dragging {
                track_drag(window, &entity, on_drag, end_drag);
            }
        }
    }

    /// A thin overlay strip at the gutter's right edge that begins a width drag;
    /// a double-click resets the gutter to its default width.
    fn gutter_handle(
        &self,
        regions: Regions,
        origin: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let drag_entity = cx.entity();
        div()
            .id("trace-gutter-handle")
            .absolute()
            .left(px(regions.content_left - f32::from(origin.x) - GUTTER_HANDLE_WIDTH / 2.0))
            .top_0()
            .w(px(GUTTER_HANDLE_WIDTH))
            .h_full()
            .cursor_col_resize()
            .on_mouse_down(
                MouseButton::Left,
                move |e: &MouseDownEvent, _: &mut Window, cx: &mut App| {
                    drag_entity.update(cx, |this, cx| {
                        if e.click_count == 2 {
                            this.gutter_width = DEFAULT_GUTTER_WIDTH;
                        } else {
                            this.drag = Drag::Gutter;
                        }
                        cx.notify();
                    });
                    cx.stop_propagation();
                },
            )
    }

    /// The tab bar between the flame chart and the bottom panel: a row of tab
    /// buttons over the content column, positioned from the last painted bounds.
    /// A press on its background begins a panel-resize drag (double-click resets
    /// the panel height); a tab button switches the panel.
    fn tab_bar(
        &self,
        regions: Regions,
        origin: Point<Pixels>,
        palette: Palette,
        colors: &bviz_ui_theme::Colors,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let (text, background, selected) = (colors.text, colors.background, colors.selected);
        let muted = bviz_ui_theme::mix(text, background, 0.4);
        let active = self.bottom_tab;
        let tabs: Vec<_> = BottomTab::ALL
            .into_iter()
            .map(|(tab, label)| {
                let is_active = tab == active;
                div()
                    .id(SharedString::from(format!("trace-tab-{label}")))
                    .flex()
                    .items_center()
                    .px_3()
                    .h_full()
                    .cursor_pointer()
                    .text_size(px(STATS_TEXT_SIZE))
                    .text_color(if is_active { text } else { muted })
                    .when(is_active, |t| {
                        t.bg(background).border_t_2().border_color(selected)
                    })
                    .hover(|t| t.bg(background))
                    // A press on a tab must switch it, not begin a panel resize.
                    .on_mouse_down(MouseButton::Left, |_, _, cx: &mut App| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_bottom_tab(tab);
                        cx.notify();
                    }))
                    .child(label)
            })
            .collect();
        div()
            .id("trace-tab-bar")
            .absolute()
            .left(px(regions.content_left - f32::from(origin.x)))
            .top(px(regions.tab_top - f32::from(origin.y)))
            .w(px((regions.content_right - regions.content_left).max(0.0)))
            .h(px(TAB_BAR_HEIGHT))
            .flex()
            .flex_row()
            .items_stretch()
            .bg(palette.header_band)
            .border_t_1()
            .border_color(palette.divider)
            .cursor_row_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    if e.click_count == 2 {
                        this.panel_height = DEFAULT_PANEL_HEIGHT;
                    } else {
                        this.drag = Drag::Splitter;
                        this.last_mouse = e.position;
                    }
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .children(tabs)
            // The flame graph's work-class legend sits at the far right of the
            // bar while its tab is up, keyed to the box fills.
            .child(div().flex_1())
            .children((active == BottomTab::FlameGraph).then(|| {
                div()
                    .flex()
                    .items_center()
                    .pr_2()
                    .child(theme::role_legend(
                        palette.label,
                        palette.background,
                        STATS_TEXT_SIZE,
                    ))
            }))
    }

    /// The bottom-panel overlay for the active tab, positioned over the content
    /// column of the panel region. The Counters tab draws on the flame chart's
    /// canvas, so it needs no overlay.
    fn bottom_panel(
        &self,
        regions: Regions,
        origin: Point<Pixels>,
        palette: Palette,
        colors: &bviz_ui_theme::Colors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let content: AnyElement = match self.bottom_tab {
            BottomTab::Counters => return None,
            BottomTab::FlameGraph => match self.aggregates() {
                Some(aggregates) => {
                    self.flame_graph_panel(aggregates, palette, cx).into_any_element()
                }
                None => centered_spinner("Building flame graph…", colors).into_any_element(),
            },
            BottomTab::Stats => self.stats_panel(palette, colors, cx).into_any_element(),
        };
        Some(
            div()
                .id("trace-panel")
                .absolute()
                .left(px(regions.content_left - f32::from(origin.x)))
                .top(px(regions.panel_top - f32::from(origin.y)))
                .w(px((regions.content_right - regions.content_left).max(0.0)))
                .h(px(regions.panel_h()))
                .flex()
                .flex_col()
                .overflow_hidden()
                .bg(palette.background)
                .child(content)
                .into_any_element(),
        )
    }

    /// The stats panel: the sortable, virtualized stats table for the current
    /// selection, with the table's scroll-bar press wiring.
    fn stats_panel(
        &self,
        palette: Palette,
        colors: &bviz_ui_theme::Colors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .size_full()
            .overflow_hidden()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    // A press on the scroll thumb/track drives the bar; the canvas
                    // paint re-installs the drag handlers while the drag is set, so
                    // repaint now.
                    this.press_stats(e.position);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_scroll_wheel(|_, _, cx: &mut App| cx.stop_propagation())
            .child(self.stats_table(&palette, colors, cx))
    }

    fn status(&self) -> String {
        match &self.source {
            Source::Loaded(p) => {
                let slices: usize = p.threads().iter().map(|t| t.slices.len()).sum();
                format!(
                    "{} threads · {} slices · {:.3}s",
                    p.threads().len(),
                    slices,
                    span_seconds(p.span()),
                )
            }
            Source::Loading => "Loading…".into(),
            Source::Message(_) => "No profile".into(),
        }
    }

    /// [`Regions`] for the current viewport, panel and gutter sizes, the loaded
    /// profile's counter count, and the flame lanes' total height.
    fn regions(&self) -> Regions {
        let n_counters = match &self.source {
            Source::Loaded(p) => p.counters().len(),
            _ => 0,
        };
        let flame_total = self
            .lane_layout()
            .map(|l| flame_total_height(&l))
            .unwrap_or(0.0);
        Regions::new(
            self.viewport.get(),
            n_counters,
            self.panel_height,
            self.gutter_width,
            flame_total,
        )
    }

    /// The pixel ceiling for the flame chart's lane scroll: the thread-lane
    /// height past the flame region's on-screen height.
    fn max_scroll(&self) -> f32 {
        let total = self
            .lane_layout()
            .map(|l| flame_total_height(&l))
            .unwrap_or(0.0);
        (total - self.regions().flame_h()).max(0.0)
    }

    /// The full lane layout for the loaded profile, built on first use and reused
    /// across paints and mouse handlers. `None` until a profile is loaded.
    fn full_lane_layout(&self) -> Option<Rc<LaneLayout>> {
        let Source::Loaded(profile) = &self.source else {
            return None;
        };
        if self.lanes.borrow().is_none() {
            let t = std::time::Instant::now();
            let layout = lay_out_lanes(profile, &METRICS);
            if std::env::var_os("BVIZ_TRACE_TIMING").is_some() {
                eprintln!("[timing] lay_out_lanes: {:.3}s", t.elapsed().as_secs_f64());
            }
            *self.lanes.borrow_mut() = Some(Rc::new(layout));
        }
        self.lanes.borrow().clone()
    }

    /// The lane layout the flame chart draws: the full layout restricted to the
    /// visible tracks and restacked into lane-class groups, cached until the
    /// visibility, collapse, or show-all state changes. Every paint and hit-test
    /// works from this, so a lane hidden by its group is neither drawn nor
    /// selectable.
    fn lane_layout(&self) -> Option<Rc<LaneLayout>> {
        self.ensure_grouped();
        self.visible_lanes.borrow().clone()
    }

    /// The group headers and show-more rows over [`Self::lane_layout`].
    fn group_chrome(&self) -> Option<Rc<GroupChrome>> {
        self.ensure_grouped();
        self.group_chrome.borrow().clone()
    }

    /// Builds and caches the grouped lane layout and its chrome if stale.
    fn ensure_grouped(&self) {
        if self.visible_lanes.borrow().is_some() {
            return;
        }
        let Some(full) = self.full_lane_layout() else {
            return;
        };
        let Source::Loaded(profile) = &self.source else {
            return;
        };
        let (layout, chrome) = build_grouped_layout(
            profile,
            &full,
            &self.visible_tracks,
            &self.collapsed_groups,
            &self.group_show_all,
        );
        *self.visible_lanes.borrow_mut() = Some(Rc::new(layout));
        *self.group_chrome.borrow_mut() = Some(Rc::new(chrome));
    }

    /// Toggles a lane-class group's collapse and rebuilds the grouped layout.
    fn toggle_group_collapsed(&mut self, group: usize) {
        if let Some(c) = self.collapsed_groups.get_mut(group) {
            *c = !*c;
            self.invalidate_visible_lanes();
        }
    }

    /// Toggles whether a group shows every lane or only the busiest.
    fn toggle_group_show_all(&mut self, group: usize) {
        if let Some(s) = self.group_show_all.get_mut(group) {
            *s = !*s;
            self.invalidate_visible_lanes();
        }
    }

    /// The number of visible tracks and the total.
    fn visible_count(&self) -> (usize, usize) {
        let visible = self.visible_tracks.iter().filter(|v| **v).count();
        (visible, self.visible_tracks.len())
    }

    /// Toggles whether track `i` is drawn in the flame chart, dropping the cached
    /// visible layout so it restacks, and clamping the lane scroll to the new
    /// height.
    fn toggle_track(&mut self, i: usize) {
        if let Some(v) = self.visible_tracks.get_mut(i) {
            *v = !*v;
            self.invalidate_visible_lanes();
        }
    }

    /// Shows or hides every track at once.
    fn set_all_visible(&mut self, visible: bool) {
        for v in &mut self.visible_tracks {
            *v = visible;
        }
        self.invalidate_visible_lanes();
    }

    /// Shows or hides every track in one lane-class group at once.
    fn set_group_visible(&mut self, group: usize, visible: bool) {
        let class = GROUP_ORDER[group];
        let targets: Vec<usize> = {
            let Source::Loaded(profile) = &self.source else {
                return;
            };
            profile
                .threads()
                .iter()
                .enumerate()
                .filter(|(_, t)| t.lane_class == class)
                .map(|(i, _)| i)
                .collect()
        };
        for i in targets {
            if let Some(v) = self.visible_tracks.get_mut(i) {
                *v = visible;
            }
        }
        self.invalidate_visible_lanes();
    }

    /// Folds or unfolds a lane-class group's track list in the tracks menu.
    fn toggle_menu_group_collapsed(&mut self, group: usize) {
        if let Some(c) = self.menu_collapsed_groups.get_mut(group) {
            *c = !*c;
        }
    }

    /// Drops the cached visible layout so it rebuilds and restacks, then clamps
    /// the lane scroll to the new height.
    fn invalidate_visible_lanes(&mut self) {
        self.visible_lanes.borrow_mut().take();
        self.group_chrome.borrow_mut().take();
        let scrolled = self.scroll_y.get().min(self.max_scroll());
        self.scroll_y.set(scrolled);
    }

    /// The aggregated flame graph and stats for the current track selection, or
    /// `None` until the first background build for the loaded profile completes.
    /// Reflects the previous selection while a rebuild is still running;
    /// [`Self::ensure_aggregates`] owns the (re)build.
    fn aggregates(&self) -> Option<Arc<ScopeAggregate>> {
        self.aggregates.borrow().clone()
    }

    /// Makes the current selection's aggregate the shown one: built on demand
    /// off the UI thread (walking every slice is far too costly for a paint) and
    /// swapped in on completion, the previously built graph staying shown until
    /// then. An in-flight build whose selection the inputs have moved past is
    /// abandoned. Idempotent — a no-op once the right value is ready or being
    /// computed — so it is safe to call each frame.
    fn ensure_aggregates(&mut self, cx: &mut Context<Self>) {
        let Source::Loaded(profile) = &self.source else {
            return;
        };
        let key = self.selection_version;
        if self.agg.is_ready(key) {
            // The shown value is already current; cancel a build for a key the
            // inputs have since left so its late result can't clobber it.
            self.agg.cancel_inflight();
            return;
        }
        if self.agg.is_inflight(key) {
            return;
        }
        let generation = self.agg.start(key);
        let profile = profile.clone();
        let selected = self.selected_tracks.clone();
        let task = load_in_background(
            cx,
            move || {
                let t = std::time::Instant::now();
                let aggregate = ScopeAggregate::build_for_tracks(&profile, |i| {
                    selected.get(i).copied().unwrap_or(false)
                });
                if std::env::var_os("BVIZ_TRACE_TIMING").is_some() {
                    eprintln!("[timing] track_aggregate: {:.3}s", t.elapsed().as_secs_f64());
                }
                aggregate
            },
            move |view, aggregates, _cx| {
                // Discard a result the inputs have moved past since this spawn.
                if !view.agg.accept(generation, key) {
                    return;
                }
                *view.aggregates.borrow_mut() = Some(Arc::new(aggregates));
                view.flame_boxes.borrow_mut().take();
                // The sorted order is keyed on the new aggregates; let
                // `ensure_stats_order` re-derive it (the old order keeps
                // showing until it lands).
                view.stats_build.clear_ready();
            },
        );
        self.agg.set_task(task);
    }

    /// Drops the cached flame graph and stats rows and abandons any in-flight
    /// build, so the next [`Self::ensure_aggregates`] rebuilds from scratch.
    /// Called when a new profile loads — unlike a mere selection change, the old
    /// graph is no longer a valid thing to keep showing.
    fn invalidate_aggregates(&mut self) {
        self.aggregates.borrow_mut().take();
        self.flame_boxes.borrow_mut().take();
        self.stats_order.borrow_mut().take();
        self.agg.invalidate();
        self.stats_build.invalidate();
    }

    /// Switches the bottom panel tab, dropping the flame-chart and flame-graph
    /// hover tooltips (the newly shown panel picks its own up on the next move).
    fn set_bottom_tab(&mut self, tab: BottomTab) {
        if self.bottom_tab == tab {
            return;
        }
        self.bottom_tab = tab;
        self.hover = None;
        self.flame_hover = None;
    }
}

impl Render for TraceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = bviz_ui_theme::colors(window);
        let palette = Palette::from_colors(&colors);
        let body = match &self.source {
            Source::Loaded(profile) => {
                // Own the handle so the borrow of `self.source` ends before the
                // `&mut self` build call below.
                let profile = profile.clone();
                // Build (or rebuild) the flame graph and stats off the UI thread
                // for the current selection; keeping them warm on any tab makes
                // the first switch to the graph or stats instant.
                self.ensure_aggregates(cx);
                // Re-sort the stats off the UI thread when the aggregates or sort
                // change; sorting ~2M rows on the paint thread would freeze it.
                self.ensure_stats_order(cx);
                let lanes = self.lane_layout().expect("a loaded profile has a lane layout");
                div()
                    .flex_1()
                    .overflow_hidden()
                    .child(self.trace_body(profile.clone(), lanes, palette, &colors, cx))
            }
            Source::Loading => centered_spinner("Loading…", &colors),
            Source::Message(msg) => centered_notice(msg.clone(), &colors),
        };
        Chrome::new(&self.focus_handle)
            .title("bazel_viz trace")
            .status(self.status())
            .child(body)
    }
}

impl TraceView {
    fn on_scroll_wheel(&mut self, e: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.lane_layout().is_none() {
            return;
        }
        let Source::Loaded(profile) = &self.source else {
            return;
        };
        let span = profile.span();
        let regions = self.regions();
        let time_w = regions.time_w;
        let content_left = regions.content_left;
        let (dx, dy) = match e.delta {
            ScrollDelta::Lines(p) => (p.x * LINE_SCROLL_PX, p.y * LINE_SCROLL_PX),
            ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
        };
        if e.modifiers.control || e.modifiers.platform {
            // Horizontal zoom anchored at the cursor, floored at the fit so the
            // whole span can't be zoomed out of view.
            let rel_x = f32::from(e.position.x) - content_left;
            let dy_lines = dy / LINE_SCROLL_PX;
            let mut v = self.view.get();
            v.zoom_at(rel_x, ZOOM_PER_NOTCH.powf(dy_lines as f64));
            v.px_per_us = v
                .px_per_us
                .clamp(fit_px_per_us(span, time_w, EDGE_MARGIN_PX), MAX_PX_PER_US);
            v.clamp_to(span, time_w, EDGE_MARGIN_PX);
            self.view.set(v);
        } else {
            // Shift folds the vertical wheel into horizontal panning, so
            // shift+wheel pans the timeline regardless of focus (the platform
            // turns shift+wheel into dx only while focused, leaving dy otherwise).
            let (pan_dx, scroll_dy) = if e.modifiers.shift {
                (dx + dy, 0.0)
            } else {
                (dx, dy)
            };
            let scrolled = (self.scroll_y.get() - scroll_dy).clamp(0.0, self.max_scroll());
            self.scroll_y.set(scrolled);
            let mut v = self.view.get();
            v.pan_px(pan_dx);
            v.clamp_to(span, time_w, EDGE_MARGIN_PX);
            self.view.set(v);
        }
        cx.notify();
    }

    /// Tracks the slice under the cursor and the crosshair position while not
    /// dragging. The crosshair follows the cursor, so any in-content move
    /// repaints; the slice tooltip repaints only when the hovered slice changes.
    fn on_mouse_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.drag, Drag::None) {
            // A drag clears the crosshair; notify only if it was showing.
            if self.crosshair.take().is_some() {
                cx.notify();
            }
            return;
        }
        // The crosshair shows anywhere in the time-content column.
        let in_content = self.in_content(e.position);
        self.crosshair = in_content.then_some(e.position);

        let hit = self.slice_at(e.position);
        let slice_unchanged = match (self.hover, hit) {
            (Some(h), Some((track, slice))) => h.track == track && h.slice == slice,
            (None, None) => true,
            _ => false,
        };
        if !slice_unchanged {
            self.hover = hit.map(|(track, slice)| Hover {
                track,
                slice,
                at: e.position,
            });
        }
        // Repaint on every in-content move (the crosshair tracks the cursor) and
        // whenever the hovered slice changes (including the move out of content
        // that clears the crosshair).
        if in_content || !slice_unchanged {
            cx.notify();
        }
    }

    /// Whether `pos` lies over a time-axis region the crosshair shows on: the
    /// flame lanes always, and the counter panel when it is the shown tab (it
    /// shares the time axis). The flame-graph and stats panels and the tab bar do
    /// not map to time, so the crosshair does not extend over them.
    fn in_content(&self, pos: Point<Pixels>) -> bool {
        if self.lane_layout().is_none() {
            return false;
        }
        let regions = self.regions();
        let x = f32::from(pos.x);
        let y = f32::from(pos.y);
        if x < regions.content_left || x >= regions.time_right {
            return false;
        }
        let in_flame = y >= regions.flame_top && y < regions.flame_bottom;
        let in_counters = self.bottom_tab == BottomTab::Counters
            && regions.has_graph
            && y >= regions.panel_top
            && y < regions.panel_bottom;
        in_flame || in_counters
    }

    /// The slice under a window-space cursor, inverting the paint transform:
    /// window → flame lanes → (lane, depth row) and time. `None` over the gutter,
    /// the ruler, the tab bar, the panel, a lane header, or empty space.
    fn slice_at(&self, pos: Point<Pixels>) -> Option<(usize, usize)> {
        let layout = self.lane_layout()?;
        let Source::Loaded(profile) = &self.source else {
            return None;
        };
        let regions = self.regions();
        let x = f32::from(pos.x);
        let y = f32::from(pos.y);
        // The flame lanes, past the gutter, clear of the reserved vertical-bar
        // column and below the ruler, above the tab bar.
        if x < regions.content_left
            || x >= regions.time_right
            || y < regions.flame_top
            || y >= regions.flame_bottom
        {
            return None;
        }
        let content_y = y - regions.flame_top + self.scroll_y.get();
        let lane = layout
            .lanes
            .iter()
            .find(|l| content_y >= l.top && content_y < l.top + l.height)?;
        let rows_top = lane.top + METRICS.header_height;
        if content_y < rows_top {
            return None;
        }
        let depth = ((content_y - rows_top) / METRICS.row_height) as u32;
        let view = self.view.get();
        let cursor_px = x - regions.content_left;
        // Hit-test against a slice's painted span, not its raw time span: a
        // sub-pixel slice paints a marker widened to MIN_SLICE_PX (see
        // visible_slices), so a cursor over that marker sits past the slice's
        // brief duration. Slices are sorted by start and x_of is monotonic, so
        // only those painting from at or left of the cursor can reach it;
        // binary-search that prefix. The first at the cursor's depth whose
        // widened span still covers the cursor is the hit (at most one per depth).
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
    fn group_row_at(&self, pos: Point<Pixels>) -> Option<GroupHit> {
        self.lane_layout()?;
        let chrome = self.group_chrome()?;
        let regions = self.regions();
        let x = f32::from(pos.x);
        let y = f32::from(pos.y);
        let gutter_left = regions.content_left - self.gutter_width;
        if x < gutter_left || x >= regions.content_right || y < regions.flame_top || y >= regions.flame_bottom {
            return None;
        }
        let content_y = y - regions.flame_top + self.scroll_y.get();
        let hits = |top: f32| content_y >= top && content_y < top + GROUP_BAND_HEIGHT;
        let band = chrome.bands.iter().find(|b| hits(b.top))?;
        // The show-all icon sits at the gutter's right edge on a capped, expanded
        // group; a press there toggles show-all, anywhere else toggles collapse.
        let icon_left = regions.content_left - GROUP_ICON - 2.0 * SLICE_TEXT_PAD;
        if band.capped && !band.collapsed && x >= icon_left && x < regions.content_left {
            Some(GroupHit::ShowAll(band.group))
        } else {
            Some(GroupHit::Collapse(band.group))
        }
    }

    /// The track whose gutter label lies under `pos`, if the press hit the flame
    /// chart's track-label column (below the ruler, left of the content, above
    /// the tab bar). `None` elsewhere or over a lane's empty tail.
    fn track_label_at(&self, pos: Point<Pixels>) -> Option<usize> {
        let layout = self.lane_layout()?;
        let regions = self.regions();
        let x = f32::from(pos.x);
        let y = f32::from(pos.y);
        let gutter_left = regions.content_left - self.gutter_width;
        if x < gutter_left
            || x >= regions.content_left
            || y < regions.flame_top
            || y >= regions.flame_bottom
        {
            return None;
        }
        let content_y = y - regions.flame_top + self.scroll_y.get();
        layout
            .lanes
            .iter()
            .find(|l| content_y >= l.top && content_y < l.top + l.height)
            .map(|l| l.track)
    }

    /// Selects `track` for the flame graph and stats, with file-manager gesture
    /// semantics. A plain click selects it alone; a `ctrl` (or cmd) click flips it
    /// in the current set; a `shift` click replaces the selection with the run of
    /// visible lanes from the anchor to `track`; a `shift`+`ctrl` click sets that
    /// run to the anchor's own state, extending a just-added run or clearing a
    /// just-removed one. A plain or ctrl click moves the anchor to `track`; a
    /// shift click leaves it, so the run can be re-adjusted. Bumps the selection
    /// version only when the set actually changes, so a repeat click is free.
    fn select_track(&mut self, track: usize, shift: bool, ctrl: bool) {
        let n = self.selected_tracks.len();
        if track >= n {
            return;
        }
        // The anchor run and the anchor's own selected state, read before any
        // mutation so the range extends from the existing anchor.
        let run = self.visible_run_to(track);
        let anchor_state = self.select_anchor.and_then(|a| self.selected_tracks.get(a).copied());
        let sel = match (shift, ctrl, run) {
            // Plain click: select the one track and anchor there.
            (false, false, _) => {
                let mut sel = vec![false; n];
                sel[track] = true;
                self.select_anchor = Some(track);
                sel
            }
            // Ctrl-click: flip the one track, keeping the rest, and re-anchor.
            (false, true, _) => {
                let mut sel = self.selected_tracks.clone();
                sel[track] = !sel[track];
                self.select_anchor = Some(track);
                sel
            }
            // Shift-click: replace the selection with the anchor run.
            (true, false, Some(run)) => {
                let mut sel = vec![false; n];
                for t in run {
                    sel[t] = true;
                }
                sel
            }
            // Shift-Ctrl-click: set the anchor run to the anchor's state,
            // extending a just-added run or clearing a just-removed one.
            (true, true, Some(run)) => {
                let state = anchor_state.unwrap_or(true);
                let mut sel = self.selected_tracks.clone();
                for t in run {
                    sel[t] = state;
                }
                sel
            }
            // A shift click with no anchor (or an anchor no longer visible) falls
            // back to selecting the one track.
            (true, _, None) => {
                let mut sel = vec![false; n];
                sel[track] = true;
                self.select_anchor = Some(track);
                sel
            }
        };
        if sel != self.selected_tracks {
            self.selected_tracks = sel;
            self.selection_version += 1;
        }
    }

    /// The visible-lane tracks from the selection anchor to `track` inclusive, in
    /// display order; `None` with no anchor or one no longer a visible lane.
    fn visible_run_to(&self, track: usize) -> Option<Vec<usize>> {
        let anchor = self.select_anchor?;
        let order: Vec<usize> = self.lane_layout()?.lanes.iter().map(|l| l.track).collect();
        let ai = order.iter().position(|&t| t == anchor)?;
        let ti = order.iter().position(|&t| t == track)?;
        Some(order[ai.min(ti)..=ai.max(ti)].to_vec())
    }

    /// Selects every currently visible track (the Ctrl-A gesture).
    fn select_all_visible(&mut self, _: &SelectAllTracks, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_tracks != self.visible_tracks {
            self.selected_tracks = self.visible_tracks.clone();
            self.selection_version += 1;
            cx.notify();
        }
    }

    /// If `pos` hits `bar`'s track, begins a thumb drag: the thumb is grabbed
    /// where pressed (a track click centers it there), `drag` becomes the active
    /// drag holding that grab offset, and `thumb_to` maps the press to the
    /// target's scroll offset. Returns whether the press was consumed by the bar.
    fn begin_thumb_drag(
        &mut self,
        pos: Point<Pixels>,
        bar: Option<Scrollbar>,
        drag: impl FnOnce(f32) -> Drag,
        thumb_to: impl FnOnce(&mut Self, Point<Pixels>, f32),
    ) -> bool {
        let Some(bar) = bar else {
            return false;
        };
        if !bar.track_hit(pos) {
            return false;
        }
        let grab = bar.grab(pos);
        self.drag = drag(grab);
        thumb_to(self, pos, grab);
        true
    }

    /// Begins a flame-chart drag at `pos`: the lane vertical or time horizontal
    /// scroll thumb/track if the press hit one, otherwise a pan.
    fn press(&mut self, pos: Point<Pixels>) {
        if self.begin_thumb_drag(pos, self.vbar.get(), Drag::VThumb, scroll_thumb_to)
            || self.begin_thumb_drag(pos, self.hbar.get(), Drag::HThumb, pan_thumb_to)
        {
            return;
        }
        self.drag = Drag::Pan;
        self.last_mouse = pos;
    }

    /// If `pos` hits the tracks-menu scroll bar, begins a thumb drag. Returns
    /// whether the press was consumed by the bar.
    fn press_menu(&mut self, pos: Point<Pixels>) -> bool {
        self.begin_thumb_drag(pos, self.menu_bar.get(), Drag::MenuThumb, menu_thumb_to)
    }

}

/// Routes an in-progress drag to its target: panning the flame chart or the
/// flame-graph panel, moving a scroll thumb, or resizing the panel or gutter.
fn on_drag(view: &mut TraceView, event: &MouseMoveEvent, cx: &mut Context<TraceView>) {
    match view.drag {
        Drag::Pan => {
            let dx = f32::from(event.position.x) - f32::from(view.last_mouse.x);
            let dy = f32::from(event.position.y) - f32::from(view.last_mouse.y);
            if view.lane_layout().is_none() {
                return;
            }
            let Source::Loaded(profile) = &view.source else {
                return;
            };
            let span = profile.span();
            let regions = view.regions();
            let mut v = view.view.get();
            v.pan_px(dx);
            v.clamp_to(span, regions.time_w, EDGE_MARGIN_PX);
            view.view.set(v);
            let scrolled = (view.scroll_y.get() - dy).clamp(0.0, view.max_scroll());
            view.scroll_y.set(scrolled);
            view.last_mouse = event.position;
        }
        Drag::FlamePan => {
            let dx = f32::from(event.position.x) - f32::from(view.last_mouse.x);
            let dy = f32::from(event.position.y) - f32::from(view.last_mouse.y);
            if let Some(fl) = view.flame_layout() {
                let mut fv = fl.view;
                fv.pan_px(dx, fl.width);
                view.flame_view.set(fv);
            }
            let scrolled = (view.flame_scroll_y.get() - dy).clamp(0.0, view.flame_max_scroll());
            view.flame_scroll_y.set(scrolled);
            view.last_mouse = event.position;
        }
        Drag::VThumb(grab) => scroll_thumb_to(view, event.position, grab),
        Drag::HThumb(grab) => pan_thumb_to(view, event.position, grab),
        Drag::FlameVThumb(grab) => flame_scroll_thumb_to(view, event.position, grab),
        Drag::FlameHThumb(grab) => view.flame_hthumb_to(event.position, grab),
        Drag::StatsThumb(grab) => stats_thumb_to(view, event.position, grab),
        Drag::MenuThumb(grab) => menu_thumb_to(view, event.position, grab),
        Drag::Splitter => {
            let Source::Loaded(profile) = &view.source else {
                return;
            };
            let vp = view.viewport.get();
            let vh = f32::from(vp.size.height);
            // The panel's top follows the cursor: the panel spans from just below
            // the tab bar down to the window bottom. Clamped to keep the flame
            // region and the panel content usable.
            let panel_bottom = f32::from(vp.origin.y) + vh;
            view.panel_height = clamp_panel_height(
                panel_bottom - f32::from(event.position.y) - TAB_BAR_HEIGHT,
                profile.counters().len(),
                vh,
            );
        }
        Drag::Gutter => {
            let vp = view.viewport.get();
            view.gutter_width = clamp_gutter_width(
                f32::from(event.position.x) - f32::from(vp.origin.x),
                f32::from(vp.size.width),
            );
        }
        Drag::None => {}
    }
    cx.notify();
}

fn end_drag(view: &mut TraceView, _: &MouseUpEvent, cx: &mut Context<TraceView>) {
    view.drag = Drag::None;
    cx.notify();
}

/// A vertical scroll bar painted over the right edge of a scrollable body. Each
/// frame the canvas reads `extents` (content extent, visible extent, scroll
/// offset) to place the thumb, paints it, stashes the geometry in `bar` for
/// hit-testing, and re-installs the drag handlers while `dragging`.
fn scrollbar_canvas(
    bar: Rc<Cell<Option<Scrollbar>>>,
    entity: Entity<TraceView>,
    dragging: bool,
    track: Rgba,
    thumb: Rgba,
    extents: impl Fn(Bounds<Pixels>) -> (f32, f32, f32) + 'static,
) -> impl IntoElement {
    canvas(
        move |_, _, _| {},
        move |bounds, _, window: &mut Window, _| {
            let (content, visible, offset) = extents(bounds);
            let layout = Scrollbar::new(
                Axis::Vertical,
                Bounds::new(
                    point(bounds.origin.x + bounds.size.width - px(BAR_WIDTH), bounds.origin.y),
                    size(px(BAR_WIDTH), bounds.size.height),
                ),
                content,
                visible,
                offset,
                DEFAULT_MIN_THUMB,
            );
            if let Some(painted) = layout {
                painted.paint(track, thumb, window);
            }
            bar.set(layout);
            if dragging {
                track_drag(window, &entity, on_drag, end_drag);
            }
        },
    )
    .absolute()
    .inset_0()
}

/// The scroll offset a thumb at `pos` (grabbed at `grab`) maps to, or `None`
/// when the bar is not currently painted.
fn thumb_offset(bar: &Cell<Option<Scrollbar>>, pos: Point<Pixels>, grab: f32) -> Option<f32> {
    Some(bar.get()?.offset_at(pos, grab))
}

/// Scrolls a list `handle` so a downward thumb `offset` maps to a matching
/// negative vertical scroll.
fn set_list_thumb(handle: &ScrollHandle, offset: f32) {
    let mut o = handle.offset();
    o.y = px(-offset);
    handle.set_offset(o);
}

/// Maps the flame chart's lane vertical thumb drag back to a scroll offset.
fn scroll_thumb_to(view: &mut TraceView, pos: Point<Pixels>, grab: f32) {
    if let Some(offset) = thumb_offset(&view.vbar, pos, grab) {
        view.scroll_y.set(offset);
    }
}

/// Maps a tracks-menu thumb drag back to the list's scroll offset.
fn menu_thumb_to(view: &mut TraceView, pos: Point<Pixels>, grab: f32) {
    if let Some(offset) = thumb_offset(&view.menu_bar, pos, grab) {
        set_list_thumb(&view.menu_scroll, offset);
    }
}

/// Maps a horizontal thumb drag back to a pan start time, then clamps the view.
fn pan_thumb_to(view: &mut TraceView, pos: Point<Pixels>, grab: f32) {
    if view.lane_layout().is_none() {
        return;
    }
    let Source::Loaded(profile) = &view.source else {
        return;
    };
    let span = profile.span();
    let regions = view.regions();
    let Some(offset) = thumb_offset(&view.hbar, pos, grab) else {
        return;
    };
    let mut v = view.view.get();
    // The bar's offset is measured from the span start less the reachable margin,
    // so map it back through the same shift before clamping.
    let margin_us = EDGE_MARGIN_PX / v.px_per_us as f32;
    v.start_us = (span.start_us as f32 - margin_us) as f64 + offset as f64;
    v.clamp_to(span, regions.time_w, EDGE_MARGIN_PX);
    view.view.set(v);
}

/// Picks the lowest of `rows` whose last occupied right-extent leaves room for a
/// label spanning `[left, right]`, adding a new row when none is clear, and
/// records the label's extent in the chosen row. Keeps clustered labels visible
/// by staggering them down across as many rows as the cluster needs.
/// The left x of a `label_w`-wide label placed `gap` right of `anchor`, flipped
/// to `gap` left of it when the right placement would spill past `right_limit`.
pub(crate) fn label_x(anchor: f32, label_w: f32, gap: f32, right_limit: f32) -> f32 {
    if anchor + gap + label_w > right_limit {
        anchor - gap - label_w
    } else {
        anchor + gap
    }
}

fn assign_marker_row(rows: &mut Vec<f32>, left: f32, right: f32) -> usize {
    let row = (0..rows.len())
        .find(|&r| left >= rows[r])
        .unwrap_or_else(|| {
            rows.push(f32::MIN);
            rows.len() - 1
        });
    rows[row] = right;
    row
}

/// Draws each build-phase boundary as a vertical line spanning both sections
/// with its name labeled near the top, confined to the time axis and skipping
/// any off-screen. A label that would run past the right edge is flipped to the
/// left of its line; clustered labels stagger across a few stacked rows so none
/// is dropped.
fn paint_markers(
    profile: &Profile,
    tv: TimeView,
    regions: Regions,
    show_counters: bool,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let top = regions.flame_top;
    let height = regions.flame_bottom - regions.flame_top;
    let font = bviz_ui_assets::text_metrics(window).font;
    let fs = px(MARKER_LABEL_SIZE);
    let row_h = MARKER_LABEL_SIZE + 2.0;
    // Each row's last occupied right-extent, growing as a cluster needs more rows,
    // so a label drops to the lowest row that clears the previous label there.
    let mut rows: Vec<f32> = Vec::new();
    for marker in profile.phase_markers() {
        let x = tv.x_of(marker.start_us);
        if x < 0.0 || x > regions.time_w {
            continue;
        }
        let lx = regions.content_left + x;
        window.paint_quad(fill(
            Bounds::new(point(px(lx), px(top)), size(px(1.), px(height))),
            palette.marker,
        ));
        // Continue the marker line through the counter panel, which shares the
        // time axis, so a phase boundary reads across both.
        if show_counters {
            window.paint_quad(fill(
                Bounds::new(
                    point(px(lx), px(regions.panel_top)),
                    size(px(1.), px(regions.panel_h())),
                ),
                palette.marker,
            ));
        }
        let line = bviz_ui_assets::shape_line(
            window,
            profile.str(marker.name).to_string(),
            font.clone(),
            fs,
            palette.marker.into(),
        );
        let label_w = f32::from(line.width());
        let left = label_x(lx, label_w, SLICE_TEXT_PAD, regions.time_right);
        let row = assign_marker_row(&mut rows, left, left + label_w + SLICE_TEXT_PAD);
        let _ = line.paint(
            point(px(left), px(top + 2.0 + row as f32 * row_h)),
            fs,
            TextAlign::Left,
            None,
            window,
            cx,
        );
    }
}

/// Tick positions (viewport x) and labels across the visible span, spaced near
/// `RULER_TARGET_TICK_PX` at a 1/2/5 step.
fn ruler_ticks(tv: TimeView, width: f32) -> Vec<(f32, String)> {
    if tv.px_per_us <= 0.0 || width <= 0.0 {
        return Vec::new();
    }
    let step = nice_step(RULER_TARGET_TICK_PX / tv.px_per_us);
    let end_us = tv.start_us + width as f64 / tv.px_per_us;
    let mut t = (tv.start_us / step).ceil() * step;
    let mut ticks = Vec::new();
    while t <= end_us {
        ticks.push((
            ((t - tv.start_us) * tv.px_per_us) as f32,
            format_time(t, step),
        ));
        t += step;
    }
    ticks
}

/// Drops a faint vertical line from each ruler tick down through both sections,
/// confined to the content column.
fn paint_gridlines(
    ticks: &[(f32, String)],
    content_left: f32,
    top: f32,
    bottom: f32,
    palette: &Palette,
    window: &mut Window,
) {
    let height = px(bottom - top);
    for (x, _) in ticks {
        let rect = Bounds::new(point(px(content_left + *x), px(top)), size(px(1.), height));
        window.paint_quad(fill(rect, palette.gridline));
    }
}

/// Paints the ruler band along the top edge spanning the full width, with a tick
/// mark and label in the content column at each tick.
fn paint_ruler(
    ticks: &[(f32, String)],
    regions: Regions,
    bounds: Bounds<Pixels>,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let band = Bounds::new(bounds.origin, size(bounds.size.width, px(RULER_HEIGHT)));
    window.paint_quad(fill(band, palette.header_band));
    let font = bviz_ui_assets::text_metrics(window).font;
    let fs = px(RULER_HEIGHT - 2. * SLICE_TEXT_PAD);
    let ty = bounds.origin.y + px((RULER_HEIGHT - f32::from(fs)) / 2.);
    for (x, label) in ticks {
        let tx = px(regions.content_left + *x);
        let tick = Bounds::new(
            point(tx, bounds.origin.y + px(RULER_HEIGHT - 4.)),
            size(px(1.), px(4.)),
        );
        window.paint_quad(fill(tick, palette.label));
        let line = bviz_ui_assets::shape_line(
            window,
            label.clone(),
            font.clone(),
            fs,
            palette.label.into(),
        );
        let _ = line.paint(
            point(tx + px(2.), ty),
            fs,
            TextAlign::Left,
            None,
            window,
            cx,
        );
    }
}

/// Paints the flame section's vertical bar and/or the content column's
/// horizontal bar for whichever axis overflows, storing each bar's geometry in
/// its cell for hit-testing; an axis that fits gets no bar and a cleared cell.
/// The vertical bar maps to the flame lanes' pixel scroll; the horizontal to the
/// view's start time within the profile span.
fn paint_scrollbars(
    span: TimeSpan,
    tv: TimeView,
    flame_total: f32,
    regions: Regions,
    scroll_y: f32,
    palette: &Palette,
    vbar: &Rc<Cell<Option<Scrollbar>>>,
    hbar: &Rc<Cell<Option<Scrollbar>>>,
    window: &mut Window,
) {
    let flame_h = regions.flame_h();

    let span_us = (span.end_us - span.start_us) as f32;
    let visible_us = regions.time_w / tv.px_per_us as f32;
    // Pan and the wheel reach EDGE_MARGIN_PX of empty space past each span end,
    // so the bar spans that padded range and its offset is measured from the
    // padded start, letting the thumb reach the same margins.
    let margin_us = EDGE_MARGIN_PX / tv.px_per_us as f32;

    let vlayout = Scrollbar::new(
        Axis::Vertical,
        Bounds::new(
            point(px(regions.content_right - BAR_WIDTH), px(regions.flame_top)),
            size(px(BAR_WIDTH), px(flame_h)),
        ),
        flame_total,
        flame_h,
        scroll_y,
        DEFAULT_MIN_THUMB,
    );
    // The time horizontal track runs the full time axis in the strip below the
    // flame lanes, ending left of the reserved vertical-bar column.
    let hlayout = Scrollbar::new(
        Axis::Horizontal,
        Bounds::new(
            point(px(regions.content_left), px(regions.flame_bottom)),
            size(px(regions.time_w), px(BAR_WIDTH)),
        ),
        span_us + 2.0 * margin_us,
        visible_us,
        tv.start_us as f32 - (span.start_us as f32 - margin_us),
        DEFAULT_MIN_THUMB,
    );

    for bar in [vlayout, hlayout].into_iter().flatten() {
        bar.paint(palette.scroll_track, palette.scroll_thumb, window);
    }
    vbar.set(vlayout);
    hbar.set(hlayout);
}

struct GutterPaint<'a> {
    left: f32,
    width: f32,
    avail: f32,
    font: gpui::Font,
    fs: Pixels,
    palette: &'a Palette,
}

/// Paints the opaque left gutter — a single column spanning the flame chart and
/// the bottom panel — over any content that panned or scrolled under it, then the
/// names left-aligned within it: thread names track the flame lanes' vertical
/// scroll (clipped to their band), and, for the Counters tab, counter names sit
/// at their fitted panel lanes. Other tabs leave the gutter empty.
#[allow(clippy::too_many_arguments)]
fn paint_gutter(
    profile: &Profile,
    layout: &LaneLayout,
    chrome: &GroupChrome,
    regions: Regions,
    gutter_width: f32,
    scroll_y: f32,
    show_counters: bool,
    selected: &[bool],
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let gutter_left = regions.content_left - gutter_width;
    let gutter_bottom = regions.panel_bottom;
    paint_gutter_frame(regions, gutter_left, gutter_width, gutter_bottom, palette, window);

    let font = bviz_ui_assets::text_metrics(window).font;
    let fs = px(GUTTER_LABEL_SIZE);
    let avail = gutter_width - 2.0 * SLICE_TEXT_PAD;
    let g = GutterPaint { left: gutter_left, width: gutter_width, avail, font, fs, palette };

    // Thread and group labels scroll with the lanes, so clip them to the flame
    // region.
    let flame_mask = ContentMask {
        bounds: Bounds::new(
            point(px(gutter_left), px(regions.flame_top)),
            size(px(gutter_width), px(regions.flame_h())),
        ),
    };
    window.with_content_mask(Some(flame_mask), |window| {
        paint_thread_labels(&g, profile, layout, regions, scroll_y, selected, window, cx);
        paint_group_bands(&g, chrome, regions, scroll_y, window, cx);
    });

    if show_counters {
        paint_counter_labels(&g, profile, regions, window, cx);
    }
}

/// Fills the gutter's opaque background and draws a divider at its right edge,
/// under the drag handle, spanning the full height so the resizable line reads as
/// one across the flame chart, the tab bar, and the panel.
fn paint_gutter_frame(
    regions: Regions,
    gutter_left: f32,
    gutter_width: f32,
    gutter_bottom: f32,
    palette: &Palette,
    window: &mut Window,
) {
    let strip = Bounds::new(
        point(px(gutter_left), px(regions.flame_top)),
        size(px(gutter_width), px(gutter_bottom - regions.flame_top)),
    );
    window.paint_quad(fill(strip, palette.header_band));
    window.paint_quad(fill(
        Bounds::new(
            point(px(regions.content_left - 1.0), px(regions.flame_top)),
            size(px(1.0), px(gutter_bottom - regions.flame_top)),
        ),
        palette.divider,
    ));
}

/// Shapes `name` truncated to `avail` and paints it left-aligned at the gutter's
/// text inset, vertically offset to `ty`.
fn paint_gutter_label(
    g: &GutterPaint,
    name: &str,
    ty: f32,
    window: &mut Window,
    cx: &mut App,
) {
    let gutter_left = g.left;
    let avail = g.avail;
    let font = &g.font;
    let fs = g.fs;
    let palette = g.palette;
    let line = bviz_ui_components::shape_truncated(name, avail, font.clone(), fs, palette.label.into(), window);
    let _ = line.paint(
        point(px(gutter_left + SLICE_TEXT_PAD), px(ty)),
        fs,
        TextAlign::Left,
        None,
        window,
        cx,
    );
}

/// Paints each thread's name at its flame lane, scrolled with the lanes and
/// centered against the lane's full height. A selected lane's row carries a faint
/// accent tint and a leading accent bar, so which tracks feed the flame graph
/// reads at a glance.
#[allow(clippy::too_many_arguments)]
fn paint_thread_labels(
    g: &GutterPaint,
    profile: &Profile,
    layout: &LaneLayout,
    regions: Regions,
    scroll_y: f32,
    selected: &[bool],
    window: &mut Window,
    cx: &mut App,
) {
    let gutter_left = g.left;
    let gutter_width = g.width;
    let palette = g.palette;
    let selection_tint = bviz_ui_theme::mix(palette.selection, palette.header_band, 0.8);
    for lane in &layout.lanes {
        let top = regions.flame_top + lane.top - scroll_y;
        if top + lane.height < regions.flame_top || top > regions.flame_bottom {
            continue;
        }
        if selected.get(lane.track).copied().unwrap_or(false) {
            window.paint_quad(fill(
                Bounds::new(point(px(gutter_left), px(top)), size(px(gutter_width), px(lane.height))),
                selection_tint,
            ));
            window.paint_quad(fill(
                Bounds::new(point(px(gutter_left), px(top)), size(px(2.0), px(lane.height))),
                palette.selection,
            ));
        }
        let ty = top + (lane.height - GUTTER_LABEL_SIZE) / 2.0;
        paint_gutter_label(g, profile.str(profile.threads()[lane.track].name), ty, window, cx);
    }
}

/// Paints each group header band's chevron, class name, and shown/total lane
/// count; a capped, expanded group shows a caret-double icon at the gutter's right
/// edge — down to show every lane, up to show the busiest.
fn paint_group_bands(
    g: &GutterPaint,
    chrome: &GroupChrome,
    regions: Regions,
    scroll_y: f32,
    window: &mut Window,
    cx: &mut App,
) {
    let gutter_left = g.left;
    let gutter_width = g.width;
    let avail = g.avail;
    let font = &g.font;
    let fs = g.fs;
    let palette = g.palette;
    let strong = palette.label.into();
    for band in &chrome.bands {
        let top = regions.flame_top + band.top - scroll_y;
        if top + GROUP_BAND_HEIGHT < regions.flame_top || top > regions.flame_bottom {
            continue;
        }
        let show_icon = band.capped && !band.collapsed;
        let chevron = if band.collapsed { "▸" } else { "▾" };
        let text = if band.collapsed {
            format!("{chevron} {}  {}", group_label(band.class), band.total)
        } else {
            format!("{chevron} {}  {}/{}", group_label(band.class), band.shown, band.total)
        };
        let ty = top + (GROUP_BAND_HEIGHT - GUTTER_LABEL_SIZE) / 2.0;
        // Keep the label clear of the icon when one is drawn.
        let label_avail = if show_icon { avail - GROUP_ICON - SLICE_TEXT_PAD } else { avail };
        let line = bviz_ui_components::shape_truncated(&text, label_avail, font.clone(), fs, strong, window);
        let _ = line.paint(
            point(px(gutter_left + SLICE_TEXT_PAD), px(ty)),
            fs,
            TextAlign::Left,
            None,
            window,
            cx,
        );
        if show_icon {
            let name = if band.show_all { "caret-double-up" } else { "caret-double-down" };
            let ix = gutter_left + gutter_width - GROUP_ICON - SLICE_TEXT_PAD;
            let iy = top + (GROUP_BAND_HEIGHT - GROUP_ICON) / 2.0;
            let _ = window.paint_svg(
                Bounds::new(point(px(ix), px(iy)), size(px(GROUP_ICON), px(GROUP_ICON))),
                SharedString::from(format!("assets/regular/{name}.svg")),
                None,
                TransformationMatrix::default(),
                palette.marker.into(),
                cx,
            );
        }
    }
}

/// Paints each counter's name at its panel lane's chart baseline, so it associates
/// with low values rather than floating at the top.
fn paint_counter_labels(
    g: &GutterPaint,
    profile: &Profile,
    regions: Regions,
    window: &mut Window,
    cx: &mut App,
) {
    for gl in graph_lanes(profile.counters().len(), regions.panel_top, regions.panel_h()) {
        let lane_bottom = gl.top + METRICS.header_height + gl.chart_h;
        let ty = lane_bottom - GUTTER_LABEL_SIZE - SLICE_TEXT_PAD;
        paint_gutter_label(g, profile.str(profile.counters()[gl.lane].name), ty, window, cx);
    }
}

/// Paints a slice quad and, if wide enough, its truncated name.
#[allow(clippy::too_many_arguments)]
fn paint_slice(
    profile: &Profile,
    slice: &Slice,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    content_left: f32,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let rect = Bounds::new(point(px(content_left + x), px(y)), size(px(w), px(h)));
    let fill = theme::span_role_color(slice.role, palette.background);
    bviz_ui_components::paint_labeled_box(
        rect,
        fill,
        profile.str(slice.name),
        bviz_ui_theme::label_on(fill),
        bviz_ui_components::LabelBoxStyle {
            background: palette.background,
            seam_min_px: SLICE_SEAM_MIN_PX,
            seam_mix: SLICE_SEAM_MIX,
            label_min_px: LABEL_MIN_PX,
            text_pad: SLICE_TEXT_PAD,
        },
        window,
        cx,
    );
}

/// Measures `lines`, sizes a padded box, clamps it inside `bounds`, and paints
/// the panel background, border, and each line near `at`.
fn paint_tooltip_box(
    lines: &[String],
    at: Point<Pixels>,
    bounds: Bounds<Pixels>,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let font = bviz_ui_assets::text_metrics(window).font;
    let fs = px(TOOLTIP_TEXT_SIZE);
    let shaped: Vec<_> = lines
        .iter()
        .map(|line| {
            bviz_ui_assets::shape_line(window, line.clone(), font.clone(), fs, palette.label.into())
        })
        .collect();
    let text_w = shaped
        .iter()
        .map(|l| f32::from(l.width()))
        .fold(0.0_f32, f32::max);
    let line_h = TOOLTIP_TEXT_SIZE + TOOLTIP_LINE_GAP;
    let box_w = text_w + 2.0 * TOOLTIP_PAD;
    let box_h = shaped.len() as f32 * line_h + 2.0 * TOOLTIP_PAD;

    let left = f32::from(bounds.origin.x);
    let top = f32::from(bounds.origin.y);
    let right = left + f32::from(bounds.size.width);
    let bottom = top + f32::from(bounds.size.height);
    let mut x = f32::from(at.x) + TOOLTIP_OFFSET;
    let mut y = f32::from(at.y) + TOOLTIP_OFFSET;
    if x + box_w > right {
        x = (right - box_w).max(left);
    }
    if y + box_h > bottom {
        y = (bottom - box_h).max(top);
    }

    window.paint_quad(quad(
        Bounds::new(point(px(x), px(y)), size(px(box_w), px(box_h))),
        px(4.),
        palette.panel,
        px(1.),
        Hsla::from(palette.border),
        BorderStyle::Solid,
    ));
    for (i, line) in shaped.into_iter().enumerate() {
        let ly = y + TOOLTIP_PAD + i as f32 * line_h;
        let _ = line.paint(
            point(px(x + TOOLTIP_PAD), px(ly)),
            fs,
            TextAlign::Left,
            None,
            window,
            cx,
        );
    }
}

/// Paints a tooltip panel near `at` with the slice's name, its category and
/// duration, and its args, one per line. Kept inside the viewport.
fn paint_tooltip(
    profile: &Profile,
    slice: &Slice,
    detail: &SliceDetail,
    at: Point<Pixels>,
    bounds: Bounds<Pixels>,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let mut lines = vec![
        profile.str(slice.name).to_string(),
        format!("{} · {}", profile.str(detail.category), format_duration(slice.dur_us)),
    ];
    if let Some(target) = profile.opt_str(detail.target) {
        lines.push(format!("target: {target}"));
    }
    if let Some(mnemonic) = profile.opt_str(detail.mnemonic) {
        lines.push(format!("mnemonic: {mnemonic}"));
    }
    if let Some(configuration) = profile.opt_str(detail.configuration) {
        lines.push(format!("configuration: {configuration}"));
    }
    for (key, value) in profile.slice_others(detail) {
        lines.push(format!("{}: {}", profile.str(*key), profile.str(*value)));
    }
    paint_tooltip_box(&lines, at, bounds, palette, window, cx);
}

/// Renders a Bazel `--profile` trace as a flame-chart timeline.
#[derive(Parser)]
struct Cli {
    /// `bazel build --profile=FILE` output (`.profile.gz` or `.json`).
    #[arg(long, value_name = "FILE")]
    profile: PathBuf,
}

/// Parses `--profile` and opens the trace-viewer window.
pub fn run() {
    bviz_alloc::retain_freed_memory_across_load();
    let path = Cli::parse().profile;
    // Loading from process entry overlaps the decode and parse with the app
    // shell's startup (font-database scan, window and GPU-surface setup); the
    // view joins this thread from its background task.
    let load = std::thread::spawn(move || Profile::from_path(&path).map_err(|e| e.to_string()));
    bviz_ui_shell::run(
        "bazel_viz trace",
        size(px(1280.), px(820.)),
        move |window, cx| cx.new(|cx| TraceView::new(load, window, cx)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_panel_height_bounds_both_sections() {
        // A tall window: clamps to the requested height, untouched.
        assert_eq!(clamp_panel_height(220.0, 3, 800.0), 220.0);
        // The floor: never below n lanes of header band plus minimum chart.
        let min = 3.0 * (METRICS.header_height + MIN_CHART_HEIGHT);
        assert_eq!(clamp_panel_height(0.0, 3, 800.0), min);
        // The flame-region ceiling: leaves FLAME_MIN_HEIGHT for the flame chart,
        // plus the ruler, time bar, and tab bar.
        let vh = 260.0;
        let max = vh - RULER_HEIGHT - BAR_WIDTH - TAB_BAR_HEIGHT - FLAME_MIN_HEIGHT;
        assert_eq!(clamp_panel_height(1000.0, 1, vh), max);
    }

    #[test]
    fn clamp_gutter_width_bounds_to_half_window() {
        // A comfortable request in a wide window is left untouched.
        assert_eq!(clamp_gutter_width(170.0, 1280.0), 170.0);
        // Never narrower than the legibility floor.
        assert_eq!(clamp_gutter_width(10.0, 1280.0), MIN_GUTTER);
        // Never wider than half the window, so the time axis keeps the majority.
        assert_eq!(clamp_gutter_width(900.0, 1000.0), 500.0);
        // In a tiny window the floor wins over the half-window ceiling.
        assert_eq!(clamp_gutter_width(120.0, 100.0), MIN_GUTTER);
    }

    #[test]
    fn graph_lanes_split_height_evenly() {
        let n = 2;
        let graph_top = 100.0;
        let graph_h = 2.0 * METRICS.header_height + 80.0;
        let lanes = graph_lanes(n, graph_top, graph_h);
        assert_eq!(lanes.len(), n);
        // Each chart gets an even share of the height past the header bands.
        assert_eq!(lanes[0].chart_h, 40.0);
        assert_eq!(lanes[1].chart_h, 40.0);
        // Lanes stack from graph_top, header band plus chart tall.
        assert_eq!(lanes[0].top, graph_top);
        assert_eq!(lanes[1].top, graph_top + METRICS.header_height + 40.0);
    }

    #[test]
    fn assign_marker_row_staggers_overlaps() {
        let mut rows: Vec<f32> = Vec::new();
        // The first label takes row 0.
        assert_eq!(assign_marker_row(&mut rows, 0.0, 50.0), 0);
        // Overlapping labels keep adding rows as the cluster needs them.
        assert_eq!(assign_marker_row(&mut rows, 10.0, 60.0), 1);
        assert_eq!(assign_marker_row(&mut rows, 20.0, 70.0), 2);
        assert_eq!(assign_marker_row(&mut rows, 30.0, 80.0), 3);
        assert_eq!(rows.len(), 4);
        // A label clear of row 0's extent reuses it rather than adding a row.
        assert_eq!(assign_marker_row(&mut rows, 60.0, 100.0), 0);
        assert_eq!(rows.len(), 4);
    }

    #[test]
    fn graph_lanes_collapse_to_labels() {
        // At the label-only minimum the charts squish to zero, leaving headers.
        let lanes = graph_lanes(2, 0.0, 2.0 * METRICS.header_height);
        assert_eq!(lanes[0].chart_h, 0.0);
        assert_eq!(lanes[1].chart_h, 0.0);
        assert!(graph_lanes(0, 0.0, 0.0).is_empty());
    }
}
