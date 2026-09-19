//! The toolbar's facet checklists: the values a loaded graph offers on each
//! axis, and the options a control draws for them.
//!
//! What a checklist holds is what the graph carries, in the order its rows
//! stand. Every checkmark is derived from the panel's filter each time the
//! options are stated, so nothing here records what is drawn — see
//! [`crate::rules`].

use crate::rules::{self, Term, Value};
use crate::{GraphPanel, derive, preset};
use bviz_graph_layout::graph::Graph;
use bviz_graph_layout::repo::{RepoCategory, RepoName};
use bviz_graph_layout::select::Filter;
use bviz_graph_layout::{Facets, Kind};
use bviz_ui_components::{MultiOption, MultiSelectEvent};
use gpui::Context;

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
    classes
        .iter()
        .map(|&kind| {
            let hidden = rules::hides(filter, &Value::Kind(kind));
            MultiOption::new(class_label(kind), !hidden)
        })
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
    let mut repos: Vec<Repository> = facets
        .repos()
        .map(|(repo, entry)| Repository {
            name: entry.name().map(|name| strings.resolve(name).to_string()),
            label: facets.repo_written(repo),
            origin: entry.origin().category(),
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

/// The options a repository checklist draws for `rows` under `filter`. An origin
/// is checked only while every repository it covers is drawn, so no row stands
/// checked over something the drawing leaves out.
pub(crate) fn origin_options(filter: &Filter, rows: &[OriginRow]) -> Vec<MultiOption> {
    rows.iter()
        .map(|row| match row {
            OriginRow::Origin(origin) => MultiOption::new(
                origin_label(*origin),
                !rules::hides(filter, &Value::RepoCategory(*origin))
                    && under(rows, *origin).all(|repo| !hides_repo(filter, repo)),
            )
            .heading(),
            OriginRow::Repo(repo) => MultiOption::new(
                repo.label.clone(),
                !hides_repo(filter, repo)
                    && !rules::hides(filter, &Value::RepoCategory(repo.origin)),
            ),
        })
        .collect()
}

/// `filter` with the repository checklist's `row` hidden, or shown where
/// `hidden` is false, holding the two terms it spans in step.
pub(crate) fn set_origin_hidden(
    filter: &Filter,
    rows: &[OriginRow],
    row: &OriginRow,
    hidden: bool,
) -> Filter {
    match row {
        OriginRow::Origin(origin) => {
            // The origin's own rule covers its repositories, so what the
            // repository rule said about them adds nothing either way.
            let filter = showing(filter, under(rows, *origin));
            rules::set_hidden(&filter, &Value::RepoCategory(*origin), hidden)
        }
        OriginRow::Repo(repo) => {
            let filter = spelled_out(filter, rows, repo.origin);
            rules::set_hidden(&filter, &Value::Repo(repo.name.clone()), hidden)
        }
    }
}

fn hides_repo(filter: &Filter, repo: &Repository) -> bool {
    rules::hides(filter, &Value::Repo(repo.name.clone()))
}

/// The repositories `rows` carries under `origin`.
fn under(rows: &[OriginRow], origin: RepoCategory) -> impl Iterator<Item = &Repository> {
    rows.iter().filter_map(move |row| match row {
        OriginRow::Repo(repo) if repo.origin == origin => Some(repo),
        _ => None,
    })
}

/// `filter` with each of `repos` dropped from what the repository term hides.
fn showing<'a>(filter: &Filter, repos: impl Iterator<Item = &'a Repository>) -> Filter {
    let mut hidden = rules::hidden(filter, Term::Repo);
    for repo in repos {
        hidden.remove(&Value::Repo(repo.name.clone()));
    }
    rules::hiding(filter, Term::Repo, hidden)
}

/// `filter` with a hidden `origin` restated as the repositories it covers, so
/// that one of them can be shown again without the rest coming back with it;
/// `filter` itself where the origin is not hidden.
fn spelled_out(filter: &Filter, rows: &[OriginRow], origin: RepoCategory) -> Filter {
    let category = Value::RepoCategory(origin);
    if !rules::hides(filter, &category) {
        return filter.clone();
    }
    let mut hidden = rules::hidden(filter, Term::Repo);
    hidden.extend(under(rows, origin).map(|repo| Value::Repo(repo.name.clone())));
    let filter = rules::hiding(filter, Term::Repo, hidden);
    rules::set_hidden(&filter, &category, false)
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
    match event {
        MultiSelectEvent::Toggled { index, checked } => row(filter, *index, *checked),
        MultiSelectEvent::SetAll { range, checked } => {
            range.clone().fold(filter.clone(), |filter, index| {
                row(&filter, index, *checked)
            })
        }
    }
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
    chosen(filter, event, |filter, index, checked| {
        match rows.get(index) {
            Some(row) => set_origin_hidden(filter, rows, row, !checked),
            None => filter.clone(),
        }
    })
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
        facets.intern_repo("", None);
        facets.intern_repo("rules_foo+", Some("rules_foo"));
        facets.intern_repo("rules_bar+", None);
        facets.intern_repo("rules_foo++ext+generated", None);
        facets
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
        facets.intern_repo("", None);
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

    /// An apparent name and a canonical one reading alike are two repositories,
    /// so each has its own row and hiding one leaves the other drawn.
    #[test]
    fn a_repository_row_hides_only_the_repository_it_names() {
        let mut facets = Facets::default();
        facets.intern_apparent_repo("somerepo");
        facets.intern_repo("somerepo", None);

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

    fn set_all(range: std::ops::Range<usize>, checked: bool) -> MultiSelectEvent {
        MultiSelectEvent::SetAll { range, checked }
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
        let mut graph = Graph::new();
        graph.add_node(Node::new(Id(0), ":lib", Kind::Target, None));
        let mut facets = Facets::default();
        facets.intern_rule_class("cc_library");
        let mut capabilities = bviz_graph_layout::capability::CapabilitiesBuilder::default();
        for axis in bviz_graph_layout::Axis::ALL {
            capabilities.observe(axis, true);
        }
        let offers = preset::offers(&graph, &facets, &capabilities.seal());
        let all = set_all(0..offers.len(), true);

        let applied = presets_chosen(&Filter::default(), &offers, &all);

        let options = preset::options(&applied, &offers);
        assert_eq!(checked(&options), [true, false, false, false]);
        assert_eq!(
            presets_chosen(&applied, &offers, &set_all(0..offers.len(), false)),
            Filter::default()
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
