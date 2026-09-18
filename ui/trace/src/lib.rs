//! Loads a Bazel `--profile` trace (`.profile.gz` or `.json`) into
//! `bviz_trace_profile` and opens a window rendering its thread tracks as a
//! flame-chart timeline: time on the x axis, stacked call depth on the y axis,
//! with horizontal zoom/pan and vertical scroll.

mod chart;
mod counters;
mod flame_panel;
mod groups;
mod model;
mod rebuild;
mod regions;
mod stats_panel;
#[cfg(test)]
mod test_profile;
mod theme;
mod timeline;
mod track_label;
mod track_order;
mod tracks_menu;

use chart::{fit_px_per_us, format_duration, format_time, nice_step, span_seconds};

pub use flame_panel::FlamePanel;
use groups::{GroupChrome, group_label, lanes_headed_by_pool};
use model::{SourceReplaced, TraceModel};
use regions::{CountersStrip, DividerGrab, DividerPress, Regions, clamp_gutter_width};
pub use stats_panel::StatsPanel;
use theme::Palette;
use timeline::{HoveredSlice, Timeline};
use track_label::{pool_member_label, track_label};
use tracks_menu::TracksMenu;

use bviz_trace_layout::{LaneLayout, LaneMetrics, TimeSelection, TimeView};
use bviz_trace_profile::{Profile, Slice, TimeSpan};
use bviz_ui_components::{
    Axis, Button, DEFAULT_MIN_THUMB, Divider, DividerSide, DragEnd, Pending, SCROLLBAR_THICKNESS,
    Scrollbar, overlay_canvas, paint_tooltip_box, track_drag,
};
use bviz_ui_trace_args::{TraceArgs, TraceSource};
use bviz_ui_workspace::{
    Document, Panel, PanelEvent, PanelGroup, PanelKind, ViewKind, ViewSpec, Weight, document_label,
};
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, ContentMask, Context, Div, Edges, Element, ElementId,
    Entity, EventEmitter, FocusHandle, Focusable, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, KeyBinding, LayoutId, MouseDownEvent, Pixels, Point, Position, Render,
    ScrollHandle, SharedString, Style, Subscription, TextAlign, TransformationMatrix, Window, div,
    fill, point, prelude::*, px, relative, size,
};
use std::sync::Arc;

gpui::actions!(bviz_trace, [SelectAllTracks, ClearTimeSelection]);

pub fn register_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-a", SelectAllTracks, Some(TIMELINE_KEY_CONTEXT)),
        KeyBinding::new("escape", ClearTimeSelection, Some(TIMELINE_KEY_CONTEXT)),
    ]);
}

/// The key context of the trace panel, enclosing every sub-surface's.
const TRACE_KEY_CONTEXT: &str = "Trace";

/// The key context scoping the timeline's own bindings, narrowing
/// [`TRACE_KEY_CONTEXT`]. The timeline element carries it, so a key pressed in
/// the tracks menu's filter field types there instead of dispatching one of them.
const TIMELINE_KEY_CONTEXT: &str = "TraceTimeline";

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

/// Narrowest width a slice or flame node paints at; one that would render thinner
/// becomes a marker of this width.
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

/// Smallest height the flame section keeps when the divider is dragged down.
const FLAME_MIN_HEIGHT: f32 = 60.0;

/// Initial height of the counters strip, chosen before the profile is read and
/// so sized for a handful of counter lanes; the divider resizes it, and a
/// profile with more lanes than fit is scrolled through.
const DEFAULT_COUNTERS_HEIGHT: f32 = 160.0;

/// Share of the trace window the timeline takes, the flame graph and stats
/// sharing the pane below it.
const TIMELINE_SHARE: f32 = 0.65;

/// How far a press must travel before it is read as a drag rather than a click.
const MIN_DRAG_PX: f32 = 3.0;

/// Height a counter lane stands at: its header band over a chart area of the
/// height [`METRICS`] declares a counter chart to be. Every lane keeps it
/// however short the strip is, so a strip that cannot hold them all scrolls
/// instead of shrinking each plot until none of them can be read.
const COUNTER_LANE_HEIGHT: f32 = METRICS.header_height + METRICS.counter_height;

/// Height of the time ruler band at the top of the timeline.
const RULER_HEIGHT: f32 = 20.0;
/// Rough pixel spacing aimed for between ruler ticks; the actual step is rounded
/// to a 1/2/5 multiple near this.
const RULER_TARGET_TICK_PX: f64 = 120.0;

/// Font size of a phase-marker label.
const MARKER_LABEL_SIZE: f32 = 11.0;

/// Height of the band between the flame chart and the counters strip that a
/// press takes hold of to resize the strip, wide enough to aim a press at where
/// the line down its middle alone would be a pixel high.
const COUNTERS_DIVIDER_HEIGHT: f32 = 7.0;
const COUNTERS_DIVIDER_LINE: f32 = 1.0;
/// Height the band stands at while the strip is shut: room to hold the caret
/// that marks a shut side, and to aim a press at.
const COUNTERS_COLLAPSED_HEIGHT: f32 = 14.0;
/// How far past its resting edge the cursor must be pulled for the counters
/// strip to shut or spring open — past the floor its lanes need to shut, past
/// where it sits shut to open. A fixed distance, so the pull asked for is the
/// same whatever the floor, and far enough that neither happens under an
/// unsteady hand.
const COUNTERS_SNAP_SLOP: f32 = 16.0;

/// Height of a panel's toolbar band, giving its chips and controls a line to sit
/// on with room around them.
const PANEL_TOOLBAR_HEIGHT: f32 = 28.0;

/// Width of the open tracks visibility menu and the tallest its track list grows
/// before it scrolls.
const TRACKS_MENU_WIDTH: f32 = 280.0;
const TRACKS_MENU_MAX_LIST: f32 = 320.0;
/// Tracks a group lists in the menu before it starts folded, chosen to keep a
/// long group from burying the groups under it while a short one stays open.
const TRACKS_MENU_FOLD_ABOVE: usize = 20;

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

/// Radius of a crosshair value dot, font size of its value labels, and the gap
/// between the crosshair line and a value label.
const CROSSHAIR_DOT_RADIUS: f32 = 3.0;
const CROSSHAIR_VALUE_SIZE: f32 = 11.0;
const CROSSHAIR_LABEL_GAP: f32 = 5.0;

/// What a left-button drag is currently moving.
#[derive(Clone, Copy)]
enum Drag {
    None,
    /// A press on the divider, resizing the counters strip while it travels.
    Divider(DividerGrab),
    /// A press on the gutter handle resizes the gutter.
    Gutter,
}

/// Height of a group header band and of a group's "show more" row.
const GROUP_BAND_HEIGHT: f32 = 18.0;
/// Side of a group band's fold and show-all icons.
const GROUP_ICON: f32 = 12.0;
/// Indent of a group band's gutter label, clearing the fold icon at the gutter's
/// leading edge.
const GROUP_LABEL_INDENT: f32 = GROUP_ICON + SLICE_TEXT_PAD;
/// Indent of a pool header's gutter label, one nesting step inside the column
/// its group band's label starts at.
const FAMILY_LABEL_INDENT: f32 = GROUP_LABEL_INDENT + 10.0;
/// Least gap kept between a header row's name and the count at its trailing edge.
const HEADER_COUNT_GAP: f32 = 6.0;
/// Lanes shown per group before the rest fold behind the band's show-all toggle.
const TOP_N_PER_GROUP: usize = 12;

pub(crate) type Source = bviz_ui_components::Source<Arc<Profile>>;

/// The trace document's timeline: the flame chart with its gutter, ruler and
/// counters strip, the divider that resizes the strip, and the tracks menu over
/// them. The chart itself is drawn by [`Timeline`]; this view holds the
/// splits the two of them are laid out by and reads the loaded document from the
/// shared [`TraceModel`].
pub struct TracePanel {
    /// Names the document shown, labelling this panel's tab.
    title: SharedString,
    /// The one instance the document's panels each hold a handle to.
    model: Entity<TraceModel>,
    timeline: Entity<Timeline>,
    drag: Drag,
    /// The counters strip as the divider has left it; clamped on use.
    counters: CountersStrip,
    /// Width of the left label gutter, set by its drag handle; clamped on use.
    gutter_width: f32,
    tracks_menu: Entity<TracksMenu>,
    /// Drops the view-local state a freshly loaded document leaves addressing
    /// nothing: the timeline's scroll and cached scene, and the tracks-menu fold
    /// state the model does not seed.
    _source_replaced: Subscription,
}

impl TracePanel {
    pub(crate) fn new(
        title: SharedString,
        model: Entity<TraceModel>,
        cx: &mut Context<Self>,
    ) -> Self {
        let source_replaced = cx.subscribe(&model, |panel, _, _: &SourceReplaced, cx| {
            panel.timeline.update(cx, |t, _| t.reset_for_new_profile());
            panel
                .tracks_menu
                .update(cx, |m, _| m.reset_for_new_profile());
            panel.invalidate_timeline(cx);
        });
        // A view that reaches back here takes a weak handle, so this panel's own
        // handle to it is what keeps it alive.
        let timeline = {
            let shell = cx.entity().downgrade();
            let model = model.clone();
            cx.new(|cx| Timeline::new(model, shell, cx))
        };
        let tracks_menu = {
            let shell = cx.entity().downgrade();
            let model = model.clone();
            cx.new(|cx| TracksMenu::new(model, shell, cx))
        };
        Self {
            title,
            model,
            timeline,
            drag: Drag::None,
            counters: CountersStrip::Open(DEFAULT_COUNTERS_HEIGHT),
            gutter_width: DEFAULT_GUTTER_WIDTH,
            tracks_menu,
            _source_replaced: source_replaced,
        }
    }

    /// Rebuilds the timeline's cached scene and re-renders this view (so the
    /// divider and gutter handle reposition). A change to the timeline content or
    /// to the strip/gutter layout uses this.
    fn invalidate_timeline(&self, cx: &mut Context<Self>) {
        self.timeline.update(cx, |t, cx| t.invalidate_canvas(cx));
        cx.notify();
    }

    /// The whole trace body: the [`Timeline`] filling the viewport (it paints
    /// the flame chart and counter plots and owns the timeline's mouse/scroll/
    /// hover wiring), a drag overlay that keeps the strip-resize and gutter drags
    /// tracking the cursor, then the divider, the gutter handle, and the tracks
    /// menu over it.
    fn trace_body(&self, palette: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let timeline = div()
            .absolute()
            .inset_0()
            .size_full()
            .child(self.timeline.clone());
        // The strip-resize and gutter drags live on this view; while one runs, a
        // full-bounds canvas re-installs their window-global handlers each frame.
        // It is non-interactive, so it does not intercept the timeline's presses.
        let drag_overlay = {
            let entity = cx.entity();
            overlay_canvas(move |_, window, cx| {
                if matches!(entity.read(cx).drag, Drag::Divider(_) | Drag::Gutter) {
                    track_drag(window, &entity, on_drag, end_drag);
                }
            })
        };

        let inputs = self.region_inputs(cx);
        let mut overlays = RegionOverlays::new(inputs);
        // A profile with no counters has no strip, and so nothing to resize.
        if inputs.n_counters > 0 {
            let panel = cx.entity();
            overlays = overlays.child(move |regions, _, window| {
                (
                    counters_divider(regions, palette, &panel, window).into_any_element(),
                    regions.divider_rect(),
                )
            });
        }
        // The gutter handle spans the full height and so crosses both the chart
        // and the strip, and must take a press over either: it is placed last,
        // and hit-testing follows paint order.
        let panel = cx.entity();
        let overlays = overlays.child(move |regions, body, window| {
            (
                gutter_handle(&panel, window).into_any_element(),
                gutter_handle_rect(regions, body),
            )
        });
        div()
            .id("trace-viewport")
            .relative()
            .size_full()
            .overflow_hidden()
            .child(timeline)
            .child(drag_overlay)
            .child(overlays)
            .child(self.tracks_menu.clone())
    }

    /// [`Regions`] for the current viewport (captured by the timeline), strip and
    /// gutter sizes, the loaded profile's counter count, and the flame lanes'
    /// total height.
    fn region_inputs(&self, cx: &App) -> RegionInputs {
        let model = self.model.read(cx);
        RegionInputs {
            n_counters: match model.source() {
                Source::Loaded(p) => p.counters().len(),
                _ => 0,
            },
            counters: self.counters,
            gutter_width: self.gutter_width,
            flame_total: model.lane_layout().map(|l| l.total_height).unwrap_or(0.0),
        }
    }

    /// The split as the timeline last laid it out, which the divider's presses
    /// are aimed at and its drags measured against.
    fn regions(&self, cx: &App) -> Regions {
        self.region_inputs(cx)
            .split(self.timeline.read(cx).viewport())
    }
}

/// A thin overlay strip at the gutter's right edge that begins a width drag; a
/// double-click resets the gutter to its default width.
fn gutter_handle(panel: &Entity<TracePanel>, window: &Window) -> Divider {
    Divider::new(
        "trace-gutter-handle",
        gpui::Axis::Horizontal,
        window.listener_for(panel, |this, e: &MouseDownEvent, _, cx| {
            if e.click_count == 2 {
                this.gutter_width = DEFAULT_GUTTER_WIDTH;
            } else {
                this.drag = Drag::Gutter;
            }
            cx.stop_propagation();
            this.invalidate_timeline(cx);
        }),
    )
}

/// The band between the flame chart and the counters strip. A press on it begins
/// a strip-resize drag; a double-click resets the strip to its default height.
fn counters_divider(
    regions: &Regions,
    palette: Palette,
    panel: &Entity<TracePanel>,
    window: &Window,
) -> Divider {
    let divider = Divider::new(
        "trace-counters-divider",
        gpui::Axis::Vertical,
        window.listener_for(panel, |this, e: &MouseDownEvent, _, cx| {
            let counters_top = this.regions(cx).counters_top;
            match this
                .counters
                .press(e.click_count, f32::from(e.position.y), counters_top)
            {
                DividerPress::Reset(strip) => {
                    this.counters = strip;
                    this.drag = Drag::None;
                }
                DividerPress::Grab(grab) => this.drag = Drag::Divider(grab),
            }
            cx.stop_propagation();
            this.invalidate_timeline(cx);
        }),
    )
    .line(px(COUNTERS_DIVIDER_LINE), palette.divider);
    match regions.shows_counters() {
        true => divider,
        false => divider.collapsed(DividerSide::End, palette.muted_label),
    }
}

/// The chip's label for the `window` a shown aggregate covers, of a build
/// spanning `span_us`. Absent while no aggregate is shown, or the shown one
/// covers the whole build.
fn time_window_summary(window: Option<&TimeSelection>, span_us: i64) -> Option<String> {
    let window = window.filter(|w| !w.is_empty())?;
    let mut text = format!(
        "Window: {} of {}",
        format_duration(window.total_us()),
        format_duration(span_us)
    );
    let regions = window.len();
    if regions > 1 {
        text.push_str(&format!(" · {regions} regions"));
    }
    Some(text)
}

/// The window the flame graph and stats cover, as a chip carrying the control
/// that widens the next aggregate back to the whole timeline. Absent while no
/// aggregate is shown, or the shown one covers the whole build.
pub(crate) fn time_window_chip(
    model: &Entity<TraceModel>,
    palette: Palette,
    cx: &App,
) -> Option<Div> {
    let summary = {
        let m = model.read(cx);
        let Source::Loaded(profile) = m.source() else {
            return None;
        };
        let span = profile.span();
        let shown = m.aggregates();
        time_window_summary(
            shown.as_deref().map(|a| &a.window),
            span.end_us - span.start_us,
        )?
    };
    let clearing = model.clone();
    Some(
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .my_1()
            .pl_2()
            .flex_shrink_0()
            .rounded_md()
            .bg(palette.selection)
            .border_1()
            .border_color(palette.accent)
            .text_size(px(STATS_TEXT_SIZE))
            .text_color(palette.label)
            .child(summary)
            .child(
                Button::new("trace-time-window-clear", move |_, _, cx| {
                    clearing.update(cx, |m, cx| {
                        m.set_time_selection(TimeSelection::default(), cx);
                    });
                })
                .ghost()
                .compact()
                .icon("x"),
            ),
    )
}

/// The bar a panel of the trace document carries its chips and controls on.
pub(crate) fn panel_toolbar(palette: Palette) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .flex_shrink_0()
        .gap_4()
        .px_2()
        .h(px(PANEL_TOOLBAR_HEIGHT))
        .overflow_hidden()
        .bg(palette.header_band)
        .border_b_1()
        .border_color(palette.divider)
}

/// What the window chrome says about the loaded document, the same from whichever
/// of its panels is on show.
pub(crate) fn profile_status(model: &Entity<TraceModel>, cx: &App) -> SharedString {
    model
        .read(cx)
        .source()
        .status("No profile", |p| {
            let slices: usize = p.threads().iter().map(|t| t.slices.len()).sum();
            format!(
                "{} threads · {} slices · {:.3}s",
                p.threads().len(),
                slices,
                span_seconds(p.span()),
            )
        })
        .into()
}

impl EventEmitter<PanelEvent> for TracePanel {}

impl Focusable for TracePanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.timeline.focus_handle(cx)
    }
}

impl Panel for TracePanel {
    fn kind() -> PanelKind {
        PanelKind {
            view: ViewKind::new("trace"),
            label: "Timeline",
            key_context: TRACE_KEY_CONTEXT,
        }
    }

    fn title(&self, _cx: &App) -> SharedString {
        self.title.clone()
    }

    fn status(&self, cx: &App) -> Option<SharedString> {
        Some(profile_status(&self.model, cx))
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        match active {
            true => self.focus_handle(cx).focus(window, cx),
            // The tooltip and crosshair anchor to a cursor that is no longer over
            // the chart.
            false => self.timeline.update(cx, |t, _| t.clear_hover()),
        }
    }
}

impl Render for TracePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = bviz_ui_theme::theme(window, cx);
        let palette = Palette::from_theme(&theme);
        // Take the placeholder before the `&mut self` build calls below, so the
        // model borrow it is read through ends first.
        let placeholder = self.model.read(cx).source().placeholder(&theme.colors);
        let chrome = div()
            .key_context(Self::kind().key_context)
            .track_focus(&self.focus_handle(cx))
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden();
        match placeholder {
            Some(placeholder) => chrome.child(placeholder),
            None => chrome.child(self.trace_body(palette, cx)),
        }
    }
}

/// What [`Regions`] needs beyond the bounds it splits.
#[derive(Clone, Copy)]
struct RegionInputs {
    n_counters: usize,
    counters: CountersStrip,
    gutter_width: f32,
    flame_total: f32,
}

impl RegionInputs {
    fn split(&self, bounds: Bounds<Pixels>) -> Regions {
        Regions::new(
            bounds,
            self.n_counters,
            self.counters,
            self.gutter_width,
            self.flame_total,
        )
    }
}

/// Builds an overlay against the split it belongs to and the body's own bounds,
/// giving the element together with the rect it stands in, so what an overlay
/// shows and where it sits are read from one split.
type OverlayInRegions =
    Box<dyn Fn(&Regions, Bounds<Pixels>, &Window) -> (AnyElement, Bounds<Pixels>)>;

fn gutter_handle_rect(r: &Regions, body: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        point(
            px(r.content_left - GUTTER_HANDLE_WIDTH / 2.0),
            body.origin.y,
        ),
        size(px(GUTTER_HANDLE_WIDTH), body.size.height),
    )
}

/// The trace body's overlays — the counters divider and the gutter handle —
/// laid out from the body's own bounds. Each belongs at a pixel offset within the
/// [`Regions`] split, which is defined only once the body has a size, so the
/// split is resolved here during prepaint and each child built against it. A
/// size measured elsewhere is known only after a paint, and would put both what
/// the children show and where they sit a frame behind the body they overlay.
struct RegionOverlays {
    inputs: RegionInputs,
    children: Vec<OverlayInRegions>,
}

impl RegionOverlays {
    fn new(inputs: RegionInputs) -> Self {
        Self {
            inputs,
            children: Vec::new(),
        }
    }

    fn child(
        mut self,
        build: impl Fn(&Regions, Bounds<Pixels>, &Window) -> (AnyElement, Bounds<Pixels>) + 'static,
    ) -> Self {
        self.children.push(Box::new(build));
        self
    }
}

impl IntoElement for RegionOverlays {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for RegionOverlays {
    type RequestLayoutState = ();
    type PrepaintState = Vec<AnyElement>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        // The overlays cover the body without displacing the timeline beneath them.
        let style = Style {
            position: Position::Absolute,
            inset: Edges::all(px(0.).into()),
            size: size(relative(1.).into(), relative(1.).into()),
            ..Default::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<AnyElement> {
        let regions = self.inputs.split(bounds);
        let mut children = Vec::with_capacity(self.children.len());
        for build in &self.children {
            let (mut child, rect) = build(&regions, bounds, window);
            let space = size(
                AvailableSpace::Definite(rect.size.width),
                AvailableSpace::Definite(rect.size.height),
            );
            child.prepaint_as_root(rect.origin, space, window, cx);
            children.push(child);
        }
        children
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        children: &mut Vec<AnyElement>,
        window: &mut Window,
        cx: &mut App,
    ) {
        for child in children {
            child.paint(window, cx);
        }
    }
}

/// Routes an in-progress drag to its target: resizing the counters strip or the
/// gutter. The flame-chart timeline's, flame-graph panel's, and tracks menu's own
/// drags are routed inside [`Timeline`], [`FlamePanel`], and [`TracksMenu`].
fn on_drag(view: &mut TracePanel, position: Point<Pixels>, cx: &mut Context<TracePanel>) {
    match view.drag {
        Drag::Divider(mut grab) => {
            let vp = view.timeline.read(cx).viewport();
            let vh = f32::from(vp.size.height);
            let counters_bottom = f32::from(vp.origin.y) + vh;
            view.counters = grab.dragged_to(f32::from(position.y), counters_bottom, vh);
            view.drag = Drag::Divider(grab);
        }
        Drag::Gutter => {
            let vp = view.timeline.read(cx).viewport();
            view.gutter_width = clamp_gutter_width(
                f32::from(position.x) - f32::from(vp.origin.x),
                f32::from(vp.size.width),
            );
        }
        Drag::None => {}
    }
    // Strip and gutter resizes move the timeline's split, so they rebuild it.
    view.invalidate_timeline(cx);
}

fn end_drag(
    view: &mut TracePanel,
    _: Point<Pixels>,
    _: DragEnd,
    _: &mut Window,
    cx: &mut Context<TracePanel>,
) {
    if let Drag::Divider(grab) = view.drag {
        view.counters = grab.released(view.counters);
    }
    view.drag = Drag::None;
    view.invalidate_timeline(cx);
}

/// The scroll offset a thumb at `pos` (grabbed at `grab`) maps to, or `None`
/// when the bar is not currently painted.
fn thumb_offset(bar: Option<Scrollbar>, pos: Point<Pixels>, grab: f32) -> Option<f32> {
    Some(bar?.offset_at(pos, grab))
}

/// The offset a view holding `held` draws at, bounded by the end of its content
/// at `ceiling`.
fn drawn_scroll(held: f32, ceiling: f32) -> f32 {
    held.clamp(0.0, ceiling)
}

/// Where a gesture asking for `request` leaves a view holding `held`, and `None`
/// for a gesture that moves nothing on screen. A held position past the ceiling
/// — a place the content has shrunk away from, which comes back when it grows
/// again — draws at the ceiling, so a further gesture into the bound asks for
/// the pixels already shown: taking it would spend the held position and invent
/// an overscroll past the end.
fn scroll_request(held: f32, ceiling: f32, request: f32) -> Option<f32> {
    let target = drawn_scroll(request, ceiling);
    (target != drawn_scroll(held, ceiling)).then_some(target)
}

/// Scrolls a list `handle` so a downward thumb `offset` maps to a matching
/// negative vertical scroll.
fn set_list_thumb(handle: &ScrollHandle, offset: f32) {
    let mut o = handle.offset();
    o.y = px(-offset);
    handle.set_offset(o);
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
        // Continue the marker line through the counters strip, which shares the
        // time axis, so a phase boundary reads across both.
        if show_counters {
            window.paint_quad(fill(
                Bounds::new(
                    point(px(lx), px(regions.counters_top)),
                    size(px(1.), px(regions.counters_h())),
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

/// The timeline's scroll bars, each `None` where its content fits and so has
/// nothing to scroll.
#[derive(Clone, Copy, Default)]
struct TimelineBars {
    /// Maps to the flame lanes' pixel scroll.
    flame: Option<Scrollbar>,
    /// Maps to the view's start time within the profile span.
    time: Option<Scrollbar>,
    /// Maps to the counters strip's pixel scroll.
    counters: Option<Scrollbar>,
}

impl TimelineBars {
    fn paint(&self, palette: &Palette, window: &mut Window) {
        for bar in [self.flame, self.time, self.counters].into_iter().flatten() {
            bar.paint(palette.scroll.track, palette.scroll.thumb, window);
        }
    }
}

/// Places the timeline's bars for its view state. The two vertical bars share
/// the column reserved at the content's right edge, each over its own section.
fn scrollbar_geom(
    span: TimeSpan,
    tv: TimeView,
    flame_total: f32,
    regions: Regions,
    scroll_y: f32,
    counter_scroll: f32,
) -> TimelineBars {
    let flame_h = regions.flame_h();
    let vbar_left = px(regions.content_right - SCROLLBAR_THICKNESS);

    let span_us = (span.end_us - span.start_us) as f32;
    let visible_us = regions.time_w / tv.px_per_us as f32;
    // Pan and the wheel reach EDGE_MARGIN_PX of empty space past each span end,
    // so the bar spans that padded range and its offset is measured from the
    // padded start, letting the thumb reach the same margins.
    let margin_us = EDGE_MARGIN_PX / tv.px_per_us as f32;

    let flame = Scrollbar::new(
        Axis::Vertical,
        Bounds::new(
            point(vbar_left, px(regions.flame_top)),
            size(px(SCROLLBAR_THICKNESS), px(flame_h)),
        ),
        flame_total,
        flame_h,
        scroll_y,
        DEFAULT_MIN_THUMB,
    );
    let counters = Scrollbar::new(
        Axis::Vertical,
        Bounds::new(
            point(vbar_left, px(regions.counters_top)),
            size(px(SCROLLBAR_THICKNESS), px(regions.counters_h())),
        ),
        regions.counters_total(),
        regions.counters_h(),
        counter_scroll,
        DEFAULT_MIN_THUMB,
    );
    // The time horizontal track runs the full time axis along the flame lanes'
    // bottom edge, ending left of the reserved vertical-bar column. Rounding the
    // extents to microseconds can leave a view sitting on the zoom floor a hair
    // wider than its own span, so the bar is gated on the floor itself.
    let time = match tv.px_per_us > fit_px_per_us(span, regions.time_w, EDGE_MARGIN_PX) {
        true => Scrollbar::new(
            Axis::Horizontal,
            Bounds::new(
                point(
                    px(regions.content_left),
                    px(regions.flame_bottom - SCROLLBAR_THICKNESS),
                ),
                size(px(regions.time_w), px(SCROLLBAR_THICKNESS)),
            ),
            span_us + 2.0 * margin_us,
            visible_us,
            tv.start_us as f32 - (span.start_us as f32 - margin_us),
            DEFAULT_MIN_THUMB,
        ),
        false => None,
    };

    TimelineBars {
        flame,
        time,
        counters,
    }
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
/// the counters strip — over any content that panned or scrolled under it, then
/// the names left-aligned within it, each clipped to the band it scrolls in:
/// thread names track the flame lanes' vertical scroll, counter names the
/// strip's.
#[allow(clippy::too_many_arguments)]
fn paint_gutter(
    profile: &Profile,
    layout: &LaneLayout,
    chrome: &GroupChrome,
    regions: Regions,
    gutter_width: f32,
    scroll_y: f32,
    counter_scroll: f32,
    show_counters: bool,
    selected: &[bool],
    shared_names: &[bool],
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let gutter_left = regions.content_left - gutter_width;
    let gutter_bottom = regions.counters_bottom;
    paint_gutter_frame(
        regions,
        gutter_left,
        gutter_width,
        gutter_bottom,
        palette,
        window,
    );

    let font = bviz_ui_assets::text_metrics(window).font;
    let fs = px(GUTTER_LABEL_SIZE);
    let avail = gutter_width - 2.0 * SLICE_TEXT_PAD;
    let g = GutterPaint {
        left: gutter_left,
        width: gutter_width,
        avail,
        font,
        fs,
        palette,
    };

    // Thread and group labels scroll with the lanes, so clip them to the flame
    // region.
    let flame_mask = ContentMask {
        bounds: Bounds::new(
            point(px(gutter_left), px(regions.flame_top)),
            size(px(gutter_width), px(regions.flame_h())),
        ),
    };
    let lanes = GutterLanes {
        profile,
        chrome,
        selected,
        shared_names,
    };
    window.with_content_mask(Some(flame_mask), |window| {
        paint_thread_labels(&g, &lanes, layout, regions, scroll_y, window, cx);
        paint_group_bands(&g, chrome, regions, scroll_y, window, cx);
        paint_family_bands(&g, chrome, regions, scroll_y, window, cx);
    });

    if show_counters {
        let counters_mask = ContentMask {
            bounds: Bounds::new(
                point(px(gutter_left), px(regions.counters_top)),
                size(px(gutter_width), px(regions.counters_h())),
            ),
        };
        window.with_content_mask(Some(counters_mask), |window| {
            paint_counter_labels(&g, profile, regions, counter_scroll, window, cx);
        });
    }
}

/// Fills the gutter's opaque background and draws a divider at its right edge,
/// under the drag handle, spanning the full height so the resizable line reads as
/// one across the flame chart and the counters strip.
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
fn paint_gutter_label(g: &GutterPaint, name: &str, ty: f32, window: &mut Window, cx: &mut App) {
    let gutter_left = g.left;
    let avail = g.avail;
    let font = &g.font;
    let fs = g.fs;
    let palette = g.palette;
    let line = bviz_ui_components::shape_truncated(
        name,
        avail,
        font.clone(),
        fs,
        palette.label.into(),
        window,
    );
    let _ = line.paint(
        point(px(gutter_left + SLICE_TEXT_PAD), px(ty)),
        fs,
        TextAlign::Left,
        None,
        window,
        cx,
    );
}

/// The state a flame lane's gutter row is drawn from.
struct GutterLanes<'a> {
    profile: &'a Profile,
    chrome: &'a GroupChrome,
    selected: &'a [bool],
    shared_names: &'a [bool],
}

/// Paints each thread's label at its flame lane, scrolled with the lanes and
/// centred on the part of the lane the flame region shows, so a lane taller than
/// that region keeps its name on screen. A selected lane's row carries a faint
/// accent tint and a leading accent bar, so which tracks feed the flame graph
/// reads at a glance.
fn paint_thread_labels(
    g: &GutterPaint,
    lanes: &GutterLanes,
    layout: &LaneLayout,
    regions: Regions,
    scroll_y: f32,
    window: &mut Window,
    cx: &mut App,
) {
    let gutter_left = g.left;
    let gutter_width = g.width;
    let palette = g.palette;
    for (lane, headed) in layout.lanes.iter().zip(lanes_headed_by_pool(lanes.chrome)) {
        let top = regions.flame_top + lane.top - scroll_y;
        if top + lane.height < regions.flame_top || top > regions.flame_bottom {
            continue;
        }
        if lanes.selected.get(lane.track).copied().unwrap_or(false) {
            window.paint_quad(fill(
                Bounds::new(
                    point(px(gutter_left), px(top)),
                    size(px(gutter_width), px(lane.height)),
                ),
                palette.selection,
            ));
            window.paint_quad(fill(
                Bounds::new(
                    point(px(gutter_left), px(top)),
                    size(px(2.0), px(lane.height)),
                ),
                palette.accent,
            ));
        }
        let seen_top = top.max(regions.flame_top);
        let seen_bottom = (top + lane.height).min(regions.flame_bottom);
        let ty = (seen_top + seen_bottom - GUTTER_LABEL_SIZE) / 2.0;
        let label = if headed {
            pool_member_label(lanes.profile, lanes.shared_names, lane.track)
        } else {
            track_label(lanes.profile, lanes.shared_names, lane.track)
        };
        paint_gutter_label(g, &label, ty, window, cx);
    }
}

/// How many of a header row's lanes the chart draws of those it holds — the
/// total alone where it draws none.
fn header_count(shown: Option<usize>, total: usize) -> String {
    match shown {
        Some(shown) => format!("{shown}/{total}"),
        None => format!("{total}"),
    }
}

/// A header row of the flame chart's gutter.
struct HeaderRow<'a> {
    /// Where the row sits in the chart, before the lane scroll moves it.
    top: f32,
    name: &'a str,
    count: &'a str,
    /// Width held at the row's leading edge, ahead of the name.
    indent: f32,
    /// Width held at the row's trailing edge for an icon drawn over it.
    reserved: f32,
    color: Hsla,
}

/// Paints a header row: its count set against the gutter's trailing edge, clear
/// of what the row reserves there, and its name truncated into the width left
/// between the two. Returns the row's top in screen space, and nothing where the
/// flame region scrolls the row out of view.
fn paint_header_row(
    g: &GutterPaint,
    regions: Regions,
    scroll_y: f32,
    row: HeaderRow,
    window: &mut Window,
    cx: &mut App,
) -> Option<f32> {
    let top = regions.flame_top + row.top - scroll_y;
    if top + GROUP_BAND_HEIGHT < regions.flame_top || top > regions.flame_bottom {
        return None;
    }
    let ty = top + (GROUP_BAND_HEIGHT - GUTTER_LABEL_SIZE) / 2.0;
    let shape = |text: &str, avail: f32, window: &mut Window| {
        bviz_ui_components::shape_truncated(text, avail, g.font.clone(), g.fs, row.color, window)
    };
    let count = shape(row.count, g.avail - row.indent - row.reserved, window);
    let count_width = f32::from(count.width());
    let name_avail = g.avail - row.indent - row.reserved - count_width - HEADER_COUNT_GAP;
    let name = shape(row.name, name_avail, window);
    let paint = |line: gpui::ShapedLine, x: f32, window: &mut Window, cx: &mut App| {
        let _ = line.paint(
            point(px(x), px(ty)),
            g.fs,
            TextAlign::Left,
            None,
            window,
            cx,
        );
    };
    paint(name, g.left + SLICE_TEXT_PAD + row.indent, window, cx);
    let count_x = g.left + g.width - SLICE_TEXT_PAD - row.reserved - count_width;
    paint(count, count_x, window, cx);
    Some(top)
}

/// Paints each group header band's caret, class name, and shown/total lane
/// count; a capped, expanded group shows a caret-double icon at the gutter's right
/// edge — down to show every lane, up to show only the leading ones.
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
    let palette = g.palette;
    for band in &chrome.bands {
        let show_icon = band.capped && !band.collapsed;
        let count = header_count((!band.collapsed).then_some(band.shown), band.total);
        let Some(top) = paint_header_row(
            g,
            regions,
            scroll_y,
            HeaderRow {
                top: band.top,
                name: group_label(band.class),
                count: &count,
                indent: GROUP_LABEL_INDENT,
                reserved: if show_icon {
                    GROUP_ICON + SLICE_TEXT_PAD
                } else {
                    0.0
                },
                color: palette.label.into(),
            },
            window,
            cx,
        ) else {
            continue;
        };
        paint_band_icon(
            if band.collapsed {
                "caret-right"
            } else {
                "caret-down"
            },
            gutter_left + SLICE_TEXT_PAD,
            top,
            palette.label.into(),
            window,
            cx,
        );
        if show_icon {
            let name = if band.show_all {
                "caret-double-up"
            } else {
                "caret-double-down"
            };
            paint_band_icon(
                name,
                gutter_left + gutter_width - GROUP_ICON - SLICE_TEXT_PAD,
                top,
                palette.marker.into(),
                window,
                cx,
            );
        }
    }
}

/// Paints one of a group band's icons, square and vertically centred in the band
/// whose screen-space top is `top`.
fn paint_band_icon(
    name: &str,
    left: f32,
    top: f32,
    color: Hsla,
    window: &mut Window,
    cx: &mut App,
) {
    let iy = top + (GROUP_BAND_HEIGHT - GROUP_ICON) / 2.0;
    let _ = window.paint_svg(
        Bounds::new(
            point(px(left), px(iy)),
            size(px(GROUP_ICON), px(GROUP_ICON)),
        ),
        SharedString::from(format!("assets/regular/{name}.svg")),
        None,
        TransformationMatrix::default(),
        color,
        cx,
    );
}

/// Paints each pool header's name and shown/total lane count, indented under the
/// group band it hangs from.
fn paint_family_bands(
    g: &GutterPaint,
    chrome: &GroupChrome,
    regions: Regions,
    scroll_y: f32,
    window: &mut Window,
    cx: &mut App,
) {
    for family in &chrome.families {
        let count = header_count(Some(family.shown), family.total);
        paint_header_row(
            g,
            regions,
            scroll_y,
            HeaderRow {
                top: family.top,
                name: &family.name,
                count: &count,
                indent: FAMILY_LABEL_INDENT,
                reserved: 0.0,
                color: g.palette.muted_label.into(),
            },
            window,
            cx,
        );
    }
}

/// Paints each counter's name at its strip lane's chart baseline, so it
/// associates with low values rather than floating at the top.
fn paint_counter_labels(
    g: &GutterPaint,
    profile: &Profile,
    regions: Regions,
    counter_scroll: f32,
    window: &mut Window,
    cx: &mut App,
) {
    for gl in regions.counter_lanes(counter_scroll) {
        let lane_bottom = gl.top + METRICS.header_height + gl.chart_h;
        let ty = lane_bottom - GUTTER_LABEL_SIZE - SLICE_TEXT_PAD;
        paint_gutter_label(
            g,
            profile.str(profile.counters()[gl.lane].name),
            ty,
            window,
            cx,
        );
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
    let fill = palette.role_fill(slice.role);
    bviz_ui_components::paint_labeled_box(
        rect,
        fill,
        profile.str(slice.name),
        bviz_ui_theme::label_on(fill),
        palette.label_box(),
        window,
        cx,
    );
}

/// Paints a tooltip panel near `at` with the slice's name, its category and
/// duration, and its args, one per line. Kept inside the viewport.
fn paint_tooltip(
    hovered: &HoveredSlice,
    at: Point<Pixels>,
    bounds: Bounds<Pixels>,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let (profile, slice, detail) = (hovered.profile(), hovered.slice(), hovered.detail());
    let mut lines = vec![
        profile.str(slice.name).to_string(),
        format!(
            "{} · {}",
            profile.str(detail.category),
            format_duration(slice.dur_us)
        ),
    ];
    if let Some(self_us) = hovered.self_us() {
        lines.push(format!("self: {}", format_duration(self_us)));
    }
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
    paint_tooltip_box(&lines, at, bounds, palette.tooltip, window, cx);
}

/// A profile being decoded and parsed.
pub struct Load {
    profile: Pending<Profile>,
    title: SharedString,
}

impl From<TraceSource> for Load {
    fn from(source: TraceSource) -> Self {
        let title = document_label(&source.path);
        Self {
            profile: Pending::start(move || {
                Profile::from_path(&source.path).map_err(|e| e.to_string())
            }),
            title,
        }
    }
}

/// The document rendering the profile `load` reads, as the timeline above the
/// flame graph and stats that aggregate whatever tracks it has selected.
pub fn open(load: impl Into<Load>, _window: &mut Window, cx: &mut App) -> Document {
    let load = load.into();
    let label = load.title;
    let model = cx.new(|cx| TraceModel::reading(load.profile, cx));
    let views = PanelGroup::split(
        gpui::Axis::Vertical,
        TIMELINE_SHARE,
        PanelGroup::one(ViewSpec::new({
            let (label, model) = (label.clone(), model.clone());
            move |cx| cx.new(|cx| TracePanel::new(label.clone(), model.clone(), cx))
        })),
        PanelGroup::tabs(
            ViewSpec::new({
                let model = model.clone();
                move |cx| cx.new(|cx| FlamePanel::new(model.clone(), cx))
            }),
            vec![ViewSpec::new(move |cx| {
                cx.new(|cx| StatsPanel::new(model.clone(), cx))
            })],
        ),
    )
    .with_weight(Weight::CHART);
    Document::new(label, views)
}

/// Renders a Bazel `--profile` trace as a flame-chart timeline. Each profile
/// given opens a timeline of its own.
#[derive(Parser)]
#[command(name = "trace", version, about)]
struct Cli {
    #[command(flatten)]
    trace: TraceArgs,
}

/// Parses `--profile` and opens the trace-viewer window.
pub fn run() {
    bviz_alloc::retain_freed_memory_across_load();
    let mut loads = Cli::parse().trace.sources().map(Load::from);
    let Some(first) = loads.next() else {
        Cli::command()
            .error(ErrorKind::MissingRequiredArgument, "--profile is required")
            .exit()
    };
    bviz_ui_workspace::run_panels(
        "bviz trace",
        size(px(1280.), px(820.)),
        first,
        loads.collect(),
        open,
        &[register_keys],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window tall enough that the strip stands at the height it is opened to,
    /// split for a profile with `n` counters and lanes short enough that the
    /// flame region never asks for a bar of its own.
    fn counters_split(n: usize, strip: CountersStrip) -> Regions {
        let window = Bounds::new(point(px(0.0), px(0.0)), size(px(1000.0), px(800.0)));
        Regions::new(window, n, strip, DEFAULT_GUTTER_WIDTH, 0.0)
    }

    /// The counters strip's bar for a split scrolled to its top.
    fn counter_bar(regions: Regions) -> Option<Scrollbar> {
        let span = TimeSpan {
            start_us: 0,
            end_us: 1_000,
        };
        let tv = TimeView {
            start_us: 0.0,
            px_per_us: 1.0,
        };
        scrollbar_geom(span, tv, 0.0, regions, 0.0, 0.0).counters
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

    /// gpui's `Bounds::contains` documents inclusive edges and implements
    /// half-open ones; the hover regions are built on the implementation, so a
    /// pin that resolved the contradiction the other way would move which pixel
    /// column and row show a tooltip.
    #[test]
    fn the_flame_region_ends_left_of_the_bar_column() {
        let window = Bounds::new(point(px(0.0), px(0.0)), size(px(1000.0), px(600.0)));
        // Lanes taller than any region this window can hold, so the split
        // reserves a column for the vertical bar.
        let flame_total = 2.0 * f32::from(window.size.height);
        let regions = Regions::new(
            window,
            0,
            CountersStrip::Open(DEFAULT_COUNTERS_HEIGHT),
            DEFAULT_GUTTER_WIDTH,
            flame_total,
        );
        let right = regions.content_left + regions.time_w;
        assert_eq!(right, regions.content_right - SCROLLBAR_THICKNESS);

        let at = |x, y| point(px(x), px(y));
        // The gutter's edge, the ruler's bottom, and the last column left of the
        // bar lie inside the region.
        assert!(regions.in_flame(at(regions.content_left, regions.flame_top)));
        assert!(regions.in_flame(at(right - 1.0, regions.flame_bottom - 1.0)));
        // The bar's own column and the row below the lanes lie past it.
        assert!(!regions.in_flame(at(right, regions.flame_top)));
        assert!(!regions.in_flame(at(regions.content_right - 1.0, regions.flame_top)));
        assert!(!regions.in_flame(at(regions.content_left, regions.flame_bottom)));
        // Clear of the region on either axis: in the gutter, and in the ruler.
        assert!(!regions.in_flame(at(regions.content_left - 1.0, regions.flame_top)));
        assert!(!regions.in_flame(at(right - 1.0, regions.flame_top - 1.0)));
    }

    /// The time bar lies along the bottom edge of the flame lanes, inside the
    /// region it pans, with its far edge on the divider.
    #[test]
    fn the_time_bar_lies_along_the_bottom_of_the_flame_lanes() {
        let regions = counters_split(3, CountersStrip::Open(DEFAULT_COUNTERS_HEIGHT));
        let span = TimeSpan {
            start_us: 0,
            end_us: 1_000_000,
        };
        // Zoomed well past the width the span fits, so there is time to pan to.
        let tv = TimeView {
            start_us: 0.0,
            px_per_us: 2.0 * fit_px_per_us(span, regions.time_w, EDGE_MARGIN_PX),
        };
        let track = scrollbar_geom(span, tv, 0.0, regions, 0.0, 0.0)
            .time
            .expect("a zoomed view has time to pan to")
            .track();
        assert_eq!(track.size.height, px(SCROLLBAR_THICKNESS));
        assert_eq!(track.origin.y + track.size.height, px(regions.flame_bottom));
        assert!(regions.in_flame(track.origin));
    }

    /// A view sitting on the zoom floor holds the whole span in sight, so there
    /// is nothing to pan to and no time bar.
    #[test]
    fn a_view_at_the_zoom_floor_has_no_time_bar() {
        let regions = counters_split(3, CountersStrip::Open(DEFAULT_COUNTERS_HEIGHT));
        let span = TimeSpan {
            start_us: 0,
            end_us: 1_000_000,
        };
        let tv = TimeView::fit(span, regions.time_w, EDGE_MARGIN_PX);
        assert!(
            scrollbar_geom(span, tv, 0.0, regions, 0.0, 0.0)
                .time
                .is_none()
        );
    }

    /// The divider stands for as long as there are counters to resize, shut or
    /// open; a profile with none has neither strip nor divider.
    #[test]
    fn only_a_profile_with_counters_has_a_divider() {
        let window = Bounds::new(point(px(0.0), px(0.0)), size(px(1000.0), px(600.0)));
        let split = |n, strip| Regions::new(window, n, strip, DEFAULT_GUTTER_WIDTH, 0.0);

        let none = split(0, CountersStrip::Open(DEFAULT_COUNTERS_HEIGHT));
        assert_eq!(none.divider_rect().size.height, px(0.0));
        assert_eq!(none.counters_h(), 0.0);

        let shut = split(3, CountersStrip::Collapsed { restore: 220.0 });
        assert!(!shut.shows_counters());
        assert_eq!(
            shut.divider_rect().size.height,
            px(COUNTERS_COLLAPSED_HEIGHT)
        );
        // The band spans the content column, so it is there to be pressed
        // wherever the cursor crosses it right of the gutter.
        assert_eq!(
            shut.divider_rect().size.width,
            window.size.width - px(DEFAULT_GUTTER_WIDTH)
        );
        // An open strip leaves a band sized for its line alone, and the flame
        // region gives up the room the strip takes.
        let open = split(3, CountersStrip::Open(DEFAULT_COUNTERS_HEIGHT));
        assert_eq!(open.divider_rect().size.height, px(COUNTERS_DIVIDER_HEIGHT));
        assert!(shut.flame_bottom > open.flame_bottom);
    }

    /// Lanes with room to spare share the strip evenly, each a header band over
    /// its chart.
    #[test]
    fn counter_lanes_share_a_roomy_strip_evenly() {
        let n = 2;
        let strip = 4.0 * COUNTER_LANE_HEIGHT;
        let regions = counters_split(n, CountersStrip::Open(strip));
        let lanes: Vec<_> = regions.counter_lanes(0.0).collect();
        assert_eq!(lanes.len(), n);
        let lane_h = strip / n as f32;
        for (i, lane) in lanes.iter().enumerate() {
            assert_eq!(lane.chart_h, lane_h - METRICS.header_height);
            assert_eq!(lane.top, regions.counters_top + i as f32 * lane_h);
        }
    }

    #[test]
    fn a_scroll_request_moving_nothing_leaves_the_held_position_standing() {
        let ceiling = 400.0;
        // A position the content has shrunk away from, drawn at the ceiling.
        let held = 900.0;
        assert_eq!(drawn_scroll(held, ceiling), ceiling);
        assert_eq!(
            scroll_request(held, ceiling, ceiling + LINE_SCROLL_PX),
            None
        );
        assert_eq!(scroll_request(held, ceiling, ceiling), None);
        // It draws where it stands again once the content grows back past it.
        assert_eq!(drawn_scroll(held, 1000.0), held);
    }

    #[test]
    fn a_scroll_request_that_moves_reports_its_bounded_target() {
        let ceiling = 400.0;
        assert_eq!(scroll_request(900.0, ceiling, 250.0), Some(250.0));
        assert_eq!(
            scroll_request(100.0, ceiling, 100.0 + LINE_SCROLL_PX),
            Some(100.0 + LINE_SCROLL_PX)
        );
        // Past either end the gesture lands on the bound rather than being refused.
        assert_eq!(scroll_request(100.0, ceiling, 5000.0), Some(ceiling));
        assert_eq!(scroll_request(100.0, ceiling, -20.0), Some(0.0));
    }

    #[test]
    fn a_scroll_request_holds_at_the_floor_and_the_ceiling() {
        let ceiling = 400.0;
        assert_eq!(scroll_request(0.0, ceiling, -LINE_SCROLL_PX), None);
        assert_eq!(
            scroll_request(ceiling, ceiling, ceiling + LINE_SCROLL_PX),
            None
        );
        // Content shorter than its viewport pins every gesture to the top.
        assert_eq!(scroll_request(0.0, 0.0, LINE_SCROLL_PX), None);
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

    const BUILD_US: i64 = 40_000_000;

    fn selection(regions: &[(i64, i64)]) -> TimeSelection {
        let mut selection = TimeSelection::default();
        for &(start_us, end_us) in regions {
            selection.insert(
                bviz_trace_layout::span_between(start_us, end_us)
                    .unwrap_or_else(|| panic!("({start_us}, {end_us}) spans no time")),
            );
        }
        selection
    }

    #[test]
    fn a_one_region_window_reads_as_a_share_of_the_build() {
        assert_eq!(
            time_window_summary(Some(&selection(&[(0, 2_500_000)])), BUILD_US).as_deref(),
            Some("Window: 2.50s of 40.00s")
        );
    }

    #[test]
    fn several_regions_are_counted_alongside_the_time_they_hold() {
        let three = selection(&[
            (0, 1_000_000),
            (2_000_000, 3_000_000),
            (4_000_000, 5_000_000),
        ]);
        assert_eq!(
            time_window_summary(Some(&three), BUILD_US).as_deref(),
            Some("Window: 3.00s of 40.00s · 3 regions")
        );
    }

    #[test]
    fn a_window_far_shorter_than_the_build_keeps_its_own_unit() {
        assert_eq!(
            time_window_summary(Some(&selection(&[(0, 4_200)])), BUILD_US).as_deref(),
            Some("Window: 4.20ms of 40.00s")
        );
    }

    #[test]
    fn an_unrestricted_aggregate_carries_no_chip() {
        assert_eq!(
            time_window_summary(Some(&TimeSelection::default()), BUILD_US),
            None
        );
    }

    #[test]
    fn no_chip_stands_for_figures_that_are_not_on_screen_yet() {
        assert_eq!(time_window_summary(None, BUILD_US), None);
    }

    /// A strip with more lanes than it can hold shows a bar over its own band,
    /// in the column reserved at the content's right edge; one whose lanes fit
    /// shows none, and reserves no column for one.
    #[test]
    fn the_counters_bar_stands_only_while_the_lanes_overflow() {
        let n = 10;
        let overflowing = counters_split(n, CountersStrip::Open(DEFAULT_COUNTERS_HEIGHT));
        assert!(overflowing.max_counter_scroll() > 0.0);
        let bar = counter_bar(overflowing).expect("the overflowing lanes are scrolled by a bar");
        assert_eq!(bar.track().origin.y, px(overflowing.counters_top));
        assert_eq!(bar.track().size.height, px(overflowing.counters_h()));
        assert_eq!(
            bar.track().origin.x,
            px(overflowing.content_right - SCROLLBAR_THICKNESS)
        );
        // The plots end left of the bar rather than running under it.
        assert_eq!(
            overflowing.time_right,
            overflowing.content_right - SCROLLBAR_THICKNESS
        );

        let roomy = counters_split(n, CountersStrip::Open(n as f32 * COUNTER_LANE_HEIGHT));
        assert_eq!(roomy.max_counter_scroll(), 0.0);
        assert!(counter_bar(roomy).is_none());
        assert_eq!(roomy.time_right, roomy.content_right);
    }
}
