//! The menu a right press puts up over a cell of the icicle.

use super::icicle::Holds;
use bviz_graph_layout::metrics::Unit;

/// One row of a cell's menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Offer {
    /// Focus the cell, which holds one point.
    Focus,
    /// Focus the cell, which gathers several points.
    Open,
    /// Focus what the cell on top stands under.
    Up,
    /// Show the point `node` in the graph, which only a graph panel `can`.
    Show { node: u32, can: bool },
    /// Root the tree at the target of the point `node`, as `caveat` explains
    /// where the target stands for other than the point alone.
    Root { node: u32, caveat: Option<Caveat> },
    /// Copy the label of the point `node`.
    CopyLabel { node: u32 },
}

/// How rooting the tree at a point's target reaches other than the point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Caveat {
    /// The target is configured more than once, and a build of it requests
    /// one copy.
    Copies,
    /// The target's only copy is built for a tool, which is not the copy a
    /// build of the target requests.
    Tool,
    /// A build of the target starts from its last actions.
    Actions,
}

/// What a cell's menu reads of the point the cell holds.
#[derive(Clone, Copy, Debug)]
pub(super) struct PointFacts {
    /// Whether the point carries the label of a target, which rooting the
    /// tree at its target and copying its label take.
    pub(super) labelled: bool,
    /// How many times the point's target is configured, where the point is
    /// a configured target.
    pub(super) copies: u32,
    /// Whether the point is built for a tool.
    pub(super) for_tool: bool,
}

/// What the menu of the cell holding `holds` offers, `top` holding the cell
/// on top, the tree being of `unit`, a graph panel able to show a point
/// where `reveals`, and `facts` telling what is known of the point `holds`
/// names, where it names one.
pub(super) fn offered(
    holds: Holds,
    top: Holds,
    unit: Unit,
    reveals: bool,
    facts: impl FnOnce(u32) -> PointFacts,
) -> Vec<Offer> {
    let focus = match holds {
        Holds::Build => None,
        _ if holds == top => Some(Offer::Up),
        Holds::Node(_) => Some(Offer::Focus),
        Holds::Shared | Holds::Rest(_) => Some(Offer::Open),
    };
    let Holds::Node(node) = holds else {
        return focus.into_iter().collect();
    };
    let facts = facts(node);
    let caveat = match unit {
        Unit::ConfiguredTarget if facts.copies > 1 => Some(Caveat::Copies),
        Unit::ConfiguredTarget if facts.for_tool => Some(Caveat::Tool),
        Unit::Target | Unit::ConfiguredTarget => None,
        Unit::Action => Some(Caveat::Actions),
    };
    let show = Offer::Show { node, can: reveals };
    let labelled = [Offer::Root { node, caveat }, Offer::CopyLabel { node }];
    let labelled = labelled.into_iter().filter(|_| facts.labelled);
    focus.into_iter().chain([show]).chain(labelled).collect()
}

impl Offer {
    pub(super) fn label(&self) -> &'static str {
        match self {
            Offer::Focus => "Focus",
            Offer::Open => "Open",
            Offer::Up => "Go Up",
            Offer::Show { .. } => "Show in Graph",
            Offer::Root { .. } => "Use as Root",
            Offer::CopyLabel { .. } => "Copy Label",
        }
    }

    /// The key the offer also answers to outside the menu.
    pub(super) fn keys(&self) -> Option<&'static str> {
        match self {
            Offer::Focus | Offer::Open => Some("Enter"),
            Offer::Up => Some("Backspace"),
            Offer::Show { .. } => Some("g"),
            Offer::Root { .. } | Offer::CopyLabel { .. } => None,
        }
    }
}

impl Caveat {
    /// What the row rooting the tree at `label` says of it.
    pub(super) fn explain(self, label: &str) -> String {
        match self {
            Caveat::Copies => format!(
                "Roots the tree at the copy of {label} a build of it requests, \
                 which may be another than this one"
            ),
            Caveat::Tool => format!(
                "Roots the tree at the copy of {label} a build of it requests, \
                 which this graph does not hold: its only copy here is built for a tool"
            ),
            Caveat::Actions => {
                format!("Roots the tree at the actions a build of {label} starts from")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exclusive::icicle::lay_out;
    use crate::exclusive::icicle::tests::{Laid, ROW, area, leaf};
    use gpui::px;

    /// A point carrying a target's label, its target configured `copies`
    /// times and built for a tool where `for_tool`.
    fn labelled(copies: u32, for_tool: bool) -> impl FnOnce(u32) -> PointFacts {
        move |_| PointFacts {
            labelled: true,
            copies,
            for_tool,
        }
    }

    #[test]
    fn a_point_offers_to_be_focused_shown_rooted_at_and_copied() {
        let offers = offered(
            Holds::Node(3),
            Holds::Build,
            Unit::Target,
            true,
            labelled(1, false),
        );
        let root = Offer::Root {
            node: 3,
            caveat: None,
        };
        let show = Offer::Show { node: 3, can: true };
        let copy = Offer::CopyLabel { node: 3 };
        assert_eq!(offers, [Offer::Focus, show, root, copy]);
    }

    #[test]
    fn showing_a_point_is_offered_disabled_without_a_graph_panel() {
        let offers = offered(
            Holds::Node(3),
            Holds::Build,
            Unit::Target,
            false,
            labelled(1, false),
        );
        let show = Offer::Show {
            node: 3,
            can: false,
        };
        assert!(offers.contains(&show));
    }

    #[test]
    fn the_cell_on_top_offers_to_go_up_and_the_whole_build_nothing() {
        let offers = offered(
            Holds::Node(3),
            Holds::Node(3),
            Unit::Target,
            true,
            labelled(1, false),
        );
        assert_eq!(offers[0], Offer::Up);
        let build = offered(Holds::Build, Holds::Build, Unit::Target, true, |_| {
            unreachable!("the whole build holds no point")
        });
        assert!(build.is_empty());
    }

    /// A cell gathering several points offers only to be opened, there being
    /// no one point to show, root at or name.
    #[test]
    fn a_cell_gathering_points_offers_only_to_open() {
        let tree = Laid::of(&[leaf(90), leaf(6), leaf(4)]);
        let cells = lay_out(&tree, Holds::Build, area(100., 2.), ROW, px(8.));
        let rest = cells
            .iter()
            .find(|cell| matches!(cell.holds, Holds::Rest(_)));
        let rest = rest.unwrap().holds;
        let unit = Unit::ConfiguredTarget;
        let no_point = |_| unreachable!("a cell gathering points holds no one point");
        assert_eq!(
            offered(rest, Holds::Build, unit, true, no_point),
            [Offer::Open]
        );
        assert_eq!(
            offered(Holds::Shared, Holds::Build, unit, true, no_point),
            [Offer::Open]
        );
    }

    #[test]
    fn rooting_at_a_copy_or_an_action_says_what_its_target_roots_at() {
        let root = |unit, copies, for_tool| {
            let facts = labelled(copies, for_tool);
            let offers = offered(Holds::Node(1), Holds::Build, unit, true, facts);
            offers.into_iter().find_map(|offer| match offer {
                Offer::Root { caveat, .. } => Some(caveat),
                _ => None,
            })
        };
        let configured = Unit::ConfiguredTarget;
        assert_eq!(root(configured, 1, false), Some(None));
        assert_eq!(root(configured, 2, false), Some(Some(Caveat::Copies)));
        assert_eq!(root(configured, 1, true), Some(Some(Caveat::Tool)));
        assert_eq!(root(Unit::Action, 1, false), Some(Some(Caveat::Actions)));
    }

    /// A point carrying no target's label offers nothing that takes one.
    #[test]
    fn a_point_without_a_label_offers_neither_to_root_at_it_nor_to_copy_it() {
        let unlabelled = |_| PointFacts {
            labelled: false,
            copies: 1,
            for_tool: false,
        };
        let offers = offered(Holds::Node(3), Holds::Build, Unit::Action, true, unlabelled);
        let show = Offer::Show { node: 3, can: true };
        assert_eq!(offers, [Offer::Focus, show]);
    }
}
