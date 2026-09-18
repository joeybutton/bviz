use crate::{Controls, GraphPanel, Palette};
use bviz_ui_components::{Button, Toolbar, spinner};
use gpui::{ClickEvent, Context, IntoElement, div, prelude::*, px};

pub(crate) fn toolbar(
    controls: Controls,
    palette: &Palette,
    cx: &mut Context<GraphPanel>,
) -> Toolbar {
    // The search leads the strip: on a graph too large to read, naming what to
    // look at is where a reader starts.
    let mut bar = Toolbar::new().child(controls.search).child(controls.picker);
    // The facet checklists sit beside the layout picker: both say what the panel
    // draws, where the toggles past them say how it is drawn.
    for checklist in [controls.class_filter, controls.origin_filter]
        .into_iter()
        .flatten()
    {
        bar = bar.child(checklist);
    }
    bar = bar.child(controls.preset_filter).child(
        Button::new(
            "neighborhood-toggle",
            cx.listener(|view, _: &ClickEvent, _, cx| view.toggle_neighborhood(cx)),
        )
        .label("neighbors")
        .selected(controls.neighborhood_enabled),
    );
    // The strip only ever offers a control the mode's layout can answer, so
    // everything on it acts on what is drawn.
    if controls.mode.honors_reduce() {
        bar = bar.child(
            Button::new(
                "reduce-toggle",
                cx.listener(|view, _: &ClickEvent, _, cx| view.toggle_reduce(cx)),
            )
            .label("reduce")
            .selected(controls.reduced),
        );
    }
    // Last, past the controls that are always up, so the strip's standing shape
    // holds still as a selection comes and goes.
    if let Some(picker) = controls.relation_picker {
        bar = bar.child(picker);
    }
    bar.when(controls.relaying, |bar| {
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
