//! One-click reductions: the rules a reader would otherwise have to assemble by
//! hand, each naming the rule classes, origins or mnemonics that qualify.
//!
//! A preset is a set of values on the [`crate::rules`] terms, so it reads back
//! out of the filter exactly as a checklist's rows do: it stands applied while
//! everything it names is hidden, and clicking it hides or shows that lot. The
//! terms are shared, so a preset and a checklist over the same term move each
//! other.

use crate::rules::{self, Value};
use bviz_graph_layout::facets::RuleCategory;
use bviz_graph_layout::graph::Graph;
use bviz_graph_layout::repo::RepoCategory;
use bviz_graph_layout::select::Filter;
use bviz_graph_layout::{Axis, Capabilities, Facets, Kind, Support};
use bviz_ui_components::MultiOption;

/// Every repository origin outside the one the build runs in.
const EXTERNAL_ORIGINS: [RepoCategory; 4] = [
    RepoCategory::Module,
    RepoCategory::ModuleExtension,
    RepoCategory::Builtin,
    RepoCategory::WorkspaceOrUnknown,
];

/// The mnemonics of the actions that build a runfiles tree or lay down a
/// symlink, manifest or expanded template: the wiring between the actions that
/// do the build's work rather than work itself.
///
/// Bazel 9 names the runfiles-tree action `RunfilesTree` where Bazel 8 and
/// earlier name it `Middleman`, and bviz reads a dump from either, so both stand
/// here.
const RUNFILES_MNEMONICS: [&str; 10] = [
    "SymlinkTree",
    "SourceSymlinkManifest",
    "PackagingSourcesManifest",
    "RepoMappingManifest",
    "Symlink",
    "ExecutableSymlink",
    "FileWrite",
    "TemplateExpand",
    "RunfilesTree",
    "Middleman",
];

/// A reduction offered as one click.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Preset {
    Plumbing,
    MainRepositoryOnly,
    ToolConfigurations,
    RunfilesPlumbing,
}

impl Preset {
    /// Every preset, in the order the checklist offers them.
    const ALL: [Preset; 4] = [
        Preset::Plumbing,
        Preset::MainRepositoryOnly,
        Preset::ToolConfigurations,
        Preset::RunfilesPlumbing,
    ];

    fn label(self) -> &'static str {
        match self {
            Preset::Plumbing => "hide build plumbing",
            Preset::MainRepositoryOnly => "the main repository only",
            Preset::ToolConfigurations => "hide exec/tool configurations",
            Preset::RunfilesPlumbing => "hide runfiles and symlink plumbing",
        }
    }

    /// The values the preset hides.
    pub(crate) fn hides(self) -> Vec<Value> {
        match self {
            Preset::Plumbing => vec![Value::RuleCategory(RuleCategory::Plumbing)],
            Preset::MainRepositoryOnly => EXTERNAL_ORIGINS
                .into_iter()
                .map(Value::RepoCategory)
                .collect(),
            Preset::ToolConfigurations => vec![Value::ToolConfiguration],
            Preset::RunfilesPlumbing => RUNFILES_MNEMONICS
                .into_iter()
                .map(|mnemonic| Value::Mnemonic(mnemonic.to_string()))
                .collect(),
        }
    }

    /// The axis the inputs must state for the preset to reach what it hides.
    fn axis(self) -> Axis {
        match self {
            Preset::Plumbing => Axis::RuleCategory,
            Preset::MainRepositoryOnly => Axis::Repo,
            Preset::ToolConfigurations => Axis::Configuration,
            Preset::RunfilesPlumbing => Axis::Mnemonic,
        }
    }

    /// What the graph would have to carry for the preset to hide anything, said
    /// as its absence, so a reader is told what is missing rather than left with
    /// a control that does nothing.
    fn absent(self) -> &'static str {
        match self {
            Preset::Plumbing => "no rule classes loaded",
            Preset::MainRepositoryOnly => "no repositories loaded",
            Preset::ToolConfigurations => "no configurations loaded",
            Preset::RunfilesPlumbing => "no actions loaded",
        }
    }

    /// Whether the graph carries a value on the preset's axis.
    fn carried(self, graph: &Graph, facets: &Facets) -> bool {
        match self {
            Preset::Plumbing => facets.rule_classes().next().is_some(),
            Preset::MainRepositoryOnly => facets.repos().next().is_some(),
            Preset::ToolConfigurations => facets.configs().next().is_some(),
            // A mnemonic is a facet only an action carries.
            Preset::RunfilesPlumbing => graph.nodes.iter().any(|node| node.kind == Kind::Action),
        }
    }
}

/// A preset as the loaded inputs leave it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Offer {
    preset: Preset,
    /// Why the loaded inputs cannot answer the preset; `None` where they can.
    withheld: Option<String>,
}

/// Every preset, read against what `capabilities` say the inputs state and what
/// `graph` and `facets` carry on each preset's axis.
pub(crate) fn offers(graph: &Graph, facets: &Facets, capabilities: &Capabilities) -> Vec<Offer> {
    Preset::ALL
        .into_iter()
        .map(|preset| Offer {
            withheld: withheld(preset, graph, facets, capabilities),
            preset,
        })
        .collect()
}

/// The options a preset checklist draws for `offers` under `filter`.
pub(crate) fn options(filter: &Filter, offers: &[Offer]) -> Vec<MultiOption> {
    offers
        .iter()
        .map(|offer| {
            let label = match &offer.withheld {
                Some(why) => format!("{} — {why}", offer.preset.label()),
                None => offer.preset.label().to_string(),
            };
            MultiOption {
                available: offer.withheld.is_none(),
                ..MultiOption::new(label, applied(filter, offer.preset))
            }
        })
        .collect()
}

/// Whether `filter` hides everything `preset` names.
pub(crate) fn applied(filter: &Filter, preset: Preset) -> bool {
    preset
        .hides()
        .iter()
        .all(|value| rules::hides(filter, value))
}

/// `filter` with `preset` applied, or with everything it names shown again.
pub(crate) fn set_applied(filter: &Filter, preset: Preset, applied: bool) -> Filter {
    preset.hides().iter().fold(filter.clone(), |filter, value| {
        rules::set_hidden(&filter, value, applied)
    })
}

/// The preset `offers[index]` names, and `None` where the inputs cannot answer
/// it, which is a row that reports no toggle.
pub(crate) fn offered(offers: &[Offer], index: usize) -> Option<Preset> {
    let offer = offers.get(index)?;
    offer.withheld.is_none().then_some(offer.preset)
}

/// Why the loaded inputs cannot answer `preset`: an axis a Bazel flag governs
/// and they were not asked for, or an axis they state and carry no value on.
fn withheld(
    preset: Preset,
    graph: &Graph,
    facets: &Facets,
    capabilities: &Capabilities,
) -> Option<String> {
    match capabilities.support(preset.axis()) {
        Support::Unavailable { flag } => Some(format!("needs {flag}")),
        _ if !preset.carried(graph, facets) => Some(preset.absent().to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_graph_layout::capability::CapabilitiesBuilder;
    use bviz_graph_layout::graph::Node;
    use bviz_graph_layout::intern::Id;
    use bviz_graph_layout::select::{Predicate, Rule, Values, Visibility};

    /// A graph of one target in an external repository, with the action that
    /// builds it, described by inputs that state every axis a preset reads.
    fn carrying() -> (Graph, Facets) {
        let mut graph = Graph::new();
        graph.add_node(Node::new(Id(0), ":lib", Kind::Target, None));
        graph.add_node(Node::new(Id(1), "SymlinkTree", Kind::Action, Some(Id(0))));
        let mut facets = Facets::default();
        facets.intern_repo("rules_foo+");
        facets.intern_rule_class("cc_library");
        facets.intern_config(
            "ck1",
            Some("k8-opt-exec"),
            bviz_graph_layout::facets::Tri::Yes,
        );
        (graph, facets)
    }

    /// The capabilities of inputs that state every axis they were read for.
    fn stated() -> Capabilities {
        let mut capabilities = CapabilitiesBuilder::default();
        for axis in Axis::ALL {
            capabilities.observe(axis, true);
        }
        capabilities.seal()
    }

    /// The capabilities of inputs that state nothing of any axis.
    fn unstated() -> Capabilities {
        let mut capabilities = CapabilitiesBuilder::default();
        for axis in Axis::ALL {
            capabilities.observe(axis, false);
        }
        capabilities.seal()
    }

    fn labels(options: &[MultiOption]) -> Vec<String> {
        options
            .iter()
            .map(|option| option.label.to_string())
            .collect()
    }

    /// The rules a preset writes, so that what it claims to hide can be read off
    /// the filter it leaves rather than off its own description.
    #[test]
    fn each_preset_writes_the_rules_it_claims() {
        let cases = [
            (
                Preset::Plumbing,
                Predicate {
                    rule_categories: Values::new([RuleCategory::Plumbing]),
                    ..Predicate::default()
                },
            ),
            (
                Preset::MainRepositoryOnly,
                Predicate {
                    repo_categories: Values::new(EXTERNAL_ORIGINS),
                    ..Predicate::default()
                },
            ),
            (
                Preset::ToolConfigurations,
                Predicate {
                    is_tool: Some(true),
                    ..Predicate::default()
                },
            ),
            (
                Preset::RunfilesPlumbing,
                Predicate {
                    mnemonics: Values::new(RUNFILES_MNEMONICS.map(str::to_string)),
                    ..Predicate::default()
                },
            ),
        ];

        for (preset, predicate) in cases {
            let filter = set_applied(&Filter::default(), preset, true);
            assert_eq!(
                filter.rules,
                vec![Rule {
                    predicate,
                    visibility: Visibility::Hidden,
                }],
                "{preset:?}"
            );
            assert!(applied(&filter, preset), "{preset:?}");
            assert_eq!(
                set_applied(&filter, preset, false),
                Filter::default(),
                "{preset:?}"
            );
        }
    }

    /// The runfiles preset covers the runfiles-tree action under both of the
    /// names Bazel has given it, so a dump from either release reduces alike.
    #[test]
    fn the_runfiles_preset_names_the_runfiles_tree_action_of_either_release() {
        let hidden = Preset::RunfilesPlumbing.hides();
        for mnemonic in ["RunfilesTree", "Middleman"] {
            assert!(
                hidden.contains(&Value::Mnemonic(mnemonic.to_string())),
                "{mnemonic}"
            );
        }
    }

    /// Everything a preset names is hidden together, so a preset stands applied
    /// only while all of it is, and drops off when one value is shown again.
    #[test]
    fn a_preset_stands_applied_only_while_everything_it_names_is_hidden() {
        let preset = Preset::MainRepositoryOnly;
        let filter = set_applied(&Filter::default(), preset, true);
        let one_back =
            rules::set_hidden(&filter, &Value::RepoCategory(RepoCategory::Builtin), false);

        assert!(!applied(&one_back, preset));
    }

    #[test]
    fn a_preset_the_inputs_state_and_carry_is_offered() {
        let (graph, facets) = carrying();

        let offers = offers(&graph, &facets, &stated());

        assert_eq!(
            labels(&options(&Filter::default(), &offers)),
            [
                "hide build plumbing",
                "the main repository only",
                "hide exec/tool configurations",
                "hide runfiles and symlink plumbing",
            ]
        );
        for index in 0..offers.len() {
            assert_eq!(offered(&offers, index), Some(Preset::ALL[index]));
        }
    }

    /// An axis a Bazel flag governs and the inputs were not run with leaves the
    /// preset over it unavailable, naming the flag that would restore it.
    #[test]
    fn a_preset_over_an_axis_the_inputs_do_not_state_is_withheld() {
        let (graph, facets) = carrying();

        let offers = offers(&graph, &facets, &unstated());
        let options = options(&Filter::default(), &offers);

        let main = Preset::ALL
            .iter()
            .position(|&preset| preset == Preset::MainRepositoryOnly)
            .expect("a main-repository preset");
        assert_eq!(
            options[main].label,
            "the main repository only — needs --consistent_labels"
        );
        assert!(!options[main].available);
        assert_eq!(offered(&offers, main), None);
    }

    /// An axis the inputs state but carry no value on is nothing to reduce, so
    /// the preset over it says what is absent rather than doing nothing.
    #[test]
    fn a_preset_the_graph_carries_no_value_for_is_withheld() {
        let mut graph = Graph::new();
        graph.add_node(Node::new(Id(0), ":lib", Kind::Target, None));

        let offers = offers(&graph, &Facets::default(), &stated());
        let options = options(&Filter::default(), &offers);

        assert_eq!(
            labels(&options),
            [
                "hide build plumbing — no rule classes loaded",
                "the main repository only — no repositories loaded",
                "hide exec/tool configurations — no configurations loaded",
                "hide runfiles and symlink plumbing — no actions loaded",
            ]
        );
        assert!(options.iter().all(|option| !option.available));
    }
}
