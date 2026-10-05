//! Moving the view onto one node another view names.

use crate::GraphPanel;
use crate::derive;
use crate::framing::Frame;
use crate::mode::LayoutMode;
use bviz_graph_layout::Id;
use bviz_ui_workspace::PanelEvent;
use gpui::Context;

/// Selects `id` and moves the view onto it, opening what holds it in a layered
/// drawing, and asks for the panel to be shown.
pub(crate) fn reveal(view: &mut GraphPanel, id: Id, cx: &mut Context<GraphPanel>) {
    view.selection.select(id);
    if view.mode == LayoutMode::Layered {
        derive::expand_to(view, id, cx);
    }
    view.framing.request_frame(Frame::Node(id));
    cx.emit(PanelEvent::Activate);
    view.invalidate_canvas(cx);
}

/// Selects and frames the box standing for `id` in the drawing as it is: in
/// a layered drawing the innermost drawn box holding it, which opens none and
/// so lays nothing out afresh.
pub(crate) fn show_in_place(view: &mut GraphPanel, id: Id, cx: &mut Context<GraphPanel>) {
    let shown = match view.mode {
        LayoutMode::Layered => derive::drawn_holder(view, id),
        LayoutMode::Map => Some(id),
    };
    let Some(shown) = shown else {
        return;
    };
    view.selection.select(shown);
    view.framing.request_frame(Frame::Node(shown));
    view.invalidate_canvas(cx);
}
