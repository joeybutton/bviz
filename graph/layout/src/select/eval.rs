//! Evaluating a [`Filter`] over every node of a graph at once.
//!
//! Each rule is resolved against the graph's facet tables before the nodes are
//! scanned, so that a term over strings becomes a set of handles or a column
//! read by id and testing one node costs a few integer lookups.

use crate::facets::{Facets, RepoRef, SparseFacet, Sym, Tri, WrittenRepos};
use crate::graph::{Graph, Kind, Node};
use crate::intern::{Id, Names};
use crate::label;
use crate::select::pattern::Package;
use crate::select::{Aspects, Filter, Predicate, Rule, Values, Visibility, stating};

impl Filter {
    /// The verdict on every node of `graph`, whose facets are `facets` and whose
    /// labels resolve through `names`.
    pub fn evaluate(&self, graph: &Graph, names: &Names, facets: &Facets) -> Verdicts {
        let ids = graph
            .nodes
            .iter()
            .map(|node| node.id.0 as usize + 1)
            .max()
            .unwrap_or(0);
        let reads_labels = self
            .rules
            .iter()
            .any(|rule| rule.predicate.label.is_some() || rule.predicate.package.is_some());
        let carried = match reads_labels {
            true => Carried::of(graph, facets),
            false => Carried::default(),
        };
        let written = WrittenRepos::of(facets);
        let rules: Vec<Resolved> = self
            .rules
            .iter()
            .map(|rule| Resolved::of(rule, names, facets, &written, &carried, ids))
            .collect();

        let mut stated: Vec<Option<Visibility>> = vec![None; ids];
        for node in &graph.nodes {
            if let Some(rule) = rules.iter().rev().find(|rule| rule.matches(node, facets)) {
                stated[node.id.0 as usize] = Some(rule.visibility);
            }
        }
        for (&id, &pinned) in &self.pins {
            if let Some(stated) = stated.get_mut(id.0 as usize) {
                *stated = Some(pinned);
            }
        }

        let unmatched = Verdict::from(self.base.visibility());
        let mut by_id: Vec<Verdict> = stated
            .iter()
            .map(|stated| stated.map_or(unmatched, Verdict::from))
            .collect();
        let parents = Parents::of(graph, ids);
        hide_empty_frames(graph, &parents, &mut by_id);
        frame_drawn_nodes(graph, &parents, &stated, &mut by_id);
        Verdicts { by_id, unmatched }
    }
}

/// What a filter makes of one node.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Verdict {
    Shown,
    Hidden,
    Faded,
    /// A containment ancestor of a drawn node that no rule or pin of the filter
    /// decided, so only the filter's base hid it. It is drawn as an ordinary
    /// container, which is what leaves the node below it stating the package
    /// and target it sits in instead of standing at the root of the drawing.
    Context,
}

impl Verdict {
    /// Whether the drawing holds a box for the node.
    pub fn drawn(self) -> bool {
        !matches!(self, Verdict::Hidden)
    }
}

impl From<Visibility> for Verdict {
    fn from(visibility: Visibility) -> Self {
        match visibility {
            Visibility::Shown => Verdict::Shown,
            Visibility::Hidden => Verdict::Hidden,
            Visibility::Faded => Verdict::Faded,
        }
    }
}

/// One graph's containment forest by node id, for the passes that walk from a
/// node towards its root.
struct Parents(Vec<Option<Id>>);

impl Parents {
    fn of(graph: &Graph, ids: usize) -> Self {
        let mut parent: Vec<Option<Id>> = vec![None; ids];
        for node in &graph.nodes {
            parent[node.id.0 as usize] = node.parent;
        }
        Parents(parent)
    }

    /// Where the node at `id` sits, `None` for a root and for a node whose
    /// parent the graph holds no node of, which leaves nothing above it to
    /// reach.
    fn above(&self, id: usize) -> Option<usize> {
        let parent = self.0[id]?.0 as usize;
        (parent < self.0.len()).then_some(parent)
    }
}

/// Hides every frame left with nothing to frame: a box standing for no more
/// than the nodes under it, none of which the drawing keeps, is a box drawn
/// around empty space. A filter discriminating on a target name leaves one —
/// a pattern naming one target, and text a reader is part way through typing,
/// each cover no package at all.
fn hide_empty_frames(graph: &Graph, parents: &Parents, by_id: &mut [Verdict]) {
    let mut framing: Vec<bool> = vec![false; by_id.len()];
    for node in &graph.nodes {
        let id = node.id.0 as usize;
        if node.kind.is_frame() || !by_id[id].drawn() {
            continue;
        }
        // Each frame is marked once and each walk stops at the first frame
        // already marked, so the whole forest costs one step per node.
        let mut at = parents.above(id);
        while let Some(ancestor) = at.filter(|&ancestor| !framing[ancestor]) {
            framing[ancestor] = true;
            at = parents.above(ancestor);
        }
    }
    for node in &graph.nodes {
        let id = node.id.0 as usize;
        if node.kind.is_frame() && !framing[id] {
            by_id[id] = Verdict::Hidden;
        }
    }
}

/// Turns into a [`Verdict::Context`] frame every containment ancestor of a drawn
/// node that `stated` leaves to the base and `by_id` therefore hides. Each
/// ancestor is decided on its own, so a rule hiding one of them unwraps that one
/// while the frames above and below it stand.
fn frame_drawn_nodes(
    graph: &Graph,
    parents: &Parents,
    stated: &[Option<Visibility>],
    by_id: &mut [Verdict],
) {
    for node in &graph.nodes {
        let id = node.id.0 as usize;
        if !by_id[id].drawn() {
            continue;
        }
        let mut at = parents.above(id);
        while let Some(ancestor) = at {
            if stated[ancestor].is_none() && !by_id[ancestor].drawn() {
                by_id[ancestor] = Verdict::Context;
            }
            at = parents.above(ancestor);
        }
    }
}

/// What a filter made of every node of one graph, by node id.
#[derive(Clone, Debug)]
pub struct Verdicts {
    by_id: Vec<Verdict>,
    unmatched: Verdict,
}

impl Verdicts {
    /// The verdict for the node `id` names. An id the evaluated graph does not
    /// name reads as the verdict of a node no rule reached.
    pub fn get(&self, id: Id) -> Verdict {
        self.by_id
            .get(id.0 as usize)
            .copied()
            .unwrap_or(self.unmatched)
    }
}

/// One rule with its predicate resolved against the graph being evaluated.
///
/// The terms reading one facet are resolved together, since the values a term
/// names and the categories another admits constrain that one facet and the
/// handles passing both are settled here rather than per node.
struct Resolved<'a> {
    kinds: Option<&'a Values<Kind>>,
    testonly: Option<Tri>,
    repos: Option<Admits<RepoRef>>,
    rule_classes: Option<Admits<Sym>>,
    configs: Option<Admits<Sym>>,
    mnemonics: Option<Admits<Sym>>,
    tags: Option<Column>,
    aspects: Option<Column>,
    admitted: Option<Admitted>,
    visibility: Visibility,
}

impl<'a> Resolved<'a> {
    fn of(
        rule: &'a Rule,
        names: &Names,
        facets: &Facets,
        written: &WrittenRepos,
        carried: &Carried,
        ids: usize,
    ) -> Self {
        let predicate = &rule.predicate;
        Resolved {
            kinds: predicate.kinds.as_ref(),
            testonly: predicate.testonly.map(stating),
            repos: repos(predicate, facets),
            rule_classes: rule_classes(predicate, facets),
            configs: configs(predicate, facets),
            mnemonics: predicate
                .mnemonics
                .as_ref()
                .map(|mnemonics| interned(mnemonics, facets)),
            tags: predicate.tags.as_ref().map(|tags| {
                let admitted = interned(tags, facets);
                column(facets.tags(), ids, false, |values| {
                    values.iter().any(|&tag| admitted.holds(tag))
                })
            }),
            aspects: predicate
                .aspects
                .as_ref()
                .map(|aspects| aspect_column(aspects, facets, ids)),
            admitted: Admitted::of(predicate, names, written, carried),
            visibility: rule.visibility,
        }
    }

    fn matches(&self, node: &Node, facets: &Facets) -> bool {
        let id = node.id;
        self.kinds.is_none_or(|kinds| kinds.contains(&node.kind))
            && self
                .testonly
                .is_none_or(|testonly| facets.testonly(id) == testonly)
            && self
                .repos
                .as_ref()
                .is_none_or(|repos| facets.repo(id).is_some_and(|repo| repos.holds(repo)))
            && self.rule_classes.as_ref().is_none_or(|rule_classes| {
                facets
                    .rule_class(id)
                    .is_some_and(|rule_class| rule_classes.holds(rule_class.sym()))
            })
            && self.configs.as_ref().is_none_or(|configs| {
                facets
                    .config(id)
                    .is_some_and(|config| configs.holds(config.sym()))
            })
            && self.mnemonics.as_ref().is_none_or(|mnemonics| {
                facets
                    .mnemonic(id)
                    .is_some_and(|mnemonic| mnemonics.holds(mnemonic))
            })
            && self.tags.as_ref().is_none_or(|tags| tags.holds(id))
            && self
                .aspects
                .as_ref()
                .is_none_or(|aspects| aspects.holds(id))
            && self
                .admitted
                .as_ref()
                .is_none_or(|admitted| admitted.holds(node, facets))
    }
}

/// The handles a term admits. Empty where every value the term names is one
/// this graph never interned, which admits no node at all.
struct Admits<T>(Vec<T>);

impl<T: Copy + Ord> Admits<T> {
    fn of(handles: impl Iterator<Item = T>) -> Self {
        let mut handles: Vec<T> = handles.collect();
        handles.sort_unstable();
        handles.dedup();
        Admits(handles)
    }

    fn holds(&self, handle: T) -> bool {
        self.0.binary_search(&handle).is_ok()
    }
}

/// What the terms over labels are resolved against, gathered once for a whole
/// filter: the targets its graph's nodes belong to, and the boxes standing for
/// its packages, each sorted and stated once.
#[derive(Default)]
struct Carried {
    targets: Vec<Id>,
    /// Each package box beside the box holding it, by which a term's answer for
    /// a box is read together with its answer for the boxes below it.
    packages: Vec<(Id, Option<Id>)>,
}

impl Carried {
    fn of(graph: &Graph, facets: &Facets) -> Self {
        let mut targets: Vec<Id> = Vec::with_capacity(graph.nodes.len());
        let mut packages: Vec<(Id, Option<Id>)> = Vec::with_capacity(graph.nodes.len());
        for node in &graph.nodes {
            if let Some(target) = facets.label(node.id) {
                targets.push(target);
            }
            if node.kind == Kind::Package {
                packages.push((node.id, node.parent));
            }
        }
        targets.sort_unstable();
        targets.dedup();
        packages.sort_unstable();
        Carried { targets, packages }
    }
}

/// What the terms over labels admit, by the id a node answers through: the
/// target it belongs to, which several nodes share, or the box itself where it
/// belongs to none — see
/// [`Pattern::covers_package`](crate::select::pattern::Pattern::covers_package).
/// Sorted, as the [`Carried`] ids they are drawn from are.
struct Admitted {
    targets: Vec<Id>,
    packages: Vec<Id>,
}

impl Admitted {
    /// What the terms over labels of `predicate` admit of `carried`: one match
    /// per target and one per package box, whatever number of nodes answer
    /// through each. `None` where the predicate states neither term.
    fn of(
        predicate: &Predicate,
        names: &Names,
        written: &WrittenRepos,
        carried: &Carried,
    ) -> Option<Self> {
        let (pattern, package) = (predicate.label.as_ref(), predicate.package.as_ref());
        if pattern.is_none() && package.is_none() {
            return None;
        }
        let pattern = pattern.map(|pattern| pattern.resolved(written));
        let package = package.map(|package| package.resolved(written));
        let targets = carried
            .targets
            .iter()
            .copied()
            .filter(|&id| {
                label::target_node_label(names.resolve(id)).is_some_and(|label| {
                    pattern
                        .as_ref()
                        .is_none_or(|pattern| pattern.matches(label))
                        && package
                            .as_ref()
                            .is_none_or(|package| package.matches(label))
                })
            })
            .collect();
        let covered: Vec<bool> = carried
            .packages
            .iter()
            .map(|&(id, _)| {
                label::package_node_parts(names.resolve(id)).is_some_and(|(repo, path)| {
                    let named = Package::written(repo, path);
                    pattern
                        .as_ref()
                        .is_none_or(|pattern| pattern.covers_package(named))
                        && package
                            .as_ref()
                            .is_none_or(|package| package.covers_package(named))
                })
            })
            .collect();
        Some(Admitted {
            targets,
            packages: covered_throughout(&carried.packages, covered),
        })
    }

    fn holds(&self, node: &Node, facets: &Facets) -> bool {
        match facets.label(node.id) {
            Some(target) => self.targets.binary_search(&target).is_ok(),
            None => self.packages.binary_search(&node.id).is_ok(),
        }
    }
}

/// The boxes of `packages` — each beside the box holding it — that `covered`
/// holds and holds for every box below them, in the order `packages` is sorted
/// in.
///
/// A box is the frame its sub-packages are drawn in as much as it is the frame
/// its own targets are drawn in, and a term reaching it unwraps that frame
/// whole. Admitting a box the term covers without covering the boxes below it
/// would leave those standing at the root of the drawing, away from the
/// packages that enclose them.
fn covered_throughout(packages: &[(Id, Option<Id>)], mut covered: Vec<bool>) -> Vec<Id> {
    for below in 0..covered.len() {
        if covered[below] {
            continue;
        }
        let mut at = packages[below].1;
        // Each box is struck out at most once, and each walk stops at the first
        // box already struck out, so this costs one step per box.
        while let Some(parent) = at {
            let Ok(above) = packages.binary_search_by_key(&parent, |&(id, _)| id) else {
                break;
            };
            if !covered[above] {
                break;
            }
            covered[above] = false;
            at = packages[above].1;
        }
    }
    packages
        .iter()
        .zip(covered)
        .filter(|&(_, covered)| covered)
        .map(|(&(id, _), _)| id)
        .collect()
}

/// A term's answer for each id, settled before the nodes are scanned.
struct Column {
    held: Vec<bool>,
    /// The answer for an id past the ones the column was built over, which is
    /// the answer for a node carrying nothing of the facet the term reads.
    beyond: bool,
}

impl Column {
    fn holds(&self, id: Id) -> bool {
        self.held.get(id.0 as usize).copied().unwrap_or(self.beyond)
    }
}

/// The handles of the values `values` names, dropping every name this graph
/// never interned.
fn interned(values: &Values<String>, facets: &Facets) -> Admits<Sym> {
    Admits::of(
        values
            .as_slice()
            .iter()
            .filter_map(|value| facets.strings().get(value)),
    )
}

fn repos(predicate: &Predicate, facets: &Facets) -> Option<Admits<RepoRef>> {
    let (names, categories) = (predicate.repos.as_ref(), predicate.repo_categories.as_ref());
    if names.is_none() && categories.is_none() {
        return None;
    }
    Some(Admits::of(
        facets
            .repos()
            .filter(|(repo, entry)| {
                let name = entry.name().map(|name| facets.strings().resolve(name));
                names.is_none_or(|names| {
                    names
                        .as_slice()
                        .iter()
                        .any(|named| named.as_deref() == name)
                }) && categories
                    .is_none_or(|categories| categories.contains(&facets.repo_category(*repo)))
            })
            .map(|(repo, _)| repo),
    ))
}

fn rule_classes(predicate: &Predicate, facets: &Facets) -> Option<Admits<Sym>> {
    let (names, categories) = (
        predicate.rule_classes.as_ref(),
        predicate.rule_categories.as_ref(),
    );
    if names.is_none() && categories.is_none() {
        return None;
    }
    Some(Admits::of(
        facets
            .rule_classes()
            .filter(|(rule_class, category)| {
                names.is_none_or(|names| names.contains(facets.strings().resolve(rule_class.sym())))
                    && categories.is_none_or(|categories| categories.contains(category))
            })
            .map(|(rule_class, _)| rule_class.sym()),
    ))
}

fn configs(predicate: &Predicate, facets: &Facets) -> Option<Admits<Sym>> {
    let (checksums, is_tool) = (predicate.configs.as_ref(), predicate.is_tool);
    if checksums.is_none() && is_tool.is_none() {
        return None;
    }
    Some(Admits::of(
        facets
            .configs()
            .filter(|(config, entry)| {
                checksums.is_none_or(|checksums| {
                    checksums.contains(facets.strings().resolve(config.sym()))
                }) && is_tool.is_none_or(|is_tool| entry.is_tool == stating(is_tool))
            })
            .map(|(config, _)| config.sym()),
    ))
}

fn aspect_column(aspects: &Aspects, facets: &Facets, ids: usize) -> Column {
    match aspects {
        Aspects::Any => column(facets.aspects(), ids, false, |values| !values.is_empty()),
        Aspects::None => column(facets.aspects(), ids, true, |values| values.is_empty()),
        Aspects::Named(named) => {
            let admitted = interned(named, facets);
            column(facets.aspects(), ids, false, |values| {
                values.iter().any(|&value| admitted.holds(value))
            })
        }
    }
}

/// A column over `ids` answering `carried` for every node `facet` records values
/// for and `bare` for every other node, which carries none.
fn column(facet: &SparseFacet, ids: usize, bare: bool, carried: impl Fn(&[Sym]) -> bool) -> Column {
    let mut held = vec![bare; ids];
    for (id, values) in facet.rows() {
        if let Some(slot) = held.get_mut(id.0 as usize) {
            *slot = carried(values);
        }
    }
    Column { held, beyond: bare }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::facets::{FacetsBuilder, RuleCategory};
    use crate::repo::{RepoCategory, RepoName};
    use crate::select::Base;
    use crate::select::pattern::{PackagePrefix, Pattern};

    /// A graph holding, in the main repository, a `cc_test` target with the
    /// configured target, action, and output artifact it owns, all of which
    /// inherit the target's facets; beside them a `cc_library` target of an
    /// external repository built for a tool, a source file no input described
    /// beyond its label, and a group box such as the quotient pass mints, whose
    /// id lies past every facet the inputs stated.
    struct Fixture {
        graph: Graph,
        names: Names,
        facets: Facets,
    }

    const APP: &str = "target://foo:app";
    const CONFIGURED: &str = "ct://foo:app#ck1";
    const ACTION: &str = "action0";
    const OUTPUT: &str = "artifact://foo:app.o#ck1";
    const LIB: &str = "target:@@rules_foo+//bar:lib";
    const SOURCE: &str = "target://foo:main.c";
    const GROUP: &str = "group:target://foo:main.c";

    impl Fixture {
        fn new() -> Self {
            let mut graph = Graph::new();
            let mut names = Names::default();
            let mut builder = FacetsBuilder::default();

            let app = names.intern(APP);
            graph.add_node(Node::new(app, ":app", Kind::Target, None));
            builder.describe_label(app, None, "//foo:app");
            builder.set_tags(app, ["manual"]);
            builder.set_aspects(app, ["//tools:coverage.bzl%instrument"]);
            let facets = builder.facets();
            let rule_class = facets.intern_rule_class("cc_test");
            facets.set_rule_class(app, Some(rule_class));
            let config = facets.intern_config("ck1", Some("k8-fastbuild"), Tri::No);
            facets.set_config(app, Some(config));
            facets.set_testonly(app, Tri::Yes);

            for (name, kind, label) in [
                (CONFIGURED, Kind::ConfiguredTarget, ":app"),
                (ACTION, Kind::Action, "CppCompile"),
                (OUTPUT, Kind::Artifact, ":app.o"),
            ] {
                let id = names.intern(name);
                graph.add_node(Node::new(id, label, kind, Some(app)));
                builder.facets().inherit(id, app);
            }
            let action = names.get(ACTION).expect("the action just interned");
            let facets = builder.facets();
            let mnemonic = facets.intern_mnemonic("CppCompile");
            facets.set_mnemonic(action, Some(mnemonic));

            let lib = names.intern(LIB);
            graph.add_node(Node::new(lib, ":lib", Kind::Target, None));
            builder.describe_label(lib, None, "@@rules_foo+//bar:lib");
            let facets = builder.facets();
            let rule_class = facets.intern_rule_class("cc_library");
            facets.set_rule_class(lib, Some(rule_class));
            let exec = facets.intern_config("ck2", Some("k8-opt-exec"), Tri::Yes);
            facets.set_config(lib, Some(exec));

            let source = names.intern(SOURCE);
            graph.add_node(Node::new(source, ":main.c", Kind::Source, None));
            builder.describe_label(source, None, "//foo:main.c");

            let (facets, _) = builder.seal();
            let group = names.intern(GROUP);
            graph.add_node(Node::new(group, "2 sources", Kind::Source, None));

            Fixture {
                graph,
                names,
                facets,
            }
        }

        fn evaluate(&self, filter: &Filter) -> Verdicts {
            filter.evaluate(&self.graph, &self.names, &self.facets)
        }

        fn id(&self, name: &str) -> Id {
            self.names.get(name).expect("an interned node")
        }

        /// The nodes `filter` leaves with `verdict`, by name.
        fn nodes(&self, filter: &Filter, verdict: Verdict) -> BTreeSet<&str> {
            let verdicts = self.evaluate(filter);
            self.graph
                .nodes
                .iter()
                .filter(|node| verdicts.get(node.id) == verdict)
                .map(|node| self.names.resolve(node.id))
                .collect()
        }
    }

    fn named<'a>(names: impl IntoIterator<Item = &'a str>) -> BTreeSet<&'a str> {
        names.into_iter().collect()
    }

    /// A filter hiding every node but the ones `predicate` matches.
    fn showing(predicate: Predicate) -> Filter {
        Filter {
            base: Base::HideAll,
            rules: vec![Rule {
                predicate,
                visibility: Visibility::Shown,
            }],
            ..Filter::default()
        }
    }

    fn values(values: &[&str]) -> Option<Values<String>> {
        Values::new(values.iter().map(|&value| value.to_string()))
    }

    fn repos(repos: &[RepoName<&str>]) -> Option<Values<RepoName<String>>> {
        Values::new(repos.iter().map(|repo| repo.map(str::to_string)))
    }

    #[test]
    fn a_repository_term_keeps_the_nodes_of_the_repositories_it_names() {
        let fixture = Fixture::new();
        let main = showing(Predicate {
            repo_categories: Values::new([RepoCategory::Main]),
            ..Predicate::default()
        });

        assert_eq!(
            fixture.nodes(&main, Verdict::Shown),
            named([APP, CONFIGURED, ACTION, OUTPUT, SOURCE])
        );
        assert_eq!(fixture.nodes(&main, Verdict::Hidden), named([LIB, GROUP]));
    }

    /// A repository term reads the name identifying each repository, so a rule
    /// naming one written canonically leaves a repository the inputs named only
    /// apparently drawn, however alike the two names read.
    #[test]
    fn a_repository_term_tells_an_apparent_name_from_a_canonical_one() {
        const APPARENT: &str = "target:@somerepo//foo:lib";
        const CANONICAL: &str = "target:@@somerepo//bar:lib";

        let mut graph = Graph::new();
        let mut names = Names::default();
        let mut builder = FacetsBuilder::default();
        for (node, label) in [
            (APPARENT, "@somerepo//foo:lib"),
            (CANONICAL, "@@somerepo//bar:lib"),
        ] {
            let id = names.intern(node);
            graph.add_node(Node::new(id, ":lib", Kind::Target, None));
            builder.describe_label(id, None, label);
        }
        let (facets, _) = builder.seal();

        let shown = |named: RepoName<&str>| {
            let filter = showing(Predicate {
                repos: repos(&[named]),
                ..Predicate::default()
            });
            let verdicts = filter.evaluate(&graph, &names, &facets);
            graph
                .nodes
                .iter()
                .filter(|node| verdicts.get(node.id) == Verdict::Shown)
                .map(|node| names.resolve(node.id))
                .collect::<BTreeSet<&str>>()
        };

        assert_eq!(shown(RepoName::Apparent("somerepo")), named([APPARENT]));
        assert_eq!(shown(RepoName::Canonical("somerepo")), named([CANONICAL]));
    }

    /// A repository named only apparently after a module the graph names
    /// canonically is selected as a module repository, so the origin a checklist
    /// lists it under reaches it.
    #[test]
    fn a_repository_term_over_modules_reaches_an_apparently_named_module_repository() {
        const APPARENT: &str = "target:@llvm//lib:core";
        const GENERATED: &str = "target:@@llvm++llvm+llvm-project//lib:support";

        let mut graph = Graph::new();
        let mut names = Names::default();
        let mut builder = FacetsBuilder::default();
        for (node, label) in [
            (APPARENT, "@llvm//lib:core"),
            (GENERATED, "@@llvm++llvm+llvm-project//lib:support"),
        ] {
            let id = names.intern(node);
            graph.add_node(Node::new(id, ":lib", Kind::Target, None));
            builder.describe_label(id, None, label);
        }
        let (facets, _) = builder.seal();

        let modules = showing(Predicate {
            repo_categories: Values::new([RepoCategory::Module]),
            ..Predicate::default()
        });
        let verdicts = modules.evaluate(&graph, &names, &facets);

        assert_eq!(
            graph
                .nodes
                .iter()
                .filter(|node| verdicts.get(node.id) == Verdict::Shown)
                .map(|node| names.resolve(node.id))
                .collect::<BTreeSet<&str>>(),
            named([APPARENT])
        );
    }

    #[test]
    fn a_repository_the_graph_never_saw_keeps_nothing() {
        let fixture = Fixture::new();
        let elsewhere = showing(Predicate {
            repos: repos(&[RepoName::Canonical("rules_bar+")]),
            ..Predicate::default()
        });

        assert!(fixture.nodes(&elsewhere, Verdict::Shown).is_empty());
    }

    /// A configured target, an action, and an output artifact inherit the rule
    /// class of the target they belong to, so a term over rule classes reaches
    /// the whole of what a target became and not the target box alone.
    #[test]
    fn a_rule_class_term_reaches_everything_a_target_became() {
        let fixture = Fixture::new();
        let tests = showing(Predicate {
            rule_categories: Values::new([RuleCategory::Test]),
            ..Predicate::default()
        });

        assert_eq!(
            fixture.nodes(&tests, Verdict::Shown),
            named([APP, CONFIGURED, ACTION, OUTPUT])
        );
    }

    #[test]
    fn a_label_pattern_reaches_the_nodes_of_the_targets_it_names() {
        let fixture = Fixture::new();
        let foo = showing(Predicate {
            label: Some(Pattern::parse("//foo/...")),
            ..Predicate::default()
        });

        assert_eq!(
            fixture.nodes(&foo, Verdict::Shown),
            named([APP, CONFIGURED, ACTION, OUTPUT, SOURCE])
        );
    }

    /// A label term is matched over the targets the graph's nodes belong to, so
    /// what it costs to resolve tracks the graph rather than the name table —
    /// which on a joint graph holds millions of names, most of them no node's
    /// label.
    #[test]
    fn a_label_term_is_matched_over_the_labels_the_nodes_carry() {
        let mut graph = Graph::new();
        let mut names = Names::default();
        let mut builder = FacetsBuilder::default();
        let app = names.intern(APP);
        graph.add_node(Node::new(app, ":app", Kind::Target, None));
        builder.describe_label(app, None, "//foo:app");
        names.intern("target://foo:unbuilt");
        let (facets, _) = builder.seal();

        let carried = Carried::of(&graph, &facets);
        let admitted = Admitted::of(
            &Predicate {
                label: Some(Pattern::parse("//foo/...")),
                ..Predicate::default()
            },
            &names,
            &WrittenRepos::of(&facets),
            &carried,
        )
        .expect("a label term");

        assert_eq!(
            carried.targets.len(),
            1,
            "the one label a node of the graph carries",
        );
        assert!(admitted.holds(&graph.nodes[0], &facets));
    }

    /// The quotient pass mints group ids above every id the inputs described, so
    /// a group answers a term over any facet as a node carrying none does.
    #[test]
    fn an_id_past_the_facets_carries_nothing_and_answers_every_term() {
        let fixture = Fixture::new();
        let group = fixture.id(GROUP);
        let over_facets = showing(Predicate {
            rule_classes: values(&["cc_test"]),
            tags: values(&["manual"]),
            aspects: Some(Aspects::Any),
            label: Some(Pattern::parse("//foo/...")),
            ..Predicate::default()
        });
        assert_eq!(fixture.evaluate(&over_facets).get(group), Verdict::Hidden);

        let unaspected = showing(Predicate {
            aspects: Some(Aspects::None),
            ..Predicate::default()
        });
        assert_eq!(fixture.evaluate(&unaspected).get(group), Verdict::Shown);

        let everything = showing(Predicate::default());
        assert_eq!(fixture.evaluate(&everything).get(group), Verdict::Shown);
    }

    #[test]
    fn an_id_the_graph_does_not_name_reads_as_a_node_no_rule_reached() {
        let fixture = Fixture::new();
        let verdicts = fixture.evaluate(&showing(Predicate::default()));

        assert_eq!(verdicts.get(Id(9_999)), Verdict::Hidden);
    }

    #[test]
    fn a_pin_decides_the_node_it_names_whatever_the_rules_say() {
        let fixture = Fixture::new();
        let mut filter = showing(Predicate {
            kinds: Values::new([Kind::Target]),
            ..Predicate::default()
        });
        filter.pins.insert(fixture.id(APP), Visibility::Faded);
        filter.pins.insert(fixture.id(SOURCE), Visibility::Shown);

        let verdicts = fixture.evaluate(&filter);
        assert_eq!(verdicts.get(fixture.id(APP)), Verdict::Faded);
        assert_eq!(verdicts.get(fixture.id(SOURCE)), Verdict::Shown);
        assert_eq!(verdicts.get(fixture.id(LIB)), Verdict::Shown);
        assert_eq!(verdicts.get(fixture.id(ACTION)), Verdict::Hidden);
    }

    /// A filter whose rules overlap, so that the order they are read in decides
    /// several of the nodes and a pin decides one more.
    fn layered(fixture: &Fixture) -> Filter {
        let rule = |predicate, visibility| Rule {
            predicate,
            visibility,
        };
        Filter {
            base: Base::HideAll,
            rules: vec![
                rule(Predicate::default(), Visibility::Shown),
                rule(
                    Predicate {
                        kinds: Values::new([Kind::Action, Kind::Artifact]),
                        ..Predicate::default()
                    },
                    Visibility::Hidden,
                ),
                rule(
                    Predicate {
                        rule_categories: Values::new([RuleCategory::Test]),
                        testonly: Some(true),
                        ..Predicate::default()
                    },
                    Visibility::Faded,
                ),
                rule(
                    Predicate {
                        repos: repos(&[RepoName::Canonical("rules_foo+")]),
                        is_tool: Some(true),
                        ..Predicate::default()
                    },
                    Visibility::Hidden,
                ),
                rule(
                    Predicate {
                        mnemonics: values(&["CppCompile"]),
                        ..Predicate::default()
                    },
                    Visibility::Shown,
                ),
                rule(
                    Predicate {
                        package: PackagePrefix::parse("//foo"),
                        aspects: Some(Aspects::Named(
                            values(&["//tools:coverage.bzl%instrument"]).expect("one aspect"),
                        )),
                        ..Predicate::default()
                    },
                    Visibility::Faded,
                ),
                rule(
                    Predicate {
                        label: Some(Pattern::parse("main.c")),
                        ..Predicate::default()
                    },
                    Visibility::Shown,
                ),
            ],
            pins: [(fixture.id(OUTPUT), Visibility::Faded)]
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn overlapping_rules_leave_each_node_with_the_last_verdict_reached() {
        let fixture = Fixture::new();
        let filter = layered(&fixture);

        assert_eq!(
            fixture.nodes(&filter, Verdict::Shown),
            named([ACTION, SOURCE, GROUP])
        );
        assert_eq!(
            fixture.nodes(&filter, Verdict::Faded),
            named([APP, CONFIGURED, OUTPUT])
        );
        assert_eq!(fixture.nodes(&filter, Verdict::Hidden), named([LIB]));
    }

    /// Two packages, each holding a target, the first holding two actions and the
    /// second one, so a filter that shows a single action leaves ancestors to
    /// keep, a sibling to leave out, and a second package to leave untouched.
    struct Nested {
        graph: Graph,
        names: Names,
        facets: Facets,
    }

    const PACKAGE: &str = "//foo";
    const TARGET: &str = "//foo:app";
    const COMPILE: &str = "//foo:app#compile";
    const LINK: &str = "//foo:app#link";
    const OTHER_PACKAGE: &str = "//bar";
    const OTHER_TARGET: &str = "//bar:lib";
    const OTHER_ACTION: &str = "//bar:lib#compile";

    impl Nested {
        fn new() -> Self {
            let mut graph = Graph::new();
            let mut names = Names::default();
            for (package, target, actions) in [
                (PACKAGE, TARGET, [COMPILE, LINK].as_slice()),
                (OTHER_PACKAGE, OTHER_TARGET, [OTHER_ACTION].as_slice()),
            ] {
                let package_id = names.intern(package);
                graph.add_node(Node::new(package_id, package, Kind::Package, None));
                let target_id = names.intern(target);
                graph.add_node(Node::new(target_id, target, Kind::Target, Some(package_id)));
                for &action in actions {
                    let action_id = names.intern(action);
                    graph.add_node(Node::new(action_id, action, Kind::Action, Some(target_id)));
                }
            }
            let (facets, _) = FacetsBuilder::default().seal();
            Nested {
                graph,
                names,
                facets,
            }
        }

        fn id(&self, name: &str) -> Id {
            self.names.get(name).expect("an interned node")
        }

        fn evaluate(&self, filter: &Filter) -> Verdicts {
            filter.evaluate(&self.graph, &self.names, &self.facets)
        }

        /// A filter that draws nothing of its own accord and shows `name` alone.
        fn showing_one(&self, name: &str) -> Filter {
            Filter {
                base: Base::HideAll,
                pins: [(self.id(name), Visibility::Shown)].into_iter().collect(),
                ..Filter::default()
            }
        }
    }

    #[test]
    fn a_shown_node_keeps_the_ancestors_only_the_base_hides() {
        let fixture = Nested::new();
        let verdicts = fixture.evaluate(&fixture.showing_one(COMPILE));

        assert_eq!(verdicts.get(fixture.id(COMPILE)), Verdict::Shown);
        assert_eq!(verdicts.get(fixture.id(TARGET)), Verdict::Context);
        assert_eq!(verdicts.get(fixture.id(PACKAGE)), Verdict::Context);
    }

    #[test]
    fn a_context_frame_keeps_none_of_its_other_children() {
        let fixture = Nested::new();
        let verdicts = fixture.evaluate(&fixture.showing_one(COMPILE));

        assert_eq!(verdicts.get(fixture.id(LINK)), Verdict::Hidden);
        for elsewhere in [OTHER_PACKAGE, OTHER_TARGET, OTHER_ACTION] {
            assert_eq!(verdicts.get(fixture.id(elsewhere)), Verdict::Hidden);
        }
    }

    /// Each ancestor is decided on its own, so a rule naming one unwraps that
    /// box while the frame between it and the shown node stands.
    #[test]
    fn a_rule_hiding_a_kind_unwraps_it_even_around_a_shown_node() {
        let fixture = Nested::new();
        let mut filter = fixture.showing_one(COMPILE);
        filter.rules.push(Rule {
            predicate: Predicate {
                kinds: Values::new([Kind::Package]),
                ..Predicate::default()
            },
            visibility: Visibility::Hidden,
        });

        let verdicts = fixture.evaluate(&filter);

        assert_eq!(verdicts.get(fixture.id(PACKAGE)), Verdict::Hidden);
        assert_eq!(verdicts.get(fixture.id(TARGET)), Verdict::Context);
        assert_eq!(verdicts.get(fixture.id(COMPILE)), Verdict::Shown);
    }

    /// A box stands for nothing beyond the nodes it holds, so a filter taking
    /// every one of them out leaves no box around the space they were in,
    /// however far up the containment the emptying reaches.
    #[test]
    fn a_container_the_filter_empties_is_not_drawn() {
        let fixture = Nested::new();
        let filter = Filter {
            rules: vec![Rule {
                predicate: Predicate {
                    kinds: Values::new([Kind::Target, Kind::Action]),
                    ..Predicate::default()
                },
                visibility: Visibility::Hidden,
            }],
            ..Filter::default()
        };

        let verdicts = fixture.evaluate(&filter);

        for package in [PACKAGE, OTHER_PACKAGE] {
            assert_eq!(verdicts.get(fixture.id(package)), Verdict::Hidden);
        }
    }

    /// A target stands for a target of the build whether or not the filter
    /// keeps what it was built from, so emptying one leaves it drawn.
    #[test]
    fn a_target_the_filter_empties_is_still_drawn() {
        let fixture = Nested::new();
        let filter = Filter {
            rules: vec![Rule {
                predicate: Predicate {
                    kinds: Values::new([Kind::Action]),
                    ..Predicate::default()
                },
                visibility: Visibility::Hidden,
            }],
            ..Filter::default()
        };

        let verdicts = fixture.evaluate(&filter);

        assert_eq!(verdicts.get(fixture.id(TARGET)), Verdict::Shown);
        assert_eq!(verdicts.get(fixture.id(PACKAGE)), Verdict::Shown);
    }

    /// Only a node the drawing holds calls for a frame, so one a rule hides
    /// leaves its ancestors as hidden as the base left them — while a node the
    /// same filter shows keeps its own.
    #[test]
    fn a_node_a_rule_hides_keeps_no_ancestor() {
        let fixture = Nested::new();
        let filter = Filter {
            base: Base::HideAll,
            rules: vec![Rule {
                predicate: Predicate {
                    kinds: Values::new([Kind::Action]),
                    ..Predicate::default()
                },
                visibility: Visibility::Shown,
            }],
            pins: [(fixture.id(OTHER_ACTION), Visibility::Hidden)]
                .into_iter()
                .collect(),
        };

        let verdicts = fixture.evaluate(&filter);

        assert_eq!(verdicts.get(fixture.id(OTHER_ACTION)), Verdict::Hidden);
        assert_eq!(verdicts.get(fixture.id(OTHER_TARGET)), Verdict::Hidden);
        assert_eq!(verdicts.get(fixture.id(OTHER_PACKAGE)), Verdict::Hidden);
        assert_eq!(verdicts.get(fixture.id(TARGET)), Verdict::Context);
        assert_eq!(verdicts.get(fixture.id(PACKAGE)), Verdict::Context);
    }
}
