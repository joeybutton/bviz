//! Loads a Bazel `--profile` trace (`.profile.gz` or `.json`) into
//! `bviz_trace_profile` and opens a window rendering its thread tracks as a
//! flame-chart timeline: time on the x axis, stacked call depth on the y axis,
//! with horizontal zoom/pan and vertical scroll.

use bviz_trace_layout::{
    LaneLayout, LaneMetrics, TimeView, Viewport, lay_out_lanes, visible_slices,
};
use bviz_trace_profile::{CounterLane, CounterPoint, PhaseMarker, Profile, TimeSpan};
use bviz_ui_components::{Axis, Scrollbar, load_in_background, track_drag};
use bviz_ui_shell::Chrome;
use clap::Parser;
use gpui::{
    Animation, AnimationExt, App, BorderStyle, Bounds, ContentMask, Context, Div, Entity,
    FocusHandle, Hsla, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PathBuilder, Pixels, Point, Render, Rgba, ScrollDelta, ScrollWheelEvent, Stateful, Task,
    TextAlign, Transformation, Window, canvas, div, hsla, percentage, point, prelude::*, px, quad,
    size,
};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

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

/// Empty space kept reachable past each end of the timeline so its start and end
/// stand clear of the window edges.
const EDGE_MARGIN_PX: f32 = 24.0;

/// Pixels scrolled per wheel line.
const LINE_SCROLL_PX: f32 = 24.0;

/// Corner radius and border width of a painted slice quad.
const SLICE_RADIUS: f32 = 2.0;
const SLICE_BORDER: f32 = 0.5;

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

/// Initial height of the bottom graph section before the splitter is dragged.
const DEFAULT_GRAPH_HEIGHT: f32 = 280.0;

/// Thickness of the painted splitter divider.
const SPLITTER_THICKNESS: f32 = 4.0;

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
/// Number of stacked rows phase-marker labels cycle through so a label near an
/// earlier one drops to a lower row rather than being suppressed.
const MARKER_LABEL_ROWS: usize = 3;

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

/// Saturation of every category color.
const CATEGORY_SATURATION: f32 = 0.5;
/// Slice-fill lightness on a dark vs. a light background.
const CATEGORY_LIGHTNESS_DARK: f32 = 0.62;
const CATEGORY_LIGHTNESS_LIGHT: f32 = 0.50;

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
}

/// What a left-button drag is currently moving.
#[derive(Clone, Copy)]
enum Drag {
    None,
    /// A press on the canvas pans the timeline.
    Pan,
    /// A press on a scroll thumb/track, holding the grab offset from the thumb's
    /// leading edge.
    VThumb(f32),
    HThumb(f32),
    /// A press on the splitter resizes the graph section.
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

/// The window split into its time-content column and stacked vertical bands,
/// derived once per frame from the viewport bounds, the counter count, and the
/// splitter's `graph_height`. Both paint and the mouse handlers compute the same
/// regions so hit-testing matches what was drawn.
#[derive(Clone, Copy)]
struct Regions {
    /// Left edge of the time content, past the gutter.
    content_left: f32,
    /// Width of the time content column.
    content_w: f32,
    /// Right edge of the content column (where the vertical bar/gutter end).
    content_right: f32,
    /// Right edge of the time axis, left of the reserved vertical-bar column.
    /// Everything mapped to time ends here so nothing underlaps the bar.
    time_right: f32,
    /// Width of the time axis: `content_w` less the reserved vertical-bar column.
    time_w: f32,
    /// Top of the flame section (ruler bottom).
    flame_top: f32,
    /// Boundary between flame and graph sections; the splitter sits here.
    graph_top: f32,
    /// Bottom of the graph section (horizontal-bar top).
    body_bottom: f32,
    /// Whether a graph section is present (the profile has counters).
    has_graph: bool,
}

impl Regions {
    /// Splits `bounds` given the counter count, the splitter's stored height, the
    /// gutter width, and the flame section's total lane height (to decide whether
    /// a vertical bar shows and so a column must be reserved for it).
    fn new(
        bounds: Bounds<Pixels>,
        n_counters: usize,
        graph_height: f32,
        gutter_width: f32,
        flame_total: f32,
    ) -> Self {
        let ox = f32::from(bounds.origin.x);
        let oy = f32::from(bounds.origin.y);
        let w = f32::from(bounds.size.width);
        let vh = f32::from(bounds.size.height);
        let flame_top = oy + RULER_HEIGHT;
        let body_bottom = oy + vh - BAR_WIDTH;
        let has_graph = n_counters > 0;
        let graph_h = if has_graph {
            clamp_graph_height(graph_height, n_counters, vh)
        } else {
            0.0
        };
        let content_left = ox + gutter_width;
        let content_w = (w - gutter_width).max(0.0);
        let content_right = ox + w;
        let flame_h = (body_bottom - graph_h).max(flame_top) - flame_top;
        // Reserve a column for the vertical bar when the flame lanes overflow it,
        // so slices and charts end left of the bar rather than under it.
        let vbar_reserve = if flame_total > flame_h.max(0.0) {
            BAR_WIDTH
        } else {
            0.0
        };
        Regions {
            content_left,
            content_w,
            content_right,
            time_right: content_right - vbar_reserve,
            time_w: (content_w - vbar_reserve).max(0.0),
            flame_top,
            graph_top: (body_bottom - graph_h).max(flame_top),
            body_bottom,
            has_graph,
        }
    }

    /// Height the flame section's lanes scroll within.
    fn flame_h(&self) -> f32 {
        (self.graph_top - self.flame_top).max(0.0)
    }
}

/// The graph section's height bounded to leave the flame section at least
/// `FLAME_MIN_HEIGHT` and never shrink the counter charts below their label-only
/// minimum (`n` header bands).
fn clamp_graph_height(graph_height: f32, n_counters: usize, vh: f32) -> f32 {
    let min = n_counters as f32 * METRICS.header_height;
    let max = (vh - RULER_HEIGHT - BAR_WIDTH - FLAME_MIN_HEIGHT).max(min);
    graph_height.clamp(min, max)
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

/// One fitted counter lane in the graph section: its top (window y) and the
/// height of its chart area below the header band.
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
    Loaded(Rc<Profile>),
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
    source: Source,
    /// Horizontal time-to-pixel mapping (pan and zoom).
    view: Rc<Cell<TimeView>>,
    /// How far the lanes are scrolled up, in pixels.
    scroll_y: Rc<Cell<f32>>,
    /// Whether the initial fit-to-window view has been applied.
    fitted: Rc<Cell<bool>>,
    /// Viewport bounds captured during canvas paint, so handlers can map a window
    /// cursor to viewport space and bound the zoom to a fit.
    viewport: Rc<Cell<Bounds<Pixels>>>,
    /// Scroll-bar geometry set during paint, read by mouse handlers; `None` when
    /// that axis doesn't overflow and so shows no bar.
    vbar: Rc<Cell<Option<Scrollbar>>>,
    hbar: Rc<Cell<Option<Scrollbar>>>,
    drag: Drag,
    last_mouse: Point<Pixels>,
    /// Height of the bottom graph section, set by the splitter; clamped on use.
    graph_height: f32,
    /// Width of the left label gutter, set by its drag handle; clamped on use.
    gutter_width: f32,
    /// The slice under the cursor, if any, drawn as a tooltip.
    hover: Option<Hover>,
    /// The cursor position while it hovers the time-content column and no drag is
    /// in progress, drawn as a crosshair; `None` otherwise.
    crosshair: Option<Point<Pixels>>,
    /// Holds the background load task so it isn't cancelled on drop.
    _load_task: Task<()>,
}

impl TraceView {
    fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let load_task = load_in_background(
            cx,
            move || Profile::from_path(&path).map_err(|e| e.to_string()),
            |view, result, _| {
                view.source = match result {
                    Ok(profile) => Source::Loaded(Rc::new(profile)),
                    Err(err) => Source::Message(err),
                };
            },
        );
        Self {
            focus_handle,
            source: Source::Loading,
            view: Rc::new(Cell::new(TimeView {
                start_us: 0.0,
                px_per_us: 1.0,
            })),
            scroll_y: Rc::new(Cell::new(0.0)),
            fitted: Rc::new(Cell::new(false)),
            viewport: Rc::new(Cell::new(Bounds::default())),
            vbar: Rc::new(Cell::new(None)),
            hbar: Rc::new(Cell::new(None)),
            drag: Drag::None,
            last_mouse: Point::default(),
            graph_height: DEFAULT_GRAPH_HEIGHT,
            gutter_width: DEFAULT_GUTTER_WIDTH,
            hover: None,
            crosshair: None,
            _load_task: load_task,
        }
    }

    /// Builds the single-canvas timeline. The closure is `'static`, so it captures
    /// owned snapshots (`Rc`s and the const metrics) rather than borrowing the
    /// view. The lane layout is recomputed inside the closure — it is cheap and
    /// depends only on the profile, which the closure already owns.
    fn timeline(
        profile: Rc<Profile>,
        view: Rc<Cell<TimeView>>,
        scroll_y: Rc<Cell<f32>>,
        fitted: Rc<Cell<bool>>,
        viewport: Rc<Cell<Bounds<Pixels>>>,
        vbar: Rc<Cell<Option<Scrollbar>>>,
        hbar: Rc<Cell<Option<Scrollbar>>>,
        entity: Entity<TraceView>,
        graph_height: f32,
        gutter_width: f32,
        palette: Palette,
    ) -> Stateful<Div> {
        let prepaint = {
            let (profile, view, fitted, viewport) = (
                profile.clone(),
                view.clone(),
                fitted.clone(),
                viewport.clone(),
            );
            move |bounds: Bounds<Pixels>, _: &mut Window, _: &mut App| {
                viewport.set(bounds);
                // The time axis maps to the time column, past the gutter and clear
                // of the reserved vertical-bar column.
                let flame_total = flame_total_height(&lay_out_lanes(&profile, &METRICS));
                let regions = Regions::new(
                    bounds,
                    profile.counters.len(),
                    graph_height,
                    gutter_width,
                    flame_total,
                );
                let time_w = regions.time_w;
                if !fitted.get() {
                    view.set(TimeView::fit(profile.span, time_w, EDGE_MARGIN_PX));
                    fitted.set(true);
                }
                // Hold the bounds across a resize: keep the fit as the zoom floor
                // and the pan within the span for the current width.
                let mut v = view.get();
                v.px_per_us = v
                    .px_per_us
                    .max(fit_px_per_us(profile.span, time_w, EDGE_MARGIN_PX));
                v.clamp_to(profile.span, time_w, EDGE_MARGIN_PX);
                view.set(v);
            }
        };
        let flame_total = flame_total_height(&lay_out_lanes(&profile, &METRICS));
        let regions = Regions::new(
            viewport.get(),
            profile.counters.len(),
            graph_height,
            gutter_width,
            flame_total,
        );
        let origin = viewport.get().origin;
        // A thin overlay strip at the splitter that begins a resize drag, placed
        // from the last painted bounds and the stored graph height. The canvas
        // recomputes the same regions when it paints. A double-click resets the
        // graph height to its default.
        let splitter = (profile.counters.len() > 0).then(|| {
            let y = regions.graph_top - BAR_WIDTH / 2.0;
            let drag_entity = entity.clone();
            div()
                .id("trace-splitter")
                .absolute()
                .left(px(regions.content_left - f32::from(origin.x)))
                .top(px(y - f32::from(origin.y)))
                .w(px(regions.time_w))
                .h(px(BAR_WIDTH))
                .cursor_row_resize()
                .on_mouse_down(
                    MouseButton::Left,
                    move |e: &MouseDownEvent, _: &mut Window, cx: &mut App| {
                        drag_entity.update(cx, |this, cx| {
                            if e.click_count == 2 {
                                this.graph_height = DEFAULT_GRAPH_HEIGHT;
                            } else {
                                this.drag = Drag::Splitter;
                            }
                            cx.notify();
                        });
                        cx.stop_propagation();
                    },
                )
        });
        // A thin overlay strip at the gutter's right edge that begins a width
        // drag; a double-click resets the gutter to its default width.
        let gutter_handle = {
            let drag_entity = entity.clone();
            div()
                .id("trace-gutter-handle")
                .absolute()
                .left(px(regions.content_left
                    - f32::from(origin.x)
                    - GUTTER_HANDLE_WIDTH / 2.0))
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
        };
        div()
            .id("trace-viewport")
            .relative()
            .size_full()
            .overflow_hidden()
            .child(
                canvas(prepaint, move |bounds, _, window, cx| {
                    let layout = lay_out_lanes(&profile, &METRICS);
                    let tv = view.get();
                    let n_counters = profile.counters.len();
                    let graph_height = entity.read(cx).graph_height;
                    let gutter_width = entity.read(cx).gutter_width;
                    // Thread lanes only; the graph section places counters itself.
                    let flame_total = flame_total_height(&layout);
                    let regions =
                        Regions::new(bounds, n_counters, graph_height, gutter_width, flame_total);
                    let scroll = scroll_y.get();
                    let vp = Viewport {
                        width: regions.time_w,
                        height: regions.flame_h(),
                        scroll_y: scroll,
                    };

                    let ticks = ruler_ticks(tv, regions.time_w);
                    // Gridlines and phase-marker lines span both sections.
                    paint_gridlines(
                        &ticks,
                        regions.content_left,
                        regions.flame_top,
                        regions.body_bottom,
                        &palette,
                        window,
                    );

                    // The flame section clips its scrolled lanes to its band so
                    // rows panned past the splitter don't bleed into the graph.
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
                        for vs in
                            visible_slices(&profile, &layout, &tv, &vp, &METRICS, MIN_SLICE_PX)
                        {
                            let slice = &profile.threads[vs.track].slices[vs.slice];
                            paint_slice(
                                slice,
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

                    paint_graph(&profile.counters, tv, regions, &palette, window);
                    if regions.has_graph {
                        paint_splitter(regions, &palette, window);
                    }
                    paint_markers(&profile.phase_markers, tv, regions, &palette, window, cx);
                    paint_scrollbars(
                        profile.span,
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
                    // it, then carries the lane and counter names.
                    paint_gutter(
                        &profile,
                        &layout,
                        regions,
                        gutter_width,
                        scroll,
                        &palette,
                        window,
                        cx,
                    );

                    // The ruler paints over scrolled-up rows; the tooltip paints
                    // over everything.
                    paint_ruler(&ticks, regions, bounds, &palette, window, cx);
                    if let Some(at) = entity.read(cx).crosshair {
                        if f32::from(at.x) >= regions.content_left {
                            paint_crosshair(
                                &profile.counters,
                                tv,
                                regions,
                                at,
                                &palette,
                                window,
                                cx,
                            );
                        }
                    }
                    if let Some(h) = entity.read(cx).hover {
                        let slice = &profile.threads[h.track].slices[h.slice];
                        paint_tooltip(slice, h.at, bounds, &palette, window, cx);
                    }
                    if !matches!(entity.read(cx).drag, Drag::None) {
                        track_drag(window, &entity, on_drag, end_drag);
                    }
                })
                .absolute()
                .left_0()
                .top_0()
                .size_full(),
            )
            .children(splitter)
            .child(gutter_handle)
    }

    fn status(&self) -> String {
        match &self.source {
            Source::Loaded(p) => {
                let slices: usize = p.threads.iter().map(|t| t.slices.len()).sum();
                format!(
                    "{} threads · {} slices · {:.3}s",
                    p.threads.len(),
                    slices,
                    span_seconds(p.span),
                )
            }
            Source::Loading => "Loading…".into(),
            Source::Message(_) => "No profile".into(),
        }
    }

    /// The pixel ceiling for an upward (toward zero) scroll: the flame section's
    /// thread-lane height past its on-screen height.
    fn max_scroll(&self) -> f32 {
        let Source::Loaded(p) = &self.source else {
            return 0.0;
        };
        let total = flame_total_height(&lay_out_lanes(p, &METRICS));
        let regions = Regions::new(
            self.viewport.get(),
            p.counters.len(),
            self.graph_height,
            self.gutter_width,
            total,
        );
        (total - regions.flame_h()).max(0.0)
    }
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
        };
        let body = match &self.source {
            Source::Loaded(profile) => div().flex_1().flex().overflow_hidden().child(
                Self::timeline(
                    profile.clone(),
                    self.view.clone(),
                    self.scroll_y.clone(),
                    self.fitted.clone(),
                    self.viewport.clone(),
                    self.vbar.clone(),
                    self.hbar.clone(),
                    cx.entity(),
                    self.graph_height,
                    self.gutter_width,
                    palette,
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, e: &MouseDownEvent, _, cx| {
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
                    // Drop the crosshair and slice hover when the cursor leaves.
                    if !*hovered {
                        let cleared = this.crosshair.take().is_some();
                        let cleared = this.hover.take().is_some() || cleared;
                        if cleared {
                            cx.notify();
                        }
                    }
                })),
            ),
            Source::Loading => div()
                .flex_1()
                .flex()
                .flex_col()
                .justify_center()
                .items_center()
                .gap_3()
                .child(
                    bviz_ui_assets::icon("circle-notch")
                        .size(px(36.))
                        .text_color(colors.text)
                        .with_animation(
                            "spinner",
                            Animation::new(Duration::from_millis(900)).repeat(),
                            |icon, delta| {
                                icon.with_transformation(Transformation::rotate(percentage(delta)))
                            },
                        ),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(colors.disabled)
                        .child("Loading…"),
                ),
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
        let Source::Loaded(profile) = &self.source else {
            return;
        };
        let span = profile.span;
        let vp = self.viewport.get();
        let flame_total = flame_total_height(&lay_out_lanes(profile, &METRICS));
        let regions = Regions::new(
            vp,
            profile.counters.len(),
            self.graph_height,
            self.gutter_width,
            flame_total,
        );
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
            v.zoom_at(rel_x, 1.1_f64.powf(dy_lines as f64));
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

    /// Whether `pos` lies in the time-content column (right of the gutter), the
    /// region the crosshair shows over.
    fn in_content(&self, pos: Point<Pixels>) -> bool {
        let Source::Loaded(profile) = &self.source else {
            return false;
        };
        let flame_total = flame_total_height(&lay_out_lanes(profile, &METRICS));
        let regions = Regions::new(
            self.viewport.get(),
            profile.counters.len(),
            self.graph_height,
            self.gutter_width,
            flame_total,
        );
        f32::from(pos.x) >= regions.content_left && f32::from(pos.x) < regions.time_right
    }

    /// The slice under a window-space cursor, inverting the paint transform:
    /// window → flame section → (lane, depth row) and time. `None` over the
    /// gutter, the ruler, the graph section, a lane header, or empty space.
    fn slice_at(&self, pos: Point<Pixels>) -> Option<(usize, usize)> {
        let Source::Loaded(profile) = &self.source else {
            return None;
        };
        let layout = lay_out_lanes(profile, &METRICS);
        let flame_total = flame_total_height(&layout);
        let regions = Regions::new(
            self.viewport.get(),
            profile.counters.len(),
            self.graph_height,
            self.gutter_width,
            flame_total,
        );
        let x = f32::from(pos.x);
        let y = f32::from(pos.y);
        // The flame band, past the gutter, clear of the reserved vertical-bar
        // column and the graph section.
        if x < regions.content_left
            || x >= regions.time_right
            || y < regions.flame_top
            || y >= regions.graph_top
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
        let t = self.view.get().t_of(x - regions.content_left);
        profile.threads[lane.track]
            .slices
            .iter()
            .position(|s| {
                s.depth == depth && t >= s.start_us as f64 && t <= (s.start_us + s.dur_us) as f64
            })
            .map(|slice| (lane.track, slice))
    }

    /// Begins a drag at `pos`: a scroll thumb or track if the press hit one (the
    /// thumb is grabbed where pressed, a track click centers the thumb there),
    /// otherwise a canvas pan.
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
}

/// Routes an in-progress drag to its target: panning the canvas or moving a
/// scroll thumb.
fn on_drag(view: &mut TraceView, event: &MouseMoveEvent, cx: &mut Context<TraceView>) {
    match view.drag {
        Drag::Pan => {
            let Source::Loaded(profile) = &view.source else {
                return;
            };
            let span = profile.span;
            let flame_total = flame_total_height(&lay_out_lanes(profile, &METRICS));
            let regions = Regions::new(
                view.viewport.get(),
                profile.counters.len(),
                view.graph_height,
                view.gutter_width,
                flame_total,
            );
            let time_w = regions.time_w;
            let dx = f32::from(event.position.x) - f32::from(view.last_mouse.x);
            let dy = f32::from(event.position.y) - f32::from(view.last_mouse.y);
            let mut v = view.view.get();
            v.pan_px(dx);
            v.clamp_to(span, time_w, EDGE_MARGIN_PX);
            view.view.set(v);
            let scrolled = (view.scroll_y.get() - dy).clamp(0.0, view.max_scroll());
            view.scroll_y.set(scrolled);
            view.last_mouse = event.position;
        }
        Drag::VThumb(grab) => scroll_thumb_to(view, event.position, grab),
        Drag::HThumb(grab) => pan_thumb_to(view, event.position, grab),
        Drag::Splitter => {
            let Source::Loaded(profile) = &view.source else {
                return;
            };
            let vp = view.viewport.get();
            let body_bottom = f32::from(vp.origin.y) + f32::from(vp.size.height) - BAR_WIDTH;
            // The splitter follows the cursor: the graph spans from it down to the
            // horizontal-bar top. Clamped to keep both sections usable.
            view.graph_height = clamp_graph_height(
                body_bottom - f32::from(event.position.y),
                profile.counters.len(),
                f32::from(vp.size.height),
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

/// Maps a vertical thumb drag back to a scroll offset.
fn scroll_thumb_to(view: &mut TraceView, pos: Point<Pixels>, grab: f32) {
    let Some(bar) = view.vbar.get() else {
        return;
    };
    view.scroll_y.set(bar.offset_at(pos, grab));
}

/// Maps a horizontal thumb drag back to a pan start time, then clamps the view.
fn pan_thumb_to(view: &mut TraceView, pos: Point<Pixels>, grab: f32) {
    let Source::Loaded(profile) = &view.source else {
        return;
    };
    let span = profile.span;
    let flame_total = flame_total_height(&lay_out_lanes(profile, &METRICS));
    let regions = Regions::new(
        view.viewport.get(),
        profile.counters.len(),
        view.graph_height,
        view.gutter_width,
        flame_total,
    );
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
/// label spanning `[left, right]`, falling back to row 0 when none is clear, and
/// records the label's extent in the chosen row. Keeps clustered labels visible
/// by staggering them down instead of dropping any.
fn assign_marker_row(rows: &mut [f32], left: f32, right: f32) -> usize {
    let row = (0..rows.len()).find(|&r| left >= rows[r]).unwrap_or(0);
    rows[row] = right;
    row
}

/// Draws each build-phase boundary as a vertical line spanning both sections
/// with its name labeled near the top, confined to the time axis and skipping
/// any off-screen. A label that would run past the right edge is flipped to the
/// left of its line; clustered labels stagger across a few stacked rows so none
/// is dropped.
fn paint_markers(
    markers: &[PhaseMarker],
    tv: TimeView,
    regions: Regions,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let top = regions.flame_top;
    let height = regions.body_bottom - regions.flame_top;
    let font = bviz_ui_assets::text_metrics(window).font;
    let fs = px(MARKER_LABEL_SIZE);
    let row_h = MARKER_LABEL_SIZE + 2.0;
    // Each row's last occupied right-extent, so a label drops to the lowest row
    // that clears the previous label there.
    let mut rows = [f32::MIN; MARKER_LABEL_ROWS];
    for marker in markers {
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
        let line = bviz_ui_assets::shape_line(
            window,
            marker.name.clone(),
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

/// Paints the visible divider between the flame and graph sections, centered on
/// the boundary the overlay drag strip covers.
fn paint_splitter(regions: Regions, palette: &Palette, window: &mut Window) {
    let bar = Bounds::new(
        point(
            px(regions.content_left),
            px(regions.graph_top - SPLITTER_THICKNESS / 2.0),
        ),
        size(px(regions.time_w), px(SPLITTER_THICKNESS)),
    );
    window.paint_quad(quad(
        bar,
        px(0.),
        palette.divider,
        px(0.),
        Hsla::from(palette.border),
        BorderStyle::Solid,
    ));
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
    // The horizontal track runs the full time axis, ending left of the reserved
    // vertical-bar column.
    let hlayout = Scrollbar::new(
        Axis::Horizontal,
        Bounds::new(
            point(px(regions.content_left), px(regions.body_bottom)),
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

/// Paints the opaque left gutter over any content that panned or scrolled under
/// it, then the thread-lane and counter names left-aligned within it: thread
/// names track the flame section's vertical scroll (clipped to its band), and
/// counter names sit at their fitted graph lanes.
fn paint_gutter(
    profile: &Profile,
    layout: &LaneLayout,
    regions: Regions,
    gutter_width: f32,
    scroll_y: f32,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let gutter_left = regions.content_left - gutter_width;
    let strip = Bounds::new(
        point(px(gutter_left), px(regions.flame_top)),
        size(
            px(gutter_width),
            px(regions.body_bottom - regions.flame_top),
        ),
    );
    window.paint_quad(quad(
        strip,
        px(0.),
        palette.header_band,
        px(0.),
        Hsla::from(palette.border),
        BorderStyle::Solid,
    ));
    // A subtle line at the gutter's right edge, under the drag handle.
    window.paint_quad(quad(
        Bounds::new(
            point(px(regions.content_left - 1.0), px(regions.flame_top)),
            size(px(1.0), px(regions.body_bottom - regions.flame_top)),
        ),
        px(0.),
        palette.divider,
        px(0.),
        Hsla::from(palette.border),
        BorderStyle::Solid,
    ));

    let font = bviz_ui_assets::text_metrics(window).font;
    let fs = px(GUTTER_LABEL_SIZE);
    let avail = gutter_width - 2.0 * SLICE_TEXT_PAD;
    let label = |name: &str, ty: f32, window: &mut Window, cx: &mut App| {
        let line = shape_truncated(name, avail, font.clone(), fs, palette.label.into(), window);
        let _ = line.paint(
            point(px(gutter_left + SLICE_TEXT_PAD), px(ty)),
            fs,
            TextAlign::Left,
            None,
            window,
            cx,
        );
    };

    // Thread names scroll with the flame lanes, clipped to the flame band, each
    // centered against its lane's full height.
    let flame_mask = ContentMask {
        bounds: Bounds::new(
            point(px(gutter_left), px(regions.flame_top)),
            size(px(gutter_width), px(regions.flame_h())),
        ),
    };
    window.with_content_mask(Some(flame_mask), |window| {
        for lane in &layout.lanes {
            let top = regions.flame_top + lane.top - scroll_y;
            if top + lane.height < regions.flame_top || top > regions.graph_top {
                continue;
            }
            let ty = top + (lane.height - GUTTER_LABEL_SIZE) / 2.0;
            label(&profile.threads[lane.track].name, ty, window, cx);
        }
    });

    // Counter names sit at the chart baseline of their lane, so they associate
    // with low values rather than floating at the lane top.
    for gl in graph_lanes(
        profile.counters.len(),
        regions.graph_top,
        graph_height_of(regions),
    ) {
        let lane_bottom = gl.top + METRICS.header_height + gl.chart_h;
        let ty = lane_bottom - GUTTER_LABEL_SIZE - SLICE_TEXT_PAD;
        label(&profile.counters[gl.lane].name, ty, window, cx);
    }
}

/// The graph section's height between the splitter and the horizontal bar.
fn graph_height_of(regions: Regions) -> f32 {
    if regions.has_graph {
        (regions.body_bottom - regions.graph_top).max(0.0)
    } else {
        0.0
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
    let chars = text.chars().count();
    let keep =
        ((chars as f32 * avail / f32::from(line.width())).floor() as usize).saturating_sub(1);
    let mut truncated: String = text.chars().take(keep).collect();
    truncated.push('…');
    bviz_ui_assets::shape_line(window, truncated, font, fs, color)
}

/// Paints a slice quad and, if wide enough, its truncated name.
fn paint_slice(
    slice: &bviz_trace_profile::Slice,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    content_left: f32,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let origin = point(px(content_left + x), px(y));
    let rect = Bounds::new(origin, size(px(w), px(h)));
    let fill = category_color(&slice.category, palette);
    window.paint_quad(quad(
        rect,
        px(SLICE_RADIUS),
        fill,
        px(SLICE_BORDER),
        Hsla::from(palette.border),
        BorderStyle::Solid,
    ));
    if w < LABEL_MIN_PX {
        return;
    }
    let avail = w - 2. * SLICE_TEXT_PAD;
    let font = bviz_ui_assets::text_metrics(window).font;
    let fs = px(h - 2. * SLICE_TEXT_PAD);
    let color = label_on(fill);
    let line = shape_truncated(&slice.name, avail, font, fs, color, window);
    let ty = origin.y + px((h - f32::from(fs)) / 2.);
    let _ = line.paint(
        point(origin.x + px(SLICE_TEXT_PAD), ty),
        fs,
        TextAlign::Left,
        None,
        window,
        cx,
    );
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
    slice: &bviz_trace_profile::Slice,
    at: Point<Pixels>,
    bounds: Bounds<Pixels>,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let mut lines = vec![
        slice.name.clone(),
        format!("{} · {}", slice.category, format_duration(slice.dur_us)),
    ];
    if let Some(target) = &slice.args.target {
        lines.push(format!("target: {target}"));
    }
    if let Some(mnemonic) = &slice.args.mnemonic {
        lines.push(format!("mnemonic: {mnemonic}"));
    }
    if let Some(configuration) = &slice.args.configuration {
        lines.push(format!("configuration: {configuration}"));
    }
    for (key, value) in &slice.args.others {
        lines.push(format!("{key}: {value}"));
    }

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

/// A deterministic per-category fill: the category hashes to a hue, with
/// saturation and lightness fixed for the theme so every category is
/// distinguishable yet legible. Same category → same color across the profile.
fn category_color(cat: &str, palette: &Palette) -> Hsla {
    let lightness = if luminance(palette.background) < 0.5 {
        CATEGORY_LIGHTNESS_DARK
    } else {
        CATEGORY_LIGHTNESS_LIGHT
    };
    hsla(hue_of(cat), CATEGORY_SATURATION, lightness, 1.0)
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

/// Paints the bottom graph section: every counter lane fitted to the section
/// height. Each lane is a header band over a chart area that holds the lane's
/// series as stacked filled areas scaled to the chart height and the lane's peak
/// stacked total. Charts are confined to the content column.
fn paint_graph(
    counters: &[CounterLane],
    tv: TimeView,
    regions: Regions,
    palette: &Palette,
    window: &mut Window,
) {
    if !regions.has_graph {
        return;
    }
    let lanes = graph_lanes(counters.len(), regions.graph_top, graph_height_of(regions));
    let content_mask = ContentMask {
        bounds: Bounds::new(
            point(px(regions.content_left), px(regions.graph_top)),
            size(
                px(regions.time_w),
                px(regions.body_bottom - regions.graph_top),
            ),
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

        // Series in a lane stack (per the Chrome counter-event model): the peak
        // is the largest stacked total over time, and each series is drawn as a
        // band between the running baseline and baseline + value.
        let chart_bottom = gl.top + METRICS.header_height + gl.chart_h;
        let mut totals: BTreeMap<i64, f64> = BTreeMap::new();
        for series in &counter.series {
            for p in &series.points {
                *totals.entry(p.ts_us).or_default() += p.value;
            }
        }
        let peak = totals.values().copied().fold(0.0_f64, f64::max);
        if peak <= 0.0 {
            continue;
        }
        let scale = gl.chart_h / peak as f32;
        let x_of = |ts| px(regions.content_left + tv.x_of(ts));
        window.with_content_mask(Some(content_mask), |window| {
            let mut baseline: BTreeMap<i64, f64> = BTreeMap::new();
            for series in &counter.series {
                if series.points.len() < 2 {
                    continue;
                }
                let mut area = PathBuilder::fill();
                let mut first = true;
                for p in &series.points {
                    let base = *baseline.get(&p.ts_us).unwrap_or(&0.0);
                    let top = px(chart_bottom - (base + p.value) as f32 * scale);
                    let at = point(x_of(p.ts_us), top);
                    if first {
                        area.move_to(at);
                        first = false;
                    } else {
                        area.line_to(at);
                    }
                }
                for p in series.points.iter().rev() {
                    let base = *baseline.get(&p.ts_us).unwrap_or(&0.0);
                    area.line_to(point(x_of(p.ts_us), px(chart_bottom - base as f32 * scale)));
                }
                if let Ok(path) = area.build() {
                    window.paint_path(path, counter_color(&series.name));
                }
                for p in &series.points {
                    *baseline.entry(p.ts_us).or_default() += p.value;
                }
            }
        });
    }
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
    let hi = points.iter().position(|p| p.ts_us as f64 >= t)?;
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
/// series whose range covers the time — a dot where the crosshair meets its
/// stacked line plus a value label. Stacking matches [`paint_graph`].
fn paint_crosshair(
    counters: &[CounterLane],
    tv: TimeView,
    regions: Regions,
    at: Point<Pixels>,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let cx_px = f32::from(at.x).min(regions.time_right);
    let t = tv.t_of(cx_px - regions.content_left);

    let line = Bounds::new(
        point(px(cx_px), px(regions.flame_top)),
        size(px(1.), px(regions.body_bottom - regions.flame_top)),
    );
    window.paint_quad(quad(
        line,
        px(0.),
        palette.crosshair,
        px(0.),
        Hsla::from(palette.border),
        BorderStyle::Solid,
    ));

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

    if !regions.has_graph {
        return;
    }
    let lanes = graph_lanes(counters.len(), regions.graph_top, graph_height_of(regions));
    for gl in lanes {
        if gl.chart_h <= 0.0 {
            continue;
        }
        let counter = &counters[gl.lane];
        // The lane peak — the largest stacked total over time — scales the chart,
        // exactly as paint_graph derives it.
        let mut totals: BTreeMap<i64, f64> = BTreeMap::new();
        for series in &counter.series {
            for p in &series.points {
                *totals.entry(p.ts_us).or_default() += p.value;
            }
        }
        let peak = totals.values().copied().fold(0.0_f64, f64::max);
        if peak <= 0.0 {
            continue;
        }
        let scale = gl.chart_h / peak as f32;
        let chart_top = gl.top + METRICS.header_height;
        let chart_bottom = chart_top + gl.chart_h;

        // Walk the series in draw order, accumulating each one's value at `t` onto
        // the running baseline so its dot lands on the stacked line. Each label
        // sits at its own dot, so its value and dot never get out of order.
        let unit = unit_for(&counter.name);
        let mut base = 0.0_f64;
        let mut dots: Vec<(f32, f64, Hsla)> = Vec::new();
        for series in &counter.series {
            let Some(value) = sample_at(&series.points, t) else {
                continue;
            };
            base += value;
            let dot_y = chart_bottom - base as f32 * scale;
            dots.push((dot_y, value, counter_color(&series.name)));
        }
        for &(dot_y, _, color) in &dots {
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
        }

        // Each label's top sits at its dot, clamped into the chart and pushed
        // apart from its neighbours so stacked values stay legible. The series
        // stack bottom-up, so the dots are already sorted descending by y.
        let max_label_y = (chart_bottom - CROSSHAIR_VALUE_SIZE).max(chart_top);
        let mut label_ys: Vec<f32> = dots
            .iter()
            .map(|&(dot_y, ..)| dot_y.clamp(chart_top, max_label_y))
            .collect();
        declutter_labels(
            &mut label_ys,
            CROSSHAIR_VALUE_SIZE + 1.0,
            chart_top,
            max_label_y,
        );
        for (&label_y, &(_, value, color)) in label_ys.iter().zip(&dots) {
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
}

/// Spreads label tops apart so consecutive ones (by y) keep at least `min_gap`,
/// in place and preserving each entry's index, bounded to `[top, bottom]`. Works
/// in whatever order the entries arrive: sorts a view of them by y, sweeps down
/// enforcing the gap, then sweeps back up to pull any run that overran `bottom`
/// back under it.
fn declutter_labels(ys: &mut [f32], min_gap: f32, top: f32, bottom: f32) {
    let mut order: Vec<usize> = (0..ys.len()).collect();
    order.sort_by(|&a, &b| ys[a].total_cmp(&ys[b]));
    // Push each label down to clear the one above it.
    for w in 1..order.len() {
        let (prev, cur) = (order[w - 1], order[w]);
        ys[cur] = ys[cur].max(ys[prev] + min_gap);
    }
    // If the run overran the bottom, pull the whole cluster back up by the same
    // gap from the bottom-most upward, then hold the top edge no higher than
    // `top`.
    if let Some(&last) = order.last() {
        ys[last] = ys[last].min(bottom);
        for w in (0..order.len() - 1).rev() {
            let (cur, next) = (order[w], order[w + 1]);
            ys[cur] = ys[cur].min(ys[next] - min_gap);
        }
        if let Some(&first) = order.first() {
            let shift = (top - ys[first]).max(0.0);
            for &i in &order {
                ys[i] += shift;
            }
        }
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
    let path = Cli::parse().profile;
    bviz_ui_shell::run(
        "bazel_viz trace",
        size(px(1280.), px(820.)),
        move |window, cx| cx.new(|cx| TraceView::new(path, window, cx)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette() -> Palette {
        let dark = Rgba {
            r: 0.1,
            g: 0.1,
            b: 0.1,
            a: 1.0,
        };
        Palette {
            border: dark,
            label: dark,
            background: dark,
            header_band: dark,
            gridline: dark,
            marker: dark,
            scroll_track: dark,
            scroll_thumb: dark,
            divider: dark,
            panel: dark,
            crosshair: dark,
        }
    }

    #[test]
    fn category_color_is_deterministic() {
        let p = palette();
        assert_eq!(
            category_color("skyframe", &p),
            category_color("skyframe", &p)
        );
        assert_ne!(category_color("skyframe", &p), category_color("action", &p));
    }

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
    fn clamp_graph_height_bounds_both_sections() {
        // A tall window: clamps to the requested height, untouched.
        assert_eq!(clamp_graph_height(220.0, 3, 800.0), 220.0);
        // The label-only floor: never below n header bands.
        let min = 3.0 * METRICS.header_height;
        assert_eq!(clamp_graph_height(0.0, 3, 800.0), min);
        // The flame-section ceiling: leaves FLAME_MIN_HEIGHT for the flame band.
        let vh = 200.0;
        let max = vh - RULER_HEIGHT - BAR_WIDTH - FLAME_MIN_HEIGHT;
        assert_eq!(clamp_graph_height(1000.0, 1, vh), max);
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
    fn declutter_labels_spreads_and_bounds() {
        // Three labels crammed together spread to the min gap, in y order.
        let mut ys = [100.0, 102.0, 101.0];
        declutter_labels(&mut ys, 10.0, 0.0, 1000.0);
        let mut sorted = ys;
        sorted.sort_by(f32::total_cmp);
        assert!(sorted[1] - sorted[0] >= 10.0 - f32::EPSILON);
        assert!(sorted[2] - sorted[1] >= 10.0 - f32::EPSILON);
        // The pairing is preserved: the originally largest stays largest.
        assert_eq!(ys[1], sorted[2]);

        // Already-separated labels are left alone.
        let mut clear = [0.0, 20.0, 40.0];
        declutter_labels(&mut clear, 10.0, 0.0, 1000.0);
        assert_eq!(clear, [0.0, 20.0, 40.0]);

        // A run that would overrun the bottom is pulled back under it.
        let mut tight = [95.0, 98.0];
        declutter_labels(&mut tight, 10.0, 0.0, 100.0);
        assert!(tight.iter().all(|&y| y <= 100.0));
        assert!((tight[1] - tight[0]).abs() >= 10.0 - f32::EPSILON);
    }

    #[test]
    fn assign_marker_row_staggers_overlaps() {
        let mut rows = [f32::MIN; MARKER_LABEL_ROWS];
        // The first label takes row 0.
        assert_eq!(assign_marker_row(&mut rows, 0.0, 50.0), 0);
        // An overlapping label drops to the next free row.
        assert_eq!(assign_marker_row(&mut rows, 10.0, 60.0), 1);
        assert_eq!(assign_marker_row(&mut rows, 20.0, 70.0), 2);
        // A label clear of row 0's extent reuses it.
        assert_eq!(assign_marker_row(&mut rows, 60.0, 100.0), 0);
        // With every row occupied past the label's left, it falls back to row 0.
        let mut full = [100.0; MARKER_LABEL_ROWS];
        assert_eq!(assign_marker_row(&mut full, 0.0, 40.0), 0);
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
