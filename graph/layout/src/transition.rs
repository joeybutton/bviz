//! The configuration transitions a graph's dependency edges cross.
//!
//! Only an edge joining two configured targets states both configurations, so
//! only such an edge — a cquery run with `--transitions=lite` or `full` writes
//! them — can be seen to cross one.

use std::cmp::Ordering;

use rustc_hash::FxHashMap;

use crate::facets::{ConfigEntry, ConfigRef, Facets, OptionSetting, Sym, Tri};
use crate::graph::{Edge, Graph, Kind, id_span};
use crate::intern::Id;
use crate::layout::Connector;

/// The end of the mnemonic Bazel gives its null configuration, which a target
/// that reads no configuration is built in (`OutputPathMnemonicComputer` in
/// Bazel's source). Entering it is no transition a reader needs to see.
const NO_CONFIG: &str = "-noconfig";

/// The end of the name of the fragment options Bazel trims on entering every
/// rule but a test or an alias (`TestTrimmingTransitionFactory` in Bazel's
/// source). They are no part of the output path, so dropping them never
/// explains a target built twice, and a difference is read with that drop
/// left out: it is no transition on its own.
const TEST_OPTIONS: &str = "TestConfiguration$TestOptions";

/// The end of the name of the fragment options holding the target platform.
const PLATFORM_OPTIONS: &str = "PlatformOptions";

const PLATFORMS: &str = "platforms";

/// The option Bazel rewrites alongside dropping the test options, which is
/// part of that drop under `--run_under`
/// (`RunUnder.trimForNonTestConfiguration` in Bazel's source).
const RUN_UNDER: &str = "run_under";

/// A change of configuration from a dependent to its dependency, named by what
/// the configurations tell of it: the proto names no transition, so it is read
/// off the two configurations' options.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Transition {
    /// The dependency is built in a configuration for a tool the build runs,
    /// and the dependent is not.
    Exec,
    /// The `--platforms` option changes. A legacy `--cpu` change alone is a
    /// flag.
    Platform,
    /// Some option other than `--platforms` changes.
    Flag,
    /// The dependent takes the dependency's target in more than one
    /// configuration, whether through one split transition or through several
    /// of its attributes.
    Split,
    /// The configurations differ in a way no input states, for one of them was
    /// described without its options.
    Changed,
}

impl Transition {
    /// Every transition, in the order a legend lists them, which is also the
    /// order in which one of several folded into a connector stands for them.
    pub const ALL: [Transition; 5] = [
        Transition::Exec,
        Transition::Platform,
        Transition::Flag,
        Transition::Split,
        Transition::Changed,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Transition::Exec => "exec transition",
            Transition::Platform => "platform transition",
            Transition::Flag => "flag transition",
            Transition::Split => "several configurations",
            Transition::Changed => "configuration change",
        }
    }

    /// Where this stands in [`Transition::ALL`], whose order the variants are
    /// declared in.
    fn rank(self) -> usize {
        self as usize
    }

    /// What an edge crosses from a dependent built in `dependent` to a
    /// dependency built in `dependency`, short of a split, which only the
    /// dependent's other edges tell. `None` where the two configurations are
    /// one, differ only by dropping the test options, or the dependency is
    /// built in the null configuration.
    fn between(facets: &Facets, dependency: ConfigRef, dependent: ConfigRef) -> Option<Self> {
        if dependency == dependent {
            return None;
        }
        let entry = facets.config_entry(dependency);
        let mnemonic = entry.and_then(|entry| entry.mnemonic);
        if mnemonic.is_some_and(|mnemonic| facets.strings().resolve(mnemonic).ends_with(NO_CONFIG))
        {
            return None;
        }
        let tool =
            |entry: Option<&ConfigEntry>| entry.is_some_and(|entry| entry.is_tool == Tri::Yes);
        if tool(entry) && !tool(facets.config_entry(dependent)) {
            return Some(Transition::Exec);
        }
        let Some(difference) = Difference::between(facets, dependency, dependent) else {
            return Some(Transition::Changed);
        };
        if difference.fragments.is_empty() && difference.changes.is_empty() {
            return None;
        }
        let resolve = |sym: Sym| facets.strings().resolve(sym);
        let platform = difference.changes.iter().any(|change| {
            resolve(change.fragment).ends_with(PLATFORM_OPTIONS)
                && resolve(change.name) == PLATFORMS
        });
        Some(match platform {
            true => Transition::Platform,
            false => Transition::Flag,
        })
    }
}

/// Which of two configurations carries something the other lacks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Dependency,
    Dependent,
}

/// One option two configurations set differently. A value is `None` where
/// that configuration sets the option to nothing, or does not set it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OptionChange {
    pub fragment: Sym,
    pub name: Sym,
    pub dependent: Option<Sym>,
    pub dependency: Option<Sym>,
}

/// How a dependency's configuration differs from its dependent's, beyond
/// dropping the test options.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Difference {
    /// The fragment options classes one of the two carries and the other does
    /// not, each with the side carrying it.
    pub fragments: Vec<(Sym, Side)>,
    /// The options set differently within the classes both carry.
    pub changes: Vec<OptionChange>,
}

impl Difference {
    /// How `dependency` differs from `dependent` beyond dropping the test
    /// options, `None` where either was described without its options.
    pub fn between(facets: &Facets, dependency: ConfigRef, dependent: ConfigRef) -> Option<Self> {
        let (dependency, dependent) = (
            facets.config_options(dependency)?,
            facets.config_options(dependent)?,
        );
        let mut fragments = one_sided(dependent.fragments(), dependency.fragments());
        let shared = |setting: &&OptionSetting| {
            !fragments
                .iter()
                .any(|&(fragment, _)| fragment == setting.fragment)
        };
        let mut changes = changed(
            dependent.settings().iter().filter(shared),
            dependency.settings().iter().filter(shared),
        );
        let resolve = |sym: Sym| facets.strings().resolve(sym);
        let trimmed = fragments.iter().position(|&(fragment, side)| {
            side == Side::Dependent && resolve(fragment).ends_with(TEST_OPTIONS)
        });
        if let Some(trimmed) = trimmed {
            fragments.remove(trimmed);
            changes.retain(|change| resolve(change.name) != RUN_UNDER);
        }
        Some(Difference { fragments, changes })
    }
}

/// The classes `dependent` and `dependency`, both sorted, do not share.
fn one_sided(dependent: &[Sym], dependency: &[Sym]) -> Vec<(Sym, Side)> {
    let (mut a, mut b) = (dependent.iter().peekable(), dependency.iter().peekable());
    let mut apart: Vec<(Sym, Side)> = Vec::new();
    loop {
        match (a.peek(), b.peek()) {
            (Some(&&x), Some(&&y)) => match x.cmp(&y) {
                Ordering::Less => {
                    apart.push((x, Side::Dependent));
                    a.next();
                }
                Ordering::Greater => {
                    apart.push((y, Side::Dependency));
                    b.next();
                }
                Ordering::Equal => {
                    a.next();
                    b.next();
                }
            },
            (Some(&&x), None) => {
                apart.push((x, Side::Dependent));
                a.next();
            }
            (None, Some(&&y)) => {
                apart.push((y, Side::Dependency));
                b.next();
            }
            (None, None) => return apart,
        }
    }
}

/// The options the two sorted setting lists set differently.
fn changed<'a>(
    dependent: impl Iterator<Item = &'a OptionSetting>,
    dependency: impl Iterator<Item = &'a OptionSetting>,
) -> Vec<OptionChange> {
    let key = |setting: &OptionSetting| (setting.fragment, setting.name);
    let (mut a, mut b) = (dependent.peekable(), dependency.peekable());
    let mut changes: Vec<OptionChange> = Vec::new();
    loop {
        let (setting, dependent, dependency) = match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return changes,
            (Some(x), Some(y)) if key(x) == key(y) => {
                a.next();
                b.next();
                if x.value == y.value {
                    continue;
                }
                (x, x.value, y.value)
            }
            (Some(x), Some(y)) if key(x) > key(y) => {
                b.next();
                (y, None, y.value)
            }
            (Some(x), _) => {
                a.next();
                (x, x.value, None)
            }
            (None, Some(y)) => {
                b.next();
                (y, None, y.value)
            }
        };
        changes.push(OptionChange {
            fragment: setting.fragment,
            name: setting.name,
            dependent,
            dependency,
        });
    }
}

/// The transitions one graph's edges cross, by the node pair each edge joins.
#[derive(Clone, Default)]
pub struct Transitions(FxHashMap<(Id, Id), Transition>);

impl Transitions {
    /// The transitions `graph`'s edges cross, read off the configurations
    /// `facets` records. Only an edge joining two configured targets states
    /// both configurations: an action or an artifact takes its configured
    /// target's, and an edge into one is a step within that target's build.
    pub fn of(graph: &Graph, facets: &Facets) -> Self {
        let span = id_span(graph);
        let mut configured: Vec<bool> = vec![false; span];
        let mut parent: Vec<Option<Id>> = vec![None; span];
        for node in &graph.nodes {
            configured[node.id.0 as usize] = node.kind == Kind::ConfiguredTarget;
            parent[node.id.0 as usize] = node.parent;
        }
        let configs = |edge: &Edge| {
            if !(configured[edge.from.0 as usize] && configured[edge.to.0 as usize]) {
                return None;
            }
            Some((facets.config(edge.from)?, facets.config(edge.to)?))
        };

        // A dependent reaching one target's configured targets in several
        // configurations splits over them, which marks each edge it takes.
        let mut reached: FxHashMap<(Id, Id), Reached> =
            FxHashMap::with_capacity_and_hasher(graph.edges.len(), Default::default());
        for edge in &graph.edges {
            if let (Some((dependency, _)), Some(target)) =
                (configs(edge), parent[edge.from.0 as usize])
            {
                reached
                    .entry((edge.to, target))
                    .and_modify(|held| held.add(dependency))
                    .or_insert(Reached::Once(dependency));
            }
        }

        let mut kinds: FxHashMap<(ConfigRef, ConfigRef), Option<Transition>> =
            FxHashMap::with_capacity_and_hasher(graph.edges.len(), Default::default());
        let crossed = graph.edges.iter().filter_map(|edge| {
            let (dependency, dependent) = configs(edge)?;
            let kind = *kinds
                .entry((dependency, dependent))
                .or_insert_with(|| Transition::between(facets, dependency, dependent));
            let split = parent[edge.from.0 as usize]
                .and_then(|target| reached.get(&(edge.to, target)))
                .is_some_and(|reached| matches!(reached, Reached::Several));
            let kind = match kind? {
                Transition::Platform | Transition::Flag | Transition::Changed if split => {
                    Transition::Split
                }
                kind => kind,
            };
            Some(((edge.from, edge.to), kind))
        });
        Transitions(crossed.collect())
    }

    /// The transition the edge from `from` to `to` crosses, `None` where it
    /// crosses none or the graph states no such edge.
    pub fn get(&self, from: Id, to: Id) -> Option<Transition> {
        self.0.get(&(from, to)).copied()
    }

    /// States on each of `connectors` the transition the edges drawn along it
    /// cross, the first in [`Transition::ALL`] where several are crossed.
    /// `drawn` names the pair of boxes an edge of the graph these were found in
    /// is drawn between, `None` for an edge the drawing folds away.
    pub fn mark(&self, connectors: &mut [Connector], drawn: impl Fn(Id, Id) -> Option<(Id, Id)>) {
        let mut crossed: FxHashMap<(Id, Id), Transition> =
            FxHashMap::with_capacity_and_hasher(self.0.len(), Default::default());
        for (&(from, to), &transition) in &self.0 {
            if let Some(pair) = drawn(from, to) {
                let held = crossed.entry(pair).or_insert(transition);
                if transition.rank() < held.rank() {
                    *held = transition;
                }
            }
        }
        for connector in connectors {
            connector.transition = crossed.get(&(connector.from, connector.to)).copied();
        }
    }
}

/// The configurations one dependent reaches one target in.
enum Reached {
    Once(ConfigRef),
    Several,
}

impl Reached {
    fn add(&mut self, config: ConfigRef) {
        if matches!(self, Reached::Once(held) if *held != config) {
            *self = Reached::Several;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{EdgeKind, Node};

    /// Configured targets `0` and `1` in the target configuration, `2` in the
    /// exec one and `3` in a third; target `4` and configured target `5` state
    /// no configuration.
    fn fixture() -> (Graph, Facets) {
        let mut graph = Graph::new();
        let mut facets = Facets::default();
        let target = facets.intern_config("ck1", Some("k8-fastbuild"), Tri::No);
        let exec = facets.intern_config("ck2", Some("k8-opt-exec"), Tri::Yes);
        let other = facets.intern_config("ck3", Some("k8-fastbuild-ST"), Tri::No);
        let configs = [Some(target), Some(target), Some(exec), Some(other)];
        for (id, config) in configs.into_iter().enumerate() {
            let id = Id(id as u32);
            graph.add_node(Node::new(id, "ct", Kind::ConfiguredTarget, None));
            facets.set_config(id, config);
        }
        graph.add_node(Node::new(Id(4), "t", Kind::Target, None));
        facets.set_config(Id(4), Some(exec));
        graph.add_node(Node::new(Id(5), "ct", Kind::ConfiguredTarget, None));
        for (from, to) in [(0, 1), (2, 0), (3, 0), (4, 0), (5, 0), (0, 2)] {
            graph.add_edge(Id(from), Id(to), EdgeKind::Cquery);
        }
        (graph, facets)
    }

    #[test]
    fn an_edge_within_one_configuration_crosses_nothing() {
        let (graph, facets) = fixture();
        assert_eq!(Transitions::of(&graph, &facets).get(Id(0), Id(1)), None);
    }

    /// The fragment options classes Bazel names the test options and the
    /// platform options by.
    const TEST: &str = "com.google.devtools.build.lib.analysis.test.TestConfiguration$TestOptions";
    const PLATFORM: &str = "com.google.devtools.build.lib.analysis.PlatformOptions";
    const CORE: &str = "com.google.devtools.build.lib.analysis.config.CoreOptions";

    /// The options a configuration sets, class by class.
    type Options = Vec<(&'static str, Vec<(&'static str, String)>)>;

    /// The configuration checksummed `checksum`, setting `options`, built for
    /// tools where `mnemonic` names an exec configuration as Bazel's do.
    fn configured(
        facets: &mut Facets,
        checksum: &str,
        mnemonic: &str,
        options: Options,
    ) -> ConfigRef {
        use bazel_query_proto::analysis::{Configuration, FragmentOptions, Option as SetOption};
        let fragment_options = options
            .into_iter()
            .map(|(name, settings)| FragmentOptions {
                name: name.to_string(),
                options: settings
                    .into_iter()
                    .map(|(name, value)| SetOption {
                        name: Some(name.to_string()),
                        value: Some(value),
                    })
                    .collect(),
            })
            .collect();
        let config = Configuration {
            checksum: checksum.to_string(),
            mnemonic: mnemonic.to_string(),
            is_tool: mnemonic.ends_with("-exec"),
            fragment_options,
            ..Configuration::default()
        };
        facets.intern_configuration(&config).expect("a checksum")
    }

    /// A target configuration for `platform` in `mode`, holding the test options
    /// unless `trimmed`, checksummed `checksum`.
    fn target(
        facets: &mut Facets,
        checksum: &str,
        platform: &str,
        mode: &str,
        trimmed: bool,
    ) -> ConfigRef {
        let mut options: Options = vec![
            (CORE, vec![("compilation_mode", mode.to_string())]),
            (PLATFORM, vec![("platforms", platform.to_string())]),
        ];
        if !trimmed {
            options.push((TEST, vec![("test_timeout", "60".to_string())]));
        }
        configured(facets, checksum, "k8-fastbuild", options)
    }

    fn kind(facets: &Facets, dependency: ConfigRef, dependent: ConfigRef) -> Option<Transition> {
        Transition::between(facets, dependency, dependent)
    }

    #[test]
    fn each_transition_ranks_where_it_is_listed() {
        for (position, transition) in Transition::ALL.into_iter().enumerate() {
            assert_eq!(transition.rank(), position, "{transition:?}");
        }
    }

    /// A tool built for another execution platform than the tool depending on
    /// it enters no exec configuration, for it was already in one.
    #[test]
    fn a_tool_depending_on_a_tool_crosses_what_their_options_say() {
        let mut facets = Facets::default();
        let exec = |platform: &str| -> Options {
            vec![(PLATFORM, vec![("platforms", platform.to_string())])]
        };
        let target = target(&mut facets, "app", "//:linux", "fastbuild", true);
        let tool = configured(&mut facets, "tool", "k8-opt-exec", exec("//:linux"));
        let remote = configured(&mut facets, "remote", "k8-opt-exec-2", exec("//:rbe"));
        assert_eq!(kind(&facets, tool, target), Some(Transition::Exec));
        assert_eq!(kind(&facets, remote, tool), Some(Transition::Platform));
    }

    #[test]
    fn dropping_the_test_options_alone_is_no_transition() {
        let mut facets = Facets::default();
        let test = target(&mut facets, "test", "//:linux", "fastbuild", false);
        let trimmed = target(&mut facets, "trimmed", "//:linux", "fastbuild", true);
        assert_eq!(kind(&facets, trimmed, test), None);
    }

    /// Under `--run_under`, Bazel rewrites that option as it drops the test
    /// options, which makes that rewrite part of the drop.
    #[test]
    fn trimming_rewrites_run_under_along_with_the_test_options() {
        let mut facets = Facets::default();
        let with_run_under = |run_under: &str, test: bool| -> Options {
            let mut options: Options = vec![(CORE, vec![("run_under", run_under.to_string())])];
            if test {
                options.push((TEST, vec![("test_timeout", "60".to_string())]));
            }
            options
        };
        let test = configured(
            &mut facets,
            "test",
            "k8-fastbuild",
            with_run_under("//:wrap", true),
        );
        let trimmed = configured(
            &mut facets,
            "trimmed",
            "k8-fastbuild",
            with_run_under("wrap", false),
        );
        let untrimmed = configured(
            &mut facets,
            "other",
            "k8-fastbuild",
            with_run_under("wrap", true),
        );
        assert_eq!(kind(&facets, trimmed, test), None);
        assert_eq!(kind(&facets, untrimmed, test), Some(Transition::Flag));
    }

    /// A transition that drops the test options on its way to another
    /// platform, or another flag, is named by that other change.
    #[test]
    fn trimming_with_another_change_is_named_by_that_change() {
        let mut facets = Facets::default();
        let test = target(&mut facets, "test", "//:linux", "fastbuild", false);
        let platform = target(&mut facets, "arm", "//:arm", "fastbuild", true);
        let flag = target(&mut facets, "opt", "//:linux", "opt", true);
        assert_eq!(kind(&facets, platform, test), Some(Transition::Platform));
        assert_eq!(kind(&facets, flag, test), Some(Transition::Flag));

        let difference = Difference::between(&facets, flag, test).expect("stated options");
        let text = |sym: Option<Sym>| sym.map(|sym| facets.strings().resolve(sym));
        let changes: Vec<(Option<&str>, Option<&str>, Option<&str>)> = difference
            .changes
            .iter()
            .map(|c| (text(Some(c.name)), text(c.dependent), text(c.dependency)))
            .collect();
        assert_eq!(
            changes,
            [(Some("compilation_mode"), Some("fastbuild"), Some("opt"))]
        );
        assert!(
            difference.fragments.is_empty(),
            "the test options dropped are left out"
        );
    }

    /// The platform options hold more than the target platform: a change to
    /// the execution platforms a build may use is a flag like any other.
    #[test]
    fn only_the_target_platform_makes_a_platform_transition() {
        let mut facets = Facets::default();
        let platforms = |extra: &str| -> Options {
            vec![(
                PLATFORM,
                vec![
                    ("extra_execution_platforms", extra.to_string()),
                    ("platforms", "//:linux".to_string()),
                ],
            )]
        };
        let dependent = configured(&mut facets, "a", "k8-fastbuild", platforms("//:local"));
        let dependency = configured(&mut facets, "b", "k8-fastbuild-ST", platforms("//:remote"));
        assert_eq!(kind(&facets, dependency, dependent), Some(Transition::Flag));
    }

    /// Bazel's null configuration holds a target that reads no configuration,
    /// which is no transition a reader follows.
    #[test]
    fn entering_the_null_configuration_crosses_nothing() {
        let mut facets = Facets::default();
        let test = target(&mut facets, "test", "//:linux", "fastbuild", false);
        let null = configured(
            &mut facets,
            "null",
            "fastbuild-noconfig",
            vec![(CORE, Vec::new())],
        );
        assert_eq!(kind(&facets, null, test), None);
    }

    #[test]
    fn a_configuration_stated_without_options_changes_it_is_not_said_how() {
        let mut facets = Facets::default();
        let test = target(&mut facets, "test", "//:linux", "fastbuild", false);
        let bare = facets.intern_config("bare", Some("k8-fastbuild"), Tri::No);
        assert_eq!(kind(&facets, bare, test), Some(Transition::Changed));
        assert_eq!(Difference::between(&facets, bare, test), None);
    }

    /// A dependent taking one target in two configurations splits over them:
    /// configured target `1` depends on target `10`'s configured targets `11`,
    /// in its own configuration, and `12`, in another platform's; `2` takes
    /// only `12`.
    #[test]
    fn a_dependent_taking_a_target_in_several_configurations_splits() {
        let mut facets = Facets::default();
        let own = target(&mut facets, "own", "//:linux", "fastbuild", true);
        let arm = target(&mut facets, "arm", "//:arm", "fastbuild", true);
        let mut graph = Graph::new();
        graph.add_node(Node::new(Id(10), "t", Kind::Target, None));
        for (id, parent, config) in [
            (1, None, own),
            (2, None, own),
            (11, Some(Id(10)), own),
            (12, Some(Id(10)), arm),
        ] {
            graph.add_node(Node::new(Id(id), "ct", Kind::ConfiguredTarget, parent));
            facets.set_config(Id(id), Some(config));
        }
        for (from, to) in [(11, 1), (12, 1), (12, 2)] {
            graph.add_edge(Id(from), Id(to), EdgeKind::Cquery);
        }
        let transitions = Transitions::of(&graph, &facets);
        assert_eq!(transitions.get(Id(11), Id(1)), None);
        assert_eq!(transitions.get(Id(12), Id(1)), Some(Transition::Split));
        assert_eq!(transitions.get(Id(12), Id(2)), Some(Transition::Platform));
    }

    #[test]
    fn a_dependency_built_as_a_tool_is_an_exec_transition() {
        let (graph, facets) = fixture();
        let transitions = Transitions::of(&graph, &facets);
        assert_eq!(transitions.get(Id(2), Id(0)), Some(Transition::Exec));
        assert_eq!(transitions.get(Id(3), Id(0)), Some(Transition::Changed));
    }

    /// Which configuration the dependency is built in decides the kind, so a
    /// tool depending on a target-configured node is no exec transition.
    #[test]
    fn the_dependency_decides_the_kind() {
        let (graph, facets) = fixture();
        let transitions = Transitions::of(&graph, &facets);
        assert_eq!(transitions.get(Id(0), Id(2)), Some(Transition::Changed));
    }

    fn connector(from: u32, to: u32) -> Connector {
        Connector {
            points: Vec::new(),
            kinds: vec![EdgeKind::Cquery],
            from: Id(from),
            to: Id(to),
            transition: None,
        }
    }

    /// Nodes `2` and `3` folded into box `20`, `0` drawn as `10` and `1` as
    /// `11`, and the rest folded away.
    #[test]
    fn a_connector_carries_the_transitions_folded_into_it() {
        let (graph, facets) = fixture();
        let drawn = |from: Id, to: Id| {
            let box_of = |id: Id| match id.0 {
                0 => Some(Id(10)),
                1 => Some(Id(11)),
                2 | 3 => Some(Id(20)),
                _ => None,
            };
            Some((box_of(from)?, box_of(to)?))
        };
        let mut connectors = [connector(20, 10), connector(10, 20), connector(10, 11)];
        Transitions::of(&graph, &facets).mark(&mut connectors, drawn);
        let marked: Vec<Option<Transition>> = connectors.iter().map(|c| c.transition).collect();
        assert_eq!(
            marked,
            [Some(Transition::Exec), Some(Transition::Changed), None],
            "an exec transition and another folded together, one other, none"
        );
    }

    /// An action or an artifact takes the configuration of the configured
    /// target holding it, and an edge between two of them joins steps inside
    /// the build of their targets rather than a dependency one target takes on
    /// another, so it crosses no transition however the two are configured.
    #[test]
    fn an_edge_between_actions_and_artifacts_crosses_nothing() {
        let (mut graph, mut facets) = fixture();
        let exec = facets.config(Id(2)).expect("the exec configuration");
        let target = facets.config(Id(0)).expect("the target configuration");
        graph.add_node(Node::new(Id(6), "tool.jar", Kind::Artifact, Some(Id(2))));
        graph.add_node(Node::new(Id(7), "Javac", Kind::Action, Some(Id(0))));
        facets.set_config(Id(6), Some(exec));
        facets.set_config(Id(7), Some(target));
        graph.add_edge(Id(6), Id(7), EdgeKind::Aquery);
        graph.add_edge(Id(6), Id(0), EdgeKind::Aquery);
        let transitions = Transitions::of(&graph, &facets);
        assert_eq!(transitions.get(Id(6), Id(7)), None, "artifact to action");
        assert_eq!(transitions.get(Id(6), Id(0)), None, "artifact to target");
    }

    #[test]
    fn an_end_stating_no_configuration_crosses_nothing() {
        let (graph, facets) = fixture();
        let transitions = Transitions::of(&graph, &facets);
        assert_eq!(transitions.get(Id(4), Id(0)), None, "a target");
        assert_eq!(transitions.get(Id(5), Id(0)), None, "no configuration");
    }
}
