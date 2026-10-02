//! The configured targets a build of some targets starts from: the copy of
//! each target in the configuration Bazel configures a requested target in.
//!
//! No input marks that configuration. A build configures every requested
//! target in its top-level configuration, which carries the test options, and
//! on entering a rule trims them away unless the rule may come to read them
//! (`analysis/test/TestTrimmingTransitionFactory.java`): a rule of a class
//! allowed the test configuration, which the test rules and `test_suite` are;
//! one that may stand for another target, as an `alias`, a `label_flag` and a
//! `label_setting` may; and a rule kept untrimmed by what no input states — a
//! `config_setting` on a test option, a `transitive_configs` attribute opting
//! out, `testonly` under `--experimental_retain_test_configuration_across_testonly`.
//! A requested target is therefore looked for in the configuration its rule
//! class says, the one carrying the test options or its trimmed twin, which
//! shares its mnemonic, and then in the other of the two.
//!
//! Bazel is cited here as of its tag 9.1.0, a source file by its path beneath
//! `src/main/java/com/google/devtools/build/lib`.

use std::collections::VecDeque;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::dominators::{Actions, ConfiguredTargets};
use crate::facets::{ConfigOptions, ConfigRef, Facets, OptionSetting, RuleCategory, Sym, Tri};
use crate::graph::{Graph, Kind, id_span};
use crate::intern::Id;

/// The fragment options class of the test options
/// (`analysis/test/TestConfiguration.java`).
const TEST_OPTIONS: &str =
    "com.google.devtools.build.lib.analysis.test.TestConfiguration$TestOptions";

/// The option outside the test options that trimming them changes
/// (`analysis/test/TestTrimmingLogic.java`).
const RUN_UNDER: &str = "run_under";

/// What a configuration's mnemonic carries where a Starlark transition set its
/// options (`analysis/config/OutputPathMnemonicComputer.java`).
const STARLARK_TRANSITION_MARK: &str = "-ST-";

/// The rule classes beside `alias` whose rules may stand for another target
/// (`rules/LabelBuildSettings.java`).
const LABEL_SETTINGS: [&str; 2] = ["label_flag", "label_setting"];

/// The copies a build of some targets configures them in at the top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopLevel {
    /// In id order.
    copies: Vec<Id>,
    guessed: u32,
    generated: u32,
    uncopied: u32,
}

/// What the graph holds of one requested target.
#[derive(Default)]
struct Requested {
    copies: Vec<Id>,
    /// Whether the target is a file a rule generates.
    generated: bool,
}

impl TopLevel {
    /// The top-level copies of `targets` among `configured`. A target with no
    /// copy in either configuration a requested target is configured in — one
    /// whose rule class transitions on entering it, which no input states, or
    /// any target where the graph states no configuration's options — stands
    /// for its copies outside a tool configuration that are top-level, or
    /// failing those every copy outside a tool configuration.
    pub fn of(
        targets: &[Id],
        graph: &Graph,
        facets: &Facets,
        configured: ConfiguredTargets<'_>,
    ) -> Self {
        let top = TopConfigurations::of(facets);
        let mut requested: FxHashMap<Id, Requested> =
            FxHashMap::with_capacity_and_hasher(targets.len(), Default::default());
        requested.extend(targets.iter().map(|&target| (target, Requested::default())));
        for node in &graph.nodes {
            match node.kind {
                Kind::ConfiguredTarget => {
                    let target = facets.label(node.id);
                    if let Some(requested) = target.and_then(|t| requested.get_mut(&t)) {
                        requested.copies.push(node.id);
                    }
                }
                Kind::Artifact => {
                    if let Some(requested) = requested.get_mut(&node.id) {
                        requested.generated = true;
                    }
                }
                _ => {}
            }
        }

        let is_tool = |copy: Id| {
            let entry = facets.config(copy).and_then(|c| facets.config_entry(c));
            entry.is_some_and(|entry| entry.is_tool == Tri::Yes)
        };
        let mut picked: Vec<Id> = Vec::with_capacity(targets.len());
        let (mut guessed, mut generated, mut uncopied) = (0, 0, 0);
        for target in targets {
            let Requested {
                copies,
                generated: is_generated,
            } = &requested[target];
            let within = |configs: &[ConfigRef]| -> Vec<Id> {
                let held = |copy: &Id| facets.config(*copy).is_some_and(|c| configs.contains(&c));
                copies.iter().copied().filter(held).collect()
            };
            let (tested, trimmed) = ((&top.tested, false), (&top.trimmed, top.twins_alike));
            let looked_in = match keeps_test_options(facets, *target) {
                true => [tested, trimmed],
                false => [trimmed, tested],
            };
            let at_top = looked_in
                .into_iter()
                .map(|(configs, alike)| (within(configs), alike))
                .find(|(copies, _)| !copies.is_empty());
            if let Some((copies, alike)) = at_top {
                guessed += u32::from(alike && copies.len() > 1);
                picked.extend(copies);
                continue;
            }
            let built: Vec<Id> = (copies.iter().copied())
                .filter(|&copy| !is_tool(copy))
                .collect();
            if built.is_empty() {
                match is_generated {
                    true => generated += 1,
                    false => uncopied += 1,
                }
                continue;
            }
            guessed += 1;
            let before = picked.len();
            picked.extend(built.iter().filter(|&&copy| configured.is_top_level(copy)));
            if picked.len() == before {
                picked.extend(built);
            }
        }
        picked.sort_unstable();
        picked.dedup();
        TopLevel {
            copies: picked,
            guessed,
            generated,
            uncopied,
        }
    }

    pub fn copies(&self) -> &[Id] {
        &self.copies
    }

    /// How many targets stand for copies the inputs do not single out: those
    /// picked by their place in the graph, for want of one in a configuration
    /// a requested target is configured in, and those in several trimmed
    /// twins no options tell apart.
    pub fn guessed(&self) -> u32 {
        self.guessed
    }

    /// How many targets are generated files with no copy outside a tool
    /// configuration. A build makes such a file through the rule generating
    /// it, which only the actions of a build tie it to.
    pub fn generated(&self) -> u32 {
        self.generated
    }

    /// How many targets other than generated files have no copy outside a
    /// tool configuration, such as a source file, which no configuration
    /// builds.
    pub fn uncopied(&self) -> u32 {
        self.uncopied
    }

    /// The actions a build of the copies starts from, in id order: of those
    /// a copy holds, each that no other of them reads among `actions`, from
    /// which a build of the copy reaches the rest; and for a copy holding
    /// none, as that of an `alias`, a `filegroup` or a `test_suite` does,
    /// those of the nearest of the `configured` targets it depends on that
    /// hold any. A copy every action of which another of them reads starts
    /// from them all.
    pub fn actions(
        &self,
        graph: &Graph,
        configured: ConfiguredTargets<'_>,
        actions: Actions<'_>,
    ) -> Vec<Id> {
        let span = id_span(graph);
        let mut parent: Vec<Option<Id>> = vec![None; span];
        let mut is_configured = vec![false; span];
        for node in &graph.nodes {
            parent[node.id.0 as usize] = parent[node.id.0 as usize].or(node.parent);
            is_configured[node.id.0 as usize] |= node.kind == Kind::ConfiguredTarget;
        }
        let holder = |action: Id| {
            std::iter::successors(parent[action.0 as usize], |at| parent[at.0 as usize])
                .find(|at| is_configured[at.0 as usize])
        };
        let held = || {
            let actions = graph.nodes.iter().filter(|node| node.kind == Kind::Action);
            actions.filter_map(|action| Some((action.id, holder(action.id)?)))
        };
        let mut holds = vec![false; span];
        for (_, holder) in held() {
            holds[holder.0 as usize] = true;
        }

        let mut starts = vec![false; span];
        let mut seen: FxHashSet<Id> = self.copies.iter().copied().collect();
        let mut walk: VecDeque<Id> = self.copies.iter().copied().collect();
        while let Some(at) = walk.pop_front() {
            match holds[at.0 as usize] {
                true => starts[at.0 as usize] = true,
                false => walk.extend(configured.dependencies(at).filter(|&d| seen.insert(d))),
            }
        }
        let read_within =
            |action: Id, copy: Id| actions.readers(action).any(|by| holder(by) == Some(copy));
        let started = || held().filter(|(_, copy)| starts[copy.0 as usize]);
        let mut from: Vec<Id> = Vec::with_capacity(self.copies.len());
        let mut ends = vec![false; span];
        for (action, copy) in started() {
            if !read_within(action, copy) {
                ends[copy.0 as usize] = true;
                from.push(action);
            }
        }
        from.extend(
            started()
                .filter(|(_, copy)| !ends[copy.0 as usize])
                .map(|(action, _)| action),
        );
        from.sort_unstable();
        from.dedup();
        from
    }
}

/// Whether `target` is a rule its class keeps the test options for.
fn keeps_test_options(facets: &Facets, target: Id) -> bool {
    let Some(class) = facets.rule_class(target) else {
        return false;
    };
    let category = facets.rule_category(class);
    matches!(category, Some(RuleCategory::Test | RuleCategory::Alias))
        || LABEL_SETTINGS.contains(&facets.strings().resolve(class.sym()))
}

/// The configurations a build configures a requested target in: those
/// carrying the test options outside a tool configuration, and their trimmed
/// twins. Either is empty where no configuration states its options.
struct TopConfigurations {
    tested: Vec<ConfigRef>,
    /// For each of `tested`, the configurations sharing its mnemonic without
    /// the test options: those setting every other option as it does, or all
    /// of them where none does.
    trimmed: Vec<ConfigRef>,
    /// Whether some one of `tested` has several twins among `trimmed`, of
    /// which a build configures a requested target in one.
    twins_alike: bool,
}

/// A configuration a requested target may be configured in.
struct Candidate<'a> {
    config: ConfigRef,
    mnemonic: Option<Sym>,
    options: &'a ConfigOptions,
}

impl TopConfigurations {
    fn of(facets: &Facets) -> Self {
        let strings = facets.strings();
        let mut top = TopConfigurations {
            tested: Vec::new(),
            trimmed: Vec::new(),
            twins_alike: false,
        };
        let Some(test_options) = strings.get(TEST_OPTIONS) else {
            return top;
        };
        let mut tested: Vec<Candidate> = Vec::new();
        let mut untested: Vec<Candidate> = Vec::new();
        for (config, entry) in facets.configs() {
            let mnemonic = entry.mnemonic.map(|m| strings.resolve(m));
            let transitioned = mnemonic.is_some_and(|m| m.contains(STARLARK_TRANSITION_MARK));
            if entry.is_tool == Tri::Yes || transitioned {
                continue;
            }
            let Some(options) = facets.config_options(config) else {
                continue;
            };
            let candidate = Candidate {
                config,
                mnemonic: entry.mnemonic,
                options,
            };
            match options.fragments().binary_search(&test_options).is_ok() {
                true => tested.push(candidate),
                false => untested.push(candidate),
            }
        }
        let run_under = strings.get(RUN_UNDER);
        let unchanged = |setting: &&OptionSetting| {
            setting.fragment != test_options && Some(setting.name) != run_under
        };
        for carrying in &tested {
            let twins = || untested.iter().filter(|c| c.mnemonic == carrying.mnemonic);
            let trims = |twin: &&Candidate| {
                let (carried, kept) = (carrying.options, twin.options);
                let fragments = carried.fragments().iter().filter(|&&f| f != test_options);
                fragments.eq(kept.fragments())
                    && (carried.settings().iter().filter(unchanged))
                        .eq(kept.settings().iter().filter(unchanged))
            };
            let before = top.trimmed.len();
            top.trimmed.extend(twins().filter(trims).map(|c| c.config));
            if top.trimmed.len() == before {
                top.trimmed.extend(twins().map(|c| c.config));
            }
            top.twins_alike |= top.trimmed.len() - before > 1;
        }
        top.tested = tested.into_iter().map(|c| c.config).collect();
        top
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dominators::ScopeGraph;
    use crate::graph::{EdgeKind, Node};
    use crate::intern::Names;
    use crate::metrics::{Scope, Unit};
    use crate::provenance::{InputSet, Provenance};

    const CORE_OPTIONS: &str = "CoreOptions";

    /// The options of one fragment, as a configuration states them.
    type Fragment = (&'static str, Vec<(&'static str, Option<&'static str>)>);

    /// A build whose configurations are named by a word each.
    struct Build {
        graph: Graph,
        names: Names,
        facets: Facets,
        configs: FxHashMap<&'static str, ConfigRef>,
    }

    impl Build {
        /// A build of the configuration `k8` with the test options, its
        /// trimmed twin `k8 trimmed`, which shares its mnemonic, a
        /// configuration `k8-ST-1` a transition set, and a tool configuration
        /// `exec`, each stating its options where `stating_options`.
        fn new(stating_options: bool) -> Self {
            let mut build = Build {
                graph: Graph::new(),
                names: Names::default(),
                facets: Facets::default(),
                configs: FxHashMap::default(),
            };
            for (name, mnemonic, tool, tested) in [
                ("k8", "k8-fastbuild", Tri::No, true),
                ("k8 trimmed", "k8-fastbuild", Tri::No, false),
                ("k8-ST-1", "k8-fastbuild-ST-1", Tri::No, false),
                ("exec", "k8-opt-exec", Tri::Yes, false),
            ] {
                let core = (CORE_OPTIONS, Vec::new());
                let fragments = match tested {
                    true => vec![core, (TEST_OPTIONS, Vec::new())],
                    false => vec![core],
                };
                build.config(name, mnemonic, tool, stating_options.then_some(fragments));
            }
            build
        }

        fn config(
            &mut self,
            name: &'static str,
            mnemonic: &str,
            tool: Tri,
            fragments: Option<Vec<Fragment>>,
        ) -> &mut Self {
            let config = self.facets.intern_config(name, Some(mnemonic), tool);
            if let Some(fragments) = fragments {
                self.facets.record_config_options(config, fragments);
            }
            self.configs.insert(name, config);
            self
        }

        /// A configuration outside any tool configuration with the mnemonic
        /// `k8` has, setting `copt` to `copt` and stating no test options.
        fn twin(&mut self, name: &'static str, copt: &'static str) -> &mut Self {
            let core = (CORE_OPTIONS, vec![("copt", Some(copt)), (RUN_UNDER, None)]);
            self.config(name, "k8-fastbuild", Tri::No, Some(vec![core]))
        }

        /// A target of rule class `class` with a copy in each of `configs`.
        fn target(&mut self, label: &str, class: &str, configs: &[&str]) -> &mut Self {
            let target = self.file(label, Kind::Target);
            let class = self.facets.intern_rule_class(class);
            self.facets.set_rule_class(target, Some(class));
            for config in configs {
                let copy = self.names.intern(&format!("{label} ({config})"));
                self.graph
                    .add_node(Node::new(copy, label, Kind::ConfiguredTarget, Some(target)));
                self.facets.set_label(copy, target);
                self.facets.set_config(copy, Some(self.configs[config]));
            }
            self
        }

        /// A target of the kind `kind` with no copy.
        fn file(&mut self, label: &str, kind: Kind) -> Id {
            let target = self.names.intern(label);
            self.graph.add_node(Node::new(target, label, kind, None));
            self.facets.set_label(target, target);
            target
        }

        fn depends(&mut self, dependent: &str, dependency: &str) -> &mut Self {
            let (from, to) = (self.id(dependency), self.id(dependent));
            self.graph.add_edge(from, to, EdgeKind::Cquery);
            self
        }

        fn action(&mut self, name: &str, copy: &str) -> &mut Self {
            let (action, copy) = (self.names.intern(name), self.id(copy));
            self.graph
                .add_node(Node::new(action, name, Kind::Action, Some(copy)));
            self
        }

        /// The action `reader` reads what the action `read` writes.
        fn reads(&mut self, reader: &str, read: &str) -> &mut Self {
            let (from, to) = (self.id(read), self.id(reader));
            self.graph.add_edge(from, to, EdgeKind::Aquery);
            self
        }

        fn id(&self, name: &str) -> Id {
            self.names.get(name).unwrap()
        }

        fn configured(&self) -> ScopeGraph {
            self.scope_graph(Unit::ConfiguredTarget)
        }

        fn scope_graph(&self, unit: Unit) -> ScopeGraph {
            let everything = Provenance::new(vec![InputSet::ALL; self.names.len()]);
            let scope = Scope::new(unit, InputSet::ALL).unwrap();
            ScopeGraph::new(&self.graph, &self.names, &self.facets, &everything, scope)
        }

        /// The actions a build of `targets` starts from.
        fn starts(&self, targets: &[&str]) -> Vec<&str> {
            let (configured, actions) = (self.configured(), self.scope_graph(Unit::Action));
            let configured = configured.configured_targets().unwrap();
            let from = (self.top_level(targets)).actions(
                &self.graph,
                configured,
                actions.actions().unwrap(),
            );
            self.named(&from)
        }

        fn top_level(&self, targets: &[&str]) -> TopLevel {
            let configured = self.configured();
            let targets: Vec<Id> = targets.iter().map(|&t| self.id(t)).collect();
            let configured = configured.configured_targets().unwrap();
            TopLevel::of(&targets, &self.graph, &self.facets, configured)
        }

        /// The copies picked for `targets`, and how many targets were guessed.
        fn copies(&self, targets: &[&str]) -> (Vec<&str>, u32) {
            let top = self.top_level(targets);
            (self.named(top.copies()), top.guessed())
        }

        fn named(&self, ids: &[Id]) -> Vec<&str> {
            let mut named: Vec<&str> = ids.iter().map(|&id| self.names.resolve(id)).collect();
            named.sort_unstable();
            named
        }
    }

    /// A rule of a class keeping the test options is built in the
    /// configuration carrying them, any other rule in its trimmed twin.
    #[test]
    fn a_rule_is_configured_at_the_top_as_its_class_says() {
        let mut build = Build::new(true);
        let all = ["k8", "k8 trimmed", "exec"];
        build
            .target("//:test", "sh_test", &all)
            .target("//:suite", "test_suite", &all)
            .target("//:alias", "alias", &all)
            .target("//:flag", "label_flag", &all)
            .target("//:setting", "label_setting", &all)
            .target("//:lib", "cc_library", &all);
        let targets = [
            "//:test",
            "//:suite",
            "//:alias",
            "//:flag",
            "//:setting",
            "//:lib",
        ];
        let copies = vec![
            "//:alias (k8)",
            "//:flag (k8)",
            "//:lib (k8 trimmed)",
            "//:setting (k8)",
            "//:suite (k8)",
            "//:test (k8)",
        ];
        assert_eq!(build.copies(&targets), (copies, 0));
    }

    /// A rule kept untrimmed by what no input states has its copy in the
    /// configuration carrying the test options, which is no guess.
    #[test]
    fn a_target_with_no_copy_where_its_class_says_is_taken_in_the_other_configuration() {
        let mut build = Build::new(true);
        build
            .target("//:untrimmed", "cc_library", &["k8", "exec"])
            .target("//:alias", "alias", &["k8 trimmed", "k8-ST-1"]);
        assert_eq!(
            build.copies(&["//:untrimmed", "//:alias"]),
            (vec!["//:alias (k8 trimmed)", "//:untrimmed (k8)"], 0)
        );
    }

    /// A rule transitioning on entering it has no copy in either top-level
    /// configuration, and stands for its top-level copies outside a tool
    /// configuration, or failing those every copy outside one.
    #[test]
    fn a_target_without_a_copy_at_the_top_stands_for_its_top_level_copies() {
        let mut build = Build::new(true);
        build
            .target("//:moved", "with_cfg", &["k8-ST-1", "exec"])
            .target("//:shared", "with_cfg", &["k8-ST-1", "exec"])
            .target("//:user", "cc_binary", &["k8 trimmed"])
            .depends("//:user (k8 trimmed)", "//:shared (k8-ST-1)")
            .target("//:tool", "cc_binary", &["exec"]);
        let top = build.top_level(&["//:moved", "//:shared", "//:tool"]);
        assert_eq!(
            build.named(top.copies()),
            ["//:moved (k8-ST-1)", "//:shared (k8-ST-1)"]
        );
        assert_eq!((top.guessed(), top.uncopied()), (2, 1));
    }

    #[test]
    fn a_generated_file_without_a_copy_is_counted_apart_from_a_source_file() {
        let mut build = Build::new(true);
        build.file("//:gen.out", Kind::Artifact);
        build.file("//:main.c", Kind::Source);
        let top = build.top_level(&["//:gen.out", "//:main.c"]);
        assert!(top.copies().is_empty());
        assert_eq!((top.generated(), top.uncopied()), (1, 1));
    }

    /// Where no configuration states its options, the top-level ones are
    /// unknown and every target's copies are picked by their place.
    #[test]
    fn without_the_options_of_any_configuration_every_copy_is_guessed() {
        let mut build = Build::new(false);
        build
            .target("//:app", "cc_binary", &["k8", "k8 trimmed"])
            .target("//:lib", "cc_library", &["k8 trimmed"])
            .depends("//:app (k8 trimmed)", "//:lib (k8 trimmed)");
        assert_eq!(
            build.copies(&["//:app", "//:lib"]),
            (
                vec!["//:app (k8 trimmed)", "//:app (k8)", "//:lib (k8 trimmed)"],
                2
            )
        );
    }

    /// A build whose configuration carrying the test options sets `copt` to
    /// `-O1`, beside the twins `twins` names with the `copt` each sets.
    fn with_twins(twins: &[(&'static str, &'static str)]) -> Build {
        let mut build = Build {
            graph: Graph::new(),
            names: Names::default(),
            facets: Facets::default(),
            configs: FxHashMap::default(),
        };
        let core = (
            CORE_OPTIONS,
            vec![("copt", Some("-O1")), (RUN_UNDER, Some("//:runner"))],
        );
        let tested = vec![core, (TEST_OPTIONS, vec![("test_timeout", Some("60"))])];
        build.config("k8", "k8-fastbuild", Tri::No, Some(tested));
        for &(name, copt) in twins {
            build.twin(name, copt);
        }
        build
    }

    /// Of several configurations sharing the mnemonic without the test
    /// options, the trimmed twin is the one setting every other option alike.
    #[test]
    fn the_trimmed_twin_is_the_one_whose_other_options_are_alike() {
        let mut build = with_twins(&[("alike", "-O1"), ("other", "-O2")]);
        build.target("//:lib", "cc_library", &["k8", "alike", "other"]);
        assert_eq!(build.copies(&["//:lib"]), (vec!["//:lib (alike)"], 0));
    }

    /// Twins no options single out all stand for the trimmed one, and a
    /// target with a copy in several of them is guessed.
    #[test]
    fn twins_the_options_tell_no_one_of_apart_are_all_taken() {
        let mut build = with_twins(&[("one", "-O2"), ("two", "-O3")]);
        build
            .target("//:lib", "cc_library", &["k8", "one", "two"])
            .target("//:app", "cc_binary", &["two"]);
        assert_eq!(
            build.copies(&["//:lib", "//:app"]),
            (vec!["//:app (two)", "//:lib (one)", "//:lib (two)"], 1)
        );
    }

    /// A copy starts from its actions nothing else it holds reads, and the
    /// copies in a tool configuration are no part of what was requested.
    #[test]
    fn a_copy_starts_from_its_actions_no_other_of_its_actions_reads() {
        let mut build = Build::new(true);
        build
            .target("//:app", "cc_binary", &["k8 trimmed", "exec"])
            .action("link", "//:app (k8 trimmed)")
            .action("compile", "//:app (k8 trimmed)")
            .action("strip", "//:app (k8 trimmed)")
            .action("tool link", "//:app (exec)")
            .reads("link", "compile")
            .reads("strip", "link");
        assert_eq!(build.starts(&["//:app"]), ["strip"]);
    }

    /// An action another requested copy reads is still where its own copy
    /// starts from, as that copy was asked for.
    #[test]
    fn an_action_another_requested_copy_reads_stays_a_start() {
        let mut build = Build::new(true);
        build
            .target("//:app", "cc_binary", &["k8 trimmed"])
            .target("//:lib", "cc_library", &["k8 trimmed"])
            .depends("//:app (k8 trimmed)", "//:lib (k8 trimmed)")
            .action("link", "//:app (k8 trimmed)")
            .action("compile", "//:lib (k8 trimmed)")
            .action("archive", "//:lib (k8 trimmed)")
            .reads("archive", "compile")
            .reads("link", "archive");
        assert_eq!(build.starts(&["//:app", "//:lib"]), ["archive", "link"]);
        assert_eq!(build.starts(&["//:app"]), ["link"]);
    }

    /// Actions reading one another round a cycle leave their copy none to
    /// start from that the others do not read, so it starts from them all.
    #[test]
    fn a_copy_whose_actions_all_read_one_another_starts_from_them_all() {
        let mut build = Build::new(true);
        build
            .target("//:app", "cc_binary", &["k8 trimmed"])
            .action("one", "//:app (k8 trimmed)")
            .action("other", "//:app (k8 trimmed)")
            .reads("one", "other")
            .reads("other", "one");
        assert_eq!(build.starts(&["//:app"]), ["one", "other"]);
    }

    /// A copy holding no action stands for the actions of the nearest
    /// configured targets it depends on that hold any, and for none beyond
    /// those.
    #[test]
    fn a_copy_holding_no_action_starts_from_those_of_what_it_depends_on() {
        let mut build = Build::new(true);
        build
            .target("//:alias", "alias", &["k8"])
            .target("//:files", "filegroup", &["k8 trimmed"])
            .target("//:app", "cc_binary", &["k8 trimmed"])
            .target("//:gen", "genrule", &["k8 trimmed"])
            .target("//:lib", "cc_library", &["k8 trimmed"])
            .depends("//:alias (k8)", "//:files (k8 trimmed)")
            .depends("//:alias (k8)", "//:app (k8 trimmed)")
            .depends("//:files (k8 trimmed)", "//:gen (k8 trimmed)")
            .depends("//:files (k8 trimmed)", "//:app (k8 trimmed)")
            .depends("//:app (k8 trimmed)", "//:lib (k8 trimmed)")
            .action("link", "//:app (k8 trimmed)")
            .action("generate", "//:gen (k8 trimmed)")
            .action("compile", "//:lib (k8 trimmed)");
        assert_eq!(build.starts(&["//:alias"]), ["generate", "link"]);
    }
}
