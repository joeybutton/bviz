//! The actions of a graph found as Bazel describes them.

use std::borrow::Cow;
use std::hash::Hash;

use bviz_graph_layout::aquery::description;
use bviz_graph_layout::{Facets, Graph, Id, Kind, Names, Resolver};
use bviz_invocation::DescribedAction;
use rustc_hash::{FxHashMap, FxHashSet};

/// The actions of a graph that some owners own, by how Bazel describes them.
pub(crate) struct DescribedActions<'a> {
    resolver: Resolver<'a>,
    /// By owning target, mnemonic and description, the primary output of the
    /// action they describe; `None` where they describe several.
    described: FxHashMap<(Id, &'a str, Cow<'a, str>), Option<&'a str>>,
    /// By owning target and mnemonic, the primary output of the action of
    /// both; `None` where several share both.
    by_mnemonic: FxHashMap<(Id, &'a str), Option<&'a str>>,
    /// Whether the graph's aquery states progress messages, which one by
    /// Bazel before 10.0.0-pre.20260723.1 does not.
    states_progress_messages: bool,
}

impl<'a> DescribedActions<'a> {
    /// The actions of `graph`, which `names` names and `facets` describes,
    /// that the owners of `wanted` own.
    pub(crate) fn new<'w>(
        graph: &Graph,
        names: &'a Names,
        facets: &'a Facets,
        wanted: impl Iterator<Item = DescribedAction<&'w str>>,
    ) -> Self {
        let resolver = Resolver::new(names);
        let mut wanted_actions = 0;
        let owners: FxHashSet<Id> = wanted
            .inspect(|_| wanted_actions += 1)
            .filter_map(|action| resolver.target(action.owner))
            .collect();
        let strings = facets.strings();
        // Sized for each action wanted being one of the graph's, of an owner
        // the graph holds.
        let mut described = FxHashMap::with_capacity_and_hasher(wanted_actions, Default::default());
        let mut by_mnemonic = FxHashMap::with_capacity_and_hasher(owners.len(), Default::default());
        let mut states_progress_messages = false;
        for node in &graph.nodes {
            if node.kind != Kind::Action {
                continue;
            }
            let progress_message = facets
                .progress_message(node.id)
                .map(|message| strings.resolve(message));
            states_progress_messages |= progress_message.is_some();
            let (Some(owner), Some(mnemonic), Some(output)) = (
                facets.label(node.id).filter(|owner| owners.contains(owner)),
                facets.mnemonic(node.id).map(|m| strings.resolve(m)),
                resolver.primary_output(node.id),
            ) else {
                continue;
            };
            let description = description(progress_message, mnemonic, output);
            note(&mut described, (owner, mnemonic, description), output);
            note(&mut by_mnemonic, (owner, mnemonic), output);
        }
        Self {
            resolver,
            described,
            by_mnemonic,
            states_progress_messages,
        }
    }

    /// The primary output of the one action of the graph `action` describes.
    /// `None` where the graph holds no such action, or holds several, as it
    /// does of an action its owner registers in each of several
    /// configurations, which the description does not tell apart: the
    /// attempts of such an action stay with its owner.
    ///
    /// Where the graph's aquery states no progress message, the description
    /// of an action setting one is unknown, and the action is found by owner
    /// and mnemonic alone where it is the only one of both.
    pub(crate) fn output(&self, action: DescribedAction<&str>) -> Option<&'a str> {
        let owner = self.resolver.target(action.owner)?;
        let described = (owner, action.mnemonic, Cow::Borrowed(action.description));
        match self.described.get(&described) {
            Some(&output) => output,
            None if !self.states_progress_messages => {
                *self.by_mnemonic.get(&(owner, action.mnemonic))?
            }
            None => None,
        }
    }
}

/// Notes the action writing `output` under `key`, which the actions noted
/// before it write other outputs under.
fn note<'a, K: Hash + Eq>(by: &mut FxHashMap<K, Option<&'a str>>, key: K, output: &'a str) {
    by.entry(key)
        .and_modify(|noted| *noted = None)
        .or_insert(Some(output));
}

#[cfg(test)]
mod tests {
    use bviz_graph_layout::graph::Node;

    use super::*;

    /// A graph of the target `//p:t`, configured as each of `configured`
    /// names, owning in each configuration the actions of `actions`: each
    /// with its mnemonic, the path of its primary output below its
    /// configuration's tree, and its progress message where it sets one.
    struct Built {
        graph: Graph,
        names: Names,
        facets: Facets,
    }

    impl Built {
        fn new(configured: &[&str], actions: &[(&str, &str, Option<&str>)]) -> Self {
            let mut names = Names::default();
            let mut graph = Graph::new();
            let mut facets = Facets::default();
            let target = names.intern("target://p:t");
            graph.add_node(Node::new(target, "t", Kind::Target, None));
            facets.set_label(target, target);
            for config in configured {
                let ct = names.intern(&format!("ct://p:t#{config}"));
                graph.add_node(Node::new(ct, *config, Kind::ConfiguredTarget, Some(target)));
                for &(mnemonic, output, message) in actions {
                    let path = format!("bazel-out/{config}/bin/{output}");
                    let id = names.intern(&format!("action:{path}"));
                    graph.add_node(Node::new(id, mnemonic, Kind::Action, Some(ct)));
                    facets.set_label(id, target);
                    let mnemonic = facets.intern_mnemonic(mnemonic);
                    facets.set_mnemonic(id, Some(mnemonic));
                    if let Some(message) = message {
                        let message = facets.intern_progress_message(message);
                        facets.set_progress_message(id, Some(message));
                    }
                }
            }
            Built {
                graph,
                names,
                facets,
            }
        }

        /// The primary output of the action of `//p:t` that `mnemonic` and
        /// `description` describe.
        fn output(&self, mnemonic: &str, description: &str) -> Option<&str> {
            let action = DescribedAction {
                owner: "//p:t",
                mnemonic,
                description,
            };
            let described =
                DescribedActions::new(&self.graph, &self.names, &self.facets, [action].into_iter());
            described.output(action)
        }
    }

    const GENRULE: &str = "Executing genrule //p:t";

    #[test]
    fn an_action_is_found_by_its_progress_message() {
        let built = Built::new(
            &["k8-fastbuild"],
            &[
                ("Genrule", "p/t.out", Some(GENRULE)),
                ("Symlink", "p/t.link", Some("Symlinking //p:t")),
            ],
        );
        assert_eq!(
            built.output("Genrule", GENRULE),
            Some("bazel-out/k8-fastbuild/bin/p/t.out")
        );
        assert_eq!(built.output("Genrule", "Executing genrule //p:u"), None);
        assert_eq!(built.output("Symlink", GENRULE), None);
    }

    #[test]
    fn an_action_setting_no_progress_message_is_found_by_its_primary_output() {
        let built = Built::new(
            &["k8-fastbuild"],
            &[
                ("Genrule", "p/t.out", Some(GENRULE)),
                ("Symlink", "p/a", None),
                ("Symlink", "p/b", None),
            ],
        );
        assert_eq!(
            built.output("Symlink", "Symlink p/b"),
            Some("bazel-out/k8-fastbuild/bin/p/b")
        );
        assert_eq!(built.output("Symlink", "Symlink p/c"), None);
    }

    #[test]
    fn an_action_registered_in_several_configurations_is_found_in_none() {
        let built = Built::new(
            &["k8-fastbuild", "k8-fastbuild-ST-1"],
            &[("Genrule", "p/t.out", Some(GENRULE))],
        );
        assert_eq!(built.output("Genrule", GENRULE), None);
    }

    #[test]
    fn where_no_progress_message_is_stated_an_action_is_found_alone_of_its_mnemonic() {
        let built = Built::new(
            &["k8-fastbuild"],
            &[
                ("Genrule", "p/t.out", None),
                ("Symlink", "p/a", None),
                ("Symlink", "p/b", None),
            ],
        );
        assert_eq!(
            built.output("Genrule", GENRULE),
            Some("bazel-out/k8-fastbuild/bin/p/t.out")
        );
        assert_eq!(built.output("Symlink", "Symlinking p/a"), None);
        assert_eq!(
            built.output("Symlink", "Symlink p/a"),
            Some("bazel-out/k8-fastbuild/bin/p/a")
        );
    }

    #[test]
    fn an_action_of_an_owner_the_graph_lacks_is_found_in_none() {
        let built = Built::new(&["k8-fastbuild"], &[("Genrule", "p/t.out", None)]);
        let action = DescribedAction {
            owner: "//q:t",
            mnemonic: "Genrule",
            description: GENRULE,
        };
        let described = DescribedActions::new(
            &built.graph,
            &built.names,
            &built.facets,
            [action].into_iter(),
        );
        assert_eq!(described.output(action), None);
    }
}
