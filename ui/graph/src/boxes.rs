//! The toolbar's boxes menu: the box classes a loaded graph carries, and the
//! options the control draws for them.
//!
//! What the menu holds is what the graph carries, in the order its rows
//! stand. Every checkmark is derived from the panel's filter each time the
//! options are stated, so nothing here records what is drawn — see
//! [`crate::rules`].

use crate::rules::{self, BoxClass};
use crate::{GraphPanel, derive};
use bviz_graph_layout::Kind;
use bviz_graph_layout::graph::{Graph, id_span};
use bviz_graph_layout::select::Filter;
use bviz_ui_components::{MultiOption, MultiSelectEvent};
use gpui::Context;

/// The box classes of the nodes holding another in `graph`, outermost first.
pub(crate) fn carried_boxes(graph: &Graph) -> Vec<BoxClass> {
    let mut kinds: Vec<Option<Kind>> = vec![None; id_span(graph)];
    for node in &graph.nodes {
        kinds[node.id.0 as usize] = Some(node.kind);
    }
    let mut classes: Vec<BoxClass> = graph
        .nodes
        .iter()
        .filter_map(|node| kinds.get(node.parent?.0 as usize).copied().flatten())
        .filter_map(BoxClass::of)
        .collect();
    classes.sort_unstable();
    classes.dedup();
    classes
}

/// How the boxes menu names `class`.
fn box_label(class: BoxClass) -> &'static str {
    match class {
        BoxClass::Package => "packages",
        BoxClass::Target => "targets",
        BoxClass::LabeledOutput => "labeled outputs",
        BoxClass::ConfiguredTarget => "configured targets",
    }
}

/// The options the boxes menu draws for `classes` under `filter`.
pub(crate) fn box_options(filter: &Filter, classes: &[BoxClass]) -> Vec<MultiOption> {
    let hidden = rules::hidden(filter);
    classes
        .iter()
        .map(|&class| MultiOption::new(box_label(class), !hidden.contains(&class)))
        .collect()
}

/// Takes up the box classes a freshly read graph carries and states the menu
/// over them.
pub(crate) fn adopt(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let Some(loaded) = view.model.read(cx).loaded() else {
        return;
    };
    view.box_classes = carried_boxes(&loaded.shared.base);
    restate(view, cx);
}

/// States the menu's rows afresh from the filter the panel draws through.
pub(crate) fn restate(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let options = box_options(view.derived.filter(), &view.box_classes);
    view.boxes
        .update(cx, |control, cx| control.set_options(options, cx));
}

/// `filter` with the box classes the boxes menu's `event` names drawn, or
/// unwrapped.
///
/// A click over a range writes every row it covers as a single click on that
/// row would, so it leaves the rules the run of single clicks would have left —
/// including the rules the control did not write, which each of those writes
/// carries through untouched.
pub(crate) fn boxes_chosen(
    filter: &Filter,
    classes: &[BoxClass],
    event: &MultiSelectEvent,
) -> Filter {
    let (rows, checked) = match event {
        MultiSelectEvent::Toggled { index, checked } => (std::slice::from_ref(index), *checked),
        MultiSelectEvent::SetAll { rows, checked } => (rows.as_slice(), *checked),
    };
    rows.iter()
        .filter_map(|&index| classes.get(index))
        .fold(filter.clone(), |filter, &class| {
            rules::set_hidden(&filter, class, !checked)
        })
}

/// Draws the graph as the boxes menu's click leaves it.
pub(crate) fn choose_boxes(
    view: &mut GraphPanel,
    event: &MultiSelectEvent,
    cx: &mut Context<GraphPanel>,
) {
    let filter = boxes_chosen(view.derived.filter(), &view.box_classes, event);
    derive::set_filter(view, filter, cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_graph_layout::graph::Node;
    use bviz_graph_layout::intern::Id;
    use bviz_graph_layout::select::{Predicate, Rule, Values, Visibility};

    /// A graph of one package holding a target, a labeled output, a package
    /// group and a source file, the target and the output each holding a
    /// configured target, the target's holding an action writing a file.
    fn graph() -> Graph {
        let mut graph = Graph::new();
        graph.add_node(Node::new(Id(0), "//foo", Kind::Package, None));
        graph.add_node(Node::new(Id(1), ":app", Kind::Target, Some(Id(0))));
        graph.add_node(Node::new(Id(2), ":main.c", Kind::Source, Some(Id(0))));
        graph.add_node(Node::new(
            Id(3),
            ":app",
            Kind::ConfiguredTarget,
            Some(Id(1)),
        ));
        graph.add_node(Node::new(Id(4), "CppLink", Kind::Action, Some(Id(3))));
        graph.add_node(Node::new(Id(5), "app", Kind::Artifact, Some(Id(3))));
        graph.add_node(Node::new(Id(6), ":gen.h", Kind::Artifact, Some(Id(0))));
        graph.add_node(Node::new(
            Id(7),
            ":gen.h",
            Kind::ConfiguredTarget,
            Some(Id(6)),
        ));
        graph.add_node(Node::new(Id(8), ":all", Kind::TargetGroup, Some(Id(0))));
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

    /// The menu offers the box classes the graph carries, outermost first, and
    /// no class of leaf, whatever the graph holds of them.
    #[test]
    fn only_the_box_classes_the_graph_carries_are_offered() {
        let classes = carried_boxes(&graph());

        assert_eq!(
            classes,
            [
                BoxClass::Package,
                BoxClass::Target,
                BoxClass::LabeledOutput,
                BoxClass::ConfiguredTarget
            ]
        );
        assert_eq!(
            labels(&box_options(&Filter::default(), &classes)),
            [
                "packages",
                "targets",
                "labeled outputs",
                "configured targets"
            ]
        );
    }

    /// A kind is a box class only where a node of it holds another, so a
    /// graph whose targets and files hold nothing offers its packages alone.
    #[test]
    fn a_class_whose_nodes_hold_nothing_is_not_offered() {
        let mut graph = Graph::new();
        graph.add_node(Node::new(Id(0), "//foo", Kind::Package, None));
        graph.add_node(Node::new(Id(1), ":app", Kind::Target, Some(Id(0))));
        graph.add_node(Node::new(Id(2), ":gen.h", Kind::Artifact, Some(Id(0))));

        assert_eq!(carried_boxes(&graph), [BoxClass::Package]);
    }

    /// The checkmarks are read out of the rules, so a filter built without the
    /// control renders exactly as that filter says.
    #[test]
    fn a_row_is_checked_by_the_rules_alone() {
        let classes = carried_boxes(&graph());
        let hiding = rules::set_hidden(&Filter::default(), BoxClass::Target, true);

        assert_eq!(
            checked(&box_options(&Filter::default(), &classes)),
            [true; 4]
        );
        assert_eq!(
            checked(&box_options(&hiding, &classes)),
            [true, false, true, true]
        );
    }

    /// Each row's click reads back as its own checkmark and nothing else's, and
    /// checking it again returns to the filter it started from.
    #[test]
    fn each_row_round_trips_through_the_rule_it_writes() {
        let classes = carried_boxes(&graph());
        for (index, class) in classes.iter().enumerate() {
            let toggle = |checked| MultiSelectEvent::Toggled { index, checked };
            let unwrapped = boxes_chosen(&Filter::default(), &classes, &toggle(false));

            let expected: Vec<bool> = (0..classes.len()).map(|row| row != index).collect();
            assert_eq!(
                checked(&box_options(&unwrapped, &classes)),
                expected,
                "{class:?}"
            );
            assert_eq!(
                boxes_chosen(&unwrapped, &classes, &toggle(true)),
                Filter::default(),
                "{class:?}"
            );
        }
    }

    /// A rule the control did not write is none of its business, so a click
    /// over every row carries it through exactly as one row's click does, and
    /// clearing and checking every row again returns to the filter it started
    /// from.
    #[test]
    fn checking_and_clearing_every_row_leaves_a_rule_the_control_did_not_write_standing() {
        let untouched = Rule {
            predicate: Predicate {
                kinds: Values::new([Kind::Action]),
                ..Predicate::default()
            },
            visibility: Visibility::Hidden,
        };
        let filter = Filter {
            rules: vec![untouched.clone()],
            ..Filter::default()
        };
        let classes = carried_boxes(&graph());

        let cleared = boxes_chosen(&filter, &classes, &set_all(0..classes.len(), false));
        assert!(cleared.rules.contains(&untouched));
        assert_eq!(checked(&box_options(&cleared, &classes)), [false; 4]);

        let restored = boxes_chosen(&cleared, &classes, &set_all(0..classes.len(), true));
        assert_eq!(restored, filter);
    }
}
