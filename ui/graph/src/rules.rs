//! The box classes the boxes menu unwraps, and the filter rule unwrapping
//! them.

use bviz_graph_layout::Kind;
use bviz_graph_layout::select::{Predicate, Rule, Values, Visibility};
use std::collections::BTreeSet;

/// A kind of node the drawing puts a box round what the node holds. Hiding
/// the class unwraps each such box, lifting what it holds to the nearest box
/// still drawn, and leaves a node of the kind that holds nothing as it is.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum BoxClass {
    Package,
    Target,
    /// A generated file under a label of its own, holding the configured
    /// targets it is built in.
    LabeledOutput,
    ConfiguredTarget,
}

impl BoxClass {
    /// The class of a box of `kind`; `None` for a kind that holds nothing in
    /// the graph a filter is evaluated over.
    pub(crate) fn of(kind: Kind) -> Option<Self> {
        match kind {
            Kind::Package => Some(BoxClass::Package),
            Kind::Target => Some(BoxClass::Target),
            Kind::Artifact => Some(BoxClass::LabeledOutput),
            Kind::ConfiguredTarget => Some(BoxClass::ConfiguredTarget),
            Kind::TargetGroup | Kind::MergeGroup | Kind::Action | Kind::Source => None,
        }
    }

    pub(crate) fn kind(self) -> Kind {
        match self {
            BoxClass::Package => Kind::Package,
            BoxClass::Target => Kind::Target,
            BoxClass::LabeledOutput => Kind::Artifact,
            BoxClass::ConfiguredTarget => Kind::ConfiguredTarget,
        }
    }
}

/// The rule unwrapping every box of the classes `unwrapped`; `None` where it
/// names none.
pub(crate) fn unwrapping(unwrapped: &BTreeSet<BoxClass>) -> Option<Rule> {
    let kinds = Values::new(unwrapped.iter().map(|class| class.kind()))?;
    Some(Rule {
        predicate: Predicate {
            kinds: Some(kinds),
            holding: true,
        },
        visibility: Visibility::Hidden,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_graph_layout::Id;
    use bviz_graph_layout::graph::{Graph, Node};
    use bviz_graph_layout::select::{Filter, Verdict};

    /// A package holding a target holding an action, beside a target holding
    /// nothing.
    fn graph() -> Graph {
        let mut graph = Graph::new();
        graph.add_node(Node::new(Id(0), "//app", Kind::Package, None));
        graph.add_node(Node::new(Id(1), ":bin", Kind::Target, Some(Id(0))));
        graph.add_node(Node::new(Id(2), "link", Kind::Action, Some(Id(1))));
        graph.add_node(Node::new(Id(3), ":empty", Kind::Target, Some(Id(0))));
        graph
    }

    /// A class unwrapped takes away each box of its kind holding another node,
    /// lifting what that held, and leaves a node of the kind holding nothing.
    #[test]
    fn a_class_unwrapped_hides_only_its_boxes_that_hold_something() {
        let filter = Filter {
            rules: unwrapping(&BTreeSet::from([BoxClass::Target]))
                .into_iter()
                .collect(),
            ..Filter::default()
        };
        let verdicts = filter.evaluate(&graph());

        assert_eq!(verdicts.get(Id(1)), Verdict::Hidden);
        assert_eq!(verdicts.get(Id(2)), Verdict::Shown);
        assert_eq!(verdicts.get(Id(3)), Verdict::Shown);
        assert_eq!(verdicts.get(Id(0)), Verdict::Shown);
    }

    /// Unwrapping nothing writes no rule at all.
    #[test]
    fn no_class_unwrapped_writes_no_rule() {
        assert_eq!(unwrapping(&BTreeSet::new()), None);
    }
}
