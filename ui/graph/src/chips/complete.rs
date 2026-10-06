//! Completion of the word typed into the query field: the graph's target
//! labels, its packages in both wildcard forms and its repositories,
//! matched against the word under the cursor and listed under the field.

use super::GraphPanel;
use crate::Palette;
use crate::fuzzy::{self, Feeder, Matching, Row, Standing, TEXT_SIZE};
use crate::jump::{Candidates, bring_into_view, drawn_ids, labelled, package_of};
use crate::model::Prepared;
use bviz_graph_layout::query_lang::{word_at, written_word};
use bviz_graph_layout::{Id, Kind};
use bviz_ui_components::{ListScroll, SCROLLBAR_THICKNESS, list_scrollbar, menu_chrome};
use gpui::{
    App, ClickEvent, Context, Deferred, Div, Entity, Focusable, MouseButton, MouseDownEvent,
    Pixels, SharedString, Stateful, TextStyleRefinement, Window, div, prelude::*, px, uniform_list,
};
use rustc_hash::FxHashMap;
use std::borrow::Cow;
use std::ops::Range;

/// The width the list stands at, wide enough for a label of some depth; a
/// longer one is cut at its start, which keeps its name in view.
const LIST_WIDTH: Pixels = px(480.);

/// The most rows the list stands tall, few enough to read at a glance; the
/// rest of the matches scroll through it.
const VISIBLE_ROWS: usize = 10;

/// What a completion names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Named {
    Target(Id),
    Package,
    Repository,
}

impl Named {
    fn name(self) -> &'static str {
        match self {
            Named::Target(_) => "target",
            Named::Package => "package",
            Named::Repository => "repository",
        }
    }
}

/// One pattern the field offers to complete a word with.
struct Completion<'a> {
    /// The pattern unquoted, which a word is matched against.
    text: &'a str,
    /// The chars of `text` leading up to the name of what it names.
    path: Range<u32>,
    named: Named,
    /// How many nodes depend on a target, or how many targets a package or a
    /// repository holds.
    weight: u32,
}

/// A pattern the field offers that is no target's own label.
#[derive(Debug)]
struct Pattern {
    text: Box<str>,
    path: Range<u32>,
    named: Named,
    weight: u32,
}

/// Every pattern the field offers over one graph, indexed targets first:
/// the palette's candidates that are targets, read through it, then the
/// packages and repositories they sit in.
pub(crate) struct Completions {
    candidates: Prepared<Candidates>,
    /// The targets, as their indices among `candidates`.
    targets: Vec<u32>,
    patterns: Vec<Pattern>,
}

impl Completions {
    pub(crate) fn of(candidates: Prepared<Candidates>) -> Self {
        let (held, shared) = (candidates.get(), candidates.shared());
        let targets: Vec<u32> = (0..held.len())
            .filter(|&index| {
                let candidate = held.get(index);
                match candidate.kind {
                    Kind::Target => true,
                    Kind::Source => labelled(shared, candidate.id),
                    _ => false,
                }
            })
            .collect();
        let labels = targets.iter().map(|&index| &*held.get(index).text);
        let patterns = patterns(labels);
        Completions {
            candidates,
            targets,
            patterns,
        }
    }

    fn len(&self) -> u32 {
        (self.targets.len() + self.patterns.len()) as u32
    }

    fn get(&self, index: u32) -> Completion<'_> {
        let index = index as usize;
        match self.targets.get(index) {
            Some(&target) => {
                let candidate = self.candidates.get().get(target);
                Completion {
                    text: &candidate.text,
                    path: candidate.path.clone(),
                    named: Named::Target(candidate.id),
                    weight: candidate.rdeps,
                }
            }
            None => {
                let pattern = &self.patterns[index - self.targets.len()];
                Completion {
                    text: &pattern.text,
                    path: pattern.path.clone(),
                    named: pattern.named,
                    weight: pattern.weight,
                }
            }
        }
    }
}

/// The patterns over the targets `labels` names: each package one sits in,
/// as every target beneath it and as every target in it, and each
/// repository whose root package holds none, as every target beneath its
/// root; each weighing the targets it holds.
fn patterns<'a>(labels: impl Iterator<Item = &'a str>) -> Vec<Pattern> {
    let mut packages: FxHashMap<&str, u32> = FxHashMap::default();
    let mut repos: FxHashMap<&str, u32> = FxHashMap::default();
    for label in labels {
        *packages.entry(package_of(label)).or_default() += 1;
        if let Some((repo, _)) = label.split_once("//").filter(|(repo, _)| !repo.is_empty()) {
            *repos.entry(repo).or_default() += 1;
        }
    }
    let mut packages: Vec<(&str, u32)> = packages.into_iter().collect();
    packages.sort_unstable();
    let mut repos: Vec<(&str, u32)> = repos
        .into_iter()
        .filter(|(repo, _)| !packages_hold(&packages, &format!("{repo}//")))
        .collect();
    repos.sort_unstable();
    let mut patterns: Vec<Pattern> = Vec::with_capacity(packages.len() * 2 + repos.len());
    for (package, targets) in packages {
        let path = 0..chars(&package[..package.rfind('/').map_or(0, |slash| slash + 1)]);
        let beneath = match package.ends_with('/') {
            true => format!("{package}..."),
            false => format!("{package}/..."),
        };
        for text in [beneath, format!("{package}:*")] {
            patterns.push(Pattern {
                text: text.into_boxed_str(),
                path: path.clone(),
                named: Named::Package,
                weight: targets,
            });
        }
    }
    for (repo, targets) in repos {
        let sigil = repo.len() - repo.trim_start_matches('@').len();
        patterns.push(Pattern {
            path: 0..sigil as u32,
            text: format!("{repo}//...").into_boxed_str(),
            named: Named::Repository,
            weight: targets,
        });
    }
    patterns
}

/// Whether the sorted `packages` hold `package`.
fn packages_hold(packages: &[(&str, u32)], package: &str) -> bool {
    packages
        .binary_search_by(|(held, _)| (*held).cmp(package))
        .is_ok()
}

fn chars(text: &str) -> u32 {
    text.chars().count() as u32
}

/// The query field's completion.
#[derive(Default)]
pub(super) struct Completer {
    /// What is offered and matched, from the field's first edit over a loaded
    /// graph until the graph is replaced.
    offered: Option<Offered>,
    /// How far the list has scrolled.
    scroll: ListScroll,
    /// What the field held as the list was closed, and where the word under
    /// the cursor stood, which keep it closed until either changes.
    closed_at: Option<(SharedString, Option<Range<usize>>)>,
}

struct Offered {
    completions: Prepared<Completions>,
    matching: Matching,
    /// Where the word the rows complete stood in the field as it was matched.
    word: Option<Range<usize>>,
    /// The row the arrow keys moved onto, which Enter accepts; none until
    /// they move, so that Enter commits the field as typed.
    aimed: Option<usize>,
}

/// The matching of what the field offers.
fn matching(view: &mut GraphPanel) -> Option<&mut Matching> {
    Some(&mut view.chips.completer.offered.as_mut()?.matching)
}

/// Stops offering, parking the matcher's pool for another list to take up;
/// the next edit offers afresh.
pub(super) fn release(view: &mut GraphPanel) {
    if let Some(offered) = view.chips.completer.offered.take() {
        view.matchers.park(offered.matching);
    }
}

/// Matches the word now under the field's cursor, starting to offer the
/// graph's patterns on the first edit or cursor move made over it.
pub(super) fn follow(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    if view.chips.completer.offered.is_none() {
        let Some(completions) = view.model.read(cx).completions() else {
            return;
        };
        view.chips.completer.offered = Some(Offered {
            completions: completions.clone(),
            matching: view.matchers.take(),
            word: None,
            aimed: None,
        });
        let feed = move |feeder: &Feeder| {
            let completions = completions.get();
            for index in 0..completions.len() {
                if !feeder.push(index, completions.get(index).text) {
                    return;
                }
            }
        };
        fuzzy::inject(view, matching, take_up, feed, cx);
    }
    let field = view.chips.field.read(cx);
    let text = field.text(cx);
    let typed = word_at(&text, field.cursor(cx));
    let (word, query) = typed
        .map(|word| (word.span, SharedString::from(word.text.to_owned())))
        .unzip();
    let Some(offered) = view.chips.completer.offered.as_mut() else {
        return;
    };
    offered.word = word;
    offered.aimed = None;
    view.chips.completer.scroll.to_top();
    if offered.matching.set_query(query.unwrap_or_default()) {
        fuzzy::poll(view, matching, take_up, cx);
    }
}

/// Keeps the list closed while the field holds what it holds now.
pub(super) fn hold_closed(view: &mut GraphPanel, cx: &App) {
    view.chips.completer.closed_at = Some(at_cursor(view, cx));
}

/// Takes up what the matcher has found since the last poll.
fn take_up(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let drawn = drawn_ids(view);
    let Some(offered) = view.chips.completer.offered.as_mut() else {
        return;
    };
    let completions = &offered.completions;
    offered.matching.rerank(|index| {
        // Only a candidate handed to the matcher is read here, and one is
        // handed only once the completions are prepared, so this never waits
        // on their preparation.
        let completion = completions.get().get(index);
        Standing {
            path: completion.path.clone(),
            drawn: matches!(completion.named, Named::Target(id) if drawn.contains(&id)),
            weight: completion.weight,
        }
    });
    let rows = offered.matching.rows().len();
    offered.aimed = offered.aimed.filter(|&aimed| aimed < rows);
    cx.notify();
}

/// What the field holds, and where the word under its cursor stands.
fn at_cursor(view: &GraphPanel, cx: &App) -> (SharedString, Option<Range<usize>>) {
    let field = view.chips.field.read(cx);
    let text = field.text(cx);
    let word = word_at(&text, field.cursor(cx)).map(|word| word.span);
    (text, word)
}

/// What is offered, while the list stands open under the field: the field
/// has the focus, the cursor is on the word matched, the list was not closed
/// over what the field holds and the word at its cursor, and the best match
/// is not the word as it is written.
fn shown<'a>(view: &'a GraphPanel, window: &Window, cx: &App) -> Option<&'a Offered> {
    let offered = view.chips.completer.offered.as_ref()?;
    let field = view.chips.field.read(cx);
    if !field.focus_handle(cx).is_focused(window) {
        return None;
    }
    let at = at_cursor(view, cx);
    if view.chips.completer.closed_at.as_ref() == Some(&at) {
        return None;
    }
    let (text, word) = at;
    let word = word?;
    if offered.word.as_ref() != Some(&word) {
        return None;
    }
    let best = offered.matching.rows().first()?;
    let best = written_word(offered.completions.get().get(best.candidate).text);
    (best != text[word]).then_some(offered)
}

/// Whether the list stands open under the field.
pub(super) fn is_shown(view: &GraphPanel, window: &Window, cx: &App) -> bool {
    shown(view, window, cx).is_some()
}

/// The word of `text` the cursor at `cursor` stands in, and what replaces it
/// to complete it as `pattern`: the pattern written to scan as one word.
fn replacement<'a>(
    text: &str,
    cursor: usize,
    pattern: &'a str,
) -> Option<(Range<usize>, Cow<'a, str>)> {
    let word = word_at(text, cursor)?;
    Some((word.span, written_word(pattern)))
}

/// Replaces the word under the cursor with the row at `at`, or with the aimed
/// row or else the first, and closes the list. Rows ranked for an earlier
/// query are not accepted.
pub(super) fn accept(
    view: &mut GraphPanel,
    at: Option<usize>,
    window: &mut Window,
    cx: &mut Context<GraphPanel>,
) {
    let Some(offered) = shown(view, window, cx) else {
        return;
    };
    if !offered.matching.current() {
        return;
    }
    let at = at.or(offered.aimed).unwrap_or(0);
    let Some(row) = offered.matching.rows().get(at) else {
        return;
    };
    let pattern = offered.completions.get().get(row.candidate).text.to_owned();
    let field = view.chips.field.read(cx);
    let text = field.text(cx);
    let Some((span, written)) = replacement(&text, field.cursor(cx), &pattern) else {
        return;
    };
    view.chips
        .field
        .update(cx, |field, cx| field.replace(span, &written, cx));
    hold_closed(view, cx);
    cx.notify();
}

/// Closes the list until the field holds something else.
pub(super) fn close(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    hold_closed(view, cx);
    cx.notify();
}

/// Moves the aim `step` rows.
pub(super) fn aim(view: &mut GraphPanel, step: isize, cx: &mut Context<GraphPanel>) {
    let Some(offered) = view.chips.completer.offered.as_mut() else {
        return;
    };
    offered.aimed = stepped(offered.aimed, step, offered.matching.rows().len());
    if let Some(aimed) = offered.aimed {
        bring_into_view(&view.chips.completer.scroll, aimed);
    }
    cx.notify();
}

/// Whether the arrow keys have moved onto a row.
pub(super) fn is_aimed(view: &GraphPanel) -> bool {
    view.chips
        .completer
        .offered
        .as_ref()
        .is_some_and(|offered| offered.aimed.is_some())
}

/// The row `step` rows on from `aimed` among `rows` rows: the first one down
/// from none, none up from the first, and held to the list past its end.
fn stepped(aimed: Option<usize>, step: isize, rows: usize) -> Option<usize> {
    let last = rows.checked_sub(1)?;
    match aimed {
        None if step > 0 => Some(0),
        None => None,
        Some(at) => at.checked_add_signed(step).map(|at| at.min(last)),
    }
}

/// The list under the field, for the field's `relative` holder to hold;
/// `None` while it is closed. It stands no taller than the window, past
/// which its rows scroll.
pub(super) fn list(
    view: &GraphPanel,
    palette: &Palette,
    window: &mut Window,
    cx: &mut Context<GraphPanel>,
) -> Option<Deferred> {
    let offered = shown(view, window, cx)?;
    let count = offered.matching.rows().len();
    let text = TextStyleRefinement {
        font_size: Some(TEXT_SIZE.into()),
        ..TextStyleRefinement::default()
    };
    let row = window.with_text_style(Some(text), |window| menu_chrome::row_height(window));
    let room = window.viewport_size();
    let height = menu_chrome::sheet_height(count.min(VISIBLE_ROWS), row, px(0.), room.height);
    let entity = cx.entity();
    let listing = *palette;
    let rows = uniform_list("chip-completions", count, move |range, _, cx| {
        let view = entity.read(cx);
        let Some(offered) = view.chips.completer.offered.as_ref() else {
            return Vec::new();
        };
        let completions = offered.completions.get();
        range
            .filter_map(|index| {
                let row = offered.matching.rows().get(index)?;
                let completion = completions.get(row.candidate);
                let accepts = match offered.aimed {
                    Some(aimed) => (aimed == index).then_some(Accepts::Aimed),
                    None => (index == 0).then_some(Accepts::First),
                };
                Some(listed(&completion, row, index, accepts, &listing, &entity))
            })
            .collect()
    })
    .flex_1()
    .min_h_0()
    .w_full()
    .pr(px(SCROLLBAR_THICKNESS))
    .track_scroll(view.chips.completer.scroll.uniform_handle());
    let sheet = menu_chrome::sheet(&palette.colors)
        .w(LIST_WIDTH)
        .max_w(room.width)
        .h(height)
        .text_size(TEXT_SIZE)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|view, event: &MouseDownEvent, _, cx| {
                view.chips.completer.scroll.press(event.position);
                // Keep the press off the panel, which would take the focus
                // from the field and so close the list mid-click.
                cx.stop_propagation();
                cx.notify();
            }),
        )
        .child(rows)
        .child(list_scrollbar(
            cx.entity(),
            |view: &GraphPanel| &view.chips.completer.scroll,
            |view: &mut GraphPanel| &mut view.chips.completer.scroll,
            palette.scroll,
        ));
    Some(menu_chrome::under_trigger(sheet))
}

/// One row: the pattern with the characters the word matched picked out, and
/// what it names.
fn listed(
    completion: &Completion,
    row: &Row,
    index: usize,
    accepts: Option<Accepts>,
    palette: &Palette,
    entity: &Entity<GraphPanel>,
) -> Stateful<Div> {
    let colors = &palette.colors;
    let entity = entity.clone();
    let text = SharedString::from(completion.text.to_string());
    let aimed = accepts == Some(Accepts::Aimed);
    menu_chrome::aimed_row(("chip-completion", index).into(), colors, aimed)
        .w_full()
        .on_click(move |_: &ClickEvent, window, cx| {
            entity.update(cx, |view, cx| accept(view, Some(index), window, cx));
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis_start()
                .child(fuzzy::highlighted(
                    text,
                    &completion.path,
                    &row.matched,
                    colors.text_muted,
                    palette.highlight.into(),
                )),
        )
        .children(accepts.map(|accepts| {
            div()
                .flex_none()
                .px_1()
                .rounded_sm()
                .border_1()
                .border_color(colors.text_muted)
                .text_xs()
                .text_color(colors.text_muted)
                .child(accepts.keys())
        }))
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(colors.text_muted)
                .child(completion.named.name()),
        )
}

/// Which keys accept a row: Tab the first row while none is aimed at, and
/// Tab or Enter the row aimed at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Accepts {
    First,
    Aimed,
}

impl Accepts {
    /// The keys, as the row's hint names them.
    fn keys(self) -> &'static str {
        match self {
            Accepts::First => "tab",
            Accepts::Aimed => "enter",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offered(labels: &[&str]) -> Vec<(String, String, Named)> {
        patterns(labels.iter().copied())
            .into_iter()
            .map(|pattern| {
                let text = pattern.text.to_string();
                let path: String = text.chars().take(pattern.path.end as usize).collect();
                (text, path, pattern.named)
            })
            .collect()
    }

    fn texts(offered: &[(String, String, Named)]) -> Vec<&str> {
        offered.iter().map(|(text, _, _)| text.as_str()).collect()
    }

    /// A package is offered as every target beneath it and as every target in
    /// it, and a repository as every target beneath its root.
    #[test]
    fn a_label_offers_its_package_and_its_repository() {
        let offered = offered(&["//app/net:client", "@zlib//src:z"]);
        assert_eq!(
            texts(&offered),
            [
                "//app/net/...",
                "//app/net:*",
                "@zlib//src/...",
                "@zlib//src:*",
                "@zlib//...",
            ]
        );
    }

    /// Only the name a pattern names counts as a hit on its name.
    #[test]
    fn the_path_leads_up_to_the_name() {
        let offered = offered(&["//app/net:client", "@zlib//:z"]);
        let path = |text: &str| {
            let (_, path, _) = offered.iter().find(|(held, _, _)| held == text).unwrap();
            path.clone()
        };
        assert_eq!(path("//app/net/..."), "//app/");
        assert_eq!(path("//app/net:*"), "//app/");
        assert_eq!(path("@zlib//..."), "@zlib//");
    }

    /// A repository whose root package is offered already is offered once.
    #[test]
    fn a_repository_with_a_root_package_is_offered_as_that_package() {
        let offered = offered(&["@zlib//:z"]);
        assert_eq!(texts(&offered), ["@zlib//...", "@zlib//:*"]);
        assert!(offered.iter().all(|(_, _, named)| *named == Named::Package));
    }

    /// Down reaches the first row from none aimed at, up from the first
    /// leaves none aimed at, and the aim holds to the list.
    #[test]
    fn the_aim_starts_at_none_and_returns_there() {
        assert_eq!(stepped(None, 1, 5), Some(0));
        assert_eq!(stepped(None, -1, 5), None);
        assert_eq!(stepped(Some(0), -1, 5), None);
        assert_eq!(stepped(Some(3), 1, 5), Some(4));
        assert_eq!(stepped(Some(4), 1, 5), Some(4));
        assert_eq!(stepped(None, 1, 0), None);
    }

    #[test]
    fn a_completion_replaces_the_word_quoted_where_it_must_be() {
        let text = "deps(//ap) + x";
        let (span, written) = replacement(text, 9, "//app:lib").unwrap();
        assert_eq!((span, &*written), (5..9, "//app:lib"));

        let canonical = "@@rules_rs++crate+crates__cbindgen//:cbindgen";
        let (span, written) = replacement("cbind", 5, canonical).unwrap();
        assert_eq!((span, &*written), (0..5, canonical));

        let (_, written) = replacement("a", 1, "//a:my lib").unwrap();
        assert_eq!(written, "\"//a:my lib\"");

        let (span, _) = replacement("x + \"//a b", 10, "//a b:c").unwrap();
        assert_eq!(span, 4..10);
        assert!(replacement("deps(x", 4, "//a").is_none());
    }

    /// A word in a quotation left open is replaced up to where an unquoted
    /// word would end, keeping what follows it.
    #[test]
    fn a_completion_in_an_open_quotation_keeps_what_follows() {
        let text = "deps(\"//ap) + x";
        let (span, written) = replacement(text, 10, "//app:lib").unwrap();
        let mut completed = text.to_owned();
        completed.replace_range(span, &written);
        assert_eq!(completed, "deps(//app:lib) + x");
    }
}
