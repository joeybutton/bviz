//! Loads a Bazel `--profile` trace (`.profile.gz` or `.json`) into
//! `bviz_trace_profile` and opens a window rendering its thread tracks as a
//! flame-chart timeline: time on the x axis, stacked call depth on the y axis,
//! with horizontal zoom/pan and vertical scroll.

use bviz_trace_layout::{Lane, LaneMetrics, TimeView, Viewport, lay_out_lanes, visible_slices};
use bviz_trace_profile::{Profile, TimeSpan};
use bviz_ui_components::{load_in_background, track_drag};
use bviz_ui_shell::Chrome;
use clap::Parser;
use gpui::{
    Animation, AnimationExt, App, BorderStyle, Bounds, Context, Div, Entity, FocusHandle, Hsla,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render,
    Rgba, ScrollDelta, ScrollWheelEvent, Stateful, Task, TextAlign, Transformation, Window, canvas,
    div, hsla, percentage, point, prelude::*, px, quad, size,
};
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

/// Vertical sizing of the stacked thread lanes, in pixels.
const METRICS: LaneMetrics = LaneMetrics {
    header_height: 16.0,
    row_height: 18.0,
    lane_gap: 6.0,
};

/// Slices rendering narrower than this are culled — too thin to see or label.
const MIN_SLICE_PX: f32 = 1.0;

/// Only slices at least this wide carry a text label; narrower ones would have no
/// room for legible text.
const LABEL_MIN_PX: f32 = 28.0;

/// Inset of a slice's label from its left and right edges.
const SLICE_TEXT_PAD: f32 = 3.0;

/// Deepest horizontal zoom, in pixels per microsecond.
const MAX_PX_PER_US: f64 = 50.0;

/// Pixels scrolled per wheel line.
const LINE_SCROLL_PX: f32 = 24.0;

/// Corner radius and border width of a painted slice quad.
const SLICE_RADIUS: f32 = 2.0;
const SLICE_BORDER: f32 = 0.5;

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
    /// The sticky lane-header band fill.
    header_band: Rgba,
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
    dragging: bool,
    last_mouse: Point<Pixels>,
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
            dragging: false,
            last_mouse: Point::default(),
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
                    view.set(TimeView::fit(profile.span, width));
                    fitted.set(true);
                }
                // Hold the bounds across a resize: keep the fit as the zoom floor
                // and the pan within the span for the current width.
                let mut v = view.get();
                v.px_per_us = v.px_per_us.max(fit_px_per_us(profile.span, width));
                v.clamp_to(profile.span, width);
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
                    let vp = Viewport {
                        width: bounds.size.width.into(),
                        height: bounds.size.height.into(),
                        scroll_y: scroll_y.get(),
                    };

                    for vs in visible_slices(&profile, &layout, &tv, &vp, &METRICS, MIN_SLICE_PX) {
                        let slice = &profile.threads[vs.track].slices[vs.slice];
                        paint_slice(slice, vs.x, vs.y, vs.w, vs.h, bounds, &palette, window, cx);
                    }
                    // Headers paint over the slices so the thread name stays
                    // readable, and sit at the left edge so it survives any pan.
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
                    if entity.read(cx).dragging {
                        track_drag(window, &entity, drag_pan, end_drag);
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
        (total - f32::from(self.viewport.get().size.height)).max(0.0)
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
        };
        let body = match &self.source {
            Source::Loaded(profile) => div().flex_1().flex().overflow_hidden().child(
                Self::timeline(
                    profile.clone(),
                    self.view.clone(),
                    self.scroll_y.clone(),
                    self.fitted.clone(),
                    self.viewport.clone(),
                    cx.entity(),
                    palette,
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, e: &MouseDownEvent, _, cx| {
                        // The window-global handlers that pan past the window edge
                        // are installed from the canvas paint while `dragging` is
                        // set, so request a repaint now.
                        this.dragging = true;
                        this.last_mouse = e.position;
                        cx.notify();
                    }),
                )
                .on_scroll_wheel(cx.listener(Self::on_scroll_wheel)),
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
            v.px_per_us = v.px_per_us.clamp(fit_px_per_us(span, width), MAX_PX_PER_US);
            v.clamp_to(span, width);
            self.view.set(v);
        } else {
            let scrolled = (self.scroll_y.get() - dy).clamp(0.0, self.max_scroll());
            self.scroll_y.set(scrolled);
            let mut v = self.view.get();
            v.pan_px(dx);
            v.clamp_to(span, width);
            self.view.set(v);
        }
        cx.notify();
    }
}

/// Pan horizontally and scroll vertically by the cursor delta during a drag,
/// bounded so the profile can't be dragged out of view.
fn drag_pan(view: &mut TraceView, event: &MouseMoveEvent, cx: &mut Context<TraceView>) {
    let Source::Loaded(profile) = &view.source else {
        return;
    };
    let span = profile.span;
    let width = f32::from(view.viewport.get().size.width);
    let dx = f32::from(event.position.x) - f32::from(view.last_mouse.x);
    let dy = f32::from(event.position.y) - f32::from(view.last_mouse.y);
    let mut v = view.view.get();
    v.pan_px(dx);
    v.clamp_to(span, width);
    view.view.set(v);
    let scrolled = (view.scroll_y.get() - dy).clamp(0.0, view.max_scroll());
    view.scroll_y.set(scrolled);
    view.last_mouse = event.position;
    cx.notify();
}

fn end_drag(view: &mut TraceView, _: &MouseUpEvent, cx: &mut Context<TraceView>) {
    view.dragging = false;
    cx.notify();
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
    let y = f32::from(bounds.origin.y) + lane.top - scroll_y;
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

/// A deterministic per-category fill: the category name hashes to a hue, with
/// saturation and lightness fixed for the theme so every category is
/// distinguishable yet legible. Same category → same color across the profile.
fn category_color(cat: &str, palette: &Palette) -> Hsla {
    // FNV-1a over the bytes, folded to a hue in [0, 1).
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in cat.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let hue = (hash % 360) as f32 / 360.0;
    let lightness = if luminance(palette.background) < 0.5 {
        CATEGORY_LIGHTNESS_DARK
    } else {
        CATEGORY_LIGHTNESS_LIGHT
    };
    hsla(hue, CATEGORY_SATURATION, lightness, 1.0)
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

/// The pixels-per-microsecond at which the whole `span` fits `width_px`; the
/// zoom-out floor.
fn fit_px_per_us(span: TimeSpan, width_px: f32) -> f64 {
    let width = (span.end_us - span.start_us).max(1) as f64;
    width_px as f64 / width
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
        assert_eq!(fit_px_per_us(span, 100.0), 0.1);
        // An empty span clamps the divisor to 1 rather than dividing by zero.
        let empty = TimeSpan {
            start_us: 5,
            end_us: 5,
        };
        assert_eq!(fit_px_per_us(empty, 100.0), 100.0);
    }
}
