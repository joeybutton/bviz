//! A scatter plot of a graph's targets by how much of the build each pulls in
//! against how much of it depends on the target.

mod paint;
mod plot;

use crate::model::GraphModel;
use crate::{Palette, model::SourceReplaced};
use bviz_graph_layout::Kind;
use bviz_ui_components::{cached_layer, centered_spinner};
use bviz_ui_workspace::{Panel, PanelEvent, PanelKind, ViewKind};
use gpui::{
    App, Bounds, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Pixels,
    Render, Rgba, SharedString, Subscription, Window, canvas, div, prelude::*, px,
};
use plot::{MIN_RADIUS, Plotted};
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
    /// The layer drawing the measured targets, from the time they land.
    canvas: Option<Entity<ScatterCanvas>>,
    /// Redraws as the graph is read and measured.
    _changed: Subscription,
    _source_replaced: Subscription,
}

impl ScatterPanel {
    pub(crate) fn new(model: Entity<GraphModel>, cx: &mut Context<Self>) -> Self {
        let changed = cx.observe(&model, |view, model, cx| {
            if view.canvas.is_none()
                && let Some(metrics) = model.read(cx).metrics()
            {
                let plotted = Arc::new(Plotted::of(metrics));
                view.canvas = Some(cx.new(|_| ScatterCanvas { plotted }));
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
            canvas: None,
            _changed: changed,
            _source_replaced: source_replaced,
        }
    }

    fn measure(&self, cx: &mut Context<Self>) {
        self.model.update(cx, |model, cx| model.ensure_metrics(cx));
    }
}

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
        if active {
            self.focus.focus(window, cx);
            self.measure(cx);
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
        match &self.canvas {
            None => root.child(centered_spinner(MEASURING, &palette.colors)),
            Some(canvas) => root
                .child(legend(&canvas.read(cx).plotted, &palette))
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h(px(0.))
                        .child(cached_layer(canvas.clone())),
                ),
        }
    }
}

/// How the legend names the targets of a kind.
fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Source => "source files",
        Kind::Artifact => "generated files",
        Kind::TargetGroup => "package and environment groups",
        _ => "rule targets",
    }
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
        let entry = (kind_name(kind), palette.kind_accent(kind));
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
