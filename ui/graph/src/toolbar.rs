use crate::{Controls, GraphPanel, Palette};
use bviz_ui_components::{Button, Toolbar, spinner};
use gpui::{ClickEvent, Context, Div, IntoElement, Pixels, div, prelude::*, px};

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
    if let Some(reduced) = controls.reduced.filter(|_| controls.mode.honors_reduce()) {
        bar = bar.child(
            Button::new(
                "reduce-toggle",
                cx.listener(|view, _: &ClickEvent, _, cx| view.toggle_reduce(cx)),
            )
            .label("reduce")
            .selected(reduced),
        );
    }
    bar = bar.child(controls.colour_picker);
    for card in [controls.report_control, controls.timecard_control]
        .into_iter()
        .flatten()
    {
        bar = bar.child(card);
    }
    // Last, past the controls that are always up, so the strip's standing shape
    // holds still as a selection comes and goes.
    if let Some(picker) = controls.relation_picker {
        bar = bar.child(picker);
    }
    if let Some(pin) = controls.pin_control {
        bar = bar.child(pin);
    }
    if let Some(why) = controls.why_controls {
        bar = bar.child(why);
    }
    bar.when(controls.relaying, |bar| {
        // The prior layout stays interactive while a relayout runs.
        bar.trailing(busy("relaying-spinner", "laying out…", palette))
    })
}

/// A strip's controls grouped under the word `name`.
pub(crate) fn group(name: &'static str, palette: &Palette) -> Div {
    labelled(palette).child(name)
}

/// The cue a strip shows that the work `doing` names is in flight, its spinner
/// keyed by `id`.
pub(crate) fn busy(id: &'static str, doing: &'static str, palette: &Palette) -> impl IntoElement {
    labelled(palette)
        .child(spinner(id, SPINNER_SIZE, palette.label))
        .child(doing)
}

const SPINNER_SIZE: Pixels = px(14.);

/// A row of a strip set in the strip's label text.
fn labelled(palette: &Palette) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .text_sm()
        .text_color(palette.label)
}
