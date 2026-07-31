use crate::{Controls, GraphView, Palette};
use bviz_ui_components::{Toggle, Toolbar, spinner};
use gpui::{ClickEvent, Context, IntoElement, div, prelude::*, px};

pub(crate) fn toolbar(
    controls: Controls,
    palette: &Palette,
    cx: &mut Context<GraphView>,
) -> Toolbar {
    Toolbar::new()
        .child(Toggle::new(
            "dim-toggle",
            "dim",
            controls.dim_enabled,
            cx.listener(|view, _: &ClickEvent, _, cx| view.toggle_dim(cx)),
        ))
        .child(Toggle::new(
            "reduce-toggle",
            "reduce",
            controls.reduced,
            cx.listener(|view, _: &ClickEvent, _, cx| view.toggle_reduce(cx)),
        ))
        .when(controls.relaying, |bar| {
            bar.trailing(relaying_indicator(palette))
        })
}

/// Shown while a relayout runs, so the prior layout stays interactive with a
/// visible cue that a new one is in flight.
fn relaying_indicator(palette: &Palette) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .text_sm()
        .text_color(palette.label)
        .child(spinner("relaying-spinner", px(14.), palette.label))
        .child("laying out…")
}
