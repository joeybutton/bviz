//! The toolbar's class checklist: the node classes a loaded graph carries, and
//! the options the control draws for them.
//!
//! What the checklist holds is what the graph carries, in the order its rows
//! stand. Every checkmark is derived from the panel's filter each time the
//! options are stated, so nothing here records what is drawn — see
//! [`crate::rules`].

use crate::{GraphPanel, derive, rules};
use bviz_graph_layout::Kind;
use bviz_graph_layout::graph::Graph;
use bviz_graph_layout::select::Filter;
use bviz_ui_components::{MultiOption, MultiSelectEvent};
use gpui::Context;

/// The node classes `graph` holds boxes for, in the order the kinds are
/// declared, which runs from the outermost box a drawing nests to the leaves
/// inside it.
pub(crate) fn carried_classes(graph: &Graph) -> Vec<Kind> {
    let mut classes: Vec<Kind> = graph.nodes.iter().map(|node| node.kind).collect();
    classes.sort_unstable();
    classes.dedup();
    classes
}

/// How a reader names the class `kind`.
///
/// A container's row names the box the drawing puts round what it holds, since
/// hiding it unwraps that box: its contents lift to the nearest box still drawn
/// rather than leaving the graph.
fn class_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Package => "package boxes",
        Kind::Target => "target boxes",
        Kind::ConfiguredTarget => "configured-target boxes",
        Kind::Action => "actions",
        Kind::Artifact => "generated files",
        Kind::Source => "source files",
        Kind::TargetGroup => "package and environment groups",
        Kind::MergeGroup => "merged file groups",
    }
}

/// The options a class checklist draws for `classes` under `filter`.
pub(crate) fn class_options(filter: &Filter, classes: &[Kind]) -> Vec<MultiOption> {
    let hidden = rules::hidden(filter);
    classes
        .iter()
        .map(|&kind| MultiOption::new(class_label(kind), !hidden.contains(&kind)))
        .collect()
}

/// Takes up the classes a freshly read graph carries and states the checklist
/// over them.
pub(crate) fn adopt(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let Some(loaded) = view.model.read(cx).loaded() else {
        return;
    };
    view.classes = carried_classes(&loaded.shared.base);
    restate(view, cx);
}

/// States the checklist's rows afresh from the filter the panel draws through.
pub(crate) fn restate(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let classes = class_options(view.derived.filter(), &view.classes);
    view.class_filter
        .update(cx, |control, cx| control.set_options(classes, cx));
}

/// Draws the graph through `filter` from here on and restates the checklist
/// over it, so a rule and the checkmark reading it move together.
pub(crate) fn set_filter(view: &mut GraphPanel, filter: Filter, cx: &mut Context<GraphPanel>) {
    derive::set_filter(view, filter, cx);
    restate(view, cx);
}

/// As [`set_filter`], for a filter set by a gesture that named what to look at:
/// the camera moves onto what the filter leaves drawn.
pub(crate) fn set_filter_framed(
    view: &mut GraphPanel,
    filter: Filter,
    cx: &mut Context<GraphPanel>,
) {
    derive::set_filter_framed(view, filter, cx);
    restate(view, cx);
}

/// `filter` with the node classes the class checklist's `event` names drawn, or
/// left out.
///
/// A click over a range writes every row it covers as a single click on that
/// row would, so it leaves the rules the run of single clicks would have left —
/// including the rules the control did not write, which each of those writes
/// carries through untouched.
pub(crate) fn classes_chosen(
    filter: &Filter,
    classes: &[Kind],
    event: &MultiSelectEvent,
) -> Filter {
    let (rows, checked) = match event {
        MultiSelectEvent::Toggled { index, checked } => (std::slice::from_ref(index), *checked),
        MultiSelectEvent::SetAll { rows, checked } => (rows.as_slice(), *checked),
    };
    rows.iter()
        .filter_map(|&index| classes.get(index))
        .fold(filter.clone(), |filter, &kind| {
            rules::set_hidden(&filter, kind, !checked)
        })
}

/// Draws the graph as the class checklist's click leaves it.
pub(crate) fn choose_classes(
    view: &mut GraphPanel,
    event: &MultiSelectEvent,
    cx: &mut Context<GraphPanel>,
) {
    let filter = classes_chosen(view.derived.filter(), &view.classes, event);
    set_filter(view, filter, cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_graph_layout::graph::Node;
    use bviz_graph_layout::intern::Id;
    use bviz_graph_layout::select::{Predicate, Rule, Values, Visibility};

    /// A graph of one package holding one target, with a source file beside it.
    fn graph() -> Graph {
        let mut graph = Graph::new();
        graph.add_node(Node::new(Id(0), "//foo", Kind::Package, None));
        graph.add_node(Node::new(Id(1), ":app", Kind::Target, Some(Id(0))));
        graph.add_node(Node::new(Id(2), ":main.c", Kind::Source, Some(Id(0))));
        graph
    }

    fn labels(options: &[MultiOption]) -> Vec<&str> {
        options.iter().map(|option| option.label.as_ref()).collect()
    }

    fn checked(options: &[MultiOption]) -> Vec<bool> {
        options.iter().map(|option| option.checked).collect()
    }

    fn set_all(rows: std::ops::Range<usize>, checked: bool) -> MultiSelectEvent {
        MultiSelectEvent::SetAll {
            rows: rows.collect(),
            checked,
        }
    }

    /// A class no node carries is nothing to offer, so the checklist lists the
    /// classes the graph holds boxes for and no others.
    #[test]
    fn only_the_classes_the_graph_carries_are_offered() {
        assert_eq!(
            carried_classes(&graph()),
            [Kind::Package, Kind::Target, Kind::Source]
        );
    }

    /// The checkmarks are read out of the rules, so a filter built without the
    /// control renders exactly as that filter says.
    #[test]
    fn a_class_row_is_checked_by_the_rules_alone() {
        let classes = carried_classes(&graph());
        let hiding = rules::set_hidden(&Filter::default(), Kind::Target, true);

        assert_eq!(
            checked(&class_options(&Filter::default(), &classes)),
            [true; 3]
        );
        assert_eq!(
            checked(&class_options(&hiding, &classes)),
            [true, false, true]
        );
    }

    /// A container's row names the box it draws, since hiding it lifts what it
    /// holds to the nearest box still drawn.
    #[test]
    fn a_container_class_is_named_by_the_box_it_draws() {
        let classes = carried_classes(&graph());
        assert_eq!(
            labels(&class_options(&Filter::default(), &classes)),
            ["package boxes", "target boxes", "source files"]
        );
    }

    /// A rule the control did not write is none of its business, so a click
    /// over every row carries it through exactly as one row's click does, and
    /// clearing and checking every row again returns to the filter it started
    /// from.
    #[test]
    fn checking_and_clearing_every_row_leaves_a_rule_the_control_did_not_write_standing() {
        let untouched = Rule {
            predicate: Predicate {
                tags: Values::new(["flaky".to_string()]),
                ..Predicate::default()
            },
            visibility: Visibility::Hidden,
        };
        let filter = Filter {
            rules: vec![untouched.clone()],
            ..Filter::default()
        };
        let classes = carried_classes(&graph());

        let cleared = classes_chosen(&filter, &classes, &set_all(0..classes.len(), false));
        assert!(cleared.rules.contains(&untouched));
        assert_eq!(checked(&class_options(&cleared, &classes)), [false; 3]);

        let restored = classes_chosen(&cleared, &classes, &set_all(0..classes.len(), true));
        assert_eq!(restored, filter);
    }
}
