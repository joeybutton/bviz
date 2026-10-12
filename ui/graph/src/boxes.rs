//! The toolbar's boxes menu: the box classes a loaded graph carries, and the
//! options the control draws for them.
//!
//! What the menu holds is what the graph carries, in the order its rows
//! stand. Every checkmark is derived from the classes the panel unwraps each time the
//! options are stated, so nothing here records what is drawn — see
//! [`crate::rules`].

use crate::rules::BoxClass;
use crate::{GraphPanel, derive};
use bviz_graph_layout::Kind;
use bviz_graph_layout::graph::{Graph, id_span};
use bviz_ui_components::{MultiOption, MultiSelectEvent};
use gpui::Context;
use std::collections::BTreeSet;

/// What the boxes menu is called, on its trigger and as a row of the toolbar's
/// overflow menu.
pub(crate) const NAME: &str = "boxes";

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
        BoxClass::LabeledOutput => "labelled outputs",
        BoxClass::ConfiguredTarget => "configured targets",
    }
}

/// The options the boxes menu draws for `classes`, with those `unwrapped`
/// unchecked.
pub(crate) fn box_options(
    unwrapped: &BTreeSet<BoxClass>,
    classes: &[BoxClass],
) -> Vec<MultiOption> {
    classes
        .iter()
        .map(|&class| MultiOption::new(box_label(class), !unwrapped.contains(&class)))
        .collect()
}

/// What a map level leaving out most of what it would draw says, where
/// every box class of `classes` is `unwrapped`. Showing the outermost class
/// gives the coarsest levels their boxes back.
pub(crate) fn every_class_hidden(
    unwrapped: &BTreeSet<BoxClass>,
    classes: &[BoxClass],
) -> Option<String> {
    let outermost = *classes.first()?;
    let all = classes.iter().all(|class| unwrapped.contains(class));
    all.then(|| {
        format!(
            "Every box type is hidden, so most of this zoom is left out. Show {} in the \
             boxes menu, or zoom in.",
            box_label(outermost)
        )
    })
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

/// States the menu's rows afresh from the classes the panel unwraps.
pub(crate) fn restate(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let options = box_options(view.derived.unwrapped(), &view.box_classes);
    view.boxes
        .update(cx, |control, cx| control.set_options(options, cx));
}

/// `unwrapped` with the box classes the boxes menu's `event` names drawn, or
/// unwrapped.
pub(crate) fn boxes_chosen(
    unwrapped: &BTreeSet<BoxClass>,
    classes: &[BoxClass],
    event: &MultiSelectEvent,
) -> BTreeSet<BoxClass> {
    let (rows, checked) = match event {
        MultiSelectEvent::Toggled { index, checked } => (std::slice::from_ref(index), *checked),
        MultiSelectEvent::SetAll { rows, checked } => (rows.as_slice(), *checked),
    };
    let mut unwrapped = unwrapped.clone();
    for &class in rows.iter().filter_map(|&index| classes.get(index)) {
        match checked {
            true => unwrapped.remove(&class),
            false => unwrapped.insert(class),
        };
    }
    unwrapped
}

/// Draws the graph as the boxes menu's click leaves it.
pub(crate) fn choose_boxes(
    view: &mut GraphPanel,
    event: &MultiSelectEvent,
    cx: &mut Context<GraphPanel>,
) {
    let unwrapped = boxes_chosen(view.derived.unwrapped(), &view.box_classes, event);
    derive::set_unwrapped(view, unwrapped, cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_graph_layout::graph::Node;
    use bviz_graph_layout::intern::Id;

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
            labels(&box_options(&BTreeSet::new(), &classes)),
            [
                "packages",
                "targets",
                "labelled outputs",
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

    fn none() -> BTreeSet<BoxClass> {
        BTreeSet::new()
    }

    #[test]
    fn a_row_is_checked_unless_its_class_is_unwrapped() {
        let classes = carried_boxes(&graph());
        let unwrapping = BTreeSet::from([BoxClass::Target]);

        assert_eq!(checked(&box_options(&none(), &classes)), [true; 4]);
        assert_eq!(
            checked(&box_options(&unwrapping, &classes)),
            [true, false, true, true]
        );
    }

    /// Each row's click reads back as its own checkmark and nothing else's, and
    /// checking it again returns to where it started.
    #[test]
    fn each_row_round_trips_through_its_class() {
        let classes = carried_boxes(&graph());
        for (index, class) in classes.iter().enumerate() {
            let toggle = |checked| MultiSelectEvent::Toggled { index, checked };
            let unwrapped = boxes_chosen(&none(), &classes, &toggle(false));

            let expected: Vec<bool> = (0..classes.len()).map(|row| row != index).collect();
            assert_eq!(
                checked(&box_options(&unwrapped, &classes)),
                expected,
                "{class:?}"
            );
            assert_eq!(
                boxes_chosen(&unwrapped, &classes, &toggle(true)),
                none(),
                "{class:?}"
            );
        }
    }

    /// A click over a range writes every row it covers as a single click on
    /// that row would.
    #[test]
    fn a_click_over_a_range_writes_every_row_it_covers() {
        let classes = carried_boxes(&graph());
        let started = BTreeSet::from([BoxClass::Target]);

        let cleared = boxes_chosen(&started, &classes, &set_all(0..classes.len(), false));
        assert_eq!(checked(&box_options(&cleared, &classes)), [false; 4]);

        let restored = boxes_chosen(&cleared, &classes, &set_all(1..2, true));
        assert_eq!(
            checked(&box_options(&restored, &classes)),
            [false, true, false, false]
        );
    }

    /// The map explains an empty level only while every class is hidden, and
    /// points at the outermost one.
    #[test]
    fn only_hiding_every_class_explains_an_empty_level() {
        let classes = carried_boxes(&graph());
        let hiding_all = boxes_chosen(&none(), &classes, &set_all(0..classes.len(), false));
        let notice = every_class_hidden(&hiding_all, &classes).expect("every class is hidden");
        assert!(
            notice.contains("Show packages in the boxes menu"),
            "{notice}"
        );

        let mut showing_one = hiding_all.clone();
        showing_one.remove(&BoxClass::ConfiguredTarget);
        assert_eq!(every_class_hidden(&showing_one, &classes), None);
        assert_eq!(every_class_hidden(&none(), &classes), None);
        assert_eq!(every_class_hidden(&hiding_all, &[]), None);
    }
}
