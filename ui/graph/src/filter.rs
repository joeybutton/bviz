//! Which of the graph a reader has chosen to see, apart from how a view lays
//! out what that leaves.

use bviz_graph_layout::Id;
use bviz_graph_layout::graph::Graph;
use bviz_graph_layout::query_lang::Stack;
use bviz_graph_layout::select::{Filter, Pin, Rule, Scope, Verdicts};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

/// The query applied and the nodes a reader pinned: what decides which nodes
/// are any part of what is shown, whichever view of the graph shows them.
#[derive(Clone, Default, Debug)]
pub(crate) struct DocumentFilter {
    applied: Option<Applied>,
    pins: HashMap<Id, Pin>,
}

/// A stack of chips applied, with the scope it named over the graph.
#[derive(Clone, Debug)]
pub(crate) struct Applied {
    stack: Stack,
    scope: Scope,
}

impl Applied {
    pub(crate) fn new(stack: Stack, scope: Scope) -> Self {
        Applied { stack, scope }
    }

    pub(crate) fn stack(&self) -> &Stack {
        &self.stack
    }

    pub(crate) fn scope(&self) -> &Scope {
        &self.scope
    }
}

impl DocumentFilter {
    /// The query applied; `None` where none names a scope.
    pub(crate) fn applied(&self) -> Option<&Applied> {
        self.applied.as_ref()
    }

    fn scope(&self) -> Option<&Scope> {
        self.applied.as_ref().map(Applied::scope)
    }

    pub(crate) fn pins(&self) -> &HashMap<Id, Pin> {
        &self.pins
    }

    /// Replaces the query applied, reporting whether that changed the scope.
    pub(crate) fn set_applied(&mut self, applied: Option<Applied>) -> bool {
        let changed = self.scope() != applied.as_ref().map(Applied::scope);
        self.applied = applied;
        changed
    }

    /// Replaces the pins, reporting whether that changed them.
    pub(crate) fn set_pins(&mut self, pins: HashMap<Id, Pin>) -> bool {
        let changed = self.pins != pins;
        self.pins = pins;
        changed
    }

    /// The filter a view draws through, with `rules` of its own.
    pub(crate) fn with_rules(&self, rules: &[Rule]) -> Filter {
        Filter {
            scope: self.scope().cloned(),
            rules: rules.to_vec(),
            pins: self.pins.clone(),
        }
    }

    /// The filter of a view laying nothing out by rules of its own.
    pub(crate) fn filter(&self) -> Filter {
        self.with_rules(&[])
    }
}

/// A filter with its verdicts on one graph, evaluated once for every view
/// drawing through it.
pub(crate) struct Judgement {
    filter: Filter,
    graph: Arc<Graph>,
    verdicts: OnceLock<Arc<Verdicts>>,
}

impl Judgement {
    pub(crate) fn new(filter: Filter, graph: Arc<Graph>) -> Self {
        Judgement {
            filter,
            graph,
            verdicts: OnceLock::new(),
        }
    }

    pub(crate) fn filter(&self) -> &Filter {
        &self.filter
    }

    /// Whether this judges `graph` through `filter`.
    pub(crate) fn judges(&self, filter: &Filter, graph: &Arc<Graph>) -> bool {
        Arc::ptr_eq(&self.graph, graph) && self.filter == *filter
    }

    /// The verdicts, evaluated by the first caller, every other one waiting on
    /// that evaluation. Evaluation passes over every node of the graph, so
    /// this belongs off the UI thread.
    pub(crate) fn verdicts(&self) -> Arc<Verdicts> {
        let evaluate = || Arc::new(self.filter.evaluate(&self.graph));
        Arc::clone(self.verdicts.get_or_init(evaluate))
    }

    /// The verdicts, where they have been evaluated.
    pub(crate) fn evaluated(&self) -> Option<&Arc<Verdicts>> {
        self.verdicts.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{self, BoxClass};
    use bviz_graph_layout::Kind;
    use bviz_graph_layout::graph::Node;
    use bviz_graph_layout::select::{Verdict, Visibility};
    use std::collections::BTreeSet;

    /// A package holding a target holding an action.
    fn graph() -> Graph {
        let mut graph = Graph::new();
        graph.add_node(Node::new(Id(0), "//app", Kind::Package, None));
        graph.add_node(Node::new(Id(1), ":bin", Kind::Target, Some(Id(0))));
        graph.add_node(Node::new(Id(2), "link", Kind::Action, Some(Id(1))));
        graph
    }

    /// A panel unwrapping a box class draws without those boxes, while the
    /// filter the document hands every view leaves them shown.
    #[test]
    fn a_panels_layout_rule_leaves_the_documents_filter_alone() {
        let graph = graph();
        let document = DocumentFilter::default();
        let unwrapping = rules::unwrapping(&BTreeSet::from([BoxClass::Target]));

        let panel = document.with_rules(unwrapping.as_slice()).evaluate(&graph);
        assert_eq!(panel.get(Id(1)), Verdict::Hidden);

        let shared = document.filter().evaluate(&graph);
        assert_eq!(shared.get(Id(1)), Verdict::Shown);
        assert_eq!(shared.get(Id(2)), Verdict::Shown);
    }

    /// A pin the reader set decides its node over a panel's layout rule.
    #[test]
    fn a_pin_outranks_a_panels_layout_rule() {
        let graph = graph();
        let mut document = DocumentFilter::default();
        assert!(document.set_pins([(Id(1), Pin::Set(Visibility::Faded))].into()));
        assert!(!document.set_pins([(Id(1), Pin::Set(Visibility::Faded))].into()));
        let unwrapping = rules::unwrapping(&BTreeSet::from([BoxClass::Target]));

        let panel = document.with_rules(unwrapping.as_slice()).evaluate(&graph);
        assert_eq!(panel.get(Id(1)), Verdict::Faded);
    }

    /// Every caller shares the one evaluation.
    #[test]
    fn a_judgement_evaluates_its_filter_once_for_every_caller() {
        let graph = Arc::new(graph());
        let mut document = DocumentFilter::default();
        document.set_pins([(Id(2), Pin::Set(Visibility::Hidden))].into());
        let judgement = Judgement::new(document.filter(), Arc::clone(&graph));
        assert!(judgement.evaluated().is_none());
        assert!(judgement.judges(&document.filter(), &graph));
        assert!(!judgement.judges(&DocumentFilter::default().filter(), &graph));

        let first = judgement.verdicts();
        assert!(Arc::ptr_eq(&first, &judgement.verdicts()));
        assert!(judgement.evaluated().is_some());
        assert_eq!(first.get(Id(2)), Verdict::Hidden);
        assert_eq!(first.get(Id(1)), Verdict::Shown);
    }
}
