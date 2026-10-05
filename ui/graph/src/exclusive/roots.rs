//! The field naming the roots of a dominator tree as a Bazel target set, and
//! the points of a unit such a set roots the tree at.

use super::{ExclusivePanel, LeaveRoots, RootedBy};
use crate::Palette;
use crate::model::{GraphModel, Prepared};
use crate::point::{point_label, unit_names};
use bviz_graph_layout::Support;
use bviz_graph_layout::dominators::{Actions, Chosen, ConfiguredTargets, ScopeGraph};
use bviz_graph_layout::metrics::{ByUnit, Scope, Unit};
use bviz_graph_layout::target_pattern::{Resolved, TargetIndex, TargetPattern};
use bviz_graph_layout::target_set::{PatternError, TargetSet, TopLevel};
use bviz_ui_components::{QueryField, TextTooltip, TooltipColors};
use gpui::{Context, Div, Entity, Pixels, Stateful, div, prelude::*, px};

/// The key context the roots field carries, by which the panel's own
/// single-key bindings stand down while a target set is typed into it.
pub(crate) const KEY_CONTEXT: &str = "GraphExclusiveRoots";

/// Width the field is drawn at, wide enough for its placeholder.
pub(super) const FIELD_WIDTH: Pixels = px(380.);

pub(super) const PLACEHOLDER: &str = "roots: top-level, or targets: //... -//tools/...";

/// What the field's tooltip says it takes.
const HELP: &str = "\
Roots the tree at what a bazel build of this target set requests.
Patterns are separated by spaces; one starting with - takes its
targets out, and # starts a comment.
Patterns are read as typed at the workspace root.
A wildcard leaves out the rules tagged manual.
A test_suite stands for itself, not for its tests.
Not applied: .bazelignore, --deleted_packages,
--build_tag_filters, --build_tests_only, and the skipping
of targets incompatible with the platform.
Empty, the tree is rooted at every top-level point.";

/// What the points a target set roots a tree at are picked from, by the
/// unit of the tree. Each graph is that of a scope of the unit its place
/// names, [`Within::of`] alone making one.
pub(super) struct Within(Graphs);

enum Graphs {
    Targets,
    /// The graph's configured targets.
    Copies(Prepared<ScopeGraph>),
    Actions {
        /// The configured targets holding the actions.
        configured: Prepared<ScopeGraph>,
        actions: Prepared<ScopeGraph>,
    },
}

/// The graphs of a [`Within`] as what their units make them.
enum Views<'a> {
    Targets,
    Copies(ConfiguredTargets<'a>),
    Actions {
        configured: ConfiguredTargets<'a>,
        actions: Actions<'a>,
    },
}

impl Within {
    /// What the roots of a tree of `scope` are picked from; absent while the
    /// graph `model` holds is still being read.
    pub(super) fn of(scope: Scope, model: &mut GraphModel) -> Option<Self> {
        Some(Within(match scope.by_unit() {
            ByUnit::Target => Graphs::Targets,
            ByUnit::ConfiguredTarget { .. } => Graphs::Copies(model.scope_graph(scope)?),
            ByUnit::Action { configured_targets } => Graphs::Actions {
                configured: model.scope_graph(configured_targets)?,
                actions: model.scope_graph(scope)?,
            },
        }))
    }

    fn views(&self) -> Views<'_> {
        let views = match &self.0 {
            Graphs::Targets => Some(Views::Targets),
            Graphs::Copies(configured) => configured.get().configured_targets().map(Views::Copies),
            Graphs::Actions {
                configured,
                actions,
            } => (configured.get().configured_targets())
                .zip(actions.get().actions())
                .map(|(configured, actions)| Views::Actions {
                    configured,
                    actions,
                }),
        };
        views.expect("a graph of the unit its place in `Within` names")
    }
}

/// The roots a target set names, and what they leave unsaid.
pub(super) struct Found {
    pub(super) roots: Chosen,
    /// The targets the set names, which root the tree of targets.
    pub(super) named: Chosen,
    pub(super) notices: Vec<String>,
}

/// The points `within` the target set `text` roots a tree at: the targets it
/// names among `targets`, their top-level copies, or the actions a build of
/// those copies starts from.
pub(super) fn find(
    text: &str,
    targets: &Prepared<TargetIndex>,
    within: &Within,
) -> Result<Found, PatternError> {
    let set = TargetSet::parse(text)?;
    let index = targets.get();
    let Resolved {
        targets: named,
        left_out,
    } = index.resolve(&set)?;
    let mut notices: Vec<String> = Vec::new();
    let shared = targets.shared();
    let left_out = left_out.iter().map(|&id| point_label(id, shared));
    notices.extend(manual_left_out(left_out.collect()));
    notices.extend(manual_untold(&set, index.tags()).map(str::to_owned));
    let (base, facets) = (&shared.base, &shared.facets);
    let top = |configured| TopLevel::of(&named, base, facets, configured);
    let (points, top) = match within.views() {
        Views::Targets => (named.clone(), None),
        Views::Copies(configured) => {
            let top = top(configured);
            (top.copies().to_vec(), Some(top))
        }
        Views::Actions {
            configured,
            actions,
        } => {
            let top = top(configured);
            (top.actions(base, configured, actions), Some(top))
        }
    };
    if let Some(top) = &top {
        notices.extend(copies_noted(top.guessed(), top.generated(), top.uncopied()));
    }
    Ok(Found {
        roots: Chosen::new(points),
        named: Chosen::new(named),
        notices,
    })
}

/// The most manual targets the field names as left out by a wildcard.
const NAMED_MANUAL: usize = 3;

/// What the field notes of the manual targets the set's wildcards left out,
/// labelled `left_out`, naming the first few; nothing where there are none.
/// The targets a bare package names come first, being the ones a reader
/// names without a wildcard and then misses under one.
fn manual_left_out(mut left_out: Vec<String>) -> Option<String> {
    let count = left_out.len();
    let (targets, name_them) = match count {
        0 => return None,
        1 => ("target", "name it to include it"),
        _ => ("targets", "name them to include them"),
    };
    left_out.sort_by_key(|label| !names_its_package(label));
    left_out.truncate(NAMED_MANUAL);
    let more = if left_out.len() < count { ", …" } else { "" };
    let named = left_out.join(", ");
    Some(format!(
        "{count} manual {targets} left out by wildcards: {named}{more} — {name_them}"
    ))
}

/// Whether `label` is the target the bare name of its package stands for,
/// as `//foo/bar:bar` is what `//foo/bar` names, and `@repo//:repo` what
/// `@repo` does.
fn names_its_package(label: &str) -> bool {
    let Some((package, name)) = label.rsplit_once(':') else {
        return true;
    };
    let last = match package.strip_suffix("//") {
        Some(repo) => repo.trim_start_matches('@'),
        None => package.rsplit_once('/').map_or(package, |(_, last)| last),
    };
    last == name
}

/// What the field notes of `set` where `tags`, how far the inputs state the
/// tags of their rules, lets a wildcard of it take in a rule tagged manual:
/// nothing where no wildcard adds targets or every rule states its tags.
fn manual_untold(set: &TargetSet, tags: Support) -> Option<&'static str> {
    if !leaves_out_manual(set) {
        return None;
    }
    match tags {
        Support::Available => None,
        Support::Partial { .. } => Some(MANUAL_PARTLY_TOLD),
        Support::Unavailable { .. } => Some(MANUAL_UNTOLD),
    }
}

const MANUAL_PARTLY_TOLD: &str = "some rules state no tags, so wildcards may take in manual ones";

const MANUAL_UNTOLD: &str = "manual targets can't be told apart in this input";

/// Whether `set` holds a pattern that leaves out the rules tagged manual: a
/// wildcard adding its targets.
fn leaves_out_manual(set: &TargetSet) -> bool {
    set.patterns().iter().any(|signed| {
        let wildcard = matches!(
            signed.pattern(),
            TargetPattern::InPackage { .. } | TargetPattern::Below { .. }
        );
        wildcard && !signed.negative()
    })
}

/// What the field notes of the targets whose top-level copies were `guessed`,
/// and of those that have none: the `generated` files and the other
/// `uncopied` targets.
fn copies_noted(guessed: u32, generated: u32, uncopied: u32) -> impl Iterator<Item = String> {
    let guessed = match guessed {
        0 => None,
        1 => Some("1 target's top-level copy guessed".to_owned()),
        _ => Some(format!("{guessed} targets' top-level copies guessed")),
    };
    let generated = match generated {
        0 => None,
        1 => Some("1 generated file can't be traced to its rule in this input".to_owned()),
        _ => Some(format!(
            "{generated} generated files can't be traced to their rules in this input"
        )),
    };
    let uncopied = (uncopied > 0).then(|| {
        let have = if uncopied == 1 { "has" } else { "have" };
        let targets = unit_names(Unit::Target).counted(uncopied);
        format!("{targets} {have} no configured copy")
    });
    guessed.into_iter().chain(generated).chain(uncopied)
}

/// The roots field `input`, from which Escape returns to the panel.
pub(super) fn field(
    input: &Entity<QueryField>,
    palette: &Palette,
    cx: &Context<ExclusivePanel>,
) -> Stateful<Div> {
    let tip = TooltipColors::from_colors(&palette.colors);
    div()
        .id("exclusive-roots")
        .key_context(KEY_CONTEXT)
        .on_action(cx.listener(|view, _: &LeaveRoots, window, cx| {
            view.focus_handle.focus(window, cx);
        }))
        .tooltip(TextTooltip::builder(HELP.into(), tip))
        .flex_none()
        .child(input.clone())
}

/// The line under the strip saying that the target set the field holds
/// failed with `error`, and what the tree drawn is rooted `by` in its place.
pub(super) fn failure_line(error: &PatternError, by: Option<RootedBy>, palette: &Palette) -> Div {
    line(palette)
        .text_color(palette.colors.error)
        .child(failure(&error.to_string(), by))
}

/// The line under the strip reading `notices`, what the roots chosen leave
/// unsaid of the target set that chose them.
pub(super) fn notices_line(notices: &[String], palette: &Palette) -> Div {
    line(palette)
        .text_color(palette.colors.text_muted)
        .child(notices.join(NOTICE_SEPARATOR))
}

fn line(palette: &Palette) -> Div {
    let colors = &palette.colors;
    div()
        .flex_none()
        .px_2()
        .py_1()
        .text_sm()
        .bg(colors.surface)
        .border_b_1()
        .border_color(colors.border)
}

/// What the line reads of a set that failed with `error` while the tree
/// drawn is rooted `by` what stands in for it.
fn failure(error: &str, by: Option<RootedBy>) -> String {
    match by {
        Some(RootedBy::Targets) => format!("{error} — showing the last valid set"),
        Some(RootedBy::Tops) => format!("{error} — showing the whole build"),
        None => error.to_owned(),
    }
}

const NOTICE_SEPARATOR: &str = " · ";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_guessed_and_missing_are_noted_only_where_there_are_any() {
        let noted = |guessed, generated, uncopied| {
            copies_noted(guessed, generated, uncopied).collect::<Vec<_>>()
        };
        assert!(noted(0, 0, 0).is_empty());
        assert_eq!(noted(1, 0, 0), ["1 target's top-level copy guessed"]);
        let all = [
            "3 targets' top-level copies guessed",
            "2 generated files can't be traced to their rules in this input",
            "1 target has no configured copy",
        ];
        assert_eq!(noted(3, 2, 1), all);
        assert_eq!(noted(0, 0, 2), ["2 targets have no configured copy"]);
        assert_eq!(
            noted(0, 1, 0),
            ["1 generated file can't be traced to its rule in this input"]
        );
    }

    #[test]
    fn the_manual_targets_left_out_are_counted_and_the_first_named() {
        let noted = |labels: &[&str]| {
            manual_left_out(labels.iter().map(|label| label.to_string()).collect())
        };
        assert_eq!(noted(&[]), None);
        assert_eq!(
            noted(&["//dist:dist"]).unwrap(),
            "1 manual target left out by wildcards: //dist:dist — name it to include it"
        );
        assert_eq!(
            noted(&["//a:x", "//b:y"]).unwrap(),
            "2 manual targets left out by wildcards: //a:x, //b:y — name them to include them"
        );
        assert_eq!(
            noted(&["//a:w", "//a:x", "//a:y", "//a:z"]).unwrap(),
            "4 manual targets left out by wildcards: //a:w, //a:x, //a:y, … — name them to \
             include them"
        );
    }

    #[test]
    fn the_manual_targets_a_bare_package_names_are_named_first() {
        let labels = [
            "//dist:a",
            "//dist:b",
            "//dist:c",
            "//dist:dist",
            "//tools/env:env",
        ];
        let noted = manual_left_out(labels.map(str::to_owned).to_vec()).unwrap();
        assert_eq!(
            noted,
            "5 manual targets left out by wildcards: //dist:dist, //tools/env:env, //dist:a, … — \
             name them to include them"
        );
        for named_bare in ["//dist", "//dist:dist", "@repo//:repo", "@repo//a/b:b"] {
            assert!(names_its_package(named_bare), "{named_bare}");
        }
        for named_whole in ["//dist:all", "//:dist", "@repo//:all", "//a/b:a"] {
            assert!(!names_its_package(named_whole), "{named_whole}");
        }
    }

    /// Only a wildcard adding targets leaves the manual ones out, so only a
    /// set holding one can let manual targets through unnoticed.
    #[test]
    fn only_a_wildcard_adding_targets_leaves_out_manual_ones() {
        let leaves_out = |text: &str| leaves_out_manual(&TargetSet::parse(text).unwrap());
        assert!(leaves_out("//..."));
        assert!(leaves_out("//a:b //c:all"));
        assert!(leaves_out("//a:*"));
        assert!(!leaves_out("//a:b //c"));
        assert!(!leaves_out("//a:b -//c/... -//d:all"));
        assert!(!leaves_out("a/b.txt"));
    }

    /// A wildcard takes a rule stating no tags for one carrying none, which
    /// the field says as far as it holds: of some rules, or of all.
    #[test]
    fn manual_targets_a_wildcard_may_take_in_are_noted_as_far_as_tags_go_unstated() {
        let noted = |text: &str, tags| manual_untold(&TargetSet::parse(text).unwrap(), tags);
        let partial = Support::Partial {
            covered: 1,
            relevant: 2,
            flag: None,
        };
        let unavailable = Support::Unavailable { flag: "--output" };
        assert_eq!(noted("//...", Support::Available), None);
        assert_eq!(noted("//...", partial), Some(MANUAL_PARTLY_TOLD));
        assert_eq!(noted("//...", unavailable), Some(MANUAL_UNTOLD));
        assert_eq!(noted("//a:b", unavailable), None);
    }

    #[test]
    fn a_failed_set_says_what_the_tree_is_rooted_by_in_its_place() {
        let error = "no such target //a:b";
        assert_eq!(
            failure(error, Some(RootedBy::Targets)),
            "no such target //a:b — showing the last valid set"
        );
        assert_eq!(
            failure(error, Some(RootedBy::Tops)),
            "no such target //a:b — showing the whole build"
        );
        assert_eq!(failure(error, None), error);
    }
}
