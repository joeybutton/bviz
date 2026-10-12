//! The nodes of a document's graph the jump palette offers, each with the text
//! it is matched by.

use crate::model::Shared;
use crate::point::{configuration_readings, mnemonic, primary_output, written_target_label};
use bviz_graph_layout::select::labels::package_label;
use bviz_graph_layout::shown::{Parts, Shown};
use bviz_graph_layout::{Id, Kind};
use std::ops::Range;

/// Which nodes the palette offers, cycled through in [`Scope::CYCLE`] order.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Scope {
    #[default]
    Targets,
    Configured,
    Actions,
    Files,
    All,
}

impl Scope {
    pub(crate) const CYCLE: [Scope; 5] = [
        Scope::Targets,
        Scope::Configured,
        Scope::Actions,
        Scope::Files,
        Scope::All,
    ];

    /// Where the scope stands in [`Scope::CYCLE`].
    fn at(self) -> usize {
        match self {
            Scope::Targets => 0,
            Scope::Configured => 1,
            Scope::Actions => 2,
            Scope::Files => 3,
            Scope::All => 4,
        }
    }

    pub(crate) fn next(self) -> Scope {
        Self::CYCLE[(self.at() + 1) % Self::CYCLE.len()]
    }

    pub(crate) fn previous(self) -> Scope {
        Self::CYCLE[(self.at() + Self::CYCLE.len() - 1) % Self::CYCLE.len()]
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Scope::Targets => "targets and packages",
            Scope::Configured => "configured targets",
            Scope::Actions => "actions",
            Scope::Files => "files",
            Scope::All => "everything",
        }
    }

    /// Whether the scope offers nodes of `kind`. A merge group is a box the
    /// drawing folds others into, not a node of the build, so none offers it.
    pub(crate) fn holds(self, kind: Kind) -> bool {
        let scoped = match kind {
            Kind::Package | Kind::Target | Kind::TargetGroup => Scope::Targets,
            Kind::ConfiguredTarget => Scope::Configured,
            Kind::Action => Scope::Actions,
            Kind::Source | Kind::Artifact => Scope::Files,
            Kind::MergeGroup => return false,
        };
        self == Scope::All || self == scoped
    }
}

/// One node the palette offers.
pub(crate) struct Candidate {
    pub(crate) id: Id,
    pub(crate) kind: Kind,
    /// What a query is matched against, as a reader writes it; the row reads
    /// it as it is shown.
    pub(crate) text: Box<str>,
    /// The chars of `text` that are no part of the node's own name: the
    /// package path leading up to a label's name, and the directories of an
    /// action's output, whose mnemonic and file name name it.
    pub(crate) path: Range<u32>,
    /// How many nodes depend on this one directly.
    pub(crate) rdeps: u32,
}

/// Every node of one document's graph the palette can offer, in the order the
/// graph holds them.
pub(crate) struct Candidates(Vec<Candidate>);

impl Candidates {
    pub(crate) fn of(shared: &Shared) -> Self {
        let graph = &shared.base;
        let rdeps = dependents(shared);
        let mut candidates: Vec<Candidate> = Vec::with_capacity(graph.nodes.len());
        candidates.extend(
            graph
                .nodes
                .iter()
                .filter(|node| Scope::All.holds(node.kind))
                .map(|node| {
                    let text = node_text(shared, node.id, node.kind);
                    Candidate {
                        id: node.id,
                        kind: node.kind,
                        path: path(&text, node.kind),
                        text: text.into_boxed_str(),
                        rdeps: rdeps[node.id.0 as usize],
                    }
                }),
        );
        Candidates(candidates)
    }

    pub(crate) fn len(&self) -> u32 {
        self.0.len() as u32
    }

    pub(crate) fn get(&self, index: u32) -> &Candidate {
        &self.0[index as usize]
    }

    /// The indices of the candidates `scope` offers.
    pub(crate) fn in_scope(&self, scope: Scope) -> impl Iterator<Item = u32> + '_ {
        (0u32..)
            .zip(&self.0)
            .filter(move |(_, candidate)| scope.holds(candidate.kind))
            .map(|(index, _)| index)
    }
}

/// How many distinct nodes depend directly on each node of `shared`'s graph,
/// indexed by id over the graph's whole id span. Several inputs can each state the same dependency, so the edges
/// between one pair are counted once.
fn dependents(shared: &Shared) -> Vec<u32> {
    let graph = &shared.base;
    let mut pairs: Vec<(Id, Id)> = Vec::with_capacity(graph.edges.len());
    pairs.extend(graph.edges.iter().map(|edge| (edge.to, edge.from)));
    pairs.sort_unstable();
    pairs.dedup();
    let mut counts = vec![0u32; bviz_graph_layout::graph::id_span(graph)];
    for (to, _) in pairs {
        counts[to.0 as usize] += 1;
    }
    counts
}

/// The chars of `text`, the text of a node of `kind`, that lead up to the
/// node's name: up to the `:` starting a label's name or past the last `/` of
/// a path, and for an action, the directories of the output following the
/// space after its mnemonic.
fn path(text: &str, kind: Kind) -> Range<u32> {
    let mut start = 0;
    let mut end = 0;
    for (index, c) in (0u32..).zip(text.chars()) {
        match c {
            ' ' if kind == Kind::Action && start == 0 => (start, end) = (index + 1, index + 1),
            ':' => return start..index,
            '/' => end = index + 1,
            _ => {}
        }
    }
    start..end
}

/// What the node `id` of `kind` is matched by, as a reader writes it.
pub(crate) fn node_text(shared: &Shared, id: Id, kind: Kind) -> String {
    match kind {
        Kind::Package => {
            package_path(shared, id).unwrap_or_else(|| node_label(shared, id).to_owned())
        }
        Kind::Action => match primary_output(id, shared) {
            Some(output) => format!("{} {output}", mnemonic(id, shared)),
            None => mnemonic(id, shared).to_owned(),
        },
        Kind::Source | Kind::Artifact if !labelled(shared, id) => {
            let file = node_label(shared, id);
            match written_target_label(id, shared) {
                Some(owner) => format!("{}/{file}", package_of(&owner)),
                None => file.to_owned(),
            }
        }
        _ => written_target_label(id, shared).unwrap_or_else(|| node_label(shared, id).to_owned()),
    }
}

/// Whether the node `id` is a target in its own right, whose label is its
/// own rather than its owner's.
pub(crate) fn labelled(shared: &Shared, id: Id) -> bool {
    shared.facets.label(id) == Some(id)
}

/// The caption the graph gives the node `id`; the base graph holds each node
/// at the position of its id.
fn node_label(shared: &Shared, id: Id) -> &str {
    shared
        .base
        .nodes
        .get(id.0 as usize)
        .map_or("", |node| node.label.as_str())
}

/// The package part of `label`, up to its `:`.
pub(crate) fn package_of(label: &str) -> &str {
    label.split_once(':').map_or(label, |(package, _)| package)
}

/// The whole path of the package box `id`, as a reader writes it.
fn package_path(shared: &Shared, id: Id) -> Option<String> {
    let path = package_label(&shared.names, id)?;
    Some(shared.labels.written(&path).into_owned())
}

/// What holds the node `id` of `kind`, as a reader writes it, where its
/// [text](node_text) does not already read it: the package holding a
/// package, and the target owning an action or a file that is no target
/// itself.
pub(crate) fn container(shared: &Shared, id: Id, kind: Kind) -> Option<String> {
    match kind {
        Kind::Package => {
            let parent = shared.base.nodes.get(id.0 as usize)?.parent?;
            package_path(shared, parent)
        }
        Kind::Action => written_target_label(id, shared),
        Kind::Source | Kind::Artifact if !labelled(shared, id) => written_target_label(id, shared),
        _ => None,
    }
}

/// How the row offering `candidate` reads its text: as the graph shows it,
/// parted where that leads up to the node's own name.
pub(crate) fn reading(shared: &Shared, candidate: &Candidate) -> (Shown, Parts) {
    let shown = shared.labels.repos().names().show(&candidate.text);
    let parts = Parts::of_label(shown.text()).or_else(|| {
        let text = shown.text();
        let path = path(text, candidate.kind);
        let byte = |char: u32| {
            text.char_indices()
                .nth(char as usize)
                .map_or(text.len(), |(byte, _)| byte)
        };
        Parts::led(text, byte(path.start), byte(path.end))
    });
    (shown, parts.unwrap_or_default())
}

/// `written`, a label or package path as a reader writes it, as the graph
/// shows it.
pub(crate) fn shown(shared: &Shared, written: &str) -> String {
    shared.labels.repos().names().show(written).into_text()
}

/// The short name of the configuration the node a row offers is built in.
pub(crate) fn configuration(shared: &Shared, candidate: &Candidate) -> Option<String> {
    let [short, _] = configuration_readings(candidate.id, shared);
    short
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::Placement;
    use crate::point::ConfigNames;
    use bviz_graph_layout::facets::{FacetsBuilder, Tri};
    use bviz_graph_layout::graph::{EdgeKind, Graph, Node};
    use bviz_graph_layout::select::labels::LabelIndex;
    use bviz_graph_layout::{Input, Names};
    use std::sync::Arc;

    /// A graph of one package holding a library and a binary depending on it,
    /// each configured and compiled, with a source and an output file.
    pub(crate) fn shared() -> Shared {
        let mut names = Names::default();
        let mut graph = Graph::new();
        let mut builder = FacetsBuilder::default();
        let config = builder
            .facets()
            .intern_config("3f2a01", Some("k8-fastbuild"), Tri::No);
        let mut add = |name: &str, label: &str, kind: Kind, parent: Option<Id>| {
            let id = names.intern(name);
            graph.add_node(Node::new(id, label, kind, parent));
            id
        };
        let root = add("pkg:", "//", Kind::Package, None);
        let app = add("pkg:app", "app", Kind::Package, Some(root));
        let lib = add("target://app:lib", ":lib", Kind::Target, Some(app));
        let bin = add("target://app:bin", ":bin", Kind::Target, Some(app));
        let lib_ct = add(
            "ct://app:lib#3f2a01",
            ":lib",
            Kind::ConfiguredTarget,
            Some(lib),
        );
        let bin_ct = add(
            "ct://app:bin#3f2a01",
            ":bin",
            Kind::ConfiguredTarget,
            Some(bin),
        );
        let src = add("target://app:lib.cc", ":lib.cc", Kind::Source, Some(app));
        let out = add("artifact7", "liblib.a", Kind::Artifact, Some(lib_ct));
        let compile = add(
            "action:bazel-out/app/liblib.a",
            "CppArchive",
            Kind::Action,
            Some(lib_ct),
        );
        for (target, label) in [
            (lib, "//app:lib"),
            (bin, "//app:bin"),
            (src, "//app:lib.cc"),
        ] {
            builder.describe_label(target, Some(app), label);
        }
        let facets = builder.facets();
        for (ct, target) in [(lib_ct, lib), (bin_ct, bin)] {
            facets.inherit(ct, target);
            facets.set_config(ct, Some(config));
        }
        for owned in [out, compile] {
            facets.inherit(owned, lib_ct);
        }
        let mnemonic = facets.intern_mnemonic("CppArchive");
        facets.set_mnemonic(compile, Some(mnemonic));
        facets.set_primary_output(compile, out);
        graph.add_edge(bin, lib, EdgeKind::Query);
        graph.add_edge(bin_ct, lib_ct, EdgeKind::Cquery);
        graph.add_edge(bin_ct, lib_ct, EdgeKind::Aquery);
        graph.add_edge(lib, src, EdgeKind::Query);
        let (facets, _) = builder.seal();
        let reduced_base = bviz_graph_layout::reduce::transitive_reduction(&graph);
        let containers = Arc::new(bviz_graph_layout::lod::expandable_containers(&graph));
        let labels = Arc::new(LabelIndex::of(&graph, &names, &facets));
        Shared {
            base: Arc::new(graph),
            reduced_base: Arc::new(reduced_base),
            map: Placement::pending().0,
            config_names: Arc::new(ConfigNames::of(&facets)),
            names: Arc::new(names),
            facets: Arc::new(facets),
            labels,
            containers,
            capabilities: bviz_graph_layout::capability::CapabilitiesBuilder::default().seal(),
            provenance: Arc::default(),
            inputs: Input::Aquery.into(),
        }
    }

    fn offered(candidates: &Candidates, scope: Scope) -> Vec<&str> {
        let mut texts: Vec<&str> = candidates
            .in_scope(scope)
            .map(|index| &*candidates.get(index).text)
            .collect();
        texts.sort_unstable();
        texts
    }

    #[test]
    fn each_scope_offers_its_own_kinds_and_all_offers_every_one() {
        let candidates = Candidates::of(&shared());

        assert_eq!(
            offered(&candidates, Scope::Targets),
            ["//", "//app", "//app:bin", "//app:lib"]
        );
        assert_eq!(
            offered(&candidates, Scope::Configured),
            ["//app:bin", "//app:lib"]
        );
        assert_eq!(
            offered(&candidates, Scope::Actions),
            ["CppArchive liblib.a"]
        );
        assert_eq!(
            offered(&candidates, Scope::Files),
            ["//app/liblib.a", "//app:lib.cc"]
        );
        assert_eq!(offered(&candidates, Scope::All).len(), 9);
    }

    #[test]
    fn the_path_leads_up_to_the_name() {
        assert_eq!(path("//app:lib", Kind::Target), 0..5);
        assert_eq!(path("//app:sub/lib.cc", Kind::Source), 0..5);
        assert_eq!(path("//app/sub", Kind::Package), 0..6);
        assert_eq!(path("liblib.a", Kind::Artifact), 0..0);
        assert_eq!(path("//app/my lib.a", Kind::Artifact), 0..6);
    }

    /// An action is named by its mnemonic and its output's file name, so only
    /// the directories of the output lead up to the name.
    #[test]
    fn an_actions_path_is_its_outputs_directories() {
        assert_eq!(path("CppArchive liblib.a", Kind::Action), 11..11);
        assert_eq!(
            path("CppArchive bazel-out/k8/bin/app/liblib.a", Kind::Action),
            11..32
        );
        assert_eq!(path("Genrule", Kind::Action), 0..0);
    }

    /// A dependency stated by several inputs is one dependent.
    #[test]
    fn a_dependent_counts_once_however_many_inputs_state_it() {
        let shared = shared();
        let candidates = Candidates::of(&shared);
        let rdeps = |name: &str| {
            let id = shared.names.get(name).expect("an interned node");
            (0..)
                .map(|index| candidates.get(index))
                .find(|candidate| candidate.id == id)
                .expect("an offered node")
                .rdeps
        };

        assert_eq!(rdeps("ct://app:lib#3f2a01"), 1);
        assert_eq!(rdeps("target://app:lib"), 1);
        assert_eq!(rdeps("target://app:bin"), 0);
    }

    #[test]
    fn a_row_names_the_container_its_text_leaves_out() {
        let shared = shared();
        let candidates = Candidates::of(&shared);
        let read = |name: &str| {
            let id = shared.names.get(name).expect("an interned node");
            let candidate = candidates
                .in_scope(Scope::All)
                .map(|index| candidates.get(index))
                .find(|candidate| candidate.id == id)
                .expect("an offered node");
            (
                container(&shared, candidate.id, candidate.kind),
                configuration(&shared, candidate),
            )
        };

        assert_eq!(read("pkg:app"), (Some("//".to_owned()), None));
        assert_eq!(read("target://app:lib"), (None, None));
        assert_eq!(
            read("ct://app:lib#3f2a01"),
            (None, Some("k8-fastbuild".to_owned()))
        );
        assert_eq!(
            read("action:bazel-out/app/liblib.a"),
            (
                Some("//app:lib".to_owned()),
                Some("k8-fastbuild".to_owned())
            )
        );
    }

    #[test]
    fn tab_cycles_every_scope_and_back() {
        let mut scope = Scope::default();
        for _ in Scope::CYCLE {
            scope = scope.next();
        }
        assert_eq!(scope, Scope::default());
        assert_eq!(Scope::default().previous(), Scope::All);
    }
}
