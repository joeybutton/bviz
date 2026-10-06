use crate::{Controls, GraphPanel, Palette, ToggleNeighborhood, ToggleReduce};
use bviz_ui_components::{Button, Toolbar, spinner};
use gpui::{ClickEvent, Context, Div, FocusHandle, IntoElement, Pixels, div, prelude::*, px};

/// The panel's controls in three strips, each one row tall, so that neither
/// a selection coming and going nor chips being added moves the drawing:
/// how the graph is drawn, then the checklists over its facets, then the
/// query bar.
pub(crate) fn toolbar(
    controls: Controls,
    palette: &Palette,
    focus: &FocusHandle,
    cx: &mut Context<GraphPanel>,
) -> Div {
    let mut view = Toolbar::new()
        .one_row("graph-view-controls")
        .child(controls.picker)
        .child(controls.colour_picker)
        .child(
            Button::new(
                "neighborhood-toggle",
                cx.listener(|view, _: &ClickEvent, _, cx| view.toggle_neighborhood(cx)),
            )
            .label("neighbors")
            .selected(controls.neighborhood_enabled)
            .keys_for(&ToggleNeighborhood, focus),
        );
    // The strip only ever offers a control the mode's layout can answer, so
    // everything on it acts on what is drawn.
    if let Some(reduced) = controls.reduced.filter(|_| controls.mode.honors_reduce()) {
        view = view.child(
            Button::new(
                "reduce-toggle",
                cx.listener(|view, _: &ClickEvent, _, cx| view.toggle_reduce(cx)),
            )
            .label("reduce")
            .selected(reduced)
            .keys_for(&ToggleReduce, focus),
        );
    }
    if let Some(enabled) = controls.critical_path {
        view = view.child(
            Button::new(
                "critical-path-toggle",
                cx.listener(|view, _: &ClickEvent, _, cx| view.toggle_critical_path(cx)),
            )
            .label("critical path")
            .selected(enabled),
        );
    }
    for card in [controls.report_control, controls.timecard_control]
        .into_iter()
        .flatten()
    {
        view = view.child(card);
    }
    // Last, past the controls that are always up, so the strip's standing shape
    // holds still as a selection comes and goes.
    if let Some(picker) = controls.relation_picker {
        view = view.child(picker);
    }
    if let Some(pin) = controls.pin_control {
        view = view.child(pin);
    }
    if let Some(why) = controls.why_controls {
        view = view.child(why);
    }
    let view = view.when(controls.relaying, |view| {
        // The prior layout stays interactive while a relayout runs.
        view.trailing(busy("relaying-spinner", "laying out…", palette))
    });
    let mut facets = Toolbar::new().one_row("graph-facet-controls");
    for checklist in [controls.class_filter, controls.origin_filter]
        .into_iter()
        .flatten()
    {
        facets = facets.child(checklist);
    }
    let facets = facets.child(controls.preset_filter);
    div()
        .flex()
        .flex_col()
        .flex_none()
        .child(view)
        .child(facets)
        .child(
            Toolbar::new()
                .one_row("graph-query-bar")
                .child(controls.chips),
        )
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
