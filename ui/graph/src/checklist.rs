//! The toolbar's facet checklists: the values a loaded graph offers on each
//! axis, and the options a control draws for them.
//!
//! What a checklist holds is what the graph carries, in the order its rows
//! stand. Every checkmark is derived from the panel's filter each time the
//! options are stated, so nothing here records what is drawn — see
//! [`crate::rules`].

use crate::rules::{self, Term, Value};
use crate::{GraphPanel, derive, preset};
use bviz_graph_layout::facets::WrittenRepos;
use bviz_graph_layout::graph::Graph;
use bviz_graph_layout::repo::{RepoCategory, RepoName};
use bviz_graph_layout::select::Filter;
use bviz_graph_layout::{Facets, Kind};
use bviz_ui_components::{MultiOption, MultiSelectEvent};
use gpui::Context;
use std::collections::BTreeSet;

/// The node classes `graph` holds boxes for, in the order the kinds are
/// declared, which runs from the outermost box a drawing nests to the leaves
/// inside it.
pub(crate) fn carried_classes(graph: &Graph) -> Vec<Kind> {
    let mut classes: Vec<Kind> = graph.nodes.iter().map(|node| node.kind).collect();
    classes.sort_unstable();
    classes.dedup();
    classes
}

/// How a reader names the class `kind`.
///
/// A container's row names the box the drawing puts round what it holds, since
/// hiding it unwraps that box: its contents lift to the nearest box still drawn
/// rather than leaving the graph.
fn class_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Package => "package boxes",
        Kind::Target => "target boxes",
        Kind::ConfiguredTarget => "configured-target boxes",
        Kind::Action => "actions",
        Kind::Artifact => "generated files",
        Kind::Source => "source files",
        Kind::TargetGroup => "package and environment groups",
        Kind::MergeGroup => "merged file groups",
    }
}

/// The options a class checklist draws for `classes` under `filter`.
pub(crate) fn class_options(filter: &Filter, classes: &[Kind]) -> Vec<MultiOption> {
    let hidden = rules::hidden(filter, Term::Kind);
    classes
        .iter()
        .map(|&kind| MultiOption::new(class_label(kind), !hidden.contains(&Value::Kind(kind))))
        .collect()
}

/// Every repository origin, outward from the repository the build runs in.
const ORIGINS: [RepoCategory; 5] = [
    RepoCategory::Main,
    RepoCategory::Module,
    RepoCategory::ModuleExtension,
    RepoCategory::Builtin,
    RepoCategory::WorkspaceOrUnknown,
];

fn origin_label(origin: RepoCategory) -> &'static str {
    match origin {
        RepoCategory::Main => "the main repository",
        RepoCategory::Module => "module repositories",
        RepoCategory::ModuleExtension => "module-extension repositories",
        RepoCategory::Builtin => "Bazel's built-in repository",
        RepoCategory::WorkspaceOrUnknown => "repositories of unknown origin",
    }
}

/// One repository a loaded graph names.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Repository {
    /// The name a rule names the repository by, which is what identifies it.
    name: RepoName<String>,
    /// The name a reader knows it by, as a box of that repository also reads.
    label: String,
    origin: RepoCategory,
}

/// A row of the repository checklist.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum OriginRow {
    /// Every repository of one origin, of which the graph carries at least one.
    Origin(RepoCategory),
    Repo(Repository),
}

/// The repository origins `facets` carries, each followed by the repositories
/// under it, origins outward and repositories by the name they are known by.
///
/// The main repository gets no row of its own: its origin covers exactly it, so
/// a second row would say the same thing twice.
pub(crate) fn carried_origins(facets: &Facets) -> Vec<OriginRow> {
    let strings = facets.strings();
    let written = WrittenRepos::of(facets);
    let mut repos: Vec<Repository> = facets
        .repos()
        .map(|(repo, entry)| Repository {
            name: entry.name().map(|name| strings.resolve(name).to_string()),
            label: written.written(repo).to_string(),
            origin: facets.repo_category(repo),
        })
        .collect();
    repos.sort_by(|a, b| a.label.cmp(&b.label));

    let mut rows: Vec<OriginRow> = Vec::with_capacity(repos.len() + ORIGINS.len());
    for origin in ORIGINS {
        let under: Vec<&Repository> = repos.iter().filter(|r| r.origin == origin).collect();
        if under.is_empty() {
            continue;
        }
        rows.push(OriginRow::Origin(origin));
        if origin != RepoCategory::Main {
            rows.extend(under.into_iter().cloned().map(OriginRow::Repo));
        }
    }
    rows
}

/// What a filter hides of the two terms a repository checklist's rows span.
///
/// Reading either out of a filter walks every rule and rebuilds the predicate
/// each states, so a checklist over hundreds of repositories reads both once and
/// answers for every row from these sets.
struct HiddenOrigins {
    repos: BTreeSet<RepoName<String>>,
    origins: BTreeSet<RepoCategory>,
}

impl HiddenOrigins {
    fn of(filter: &Filter) -> Self {
        HiddenOrigins {
            repos: rules::hidden(filter, Term::Repo)
                .into_iter()
                .filter_map(|value| match value {
                    Value::Repo(name) => Some(name),
                    _ => None,
                })
                .collect(),
            origins: rules::hidden(filter, Term::RepoCategory)
                .into_iter()
                .filter_map(|value| match value {
                    Value::RepoCategory(origin) => Some(origin),
                    _ => None,
                })
                .collect(),
        }
    }

    fn hides_repo(&self, repo: &Repository) -> bool {
        self.repos.contains(&repo.name)
    }

    fn hides_origin(&self, origin: RepoCategory) -> bool {
        self.origins.contains(&origin)
    }

    /// Hides the repository checklist's `row`, or shows it where `hidden` is
    /// false, holding the two terms it spans in step.
    fn set(&mut self, rows: &[OriginRow], row: &OriginRow, hidden: bool) {
        match row {
            OriginRow::Origin(origin) => {
                // The origin's own rule covers its repositories, so what the
                // repository rule said about them adds nothing either way.
                for repo in under(rows, *origin) {
                    self.repos.remove(&repo.name);
                }
                set_in(&mut self.origins, *origin, hidden);
            }
            OriginRow::Repo(repo) => {
                self.spell_out(rows, repo.origin);
                set_in(&mut self.repos, repo.name.clone(), hidden);
            }
        }
    }

    /// Restates a hidden `origin` as the repositories it covers, so that one of
    /// them can be shown again without the rest coming back with it.
    fn spell_out(&mut self, rows: &[OriginRow], origin: RepoCategory) {
        if !self.origins.remove(&origin) {
            return;
        }
        self.repos
            .extend(under(rows, origin).map(|repo| repo.name.clone()));
    }

    /// `filter` with its rules over the two terms restated over these sets.
    fn written(self, filter: &Filter) -> Filter {
        let repos = self.repos.into_iter().map(Value::Repo).collect();
        let origins = self.origins.into_iter().map(Value::RepoCategory).collect();
        let filter = rules::hiding(filter, Term::Repo, repos);
        rules::hiding(&filter, Term::RepoCategory, origins)
    }
}

fn set_in<T: Ord>(values: &mut BTreeSet<T>, value: T, held: bool) {
    match held {
        true => values.insert(value),
        false => values.remove(&value),
    };
}

/// The options a repository checklist draws for `rows` under `filter`. An origin
/// is checked only while every repository it covers is drawn, so no row stands
/// checked over something the drawing leaves out.
pub(crate) fn origin_options(filter: &Filter, rows: &[OriginRow]) -> Vec<MultiOption> {
    let hidden = HiddenOrigins::of(filter);
    rows.iter()
        .map(|row| match row {
            OriginRow::Origin(origin) => MultiOption::new(
                origin_label(*origin),
                !hidden.hides_origin(*origin)
                    && under(rows, *origin).all(|repo| !hidden.hides_repo(repo)),
            )
            .heading(),
            OriginRow::Repo(repo) => MultiOption::new(
                repo.label.clone(),
                !hidden.hides_repo(repo) && !hidden.hides_origin(repo.origin),
            ),
        })
        .collect()
}

/// The repositories `rows` carries under `origin`.
fn under(rows: &[OriginRow], origin: RepoCategory) -> impl Iterator<Item = &Repository> {
    rows.iter().filter_map(move |row| match row {
        OriginRow::Repo(repo) if repo.origin == origin => Some(repo),
        _ => None,
    })
}

/// Takes up what a freshly read graph carries on each axis and states the
/// checklists over it.
pub(crate) fn adopt(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let Some(loaded) = view.model.read(cx).loaded() else {
        return;
    };
    view.classes = carried_classes(&loaded.shared.base);
    view.origins = carried_origins(&loaded.shared.facets);
    view.presets = preset::offers(
        &loaded.shared.base,
        &loaded.shared.facets,
        &loaded.shared.capabilities,
    );
    restate(view, cx);
}

/// States every checklist's rows afresh from the filter the panel draws through.
pub(crate) fn restate(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let classes = class_options(view.derived.filter(), &view.classes);
    let origins = origin_options(view.derived.filter(), &view.origins);
    let presets = preset::options(view.derived.filter(), &view.presets);
    view.class_filter
        .update(cx, |control, cx| control.set_options(classes, cx));
    view.origin_filter
        .update(cx, |control, cx| control.set_options(origins, cx));
    view.preset_filter
        .update(cx, |control, cx| control.set_options(presets, cx));
}

/// Draws the graph through `filter` from here on and restates the checklists
/// over it, so a rule and the checkmark reading it move together.
pub(crate) fn set_filter(view: &mut GraphPanel, filter: Filter, cx: &mut Context<GraphPanel>) {
    derive::set_filter(view, filter, cx);
    restate(view, cx);
}

/// As [`set_filter`], for a filter set by a gesture that named what to look at:
/// the camera moves onto what the filter leaves drawn.
pub(crate) fn set_filter_framed(
    view: &mut GraphPanel,
    filter: Filter,
    cx: &mut Context<GraphPanel>,
) {
    derive::set_filter_framed(view, filter, cx);
    restate(view, cx);
}

/// The filter a click on a checklist leaves, `row` writing the one the row at an
/// index stands at when asked to become checked or unchecked.
///
/// A click over a range writes every row it covers through that same writer, so
/// it leaves the rules the run of single clicks would have left — including the
/// rules no control wrote, which each of those writes carries through untouched.
fn chosen(
    filter: &Filter,
    event: &MultiSelectEvent,
    row: impl Fn(&Filter, usize, bool) -> Filter,
) -> Filter {
    acted(event).fold(filter.clone(), |filter, (index, checked)| {
        row(&filter, index, checked)
    })
}

/// The rows `event` acts on, each with what it asks that row to become.
fn acted(event: &MultiSelectEvent) -> impl Iterator<Item = (usize, bool)> {
    let (rows, checked) = match event {
        MultiSelectEvent::Toggled { index, checked } => (std::slice::from_ref(index), *checked),
        MultiSelectEvent::SetAll { rows, checked } => (rows.as_slice(), *checked),
    };
    rows.iter().map(move |&index| (index, checked))
}

/// `filter` with the node classes the class checklist's `event` names drawn, or
/// left out.
pub(crate) fn classes_chosen(
    filter: &Filter,
    classes: &[Kind],
    event: &MultiSelectEvent,
) -> Filter {
    chosen(filter, event, |filter, index, checked| {
        match classes.get(index) {
            Some(&kind) => rules::set_hidden(filter, &Value::Kind(kind), !checked),
            None => filter.clone(),
        }
    })
}

/// `filter` with the repositories the repository checklist's `event` names
/// drawn, or left out.
pub(crate) fn origins_chosen(
    filter: &Filter,
    rows: &[OriginRow],
    event: &MultiSelectEvent,
) -> Filter {
    let mut hiding = HiddenOrigins::of(filter);
    for (index, checked) in acted(event) {
        if let Some(row) = rows.get(index) {
            hiding.set(rows, row, !checked);
        }
    }
    hiding.written(filter)
}

/// `filter` with the reductions the preset checklist's `event` names applied, or
/// taken back off. A preset the loaded inputs cannot answer is left alone, so a
/// click over every row reaches exactly the rows that report one.
pub(crate) fn presets_chosen(
    filter: &Filter,
    offers: &[preset::Offer],
    event: &MultiSelectEvent,
) -> Filter {
    chosen(
        filter,
        event,
        |filter, index, applied| match preset::offered(offers, index) {
            Some(preset) => preset::set_applied(filter, preset, applied),
            None => filter.clone(),
        },
    )
}

/// Draws the graph as the class checklist's click leaves it.
pub(crate) fn choose_classes(
    view: &mut GraphPanel,
    event: &MultiSelectEvent,
    cx: &mut Context<GraphPanel>,
) {
    let filter = classes_chosen(view.derived.filter(), &view.classes, event);
    set_filter(view, filter, cx);
}

/// Draws the graph as the repository checklist's click leaves it.
pub(crate) fn choose_origins(
    view: &mut GraphPanel,
    event: &MultiSelectEvent,
    cx: &mut Context<GraphPanel>,
) {
    let filter = origins_chosen(view.derived.filter(), &view.origins, event);
    set_filter(view, filter, cx);
}

/// Draws the graph as the preset checklist's click leaves it.
pub(crate) fn choose_presets(
    view: &mut GraphPanel,
    event: &MultiSelectEvent,
    cx: &mut Context<GraphPanel>,
) {
    let filter = presets_chosen(view.derived.filter(), &view.presets, event);
    set_filter(view, filter, cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_graph_layout::graph::Node;
    use bviz_graph_layout::intern::Id;
    use bviz_graph_layout::select::pattern::Pattern;
    use bviz_graph_layout::select::{Predicate, Rule, Visibility};

    /// A graph of one package holding one target, with a source file beside it.
    fn graph() -> Graph {
        let mut graph = Graph::new();
        graph.add_node(Node::new(Id(0), "//foo", Kind::Package, None));
        graph.add_node(Node::new(Id(1), ":app", Kind::Target, Some(Id(0))));
        graph.add_node(Node::new(Id(2), ":main.c", Kind::Source, Some(Id(0))));
        graph
    }

    /// The facets of a graph naming the main repository, two modules, and one
    /// repository a module extension generated.
    fn facets() -> Facets {
        let mut facets = Facets::default();
        facets.intern_repo("");
        facets.intern_repo("rules_foo+");
        facets.intern_repo("rules_bar+");
        facets.intern_repo("rules_foo++ext+generated");
        facets
    }

    /// `filter` with the repository checklist's `row` hidden, or shown where
    /// `hidden` is false, which is what one click on that row leaves.
    fn set_origin_hidden(
        filter: &Filter,
        rows: &[OriginRow],
        row: &OriginRow,
        hidden: bool,
    ) -> Filter {
        let mut hiding = HiddenOrigins::of(filter);
        hiding.set(rows, row, hidden);
        hiding.written(filter)
    }

    fn labels(options: &[MultiOption]) -> Vec<&str> {
        options.iter().map(|option| option.label.as_ref()).collect()
    }

    fn checked(options: &[MultiOption]) -> Vec<bool> {
        options.iter().map(|option| option.checked).collect()
    }

    /// A class no node carries is nothing to offer, so the checklist lists the
    /// classes the graph holds boxes for and no others.
    #[test]
    fn only_the_classes_the_graph_carries_are_offered() {
        assert_eq!(
            carried_classes(&graph()),
            [Kind::Package, Kind::Target, Kind::Source]
        );
    }

    /// The checkmarks are read out of the rules, so a filter built without the
    /// control renders exactly as that filter says.
    #[test]
    fn a_class_row_is_checked_by_the_rules_alone() {
        let classes = carried_classes(&graph());
        let hiding = rules::set_hidden(&Filter::default(), &Value::Kind(Kind::Target), true);

        assert_eq!(
            checked(&class_options(&Filter::default(), &classes)),
            [true; 3]
        );
        assert_eq!(
            checked(&class_options(&hiding, &classes)),
            [true, false, true]
        );
    }

    /// A container's row names the box it draws, since hiding it lifts what it
    /// holds to the nearest box still drawn.
    #[test]
    fn a_container_class_is_named_by_the_box_it_draws() {
        let classes = carried_classes(&graph());
        assert_eq!(
            labels(&class_options(&Filter::default(), &classes)),
            ["package boxes", "target boxes", "source files"]
        );
    }

    /// An origin with no repository behind it is nothing to offer, and neither
    /// is a repository no input named. A repository is named the way a reader
    /// writes it, which for a module is its module name whether or not an input
    /// wrote that name, and a canonical name only where nothing else is known.
    #[test]
    fn only_the_repositories_the_graph_carries_are_offered() {
        let rows = carried_origins(&facets());

        assert_eq!(
            labels(&origin_options(&Filter::default(), &rows)),
            [
                "the main repository",
                "module repositories",
                "@rules_bar",
                "@rules_foo",
                "module-extension repositories",
                "@@rules_foo++ext+generated",
            ]
        );
    }

    /// A repository the inputs named only apparently is offered like any other.
    /// Its name states nothing of where it comes from, so it stands under the
    /// origin that says exactly that rather than one guessed from the name.
    #[test]
    fn a_repository_named_only_apparently_is_offered_under_an_unknown_origin() {
        let mut facets = Facets::default();
        facets.intern_repo("");
        facets.intern_apparent_repo("rules_foo");

        let rows = carried_origins(&facets);

        assert_eq!(
            labels(&origin_options(&Filter::default(), &rows)),
            [
                "the main repository",
                "repositories of unknown origin",
                "@rules_foo",
            ]
        );
    }

    /// A canonical name in the same graph naming the module an apparent name
    /// reads as is evidence that the module exists, which lists the repository
    /// under the module origin while leaving it a row of its own.
    #[test]
    fn a_repository_named_apparently_after_a_module_the_graph_names_is_listed_under_it() {
        let mut facets = Facets::default();
        facets.intern_repo("");
        facets.intern_apparent_repo("llvm");
        facets.intern_repo("llvm++llvm+llvm-project");

        let rows = carried_origins(&facets);

        assert_eq!(
            labels(&origin_options(&Filter::default(), &rows)),
            [
                "the main repository",
                "module repositories",
                "@llvm",
                "module-extension repositories",
                "@@llvm++llvm+llvm-project",
            ]
        );
    }

    /// An apparent name and a canonical one reading alike are two repositories,
    /// so each has its own row and hiding one leaves the other drawn.
    #[test]
    fn a_repository_row_hides_only_the_repository_it_names() {
        let mut facets = Facets::default();
        facets.intern_apparent_repo("somerepo");
        facets.intern_repo("somerepo");

        let rows = carried_origins(&facets);
        assert_eq!(
            labels(&origin_options(&Filter::default(), &rows)),
            ["repositories of unknown origin", "@@somerepo", "@somerepo"]
        );

        let hiding = set_origin_hidden(&Filter::default(), &rows, &rows[2], true);

        assert_eq!(
            checked(&origin_options(&hiding, &rows)),
            [false, true, false]
        );
    }

    /// The origin covering a hidden repository stands unchecked, or it would
    /// claim to draw something the drawing leaves out.
    #[test]
    fn an_origin_is_checked_only_while_every_repository_under_it_is_drawn() {
        let rows = carried_origins(&facets());
        let hiding = set_origin_hidden(&Filter::default(), &rows, &rows[2], true);

        assert_eq!(
            checked(&origin_options(&hiding, &rows)),
            [true, false, false, true, true, true]
        );
    }

    /// Hiding an origin hides every repository under it, and showing one of them
    /// again leaves the rest hidden rather than bringing the whole origin back.
    #[test]
    fn showing_one_repository_of_a_hidden_origin_leaves_the_rest_hidden() {
        let rows = carried_origins(&facets());
        let modules = rows
            .iter()
            .position(|row| *row == OriginRow::Origin(RepoCategory::Module))
            .expect("the fixture carries module repositories");

        let hidden = set_origin_hidden(&Filter::default(), &rows, &rows[modules], true);
        assert_eq!(
            checked(&origin_options(&hidden, &rows)),
            [true, false, false, false, true, true]
        );

        let one_back = set_origin_hidden(&hidden, &rows, &rows[modules + 1], false);
        assert_eq!(
            checked(&origin_options(&one_back, &rows)),
            [true, false, true, false, true, true]
        );
    }

    fn set_all(rows: std::ops::Range<usize>, checked: bool) -> MultiSelectEvent {
        MultiSelectEvent::SetAll {
            rows: rows.collect(),
            checked,
        }
    }

    /// The capabilities of inputs stating every axis a preset reads.
    fn stating_every_axis() -> bviz_graph_layout::Capabilities {
        let mut capabilities = bviz_graph_layout::capability::CapabilitiesBuilder::default();
        for axis in bviz_graph_layout::Axis::ALL {
            capabilities.observe(axis, true);
        }
        capabilities.seal()
    }

    /// The presets offered over the fixtures, which carry repositories and one
    /// rule class but no configuration and no action — so the presets over the
    /// first two axes are on offer and the other two are withheld.
    fn offered_presets() -> Vec<preset::Offer> {
        let mut facets = facets();
        facets.intern_rule_class("cc_library");
        preset::offers(&graph(), &facets, &stating_every_axis())
    }

    /// Clearing the menu leaves nothing drawn, and checking one repository after
    /// it brings back that repository alone — which is how a graph naming
    /// hundreds is narrowed to the one being looked at.
    #[test]
    fn clearing_every_repository_and_checking_one_draws_that_one_alone() {
        let rows = carried_origins(&facets());
        let cleared = origins_chosen(&Filter::default(), &rows, &set_all(0..rows.len(), false));
        assert_eq!(checked(&origin_options(&cleared, &rows)), [false; 6]);

        let one = origins_chosen(
            &cleared,
            &rows,
            &MultiSelectEvent::Toggled {
                index: 3,
                checked: true,
            },
        );

        assert_eq!(
            checked(&origin_options(&one, &rows)),
            [false, false, false, true, false, false]
        );
    }

    /// The preset naming the main repository draws it however the checklist was
    /// left, so clearing every repository and then asking for that repository
    /// alone leaves it drawn and the checklist reading it. Taking the preset
    /// back off lifts the restriction it stated rather than half of it.
    #[test]
    fn clearing_every_repository_and_applying_the_main_repository_preset_draws_it() {
        let rows = carried_origins(&facets());
        let cleared = origins_chosen(&Filter::default(), &rows, &set_all(0..rows.len(), false));

        let only_main = preset::set_applied(&cleared, preset::Preset::MainRepositoryOnly, true);

        assert_eq!(
            checked(&origin_options(&only_main, &rows)),
            [true, false, false, false, false, false]
        );
        assert_eq!(
            checked(&origin_options(
                &preset::set_applied(&only_main, preset::Preset::MainRepositoryOnly, false),
                &rows
            )),
            [true; 6]
        );
    }

    /// The checklist acts on whatever the preset left, so clearing every row
    /// after the preset leaves nothing drawn and the preset no longer applied.
    #[test]
    fn clearing_every_repository_after_the_main_repository_preset_leaves_nothing_drawn() {
        let rows = carried_origins(&facets());
        let only_main =
            preset::set_applied(&Filter::default(), preset::Preset::MainRepositoryOnly, true);

        let cleared = origins_chosen(&only_main, &rows, &set_all(0..rows.len(), false));

        assert_eq!(checked(&origin_options(&cleared, &rows)), [false; 6]);
        assert!(!preset::applied(
            &cleared,
            preset::Preset::MainRepositoryOnly
        ));
    }

    /// A group's own actions reach the repositories under it and no others, so
    /// one origin is cleared while the rest stay drawn.
    #[test]
    fn clearing_a_group_leaves_the_repositories_of_every_other_origin_drawn() {
        let rows = carried_origins(&facets());
        let modules = rows
            .iter()
            .position(|row| *row == OriginRow::Origin(RepoCategory::Module))
            .expect("the fixture carries module repositories");

        let cleared = origins_chosen(
            &Filter::default(),
            &rows,
            &set_all(modules..modules + 3, false),
        );

        assert_eq!(
            checked(&origin_options(&cleared, &rows)),
            [true, false, false, false, true, true]
        );
    }

    /// A rule no control wrote is none of a checklist's business, so a click
    /// over every row carries it through exactly as one row's click does.
    #[test]
    fn checking_and_clearing_every_row_leaves_a_rule_no_control_wrote_standing() {
        let untouched = Rule {
            predicate: Predicate {
                label: Some(Pattern::parse("//foo/...")),
                ..Predicate::default()
            },
            visibility: Visibility::Hidden,
        };
        let filter = Filter {
            rules: vec![untouched.clone()],
            ..Filter::default()
        };
        let rows = carried_origins(&facets());

        for checked in [false, true] {
            let acted = origins_chosen(&filter, &rows, &set_all(0..rows.len(), checked));
            assert!(acted.rules.contains(&untouched), "checked: {checked}");
        }
    }

    /// The preset checklist's rows are reductions rather than values, so its
    /// menu-wide actions apply and take back off every reduction the loaded
    /// inputs can answer, and leave the rest alone.
    #[test]
    fn applying_every_preset_reaches_the_ones_the_inputs_can_answer() {
        let offers = offered_presets();
        let all = set_all(0..offers.len(), true);

        let applied = presets_chosen(&Filter::default(), &offers, &all);

        assert_eq!(
            checked(&preset::options(&applied, &offers)),
            [true, true, false, false]
        );
        assert_eq!(
            presets_chosen(&applied, &offers, &set_all(0..offers.len(), false)),
            Filter::default()
        );
    }

    /// A menu-wide clear over the presets takes off the presets standing and
    /// nothing else. A preset the reader never applied owns none of the rules
    /// over the terms it would state, so the repositories the repository
    /// checklist left out stay out through it.
    #[test]
    fn clearing_every_preset_leaves_what_the_repository_checklist_hid() {
        let rows = carried_origins(&facets());
        let extensions = rows
            .iter()
            .position(|row| *row == OriginRow::Origin(RepoCategory::ModuleExtension))
            .expect("the fixture carries module-extension repositories");
        let hiding = origins_chosen(
            &Filter::default(),
            &rows,
            &MultiSelectEvent::Toggled {
                index: extensions,
                checked: false,
            },
        );
        let offers = offered_presets();
        assert!(!preset::applied(
            &hiding,
            preset::Preset::MainRepositoryOnly
        ));

        let cleared = presets_chosen(&hiding, &offers, &set_all(0..offers.len(), false));

        assert_eq!(
            checked(&origin_options(&cleared, &rows)),
            [true, true, true, true, false, false]
        );
    }

    /// Hiding every repository the graph carries and showing them again leaves
    /// no rule behind, so the checklists start and end at the same filter.
    #[test]
    fn hiding_and_showing_every_row_again_leaves_the_filter_as_it_was() {
        let rows = carried_origins(&facets());
        let mut filter = Filter::default();
        for row in &rows {
            filter = set_origin_hidden(&filter, &rows, row, true);
        }
        assert_eq!(checked(&origin_options(&filter, &rows)), [false; 6]);

        for row in &rows {
            filter = set_origin_hidden(&filter, &rows, row, false);
        }
        assert_eq!(filter, Filter::default());
    }
}
