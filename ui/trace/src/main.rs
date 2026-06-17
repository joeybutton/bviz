//! Loads a Bazel `--profile` trace (`.profile.gz` or `.json`) into
//! `bviz_trace_profile` and opens a window rendering its thread tracks as a
//! flame-chart timeline: time on the x axis, stacked call depth on the y axis,
//! with horizontal zoom/pan and vertical scroll.

use bviz_trace_layout::{
    CounterPos, Lane, LaneMetrics, TimeView, Viewport, lay_out_lanes, visible_slices,
};
use bviz_trace_profile::{CounterLane, PhaseMarker, Profile, TimeSpan};
use bviz_ui_components::{load_in_background, track_drag};
use bviz_ui_shell::Chrome;
use clap::Parser;
use gpui::{
    Animation, AnimationExt, App, BorderStyle, Bounds, Context, Div, Entity, FocusHandle, Hsla,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels,
    Point, Render, Rgba, ScrollDelta, ScrollWheelEvent, Stateful, Task, TextAlign, Transformation,
    Window, canvas, div, hsla, percentage, point, prelude::*, px, quad, size,
};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

/// Vertical sizing of the stacked thread and counter lanes, in pixels.
const METRICS: LaneMetrics = LaneMetrics {
    header_height: 16.0,
    row_height: 18.0,
    lane_gap: 6.0,
    counter_height: 36.0,
};

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

/// Hover-tooltip text size, inner padding, gap between lines, and offset from the
/// cursor.
const TOOLTIP_TEXT_SIZE: f32 = 12.0;
const TOOLTIP_PAD: f32 = 6.0;
const TOOLTIP_LINE_GAP: f32 = 4.0;
const TOOLTIP_OFFSET: f32 = 12.0;

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
    /// Opaque fill of the hover tooltip panel.
    panel: Rgba,
}

/// A painted scroll bar's geometry in window coordinates, kept so mouse events
/// can hit-test and drag it. The scalars run along the bar's long axis (y for
/// the vertical bar, x for the horizontal).
#[derive(Clone, Copy)]
struct BarLayout {
    track: Bounds<Pixels>,
    thumb: Bounds<Pixels>,
    track_start: f32,
    track_len: f32,
    thumb_start: f32,
    thumb_len: f32,
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
}

/// The slice under the cursor and where the cursor was when it was picked, so a
/// tooltip can be drawn for it.
#[derive(Clone, Copy)]
struct Hover {
    track: usize,
    slice: usize,
    at: Point<Pixels>,
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
    vbar: Rc<Cell<Option<BarLayout>>>,
    hbar: Rc<Cell<Option<BarLayout>>>,
    drag: Drag,
    last_mouse: Point<Pixels>,
    /// The slice under the cursor, if any, drawn as a tooltip.
    hover: Option<Hover>,
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
            hover: None,
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
        vbar: Rc<Cell<Option<BarLayout>>>,
        hbar: Rc<Cell<Option<BarLayout>>>,
        entity: Entity<TraceView>,
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
                let width = f32::from(bounds.size.width);
                if !fitted.get() {
                    view.set(TimeView::fit(profile.span, width, EDGE_MARGIN_PX));
                    fitted.set(true);
                }
                // Hold the bounds across a resize: keep the fit as the zoom floor
                // and the pan within the span for the current width.
                let mut v = view.get();
                v.px_per_us = v
                    .px_per_us
                    .max(fit_px_per_us(profile.span, width, EDGE_MARGIN_PX));
                v.clamp_to(profile.span, width, EDGE_MARGIN_PX);
                view.set(v);
            }
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
                    let vw = f32::from(bounds.size.width);
                    // The ruler band occupies the top; the lanes scroll below it.
                    let vp = Viewport {
                        width: vw,
                        height: f32::from(bounds.size.height) - RULER_HEIGHT,
                        scroll_y: scroll_y.get(),
                    };

                    let ticks = ruler_ticks(tv, vw);
                    paint_gridlines(&ticks, bounds, &palette, window);
                    for vs in visible_slices(&profile, &layout, &tv, &vp, &METRICS, MIN_SLICE_PX) {
                        let slice = &profile.threads[vs.track].slices[vs.slice];
                        paint_slice(
                            slice,
                            vs.x,
                            vs.y + RULER_HEIGHT,
                            vs.w,
                            vs.h,
                            bounds,
                            &palette,
                            window,
                            cx,
                        );
                    }
                    // Headers sit at the left edge so the thread name survives any
                    // pan; they paint over the slices for legibility.
                    for lane in &layout.lanes {
                        paint_header(
                            &profile.threads[lane.track].name,
                            lane,
                            vp.scroll_y,
                            bounds,
                            &palette,
                            window,
                            cx,
                        );
                    }
                    paint_counters(
                        &profile.counters,
                        &layout.counters,
                        tv,
                        vp.scroll_y,
                        bounds,
                        &palette,
                        window,
                        cx,
                    );
                    paint_markers(&profile.phase_markers, tv, bounds, &palette, window, cx);
                    paint_scrollbars(
                        profile.span,
                        tv,
                        layout.total_height,
                        bounds,
                        vp.scroll_y,
                        &palette,
                        &vbar,
                        &hbar,
                        window,
                    );
                    // The ruler paints over scrolled-up rows; the tooltip paints
                    // over everything.
                    paint_ruler(&ticks, bounds, &palette, window, cx);
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

    /// The pixel ceiling for an upward (toward zero) scroll, given the laid-out
    /// total height and the current viewport height.
    fn max_scroll(&self) -> f32 {
        let Source::Loaded(p) = &self.source else {
            return 0.0;
        };
        let total = lay_out_lanes(p, &METRICS).total_height;
        let lane_area = f32::from(self.viewport.get().size.height) - RULER_HEIGHT;
        (total - lane_area).max(0.0)
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
            panel: colors.container,
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
                .on_mouse_move(cx.listener(Self::on_mouse_move)),
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
        let width = f32::from(vp.size.width);
        let (dx, dy) = match e.delta {
            ScrollDelta::Lines(p) => (p.x * LINE_SCROLL_PX, p.y * LINE_SCROLL_PX),
            ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
        };
        if e.modifiers.control || e.modifiers.platform {
            // Horizontal zoom anchored at the cursor, floored at the fit so the
            // whole span can't be zoomed out of view.
            let rel_x = f32::from(e.position.x) - f32::from(vp.origin.x);
            let dy_lines = dy / LINE_SCROLL_PX;
            let mut v = self.view.get();
            v.zoom_at(rel_x, 1.1_f64.powf(dy_lines as f64));
            v.px_per_us = v
                .px_per_us
                .clamp(fit_px_per_us(span, width, EDGE_MARGIN_PX), MAX_PX_PER_US);
            v.clamp_to(span, width, EDGE_MARGIN_PX);
            self.view.set(v);
        } else {
            let scrolled = (self.scroll_y.get() - dy).clamp(0.0, self.max_scroll());
            self.scroll_y.set(scrolled);
            let mut v = self.view.get();
            v.pan_px(dx);
            v.clamp_to(span, width, EDGE_MARGIN_PX);
            self.view.set(v);
        }
        cx.notify();
    }

    /// Tracks the slice under the cursor while not dragging, repainting only when
    /// the hovered slice changes.
    fn on_mouse_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.drag, Drag::None) {
            return;
        }
        let hit = self.slice_at(e.position);
        let unchanged = match (self.hover, hit) {
            (Some(h), Some((track, slice))) => h.track == track && h.slice == slice,
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return;
        }
        self.hover = hit.map(|(track, slice)| Hover {
            track,
            slice,
            at: e.position,
        });
        cx.notify();
    }

    /// The slice under a window-space cursor, inverting the paint transform:
    /// window → lane area → (lane, depth row) and time. `None` over a header, the
    /// ruler, a counter lane, or empty space.
    fn slice_at(&self, pos: Point<Pixels>) -> Option<(usize, usize)> {
        let Source::Loaded(profile) = &self.source else {
            return None;
        };
        let origin = self.viewport.get().origin;
        let content_y = f32::from(pos.y) - f32::from(origin.y) - RULER_HEIGHT + self.scroll_y.get();
        if content_y < 0.0 {
            return None;
        }
        let layout = lay_out_lanes(profile, &METRICS);
        let lane = layout
            .lanes
            .iter()
            .find(|l| content_y >= l.top && content_y < l.top + l.height)?;
        let rows_top = lane.top + METRICS.header_height;
        if content_y < rows_top {
            return None;
        }
        let depth = ((content_y - rows_top) / METRICS.row_height) as u32;
        let t = self.view.get().t_of(f32::from(pos.x) - f32::from(origin.x));
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
            if bar.track.contains(&pos) {
                let grab = grab_offset(f32::from(pos.y), &bar);
                self.drag = Drag::VThumb(grab);
                scroll_thumb_to(self, f32::from(pos.y), grab);
                return;
            }
        }
        if let Some(bar) = self.hbar.get() {
            if bar.track.contains(&pos) {
                let grab = grab_offset(f32::from(pos.x), &bar);
                self.drag = Drag::HThumb(grab);
                pan_thumb_to(self, f32::from(pos.x), grab);
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
            let width = f32::from(view.viewport.get().size.width);
            let dx = f32::from(event.position.x) - f32::from(view.last_mouse.x);
            let dy = f32::from(event.position.y) - f32::from(view.last_mouse.y);
            let mut v = view.view.get();
            v.pan_px(dx);
            v.clamp_to(span, width, EDGE_MARGIN_PX);
            view.view.set(v);
            let scrolled = (view.scroll_y.get() - dy).clamp(0.0, view.max_scroll());
            view.scroll_y.set(scrolled);
            view.last_mouse = event.position;
        }
        Drag::VThumb(grab) => scroll_thumb_to(view, f32::from(event.position.y), grab),
        Drag::HThumb(grab) => pan_thumb_to(view, f32::from(event.position.x), grab),
        Drag::None => {}
    }
    cx.notify();
}

fn end_drag(view: &mut TraceView, _: &MouseUpEvent, cx: &mut Context<TraceView>) {
    view.drag = Drag::None;
    cx.notify();
}

/// The grab offset for a press at `along` (the cursor on the bar's long axis):
/// the distance into the thumb when the press is on it, else half the thumb so a
/// track click centers it on the cursor.
fn grab_offset(along: f32, bar: &BarLayout) -> f32 {
    if along >= bar.thumb_start && along <= bar.thumb_start + bar.thumb_len {
        along - bar.thumb_start
    } else {
        bar.thumb_len / 2.0
    }
}

/// Maps a vertical thumb position back to a scroll offset (inverse of the paint
/// math).
fn scroll_thumb_to(view: &mut TraceView, mouse_along: f32, grab: f32) {
    let Some(bar) = view.vbar.get() else {
        return;
    };
    let travel = (bar.track_len - bar.thumb_len).max(1.0);
    let thumb_start = (mouse_along - grab - bar.track_start).clamp(0.0, travel);
    let max_scroll = view.max_scroll();
    view.scroll_y
        .set((thumb_start / travel * max_scroll).clamp(0.0, max_scroll));
}

/// Maps a horizontal thumb position back to a pan start time (inverse of the
/// paint math).
fn pan_thumb_to(view: &mut TraceView, mouse_along: f32, grab: f32) {
    let Source::Loaded(profile) = &view.source else {
        return;
    };
    let span = profile.span;
    let width = f32::from(view.viewport.get().size.width);
    let Some(bar) = view.hbar.get() else {
        return;
    };
    let mut v = view.view.get();
    let visible_us = width as f64 / v.px_per_us;
    let range = ((span.end_us - span.start_us) as f64 - visible_us).max(0.0);
    let travel = (bar.track_len - bar.thumb_len).max(1.0);
    let thumb_start = (mouse_along - grab - bar.track_start).clamp(0.0, travel);
    v.start_us = span.start_us as f64 + (thumb_start / travel) as f64 * range;
    v.clamp_to(span, width, EDGE_MARGIN_PX);
    view.view.set(v);
}

/// Draws each build-phase boundary as a vertical line across the lanes with its
/// name at the top, skipping any off-screen.
fn paint_markers(
    markers: &[PhaseMarker],
    tv: TimeView,
    bounds: Bounds<Pixels>,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let vw = f32::from(bounds.size.width);
    let top = f32::from(bounds.origin.y) + RULER_HEIGHT;
    let height = f32::from(bounds.size.height) - RULER_HEIGHT;
    let font = bviz_ui_assets::text_metrics(window).font;
    let fs = px(MARKER_LABEL_SIZE);
    // Where the previous label ended, so clustered markers (the build's early
    // phases all fall near t=0) keep their lines without overprinting labels.
    let mut label_right = f32::MIN;
    for marker in markers {
        let x = tv.x_of(marker.start_us);
        if x < 0.0 || x > vw {
            continue;
        }
        let lx = f32::from(bounds.origin.x) + x;
        window.paint_quad(quad(
            Bounds::new(point(px(lx), px(top)), size(px(1.), px(height))),
            px(0.),
            palette.marker,
            px(0.),
            Hsla::from(palette.border),
            BorderStyle::Solid,
        ));
        if lx < label_right {
            continue;
        }
        let line = bviz_ui_assets::shape_line(
            window,
            marker.name.clone(),
            font.clone(),
            fs,
            palette.marker.into(),
        );
        let tx = lx + SLICE_TEXT_PAD;
        let _ = line.paint(
            point(px(tx), px(top + 2.)),
            fs,
            TextAlign::Left,
            None,
            window,
            cx,
        );
        label_right = tx + f32::from(line.width()) + SLICE_TEXT_PAD;
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

/// Drops a faint vertical line from each ruler tick down through the lanes.
fn paint_gridlines(
    ticks: &[(f32, String)],
    bounds: Bounds<Pixels>,
    palette: &Palette,
    window: &mut Window,
) {
    let top = bounds.origin.y + px(RULER_HEIGHT);
    let height = px(f32::from(bounds.size.height) - RULER_HEIGHT);
    for (x, _) in ticks {
        let rect = Bounds::new(point(bounds.origin.x + px(*x), top), size(px(1.), height));
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

/// Paints the ruler band along the top edge with a tick mark and label at each
/// tick.
fn paint_ruler(
    ticks: &[(f32, String)],
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
        let tx = bounds.origin.x + px(*x);
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

/// Paints a vertical and/or horizontal scroll bar for whichever axes overflow,
/// storing each bar's geometry in its cell for hit-testing; an axis that fits
/// gets no bar and a cleared cell. The vertical thumb is sized by the visible
/// fraction of the laid-out height; the horizontal by the visible fraction of
/// the profile span.
fn paint_scrollbars(
    span: TimeSpan,
    tv: TimeView,
    total_height: f32,
    bounds: Bounds<Pixels>,
    scroll_y: f32,
    palette: &Palette,
    vbar: &Rc<Cell<Option<BarLayout>>>,
    hbar: &Rc<Cell<Option<BarLayout>>>,
    window: &mut Window,
) {
    let left = f32::from(bounds.origin.x);
    let top = f32::from(bounds.origin.y);
    let vw = f32::from(bounds.size.width);
    let vh = f32::from(bounds.size.height);
    // The vertical bar spans only the lane area below the ruler.
    let lane_top = top + RULER_HEIGHT;
    let lane_h = vh - RULER_HEIGHT;

    let span_us = (span.end_us - span.start_us) as f64;
    let visible_us = vw as f64 / tv.px_per_us;
    let v_overflow = total_height > lane_h;
    let h_overflow = visible_us < span_us;
    // Each bar shortens at the shared corner when the other is present.
    let v_track_len = lane_h - if h_overflow { BAR_WIDTH } else { 0.0 };
    let h_track_len = vw - if v_overflow { BAR_WIDTH } else { 0.0 };

    let vlayout = v_overflow.then(|| {
        let thumb_len = (v_track_len * lane_h / total_height)
            .max(MIN_THUMB)
            .min(v_track_len);
        let frac = (scroll_y / (total_height - lane_h).max(1.0)).clamp(0.0, 1.0);
        let thumb_start = lane_top + frac * (v_track_len - thumb_len);
        let x = left + vw - BAR_WIDTH;
        BarLayout {
            track: Bounds::new(
                point(px(x), px(lane_top)),
                size(px(BAR_WIDTH), px(v_track_len)),
            ),
            thumb: Bounds::new(
                point(px(x), px(thumb_start)),
                size(px(BAR_WIDTH), px(thumb_len)),
            ),
            track_start: lane_top,
            track_len: v_track_len,
            thumb_start,
            thumb_len,
        }
    });

    let hlayout = h_overflow.then(|| {
        let thumb_len = (h_track_len * (visible_us / span_us) as f32)
            .max(MIN_THUMB)
            .min(h_track_len);
        let frac = (((tv.start_us - span.start_us as f64) / (span_us - visible_us).max(1.0))
            as f32)
            .clamp(0.0, 1.0);
        let thumb_start = left + frac * (h_track_len - thumb_len);
        let y = top + vh - BAR_WIDTH;
        BarLayout {
            track: Bounds::new(point(px(left), px(y)), size(px(h_track_len), px(BAR_WIDTH))),
            thumb: Bounds::new(
                point(px(thumb_start), px(y)),
                size(px(thumb_len), px(BAR_WIDTH)),
            ),
            track_start: left,
            track_len: h_track_len,
            thumb_start,
            thumb_len,
        }
    });

    for bar in [vlayout, hlayout].into_iter().flatten() {
        window.paint_quad(quad(
            bar.track,
            px(0.),
            palette.scroll_track,
            px(0.),
            Hsla::from(palette.border),
            BorderStyle::Solid,
        ));
        window.paint_quad(quad(
            bar.thumb,
            px(BAR_WIDTH / 2.),
            palette.scroll_thumb,
            px(0.),
            Hsla::from(palette.border),
            BorderStyle::Solid,
        ));
    }
    vbar.set(vlayout);
    hbar.set(hlayout);
}

/// Paints a sticky lane header: a band spanning the viewport width with the
/// thread name at its left edge. Culled when fully off-screen.
fn paint_header(
    name: &str,
    lane: &Lane,
    scroll_y: f32,
    bounds: Bounds<Pixels>,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let y = f32::from(bounds.origin.y) + RULER_HEIGHT + lane.top - scroll_y;
    if y + METRICS.header_height < f32::from(bounds.origin.y)
        || y > f32::from(bounds.origin.y) + f32::from(bounds.size.height)
    {
        return;
    }
    let rect = Bounds::new(
        point(bounds.origin.x, px(y)),
        size(bounds.size.width, px(METRICS.header_height)),
    );
    window.paint_quad(quad(
        rect,
        px(0.),
        palette.header_band,
        px(0.),
        Hsla::from(palette.border),
        BorderStyle::Solid,
    ));
    let font = bviz_ui_assets::text_metrics(window).font;
    let fs = px(METRICS.header_height - 2. * SLICE_TEXT_PAD);
    let line = bviz_ui_assets::shape_line(window, name.to_string(), font, fs, palette.label.into());
    let ty = px(y + (METRICS.header_height - f32::from(fs)) / 2.);
    let _ = line.paint(
        point(bounds.origin.x + px(SLICE_TEXT_PAD), ty),
        fs,
        TextAlign::Left,
        None,
        window,
        cx,
    );
}

/// Paints a slice quad and, if wide enough, its truncated name.
fn paint_slice(
    slice: &bviz_trace_profile::Slice,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    bounds: Bounds<Pixels>,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let origin = point(bounds.origin.x + px(x), bounds.origin.y + px(y));
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
    let mut line = bviz_ui_assets::shape_line(window, slice.name.clone(), font.clone(), fs, color);
    if f32::from(line.width()) > avail {
        let chars = slice.name.chars().count();
        let keep =
            ((chars as f32 * avail / f32::from(line.width())).floor() as usize).saturating_sub(1);
        let mut truncated: String = slice.name.chars().take(keep).collect();
        truncated.push('…');
        line = bviz_ui_assets::shape_line(window, truncated, font, fs, color);
    }
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

/// Paints the counter lanes below the threads: a header band with the lane name,
/// then the lane's series as stacked filled areas scaled to the lane's peak
/// stacked total. Off-screen lanes are skipped.
fn paint_counters(
    counters: &[CounterLane],
    positions: &[CounterPos],
    tv: TimeView,
    scroll_y: f32,
    bounds: Bounds<Pixels>,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let origin_y = f32::from(bounds.origin.y);
    let visible_top = origin_y + RULER_HEIGHT;
    let visible_bottom = origin_y + f32::from(bounds.size.height);
    let font = bviz_ui_assets::text_metrics(window).font;
    let fs = px(METRICS.header_height - 2. * SLICE_TEXT_PAD);
    for pos in positions {
        let lane_y = visible_top + pos.top - scroll_y;
        if lane_y + pos.height < visible_top || lane_y > visible_bottom {
            continue;
        }
        let counter = &counters[pos.lane];
        let band = Bounds::new(
            point(bounds.origin.x, px(lane_y)),
            size(bounds.size.width, px(METRICS.header_height)),
        );
        window.paint_quad(quad(
            band,
            px(0.),
            palette.header_band,
            px(0.),
            Hsla::from(palette.border),
            BorderStyle::Solid,
        ));
        let name = bviz_ui_assets::shape_line(
            window,
            counter.name.clone(),
            font.clone(),
            fs,
            palette.label.into(),
        );
        let ty = px(lane_y + (METRICS.header_height - f32::from(fs)) / 2.);
        let _ = name.paint(
            point(bounds.origin.x + px(SLICE_TEXT_PAD), ty),
            fs,
            TextAlign::Left,
            None,
            window,
            cx,
        );

        // Series in a lane stack (per the Chrome counter-event model): the peak
        // is the largest stacked total over time, and each series is drawn as a
        // band between the running baseline and baseline + value.
        let chart_bottom = lane_y + pos.height;
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
        let scale = METRICS.counter_height / peak as f32;
        let x_of = |ts| bounds.origin.x + px(tv.x_of(ts));
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
    }
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
            panel: dark,
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
}
