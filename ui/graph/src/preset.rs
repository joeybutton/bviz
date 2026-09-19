//! One-click reductions: the rules a reader would otherwise have to assemble by
//! hand, each naming the rule classes, origins or mnemonics that qualify.
//!
//! A preset is written over the [`crate::rules`] terms, so it reads back out of
//! the filter exactly as a checklist's rows do, and the terms are shared, so a
//! preset and a checklist over the same term move each other.

use crate::rules::{self, Term, Value};
use bviz_graph_layout::facets::RuleCategory;
use bviz_graph_layout::graph::Graph;
use bviz_graph_layout::repo::RepoCategory;
use bviz_graph_layout::select::Filter;
use bviz_graph_layout::{Axis, Capabilities, Facets, Kind, Support};
use bviz_ui_components::MultiOption;
use std::collections::BTreeSet;

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

/// What a preset leaves on the terms it is written over, which is what its
/// label promises a reader.
///
/// A label naming what the preset removes promises nothing about the rest of
/// its terms, so it may only add to what they hide. A label asserting what is
/// left — "the main repository only" — promises the whole of its terms, which
/// is reached only by stating them.
enum Reduction {
    /// Hides these values, leaving everything else hidden on their terms as it
    /// stands, and shows these again when taken off.
    Adds(Vec<Value>),
    /// Hides on each term exactly the values given and nothing else, and leaves
    /// each term hiding nothing at all when taken off. A term is stated by at
    /// most one preset, or the end it stands at would be settled by whichever
    /// row was written last rather than by what either label promises.
    States(Vec<Stated>),
}

/// One term and the whole of what a preset leaves it hiding.
struct Stated {
    term: Term,
    values: BTreeSet<Value>,
}

impl Stated {
    /// `term` left hiding those of `values` that are of it. A value of another
    /// term names nothing a rule over `term` can hide, so it would read back as
    /// a difference the preset's own click could never close.
    fn new(term: Term, values: impl IntoIterator<Item = Value>) -> Self {
        Stated {
            term,
            values: values
                .into_iter()
                .filter(|value| value.term() == term)
                .collect(),
        }
    }
}

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

    /// What the preset leaves on the terms it is written over.
    fn reduction(self) -> Reduction {
        match self {
            Preset::Plumbing => Reduction::Adds(vec![Value::RuleCategory(RuleCategory::Plumbing)]),
            // A repository is selected on two terms at once, its origin and its
            // name, so both have to be stated for the main repository to be the
            // whole of what is drawn.
            Preset::MainRepositoryOnly => Reduction::States(vec![
                Stated::new(
                    Term::RepoCategory,
                    EXTERNAL_ORIGINS.into_iter().map(Value::RepoCategory),
                ),
                Stated::new(Term::Repo, []),
            ]),
            Preset::ToolConfigurations => Reduction::Adds(vec![Value::ToolConfiguration]),
            Preset::RunfilesPlumbing => Reduction::Adds(
                RUNFILES_MNEMONICS
                    .into_iter()
                    .map(|mnemonic| Value::Mnemonic(mnemonic.to_string()))
                    .collect(),
            ),
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

/// Whether `filter` stands where `preset` leaves it.
pub(crate) fn applied(filter: &Filter, preset: Preset) -> bool {
    match preset.reduction() {
        Reduction::Adds(values) => values.iter().all(|value| rules::hides(filter, value)),
        Reduction::States(terms) => terms
            .into_iter()
            .all(|stated| rules::hidden(filter, stated.term) == stated.values),
    }
}

/// `filter` with `preset` applied, or taken back off; `filter` as it stands
/// where it already reads as asked.
///
/// A preset naming an end state takes itself off by clearing the whole of every
/// term it states, so acting on one that does not stand would drop rules the
/// preset never wrote — among them whatever another control hid on those terms.
pub(crate) fn set_applied(filter: &Filter, preset: Preset, applied: bool) -> Filter {
    if self::applied(filter, preset) == applied {
        return filter.clone();
    }
    match preset.reduction() {
        Reduction::Adds(values) => values.iter().fold(filter.clone(), |filter, value| {
            rules::set_hidden(&filter, value, applied)
        }),
        Reduction::States(terms) => terms.into_iter().fold(filter.clone(), |filter, stated| {
            let values = if applied {
                stated.values
            } else {
                BTreeSet::new()
            };
            rules::hiding(&filter, stated.term, values)
        }),
    }
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
    use bviz_graph_layout::repo::RepoName;
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
        let Reduction::Adds(hidden) = Preset::RunfilesPlumbing.reduction() else {
            panic!("the runfiles preset names what it removes");
        };
        for mnemonic in ["RunfilesTree", "Middleman"] {
            assert!(
                hidden.contains(&Value::Mnemonic(mnemonic.to_string())),
                "{mnemonic}"
            );
        }
    }

    /// A preset asserting what is left states its terms, so what it keeps is
    /// drawn again however they were left — a repository checklist cleared to
    /// nothing included, which is the state the preset would otherwise add to
    /// and leave empty.
    #[test]
    fn a_preset_naming_an_end_state_shows_again_what_it_keeps() {
        let cleared = [
            Value::RepoCategory(RepoCategory::Main),
            Value::Repo(RepoName::Canonical("rules_foo+".into())),
        ]
        .iter()
        .fold(Filter::default(), |filter, value| {
            rules::set_hidden(&filter, value, true)
        });

        let only_main = set_applied(&cleared, Preset::MainRepositoryOnly, true);

        assert!(applied(&only_main, Preset::MainRepositoryOnly));
        assert!(!rules::hides(
            &only_main,
            &Value::RepoCategory(RepoCategory::Main)
        ));
        assert!(rules::hidden(&only_main, Term::Repo).is_empty());
        assert_eq!(
            set_applied(&only_main, Preset::MainRepositoryOnly, false),
            Filter::default()
        );
    }

    /// A preset naming what it removes adds to its terms and takes back only
    /// its own, so what a bulk clear hid there stands through both.
    #[test]
    fn a_preset_naming_what_it_removes_leaves_the_rest_of_its_term_hidden() {
        let others = [RuleCategory::Test, RuleCategory::Alias];
        let cleared = others.iter().fold(Filter::default(), |filter, &category| {
            rules::set_hidden(&filter, &Value::RuleCategory(category), true)
        });

        let reduced = set_applied(&cleared, Preset::Plumbing, true);

        assert!(applied(&reduced, Preset::Plumbing));
        for category in others {
            assert!(
                rules::hides(&reduced, &Value::RuleCategory(category)),
                "{category:?}"
            );
        }
        assert_eq!(set_applied(&reduced, Preset::Plumbing, false), cleared);
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

    /// A preset naming an end state claims the whole of each term it states, so
    /// two of them over one term would promise ends that cannot both hold and
    /// settle it by whichever row a menu-wide action wrote last.
    #[test]
    fn no_two_presets_state_an_end_for_one_term() {
        let stated: Vec<Term> = Preset::ALL
            .into_iter()
            .filter_map(|preset| match preset.reduction() {
                Reduction::States(terms) => Some(terms),
                Reduction::Adds(_) => None,
            })
            .flatten()
            .map(|stated| stated.term)
            .collect();

        for (index, term) in stated.iter().enumerate() {
            assert!(!stated[index + 1..].contains(term), "{term:?}");
        }
    }

    /// A preset states only values of the term it names, so a term reads back
    /// exactly what the preset wrote and its row closes the difference.
    #[test]
    fn a_term_a_preset_states_holds_only_its_own_values() {
        let stated = Stated::new(
            Term::RepoCategory,
            [
                Value::RepoCategory(RepoCategory::Module),
                Value::Kind(Kind::Action),
            ],
        );

        assert_eq!(
            stated.values,
            BTreeSet::from([Value::RepoCategory(RepoCategory::Module)])
        );
    }

    /// A preset is taken off only where it stands. One naming an end state
    /// clears the whole of the terms it states, so taking off a preset that was
    /// never applied would drop what another control wrote over those terms.
    #[test]
    fn taking_off_a_preset_that_does_not_stand_leaves_the_filter_alone() {
        let hiding_extensions = rules::set_hidden(
            &Filter::default(),
            &Value::RepoCategory(RepoCategory::ModuleExtension),
            true,
        );
        assert!(!applied(&hiding_extensions, Preset::MainRepositoryOnly));

        assert_eq!(
            set_applied(&hiding_extensions, Preset::MainRepositoryOnly, false),
            hiding_extensions
        );
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
