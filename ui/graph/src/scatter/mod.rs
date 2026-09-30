//! A scatter plot of a graph's targets by how much of the build each pulls in
//! against how much of it depends on the target.

use crate::model::GraphModel;
use crate::{Palette, model::SourceReplaced};
use bviz_ui_components::centered_spinner;
use bviz_ui_workspace::{Panel, PanelEvent, PanelKind, ViewKind};
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render, SharedString,
    Subscription, Window, div, prelude::*,
};

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
    /// Redraws as the graph is read and measured.
    _changed: Subscription,
    _source_replaced: Subscription,
}

impl ScatterPanel {
    pub(crate) fn new(model: Entity<GraphModel>, cx: &mut Context<Self>) -> Self {
        let changed = cx.observe(&model, |_, _, cx| cx.notify());
        let source_replaced = cx.subscribe(&model, |view, _, _: &SourceReplaced, cx| {
            if view.shown {
                view.measure(cx);
            }
        });
        Self {
            focus: cx.focus_handle(),
            model,
            shown: false,
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
        match model.metrics() {
            None => root.child(centered_spinner(MEASURING, &palette.colors)),
            Some(_) => root,
        }
    }
}
