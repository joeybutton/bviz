//! A scatter plot of a graph's targets by how much of the build each pulls in
//! against how much of it depends on the target.

mod paint;
mod plot;

use crate::Palette;
use crate::drag::CLICK_SLOP;
use crate::model::{FilterChanged, GraphModel, Loaded, MetricsLanded, SourceReplaced};
use crate::{PIXELS_PER_NOTCH, ZOOM_PER_NOTCH};
use bviz_graph_layout::Kind;
use bviz_graph_layout::metrics::Metrics;
use bviz_graph_layout::select::Filter;
use bviz_graph_layout::select::labels::carried_label;
use bviz_ui_components::{DragEnd, cached_layer, centered_spinner, load_in_background, track_drag};
use bviz_ui_workspace::{Panel, PanelEvent, PanelKind, ViewKind};
use gpui::{
    App, Bounds, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, Rgba,
    ScrollDelta, ScrollWheelEvent, SharedString, Subscription, Task, WeakEntity, Window, canvas,
    div, prelude::*, px,
};
use plot::{MIN_RADIUS, Picked, Plotted, Zoom};
use std::sync::Arc;

/// The key context scoping the scatter plot's own bindings, so a key pressed
/// here reaches this panel and no other.
pub(crate) const KEY_CONTEXT: &str = "GraphScatter";

gpui::actions!(bviz_graph, [ShowWholePlot]);

/// What the panel reads while the graph's targets are being measured.
const MEASURING: &str = "Computing target metrics…";

/// The targets of a graph, placed by their closure and blast radius.
pub struct ScatterPanel {
    focus: FocusHandle,
    /// Measures the targets the first time this panel is shown.
    model: Entity<GraphModel>,
    /// Whether the workspace is showing the panel, which a graph landing while
    /// it is shown is measured for at once.
    shown: bool,
    /// The measured targets as drawn, from the time they land.
    plot: Option<Plot>,
    /// The part of each axis the plot shows, which outlasts a replot: a filter
    /// moves no mark.
    zoom: Zoom,
    /// The layer drawing the marks; notified through
    /// [`Self::invalidate_canvas`].
    canvas: Entity<ScatterCanvas>,
    /// Where the plot was last laid out, which a pointer is resolved against.
    bounds: Option<Bounds<Pixels>>,
    /// The press over the plot, until the button comes up again.
    press: Option<Press>,
    /// Plots the targets through the graph's filter, absent while nothing is
    /// being plotted, so that one plotting runs at a time.
    plotting: Option<Task<()>>,
    /// Redraws as the graph is read and measured.
    _changed: Subscription,
    _source_replaced: Subscription,
    _metrics_landed: Subscription,
    _filter_changed: Subscription,
}

impl ScatterPanel {
    pub(crate) fn new(model: Entity<GraphModel>, cx: &mut Context<Self>) -> Self {
        let changed = cx.observe(&model, |_, _, cx| cx.notify());
        let source_replaced = cx.subscribe(&model, |view, _, _: &SourceReplaced, cx| {
            view.plot = None;
            view.plotting = None;
            if view.shown {
                view.measure(cx);
            }
        });
        let metrics_landed = cx.subscribe(&model, |view, _, _: &MetricsLanded, cx| {
            view.replot(cx);
        });
        let filter_changed = cx.subscribe(&model, |view, _, _: &FilterChanged, cx| {
            view.replot(cx);
        });
        let parent = cx.entity().downgrade();
        Self {
            focus: cx.focus_handle(),
            model,
            shown: false,
            plot: None,
            zoom: Zoom::WHOLE,
            canvas: cx.new(|_| ScatterCanvas { parent }),
            bounds: None,
            press: None,
            plotting: None,
            _changed: changed,
            _source_replaced: source_replaced,
            _metrics_landed: metrics_landed,
            _filter_changed: filter_changed,
        }
    }

    /// Has the graph's targets measured, and brings the plot up to date where
    /// they already are.
    fn measure(&mut self, cx: &mut Context<Self>) {
        self.model.update(cx, |model, cx| model.ensure_metrics(cx));
        self.replot(cx);
    }

    /// Plots the measured targets through the filter the graph's other views
    /// draw through, where the plot drawn is not drawn through it. A hidden
    /// panel waits until it is shown, and a plotting running replots once it
    /// lands. The filter is evaluated over every node of the graph, so this
    /// runs off the UI thread, and the plot drawn stays until the new one lands.
    fn replot(&mut self, cx: &mut Context<Self>) {
        if !self.shown || self.plotting.is_some() {
            return;
        }
        let model = self.model.read(cx);
        let (Some(metrics), Some(loaded)) = (model.metrics(), model.loaded()) else {
            return;
        };
        let filter = model.filter().cloned().unwrap_or_default();
        if self.plot.as_ref().is_some_and(|plot| plot.filter == filter) {
            return;
        }
        let (metrics, shared) = (Arc::clone(metrics), loaded.shared.clone());
        self.plotting = Some(load_in_background(
            cx,
            move || {
                let verdicts = filter.evaluate(&shared.base, &shared.names, &shared.facets);
                (Plotted::of(&metrics.points, |id| verdicts.get(id)), filter)
            },
            |view, (plotted, filter), cx| {
                view.plotting = None;
                view.plot = Some(Plot {
                    plotted: Arc::new(plotted),
                    hover: None,
                    filter,
                });
                view.invalidate_canvas(cx);
                view.replot(cx);
            },
        ));
    }

    /// Repaints the marks along with the layer over them; every change to what
    /// the marks show routes through here.
    fn invalidate_canvas(&self, cx: &mut Context<Self>) {
        self.canvas.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    /// Where the plot places counts, where it is drawn.
    fn area(&self) -> Option<plot::DataArea> {
        paint::data_area(&self.plot.as_ref()?.plotted, self.zoom, self.bounds?)
    }

    /// The mark a pointer at `at` picks, where the plot is drawn.
    fn pick(&self, at: Point<Pixels>) -> Option<Picked> {
        let marks = &self.plot.as_ref()?.plotted.marks;
        self.area()?.pick(marks, at)
    }

    /// Shows the plot at `zoom`, and names what the pointer at `at` is over
    /// there unless a press is in progress, whose release names it.
    fn zoom_to(&mut self, zoom: Zoom, at: Point<Pixels>, cx: &mut Context<Self>) {
        if zoom == self.zoom {
            return;
        }
        self.zoom = zoom;
        self.invalidate_canvas(cx);
        if self.press.is_none() {
            self.hover_at(at, cx);
        }
    }

    /// Zooms about the pointer by as many notches as the wheel turned.
    fn on_scroll_wheel(&mut self, e: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let notches = match e.delta {
            ScrollDelta::Lines(lines) => lines.y,
            ScrollDelta::Pixels(pixels) => f32::from(pixels.y) / PIXELS_PER_NOTCH,
        };
        if let Some(area) = self.area() {
            let zoom = area.zoomed(e.position, ZOOM_PER_NOTCH.powf(notches));
            self.zoom_to(zoom, e.position, cx);
        }
    }

    /// Starts a press, or shows the whole of each axis again on a double
    /// click clear of every mark: a first click on a mark shows it in the
    /// graph, which the second click then lands on.
    fn on_mouse_down(&mut self, e: &MouseDownEvent, cx: &mut Context<Self>) {
        if e.click_count == DOUBLE_CLICK && self.pick(e.position).is_none() {
            self.press = None;
            self.zoom_to(Zoom::WHOLE, e.position, cx);
            return;
        }
        self.press = Some(Press {
            from: e.position,
            last: e.position,
            dragged: false,
        });
        // The frame this asks for installs the handlers carrying the drag.
        cx.notify();
    }

    /// Follows the pointer to the mark it picks, repainting only where that
    /// changes which mark the tooltip names.
    fn hover_at(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let picked = self.pick(at);
        let model = self.model.read(cx);
        let Some(plot) = &mut self.plot else {
            return;
        };
        if plot.hover.as_ref().map(|hover| hover.mark) == picked.map(|picked| picked.mark) {
            return;
        }
        plot.hover = picked.and_then(|picked| {
            let target = &plot.plotted.marks[picked.mark].target;
            Some(Hover {
                mark: picked.mark,
                lines: describe(target, picked.coincident, model.loaded()?),
            })
        });
        cx.notify();
    }

    /// Carries the zoom along with a press dragged past a click's reach, which
    /// puts away the tooltip until the release.
    fn drag_to(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(press) = &mut self.press else {
            return;
        };
        let travelled = at - press.from;
        let starts =
            !press.dragged && f32::from(travelled.x).hypot(f32::from(travelled.y)) > CLICK_SLOP;
        press.dragged |= starts;
        if !press.dragged {
            return;
        }
        let by = at - press.last;
        press.last = at;
        if starts {
            self.clear_hover(cx);
        }
        if let Some(area) = self.area() {
            self.zoom_to(area.panned(by), at, cx);
        }
    }

    /// Ends the press, which shows the graph's drawing of the target under the
    /// pointer where it was a click. Taking the press makes a second call for
    /// the same release a no-op.
    fn end_press(&mut self, at: Point<Pixels>, end: DragEnd, cx: &mut Context<Self>) {
        let Some(press) = self.press.take() else {
            return;
        };
        cx.notify();
        self.hover_at(at, cx);
        if press.dragged || !matches!(end, DragEnd::Released) {
            return;
        }
        let Some((plot, picked)) = self.plot.as_ref().zip(self.pick(at)) else {
            return;
        };
        let id = plot.plotted.marks[picked.mark].target.id;
        self.model.update(cx, |model, cx| model.reveal(id, cx));
    }

    /// Drops the tooltip, which names the mark under a pointer that has left.
    fn clear_hover(&mut self, cx: &mut Context<Self>) {
        if let Some(plot) = &mut self.plot
            && plot.hover.take().is_some()
        {
            cx.notify();
        }
    }
}

/// The `click_count` of the press completing a double click.
const DOUBLE_CLICK: usize = 2;

/// The measured targets as drawn, and the mark among them under the pointer.
struct Plot {
    plotted: Arc<Plotted>,
    hover: Option<Hover>,
    /// What `plotted` was drawn through.
    filter: Filter,
}

/// A press over the plot, which is a click until it is dragged past
/// [`CLICK_SLOP`] and pans the plot from then on.
struct Press {
    from: Point<Pixels>,
    /// Where the pointer was at the last step of the pan.
    last: Point<Pixels>,
    dragged: bool,
}

/// A mark under the pointer, by its index among the plotted marks, and what its
/// tooltip reads.
struct Hover {
    mark: usize,
    lines: Vec<String>,
}

/// How many targets the plot draws, of how many were measured.
fn drawn_status(plotted: &Plotted) -> String {
    match (plotted.marks.len(), plotted.measured) {
        (drawn, measured) if drawn == measured => format!("{measured} targets"),
        (drawn, measured) => format!("{drawn} of {measured} targets"),
    }
}

/// What the tooltip over `target` reads, where `coincident` other targets sit
/// at its place in the plot.
fn describe(target: &Metrics, coincident: usize, loaded: &Loaded) -> Vec<String> {
    let shared = &loaded.shared;
    let label = match carried_label(&shared.names, &shared.facets, target.id) {
        Some(label) => shared.labels.written(label).into_owned(),
        None => shared.names.resolve(target.id).to_owned(),
    };
    let repo = match shared.facets.repo(target.id) {
        Some(repo) => match shared.labels.repos().written(repo) {
            "" => MAIN_REPO,
            written => written,
        },
        None => UNKNOWN_REPO,
    };
    let mut lines = Vec::with_capacity(TOOLTIP_LINES);
    lines.extend([
        label,
        format!(
            "closure {} · blast radius {}",
            target.closure, target.blast_radius
        ),
        format!("fan-in {} · fan-out {}", target.fan_in, target.fan_out),
        format!("{} in {repo}", kind_names(target.kind).one),
    ]);
    if coincident > 0 {
        lines.push(format!("+{coincident} more here"));
    }
    lines
}

/// The most lines a tooltip reads.
const TOOLTIP_LINES: usize = 5;

const MAIN_REPO: &str = "the main repository";
const UNKNOWN_REPO: &str = "an unnamed repository";

impl EventEmitter<PanelEvent> for ScatterPanel {}

impl Focusable for ScatterPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Panel for ScatterPanel {
    fn kind() -> PanelKind {
        PanelKind {
            view: ViewKind::new("graph-scatter"),
            label: "Scatter",
            key_context: KEY_CONTEXT,
        }
    }

    fn title(&self, _cx: &App) -> SharedString {
        Self::kind().label.into()
    }

    fn status(&self, cx: &App) -> Option<SharedString> {
        let model = self.model.read(cx);
        let status = model.source().status("No graph", |_| match &self.plot {
            None => MEASURING.into(),
            Some(plot) => drawn_status(&plot.plotted),
        });
        Some(status.into())
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.shown = active;
        match active {
            true => {
                self.focus.focus(window, cx);
                self.measure(cx);
            }
            false => self.clear_hover(cx),
        }
    }
}

impl Render for ScatterPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::from_theme(&bviz_ui_theme::theme(window, cx));
        let root = div()
            .key_context(Self::kind().key_context)
            .on_action(cx.listener(|view, _: &ShowWholePlot, window, cx| {
                view.zoom_to(Zoom::WHOLE, window.mouse_position(), cx);
            }))
            .track_focus(&self.focus)
            .flex_1()
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden();
        let model = self.model.read(cx);
        if let Some(placeholder) = model.source().placeholder(&palette.colors) {
            return root.child(placeholder);
        }
        match &self.plot {
            None => root.child(centered_spinner(MEASURING, &palette.colors)),
            Some(plot) => root
                .child(legend(&plot.plotted, self.zoom, &palette))
                .child(
                    div()
                        .id("scatter-plot")
                        .relative()
                        .flex_1()
                        .min_h(px(0.))
                        .child(cached_layer(self.canvas.clone()))
                        .when(plot.hover.is_some() && model.reveals(), |plot| {
                            plot.cursor_pointer()
                        })
                        .child(hover_layer(cx.entity(), palette))
                        .on_mouse_move(cx.listener(|view, e: &MouseMoveEvent, _, cx| {
                            if view.press.is_none() {
                                view.hover_at(e.position, cx);
                            }
                        }))
                        .on_scroll_wheel(cx.listener(|view, e: &ScrollWheelEvent, _, cx| {
                            view.on_scroll_wheel(e, cx);
                        }))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|view, e: &MouseDownEvent, _, cx| {
                                view.on_mouse_down(e, cx)
                            }),
                        )
                        // The window-global handlers are installed only by a frame
                        // painted with the press in progress, which a quick click
                        // finishes before.
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|view, e: &MouseUpEvent, _, cx| {
                                view.end_press(e.position, DragEnd::Released, cx);
                            }),
                        )
                        .on_hover(cx.listener(|view, hovered: &bool, _, cx| {
                            if !hovered {
                                view.clear_hover(cx);
                            }
                        })),
                ),
        }
    }
}

/// The layer over the marks that rings the hovered one and names it, painted
/// apart from them so that a hover repaints none of the marks. Its layout
/// records where the plot lies for the pointer handlers, and its paint carries
/// a press past the plot's edge.
fn hover_layer(view: Entity<ScatterPanel>, palette: Palette) -> impl IntoElement {
    let prepaint = {
        let view = view.clone();
        move |bounds: Bounds<Pixels>, _: &mut Window, cx: &mut App| {
            view.update(cx, |view, _| view.bounds = Some(bounds));
        }
    };
    let paint = move |bounds: Bounds<Pixels>, (), window: &mut Window, cx: &mut App| {
        let panel = view.read(cx);
        let (zoom, pressed) = (panel.zoom, panel.press.is_some());
        let hovered = panel.plot.as_ref().and_then(|plot| {
            let hover = plot.hover.as_ref()?;
            Some((Arc::clone(&plot.plotted), hover.mark, hover.lines.clone()))
        });
        if pressed {
            track_drag(
                window,
                &view,
                |view, at, _, cx| view.drag_to(at, cx),
                |view, at, end, _, cx| view.end_press(at, end, cx),
            );
        }
        let Some((plotted, mark, lines)) = hovered else {
            return;
        };
        let Some(area) = paint::data_area(&plotted, zoom, bounds) else {
            return;
        };
        let mark = &plotted.marks[mark];
        paint::paint_hover(mark, &lines, &area, &palette, bounds, window, cx);
    };
    canvas(prepaint, paint).absolute().inset_0().size_full()
}

/// How a reader names one target of a kind, and several.
struct KindNames {
    one: &'static str,
    many: &'static str,
}

fn kind_names(kind: Kind) -> KindNames {
    let (one, many) = match kind {
        Kind::Source => ("source file", "source files"),
        Kind::Artifact => ("generated file", "generated files"),
        Kind::TargetGroup => (
            "package or environment group",
            "package and environment groups",
        ),
        _ => ("rule target", "rule targets"),
    };
    KindNames { one, many }
}

/// Opacity of a legend swatch, firmer than a mark's so that a lone swatch still
/// shows the hue a cluster of marks builds up to.
const SWATCH_ALPHA: f32 = 0.7;

/// Diameter of a legend swatch: twice the smallest mark's, so its hue reads.
const SWATCH_DIAMETER: f32 = 4.0 * MIN_RADIUS;

/// What the legend reads while the plot shows part of an axis.
const ZOOMED: &str = "double-click empty space or press 0 to show all";

/// The strip naming the colour each kind of target is drawn in, what a mark's
/// size stands for, and how to leave a zoom.
fn legend(plotted: &Plotted, zoom: Zoom, palette: &Palette) -> Div {
    // Kinds a reader names alike and the graph colours alike read as one.
    let mut entries: Vec<(&str, Rgba)> = Vec::with_capacity(plotted.kinds.len());
    for &kind in &plotted.kinds {
        let entry = (kind_names(kind).many, palette.kind_accent(kind));
        if !entries.contains(&entry) {
            entries.push(entry);
        }
    }
    let entry = |(name, color): (&'static str, Rgba)| {
        let color = Rgba {
            a: SWATCH_ALPHA,
            ..color
        };
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(div().size(px(SWATCH_DIAMETER)).rounded_full().bg(color))
            .child(name)
    };
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_4()
        .px_3()
        .py_1()
        .text_sm()
        .text_color(palette.label)
        .border_b_1()
        .border_color(palette.border)
        .children(entries.into_iter().map(entry))
        .child(
            div()
                .ml_auto()
                .flex()
                .gap_4()
                .text_color(palette.colors.text_muted)
                .when(zoom != Zoom::WHOLE, |hints| hints.child(ZOOMED))
                .child("mark size: direct dependents"),
        )
}

/// The layer the targets are drawn in, cached across frames that leave the
/// plot and zoom unchanged.
struct ScatterCanvas {
    /// The panel whose plot and zoom this draws.
    parent: WeakEntity<ScatterPanel>,
}

impl Render for ScatterCanvas {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::from_theme(&bviz_ui_theme::theme(window, cx));
        let drawn = self.parent.upgrade().and_then(|parent| {
            let panel = parent.read(cx);
            Some((Arc::clone(&panel.plot.as_ref()?.plotted), panel.zoom))
        });
        canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, (), window, cx| {
                if let Some((plotted, zoom)) = drawn {
                    paint::paint_plot(&plotted, zoom, &palette, bounds, window, cx);
                }
            },
        )
        .size_full()
    }
}
