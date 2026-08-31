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

use chart::{format_duration, format_time, nice_step, span_seconds};

use flame_panel::FlamePanel;
use groups::{GroupChrome, group_label, lanes_headed_by_pool};
use model::TraceModel;
use regions::{Regions, clamp_gutter_width, clamp_panel_height, flame_total_height, graph_lanes};
use stats_panel::StatsPanel;
use theme::Palette;
use timeline::{HoveredSlice, TimelinePanel};
use track_label::{pool_member_label, track_label};
use tracks_menu::TracksMenu;

use bviz_trace_layout::{
    FlameDirection, FlameLabels, FlameMode, LaneLayout, LaneMetrics, TimeSelection, TimeView,
};
use bviz_trace_profile::{Profile, Slice, TimeSpan};
use bviz_ui_components::{
    Axis, Button, DEFAULT_MIN_THUMB, DragEnd, Pending, SCROLLBAR_THICKNESS, Scrollbar,
    centered_spinner, load_in_background, overlay_canvas, paint_tooltip_box, track_drag,
};
use bviz_ui_shell::Chrome;
use bviz_ui_trace_args::{TraceArgs, TraceSource};
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, ContentMask, Context, Div, Edges, Element, ElementId,
    Entity, FocusHandle, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId,
    MouseButton, MouseDownEvent, Pixels, Point, Position, Render, ScrollHandle, SharedString,
    Stateful, Style, Subscription, Task, TextAlign, TransformationMatrix, Window, div, fill, point,
    prelude::*, px, relative, size,
};
use std::sync::Arc;

gpui::actions!(bviz_trace, [SelectAllTracks, ClearTimeSelection]);

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

/// Font size of a phase-marker label.
const MARKER_LABEL_SIZE: f32 = 11.0;

/// Height of the tab bar between the flame chart and the bottom panel, which
/// doubles as the resize splitter for the panel.
const TAB_BAR_HEIGHT: f32 = 28.0;

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
    /// A press on the tab bar resizes the bottom panel.
    Splitter,
    /// A press on the gutter handle resizes the gutter.
    Gutter,
}

/// Height of a group header band and of a group's "show more" row.
const GROUP_BAND_HEIGHT: f32 = 18.0;
/// Side of the show-all icon at a group band's gutter edge.
const GROUP_ICON: f32 = 12.0;
/// Indent of a pool header's gutter label under its group band's label, one
/// nesting step in.
const FAMILY_LABEL_INDENT: f32 = 10.0;
/// Least gap kept between a header row's name and the count at its trailing edge.
const HEADER_COUNT_GAP: f32 = 6.0;
/// Lanes shown per group before the rest fold behind the band's show-all toggle.
const TOP_N_PER_GROUP: usize = 12;

pub(crate) type Source = bviz_ui_components::Source<Arc<Profile>>;

/// Loads and renders a Bazel trace profile as a flame chart.
///
/// The flame-chart timeline lives in [`TimelinePanel`] and the aggregated
/// flame-graph panel in [`FlamePanel`]; each owns its own view state, reading the
/// document and selection from the shared [`TraceModel`].
struct TraceView {
    /// Focus for the window chrome, held by [`Chrome`] so the window's own
    /// actions (quit, close, tab) dispatch to it; the ancestor of the timeline's
    /// content focus in the focus tree.
    window_focus: FocusHandle,
    /// The one instance the four panels each hold a handle to.
    model: Entity<TraceModel>,
    timeline: Entity<TimelinePanel>,
    flame: Entity<FlamePanel>,
    stats: Entity<StatsPanel>,
    drag: Drag,
    last_mouse: Point<Pixels>,
    /// Height of the bottom panel, set by the tab-bar splitter; clamped on use.
    panel_height: f32,
    /// Width of the left label gutter, set by its drag handle; clamped on use.
    gutter_width: f32,
    /// Which panel the tab bar shows.
    bottom_tab: BottomTab,
    tracks_menu: Entity<TracksMenu>,
    /// Rebuilds the timeline's and flame-graph panel's cached scenes on a system
    /// appearance change so they repaint in the new palette.
    _appearance_observer: Subscription,
    /// Holds the background load task so it isn't cancelled on drop.
    _load_task: Task<()>,
}

impl TraceView {
    fn new(load: Load, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let window_focus = cx.focus_handle();
        let load_task = load_in_background(
            cx,
            move || load.0.join(),
            |view, result, cx| view.on_profile_loaded(result, cx),
        );
        let model = cx.new(|_| TraceModel::new());
        // A panel that reaches back here takes a weak handle, so the shell's own
        // handle to it is what keeps it alive.
        let timeline = {
            let shell = cx.entity().downgrade();
            let model = model.clone();
            cx.new(|cx| TimelinePanel::new(model, shell, window, cx))
        };
        let flame = {
            let model = model.clone();
            cx.new(|cx| FlamePanel::new(model, cx))
        };
        let stats = {
            let model = model.clone();
            cx.new(|cx| StatsPanel::new(model, cx))
        };
        let tracks_menu = {
            let shell = cx.entity().downgrade();
            let model = model.clone();
            cx.new(|cx| TracksMenu::new(model, shell, cx))
        };
        let appearance_observer = window.observe_window_appearance({
            let timeline = timeline.clone();
            let flame = flame.clone();
            move |_, cx| {
                timeline.update(cx, |t, cx| t.invalidate_canvas(cx));
                flame.update(cx, |f, cx| f.invalidate_canvas(cx));
            }
        });
        Self {
            window_focus,
            model,
            timeline,
            flame,
            stats,
            drag: Drag::None,
            last_mouse: Point::default(),
            panel_height: DEFAULT_PANEL_HEIGHT,
            gutter_width: DEFAULT_GUTTER_WIDTH,
            bottom_tab: BottomTab::Counters,
            tracks_menu,
            _appearance_observer: appearance_observer,
            _load_task: load_task,
        }
    }

    /// Rebuilds the timeline's cached scene and re-renders this view (so the tab
    /// bar, bottom panel, and gutter handle reposition). A change to the timeline
    /// content, the panel/gutter layout, or the shown tab uses this.
    fn invalidate_timeline(&self, cx: &mut Context<Self>) {
        self.timeline.update(cx, |t, cx| t.invalidate_canvas(cx));
        cx.notify();
    }

    fn on_profile_loaded(&mut self, result: Result<Profile, String>, cx: &mut Context<Self>) {
        self.timeline.update(cx, |t, _| t.reset_for_new_profile());
        self.flame.update(cx, |f, _| f.reset_view());
        // The aggregate-derived panel caches: dropped up front so a new profile
        // shows the spinner rather than the prior graph until its build lands.
        self.stats.update(cx, |s, _| s.reset_for_new_profile());
        let source = match result {
            Ok(profile) => Source::Loaded(Arc::new(profile)),
            Err(err) => Source::Failed(err),
        };
        // The tracks-menu fold state, unlike the document-derived selection the
        // model seeds, is view-local, so reset it here for a freshly loaded
        // profile.
        let loaded = matches!(source, Source::Loaded(_));
        self.model.update(cx, |m, cx| {
            m.invalidate_aggregates();
            m.set_source(source, cx);
        });
        if loaded {
            self.tracks_menu
                .update(cx, |m, _| m.reset_for_new_profile());
        }
        self.invalidate_timeline(cx);
    }

    /// The whole trace body: the [`TimelinePanel`] filling the viewport (it paints
    /// the flame chart and counter plots and owns the timeline's mouse/scroll/
    /// hover wiring), a drag overlay that keeps the panel-resize and gutter drags
    /// tracking the cursor, then the tab bar, the selected bottom panel, the
    /// gutter handle, and the tracks menu over it.
    fn trace_body(&self, palette: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let timeline = div()
            .absolute()
            .inset_0()
            .size_full()
            .child(self.timeline.clone());
        // The panel-resize and gutter drags live on this view; while one runs, a
        // full-bounds canvas re-installs their window-global handlers each frame.
        // It is non-interactive, so it does not intercept the timeline's presses.
        let drag_overlay = {
            let entity = cx.entity();
            overlay_canvas(move |_, window, cx| {
                if matches!(entity.read(cx).drag, Drag::Splitter | Drag::Gutter) {
                    track_drag(window, &entity, on_drag, end_drag);
                }
            })
        };

        // The tab bar spans the content column between the flame chart and the
        // bottom panel, doubling as the panel's resize splitter. The selected
        // panel sits over the content column of the bottom region; the counter
        // plots are drawn by the timeline instead.
        let mut overlays = RegionOverlays::new(self.region_inputs(cx));
        if let Some(panel) = self.bottom_panel(palette, cx) {
            overlays = overlays.child(panel, panel_rect);
        }
        // The gutter handle spans the full height and so crosses both, and must
        // take a press over either: it is placed last, and hit-testing follows
        // paint order.
        let overlays = overlays
            .child(self.tab_bar(palette, cx), tab_bar_rect)
            .child(self.gutter_handle(cx), gutter_handle_rect);
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

    /// A thin overlay strip at the gutter's right edge that begins a width drag;
    /// a double-click resets the gutter to its default width.
    fn gutter_handle(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let drag_entity = cx.entity();
        div()
            .id("trace-gutter-handle")
            .size_full()
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
                        this.invalidate_timeline(cx);
                    });
                    cx.stop_propagation();
                },
            )
    }

    /// The tab bar between the flame chart and the bottom panel: a row of tab
    /// buttons filling the rect [`RegionOverlays`] places it in. A press on its
    /// background begins a panel-resize drag (double-click resets the panel
    /// height); a tab button switches the panel.
    fn tab_bar(&self, palette: Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let active = self.bottom_tab;
        let tabs: Vec<_> = BottomTab::ALL
            .into_iter()
            .map(|(tab, label)| {
                let is_active = tab == active;
                let hover = palette.hover;
                div()
                    .id(SharedString::from(format!("trace-tab-{label}")))
                    .flex()
                    .items_center()
                    .flex_shrink_0()
                    .px_3()
                    .h_full()
                    .cursor_pointer()
                    .text_size(px(STATS_TEXT_SIZE))
                    .text_color(if is_active {
                        palette.label
                    } else {
                        palette.muted_label
                    })
                    .when(is_active, |t| {
                        t.bg(palette.background)
                            .border_t_2()
                            .border_color(palette.accent)
                    })
                    .hover(move |t| t.bg(hover))
                    // A press on a tab must switch it, not begin a panel resize.
                    .on_mouse_down(MouseButton::Left, |_, _, cx: &mut App| {
                        cx.stop_propagation()
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_bottom_tab(tab, cx);
                        this.invalidate_timeline(cx);
                    }))
                    .child(label)
            })
            .collect();
        div()
            .id("trace-tab-bar")
            .size_full()
            .flex()
            .flex_row()
            .items_stretch()
            .overflow_hidden()
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
                    this.invalidate_timeline(cx);
                }),
            )
            .children(tabs)
            .children(self.time_window_chip(palette, cx))
            .child(div().flex_1())
            .children((active == BottomTab::FlameGraph).then(|| {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_4()
                    .pr_2()
                    .child(self.flame_mode_controls(cx))
                    .child(theme::role_legend(&palette, STATS_TEXT_SIZE))
            }))
    }

    /// The window the shown flame graph and stats cover, as a chip carrying
    /// [`Self::clear_time_selection`].
    fn time_window_chip(
        &self,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let summary = {
            let model = self.model.read(cx);
            let Source::Loaded(profile) = model.source() else {
                return None;
            };
            let span = profile.span();
            let shown = model.aggregates();
            time_window_summary(
                shown.as_deref().map(|a| &a.window),
                span.end_us - span.start_us,
            )?
        };
        let view = cx.entity();
        Some(
            div()
                .id("trace-time-window")
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .ml_2()
                .my_1()
                .pl_2()
                .flex_shrink_0()
                .rounded_md()
                .bg(palette.selection)
                .border_1()
                .border_color(palette.accent)
                .text_size(px(STATS_TEXT_SIZE))
                .text_color(palette.label)
                // A press on the chip must not also begin a panel resize.
                .on_mouse_down(MouseButton::Left, |_, _, cx: &mut App| {
                    cx.stop_propagation()
                })
                .child(summary)
                .child(
                    Button::new("trace-time-window-clear", move |_, _, cx| {
                        view.update(cx, |this, cx| this.clear_time_selection(cx));
                    })
                    .ghost()
                    .compact()
                    .icon("x"),
                ),
        )
    }

    /// Widens the next aggregate build back to the whole timeline.
    fn clear_time_selection(&mut self, cx: &mut Context<Self>) {
        if !self.model.update(cx, |m, cx| {
            m.set_time_selection(TimeSelection::default(), cx)
        }) {
            return;
        }
        cx.notify();
    }

    /// The flame graph's mode controls: the two directions as an exclusive pair,
    /// and the labelling as a toggle that holds across both.
    fn flame_mode_controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.model.read(cx).flame_mode();
        let mnemonics = mode.labels == FlameLabels::Mnemonic;
        let button = |id: &'static str, label: &'static str, on: bool, to: FlameMode| {
            let view = cx.entity();
            div()
                .id(SharedString::from(format!("{id}-press")))
                // A press on a control must not also begin a panel resize.
                .on_mouse_down(MouseButton::Left, |_, _, cx: &mut App| {
                    cx.stop_propagation()
                })
                .child(
                    Button::new(id, move |_, _, cx| {
                        view.update(cx, |this, cx| this.set_flame_mode(to, cx));
                    })
                    .ghost()
                    .compact()
                    .selected(on)
                    .label(label),
                )
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .gap_1()
            .text_size(px(STATS_TEXT_SIZE))
            .child(button(
                "flame-top-down",
                "Top-down",
                mode.direction == FlameDirection::TopDown,
                FlameMode {
                    direction: FlameDirection::TopDown,
                    ..mode
                },
            ))
            .child(button(
                "flame-bottom-up",
                "Bottom-up",
                mode.direction == FlameDirection::BottomUp,
                FlameMode {
                    direction: FlameDirection::BottomUp,
                    ..mode
                },
            ))
            .child(button(
                "flame-mnemonics",
                "Mnemonics",
                mnemonics,
                FlameMode {
                    labels: if mnemonics {
                        FlameLabels::Name
                    } else {
                        FlameLabels::Mnemonic
                    },
                    ..mode
                },
            ))
    }

    /// Applies a mode chosen in the tab bar, leaving the panel's view alone for a
    /// click that re-chooses the standing mode.
    fn set_flame_mode(&mut self, mode: FlameMode, cx: &mut Context<Self>) {
        if !self.model.update(cx, |m, cx| m.set_flame_mode(mode, cx)) {
            return;
        }
        self.flame.update(cx, |f, cx| {
            f.reset_view();
            f.invalidate_canvas(cx);
        });
    }

    /// The bottom-panel overlay for the active tab, positioned over the content
    /// column of the panel region. The Counters tab draws on the flame chart's
    /// canvas, so it needs no overlay.
    fn bottom_panel(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let content: AnyElement = match self.bottom_tab {
            BottomTab::Counters => return None,
            BottomTab::FlameGraph => match self.model.read(cx).aggregates() {
                Some(_) => self.flame.clone().into_any_element(),
                None => {
                    centered_spinner("Building flame graph…", &palette.colors).into_any_element()
                }
            },
            BottomTab::Stats => self.stats.clone().into_any_element(),
        };
        Some(
            div()
                .id("trace-panel")
                .size_full()
                .flex()
                .flex_col()
                .overflow_hidden()
                .bg(palette.background)
                .child(content)
                .into_any_element(),
        )
    }

    fn status(&self, cx: &App) -> String {
        self.model.read(cx).source().status("No profile", |p| {
            let slices: usize = p.threads().iter().map(|t| t.slices.len()).sum();
            format!(
                "{} threads · {} slices · {:.3}s",
                p.threads().len(),
                slices,
                span_seconds(p.span()),
            )
        })
    }

    /// [`Regions`] for the current viewport (captured by the timeline), panel and
    /// gutter sizes, the loaded profile's counter count, and the flame lanes'
    /// total height.
    fn region_inputs(&self, cx: &App) -> RegionInputs {
        let model = self.model.read(cx);
        RegionInputs {
            n_counters: match model.source() {
                Source::Loaded(p) => p.counters().len(),
                _ => 0,
            },
            panel_height: self.panel_height,
            gutter_width: self.gutter_width,
            flame_total: model
                .lane_layout()
                .map(|l| flame_total_height(&l))
                .unwrap_or(0.0),
        }
    }

    /// Switches the bottom panel tab, dropping the flame-graph and timeline hover
    /// tooltips and the timeline crosshair (the newly shown panel picks its own
    /// up on the next move).
    fn set_bottom_tab(&mut self, tab: BottomTab, cx: &mut Context<Self>) {
        if self.bottom_tab == tab {
            return;
        }
        self.bottom_tab = tab;
        self.flame.update(cx, |f, _| f.clear_hover());
        self.timeline.update(cx, |t, _| t.clear_hover());
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

impl Render for TraceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = bviz_ui_theme::theme(window, cx);
        let palette = Palette::from_theme(&theme);
        // Take the placeholder before the `&mut self` build calls below, so the
        // model borrow it is read through ends first.
        let placeholder = self.model.read(cx).source().placeholder(&theme.colors);
        let body = match placeholder {
            Some(placeholder) => placeholder,
            None => div()
                .flex_1()
                .overflow_hidden()
                .child(self.trace_body(palette, cx)),
        };
        Chrome::new(&self.window_focus)
            .title("bazel_viz trace")
            .status(self.status(cx))
            .child(body)
    }
}

/// What [`Regions`] needs beyond the bounds it splits.
#[derive(Clone, Copy)]
struct RegionInputs {
    n_counters: usize,
    panel_height: f32,
    gutter_width: f32,
    flame_total: f32,
}

impl RegionInputs {
    fn split(&self, bounds: Bounds<Pixels>) -> Regions {
        Regions::new(
            bounds,
            self.n_counters,
            self.panel_height,
            self.gutter_width,
            self.flame_total,
        )
    }
}

/// Where an overlay sits within the split, given the body's own bounds.
type PlaceInRegions = fn(&Regions, Bounds<Pixels>) -> Bounds<Pixels>;

fn tab_bar_rect(r: &Regions, _: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        point(px(r.content_left), px(r.tab_top)),
        size(
            px((r.content_right - r.content_left).max(0.0)),
            px(TAB_BAR_HEIGHT),
        ),
    )
}

fn panel_rect(r: &Regions, _: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        point(px(r.content_left), px(r.panel_top)),
        size(
            px((r.content_right - r.content_left).max(0.0)),
            px(r.panel_h()),
        ),
    )
}

fn gutter_handle_rect(r: &Regions, body: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        point(
            px(r.content_left - GUTTER_HANDLE_WIDTH / 2.0),
            body.origin.y,
        ),
        size(px(GUTTER_HANDLE_WIDTH), body.size.height),
    )
}

/// The trace body's overlays — the tab bar, the bottom panel, and the gutter
/// handle — laid out from the body's own bounds. Each belongs at a pixel offset
/// within the [`Regions`] split, which is defined only once the body has a size,
/// so the split is resolved here during prepaint and each child placed against
/// it. Styling the children from a size measured elsewhere would put them a frame
/// behind the body they overlay, since that size is known only after a paint.
struct RegionOverlays {
    inputs: RegionInputs,
    children: Vec<(AnyElement, PlaceInRegions)>,
}

impl RegionOverlays {
    fn new(inputs: RegionInputs) -> Self {
        Self {
            inputs,
            children: Vec::new(),
        }
    }

    fn child(mut self, element: impl IntoElement, place: PlaceInRegions) -> Self {
        self.children.push((element.into_any_element(), place));
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
    type PrepaintState = ();

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
    ) {
        let regions = self.inputs.split(bounds);
        for (child, place) in &mut self.children {
            let rect = place(&regions, bounds);
            let space = size(
                AvailableSpace::Definite(rect.size.width),
                AvailableSpace::Definite(rect.size.height),
            );
            child.prepaint_as_root(rect.origin, space, window, cx);
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        for (child, _) in &mut self.children {
            child.paint(window, cx);
        }
    }
}

/// Routes an in-progress drag to its target: resizing the bottom panel or the
/// gutter. The flame-chart timeline's, flame-graph panel's, and tracks menu's own
/// drags are routed inside [`TimelinePanel`], [`FlamePanel`], and [`TracksMenu`].
fn on_drag(view: &mut TraceView, position: Point<Pixels>, cx: &mut Context<TraceView>) {
    match view.drag {
        Drag::Splitter => {
            let profile = match view.model.read(cx).source() {
                Source::Loaded(p) => p.clone(),
                _ => return,
            };
            let vp = view.timeline.read(cx).viewport();
            let vh = f32::from(vp.size.height);
            // The panel's top follows the cursor: the panel spans from just below
            // the tab bar down to the window bottom. Clamped to keep the flame
            // region and the panel content usable.
            let panel_bottom = f32::from(vp.origin.y) + vh;
            view.panel_height = clamp_panel_height(
                panel_bottom - f32::from(position.y) - TAB_BAR_HEIGHT,
                profile.counters().len(),
                vh,
            );
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
    // Panel and gutter resizes move the timeline's split, so they rebuild it.
    view.invalidate_timeline(cx);
}

fn end_drag(view: &mut TraceView, _: Point<Pixels>, _: DragEnd, cx: &mut Context<TraceView>) {
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

/// The flame section's vertical bar and the content column's horizontal bar,
/// each `None` on an axis that fits. The vertical bar maps to the flame lanes'
/// pixel scroll; the horizontal to the view's start time within the profile
/// span.
fn scrollbar_geom(
    span: TimeSpan,
    tv: TimeView,
    flame_total: f32,
    regions: Regions,
    scroll_y: f32,
) -> (Option<Scrollbar>, Option<Scrollbar>) {
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
            point(
                px(regions.content_right - SCROLLBAR_THICKNESS),
                px(regions.flame_top),
            ),
            size(px(SCROLLBAR_THICKNESS), px(flame_h)),
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
            size(px(regions.time_w), px(SCROLLBAR_THICKNESS)),
        ),
        span_us + 2.0 * margin_us,
        visible_us,
        tv.start_us as f32 - (span.start_us as f32 - margin_us),
        DEFAULT_MIN_THUMB,
    );

    (vlayout, hlayout)
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
    shared_names: &[bool],
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let gutter_left = regions.content_left - gutter_width;
    let gutter_bottom = regions.panel_bottom;
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
    /// Inset of the name from the gutter's text inset, one nesting step per level.
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

/// Paints each group header band's chevron, class name, and shown/total lane
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
        let chevron = if band.collapsed { "▸" } else { "▾" };
        let name = format!("{chevron} {}", group_label(band.class));
        let count = header_count((!band.collapsed).then_some(band.shown), band.total);
        let Some(top) = paint_header_row(
            g,
            regions,
            scroll_y,
            HeaderRow {
                top: band.top,
                name: &name,
                count: &count,
                indent: 0.0,
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
        if show_icon {
            let name = if band.show_all {
                "caret-double-up"
            } else {
                "caret-double-down"
            };
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

/// Paints each counter's name at its panel lane's chart baseline, so it associates
/// with low values rather than floating at the top.
fn paint_counter_labels(
    g: &GutterPaint,
    profile: &Profile,
    regions: Regions,
    window: &mut Window,
    cx: &mut App,
) {
    for gl in graph_lanes(
        profile.counters().len(),
        regions.panel_top,
        regions.panel_h(),
    ) {
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
pub struct Load(Pending<Profile>);

impl From<TraceSource> for Load {
    fn from(source: TraceSource) -> Self {
        // rayon's global pool takes the size of the first build_global to win,
        // so it is sized here, ahead of any layout pass that runs on it.
        bviz_trace_layout::init_thread_pool();
        Self(Pending::start(move || {
            Profile::from_path(&source.path).map_err(|e| e.to_string())
        }))
    }
}

/// Opens a window rendering the profile `load` reads.
pub fn open_window(load: impl Into<Load>) {
    let load = load.into();
    bviz_ui_shell::run(
        "bazel_viz trace",
        size(px(1280.), px(820.)),
        move |window, cx| cx.new(|cx| TraceView::new(load, window, cx)),
    );
}

/// Renders a Bazel `--profile` trace as a flame-chart timeline.
#[derive(Parser)]
#[command(name = "trace", version, about)]
struct Cli {
    #[command(flatten)]
    trace: TraceArgs,
}

/// Parses `--profile` and opens the trace-viewer window.
pub fn run() {
    bviz_alloc::retain_freed_memory_across_load();
    let mut sources = Cli::parse().trace.sources();
    let Some(source) = sources.next() else {
        Cli::command()
            .error(ErrorKind::MissingRequiredArgument, "--profile is required")
            .exit()
    };
    if sources.next().is_some() {
        Cli::command()
            .error(
                ErrorKind::TooManyValues,
                "a window shows one document, so one is opened per invocation",
            )
            .exit()
    }
    open_window(source);
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
        let max = vh - RULER_HEIGHT - SCROLLBAR_THICKNESS - TAB_BAR_HEIGHT - FLAME_MIN_HEIGHT;
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
            DEFAULT_PANEL_HEIGHT,
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

    #[test]
    fn graph_lanes_collapse_to_labels() {
        // At the label-only minimum the charts squish to zero, leaving headers.
        let lanes = graph_lanes(2, 0.0, 2.0 * METRICS.header_height);
        assert_eq!(lanes[0].chart_h, 0.0);
        assert_eq!(lanes[1].chart_h, 0.0);
        assert!(graph_lanes(0, 0.0, 0.0).is_empty());
    }
}
