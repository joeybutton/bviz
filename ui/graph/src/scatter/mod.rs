//! A scatter plot of a graph's targets by how much of the build each pulls in
//! against how much of it depends on the target.

mod paint;
mod plot;

use crate::drag::CLICK_SLOP;
use crate::model::{GraphModel, Loaded};
use crate::{Palette, model::SourceReplaced};
use bviz_graph_layout::Kind;
use bviz_graph_layout::metrics::TargetMetrics;
use bviz_graph_layout::select::labels::carried_label;
use bviz_ui_components::{cached_layer, centered_spinner};
use bviz_ui_workspace::{Panel, PanelEvent, PanelKind, ViewKind};
use gpui::{
    App, Bounds, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, Rgba,
    SharedString, Subscription, Window, canvas, div, prelude::*, px,
};
use plot::{MIN_RADIUS, Picked, Plotted};
use std::sync::Arc;

/// The key context scoping the scatter plot's own bindings, so a key pressed
/// here reaches this panel and no other.
const KEY_CONTEXT: &str = "GraphScatter";

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
    /// Where the plot was last laid out, which a pointer is resolved against.
    bounds: Option<Bounds<Pixels>>,
    /// Where the button went down over the plot, until it comes up again.
    pressed_at: Option<Point<Pixels>>,
    /// Redraws as the graph is read and measured.
    _changed: Subscription,
    _source_replaced: Subscription,
}

impl ScatterPanel {
    pub(crate) fn new(model: Entity<GraphModel>, cx: &mut Context<Self>) -> Self {
        let changed = cx.observe(&model, |view, model, cx| {
            if view.plot.is_none()
                && let Some(metrics) = model.read(cx).metrics()
            {
                let plotted = Arc::new(Plotted::of(metrics));
                let canvas = cx.new(|_| ScatterCanvas {
                    plotted: Arc::clone(&plotted),
                });
                view.plot = Some(Plot {
                    plotted,
                    canvas,
                    hover: None,
                });
            }
            cx.notify();
        });
        let source_replaced = cx.subscribe(&model, |view, _, _: &SourceReplaced, cx| {
            if view.shown {
                view.measure(cx);
            }
        });
        Self {
            focus: cx.focus_handle(),
            model,
            shown: false,
            plot: None,
            bounds: None,
            pressed_at: None,
            _changed: changed,
            _source_replaced: source_replaced,
        }
    }

    fn measure(&self, cx: &mut Context<Self>) {
        self.model.update(cx, |model, cx| model.ensure_metrics(cx));
    }

    /// The mark a pointer at `at` picks, where the plot is drawn.
    fn pick(&self, at: Point<Pixels>) -> Option<Picked> {
        let plotted = &self.plot.as_ref()?.plotted;
        paint::data_area(plotted, self.bounds?)?.pick(&plotted.marks, at)
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

    /// Shows the graph's drawing of the target under the pointer, where the
    /// button comes up where it went down.
    fn release_at(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(from) = self.pressed_at.take() else {
            return;
        };
        if f32::from(at.x - from.x).hypot(f32::from(at.y - from.y)) > CLICK_SLOP {
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

/// The measured targets as drawn, and the mark among them under the pointer.
struct Plot {
    plotted: Arc<Plotted>,
    /// The layer drawing `plotted`.
    canvas: Entity<ScatterCanvas>,
    hover: Option<Hover>,
}

/// A mark under the pointer, by its index among the plotted marks, and what its
/// tooltip reads.
struct Hover {
    mark: usize,
    lines: Vec<String>,
}

/// What the tooltip over `target` reads, where `coincident` other targets sit
/// at its place in the plot.
fn describe(target: &TargetMetrics, coincident: usize, loaded: &Loaded) -> Vec<String> {
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
        let status = model
            .source()
            .status("No graph", |_| match model.metrics() {
                None => MEASURING.into(),
                Some(metrics) => format!("{} targets", metrics.len()),
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
            Some(plot) => root.child(legend(&plot.plotted, &palette)).child(
                div()
                    .id("scatter-plot")
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .child(cached_layer(plot.canvas.clone()))
                    .when(plot.hover.is_some() && model.reveals(), |plot| {
                        plot.cursor_pointer()
                    })
                    .child(hover_layer(cx.entity(), palette))
                    .on_mouse_move(cx.listener(|view, e: &MouseMoveEvent, _, cx| {
                        view.hover_at(e.position, cx);
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, e: &MouseDownEvent, _, _| {
                            view.pressed_at = Some(e.position);
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|view, e: &MouseUpEvent, _, cx| {
                            view.release_at(e.position, cx);
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
/// records where the plot lies for the pointer handlers.
fn hover_layer(view: Entity<ScatterPanel>, palette: Palette) -> impl IntoElement {
    let prepaint = {
        let view = view.clone();
        move |bounds: Bounds<Pixels>, _: &mut Window, cx: &mut App| {
            view.update(cx, |view, _| view.bounds = Some(bounds));
        }
    };
    let paint = move |bounds: Bounds<Pixels>, (), window: &mut Window, cx: &mut App| {
        let hovered = view.read(cx).plot.as_ref().and_then(|plot| {
            let hover = plot.hover.as_ref()?;
            Some((Arc::clone(&plot.plotted), hover.mark, hover.lines.clone()))
        });
        let Some((plotted, mark, lines)) = hovered else {
            return;
        };
        let mark = &plotted.marks[mark];
        paint::paint_hover(mark, &lines, &plotted, &palette, bounds, window, cx);
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

/// The strip naming the colour each kind of target is drawn in, and what a
/// mark's size stands for.
fn legend(plotted: &Plotted, palette: &Palette) -> Div {
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
                .text_color(palette.colors.text_muted)
                .child("mark size: direct dependents"),
        )
}

/// The layer the targets are drawn in, cached across frames that leave the
/// plot and zoom unchanged.
struct ScatterCanvas {
    plotted: Arc<Plotted>,
}

impl Render for ScatterCanvas {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::from_theme(&bviz_ui_theme::theme(window, cx));
        let plotted = Arc::clone(&self.plotted);
        canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, (), window, cx| {
                paint::paint_plot(&plotted, &palette, bounds, window, cx)
            },
        )
        .size_full()
    }
}
