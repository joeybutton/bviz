//! Loads a Bazel `--profile` trace (`.profile.gz` or `.json`) into
//! `bviz_trace_profile` and opens a window rendering its thread tracks as a
//! flame-chart timeline: time on the x axis, stacked call depth on the y axis,
//! with horizontal zoom/pan and vertical scroll.

#[global_allocator]
static GLOBAL: bviz_alloc::MiMalloc = bviz_alloc::MiMalloc;

use bviz_trace_layout::{
    FlameBox, FlameGraph, FlameMetrics, FlameView, Lane, LaneLayout, LaneMetrics, ScopeAggregate,
    SpanRole, StatRow, TimeView, Viewport, Workload, flame_height, lay_out_flame, lay_out_lanes,
    visible_slices,
};
use bviz_trace_profile::{CounterPoint, LaneClass, Profile, Slice, SliceDetail, TimeSpan};
use bviz_ui_components::{Axis, Scrollbar, TextInput, load_in_background, track_drag};
use bviz_ui_shell::Chrome;
use clap::Parser;
use gpui::{
    AnyElement, Animation, AnimationExt, App, BorderStyle, Bounds, ContentMask, Context, Div, Edges,
    Entity, FocusHandle, Hsla, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PathBuilder, Pixels, Point, Render, Rgba, ScrollDelta, ScrollWheelEvent,
    ScrollHandle, SharedString, Stateful, Subscription, Task, TextAlign, Transformation,
    TransformationMatrix, UniformListScrollHandle,
    Window, canvas, deferred, div, hsla, percentage, point, prelude::*, px, quad, size, uniform_list,
};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

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

/// Alpha of a counter series' filled area.
const COUNTER_AREA_ALPHA: f32 = 0.5;

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
/// Shortest a scroll thumb is drawn, so it stays grabbable when the visible
/// fraction is tiny.
const MIN_THUMB: f32 = 24.0;

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

/// Lightness of a neutral meta fill (the flame graph's synthetic root and any
/// meta-class frame), on a dark vs. a light background.
const FLAME_ROOT_LIGHTNESS_DARK: f32 = 0.30;
const FLAME_ROOT_LIGHTNESS_LIGHT: f32 = 0.78;

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

/// Saturation of a counter series' translucent fill.
const CATEGORY_SATURATION: f32 = 0.5;

/// Hue (fraction of the wheel) per span workload, spread so the classes read as
/// distinct on either theme: the two payload workloads are warm (action orange,
/// starlark green) and the overhead workloads cool (filesystem cyan, remote/cache
/// blue, skyframe indigo, scheduling magenta), with gc red set apart. The Other
/// workload carries no hue — it is drawn as neutral grey.
const HUE_SUBPROCESS: f32 = 0.07;
const HUE_STARLARK: f32 = 0.34;
const HUE_FILESYSTEM: f32 = 0.50;
const HUE_REMOTE_CACHE: f32 = 0.60;
const HUE_SKYFRAME: f32 = 0.70;
const HUE_SCHEDULING: f32 = 0.83;
const HUE_GC: f32 = 0.99;

/// Payload fills are vivid (saturated, mid lightness) so real work draws the eye;
/// overhead fills are muted (desaturated, pulled toward the background) so Bazel
/// machinery recedes. Lightness is chosen per background.
const PAYLOAD_SATURATION: f32 = 0.72;
const PAYLOAD_LIGHTNESS_DARK: f32 = 0.60;
const PAYLOAD_LIGHTNESS_LIGHT: f32 = 0.46;
const OVERHEAD_SATURATION: f32 = 0.30;
const OVERHEAD_LIGHTNESS_DARK: f32 = 0.40;
const OVERHEAD_LIGHTNESS_LIGHT: f32 = 0.72;
/// Side of a legend swatch and the gap between a swatch and its label.
const LEGEND_SWATCH: f32 = 10.0;
const LEGEND_LABEL_GAP: f32 = 4.0;

/// Theme-resolved colors the canvas paints with, owned so the `'static` paint
/// closure can capture them.
#[derive(Clone, Copy)]
struct Palette {
    border: Rgba,
    label: Rgba,
    background: Rgba,
    /// The sticky lane-header band fill, shared by the ruler band.
    header_band: Rgba,
    /// Faint vertical line dropped from each ruler tick through the lanes.
    gridline: Rgba,
    /// Accent for the build-phase boundary lines and their labels.
    marker: Rgba,
    /// Scroll-bar track and thumb fills.
    scroll_track: Rgba,
    scroll_thumb: Rgba,
    /// The section-splitter and gutter-edge divider lines, subtler than the
    /// scroll thumb so they don't dominate a dark background.
    divider: Rgba,
    /// Opaque fill of the hover tooltip panel.
    panel: Rgba,
    /// The hover crosshair line, unobtrusive against the background.
    crosshair: Rgba,
    /// Accent marking a selected track's gutter row.
    selection: Rgba,
}

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

/// A sortable column of the graph-mode stats table.
#[derive(Clone, Copy, PartialEq)]
enum StatsColumn {
    Key,
    Count,
    Total,
    Avg,
    Max,
    Percent,
}

/// One column of the stats table: which value it sorts by, its header label, and
/// its fixed width (`None` for the one flexible column that takes the remainder).
/// The header and every body cell are laid out from this single spec so their
/// widths cannot disagree.
struct StatsColumnSpec {
    column: StatsColumn,
    label: &'static str,
    width: Option<f32>,
}

const STATS_COLUMNS: [StatsColumnSpec; 6] = [
    StatsColumnSpec { column: StatsColumn::Key, label: "Mnemonic", width: None },
    StatsColumnSpec { column: StatsColumn::Count, label: "Count", width: Some(STATS_NUM_COL_WIDTH) },
    StatsColumnSpec { column: StatsColumn::Total, label: "Total", width: Some(STATS_NUM_COL_WIDTH) },
    StatsColumnSpec { column: StatsColumn::Avg, label: "Avg", width: Some(STATS_NUM_COL_WIDTH) },
    StatsColumnSpec { column: StatsColumn::Max, label: "Max", width: Some(STATS_NUM_COL_WIDTH) },
    StatsColumnSpec { column: StatsColumn::Percent, label: "%", width: Some(STATS_NUM_COL_WIDTH) },
];

/// The sort the stats table opens with. Every aggregate ships this order
/// precomputed ([`ScopeAggregate::default_order`]), so serving it costs no sort.
const DEFAULT_STATS_SORT: (StatsColumn, bool) = (StatsColumn::Total, true);

/// The stats rows in sorted display order, as indices into `aggregates.stats`.
/// Holds its own `Arc<ScopeAggregate>` so the table renders consistent rows even
/// mid-reswap; the percent denominator is the aggregate's `grand_total`. Only the
/// visible rows are formatted, lazily during paint, so no per-row strings are
/// retained.
struct StatsOrder {
    aggregates: Arc<ScopeAggregate>,
    /// Row order for a non-default sort, built off the UI thread: sorting ~2M
    /// rows costs far more than a frame budget. `None` for the default sort,
    /// whose order ships precomputed inside `aggregates`.
    sorted: Option<Vec<u32>>,
}

impl StatsOrder {
    fn order(&self) -> &[u32] {
        self.sorted.as_deref().unwrap_or(&self.aggregates.default_order)
    }
}

/// The flame graph's laid-out boxes together with the inputs that determine
/// them, so a paint reuses them unless one changed. The retained
/// `ScopeAggregate` both keeps the graph alive for the boxes' name handles and
/// serves as a cache key: swapping in another selection's aggregate changes the
/// `Arc`, so `Arc::ptr_eq` detects a stale layout. The layout is viewport-
/// relative — it depends on the zoomed width `content_w`, the horizontal scroll
/// `scroll_x`, and the viewport width `view_w` — so all three are keys. The
/// vertical `scroll_y` is not: it only offsets a box's y at paint.
struct FlameBoxCache {
    aggregates: Arc<ScopeAggregate>,
    content_w: f64,
    scroll_x: f64,
    view_w: f32,
    boxes: Rc<Vec<FlameBox>>,
}

/// One stats-table cell, header or body, laid out per its column spec. A fixed
/// column reserves its width and never shrinks (a bare `w(px)` is only a flex
/// basis and gets shrunk unevenly between header and body when a row overflows,
/// drifting the columns out of alignment) and right-aligns its value; the
/// flexible key column takes the remainder and ellipsizes a long name. Only the
/// key column ellipsizes: a right-aligned numeric cell with `text_ellipsis`
/// truncates values that already fit, so its fixed width is just sized to hold
/// the longest value instead.
fn stats_cell(spec: &StatsColumnSpec, text: impl Into<SharedString>) -> Div {
    let cell = div()
        .flex()
        .flex_row()
        .items_center()
        .h_full()
        .px(px(STATS_CELL_PAD))
        .overflow_hidden()
        .whitespace_nowrap()
        .child(text.into());
    match spec.width {
        Some(w) => cell.w(px(w)).flex_shrink_0().justify_end(),
        None => cell.flex_1().min_w_0().text_ellipsis(),
    }
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

/// A flame-graph box under the cursor in graph mode: the laid-out box and where
/// the cursor was, so a tooltip can be drawn for it.
#[derive(Clone)]
struct FlameHover {
    boxed: FlameBox,
    at: Point<Pixels>,
}

/// The window content split into the flame chart region on top, the tab bar,
/// and the bottom panel, plus the gutter/time-axis columns shared across them.
/// Derived once per frame from the viewport bounds, the counter count, the panel
/// height, the gutter width, and the flame lanes' total height (to decide whether
/// a vertical bar column must be reserved). Both paint and the mouse handlers
/// compute the same regions so hit-testing matches what was drawn.
#[derive(Clone, Copy)]
struct Regions {
    /// Left edge of the time content, past the gutter.
    content_left: f32,
    /// Right edge of the content column (where the vertical bar/gutter end).
    content_right: f32,
    /// Right edge of the time axis, left of the reserved vertical-bar column.
    /// Everything mapped to time ends here so nothing underlaps the bar.
    time_right: f32,
    /// Width of the time axis: the content column less the reserved vertical-bar
    /// column.
    time_w: f32,
    /// Top of the flame lanes (ruler bottom).
    flame_top: f32,
    /// Bottom of the scrollable flame lanes; the time horizontal bar sits below.
    flame_bottom: f32,
    /// Top of the tab bar (below the flame region and its horizontal bar).
    tab_top: f32,
    /// Bottom of the tab bar, which is the bottom panel's top.
    panel_top: f32,
    /// Bottom of the bottom panel (window bottom).
    panel_bottom: f32,
    /// Whether the profile has counters (so the Counters tab has content).
    has_graph: bool,
}

impl Regions {
    /// Splits `bounds` given the counter count, the panel's stored height, the
    /// gutter width, and the flame lanes' total height (to decide whether a
    /// vertical bar shows and so a column must be reserved for it).
    fn new(
        bounds: Bounds<Pixels>,
        n_counters: usize,
        panel_height: f32,
        gutter_width: f32,
        flame_total: f32,
    ) -> Self {
        let ox = f32::from(bounds.origin.x);
        let oy = f32::from(bounds.origin.y);
        let w = f32::from(bounds.size.width);
        let vh = f32::from(bounds.size.height);
        let panel_bottom = oy + vh;
        let panel_h = clamp_panel_height(panel_height, n_counters, vh);
        let panel_top = (panel_bottom - panel_h).max(oy);
        let tab_top = (panel_top - TAB_BAR_HEIGHT).max(oy);
        // The flame region: ruler at the top, scrollable lanes, then a strip for
        // the time horizontal bar always reserved above the tab bar.
        let flame_top = oy + RULER_HEIGHT;
        let flame_bottom = (tab_top - BAR_WIDTH).max(flame_top);
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
            BAR_WIDTH
        } else {
            0.0
        };
        Regions {
            content_left,
            content_right,
            time_right: content_right - vbar_reserve,
            time_w: (content_w - vbar_reserve).max(0.0),
            flame_top,
            flame_bottom,
            tab_top,
            panel_top,
            panel_bottom,
            has_graph: n_counters > 0,
        }
    }

    /// Height the flame lanes scroll within.
    fn flame_h(&self) -> f32 {
        (self.flame_bottom - self.flame_top).max(0.0)
    }

    /// Height of the bottom panel below the tab bar.
    fn panel_h(&self) -> f32 {
        (self.panel_bottom - self.panel_top).max(0.0)
    }
}

/// The bottom panel's height bounded to leave the flame region at least
/// `FLAME_MIN_HEIGHT` and to keep the panel tall enough for its content — every
/// counter lane at least a header band plus `MIN_CHART_HEIGHT` of chart, or at
/// least a stats header plus one row — so no tab collapses to nothing.
fn clamp_panel_height(panel_height: f32, n_counters: usize, vh: f32) -> f32 {
    let counter_floor = n_counters as f32 * (METRICS.header_height + MIN_CHART_HEIGHT);
    let min = counter_floor.max(STATS_HEADER_HEIGHT + STATS_ROW_HEIGHT);
    let max = (vh - RULER_HEIGHT - BAR_WIDTH - TAB_BAR_HEIGHT - FLAME_MIN_HEIGHT).max(min);
    panel_height.clamp(min, max)
}

/// The gutter width bounded to stay legible yet never claim more than half the
/// window, so the time axis keeps the larger share.
fn clamp_gutter_width(gutter_width: f32, window_w: f32) -> f32 {
    let max = (window_w * 0.5).max(MIN_GUTTER);
    gutter_width.clamp(MIN_GUTTER, max)
}

/// Total height the flame section's thread lanes occupy, ignoring the layout's
/// counter lanes (the graph section places those itself).
fn flame_total_height(layout: &LaneLayout) -> f32 {
    layout.lanes.last().map(|l| l.top + l.height).unwrap_or(0.0)
}

/// The full layout restricted to the tracks `visible` admits, the kept lanes
/// Lane classes in the order their groups stack in the flame chart: the critical
/// path and main thread first, then the build phases (loading, analysis,
/// execution), then gc and the leftovers.
const GROUP_ORDER: [LaneClass; 7] = [
    LaneClass::CriticalPath,
    LaneClass::Main,
    LaneClass::RepoFetch,
    LaneClass::Analysis,
    LaneClass::Execution,
    LaneClass::Gc,
    LaneClass::Other,
];

/// Height of a group header band and of a group's "show more" row.
const GROUP_BAND_HEIGHT: f32 = 18.0;
/// Side of the show-all icon at a group band's gutter edge.
const GROUP_ICON: f32 = 12.0;
/// Lanes shown per group before the rest fold behind the band's show-all toggle.
const TOP_N_PER_GROUP: usize = 12;

/// The initial collapse state: the loading group starts collapsed, the rest open.
fn default_collapsed_groups() -> [bool; GROUP_ORDER.len()] {
    let mut collapsed = [false; GROUP_ORDER.len()];
    if let Some(i) = GROUP_ORDER.iter().position(|&g| g == LaneClass::RepoFetch) {
        collapsed[i] = true;
    }
    collapsed
}

/// The readable name of a lane-class group's header band.
fn group_label(class: LaneClass) -> &'static str {
    match class {
        LaneClass::CriticalPath => "Critical path",
        LaneClass::Main => "Main",
        LaneClass::RepoFetch => "Loading",
        LaneClass::Analysis => "Analysis",
        LaneClass::Execution => "Execution",
        LaneClass::Gc => "Garbage collection",
        LaneClass::Other => "Other",
    }
}

/// A collapsible group header in the flame chart, one per non-empty lane class,
/// placed at `top` above its group's lanes. `shown`/`total` count the group's
/// lanes drawn versus visible; a group with more than [`TOP_N_PER_GROUP`] visible
/// lanes is `capped`, showing only the busiest until `show_all`.
#[derive(Debug, Clone, Copy)]
struct GroupBand {
    group: usize,
    class: LaneClass,
    top: f32,
    collapsed: bool,
    shown: usize,
    total: usize,
    capped: bool,
    show_all: bool,
}

/// The group headers overlaid on a grouped lane layout.
#[derive(Debug, Clone, Default)]
struct GroupChrome {
    bands: Vec<GroupBand>,
}

/// What a press on a group header band hit: its gutter half (toggles the group's
/// collapse) or, for a capped expanded group, its time half (toggles whether the
/// group shows every lane or only the busiest).
#[derive(Debug, Clone, Copy)]
enum GroupHit {
    Collapse(usize),
    ShowAll(usize),
}

/// The lane layout the flame chart draws: visible tracks ordered into lane-class
/// groups, each group behind a header band and sorted by descending activity, so
/// the busiest lanes lead and a collapsed or capped group folds its lanes away.
/// Returns the restacked [`LaneLayout`] (its `lanes` tops leave room for the
/// bands) and the [`GroupChrome`] painted over it. A track absent from `visible`
/// defaults to shown.
fn build_grouped_layout(
    profile: &Profile,
    full: &LaneLayout,
    visible: &[bool],
    collapsed: &[bool; GROUP_ORDER.len()],
    show_all: &[bool; GROUP_ORDER.len()],
) -> (LaneLayout, GroupChrome) {
    let threads = profile.threads();
    let activity = |t: usize| profile.slices(&threads[t]).len();
    let mut lanes = Vec::new();
    let mut chrome = GroupChrome::default();
    let mut top = 0.0;
    for (group, &class) in GROUP_ORDER.iter().enumerate() {
        let mut tracks: Vec<usize> = (0..threads.len())
            .filter(|&t| {
                threads[t].lane_class == class && visible.get(t).copied().unwrap_or(true)
            })
            .collect();
        if tracks.is_empty() {
            continue;
        }
        let total = tracks.len();
        // Busiest first; tie-break by tid so the order is stable frame to frame.
        tracks.sort_by(|&a, &b| {
            activity(b)
                .cmp(&activity(a))
                .then(threads[a].tid.cmp(&threads[b].tid))
        });
        let band_top = top;
        top += GROUP_BAND_HEIGHT + METRICS.lane_gap;
        let is_collapsed = collapsed[group];
        let capped = total > TOP_N_PER_GROUP;
        let mut shown = 0;
        if !is_collapsed {
            let cap = if show_all[group] {
                tracks.len()
            } else {
                TOP_N_PER_GROUP.min(tracks.len())
            };
            for &t in &tracks[..cap] {
                let lane = full.lanes[t];
                lanes.push(Lane { top, ..lane });
                top += lane.height + METRICS.lane_gap;
            }
            shown = cap;
        }
        chrome.bands.push(GroupBand {
            group,
            class,
            top: band_top,
            collapsed: is_collapsed,
            shown,
            total,
            capped,
            show_all: show_all[group],
        });
    }
    let layout = LaneLayout {
        lanes,
        counters: full.counters.clone(),
        total_height: top,
    };
    (layout, chrome)
}

/// The flame metrics matching the chart's lane row height, so the graph's rows
/// align with the rest of the view.
fn flame_metrics() -> FlameMetrics {
    FlameMetrics {
        row_height: METRICS.row_height,
    }
}

/// The flame graph's current geometry, shared by hit-testing and the
/// scroll/zoom/pan handlers so they map the cursor the same way the paint does:
/// the horizontal view, the full laid-out height, and the flame section's
/// content rect after the scroll-bar reserves.
#[derive(Clone, Copy)]
struct FlameLayout {
    view: FlameView,
    total_h: f32,
    left: f32,
    top: f32,
    width: f32,
    height: f32,
}

/// The flame graph's content rectangle: it spans the full body with no gutter
/// and no time axis, less a reserved vertical-bar column (when the tree's rows
/// overflow the visible height) and a reserved horizontal-bar strip (when the
/// zoomed width overflows the visible width). Both reserves may apply at once,
/// leaving the bottom-right corner to neither bar. Returned as
/// `(left, top, width, height)`.
fn flame_graph_rect(
    bounds: Bounds<Pixels>,
    flame_total: f32,
    flame_view: FlameView,
) -> (f32, f32, f32, f32) {
    let left = f32::from(bounds.origin.x);
    let top = f32::from(bounds.origin.y);
    let width_full = f32::from(bounds.size.width);
    let height_full = f32::from(bounds.size.height);
    // Zoom > 1 overflows horizontally, so a bottom strip is reserved for the
    // h-bar; that shrinks the height the rows must clear, which is what decides
    // the v-bar reserve. Resolve in that order.
    let hbar_reserve = if flame_view.zoom > 1.0 {
        BAR_WIDTH
    } else {
        0.0
    };
    let height = (height_full - hbar_reserve).max(0.0);
    let vbar_reserve = if flame_total > height {
        BAR_WIDTH
    } else {
        0.0
    };
    let width = (width_full - vbar_reserve).max(0.0);
    (left, top, width, height)
}

/// One fitted counter lane in the panel: its top (window y) and the height of
/// its chart area below the header band.
#[derive(Clone, Copy)]
struct GraphLane {
    lane: usize,
    top: f32,
    chart_h: f32,
}

/// Distributes the graph section's height evenly across its `n` counter lanes:
/// each lane is a header band plus a chart area that shrinks to zero as the
/// section collapses toward the label-only minimum.
fn graph_lanes(n: usize, graph_top: f32, graph_h: f32) -> Vec<GraphLane> {
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
    /// The `selection_version` the ready `flame`/`stats` reflect, if any.
    agg_ready: Option<u64>,
    /// The `selection_version` a background flame/stats build is running for.
    agg_inflight: Option<u64>,
    /// Bumped on every (re)spawn so a superseded build's result is discarded when
    /// it lands after the inputs have moved on.
    agg_gen: u64,
    /// Holds the background flame/stats build task so it isn't cancelled on drop.
    _agg_task: Task<()>,
    /// Which column the stats table sorts by, and whether descending.
    stats_sort: StatsColumn,
    stats_desc: bool,
    /// The stats rows in sorted display order for the active key and sort, built
    /// off the UI thread (sorting ~2M rows would freeze a paint) and swapped in
    /// when ready. The previous order stays shown while a re-sort runs, so a
    /// header click doesn't blank the table.
    stats_order: Rc<RefCell<Option<Rc<StatsOrder>>>>,
    /// The `(selection_version, sort, desc)` the ready `stats_order` reflects.
    stats_order_ready: Option<(u64, StatsColumn, bool)>,
    /// The `(selection_version, sort, desc)` a background sort is running for.
    stats_order_inflight: Option<(u64, StatsColumn, bool)>,
    /// Bumped on every (re)spawn so a superseded sort's result is discarded when
    /// it lands after the inputs have moved on.
    stats_order_gen: u64,
    /// Holds the background sort task so it isn't cancelled on drop.
    _stats_order_task: Task<()>,
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
            |view, result, cx| {
                view.invalidate_aggregates();
                // The lane layouts depend only on the profile, so they must be
                // dropped when a new profile loads.
                view.lanes.borrow_mut().take();
                view.visible_lanes.borrow_mut().take();
                view.flame_view.set(FlameView::FIT);
                view.source = match result {
                    Ok(profile) => Source::Loaded(Arc::new(profile)),
                    Err(err) => Source::Message(err),
                };
                if let Source::Loaded(profile) = &view.source {
                    let profile = profile.clone();
                    view.init_tracks(&profile);
                }
                cx.notify();
            },
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
            agg_ready: None,
            agg_inflight: None,
            agg_gen: 0,
            _agg_task: Task::ready(()),
            stats_sort: DEFAULT_STATS_SORT.0,
            stats_desc: DEFAULT_STATS_SORT.1,
            stats_order: Rc::new(RefCell::new(None)),
            stats_order_ready: None,
            stats_order_inflight: None,
            stats_order_gen: 0,
            _stats_order_task: Task::ready(()),
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
                    window.paint_quad(quad(
                        Bounds::new(
                            point(px(regions.content_left), px(ry)),
                            size(px(regions.time_w), px(1.0)),
                        ),
                        px(0.),
                        palette.gridline,
                        px(0.),
                        Hsla::from(palette.border),
                        BorderStyle::Solid,
                    ));
                }
                // Group header bands read across the full width as section
                // dividers; their gutter labels and show-all icon paint later,
                // over the opaque gutter.
                for band in &chrome.bands {
                    let y = regions.flame_top + band.top - scroll;
                    window.paint_quad(quad(
                        Bounds::new(
                            point(px(regions.content_left), px(y)),
                            size(px(regions.time_w), px(GROUP_BAND_HEIGHT)),
                        ),
                        px(0.),
                        palette.header_band,
                        px(0.),
                        Hsla::from(palette.border),
                        BorderStyle::Solid,
                    ));
                }
                for vs in visible_slices(&profile, layout, &tv, &vp, &METRICS, MIN_SLICE_PX) {
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
                    .child(role_legend(palette))
            }))
    }

    /// The tracks visibility menu in the gutter corner where the ruler meets the
    /// track-label column: a trigger showing how many tracks are visible, and,
    /// when open, a filter field over a checklist of track names. A backdrop
    /// behind the open menu dismisses it on an outside click.
    fn tracks_menu(
        &self,
        palette: Palette,
        colors: &bviz_ui_theme::Colors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (nvis, ntot) = self.visible_count();
        let hover_bg = bviz_ui_theme::mix(colors.container, colors.background, 0.2);
        let trigger = div()
            .id("tracks-menu")
            .size_full()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_2()
            .gap_1()
            .cursor_pointer()
            .text_size(px(GUTTER_LABEL_SIZE))
            .text_color(palette.label)
            .bg(palette.header_band)
            .hover(move |s| s.bg(hover_bg))
            // Keep the press off the chart handlers underneath.
            .on_mouse_down(MouseButton::Left, |_, _, cx: &mut App| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| {
                this.tracks_menu_open = !this.tracks_menu_open;
                cx.notify();
            }))
            .child(div().overflow_hidden().whitespace_nowrap().child(format!("{nvis}/{ntot} tracks")))
            .child(
                bviz_ui_assets::icon("caret-down")
                    .size(px(12.))
                    .text_color(palette.label),
            );
        // An anchor at the corner holds the trigger and, below it, the popover.
        // The popover is an absolute child of this anchor (not a flex sibling of
        // the trigger row), so it opens directly under the trigger and does not
        // shift the trigger's own layout.
        div()
            .absolute()
            .left_0()
            .top_0()
            .w(px(self.gutter_width))
            .h(px(RULER_HEIGHT))
            .child(trigger)
            .children(self.tracks_menu_popover(palette, colors, cx))
    }

    /// The open tracks menu's deferred body: a filter field over the filtered
    /// track checklist, with a full-viewport backdrop behind it that closes the
    /// menu on an outside click. `None` when the menu is closed.
    fn tracks_menu_popover(
        &self,
        palette: Palette,
        colors: &bviz_ui_theme::Colors,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        if !self.tracks_menu_open {
            return None;
        }
        let Source::Loaded(profile) = &self.source else {
            return None;
        };
        let rows = self.menu_rows(profile, palette, colors, cx);

        let menu = self.tracks_menu_chrome(rows, palette, colors, cx);
        let backdrop = self.tracks_menu_backdrop(cx);
        Some(deferred(div().child(backdrop).child(menu)))
    }

    /// Tracks under their lane-class group headers, each group's tracks sorted
    /// busiest first — the same grouping and order the chart lays out.
    fn menu_rows(
        &self,
        profile: &Profile,
        palette: Palette,
        colors: &bviz_ui_theme::Colors,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let selected = colors.selected;
        let hover_bg = bviz_ui_theme::mix(colors.container, colors.background, 0.2);
        let filter = self.track_filter.read(cx).content().to_lowercase();
        let threads = profile.threads();
        let activity = |t: usize| profile.slices(&threads[t]).len();
        let mut rows: Vec<AnyElement> = Vec::new();
        for (group, &class) in GROUP_ORDER.iter().enumerate() {
            let mut tracks: Vec<usize> = (0..threads.len())
                .filter(|&t| threads[t].lane_class == class)
                .filter(|&t| {
                    filter.is_empty()
                        || profile.str(threads[t].name).to_lowercase().contains(&filter)
                })
                .collect();
            if tracks.is_empty() {
                continue;
            }
            tracks.sort_by(|&a, &b| {
                activity(b)
                    .cmp(&activity(a))
                    .then(threads[a].tid.cmp(&threads[b].tid))
            });
            let folded = self.menu_collapsed_groups.get(group).copied().unwrap_or(false);
            rows.push(
                self.group_row(group, class, &tracks, folded, palette, hover_bg, selected, cx)
                    .into_any_element(),
            );
            if folded {
                continue;
            }
            for i in tracks {
                let name = profile.str(threads[i].name);
                let visible = self.visible_tracks.get(i).copied().unwrap_or(true);
                rows.push(
                    self.track_row(i, name, visible, palette, selected, cx).into_any_element(),
                );
            }
        }
        rows
    }

    /// A group header: a chevron and the group's visible/total count, folding the
    /// group's track list on click. Its trailing control toggles the whole
    /// group's visibility without also folding.
    fn group_row(
        &self,
        group: usize,
        class: LaneClass,
        tracks: &[usize],
        folded: bool,
        palette: Palette,
        hover_bg: Rgba,
        selected: Rgba,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let n_visible = tracks
            .iter()
            .filter(|&&t| self.visible_tracks.get(t).copied().unwrap_or(true))
            .count();
        let group_all_visible = n_visible == tracks.len();
        let chevron = if folded { "▸" } else { "▾" };
        div()
            .id(SharedString::from(format!("track-group-{group}")))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .mt_1()
            .px_2()
            .py_1()
            .cursor_pointer()
            .text_color(palette.label)
            .hover(move |s| s.bg(hover_bg))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_menu_group_collapsed(group);
                cx.notify();
            }))
            .child(format!(
                "{chevron} {}  {}/{}",
                group_label(class),
                n_visible,
                tracks.len()
            ))
            .child(
                div()
                    .id(SharedString::from(format!("track-group-vis-{group}")))
                    .px_1()
                    .rounded_sm()
                    .hover(move |s| s.bg(selected))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.set_group_visible(group, !group_all_visible);
                        cx.notify();
                    }))
                    .child(if group_all_visible { "hide" } else { "show" }),
            )
    }

    /// A track row: a visibility checkbox and the track name, toggling the track
    /// on click.
    fn track_row(
        &self,
        i: usize,
        name: &str,
        visible: bool,
        palette: Palette,
        selected: Rgba,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let check = div()
            .size(px(13.))
            .flex_none()
            .rounded_sm()
            .border_1()
            .border_color(palette.label)
            .when(visible, |c| c.bg(selected).border_color(selected));
        div()
            .id(SharedString::from(format!("track-row-{i}")))
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .pl_4()
            .pr_2()
            .py_1()
            .cursor_pointer()
            .rounded_sm()
            .hover(move |s| s.bg(selected))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_track(i);
                cx.notify();
            }))
            .child(check)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(name.to_string()),
            )
    }

    /// The menu container over the built rows: the filter field, a show/hide-all
    /// toggle, and the scrollable track list with its scroll bar. Clicks inside
    /// must not reach the backdrop and close it; a press on the scroll bar grabs
    /// its thumb.
    fn tracks_menu_chrome(
        &self,
        rows: Vec<AnyElement>,
        palette: Palette,
        colors: &bviz_ui_theme::Colors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let (container, text) = (colors.container, colors.text);
        let hover_bg = bviz_ui_theme::mix(colors.container, colors.background, 0.2);
        div()
            .occlude()
            .absolute()
            .left_0()
            .top_0()
            .w(px(TRACKS_MENU_WIDTH))
            .flex()
            .flex_col()
            .bg(container)
            .border_1()
            .border_color(palette.border)
            .rounded_md()
            .shadow_lg()
            .text_size(px(GUTTER_LABEL_SIZE))
            .text_color(text)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    this.press_menu(e.position);
                    cx.stop_propagation();
                }),
            )
            .child(div().p_1().child(self.track_filter.clone()))
            .child({
                let (nvis, ntot) = self.visible_count();
                let all_visible = ntot > 0 && nvis == ntot;
                div()
                    .id("tracks-toggle-all")
                    .flex()
                    .flex_row()
                    .items_center()
                    .mx_1()
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .border_b_1()
                    .border_color(palette.border)
                    .hover(move |s| s.bg(hover_bg))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_all_visible(!all_visible);
                        cx.notify();
                    }))
                    .child(if all_visible { "Hide all" } else { "Show all" })
            })
            .child(self.menu_list(rows, palette, cx))
    }

    /// The scrollable track list with a scroll bar over its right edge; the bar
    /// reads the list's scroll handle to place the thumb and stashes the geometry
    /// for the menu's press hit-test.
    fn menu_list(
        &self,
        rows: Vec<AnyElement>,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let list = div()
            .id("tracks-menu-list")
            .flex()
            .flex_col()
            .w_full()
            .p_1()
            .max_h(px(TRACKS_MENU_MAX_LIST))
            .overflow_y_scroll()
            .track_scroll(&self.menu_scroll)
            .children(rows);
        let menu_scroll = self.menu_scroll.clone();
        let menu_bar = self.menu_bar.clone();
        let entity = cx.entity();
        let dragging = matches!(self.drag, Drag::MenuThumb(_));
        let (track, thumb) = (palette.scroll_track, palette.scroll_thumb);
        let bar = canvas(
            move |_, _, _| {},
            move |bounds, _, window: &mut Window, _| {
                let visible = f32::from(bounds.size.height);
                let content = visible + f32::from(menu_scroll.max_offset().y);
                let offset = -f32::from(menu_scroll.offset().y);
                let layout = Scrollbar::new(
                    Axis::Vertical,
                    Bounds::new(
                        point(
                            bounds.origin.x + bounds.size.width - px(BAR_WIDTH),
                            bounds.origin.y,
                        ),
                        size(px(BAR_WIDTH), bounds.size.height),
                    ),
                    content,
                    visible,
                    offset,
                    MIN_THUMB,
                );
                if let Some(bar) = layout {
                    bar.paint(track, thumb, window);
                }
                menu_bar.set(layout);
                if dragging {
                    track_drag(window, &entity, on_drag, end_drag);
                }
            },
        );
        div()
            .relative()
            .w_full()
            .flex()
            .min_h_0()
            .child(list)
            .child(bar.absolute().inset_0())
    }

    /// A backdrop the size of the viewport, behind the menu, closing it on an
    /// outside click.
    fn tracks_menu_backdrop(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let viewport = self.viewport.get();
        div()
            .absolute()
            .left_0()
            .top_0()
            .w(viewport.size.width)
            .h(viewport.size.height)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.tracks_menu_open = false;
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
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
                None => loading_indicator("Building flame graph…", colors).into_any_element(),
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

    /// The flame-graph panel: its own canvas painting the merged boxes and the
    /// panel's horizontal/vertical scroll bars, with the panel's pan/scroll/hover
    /// handlers. The canvas closures are `'static`, so they capture owned
    /// snapshots rather than borrowing the view.
    fn flame_graph_panel(
        &self,
        aggregates: Arc<ScopeAggregate>,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let flame_scroll_y = self.flame_scroll_y.clone();
        let flame_view = self.flame_view.clone();
        let flame_bounds = self.flame_bounds.clone();
        let vbar = self.flame_vbar.clone();
        let hbar = self.flame_hbar.clone();
        let flame_boxes = self.flame_boxes.clone();
        let entity = cx.entity();
        let prepaint = {
            let flame_bounds = flame_bounds.clone();
            move |bounds: Bounds<Pixels>, _: &mut Window, _: &mut App| {
                flame_bounds.set(bounds);
            }
        };
        let canvas = canvas(prepaint, move |bounds, _, window, cx| {
            let (flame_hover, dragging) = {
                let this = entity.read(cx);
                (this.flame_hover.clone(), !matches!(this.drag, Drag::None))
            };
            paint_flame_graph(
                &aggregates,
                &flame_boxes,
                flame_scroll_y.get(),
                flame_view.get(),
                &palette,
                &vbar,
                &hbar,
                flame_hover,
                bounds,
                window,
                cx,
            );
            if dragging {
                track_drag(window, &entity, on_drag, end_drag);
            }
        })
        .absolute()
        .left_0()
        .top_0()
        .size_full();
        // While a rebuild for a new selection runs off the thread, the prior
        // graph stays shown and interactive; a spinner pill signals the update,
        // since aggregating a large selection can take a moment.
        let spinner = self.agg_inflight.is_some().then(|| {
            div()
                .absolute()
                .top_2()
                .left_0()
                .right_0()
                .flex()
                .flex_row()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(palette.panel)
                        .border_1()
                        .border_color(palette.border)
                        .text_color(palette.label)
                        .text_size(px(STATS_TEXT_SIZE))
                        .child(spinner_icon("flame-rebuild", px(14.), palette.label.into()))
                        .child("Updating…"),
                )
        });
        self.flame_panel_handlers(
            div()
                .id("flame-graph-panel")
                .relative()
                .size_full()
                .overflow_hidden()
                .child(canvas)
                .children(spinner),
            cx,
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

    /// The pixel ceiling for the flame-graph panel's vertical scroll: the tree's
    /// height past the panel's on-screen height (which excludes any reserved
    /// horizontal-bar strip, so the last row clears that bar).
    fn flame_max_scroll(&self) -> f32 {
        self.flame_layout()
            .map(|fl| (fl.total_h - fl.height).max(0.0))
            .unwrap_or(0.0)
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
        if self.agg_ready == Some(key) {
            // The shown value is already current; cancel a build for a key the
            // inputs have since left so its late result can't clobber it.
            if self.agg_inflight.take().is_some() {
                self.agg_gen += 1;
                self._agg_task = Task::ready(());
            }
            return;
        }
        if self.agg_inflight == Some(key) {
            return;
        }
        self.agg_inflight = Some(key);
        self.agg_gen += 1;
        let generation = self.agg_gen;
        let profile = profile.clone();
        let selected = self.selected_tracks.clone();
        self._agg_task = load_in_background(
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
                if view.agg_gen != generation {
                    return;
                }
                *view.aggregates.borrow_mut() = Some(Arc::new(aggregates));
                view.flame_boxes.borrow_mut().take();
                view.agg_ready = Some(key);
                view.agg_inflight = None;
                // The sorted order is keyed on the new aggregates; let
                // `ensure_stats_order` re-derive it (the old order keeps
                // showing until it lands).
                view.stats_order_ready = None;
            },
        );
    }

    /// The current flame-graph panel geometry, or `None` until the graph exists.
    fn flame_layout(&self) -> Option<FlameLayout> {
        let aggregates = self.aggregates.borrow().clone()?;
        let view = self.flame_view.get();
        let total_h = flame_height(&aggregates.flame, &flame_metrics());
        let (left, top, width, height) = flame_graph_rect(self.flame_bounds.get(), total_h, view);
        Some(FlameLayout {
            view,
            total_h,
            left,
            top,
            width,
            height,
        })
    }

    /// Drops the cached flame graph and stats rows and abandons any in-flight
    /// build, so the next [`Self::ensure_aggregates`] rebuilds from scratch.
    /// Called when a new profile loads — unlike a mere selection change, the old
    /// graph is no longer a valid thing to keep showing.
    fn invalidate_aggregates(&mut self) {
        self.aggregates.borrow_mut().take();
        self.flame_boxes.borrow_mut().take();
        self.stats_order.borrow_mut().take();
        self.agg_ready = None;
        self.agg_inflight = None;
        self.stats_order_ready = None;
        self.stats_order_inflight = None;
        // Supersede any build/sort still running for the previous profile.
        self.agg_gen += 1;
        self.stats_order_gen += 1;
    }

    /// Sets the stats sort to `column`, toggling the direction when it is already
    /// the active column.
    fn sort_by(&mut self, column: StatsColumn) {
        if self.stats_sort == column {
            self.stats_desc = !self.stats_desc;
        } else {
            self.stats_sort = column;
            self.stats_desc = true;
        }
    }

    /// Makes the current `(scope, sort)`'s row order the shown one. The default
    /// sort's order ships precomputed inside the aggregate, so it is served
    /// with no sort at all; any other sort runs on the background executor
    /// (sorting ~2M rows is far too costly for the UI thread) and only the row
    /// indices come back, the previous order staying shown until the new one
    /// lands. An in-flight sort whose key the inputs have moved past is
    /// abandoned. The visible rows are formatted lazily at paint. Idempotent —
    /// safe to call each frame.
    fn ensure_stats_order(&mut self, cx: &mut Context<Self>) {
        let Some(aggregates) = self.aggregates() else {
            return;
        };
        let Some(version) = self.agg_ready else {
            return;
        };
        let key = (version, self.stats_sort, self.stats_desc);
        if self.stats_order_ready == Some(key) {
            // The shown order is already current; cancel a sort for a key the
            // inputs have since left so its late result can't clobber it.
            if self.stats_order_inflight.take().is_some() {
                self.stats_order_gen += 1;
                self._stats_order_task = Task::ready(());
            }
            return;
        }
        if (self.stats_sort, self.stats_desc) == DEFAULT_STATS_SORT {
            *self.stats_order.borrow_mut() =
                Some(Rc::new(StatsOrder { aggregates, sorted: None }));
            self.stats_order_ready = Some(key);
            // The swap supersedes any background sort still running.
            if self.stats_order_inflight.take().is_some() {
                self.stats_order_gen += 1;
                self._stats_order_task = Task::ready(());
            }
            return;
        }
        if self.stats_order_inflight == Some(key) {
            return;
        }
        self.stats_order_inflight = Some(key);
        self.stats_order_gen += 1;
        let generation = self.stats_order_gen;
        let column = self.stats_sort;
        let desc = self.stats_desc;
        self._stats_order_task = load_in_background(
            cx,
            move || {
                let stats = &aggregates.stats;
                // Every comparison is integer-only: a row's key rank orders
                // exactly as its key text does (byte-wise), so the Key column
                // sorts by rank and every other column tie-breaks by rank —
                // ties by key keep the order stable regardless of input order.
                // Order-preserving bias from i64 to u64, so a signed value
                // packs into the unsigned key.
                let bias = |v: i64| (v as u64) ^ (1 << 63);
                let primary = |r: &StatRow| match column {
                    StatsColumn::Key => r.rank as u64,
                    StatsColumn::Count => r.count as u64,
                    StatsColumn::Total | StatsColumn::Percent => bias(r.total_us),
                    StatsColumn::Avg => bias(r.avg_us()),
                    StatsColumn::Max => bias(r.max_us),
                };
                // A descending numeric sort inverts its primary; for the Key
                // column the primary IS the tie-break key, whose descending
                // header state shows ascending text.
                let invert = if column == StatsColumn::Key { !desc } else { desc };
                // Decorate-sort on one packed key per row: primary above the
                // always-ascending rank above the row index (never reached —
                // ranks are distinct), so no comparison chases a row pointer.
                let mut keyed: Vec<u128> = stats
                    .iter()
                    .enumerate()
                    .map(|(i, r)| {
                        let p = if invert { !primary(r) } else { primary(r) };
                        ((p as u128) << 64) | ((r.rank as u128) << 32) | i as u128
                    })
                    .collect();
                keyed.sort_unstable();
                let order: Vec<u32> = keyed.into_iter().map(|k| k as u32).collect();
                StatsOrder { aggregates, sorted: Some(order) }
            },
            move |view, stats_order, _cx| {
                // Discard a result the inputs have moved past since this spawn.
                if view.stats_order_gen != generation {
                    return;
                }
                *view.stats_order.borrow_mut() = Some(Rc::new(stats_order));
                view.stats_order_ready = Some(key);
                view.stats_order_inflight = None;
            },
        );
    }

    /// The graph-mode stats table: a header band of clickable, sortable columns
    /// over a scrollable, virtualized body of one row per group, themed like the
    /// rest of the view. The rows are aggregated over the same scope as the
    /// flame graph and sorted by the active column; the percent column is each
    /// group's share of the summed group durations (so it totals 100%).
    ///
    /// Header and body cells are laid out from the one [`STATS_COLUMNS`] spec
    /// through [`stats_cell`], so their widths cannot disagree at any window
    /// width: each fixed column reserves its pixel width without shrinking, and
    /// the one flexible column takes the remainder.
    fn stats_table(
        &self,
        palette: &Palette,
        colors: &bviz_ui_theme::Colors,
        cx: &mut Context<Self>,
    ) -> Div {
        // Reserve the scroll bar's width on the right of the header and every row
        // (the bar is painted as an overlay there), so the last column's values
        // don't run under or up against the bar.
        let header = div()
            .flex()
            .flex_row()
            .h(px(STATS_HEADER_HEIGHT))
            .w_full()
            .pr(px(BAR_WIDTH))
            .bg(palette.header_band)
            .border_b_1()
            .border_color(palette.border)
            .text_color(palette.label)
            .children(STATS_COLUMNS.iter().map(|spec| {
                let column = spec.column;
                let glyph = if self.stats_sort != column {
                    None
                } else if self.stats_desc {
                    Some('▾')
                } else {
                    Some('▴')
                };
                let label = spec.label;
                // Place the caret on the side away from the text's anchor so the
                // label can't shift as the sort indicator moves: left of a
                // right-aligned numeric value, right of the left-aligned key.
                let text = match (glyph, spec.width.is_some()) {
                    (Some(g), true) => format!("{g} {label}"),
                    (Some(g), false) => format!("{label} {g}"),
                    (None, _) => label.to_string(),
                };
                stats_cell(spec, text)
                    .id(SharedString::from(format!("stats-h-{}", spec.label)))
                    .cursor_pointer()
                    .hover(|s| s.bg(palette.scroll_track))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.sort_by(column);
                        cx.notify();
                    }))
            }));

        let table = div()
            .flex()
            .flex_col()
            .size_full()
            .bg(palette.background)
            .text_color(palette.label)
            .text_size(px(STATS_TEXT_SIZE))
            .child(header);

        // Until the off-thread sort lands, keep the header but show a spinner in
        // place of the body rather than an empty band.
        let Some(order) = self.stats_order.borrow().clone() else {
            return table.child(loading_indicator("Sorting…", colors));
        };

        let row_count = order.order().len();
        let scroll_track = palette.scroll_track;
        let body = uniform_list(
            "stats-body",
            row_count,
            move |range, _window, _cx| {
                let graph = &order.aggregates.flame;
                range
                    .map(|i| {
                        // Format only the visible rows here: formatting all ~2M up
                        // front froze the UI thread for seconds and retained a huge
                        // string structure that was slow to drop.
                        let row = order.aggregates.stats[order.order()[i] as usize];
                        let key = graph.str(row.key);
                        let pct = 100.0 * row.total_us as f64
                            / order.aggregates.grand_total.max(1) as f64;
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .h(px(STATS_ROW_HEIGHT))
                            .w_full()
                            .pr(px(BAR_WIDTH))
                            .hover(move |s| s.bg(scroll_track))
                            .children(STATS_COLUMNS.iter().map(|spec| {
                                let text: SharedString = match spec.column {
                                    StatsColumn::Key => key.to_string().into(),
                                    StatsColumn::Count => row.count.to_string().into(),
                                    StatsColumn::Total => format_duration(row.total_us).into(),
                                    StatsColumn::Avg => format_duration(row.avg_us()).into(),
                                    StatsColumn::Max => format_duration(row.max_us).into(),
                                    StatsColumn::Percent => format!("{pct:.1}%").into(),
                                };
                                stats_cell(spec, text)
                            }))
                    })
                    .collect()
            },
        )
        .track_scroll(&self.stats_scroll);

        // A scroll bar painted over the body's right edge: a canvas reads the
        // list's scroll offset and content/visible extents to place the thumb,
        // stashes the geometry for hit-testing, and re-installs the drag handlers
        // each frame while a thumb drag is in progress.
        let scroll = self.stats_scroll.clone();
        let stats_bar = self.stats_bar.clone();
        let entity = cx.entity();
        let dragging = matches!(self.drag, Drag::StatsThumb(_));
        let track = palette.scroll_track;
        let thumb = palette.scroll_thumb;
        let bar = canvas(
            move |_, _, _| {},
            move |bounds, _, window: &mut Window, _| {
                let content = row_count as f32 * STATS_ROW_HEIGHT;
                let visible = f32::from(bounds.size.height);
                let offset = -f32::from(scroll.0.borrow().base_handle.offset().y);
                let layout = Scrollbar::new(
                    Axis::Vertical,
                    Bounds::new(
                        point(bounds.origin.x + bounds.size.width - px(BAR_WIDTH), bounds.origin.y),
                        size(px(BAR_WIDTH), bounds.size.height),
                    ),
                    content,
                    visible,
                    offset,
                    MIN_THUMB,
                );
                if let Some(bar) = layout {
                    bar.paint(track, thumb, window);
                }
                stats_bar.set(layout);
                if dragging {
                    track_drag(window, &entity, on_drag, end_drag);
                }
            },
        );

        table.child(
            div()
                .relative()
                .flex_1()
                .w_full()
                .child(body.size_full())
                .child(bar.absolute().inset_0()),
        )
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

/// A spinning circle-notch icon at `size`, tinted `color`. `id` keys its
/// animation, so distinct spinners in one tree get distinct ids.
fn spinner_icon(id: &'static str, size: Pixels, color: Hsla) -> impl IntoElement {
    bviz_ui_assets::icon("circle-notch")
        .size(size)
        .text_color(color)
        .with_animation(
            id,
            Animation::new(Duration::from_millis(900)).repeat(),
            |icon, delta| icon.with_transformation(Transformation::rotate(percentage(delta))),
        )
}

/// A centered spinner over a caption, shown while a profile loads or its flame
/// graph and stats are (re)built off the UI thread.
fn loading_indicator(label: impl Into<SharedString>, colors: &bviz_ui_theme::Colors) -> Div {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .justify_center()
        .items_center()
        .gap_3()
        .child(spinner_icon("spinner", px(36.), colors.text.into()))
        .child(div().text_sm().text_color(colors.disabled).child(label.into()))
}

impl Render for TraceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = bviz_ui_theme::colors(window);
        let palette = Palette {
            border: colors.border,
            label: colors.text,
            background: colors.background,
            header_band: bviz_ui_theme::mix(colors.container, colors.background, 0.5),
            gridline: bviz_ui_theme::mix(colors.border, colors.background, 0.55),
            // A mid-tone amber, legible against a light or dark background.
            marker: Rgba::from(hsla(0.09, 0.6, 0.55, 1.0)),
            scroll_track: bviz_ui_theme::mix(colors.container, colors.background, 0.3),
            scroll_thumb: bviz_ui_theme::mix(colors.text, colors.background, 0.55),
            divider: bviz_ui_theme::mix(colors.border, colors.background, 0.4),
            panel: colors.container,
            crosshair: bviz_ui_theme::mix(colors.text, colors.background, 0.35),
            selection: colors.selected,
        };
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
            Source::Loading => loading_indicator("Loading…", &colors),
            Source::Message(msg) => div()
                .flex_1()
                .flex()
                .flex_col()
                .justify_center()
                .items_center()
                .gap_4()
                .p_4()
                .child(
                    div()
                        .max_w(px(400.))
                        .text_sm()
                        .text_color(colors.disabled)
                        .child(msg.clone()),
                ),
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

    /// The track whose gutter label lies under `pos`, if the press hit the flame
    /// chart's track-label column (below the ruler, left of the content, above
    /// the tab bar). `None` elsewhere or over a lane's empty tail.
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

    /// Updates the flame-graph panel hover to the box under `pos`, repainting
    /// only when the hovered box changes. The tooltip anchors at the cursor
    /// position on entry into a box and stays there while the cursor moves within
    /// it, so a move that stays in one box costs no repaint.
    fn update_flame_hover(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let hit = self.flame_box_at(pos);
        let changed = match (&self.flame_hover, &hit) {
            (Some(h), Some(b)) => h.boxed != *b,
            (None, None) => false,
            _ => true,
        };
        if !changed {
            return;
        }
        self.flame_hover = hit.map(|boxed| FlameHover { boxed, at: pos });
        cx.notify();
    }

    /// The flame-graph box under a window-space cursor: map the cursor to a depth
    /// row via the vertical scroll and to a viewport x relative to the content
    /// column, then find the laid-out box at that depth whose x-range contains it.
    /// Searching the same viewport-relative box vector the paint uses (widened
    /// and coalesced) makes the hover match what is drawn.
    fn flame_box_at(&self, pos: Point<Pixels>) -> Option<FlameBox> {
        let fl = self.flame_layout()?;
        let aggregates = self.aggregates.borrow().clone()?;
        let metrics = flame_metrics();
        let x = f32::from(pos.x);
        let y = f32::from(pos.y);
        if x < fl.left || x >= fl.left + fl.width || y < fl.top || y >= fl.top + fl.height {
            return None;
        }
        let content_y = y - fl.top + self.flame_scroll_y.get();
        let depth = (content_y / metrics.row_height) as u32;
        let content_w = fl.width as f64 * fl.view.zoom;
        let scroll_x = fl.view.left * content_w;
        let vx = x - fl.left;
        let boxes = flame_boxes(&self.flame_boxes, &aggregates, content_w, scroll_x, fl.width, &metrics);
        boxes
            .iter()
            .find(|b| b.depth == depth && vx >= b.x && vx < b.x + b.w)
            .copied()
    }

    /// Begins a flame-chart drag at `pos`: the lane vertical or time horizontal
    /// scroll thumb/track if the press hit one (the thumb is grabbed where
    /// pressed, a track click centers the thumb there), otherwise a pan.
    fn press(&mut self, pos: Point<Pixels>) {
        if let Some(bar) = self.vbar.get() {
            if bar.track_hit(pos) {
                let grab = bar.grab(pos);
                self.drag = Drag::VThumb(grab);
                scroll_thumb_to(self, pos, grab);
                return;
            }
        }
        if let Some(bar) = self.hbar.get() {
            if bar.track_hit(pos) {
                let grab = bar.grab(pos);
                self.drag = Drag::HThumb(grab);
                pan_thumb_to(self, pos, grab);
                return;
            }
        }
        self.drag = Drag::Pan;
        self.last_mouse = pos;
    }

    /// Begins a flame-graph panel drag at `pos`: its vertical or horizontal
    /// scroll thumb/track if the press hit one, otherwise a pan of the panel.
    fn press_flame(&mut self, pos: Point<Pixels>) {
        if let Some(bar) = self.flame_vbar.get() {
            if bar.track_hit(pos) {
                let grab = bar.grab(pos);
                self.drag = Drag::FlameVThumb(grab);
                flame_scroll_thumb_to(self, pos, grab);
                return;
            }
        }
        if let Some(bar) = self.flame_hbar.get() {
            if bar.track_hit(pos) {
                let grab = bar.grab(pos);
                self.drag = Drag::FlameHThumb(grab);
                self.flame_hthumb_to(pos, grab);
                return;
            }
        }
        self.drag = Drag::FlamePan;
        self.last_mouse = pos;
    }

    /// Begins a stats-table scroll drag if `pos` hit its bar (grabbing the thumb
    /// where pressed, a track click centering the thumb there); otherwise leaves
    /// the press to fall through to a row.
    fn press_stats(&mut self, pos: Point<Pixels>) {
        if let Some(bar) = self.stats_bar.get() {
            if bar.track_hit(pos) {
                let grab = bar.grab(pos);
                self.drag = Drag::StatsThumb(grab);
                stats_thumb_to(self, pos, grab);
            }
        }
    }

    /// If `pos` hits the tracks-menu scroll bar, grabs its thumb (or jumps the
    /// track to it). Returns whether the press was consumed by the bar.
    fn press_menu(&mut self, pos: Point<Pixels>) -> bool {
        let Some(bar) = self.menu_bar.get() else {
            return false;
        };
        if !bar.track_hit(pos) {
            return false;
        }
        let grab = bar.grab(pos);
        self.drag = Drag::MenuThumb(grab);
        menu_thumb_to(self, pos, grab);
        true
    }

    /// Maps a flame-graph panel horizontal thumb drag to its normalized scroll.
    fn flame_hthumb_to(&mut self, pos: Point<Pixels>, grab: f32) {
        let (Some(bar), Some(fl)) = (self.flame_hbar.get(), self.flame_layout()) else {
            return;
        };
        let mut fv = fl.view;
        fv.set_scroll_px(bar.offset_at(pos, grab), fl.width);
        self.flame_view.set(fv);
    }

    /// The flame-graph panel's mouse/scroll/hover wiring: a press drives a scroll
    /// bar or pans, the wheel zooms/pans/scrolls the panel, and a move updates the
    /// panel's box hover. Presses and the wheel stop propagation so they don't
    /// also reach the flame chart's handlers underneath.
    fn flame_panel_handlers(&self, root: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        root.on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, e: &MouseDownEvent, _, cx| {
                this.press_flame(e.position);
                cx.stop_propagation();
                cx.notify();
            }),
        )
        .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, _, cx| {
            this.on_flame_scroll_wheel(e, cx);
            cx.stop_propagation();
        }))
        .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
            if matches!(this.drag, Drag::None) {
                this.update_flame_hover(e.position, cx);
            }
        }))
        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
            if !*hovered && this.flame_hover.take().is_some() {
                cx.notify();
            }
        }))
    }

    /// Ctrl/Cmd+wheel zooms the flame-graph panel about the cursor, Shift+wheel
    /// pans it horizontally, plain wheel scrolls it vertically.
    fn on_flame_scroll_wheel(&mut self, e: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let Some(fl) = self.flame_layout() else {
            return;
        };
        let (dx, dy) = match e.delta {
            ScrollDelta::Lines(p) => (p.x * LINE_SCROLL_PX, p.y * LINE_SCROLL_PX),
            ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
        };
        let (mut fv, left, width) = (fl.view, fl.left, fl.width);
        if e.modifiers.control || e.modifiers.platform {
            let dy_lines = dy / LINE_SCROLL_PX;
            fv.zoom_at(
                f32::from(e.position.x) - left,
                width,
                ZOOM_PER_NOTCH.powf(dy_lines as f64),
            );
            self.flame_view.set(fv);
        } else if e.modifiers.shift {
            fv.pan_px(dx + dy, width);
            self.flame_view.set(fv);
        } else {
            self.flame_scroll_y.set(self.flame_scroll_y.get() - dy);
        }
        // Re-clamp after any wheel action: a zoom that adds or drops the
        // horizontal-bar strip changes the panel's usable height, and so the
        // vertical scroll ceiling.
        let scrolled = self.flame_scroll_y.get().clamp(0.0, self.flame_max_scroll());
        self.flame_scroll_y.set(scrolled);
        cx.notify();
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

/// Maps the flame chart's lane vertical thumb drag back to a scroll offset.
fn scroll_thumb_to(view: &mut TraceView, pos: Point<Pixels>, grab: f32) {
    let Some(bar) = view.vbar.get() else {
        return;
    };
    view.scroll_y.set(bar.offset_at(pos, grab));
}

/// Maps the flame-graph panel's vertical thumb drag back to a scroll offset.
fn flame_scroll_thumb_to(view: &mut TraceView, pos: Point<Pixels>, grab: f32) {
    let Some(bar) = view.flame_vbar.get() else {
        return;
    };
    view.flame_scroll_y.set(bar.offset_at(pos, grab));
}

/// Maps a stats-table thumb drag back to the list's scroll offset (negative y as
/// the body scrolls down).
fn stats_thumb_to(view: &mut TraceView, pos: Point<Pixels>, grab: f32) {
    let Some(bar) = view.stats_bar.get() else {
        return;
    };
    let handle = view.stats_scroll.0.borrow();
    let mut offset = handle.base_handle.offset();
    offset.y = px(-bar.offset_at(pos, grab));
    handle.base_handle.set_offset(offset);
}

/// Maps a tracks-menu thumb drag back to the list's scroll offset (negative y as
/// the list scrolls down).
fn menu_thumb_to(view: &mut TraceView, pos: Point<Pixels>, grab: f32) {
    let Some(bar) = view.menu_bar.get() else {
        return;
    };
    let mut offset = view.menu_scroll.offset();
    offset.y = px(-bar.offset_at(pos, grab));
    view.menu_scroll.set_offset(offset);
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
    let Some(bar) = view.hbar.get() else {
        return;
    };
    let mut v = view.view.get();
    // The bar's offset is measured from the span start less the reachable margin,
    // so map it back through the same shift before clamping.
    let margin_us = EDGE_MARGIN_PX / v.px_per_us as f32;
    v.start_us = (span.start_us as f32 - margin_us) as f64 + bar.offset_at(pos, grab) as f64;
    v.clamp_to(span, regions.time_w, EDGE_MARGIN_PX);
    view.view.set(v);
}

/// Picks the lowest of `rows` whose last occupied right-extent leaves room for a
/// label spanning `[left, right]`, adding a new row when none is clear, and
/// records the label's extent in the chosen row. Keeps clustered labels visible
/// by staggering them down across as many rows as the cluster needs.
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
        window.paint_quad(quad(
            Bounds::new(point(px(lx), px(top)), size(px(1.), px(height))),
            px(0.),
            palette.marker,
            px(0.),
            Hsla::from(palette.border),
            BorderStyle::Solid,
        ));
        // Continue the marker line through the counter panel, which shares the
        // time axis, so a phase boundary reads across both.
        if show_counters {
            window.paint_quad(quad(
                Bounds::new(
                    point(px(lx), px(regions.panel_top)),
                    size(px(1.), px(regions.panel_h())),
                ),
                px(0.),
                palette.marker,
                px(0.),
                Hsla::from(palette.border),
                BorderStyle::Solid,
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
        // Place the label to the right of the line, flipping it left of the line
        // when it would otherwise spill past the right edge.
        let (left, tx) = if lx + SLICE_TEXT_PAD + label_w > regions.time_right {
            let left = lx - SLICE_TEXT_PAD - label_w;
            (left, left)
        } else {
            let tx = lx + SLICE_TEXT_PAD;
            (tx, tx)
        };
        let row = assign_marker_row(&mut rows, left, left + label_w + SLICE_TEXT_PAD);
        let _ = line.paint(
            point(px(tx), px(top + 2.0 + row as f32 * row_h)),
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

/// The smallest 1/2/5 ×10ⁿ value that is at least `min`.
fn nice_step(min: f64) -> f64 {
    if min <= 0.0 {
        return 1.0;
    }
    let pow = 10f64.powf(min.log10().floor());
    [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .map(|m| m * pow)
        .find(|&s| s >= min)
        .unwrap_or(pow * 10.0)
}

/// Formats a profile-relative timestamp (microseconds) in the unit `step`
/// warrants, with just enough decimals to tell adjacent ticks apart.
fn format_time(us: f64, step: f64) -> String {
    let (unit, scale) = if step < 1_000.0 {
        ("µs", 1.0)
    } else if step < 1_000_000.0 {
        ("ms", 1_000.0)
    } else {
        ("s", 1_000_000.0)
    };
    let step_in_unit = step / scale;
    let decimals = if step_in_unit >= 1.0 {
        0
    } else if step_in_unit >= 0.1 {
        1
    } else {
        2
    };
    // Collapse a negative zero (from rounding a tick just below the origin) to 0.
    let value = us / scale;
    let value = if value == 0.0 { 0.0 } else { value };
    format!("{:.*}{}", decimals, value, unit)
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
        window.paint_quad(quad(
            rect,
            px(0.),
            palette.gridline,
            px(0.),
            Hsla::from(palette.border),
            BorderStyle::Solid,
        ));
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
    window.paint_quad(quad(
        band,
        px(0.),
        palette.header_band,
        px(0.),
        Hsla::from(palette.border),
        BorderStyle::Solid,
    ));
    let font = bviz_ui_assets::text_metrics(window).font;
    let fs = px(RULER_HEIGHT - 2. * SLICE_TEXT_PAD);
    let ty = bounds.origin.y + px((RULER_HEIGHT - f32::from(fs)) / 2.);
    for (x, label) in ticks {
        let tx = px(regions.content_left + *x);
        let tick = Bounds::new(
            point(tx, bounds.origin.y + px(RULER_HEIGHT - 4.)),
            size(px(1.), px(4.)),
        );
        window.paint_quad(quad(
            tick,
            px(0.),
            palette.label,
            px(0.),
            Hsla::from(palette.border),
            BorderStyle::Solid,
        ));
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
        MIN_THUMB,
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
        MIN_THUMB,
    );

    for bar in [vlayout, hlayout].into_iter().flatten() {
        bar.paint(palette.scroll_track, palette.scroll_thumb, window);
    }
    vbar.set(vlayout);
    hbar.set(hlayout);
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

    // Thread and group labels scroll with the lanes, so clip them to the flame
    // region.
    let flame_mask = ContentMask {
        bounds: Bounds::new(
            point(px(gutter_left), px(regions.flame_top)),
            size(px(gutter_width), px(regions.flame_h())),
        ),
    };
    window.with_content_mask(Some(flame_mask), |window| {
        paint_thread_labels(
            profile, layout, regions, gutter_left, gutter_width, scroll_y, selected, avail, &font,
            fs, palette, window, cx,
        );
        paint_group_bands(
            chrome, regions, gutter_left, gutter_width, scroll_y, avail, &font, fs, palette, window,
            cx,
        );
    });

    if show_counters {
        paint_counter_labels(profile, regions, gutter_left, avail, &font, fs, palette, window, cx);
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
    window.paint_quad(quad(
        strip,
        px(0.),
        palette.header_band,
        px(0.),
        Hsla::from(palette.border),
        BorderStyle::Solid,
    ));
    window.paint_quad(quad(
        Bounds::new(
            point(px(regions.content_left - 1.0), px(regions.flame_top)),
            size(px(1.0), px(gutter_bottom - regions.flame_top)),
        ),
        px(0.),
        palette.divider,
        px(0.),
        Hsla::from(palette.border),
        BorderStyle::Solid,
    ));
}

/// Shapes `name` truncated to `avail` and paints it left-aligned at the gutter's
/// text inset, vertically offset to `ty`.
#[allow(clippy::too_many_arguments)]
fn paint_gutter_label(
    name: &str,
    ty: f32,
    gutter_left: f32,
    avail: f32,
    font: &gpui::Font,
    fs: Pixels,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let line = shape_truncated(name, avail, font.clone(), fs, palette.label.into(), window);
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
    profile: &Profile,
    layout: &LaneLayout,
    regions: Regions,
    gutter_left: f32,
    gutter_width: f32,
    scroll_y: f32,
    selected: &[bool],
    avail: f32,
    font: &gpui::Font,
    fs: Pixels,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let selection_tint = bviz_ui_theme::mix(palette.selection, palette.header_band, 0.8);
    for lane in &layout.lanes {
        let top = regions.flame_top + lane.top - scroll_y;
        if top + lane.height < regions.flame_top || top > regions.flame_bottom {
            continue;
        }
        if selected.get(lane.track).copied().unwrap_or(false) {
            window.paint_quad(quad(
                Bounds::new(point(px(gutter_left), px(top)), size(px(gutter_width), px(lane.height))),
                px(0.),
                selection_tint,
                px(0.),
                Hsla::from(palette.border),
                BorderStyle::Solid,
            ));
            window.paint_quad(quad(
                Bounds::new(point(px(gutter_left), px(top)), size(px(2.0), px(lane.height))),
                px(0.),
                palette.selection,
                px(0.),
                Hsla::from(palette.border),
                BorderStyle::Solid,
            ));
        }
        let ty = top + (lane.height - GUTTER_LABEL_SIZE) / 2.0;
        paint_gutter_label(
            profile.str(profile.threads()[lane.track].name),
            ty,
            gutter_left,
            avail,
            font,
            fs,
            palette,
            window,
            cx,
        );
    }
}

/// Paints each group header band's chevron, class name, and shown/total lane
/// count; a capped, expanded group shows a caret-double icon at the gutter's right
/// edge — down to show every lane, up to show the busiest.
#[allow(clippy::too_many_arguments)]
fn paint_group_bands(
    chrome: &GroupChrome,
    regions: Regions,
    gutter_left: f32,
    gutter_width: f32,
    scroll_y: f32,
    avail: f32,
    font: &gpui::Font,
    fs: Pixels,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
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
        let line = shape_truncated(&text, label_avail, font.clone(), fs, strong, window);
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
#[allow(clippy::too_many_arguments)]
fn paint_counter_labels(
    profile: &Profile,
    regions: Regions,
    gutter_left: f32,
    avail: f32,
    font: &gpui::Font,
    fs: Pixels,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    for gl in graph_lanes(profile.counters().len(), regions.panel_top, regions.panel_h()) {
        let lane_bottom = gl.top + METRICS.header_height + gl.chart_h;
        let ty = lane_bottom - GUTTER_LABEL_SIZE - SLICE_TEXT_PAD;
        paint_gutter_label(
            profile.str(profile.counters()[gl.lane].name),
            ty,
            gutter_left,
            avail,
            font,
            fs,
            palette,
            window,
            cx,
        );
    }
}

/// Shapes `text` at `fs`, truncating with an ellipsis to fit `avail` pixels by
/// the same proportional estimate slices and gutter labels share.
fn shape_truncated(
    text: &str,
    avail: f32,
    font: gpui::Font,
    fs: Pixels,
    color: Hsla,
    window: &mut Window,
) -> gpui::ShapedLine {
    let line = bviz_ui_assets::shape_line(window, text.to_string(), font.clone(), fs, color);
    if f32::from(line.width()) <= avail {
        return line;
    }
    // The proportional estimate can overshoot in a variable-width font, leaving
    // the ellipsized line wider than the box, so drop characters until it fits.
    let chars: Vec<char> = text.chars().collect();
    let mut keep = ((chars.len() as f32 * avail / f32::from(line.width())).floor() as usize)
        .min(chars.len())
        .saturating_sub(1);
    loop {
        let mut truncated: String = chars[..keep].iter().collect();
        truncated.push('…');
        let shaped = bviz_ui_assets::shape_line(window, truncated, font.clone(), fs, color);
        if keep == 0 || f32::from(shaped.width()) <= avail {
            return shaped;
        }
        keep -= 1;
    }
}

/// Paints a filled box and, if it is wide enough, its name truncated to fit and
/// centered vertically — the shared shape behind a flame-chart slice and a
/// flame-graph node.
fn paint_labeled_box(
    rect: Bounds<Pixels>,
    fill: Hsla,
    name: &str,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let w = f32::from(rect.size.width);
    let h = f32::from(rect.size.height);
    // Square corners: every column paints the box's full height, so a run of
    // thin boxes reads as an even band rather than rounded humps that dip in
    // height between them. A box wide enough to spare its first column carries a
    // muted left-edge seam to divide it from its neighbor; a narrower one keeps
    // its whole fill so it still reads as its color rather than an edge line.
    let (border, seam) = if w > SLICE_SEAM_MIN_PX {
        let seam = Hsla::from(bviz_ui_theme::mix(Rgba::from(fill), palette.background, SLICE_SEAM_MIX));
        (Edges { left: px(1.), ..Default::default() }, seam)
    } else {
        (Edges::default(), fill)
    };
    window.paint_quad(quad(rect, px(0.), fill, border, seam, BorderStyle::Solid));
    if w < LABEL_MIN_PX {
        return;
    }
    let avail = w - 2. * SLICE_TEXT_PAD;
    let font = bviz_ui_assets::text_metrics(window).font;
    let fs = px(h - 2. * SLICE_TEXT_PAD);
    let line = shape_truncated(name, avail, font, fs, label_on(fill), window);
    let ty = rect.origin.y + px((h - f32::from(fs)) / 2.);
    let _ = line.paint(
        point(rect.origin.x + px(SLICE_TEXT_PAD), ty),
        fs,
        TextAlign::Left,
        None,
        window,
        cx,
    );
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
    let fill = span_role_color(slice.role, palette);
    paint_labeled_box(rect, fill, profile.str(slice.name), palette, window, cx);
}

/// Fill for a span, keyed by its [`SpanRole`]: the workload gives the base hue and
/// the payload flag its weight, so the compiler and the Starlark that drives the
/// build read vivid while Bazel's own overhead (scheduling, caching, graph
/// bookkeeping, the sandbox) recedes. Both the flame chart and the flame graph
/// paint with it, so a role is the same color in both. The Other workload —
/// including the flame graph's synthetic root — is a neutral hueless grey.
fn span_role_color(role: SpanRole, palette: &Palette) -> Hsla {
    let dark = luminance(palette.background) < 0.5;
    if role.workload == Workload::Other {
        let lightness = if dark {
            FLAME_ROOT_LIGHTNESS_DARK
        } else {
            FLAME_ROOT_LIGHTNESS_LIGHT
        };
        return hsla(0.0, 0.0, lightness, 1.0);
    }
    let hue = match role.workload {
        Workload::Subprocess => HUE_SUBPROCESS,
        Workload::Starlark => HUE_STARLARK,
        Workload::Filesystem => HUE_FILESYSTEM,
        Workload::RemoteCache => HUE_REMOTE_CACHE,
        Workload::Skyframe => HUE_SKYFRAME,
        Workload::Scheduling => HUE_SCHEDULING,
        Workload::Gc => HUE_GC,
        Workload::Other => unreachable!("other handled above"),
    };
    let (saturation, lightness) = if role.payload {
        (
            PAYLOAD_SATURATION,
            if dark { PAYLOAD_LIGHTNESS_DARK } else { PAYLOAD_LIGHTNESS_LIGHT },
        )
    } else {
        (
            OVERHEAD_SATURATION,
            if dark { OVERHEAD_LIGHTNESS_DARK } else { OVERHEAD_LIGHTNESS_LIGHT },
        )
    };
    hsla(hue, saturation, lightness, 1.0)
}

/// A legend of the span-role colors: a swatch and name per workload in a row,
/// matching the fills the flame boxes use. Payload workloads lead, then overhead.
fn role_legend(palette: Palette) -> impl IntoElement {
    const ROLES: [(Workload, bool, &str); 8] = [
        (Workload::Subprocess, true, "action"),
        (Workload::Starlark, true, "starlark"),
        (Workload::Filesystem, false, "filesystem"),
        (Workload::RemoteCache, false, "remote/cache"),
        (Workload::Scheduling, false, "scheduling"),
        (Workload::Skyframe, false, "skyframe"),
        (Workload::Gc, false, "gc"),
        (Workload::Other, false, "other"),
    ];
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .text_color(palette.label)
        .text_size(px(STATS_TEXT_SIZE))
        .children(ROLES.map(|(workload, payload, name)| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(LEGEND_LABEL_GAP))
                .child(
                    div()
                        .size(px(LEGEND_SWATCH))
                        .rounded_sm()
                        .bg(span_role_color(SpanRole { workload, payload }, &palette)),
                )
                .child(name)
        }))
}

/// Paints the aggregated flame graph filling the body: each box a work-class-colored
/// quad with a truncated label, clipped to the content rect, scrolled vertically
/// by `scroll_y` and zoomed/panned horizontally by `flame_view`. A vertical
/// scroll bar shows when the tree's rows overflow the height and a horizontal one
/// when the zoomed width overflows the width; a tooltip draws for the hovered
/// box. No time-axis chrome (ruler, gridlines, gutter, markers) applies, so none
/// is drawn.
#[allow(clippy::too_many_arguments)]
fn paint_flame_graph(
    aggregates: &Arc<ScopeAggregate>,
    cache: &Rc<RefCell<Option<FlameBoxCache>>>,
    scroll_y: f32,
    flame_view: FlameView,
    palette: &Palette,
    vbar: &Rc<Cell<Option<Scrollbar>>>,
    hbar: &Rc<Cell<Option<Scrollbar>>>,
    hover: Option<FlameHover>,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let graph = &aggregates.flame;
    let metrics = flame_metrics();
    let total_h = flame_height(graph, &metrics);
    let (left, top, width, height) = flame_graph_rect(bounds, total_h, flame_view);
    // Lay out only the visible window, viewport-relative: the tree occupies
    // `content_w` pixels at this zoom, scrolled `scroll_x` in, so a box's x is
    // already relative to the content column's left edge. Off-screen boxes are
    // culled and coalescing runs locally, matching the flame chart's slice
    // layout. Coordinates are f64 so precision holds at deep zoom.
    let content_w = width as f64 * flame_view.zoom;
    let scroll_x = flame_view.left * content_w;
    let boxes = flame_boxes(cache, aggregates, content_w, scroll_x, width, &metrics);

    let mask = ContentMask {
        bounds: Bounds::new(point(px(left), px(top)), size(px(width), px(height))),
    };
    window.with_content_mask(Some(mask), |window| {
        for b in boxes.iter() {
            let y = top + b.y - scroll_y;
            if y + b.h < top || y > top + height {
                continue;
            }
            let rect = Bounds::new(point(px(left + b.x), px(y)), size(px(b.w), px(b.h)));
            let fill = span_role_color(b.role, palette);
            paint_labeled_box(rect, fill, graph.str(b.name), palette, window, cx);
        }
    });

    let vlayout = Scrollbar::new(
        Axis::Vertical,
        Bounds::new(
            point(px(left + width), px(top)),
            size(px(BAR_WIDTH), px(height)),
        ),
        total_h,
        height,
        scroll_y,
        MIN_THUMB,
    );
    if let Some(bar) = vlayout {
        bar.paint(palette.scroll_track, palette.scroll_thumb, window);
    }
    vbar.set(vlayout);

    // The horizontal bar runs along the bottom strip reserved beneath the content.
    let hlayout = Scrollbar::new(
        Axis::Horizontal,
        Bounds::new(
            point(px(left), px(top + height)),
            size(px(width), px(BAR_WIDTH)),
        ),
        content_w as f32,
        width,
        scroll_x as f32,
        MIN_THUMB,
    );
    if let Some(bar) = hlayout {
        bar.paint(palette.scroll_track, palette.scroll_thumb, window);
    }
    hbar.set(hlayout);

    if let Some(h) = hover {
        paint_flame_tooltip(graph, &h.boxed, graph.root().total_us, h.at, bounds, palette, window, cx);
    }
}

/// Lays out the graph's visible window, reusing the [`FlameBoxCache`] in `cache`
/// unless a key changed. The retained `aggregates` keeps the graph alive for the
/// boxes' name handles and keys the cache by identity; the layout is
/// viewport-relative, so panning (a change in `scroll_x`) is a key too.
fn flame_boxes(
    cache: &Rc<RefCell<Option<FlameBoxCache>>>,
    aggregates: &Arc<ScopeAggregate>,
    content_w: f64,
    scroll_x: f64,
    view_w: f32,
    metrics: &FlameMetrics,
) -> Rc<Vec<FlameBox>> {
    if let Some(c) = cache.borrow().as_ref() {
        if Arc::ptr_eq(&c.aggregates, aggregates)
            && c.content_w == content_w
            && c.scroll_x == scroll_x
            && c.view_w == view_w
        {
            return c.boxes.clone();
        }
    }
    let boxes = Rc::new(lay_out_flame(
        &aggregates.flame,
        content_w,
        scroll_x,
        view_w,
        metrics,
        MIN_SLICE_PX,
    ));
    *cache.borrow_mut() = Some(FlameBoxCache {
        aggregates: aggregates.clone(),
        content_w,
        scroll_x,
        view_w,
        boxes: boxes.clone(),
    });
    boxes
}

/// Paints a tooltip near `at` with the flame node's name and, below it, its total
/// and self time, occurrence count, and share of the view's aggregated total.
#[allow(clippy::too_many_arguments)]
fn paint_flame_tooltip(
    graph: &FlameGraph,
    boxed: &FlameBox,
    root_total_us: i64,
    at: Point<Pixels>,
    bounds: Bounds<Pixels>,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let pct = if root_total_us > 0 {
        100.0 * boxed.total_us as f64 / root_total_us as f64
    } else {
        0.0
    };
    let detail = format!(
        "total {}, self {}, {}×, {:.1}% of view",
        format_duration(boxed.total_us),
        format_duration(boxed.self_us),
        boxed.count,
        pct,
    );
    let lines = [graph.str(boxed.name).to_string(), detail];
    paint_tooltip_box(&lines, at, bounds, palette, window, cx);
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

/// A compact duration label (e.g. `4.20ms`) for a microsecond span.
fn format_duration(us: i64) -> String {
    let a = (us as f64).abs();
    if a < 1_000.0 {
        format!("{us}µs")
    } else if a < 1_000_000.0 {
        format!("{:.2}ms", us as f64 / 1_000.0)
    } else {
        format!("{:.2}s", us as f64 / 1_000_000.0)
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

/// A stable hue in [0, 1) for `s`: FNV-1a over the bytes folded onto the wheel.
/// The same string always maps to the same hue.
fn hue_of(s: &str) -> f32 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    (hash % 360) as f32 / 360.0
}

/// A translucent fill for a counter series, keyed by its name like a category.
fn counter_color(series: &str) -> Hsla {
    hsla(
        hue_of(series),
        CATEGORY_SATURATION,
        0.55,
        COUNTER_AREA_ALPHA,
    )
}

/// The y of the segment `(x0, y0)`–`(x1, y1)` at `x`; `x0` must differ from `x1`.
fn lerp_y(x0: f32, y0: f32, x1: f32, y1: f32, x: f32) -> f32 {
    y0 + (y1 - y0) * (x - x0) / (x1 - x0)
}

/// Clips a polyline whose x is non-decreasing to the vertical strip `[lo, hi]`,
/// interpolating y where a segment crosses an edge so the on-screen shape is
/// exact. At high zoom a series' off-screen vertices otherwise carry pixel x far
/// outside the window, where the renderer loses precision and the filled area
/// bleeds past its content mask.
fn clip_polyline_x(verts: &[(f32, f32)], lo: f32, hi: f32) -> Vec<(f32, f32)> {
    let mut out: Vec<(f32, f32)> = Vec::new();
    let mut push = |p: (f32, f32)| {
        if out.last() != Some(&p) {
            out.push(p);
        }
    };
    for seg in verts.windows(2) {
        let (x0, y0) = seg[0];
        let (x1, y1) = seg[1];
        if x1 < lo || x0 > hi {
            continue;
        }
        if x0 >= x1 {
            if (lo..=hi).contains(&x0) {
                push((x0, y0));
                push((x1, y1));
            }
            continue;
        }
        let a = if x0 < lo {
            (lo, lerp_y(x0, y0, x1, y1, lo))
        } else {
            (x0, y0)
        };
        let b = if x1 > hi {
            (hi, lerp_y(x0, y0, x1, y1, hi))
        } else {
            (x1, y1)
        };
        push(a);
        push(b);
    }
    out
}

/// Maps a counter lane's values to y-positions in its chart area, so the plotted
/// line and the crosshair readout share one transform. The floor (value 0) sits
/// at `chart_bottom`; `max_value` reaches `chart_top`.
struct CounterYScale {
    chart_top: f32,
    chart_bottom: f32,
    scale: f32,
}

impl CounterYScale {
    fn new(lane_top: f32, chart_h: f32, max_value: f64) -> Self {
        let chart_top = lane_top + METRICS.header_height;
        Self {
            chart_top,
            chart_bottom: chart_top + chart_h,
            scale: chart_h / max_value as f32,
        }
    }

    fn value_to_y(&self, value: f64) -> f32 {
        self.chart_bottom - value as f32 * self.scale
    }
}

/// Paints the bottom graph section: every counter lane fitted to the section
/// height. Each lane is a header band over a chart area that holds the lane's
/// series as a filled area scaled to the chart height and the series' peak.
/// Charts are confined to the content column. Work per lane is bounded by the
/// viewport: the visible points are found by binary search and dense runs are
/// collapsed to per-pixel-column extremes before tessellation.
fn paint_graph(
    profile: &Profile,
    tv: TimeView,
    regions: Regions,
    palette: &Palette,
    window: &mut Window,
) {
    if !regions.has_graph {
        return;
    }
    let counters = profile.counters();
    let lanes = graph_lanes(counters.len(), regions.panel_top, regions.panel_h());
    let content_mask = ContentMask {
        bounds: Bounds::new(
            point(px(regions.content_left), px(regions.panel_top)),
            size(px(regions.time_w), px(regions.panel_h())),
        ),
    };
    for gl in lanes {
        let counter = &counters[gl.lane];
        let band = Bounds::new(
            point(px(regions.content_left), px(gl.top)),
            size(px(regions.time_w), px(METRICS.header_height)),
        );
        window.paint_quad(quad(
            band,
            px(0.),
            palette.header_band,
            px(0.),
            Hsla::from(palette.border),
            BorderStyle::Solid,
        ));
        if gl.chart_h <= 0.0 {
            continue;
        }

        let Some(series) = profile.series(counter).first() else {
            continue;
        };
        if series.max_value <= 0.0 {
            continue;
        }
        let points = profile.points(series);
        if points.len() < 2 {
            continue;
        }
        let y_scale = CounterYScale::new(gl.top, gl.chart_h, series.max_value as f64);
        let chart_bottom = y_scale.chart_bottom;
        let x_of = |ts| regions.content_left + tv.x_of(ts);
        // Clip the series to a margin around the content column so off-screen
        // vertices at high zoom stay near the window and the renderer keeps the
        // filled area's precision; the margin exceeds the window, so the visible
        // plot is unchanged.
        let clip_lo = regions.content_left - regions.time_w;
        let clip_hi = regions.content_left + 2.0 * regions.time_w;
        // The points that can reach the clip strip: from the last point at or
        // before its left edge to the first at or after its right edge.
        let t_lo = tv.t_of(clip_lo - regions.content_left);
        let t_hi = tv.t_of(clip_hi - regions.content_left);
        let start = points
            .partition_point(|p| (p.ts_us as f64) < t_lo)
            .saturating_sub(1);
        let end = (points.partition_point(|p| (p.ts_us as f64) < t_hi) + 1).min(points.len());
        let visible = &points[start..end];
        if visible.len() < 2 {
            continue;
        }
        let boundary = downsample_columns(
            visible
                .iter()
                .map(|p| (x_of(p.ts_us), y_scale.value_to_y(p.value))),
        );
        let top = clip_polyline_x(&boundary, clip_lo, clip_hi);
        if top.len() < 2 {
            continue;
        }
        window.with_content_mask(Some(content_mask), |window| {
            let mut area = PathBuilder::fill();
            area.move_to(point(px(top[0].0), px(top[0].1)));
            for &(x, y) in &top[1..] {
                area.line_to(point(px(x), px(y)));
            }
            // The area sits on the chart floor, so the base is two corners.
            area.line_to(point(px(top[top.len() - 1].0), px(chart_bottom)));
            area.line_to(point(px(top[0].0), px(chart_bottom)));
            if let Ok(path) = area.build() {
                window.paint_path(path, counter_color(profile.str(series.name)));
            }
        });
    }
}

/// Collapses a boundary polyline (x non-decreasing) to at most two vertices per
/// whole-pixel column: a column crowded with points keeps only its extreme-y
/// pair, in encounter order, preserving the filled area's per-pixel envelope; a
/// column with at most two points passes through unchanged, so sparse runs keep
/// their exact linear slopes at high zoom. Streams its input, so the caller
/// need not materialize the projected vertices.
fn downsample_columns(verts: impl IntoIterator<Item = (f32, f32)>) -> Vec<(f32, f32)> {
    struct Col {
        x: f32,
        n: usize,
        head: [(f32, f32); 2],
        lo_i: usize,
        lo: (f32, f32),
        hi_i: usize,
        hi: (f32, f32),
    }
    fn flush(c: &Col, out: &mut Vec<(f32, f32)>) {
        if c.n <= 2 {
            out.extend_from_slice(&c.head[..c.n]);
        } else if c.lo_i == c.hi_i {
            out.push(c.lo);
        } else if c.lo_i < c.hi_i {
            out.extend_from_slice(&[c.lo, c.hi]);
        } else {
            out.extend_from_slice(&[c.hi, c.lo]);
        }
    }
    let mut out: Vec<(f32, f32)> = Vec::new();
    let mut cur: Option<Col> = None;
    for v in verts {
        let x = v.0.floor();
        match &mut cur {
            Some(c) if c.x == x => {
                if c.n < 2 {
                    c.head[c.n] = v;
                }
                if v.1 < c.lo.1 {
                    (c.lo_i, c.lo) = (c.n, v);
                }
                if v.1 > c.hi.1 {
                    (c.hi_i, c.hi) = (c.n, v);
                }
                c.n += 1;
            }
            _ => {
                if let Some(c) = &cur {
                    flush(c, &mut out);
                }
                cur = Some(Col {
                    x,
                    n: 1,
                    head: [v, v],
                    lo_i: 0,
                    lo: v,
                    hi_i: 0,
                    hi: v,
                });
            }
        }
    }
    if let Some(c) = &cur {
        flush(c, &mut out);
    }
    out
}

/// The series' value at time `t` (microseconds), linearly interpolated between
/// the two bracketing points. `None` when `t` lies outside the series' covered
/// `[first.ts, last.ts]` range. Points are assumed sorted by `ts_us` ascending.
fn sample_at(points: &[CounterPoint], t: f64) -> Option<f64> {
    let first = points.first()?;
    let last = points.last()?;
    if t < first.ts_us as f64 || t > last.ts_us as f64 {
        return None;
    }
    let hi = points.partition_point(|p| (p.ts_us as f64) < t);
    if hi == 0 {
        return Some(points[0].value);
    }
    let a = &points[hi - 1];
    let b = &points[hi];
    let span = (b.ts_us - a.ts_us) as f64;
    if span <= 0.0 {
        return Some(b.value);
    }
    let frac = (t - a.ts_us as f64) / span;
    Some(a.value + (b.value - a.value) * frac)
}

/// The unit a counter lane's values carry, inferred from its name since the
/// trace format stores none. Empty for unitless counts (action counts,
/// SkyFunctions, RPCs, load averages).
fn unit_for(lane: &str) -> &'static str {
    let lane = lane.to_lowercase();
    if lane.contains("cpu usage") {
        "cores"
    } else if lane.contains("memory") {
        "MB"
    } else if lane.contains("network") {
        "Mbps"
    } else if lane.contains("pressure") {
        "%"
    } else {
        ""
    }
}

/// A compact `"<value> <unit>"` label, rescaling memory past 1 GB.
fn format_counter(value: f64, unit: &str) -> String {
    match unit {
        "cores" => format!("{value:.2} cores"),
        "MB" => {
            if value >= 1024.0 {
                format!("{:.2} GB", value / 1024.0)
            } else {
                format!("{value:.0} MB")
            }
        }
        "Mbps" => format!("{value:.2} Mbps"),
        "%" => format!("{value:.0}%"),
        _ => format!("{value:.1}"),
    }
}

/// Paints the hover crosshair: a vertical line at the cursor x spanning both
/// sections, the hovered absolute time near the ruler, and — for each counter
/// lane whose series covers the time — a dot where the crosshair meets its
/// line plus a value label. Scaling matches [`paint_graph`].
fn paint_crosshair(
    profile: &Profile,
    tv: TimeView,
    regions: Regions,
    at: Point<Pixels>,
    show_counters: bool,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let cx_px = f32::from(at.x).min(regions.time_right);
    let t = tv.t_of(cx_px - regions.content_left);

    let crosshair_line = |top: f32, height: f32, window: &mut Window| {
        window.paint_quad(quad(
            Bounds::new(point(px(cx_px), px(top)), size(px(1.), px(height))),
            px(0.),
            palette.crosshair,
            px(0.),
            Hsla::from(palette.border),
            BorderStyle::Solid,
        ));
    };
    crosshair_line(regions.flame_top, regions.flame_h(), window);
    // The counter panel shares the time axis, so the crosshair continues into it.
    if show_counters {
        crosshair_line(regions.panel_top, regions.panel_h(), window);
    }

    let font = bviz_ui_assets::text_metrics(window).font;

    // The hovered time, formatted like the ruler labels, just under the ruler.
    let step = nice_step(RULER_TARGET_TICK_PX / tv.px_per_us);
    let time_line = bviz_ui_assets::shape_line(
        window,
        format_time(t, step),
        font.clone(),
        px(CROSSHAIR_VALUE_SIZE),
        palette.label.into(),
    );
    let time_w = f32::from(time_line.width());
    let time_x = (cx_px + CROSSHAIR_LABEL_GAP).min(regions.time_right - time_w);
    let _ = time_line.paint(
        point(px(time_x), px(regions.flame_top + 1.)),
        px(CROSSHAIR_VALUE_SIZE),
        TextAlign::Left,
        None,
        window,
        cx,
    );

    if !show_counters {
        return;
    }
    let counters = profile.counters();
    let lanes = graph_lanes(counters.len(), regions.panel_top, regions.panel_h());
    for gl in lanes {
        if gl.chart_h <= 0.0 {
            continue;
        }
        let counter = &counters[gl.lane];
        let Some(series) = profile.series(counter).first() else {
            continue;
        };
        if series.max_value <= 0.0 {
            continue;
        }
        let Some(value) = sample_at(profile.points(series), t) else {
            continue;
        };
        let y_scale = CounterYScale::new(gl.top, gl.chart_h, series.max_value as f64);
        let chart_top = y_scale.chart_top;
        let chart_bottom = y_scale.chart_bottom;
        let dot_y = y_scale.value_to_y(value);
        let color = counter_color(profile.str(series.name));
        window.paint_quad(quad(
            Bounds::new(
                point(
                    px(cx_px - CROSSHAIR_DOT_RADIUS),
                    px(dot_y - CROSSHAIR_DOT_RADIUS),
                ),
                size(
                    px(2.0 * CROSSHAIR_DOT_RADIUS),
                    px(2.0 * CROSSHAIR_DOT_RADIUS),
                ),
            ),
            px(CROSSHAIR_DOT_RADIUS),
            color,
            px(0.),
            Hsla::from(palette.border),
            BorderStyle::Solid,
        ));

        // The label's top sits at its dot, clamped into the chart.
        let max_label_y = (chart_bottom - CROSSHAIR_VALUE_SIZE).max(chart_top);
        let label_y = dot_y.clamp(chart_top, max_label_y);
        let unit = unit_for(profile.str(counter.name));
        let value_line = bviz_ui_assets::shape_line(
            window,
            format_counter(value, unit),
            font.clone(),
            px(CROSSHAIR_VALUE_SIZE),
            opaque(color).into(),
        );
        let value_w = f32::from(value_line.width());
        // Default to the right of the line; flip to the left near the edge.
        let mut value_x = cx_px + CROSSHAIR_LABEL_GAP;
        if value_x + value_w > regions.time_right {
            value_x = cx_px - CROSSHAIR_LABEL_GAP - value_w;
        }
        let _ = value_line.paint(
            point(px(value_x), px(label_y)),
            px(CROSSHAIR_VALUE_SIZE),
            TextAlign::Left,
            None,
            window,
            cx,
        );
    }
}

/// `color` at full opacity, for legible text tinted like its translucent dot.
fn opaque(color: Hsla) -> Hsla {
    Hsla { a: 1.0, ..color }
}

/// Perceptual luminance of `c`, used to choose contrasting text and theme-fitting
/// lightness.
fn luminance(c: Rgba) -> f32 {
    0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b
}

/// Black or white, whichever reads on `fill`.
fn label_on(fill: Hsla) -> Hsla {
    let rgba = Rgba::from(fill);
    if luminance(rgba) < 0.5 {
        hsla(0.0, 0.0, 1.0, 1.0)
    } else {
        hsla(0.0, 0.0, 0.0, 1.0)
    }
}

/// The pixels-per-microsecond at which `span` fits `width_px` less `margin_px` of
/// empty space at each end; the zoom-out floor, matching [`TimeView::fit`].
fn fit_px_per_us(span: TimeSpan, width_px: f32, margin_px: f32) -> f64 {
    let span_us = (span.end_us - span.start_us).max(1) as f64;
    (width_px - 2.0 * margin_px).max(1.0) as f64 / span_us
}

fn span_seconds(span: TimeSpan) -> f64 {
    (span.end_us - span.start_us) as f64 / 1_000_000.0
}

/// Renders a Bazel `--profile` trace as a flame-chart timeline.
#[derive(Parser)]
struct Cli {
    /// `bazel build --profile=FILE` output (`.profile.gz` or `.json`).
    #[arg(long, value_name = "FILE")]
    profile: PathBuf,
}

fn main() {
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
    fn fit_px_per_us_maps_span_to_width() {
        let span = TimeSpan {
            start_us: 0,
            end_us: 1000,
        };
        assert_eq!(fit_px_per_us(span, 100.0, 0.0), 0.1);
        // A 10px margin each side leaves 80px for the span.
        assert_eq!(fit_px_per_us(span, 100.0, 10.0), 0.08);
        // An empty span clamps the divisor to 1 rather than dividing by zero.
        let empty = TimeSpan {
            start_us: 5,
            end_us: 5,
        };
        assert_eq!(fit_px_per_us(empty, 100.0, 0.0), 100.0);
    }

    #[test]
    fn nice_step_rounds_to_1_2_5() {
        assert_eq!(nice_step(1.0), 1.0);
        assert_eq!(nice_step(3.0), 5.0);
        assert_eq!(nice_step(7.0), 10.0);
        assert_eq!(nice_step(150.0), 200.0);
    }

    #[test]
    fn format_time_picks_unit_and_decimals() {
        assert_eq!(format_time(500.0, 100.0), "500µs");
        assert_eq!(format_time(200_000.0, 100_000.0), "200ms");
        assert_eq!(format_time(5_000_000.0, 5_000_000.0), "5s");
        assert_eq!(format_time(-2_000_000.0, 1_000_000.0), "-2s");
        assert_eq!(format_time(-0.0, 5_000_000.0), "0s");
    }

    #[test]
    fn format_duration_is_compact() {
        assert_eq!(format_duration(500), "500µs");
        assert_eq!(format_duration(4_200), "4.20ms");
        assert_eq!(format_duration(2_500_000), "2.50s");
    }

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
    fn sample_at_interpolates_within_range() {
        let points = vec![
            CounterPoint {
                ts_us: 100,
                value: 10.0,
            },
            CounterPoint {
                ts_us: 200,
                value: 30.0,
            },
        ];
        // Exact hit on a point.
        assert_eq!(sample_at(&points, 100.0), Some(10.0));
        assert_eq!(sample_at(&points, 200.0), Some(30.0));
        // Midpoint linear interpolation.
        assert_eq!(sample_at(&points, 150.0), Some(20.0));
        // Out of range on either side yields no value.
        assert_eq!(sample_at(&points, 50.0), None);
        assert_eq!(sample_at(&points, 250.0), None);
        assert_eq!(sample_at(&[], 100.0), None);
        // Coincident timestamps resolve to the first, without dividing by 0.
        let dup = vec![
            CounterPoint {
                ts_us: 100,
                value: 10.0,
            },
            CounterPoint {
                ts_us: 100,
                value: 30.0,
            },
        ];
        assert_eq!(sample_at(&dup, 100.0), Some(10.0));
    }

    #[test]
    fn downsample_columns_keeps_sparse_runs_and_column_extremes() {
        // At most two points per whole-pixel column: passthrough.
        let sparse = vec![(0.2, 5.0), (0.8, 7.0), (3.0, 1.0), (10.5, 4.0)];
        assert_eq!(downsample_columns(sparse.iter().copied()), sparse);

        // A dense oscillation collapses to each column's extremes, in encounter
        // order, with x monotone and the global extremes preserved.
        let dense: Vec<(f32, f32)> = (0..1000)
            .map(|i| {
                let x = i as f32 / 100.0; // 100 points per column
                (x, (i as f32 * 0.71).sin() * 50.0)
            })
            .collect();
        let out = downsample_columns(dense.iter().copied());
        let columns = 10.0_f32;
        assert!(out.len() as f32 <= 2.0 * columns);
        let max = |v: &[(f32, f32)]| v.iter().map(|p| p.1).fold(f32::MIN, f32::max);
        let min = |v: &[(f32, f32)]| v.iter().map(|p| p.1).fold(f32::MAX, f32::min);
        assert_eq!(max(&out), max(&dense));
        assert_eq!(min(&out), min(&dense));
        assert!(out.windows(2).all(|w| w[0].0 <= w[1].0));
        // Per column both extremes survive.
        for col in 0..10 {
            let in_col = |v: &[(f32, f32)]| -> Vec<(f32, f32)> {
                v.iter().filter(|p| p.0.floor() == col as f32).copied().collect()
            };
            let (all, kept) = (in_col(&dense), in_col(&out));
            assert_eq!(max(&kept), max(&all));
            assert_eq!(min(&kept), min(&all));
        }

        // A flat dense column (min == max) collapses to a single vertex.
        let flat = (0..10).map(|i| (i as f32 / 100.0, 3.0));
        assert_eq!(downsample_columns(flat), vec![(0.0, 3.0)]);
    }

    #[test]
    fn unit_for_infers_from_lane_name() {
        assert_eq!(unit_for("CPU usage (Bazel)"), "cores");
        assert_eq!(unit_for("System memory usage"), "MB");
        assert_eq!(unit_for("Network Up (system)"), "Mbps");
        // "memory" wins over "pressure" by the inference order, so a pressure
        // lane reads as "%" only when it does not also mention memory.
        assert_eq!(unit_for("CPU pressure"), "%");
        assert_eq!(unit_for("Action count"), "");
        assert_eq!(unit_for("System load average"), "");
    }

    #[test]
    fn format_counter_uses_unit_and_thresholds() {
        assert_eq!(format_counter(3.5, "cores"), "3.50 cores");
        assert_eq!(format_counter(512.0, "MB"), "512 MB");
        // At and past 1024 MB, rescale to GB.
        assert_eq!(format_counter(1024.0, "MB"), "1.00 GB");
        assert_eq!(format_counter(2048.0, "MB"), "2.00 GB");
        assert_eq!(format_counter(12.5, "Mbps"), "12.50 Mbps");
        assert_eq!(format_counter(80.0, "%"), "80%");
        assert_eq!(format_counter(42.0, ""), "42.0");
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
