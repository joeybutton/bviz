//! What keeps a node out of a view of the graph, and the card any view stands
//! to offer it shown another way.

use crate::Palette;
use bviz_graph_layout::select::Hiding;
use bviz_ui_components::{Button, LabelLine, TruncatingLabel};
use gpui::{Div, SharedString, div, prelude::*};

/// What keeps a node out of a view, as a reader names it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HiddenBy {
    /// The dependency paths a graph panel draws in its filter's place.
    Paths,
    /// The reader's own pin.
    Pin,
    /// The query the filter is scoped to, which does not name the node.
    Query,
    /// A graph panel's boxes menu, which unwraps the node.
    Boxes,
    /// The node frames others and none of them is drawn.
    NothingToFrame,
}

impl HiddenBy {
    /// What a filter hiding a node by `hiding` names; a rule of the filter is
    /// a graph panel's boxes menu, the one control that adds one.
    pub(crate) fn of(hiding: Hiding) -> Self {
        match hiding {
            Hiding::Pin => HiddenBy::Pin,
            Hiding::Scope => HiddenBy::Query,
            Hiding::Rule(_) => HiddenBy::Boxes,
            Hiding::NothingToFrame => HiddenBy::NothingToFrame,
        }
    }

    /// Whether a reveal can bring the node into the view.
    pub(crate) fn revealable(self) -> bool {
        self != HiddenBy::NothingToFrame
    }

    pub(crate) fn describe(self) -> &'static str {
        match self {
            HiddenBy::Paths => "hidden by the dependency paths drawn",
            HiddenBy::Pin => "hidden by its pin",
            HiddenBy::Query => "outside the query",
            HiddenBy::Boxes => "unwrapped by the boxes menu",
            HiddenBy::NothingToFrame => "hidden: nothing it holds is drawn",
        }
    }
}

/// The card offering to show the node `name` names, written `written`, which
/// `reason` says the view leaves out, by the `offers` it stands.
pub(crate) fn card(
    name: impl Into<SharedString>,
    written: impl Into<SharedString>,
    reason: impl Into<SharedString>,
    offers: impl IntoIterator<Item = Button>,
    palette: &Palette,
) -> Div {
    div()
        .w_full()
        .occlude()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded_md()
        .bg(palette.container)
        .text_xs()
        .text_color(palette.label)
        .child(LabelLine::new(name, palette.colors.text_muted).hinted("hidden-name", written))
        .child(TruncatingLabel::new("hidden-reason", reason))
        .child(div().flex().justify_end().gap_1().children(offers))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A filter's rules are the boxes menu's, so the reader is told to turn
    /// that back.
    #[test]
    fn a_rule_is_named_as_the_boxes_menu() {
        assert_eq!(HiddenBy::of(Hiding::Rule(0)), HiddenBy::Boxes);
    }

    /// A pin outranks the scope, so revealing a node the query leaves out
    /// shows it.
    #[test]
    fn a_node_outside_the_query_is_hidden_by_it_and_revealable() {
        let by = HiddenBy::of(Hiding::Scope);
        assert_eq!(by, HiddenBy::Query);
        assert!(by.revealable());
    }

    #[test]
    fn only_a_frame_with_nothing_drawn_inside_cannot_be_revealed() {
        assert!(!HiddenBy::of(Hiding::NothingToFrame).revealable());
        for revealable in [
            HiddenBy::Paths,
            HiddenBy::of(Hiding::Pin),
            HiddenBy::of(Hiding::Scope),
        ] {
            assert!(revealable.revealable());
        }
    }
}
