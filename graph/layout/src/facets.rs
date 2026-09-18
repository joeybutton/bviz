//! The per-node metadata a graph filter selects on, held as interned handles.
//!
//! Facet values — a canonical repository name, a configuration key, a rule
//! class — are strings shared by many nodes, so they are interned once into a
//! [`FacetStrings`] table and carried per node as a four-byte handle. The table
//! is the filter's own; node ids keep theirs, whose indices double as positions
//! in the node arrays.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;

use bazel_query_proto::analysis::Configuration;

use crate::capability::{Axis, Capabilities, CapabilitiesBuilder};
use crate::intern::Id;
use crate::label;
use crate::repo::{self, RepoCategory, RepoOrigin};

/// A string interned in a [`FacetStrings`] table. The index is stored biased
/// off one, so the handle is never zero and `Option<Sym>` is as wide as `Sym`
/// without spending an index on a sentinel.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Sym(NonZeroU32);

impl Sym {
    fn at(index: usize) -> Self {
        let sym = u32::try_from(index)
            .ok()
            .and_then(|index| NonZeroU32::MIN.checked_add(index));
        Sym(sym.expect("more than u32::MAX facet strings"))
    }

    fn index(self) -> usize {
        self.0.get() as usize - 1
    }
}

/// Declares a facet handle: a [`Sym`] tagged with the facet it names, so one
/// facet's value cannot be passed where another's is expected. A handle exists
/// only for a string the table has already interned, since a `Sym` is the sole
/// way to build one.
macro_rules! facet_handle {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
        pub struct $name(Sym);

        impl $name {
            pub fn of(value: Sym) -> Self {
                $name(value)
            }

            pub fn sym(self) -> Sym {
                self.0
            }
        }
    };
}

facet_handle! {
    /// The canonical name of the repository a node's label names.
    RepoRef
}

facet_handle! {
    /// The key of the configuration a node is built in.
    ConfigRef
}

facet_handle! {
    /// The Bazel rule class a target was declared by.
    RuleClassRef
}

/// A yes/no facet that an input may leave unsaid. A filter must keep
/// `Unknown` distinct from `No`: the node is not known to lack the property,
/// only not known to have it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Tri {
    #[default]
    Unknown,
    No,
    Yes,
}

/// The strings every facet handle resolves through.
#[derive(Default, Clone)]
pub struct FacetStrings {
    strings: Vec<String>,
    lookup: HashMap<String, Sym>,
}

impl FacetStrings {
    pub fn intern(&mut self, value: &str) -> Sym {
        if let Some(&sym) = self.lookup.get(value) {
            return sym;
        }
        let sym = Sym::at(self.strings.len());
        self.strings.push(value.to_string());
        self.lookup.insert(value.to_string(), sym);
        sym
    }

    pub fn get(&self, value: &str) -> Option<Sym> {
        self.lookup.get(value).copied()
    }

    pub fn resolve(&self, sym: Sym) -> &str {
        &self.strings[sym.index()]
    }

    pub fn len(&self) -> usize {
        self.strings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }
}

/// A node id held in a facet array, stored biased off one so that an entry
/// naming no node is as wide as one that does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct NodeRef(NonZeroU32);

impl NodeRef {
    fn of(id: Id) -> Self {
        NodeRef(
            NonZeroU32::MIN
                .checked_add(id.0)
                .expect("more than u32::MAX node ids"),
        )
    }

    fn id(self) -> Id {
        Id(self.0.get() - 1)
    }
}

/// [`RepoOrigin`] with the parts it borrows from a repository name interned, so
/// that a table of repositories carries no lifetime.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin {
    Main,
    Module { version: Option<Sym> },
    ModuleExtension { isolated: bool },
    Builtin,
    WorkspaceOrUnknown,
}

impl Origin {
    pub fn category(self) -> RepoCategory {
        match self {
            Origin::Main => RepoCategory::Main,
            Origin::Module { .. } => RepoCategory::Module,
            Origin::ModuleExtension { .. } => RepoCategory::ModuleExtension,
            Origin::Builtin => RepoCategory::Builtin,
            Origin::WorkspaceOrUnknown => RepoCategory::WorkspaceOrUnknown,
        }
    }
}

/// What is known about one repository beyond the canonical name its
/// [`RepoRef`] resolves to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RepoEntry {
    /// The apparent name a label reached the repository by, where an input
    /// wrote one; a canonical label states no apparent name.
    pub apparent: Option<Sym>,
    pub origin: Origin,
    /// The Bzlmod module the repository belongs to.
    pub module: Option<Sym>,
}

/// What is known about one configuration beyond the checksum its [`ConfigRef`]
/// resolves to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ConfigEntry {
    /// The short name Bazel gives the configuration, which names its output
    /// directory.
    pub mnemonic: Option<Sym>,
    /// Whether the configuration builds a tool the build runs rather than an
    /// artifact it produces.
    pub is_tool: Tri,
}

/// The role a rule class plays, so that a filter can offer the tests, the
/// aliases, or the build's own plumbing without naming each rule class that
/// qualifies.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum RuleCategory {
    Test,
    Alias,
    Filegroup,
    /// A rule that states part of how the build is configured rather than
    /// something the build produces.
    Plumbing,
    Other,
}

/// The rule classes [`RuleCategory::Plumbing`] covers.
const PLUMBING_RULES: [&str; 6] = [
    "config_setting",
    "constraint_setting",
    "constraint_value",
    "platform",
    "toolchain",
    "toolchain_type",
];

/// The suffix every Bazel test rule's class name carries, `test_suite` apart.
const TEST_SUFFIX: &str = "_test";

impl RuleCategory {
    /// The category the rule class `name` falls in. A rule class bviz knows no
    /// role for is [`RuleCategory::Other`], so a Starlark rule and an unfamiliar
    /// native one land there rather than being mistaken for a category.
    pub fn of(name: &str) -> Self {
        match name {
            "alias" => Self::Alias,
            "filegroup" => Self::Filegroup,
            "test_suite" => Self::Test,
            _ if name.ends_with(TEST_SUFFIX) => Self::Test,
            _ if PLUMBING_RULES.contains(&name) => Self::Plumbing,
            _ => Self::Other,
        }
    }
}

/// A facet whose value is a set the nodes carrying it may state several of,
/// held as compressed sparse rows: the ids that carry a value in ascending
/// order, and every node's values end to end in one array, which `starts` cuts
/// into one run per id and so runs one entry longer than `ids`.
#[derive(Clone)]
pub struct SparseFacet {
    ids: Vec<Id>,
    starts: Vec<u32>,
    values: Vec<Sym>,
}

impl Default for SparseFacet {
    fn default() -> Self {
        SparseFacetBuilder::default().seal()
    }
}

impl SparseFacet {
    /// The values `id` carries, empty where it carries none.
    pub fn values(&self, id: Id) -> &[Sym] {
        match self.ids.binary_search(&id) {
            Ok(row) => self.row(row),
            Err(_) => &[],
        }
    }

    pub fn contains(&self, id: Id, value: Sym) -> bool {
        self.values(id).contains(&value)
    }

    /// Every node carrying a value, in ascending id order.
    pub fn rows(&self) -> impl Iterator<Item = (Id, &[Sym])> {
        self.ids
            .iter()
            .enumerate()
            .map(|(row, &id)| (id, self.row(row)))
    }

    /// Moves every value to the node id `remap` gives its node, under the same
    /// terms as [`Facets::renumber`].
    pub fn renumber(&mut self, remap: &[Id]) {
        let mut renumbered = SparseFacetBuilder::with_capacity(self.values.len());
        for (id, values) in self.rows() {
            for &value in values {
                renumbered.push(remap[id.0 as usize], value);
            }
        }
        *self = renumbered.seal();
    }

    fn row(&self, row: usize) -> &[Sym] {
        &self.values[self.starts[row] as usize..self.starts[row + 1] as usize]
    }
}

/// Collects a [`SparseFacet`]'s values as the inputs state them, in any order.
#[derive(Default)]
pub struct SparseFacetBuilder {
    pairs: Vec<(Id, Sym)>,
}

impl SparseFacetBuilder {
    pub fn with_capacity(values: usize) -> Self {
        Self {
            pairs: Vec::with_capacity(values),
        }
    }

    pub fn push(&mut self, id: Id, value: Sym) {
        self.pairs.push((id, value));
    }

    /// The facet the collected pairs make, sorted into the order it is read
    /// in. A node carries each of its values once, however many times it was
    /// given one.
    pub fn seal(mut self) -> SparseFacet {
        self.pairs.sort_unstable();
        self.pairs.dedup();
        let carrying = |a: &(Id, Sym), b: &(Id, Sym)| a.0 == b.0;
        let rows = self.pairs.chunk_by(carrying).count();
        let mut ids = Vec::with_capacity(rows);
        let mut starts = Vec::with_capacity(rows + 1);
        let mut values = Vec::with_capacity(self.pairs.len());
        starts.push(0);
        for row in self.pairs.chunk_by(carrying) {
            ids.push(row[0].0);
            values.extend(row.iter().map(|&(_, value)| value));
            starts.push(u32::try_from(values.len()).expect("more than u32::MAX facet values"));
        }
        SparseFacet {
            ids,
            starts,
            values,
        }
    }
}

/// The facets of every node of one graph, each held in its own array indexed by
/// node id.
///
/// An id no input described reads as absent, as does one past the end of an
/// array — the quotient pass mints group ids above every id the inputs named,
/// and those name nothing a filter selects on.
///
/// The package and label facets hold the id of the node whose name already
/// spells them out, the package box and the target, so that neither costs a
/// second copy of the label text the graph's `Names` holds.
///
/// A facet is filled only where the node carries none, so the first input to
/// state one wins and a later input leaves it as it stands. Several inputs
/// describing one node then settle its facets the way the graph settles the node
/// itself, by first arrival, with no reconciliation pass over the merged result.
#[derive(Default, Clone)]
pub struct Facets {
    strings: FacetStrings,
    repo: Vec<Option<RepoRef>>,
    rule_class: Vec<Option<RuleClassRef>>,
    config: Vec<Option<ConfigRef>>,
    mnemonic: Vec<Option<Sym>>,
    testonly: Vec<Tri>,
    package: Vec<Option<NodeRef>>,
    label: Vec<Option<NodeRef>>,
    tags: SparseFacet,
    aspects: SparseFacet,
    repos: HashMap<RepoRef, RepoEntry>,
    configs: HashMap<ConfigRef, ConfigEntry>,
    rule_classes: HashMap<RuleClassRef, RuleCategory>,
}

/// Declares one facet array: the accessor and setter a node id reaches its
/// value by.
macro_rules! dense_facet {
    ($(#[$doc:meta])* $name:ident, $set:ident: $value:ty) => {
        $(#[$doc])*
        pub fn $name(&self, id: Id) -> $value {
            at(&self.$name, id)
        }

        pub fn $set(&mut self, id: Id, value: $value) {
            put(&mut self.$name, id, value);
        }
    };
}

impl Facets {
    dense_facet! {
        /// The repository the node's label names.
        repo, set_repo: Option<RepoRef>
    }

    dense_facet! {
        /// The Bazel rule class the node's target was declared by.
        rule_class, set_rule_class: Option<RuleClassRef>
    }

    dense_facet! {
        /// The configuration the node is built in.
        config, set_config: Option<ConfigRef>
    }

    dense_facet! {
        /// The mnemonic of the action the node runs.
        mnemonic, set_mnemonic: Option<Sym>
    }

    dense_facet! {
        /// Whether the node's target is marked `testonly`.
        testonly, set_testonly: Tri
    }

    /// The package box holding the node.
    pub fn package(&self, id: Id) -> Option<Id> {
        at(&self.package, id).map(NodeRef::id)
    }

    pub fn set_package(&mut self, id: Id, package: Id) {
        put(&mut self.package, id, Some(NodeRef::of(package)));
    }

    /// The target the node belongs to, which for a target is itself.
    pub fn label(&self, id: Id) -> Option<Id> {
        at(&self.label, id).map(NodeRef::id)
    }

    pub fn set_label(&mut self, id: Id, label: Id) {
        put(&mut self.label, id, Some(NodeRef::of(label)));
    }

    /// Copies into `child` the facets of the `owner` it is part of: what the
    /// owner's label and declaration say, and the configuration it is built in.
    /// A facet `child` already states is the one it keeps, so a node that names
    /// its own target or configuration is not overwritten by its owner's.
    pub fn inherit(&mut self, child: Id, owner: Id) {
        let (repo, rule_class, config, package, label) = (
            self.repo(owner),
            self.rule_class(owner),
            self.config(owner),
            self.package(owner),
            self.label(owner),
        );
        self.set_repo(child, repo);
        self.set_rule_class(child, rule_class);
        self.set_config(child, config);
        if let Some(package) = package {
            self.set_package(child, package);
        }
        if let Some(label) = label {
            self.set_label(child, label);
        }
    }

    /// The tags the node's target sets.
    pub fn tags(&self) -> &SparseFacet {
        &self.tags
    }

    pub fn set_tags(&mut self, tags: SparseFacet) {
        self.tags = tags;
    }

    /// The aspects that took part in producing the node.
    pub fn aspects(&self) -> &SparseFacet {
        &self.aspects
    }

    pub fn set_aspects(&mut self, aspects: SparseFacet) {
        self.aspects = aspects;
    }

    /// The strings every handle these facets carry resolves through.
    pub fn strings(&self) -> &FacetStrings {
        &self.strings
    }

    /// The handle for the repository whose canonical name is `canonical`,
    /// deriving its origin from that name. `apparent` is recorded by the call
    /// that first names the repository, since every later call describes the
    /// repository the first one already did.
    pub fn intern_repo(&mut self, canonical: &str, apparent: Option<&str>) -> RepoRef {
        let handle = RepoRef::of(self.strings.intern(canonical));
        if !self.repos.contains_key(&handle) {
            let entry = RepoEntry {
                apparent: apparent.map(|name| self.strings.intern(name)),
                origin: self.origin(canonical),
                module: repo::module_name(canonical).map(|name| self.strings.intern(name)),
            };
            self.repos.insert(handle, entry);
        }
        handle
    }

    /// The handle for the configuration whose checksum is `checksum`. The rest
    /// is recorded by the call that first names the configuration, as
    /// [`intern_repo`](Self::intern_repo) records a repository's.
    pub fn intern_config(
        &mut self,
        checksum: &str,
        mnemonic: Option<&str>,
        is_tool: Tri,
    ) -> ConfigRef {
        let handle = ConfigRef::of(self.strings.intern(checksum));
        if !self.configs.contains_key(&handle) {
            let entry = ConfigEntry {
                mnemonic: mnemonic.map(|name| self.strings.intern(name)),
                is_tool,
            };
            self.configs.insert(handle, entry);
        }
        handle
    }

    /// The handle for the configuration `config` describes. `None` for one
    /// stating no checksum, which leaves it with no name a filter can hold on to.
    pub fn intern_configuration(&mut self, config: &Configuration) -> Option<ConfigRef> {
        if config.checksum.is_empty() {
            return None;
        }
        let mnemonic = (!config.mnemonic.is_empty()).then_some(config.mnemonic.as_str());
        let is_tool = if config.is_tool { Tri::Yes } else { Tri::No };
        Some(self.intern_config(&config.checksum, mnemonic, is_tool))
    }

    /// The handle for the mnemonic `name` of an action.
    pub fn intern_mnemonic(&mut self, name: &str) -> Sym {
        self.strings.intern(name)
    }

    /// The handle for the rule class `name`, categorizing it by that name.
    pub fn intern_rule_class(&mut self, name: &str) -> RuleClassRef {
        let handle = RuleClassRef::of(self.strings.intern(name));
        self.rule_classes
            .entry(handle)
            .or_insert_with(|| RuleCategory::of(name));
        handle
    }

    pub fn repo_entry(&self, repo: RepoRef) -> Option<&RepoEntry> {
        self.repos.get(&repo)
    }

    pub fn config_entry(&self, config: ConfigRef) -> Option<&ConfigEntry> {
        self.configs.get(&config)
    }

    pub fn rule_category(&self, rule_class: RuleClassRef) -> Option<RuleCategory> {
        self.rule_classes.get(&rule_class).copied()
    }

    /// Moves every facet to the node id `remap` gives its node, which must map
    /// each id of `0..remap.len()` to a distinct id of that same range.
    pub fn renumber(&mut self, remap: &[Id]) {
        self.repo = permuted(&self.repo, remap);
        self.rule_class = permuted(&self.rule_class, remap);
        self.config = permuted(&self.config, remap);
        self.mnemonic = permuted(&self.mnemonic, remap);
        self.testonly = permuted(&self.testonly, remap);
        self.package = permuted_nodes(&self.package, remap);
        self.label = permuted_nodes(&self.label, remap);
        self.tags.renumber(remap);
        self.aspects.renumber(remap);
    }

    fn origin(&mut self, canonical: &str) -> Origin {
        match repo::classify(canonical) {
            RepoOrigin::Main => Origin::Main,
            RepoOrigin::Module { version } => Origin::Module {
                version: version.map(|version| self.strings.intern(version)),
            },
            RepoOrigin::ModuleExtension { isolated } => Origin::ModuleExtension { isolated },
            RepoOrigin::Builtin => Origin::Builtin,
            RepoOrigin::WorkspaceOrUnknown => Origin::WorkspaceOrUnknown,
        }
    }
}

/// Collects a graph's facets while its inputs are read. A dense facet fills the
/// [`Facets`] as it is stated; a set-valued one gathers its pairs here and is
/// sealed into a [`SparseFacet`] once every input has been read.
#[derive(Default)]
pub struct FacetsBuilder {
    facets: Facets,
    tags: SparseFill,
    aspects: SparseFill,
    capabilities: CapabilitiesBuilder,
}

impl FacetsBuilder {
    /// The facets filled so far, which an input reads as well as writes: a node
    /// standing for another inherits the facets already recorded for it.
    pub fn facets(&mut self) -> &mut Facets {
        &mut self.facets
    }

    /// Records what the label of the node `node` names outright: itself as a
    /// target, the `package` box it sits in, and its repository. `canonical` is
    /// the label in the form node ids are keyed by and `written` the form the
    /// input printed, whose repository part is the apparent name that
    /// repository was reached by.
    pub fn describe_label(
        &mut self,
        node: Id,
        package: Option<Id>,
        canonical: &str,
        written: &str,
    ) {
        self.facets.set_label(node, node);
        self.observe(Axis::Label, true);
        if let Some(package) = package {
            self.facets.set_package(node, package);
        }
        self.observe(Axis::Package, package.is_some());
        if let Some(canonical_repo) = label::label_repo(canonical) {
            let apparent = label::label_repo(written).filter(|&name| name != canonical_repo);
            let repo = self.facets.intern_repo(canonical_repo, apparent);
            self.facets.set_repo(node, Some(repo));
        }
        self.observe(Axis::Repo, label::is_canonical(canonical));
    }

    pub fn set_tags<'a>(&mut self, id: Id, tags: impl IntoIterator<Item = &'a str>) {
        self.tags.fill(&mut self.facets.strings, id, tags);
    }

    pub fn set_aspects<'a>(&mut self, id: Id, aspects: impl IntoIterator<Item = &'a str>) {
        self.aspects.fill(&mut self.facets.strings, id, aspects);
    }

    pub fn observe(&mut self, axis: Axis, carried: bool) {
        self.capabilities.observe(axis, carried);
    }

    pub fn seal(mut self) -> (Facets, Capabilities) {
        self.facets.set_tags(self.tags.values.seal());
        self.facets.set_aspects(self.aspects.values.seal());
        (self.facets, self.capabilities.seal())
    }
}

/// Collects one sparse facet under the fill-if-empty rule the dense facets
/// follow: the values a node is first given are the ones it keeps.
#[derive(Default)]
struct SparseFill {
    values: SparseFacetBuilder,
    /// The nodes a value has been recorded for.
    filled: HashSet<Id>,
}

impl SparseFill {
    fn fill<'a>(
        &mut self,
        strings: &mut FacetStrings,
        id: Id,
        values: impl IntoIterator<Item = &'a str>,
    ) {
        if !self.filled.insert(id) {
            return;
        }
        for value in values {
            let value = strings.intern(value);
            self.values.push(id, value);
        }
    }
}

fn at<T: Copy + Default>(column: &[T], id: Id) -> T {
    column.get(id.0 as usize).copied().unwrap_or_default()
}

fn put<T: Copy + Default + PartialEq>(column: &mut Vec<T>, id: Id, value: T) {
    let index = id.0 as usize;
    if value == T::default() || at(column, id) != T::default() {
        return;
    }
    if column.len() <= index {
        column.resize(index + 1, T::default());
    }
    column[index] = value;
}

fn permuted<T: Copy + Default>(column: &[T], remap: &[Id]) -> Vec<T> {
    // A facet no input stated holds nothing to move, and every id reads absent
    // from an empty array as it would from a permuted one.
    if column.is_empty() {
        return Vec::new();
    }
    let mut permuted = vec![T::default(); remap.len()];
    for (old, &new) in remap.iter().enumerate() {
        permuted[new.0 as usize] = at(column, Id(old as u32));
    }
    permuted
}

/// [`permuted`] for an array of node ids, whose values name nodes and so are
/// renumbered along with the positions they sit at.
fn permuted_nodes(column: &[Option<NodeRef>], remap: &[Id]) -> Vec<Option<NodeRef>> {
    let mut permuted = permuted(column, remap);
    for entry in &mut permuted {
        *entry = entry.map(|node| NodeRef::of(remap[node.id().0 as usize]));
    }
    permuted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_the_same_string_twice_yields_one_handle() {
        let mut facets = FacetStrings::default();

        let first = facets.intern("rules_rust+");
        let second = facets.intern("rules_rust+");
        let other = facets.intern("rules_cc+");

        assert_eq!(first, second);
        assert_ne!(first, other);
        assert_eq!(facets.len(), 2);
    }

    #[test]
    fn a_handle_resolves_to_the_string_it_was_interned_from() {
        let mut facets = FacetStrings::default();

        let first = facets.intern("k8-fastbuild");
        let last = facets.intern("k8-opt-exec");

        assert_eq!(facets.resolve(first), "k8-fastbuild");
        assert_eq!(facets.resolve(last), "k8-opt-exec");
        assert_eq!(facets.get("k8-opt-exec"), Some(last));
        assert_eq!(facets.get("k8-dbg"), None);
    }

    #[test]
    fn an_empty_table_holds_no_strings() {
        let mut facets = FacetStrings::default();
        assert!(facets.is_empty());

        facets.intern("");

        assert!(!facets.is_empty());
        assert_eq!(facets.resolve(facets.get("").unwrap()), "");
    }

    /// The non-zero inner value is what lets an absent facet cost nothing: a
    /// missing facet is `None`, not a reserved index.
    #[test]
    fn an_optional_handle_is_no_wider_than_the_handle() {
        assert_eq!(size_of::<Option<Sym>>(), 4);
        assert_eq!(size_of::<Option<RepoRef>>(), 4);
        assert_eq!(size_of::<Option<ConfigRef>>(), 4);
        assert_eq!(size_of::<Option<RuleClassRef>>(), 4);
    }

    #[test]
    fn a_facet_handle_carries_the_string_it_tags() {
        let mut facets = FacetStrings::default();

        let repo = RepoRef::of(facets.intern("rules_java+"));

        assert_eq!(facets.resolve(repo.sym()), "rules_java+");
    }

    /// What a facet array costs per node, which is what keeps the facets out of
    /// `Node` and beside the graph instead.
    #[test]
    fn a_facet_array_entry_is_four_bytes_at_most() {
        assert_eq!(size_of::<Option<NodeRef>>(), 4);
        assert_eq!(size_of::<Tri>(), 1);
    }

    #[test]
    fn an_id_the_facets_never_described_reads_absent() {
        let mut facets = Facets::default();
        facets.set_testonly(Id(1), Tri::Yes);

        assert_eq!(facets.repo(Id(0)), None);
        assert_eq!(facets.testonly(Id(0)), Tri::Unknown);
        assert_eq!(facets.package(Id(9_999)), None);
        assert_eq!(facets.testonly(Id(9_999)), Tri::Unknown);
    }

    #[test]
    fn the_first_value_a_node_is_given_is_the_one_it_keeps() {
        let mut facets = Facets::default();
        let first = facets.intern_repo("rules_foo+", None);
        let second = facets.intern_repo("rules_bar+", None);

        facets.set_repo(Id(0), Some(first));
        facets.set_repo(Id(0), Some(second));
        facets.set_testonly(Id(0), Tri::Yes);
        facets.set_testonly(Id(0), Tri::No);
        facets.set_package(Id(0), Id(3));
        facets.set_package(Id(0), Id(4));

        assert_eq!(facets.repo(Id(0)), Some(first));
        assert_eq!(facets.testonly(Id(0)), Tri::Yes);
        assert_eq!(facets.package(Id(0)), Some(Id(3)));
    }

    #[test]
    fn a_facet_given_no_value_leaves_the_node_undescribed() {
        let mut facets = Facets::default();

        facets.set_repo(Id(7), None);
        facets.set_testonly(Id(7), Tri::Unknown);

        assert!(facets.repo.is_empty());
        assert!(facets.testonly.is_empty());
    }

    #[test]
    fn a_nodes_tags_are_the_ones_it_was_first_given() {
        let mut builder = FacetsBuilder::default();

        builder.set_tags(Id(1), ["manual", "flaky"]);
        builder.set_tags(Id(1), ["local"]);

        let (facets, _) = builder.seal();
        let mut tags: Vec<&str> = facets
            .tags()
            .values(Id(1))
            .iter()
            .map(|&tag| facets.strings().resolve(tag))
            .collect();
        tags.sort_unstable();
        assert_eq!(tags, ["flaky", "manual"]);
    }

    #[test]
    fn interning_one_value_twice_yields_one_entry() {
        let mut facets = Facets::default();

        let repo = facets.intern_repo("rules_foo+", Some("foo"));
        let config = facets.intern_config("ck1", Some("k8-fastbuild"), Tri::No);
        let rule_class = facets.intern_rule_class("cc_library");

        assert_eq!(facets.intern_repo("rules_foo+", None), repo);
        assert_eq!(facets.intern_config("ck1", None, Tri::Unknown), config);
        assert_eq!(facets.intern_rule_class("cc_library"), rule_class);
        // The description the repeat call carried does not displace the first.
        let apparent = facets.repo_entry(repo).unwrap().apparent;
        assert_eq!(
            apparent.map(|name| facets.strings().resolve(name)),
            Some("foo")
        );
        assert_eq!(facets.config_entry(config).unwrap().is_tool, Tri::No);
        assert_eq!(facets.repos.len(), 1);
        assert_eq!(facets.configs.len(), 1);
        assert_eq!(facets.rule_classes.len(), 1);
    }

    #[test]
    fn a_repository_entry_states_the_origin_its_canonical_name_encodes() {
        let mut facets = Facets::default();

        let repo = facets.intern_repo("rules_foo+1.2.3", None);

        let entry = *facets.repo_entry(repo).unwrap();
        let resolve = |sym: Option<Sym>| sym.map(|sym| facets.strings().resolve(sym));
        assert_eq!(resolve(entry.module), Some("rules_foo"));
        let Origin::Module { version } = entry.origin else {
            panic!(
                "a module repository's origin is a module, not {:?}",
                entry.origin
            )
        };
        assert_eq!(resolve(version), Some("1.2.3"));
    }

    #[test]
    fn a_rule_class_name_states_the_role_its_rule_plays() {
        let cases = [
            ("cc_test", RuleCategory::Test),
            ("test_suite", RuleCategory::Test),
            ("alias", RuleCategory::Alias),
            ("filegroup", RuleCategory::Filegroup),
            ("config_setting", RuleCategory::Plumbing),
            ("toolchain_type", RuleCategory::Plumbing),
            ("cc_library", RuleCategory::Other),
        ];

        let mut facets = Facets::default();
        for (name, expected) in cases {
            let rule_class = facets.intern_rule_class(name);
            assert_eq!(facets.rule_category(rule_class), Some(expected), "{name}");
        }
    }

    #[test]
    fn inheriting_fills_what_a_node_leaves_unsaid_and_no_more() {
        let mut facets = Facets::default();
        let rule_class = facets.intern_rule_class("cc_library");
        let (owners, own) = (
            facets.intern_config("owners", None, Tri::Unknown),
            facets.intern_config("own", None, Tri::Unknown),
        );
        facets.set_rule_class(Id(0), Some(rule_class));
        facets.set_config(Id(0), Some(owners));
        facets.set_label(Id(0), Id(0));
        facets.set_config(Id(1), Some(own));
        facets.set_label(Id(1), Id(1));

        facets.inherit(Id(1), Id(0));

        assert_eq!(facets.rule_class(Id(1)), Some(rule_class));
        assert_eq!(facets.config(Id(1)), Some(own));
        assert_eq!(facets.label(Id(1)), Some(Id(1)));
    }

    /// Ids `0`, `1`, `2` renumbered to `2`, `0`, `1`.
    fn rotated() -> Vec<Id> {
        vec![Id(2), Id(0), Id(1)]
    }

    #[test]
    fn renumbering_moves_each_nodes_facets_to_its_new_id() {
        let mut facets = Facets::default();
        let repo = facets.intern_repo("rules_foo+", None);
        facets.set_repo(Id(0), Some(repo));
        facets.set_testonly(Id(1), Tri::Yes);
        facets.set_testonly(Id(2), Tri::No);

        facets.renumber(&rotated());

        assert_eq!(facets.repo(Id(2)), Some(repo));
        assert_eq!(facets.repo(Id(0)), None);
        assert_eq!(facets.testonly(Id(0)), Tri::Yes);
        assert_eq!(facets.testonly(Id(1)), Tri::No);
        assert_eq!(facets.testonly(Id(2)), Tri::Unknown);
    }

    #[test]
    fn renumbering_rewrites_the_node_ids_a_facet_stores() {
        let mut facets = Facets::default();
        facets.set_package(Id(1), Id(0));
        facets.set_label(Id(1), Id(1));

        facets.renumber(&rotated());

        assert_eq!(facets.package(Id(0)), Some(Id(2)));
        assert_eq!(facets.label(Id(0)), Some(Id(0)));
        assert_eq!(facets.package(Id(2)), None);
    }

    /// A sparse facet over ids `2` and `0`, given in neither id nor value
    /// order, alongside the strings its values resolve to.
    fn sparse() -> (SparseFacet, FacetStrings) {
        let mut strings = FacetStrings::default();
        let (manual, flaky, local) = (
            strings.intern("manual"),
            strings.intern("flaky"),
            strings.intern("local"),
        );
        let mut builder = SparseFacetBuilder::with_capacity(3);
        builder.push(Id(2), local);
        builder.push(Id(0), manual);
        builder.push(Id(2), flaky);
        builder.push(Id(0), manual);
        (builder.seal(), strings)
    }

    #[test]
    fn sealing_puts_the_nodes_carrying_a_value_in_ascending_order() {
        let (facet, strings) = sparse();

        let read: Vec<(Id, Vec<&str>)> = facet
            .rows()
            .map(|(id, values)| {
                (
                    id,
                    values.iter().map(|&value| strings.resolve(value)).collect(),
                )
            })
            .collect();
        assert_eq!(
            read,
            [(Id(0), vec!["manual"]), (Id(2), vec!["flaky", "local"])]
        );
    }

    #[test]
    fn a_node_carries_the_values_it_was_given_and_no_others() {
        let (facet, mut strings) = sparse();
        let (manual, local) = (strings.intern("manual"), strings.intern("local"));

        assert!(facet.contains(Id(0), manual));
        assert!(!facet.contains(Id(0), local));
        assert!(!facet.contains(Id(1), manual));
        assert!(facet.values(Id(1)).is_empty());
        assert!(facet.values(Id(9_999)).is_empty());
    }

    #[test]
    fn renumbering_re_sorts_the_nodes_and_keeps_each_ones_values() {
        let (mut facet, mut strings) = sparse();
        let (manual, flaky, local) = (
            strings.intern("manual"),
            strings.intern("flaky"),
            strings.intern("local"),
        );

        facet.renumber(&rotated());

        let read: Vec<(Id, Vec<Sym>)> = facet
            .rows()
            .map(|(id, values)| (id, values.to_vec()))
            .collect();
        assert_eq!(read, [(Id(1), vec![flaky, local]), (Id(2), vec![manual])]);
    }

    #[test]
    fn renumbering_a_facet_no_input_stated_leaves_it_empty() {
        let mut facets = Facets::default();
        facets.set_testonly(Id(1), Tri::Yes);

        facets.renumber(&rotated());

        assert!(facets.repo.is_empty());
        assert_eq!(facets.testonly.len(), 3);
    }
}
