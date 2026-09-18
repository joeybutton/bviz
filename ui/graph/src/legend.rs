use crate::Palette;
use bviz_graph_layout::{EdgeKind, LaidOut};
use gpui::{Div, div, prelude::*, px};

/// Width of the line shown beside a kind's name.
const SWATCH_WIDTH: f32 = 16.;

/// Dash and gap of the line shown for a bridge. A swatch is too short to
/// reproduce the lengths a route is dashed at, so it shows the pattern at a size
/// that reads as dashed over the width it has.
const SWATCH_DASH: f32 = 4.;
const SWATCH_GAP: f32 = 2.;

/// The line drawn beside a kind's name, as a route of that kind is drawn: solid
/// for a dependency the graph states, dashed for a bridge standing in for a path
/// through boxes the drawing does not hold.
fn swatch(kind: EdgeKind, palette: &Palette) -> Div {
    let color = palette.edge_color(kind, false);
    let line = |width: f32| div().w(px(width)).h(px(2.)).bg(color);
    match kind {
        EdgeKind::Transitive => {
            let dashes = ((SWATCH_WIDTH + SWATCH_GAP) / (SWATCH_DASH + SWATCH_GAP)) as usize;
            div()
                .flex()
                .items_center()
                .gap(px(SWATCH_GAP))
                .children((0..dashes).map(|_| line(SWATCH_DASH)))
        }
        _ => line(SWATCH_WIDTH),
    }
}

/// Lists the edge kinds actually drawn, in enum order, so it explains only the
/// kinds present in this graph. `None` when none are drawn.
pub(crate) fn legend(g: &LaidOut, palette: &Palette) -> Option<Div> {
    let present: Vec<EdgeKind> = [
        EdgeKind::Query,
        EdgeKind::Cquery,
        EdgeKind::Aquery,
        EdgeKind::Transitive,
    ]
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
            .bg(palette.container)
            .children(present.into_iter().map(|k| {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(swatch(k, palette))
                    .child(div().text_xs().text_color(palette.label).child(k.label()))
            }))
    })
}
