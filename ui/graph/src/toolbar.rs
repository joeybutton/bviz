use crate::{Controls, GraphPanel, Palette, ToggleNeighborhood};
use bviz_ui_components::{OneRow, Switch, ToggleGroup, ToggleSegment, Toolbar, spinner};
use gpui::{Context, Div, FocusHandle, IntoElement, Pixels, div, prelude::*, px};

/// How many strips [`toolbar`] stacks.
const STRIPS: usize = 2;

/// The height of [`toolbar`], whatever its strips hold.
pub(crate) fn height() -> Pixels {
    OneRow::height() * STRIPS as f32
}

/// The panel's controls in two strips, each one row tall, so that neither
/// a selection coming and going nor chips being added moves the drawing:
/// how the graph is drawn, ending in the toggles putting panels up and away,
/// then what it holds.
pub(crate) fn toolbar(
    controls: Controls,
    palette: &Palette,
    focus: &FocusHandle,
    cx: &Context<GraphPanel>,
) -> Div {
    let marks = marks(&controls, focus, cx);
    let mut view = Toolbar::new()
        .one_row("graph-view-controls")
        .child(group("layout", palette).child(controls.layout_picker))
        .child(controls.reduce_toggle)
        .child(group("colour", palette).child(controls.colour_picker))
        .child(group("mark", palette).child(marks));
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
    let mut view = view.when(controls.relaying, |view| {
        // The prior layout stays interactive while a relayout runs.
        view.trailing(busy("relaying-spinner", "laying out…", palette))
    });
    for panel in [controls.report_control, controls.timecard_control]
        .into_iter()
        .flatten()
    {
        view = view.trailing(panel);
    }
    let view = view.when_some(controls.legend, |view, legend| {
        view.trailing(legend.toggle())
    });
    let query = Toolbar::new()
        .one_row("graph-query-bar")
        .when_some(controls.boxes, OneRow::child)
        .child(controls.chips);
    let strips: [OneRow; STRIPS] = [view, query];
    div().size_full().flex().flex_col().children(strips)
}

/// The toggles choosing what the drawing marks.
fn marks(controls: &Controls, focus: &FocusHandle, cx: &Context<GraphPanel>) -> ToggleGroup {
    let panel = cx.entity();
    let neighborhood = Switch::new(
        ToggleSegment::new("neighbors").keys_for(&ToggleNeighborhood, focus),
        controls.neighborhood_enabled,
        {
            let panel = panel.clone();
            move |_, _, cx| panel.update(cx, |view, cx| view.toggle_neighborhood(cx))
        },
    );
    let critical_path = controls.critical_path.map(|enabled| {
        Switch::new(
            ToggleSegment::new("critical path").tooltip("Toggle critical path"),
            enabled,
            move |_, _, cx| panel.update(cx, |view, cx| view.toggle_critical_path(cx)),
        )
    });
    ToggleGroup::any_of(
        "graph-marks",
        "mark",
        std::iter::once(neighborhood).chain(critical_path),
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
