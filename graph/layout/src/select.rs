//! Which nodes of a graph a filter shows, stated as rules over their kinds and
//! whether they hold another node.
//!
//! Rules name no node by id, so they hold of a graph rebuilt from the same
//! inputs; a scope and the pins name nodes by id, so they hold only of the
//! graph they were made over.

mod eval;
pub mod labels;

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use fixedbitset::FixedBitSet;

use crate::graph::Kind;
use crate::intern::Id;
pub use eval::{Hiding, Verdict, Verdicts};

/// What a filter says is to become of one node.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Visibility {
    Shown,
    Hidden,
    /// Drawn, but marked as no part of what the filter selects.
    Faded,
}

/// An override a filter holds for one node, by who set it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Pin {
    /// Set by a reader, deciding the node until they take it back.
    Set(Visibility),
    /// Shows a node the rules hide for a view that was asked to show it, held
    /// apart from a reader's own pins so it can be withdrawn without touching
    /// them.
    Revealed,
}

impl Pin {
    pub fn visibility(self) -> Visibility {
        match self {
            Pin::Set(visibility) => visibility,
            Pin::Revealed => Visibility::Shown,
        }
    }
}

/// The visibility a [`Predicate`] gives the nodes it matches.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Rule {
    pub predicate: Predicate,
    pub visibility: Visibility,
}

/// A filter over one graph's nodes.
///
/// The rules are read in order and the last one matching a node decides it, as
/// a gitignore file's patterns do, so a rule narrows or widens the ones before
/// it without either being rewritten; a node no rule matches is shown. The
/// scope hides what lies outside it ahead of every rule, so no rule draws back
/// a node the scope leaves out; a box it leaves out around a node drawn is
/// drawn as its frame, unless a rule hides the box, which unwraps it. `pins`
/// decide the nodes they name whatever the scope and the rules say, which is
/// what lets a node shown or hidden on its own outlast a change to a rule that
/// has nothing to do with it.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Filter {
    pub scope: Option<Scope>,
    pub rules: Vec<Rule>,
    pub pins: HashMap<Id, Pin>,
}

/// The nodes a query names, outside which a filter draws nothing but the
/// frames around what it draws. A frame — a package box, a merge group —
/// stands for no part of the build, so a scope neither holds nor hides one.
///
/// The nodes are ids of the one graph the query was evaluated over, so a scope
/// does not outlive that graph as the rules do.
///
/// A scope is made only by [`Universe::scope`](crate::query_lang::Universe::scope),
/// over the graph whose ids it holds.
#[derive(Clone, PartialEq, Eq)]
pub struct Scope {
    query: Arc<str>,
    nodes: Arc<FixedBitSet>,
}

impl Scope {
    pub(crate) fn new(query: Arc<str>, nodes: FixedBitSet) -> Self {
        Scope {
            query,
            nodes: Arc::new(nodes),
        }
    }

    /// The query naming the nodes, as the scope's maker wrote it.
    pub fn query(&self) -> &str {
        &self.query
    }

    fn holds(&self, node: &crate::graph::Node) -> bool {
        node.kind.is_frame() || self.nodes.contains(node.id.0 as usize)
    }
}

impl fmt::Debug for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scope")
            .field("query", &self.query)
            .field("nodes", &self.nodes.count_ones(..))
            .finish()
    }
}

/// The values one term admits, of which a node's facet must carry one.
///
/// Sorted and deduplicated, and never empty: a term admitting nothing matches no
/// node at all, which is what leaving the term unset already says.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Values<T>(Vec<T>);

impl<T: Ord> Values<T> {
    /// `None` where `values` is empty.
    pub fn new(values: impl IntoIterator<Item = T>) -> Option<Self> {
        let mut values: Vec<T> = values.into_iter().collect();
        values.sort_unstable();
        values.dedup();
        (!values.is_empty()).then_some(Values(values))
    }

    pub fn contains(&self, value: &T) -> bool {
        self.0.binary_search(value).is_ok()
    }
}

impl<T> Values<T> {
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }
}

/// A conjunction of terms over one node, each of which constrains the nodes
/// matching only where it is set.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Predicate {
    pub kinds: Option<Values<Kind>>,
    /// Whether the node must hold another node of the graph.
    pub holding: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Graph, Node};
    use crate::intern::Names;

    /// A graph of one target with an action below it, a second target holding
    /// nothing, and one source file.
    struct Fixture {
        graph: Graph,
        names: Names,
    }

    const APP: &str = "target://foo:app";
    const ACTION: &str = "action0";
    const LIB: &str = "target://bar:lib";
    const SOURCE: &str = "target://foo:main.c";

    impl Fixture {
        fn new() -> Self {
            let mut graph = Graph::new();
            let mut names = Names::default();
            let app = names.intern(APP);
            graph.add_node(Node::new(app, ":app", Kind::Target, None));
            let action = names.intern(ACTION);
            graph.add_node(Node::new(action, "CppCompile", Kind::Action, Some(app)));
            let lib = names.intern(LIB);
            graph.add_node(Node::new(lib, ":lib", Kind::Target, None));
            let source = names.intern(SOURCE);
            graph.add_node(Node::new(source, ":main.c", Kind::Source, None));
            Fixture { graph, names }
        }

        fn id(&self, name: &str) -> Id {
            self.names.get(name).expect("an interned node")
        }

        /// What `filter` makes of the node `name`.
        fn verdict(&self, filter: &Filter, name: &str) -> Verdict {
            filter.evaluate(&self.graph).get(self.id(name))
        }

        /// The nodes `predicate` matches, as a rule fading them alone reads.
        fn matching(&self, predicate: &Predicate) -> Vec<&str> {
            let fading = Filter {
                rules: vec![rule(predicate.clone(), Visibility::Faded)],
                ..Filter::default()
            };
            let verdicts = fading.evaluate(&self.graph);
            let mut matched: Vec<&str> = (self.graph.nodes.iter())
                .filter(|node| verdicts.get(node.id) == Verdict::Faded)
                .map(|node| self.names.resolve(node.id))
                .collect();
            matched.sort_unstable();
            matched
        }
    }

    #[test]
    fn a_predicate_with_no_term_matches_every_node() {
        let fixture = Fixture::new();
        assert_eq!(fixture.matching(&Predicate::default()).len(), 4);
    }

    #[test]
    fn a_term_over_values_none_of_which_is_named_constrains_nothing() {
        assert_eq!(Values::<Kind>::new([]), None);
    }

    #[test]
    fn each_term_selects_the_nodes_answering_it() {
        let fixture = Fixture::new();
        let targets = Predicate {
            kinds: Values::new([Kind::Target]),
            ..Predicate::default()
        };
        assert_eq!(fixture.matching(&targets), [LIB, APP]);

        let holding = Predicate {
            holding: true,
            ..Predicate::default()
        };
        assert_eq!(fixture.matching(&holding), [APP]);
    }

    /// Every term of a predicate must hold, so two terms no node answers
    /// together select nothing even though each selects on its own.
    #[test]
    fn a_predicate_holds_only_where_all_of_its_terms_do() {
        let fixture = Fixture::new();
        let both = Predicate {
            kinds: Values::new([Kind::Target]),
            holding: true,
        };
        assert_eq!(fixture.matching(&both), [APP]);

        let neither = Predicate {
            kinds: Values::new([Kind::Source]),
            holding: true,
        };
        assert!(fixture.matching(&neither).is_empty());
    }

    fn rule(predicate: Predicate, visibility: Visibility) -> Rule {
        Rule {
            predicate,
            visibility,
        }
    }

    fn hide_targets() -> Rule {
        rule(
            Predicate {
                kinds: Values::new([Kind::Target]),
                ..Predicate::default()
            },
            Visibility::Hidden,
        )
    }

    fn fade_holders() -> Rule {
        rule(
            Predicate {
                holding: true,
                ..Predicate::default()
            },
            Visibility::Faded,
        )
    }

    #[test]
    fn a_node_no_rule_matches_is_shown() {
        let fixture = Fixture::new();
        let filter = Filter {
            rules: vec![hide_targets()],
            ..Filter::default()
        };
        assert_eq!(fixture.verdict(&filter, SOURCE), Verdict::Shown);
    }

    #[test]
    fn the_last_rule_matching_a_node_decides_it() {
        let fixture = Fixture::new();
        let first = Filter {
            rules: vec![hide_targets(), fade_holders()],
            ..Filter::default()
        };
        assert_eq!(fixture.verdict(&first, APP), Verdict::Faded);

        let reversed = Filter {
            rules: vec![fade_holders(), hide_targets()],
            ..Filter::default()
        };
        assert_eq!(fixture.verdict(&reversed, APP), Verdict::Hidden);
    }

    #[test]
    fn a_pin_decides_the_node_it_names_whatever_the_rules_say() {
        let fixture = Fixture::new();
        let filter = Filter {
            scope: None,
            rules: vec![hide_targets()],
            pins: [(fixture.id(APP), Pin::Set(Visibility::Shown))]
                .into_iter()
                .collect(),
        };

        assert_eq!(fixture.verdict(&filter, APP), Verdict::Shown);
        assert_eq!(fixture.verdict(&filter, LIB), Verdict::Hidden);
    }
}
