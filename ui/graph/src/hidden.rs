//! What keeps a node out of a view of the graph, and the card any view stands
//! to offer it shown another way.

use crate::Palette;
use crate::model::GraphModel;
use bviz_graph_layout::Id;
use bviz_graph_layout::query_lang::{Exclusion, Stack};
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

/// What a card says keeps `node` out of a view, hidden `by` that: for a node
/// outside the query, the chip leaving it out once `model` has named it.
pub(crate) fn reason(by: HiddenBy, node: Id, model: &GraphModel) -> SharedString {
    let named = (by == HiddenBy::Query)
        .then(|| model.exclusion(node))
        .flatten();
    let Some((exclusion, stack)) = named else {
        return by.describe().into();
    };
    excluded(exclusion, stack).map_or_else(|| by.describe().into(), SharedString::from)
}

/// What a card says of a node `exclusion` leaves out of what `stack` names.
fn excluded(exclusion: Exclusion, stack: &Stack) -> Option<String> {
    match exclusion {
        Exclusion::Chip(chip) => Some(format!("left out by chip ‹{}›", stack.chip(chip)?.text())),
        Exclusion::Sources => {
            let mut enabled = stack.sources().iter().filter(|chip| chip.enabled());
            Some(match (enabled.next(), enabled.next()) {
                (Some(only), None) => format!("not named by chip ‹{}›", only.text()),
                _ => "named by no source chip".to_owned(),
            })
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

    /// A source leaving a node out is named only where it is the one
    /// enabled; a refinement is named as the chip taking the node out.
    #[test]
    fn the_card_names_the_chip_leaving_a_node_out() {
        let mut stack = Stack::default();
        let first = stack.push("//a/...");
        let second = stack.push("//b/...");
        let refinement = stack.push("- //a:x");
        let excluded = |exclusion, stack: &Stack| super::excluded(exclusion, stack).unwrap();
        assert_eq!(
            excluded(Exclusion::Chip(refinement), &stack),
            "left out by chip ‹- //a:x›"
        );
        assert_eq!(
            excluded(Exclusion::Sources, &stack),
            "named by no source chip"
        );
        stack.toggle_enabled(second);
        assert_eq!(
            excluded(Exclusion::Sources, &stack),
            "not named by chip ‹//a/...›"
        );
        assert!(stack.chip(first).is_some());
    }

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
