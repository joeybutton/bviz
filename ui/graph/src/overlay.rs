use crate::model;
use crate::paint::{edge_color, kind_accent};
use crate::{GraphView, Palette, ViewFlags};
use bviz_graph_layout::{EdgeKind, Kind, LaidOut};
use bviz_ui_components::{PillColors, spinner, toggle_pill};
use gpui::{AnyElement, Div, Entity, IntoElement, div, prelude::*, px, rems};

/// The overlay's elements, in the order they paint over the canvas.
pub(crate) fn controls(
    g: &LaidOut,
    palette: &Palette,
    flags: &ViewFlags,
    entity: Entity<GraphView>,
) -> Vec<AnyElement> {
    let pill = PillColors {
        accent: kind_accent(Kind::Artifact),
        off_label: palette.label,
        off_border: palette.border,
        fill: palette.container_fill,
    };
    let dim_toggle = toggle_pill(
        "dim-toggle",
        rems(0.5),
        "dim",
        flags.dim_enabled,
        pill,
        entity.clone(),
        |this, _| this.dim_enabled = !this.dim_enabled,
    );
    let reduce_toggle = toggle_pill(
        "reduce-toggle",
        px(40.),
        "reduce",
        flags.reduced,
        pill,
        entity,
        |this, cx| {
            this.reduced = !this.reduced;
            model::relayout(this, cx);
        },
    );
    let mut elements = Vec::with_capacity(4);
    elements.extend(legend(g, palette).map(IntoElement::into_any_element));
    elements.extend(relaying_indicator(flags.relaying, palette).map(IntoElement::into_any_element));
    elements.push(dim_toggle.into_any_element());
    elements.push(reduce_toggle.into_any_element());
    elements
}

/// Overlay listing the edge provenances actually drawn, in enum order, so it
/// explains only the kinds present in this graph. `None` when none are drawn.
fn legend(g: &LaidOut, palette: &Palette) -> Option<Div> {
    let present: Vec<EdgeKind> = [EdgeKind::Query, EdgeKind::Cquery, EdgeKind::Aquery]
        .into_iter()
        .filter(|k| g.connectors.iter().any(|c| c.kinds.contains(k)))
        .collect();
    (!present.is_empty()).then(|| {
        div()
            .absolute()
            .left_2()
            .bottom_2()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_md()
            .bg(palette.container_fill)
            .children(present.into_iter().map(|k| {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(16.)).h(px(2.)).bg(edge_color(k)))
                    .child(div().text_xs().text_color(palette.label).child(k.label()))
            }))
    })
}

/// Spinner shown while a relayout runs, so the prior layout stays interactive
/// with a visible cue that a new one is in flight. `None` when not relaying.
fn relaying_indicator(relaying: bool, palette: &Palette) -> Option<Div> {
    relaying.then(|| {
        div()
            .absolute()
            .right_2()
            .top_2()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(palette.container_fill)
            .child(spinner("relaying-spinner", px(14.), palette.label))
            .child(div().text_xs().text_color(palette.label).child("laying out…"))
    })
}
