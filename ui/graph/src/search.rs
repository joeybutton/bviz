//! The toolbar's label search: a target pattern typed over the canonical labels
//! the graph carries, and the rule showing or hiding everything it names.
//!
//! The search owns one rule of the filter — the one constraining the label term
//! and nothing else — and the base, which nothing else writes. Every other rule
//! belongs to a control of its own and is left where it stands, so a search
//! narrows a checklist's reductions and emptying the field leaves them as they
//! were.
//!
//! What a rule selects is the pattern, never the labels the result list shows:
//! that list samples the matches so a reader can see what the pattern reached,
//! and the count beside it covers all of them.

use crate::{CARD_MAX_SHARE, GraphPanel, Palette, checklist, pins};
use bviz_graph_layout::Id;
use bviz_graph_layout::select::labels::LabelIndex;
use bviz_graph_layout::select::pattern::Pattern;
use bviz_graph_layout::select::{Base, Filter, Predicate, Rule, Visibility};
use bviz_ui_components::{Button, TextInput};
use gpui::{
    ClickEvent, Context, Div, Entity, Focusable, IntoElement, MouseButton, MouseDownEvent, Pixels,
    SharedString, Stateful, Subscription, Task, Window, actions, div, prelude::*, px, relative,
};
use std::time::Duration;

actions!(bviz_graph, [DismissResults]);

/// The key context the search field carries, by which the panel's own
/// single-key bindings stand down while a pattern is being typed into it.
pub(crate) const KEY_CONTEXT: &str = "GraphSearch";

/// How long typing must settle before the pattern is matched, so a pattern part
/// typed costs one pass over the labels rather than one per character.
const SETTLE: Duration = Duration::from_millis(150);

/// How many matches the result list names. It is a sample of what the pattern
/// reaches, so it is short enough to read at a glance.
const PREVIEW: usize = 20;

/// Width the field is drawn at, wide enough for a package path and a target.
const FIELD_WIDTH: Pixels = px(240.);

/// Width the result list is drawn at, over the drawing it names boxes of.
const RESULTS_WIDTH: Pixels = px(360.);

/// What a search does with the nodes its pattern names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Act {
    /// Draws the matches and nothing else, which is how a graph too large to
    /// read is narrowed to the part being looked for.
    Only,
    Hide,
}

impl Act {
    const ALL: [Act; 2] = [Act::Only, Act::Hide];

    fn label(self) -> &'static str {
        match self {
            Act::Only => "show only",
            Act::Hide => "hide",
        }
    }

    fn visibility(self) -> Visibility {
        match self {
            Act::Only => Visibility::Shown,
            Act::Hide => Visibility::Hidden,
        }
    }

    /// The verdict every node the pattern does not name is read by.
    fn base(self) -> Base {
        match self {
            Act::Only => Base::HideAll,
            Act::Hide => Base::ShowAll,
        }
    }

    fn of(visibility: Visibility) -> Option<Act> {
        match visibility {
            Visibility::Shown => Some(Act::Only),
            Visibility::Hidden => Some(Act::Hide),
            Visibility::Faded => None,
        }
    }
}

/// One label a pattern named, and the target node carrying it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Match {
    pub(crate) target: Id,
    /// The label as a reader writes it, which is how the same target's box is
    /// captioned; the pattern is matched and the target pinned by the canonical
    /// form instead.
    pub(crate) label: String,
}

/// What matching one pattern over the graph's labels found.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Found {
    /// Every label the pattern names, of which [`Found::sample`] holds the first
    /// few.
    total: usize,
    sample: Vec<Match>,
}

/// The card floating over the drawing: the pattern it stands for, what that
/// pattern named, and whether it is up.
#[derive(Default)]
struct Card {
    /// What the field held when matching was last started, by which a repaint of
    /// the field is told from an edit to it.
    text: SharedString,
    /// What `text` names, absent while the matching for it is still running.
    found: Option<Found>,
    /// Whether the card has been put away. It covers the drawing it names boxes
    /// of, so a press outside it puts it away and the field brings it back —
    /// neither of which touches the rule the search wrote, leaving the same
    /// graph drawn either way.
    dismissed: bool,
}

impl Card {
    /// Takes up `text` as the pattern the card stands for, which brings the card
    /// back over the matches still to be found; `false` where the card already
    /// stands for that text, which a repaint of the field reports.
    fn typed(&mut self, text: SharedString) -> bool {
        if self.text == text {
            return false;
        }
        self.text = text;
        self.found = None;
        self.dismissed = false;
        true
    }

    /// Puts the card away.
    fn dismiss(&mut self) {
        self.dismissed = true;
    }

    /// Brings the card back over what the pattern last named.
    fn reveal(&mut self) {
        self.dismissed = false;
    }

    /// Whether the card stands over the drawing.
    fn listing(&self) -> bool {
        !self.dismissed && !self.text.is_empty()
    }
}

/// The label search: the field a pattern is typed into, and the card reading
/// what matching it last found.
pub(crate) struct Search {
    field: Entity<TextInput>,
    card: Card,
    /// Follows the field's focus with the card; installed on the first render,
    /// where the window is in hand.
    watching: Option<[Subscription; 2]>,
    /// The wait for typing to settle and the matching that follows it, held so
    /// that the next edit replaces — and so cancels — both.
    _matching: Task<()>,
}

impl Search {
    pub(crate) fn new(cx: &mut Context<GraphPanel>) -> Self {
        Search {
            field: cx.new(|cx| TextInput::new(cx, "search labels: //foo/…", 0)),
            card: Card::default(),
            watching: None,
            _matching: Task::ready(()),
        }
    }

    /// The field, for subscribing to what is typed into it.
    pub(crate) fn field(&self) -> &Entity<TextInput> {
        &self.field
    }

    fn pattern(&self) -> Pattern {
        Pattern::parse(&self.card.text)
    }
}

/// Takes up what the field now holds: the pattern is matched once typing
/// settles, and emptying the field takes the search back off at once.
pub(crate) fn typed(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let text = view.search.field.read(cx).content();
    if !view.search.card.typed(text.clone()) {
        return;
    }
    if text.is_empty() {
        view.search._matching = Task::ready(());
        let filter = cleared(view.derived.filter());
        checklist::set_filter(view, filter, cx);
        return;
    }
    match_pattern(view, cx);
    cx.notify();
}

/// Matches the pattern the card stands for over the graph's labels, off the UI
/// thread and once typing has settled. An empty field names nothing to match.
fn match_pattern(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let text = view.search.card.text.clone();
    if text.is_empty() {
        return;
    }
    view.search._matching = cx.spawn(async move |view, cx| {
        cx.background_executor().timer(SETTLE).await;
        let Ok(Some(index)) = view.read_with(cx, |view, cx| view.labels(cx)) else {
            return;
        };
        let pattern = Pattern::parse(&text);
        let found = cx
            .background_executor()
            .spawn(async move { matched(&index, &pattern) })
            .await;
        let _ = view.update(cx, |view, cx| found_by(view, found, cx));
    });
}

/// Follows the field's focus with the card: taking the field up brings the card
/// back, and a click that lands on another control puts it away. Installed on
/// the panel's first render, where the window is in hand.
pub(crate) fn watch_field(
    view: &mut GraphPanel,
    window: &mut Window,
    cx: &mut Context<GraphPanel>,
) {
    if view.search.watching.is_some() {
        return;
    }
    let field = view.search.field.read(cx).focus_handle(cx);
    view.search.watching = Some([
        cx.on_focus_in(&field, window, |view, _, cx| reveal(view, cx)),
        cx.on_focus_out(&field, window, |view, _, _, cx| dismiss(view, cx)),
    ]);
}

/// Puts the card away. What the search left drawn stands, as does the pattern
/// that wrote it, so the card comes back to a search still in force.
pub(crate) fn dismiss(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    if !view.search.card.listing() {
        return;
    }
    view.search.card.dismiss();
    cx.notify();
}

/// Brings the card back where a press on the field lands while it is away. A
/// press that lands on the field holding focus moves no focus, so the field's
/// own focus says nothing about it.
fn pressed(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    if view.search.card.dismissed {
        reveal(view, cx);
    }
}

/// Brings the card back, matching the pattern afresh: the graph it names boxes
/// of may have been drawn over since the last pass. What that pass found stays
/// listed until the new matches land.
fn reveal(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    view.search.card.reveal();
    match_pattern(view, cx);
    cx.notify();
}

/// What `pattern` names of `index`.
fn matched(index: &LabelIndex, pattern: &Pattern) -> Found {
    let mut sample: Vec<Match> = Vec::with_capacity(PREVIEW);
    let mut total = 0;
    for (target, label) in index.matching(pattern) {
        total += 1;
        if sample.len() < PREVIEW {
            sample.push(Match {
                target,
                label: index.written(label).into_owned(),
            });
        }
    }
    Found { total, sample }
}

/// Adopts what the pattern in the field named. A search in force follows the
/// field, so its rule is restated over the pattern that has just been matched.
fn found_by(view: &mut GraphPanel, found: Found, cx: &mut Context<GraphPanel>) {
    view.search.card.found = Some(found);
    if let Some(act) = acting(view.derived.filter()) {
        let filter = applying(view.derived.filter(), &view.search.pattern(), act);
        checklist::set_filter(view, filter, cx);
    }
    cx.notify();
}

/// Acts on every node the pattern in the field names, or takes the search back
/// off where `act` is the one already in force.
fn act_on_matches(view: &mut GraphPanel, act: Act, cx: &mut Context<GraphPanel>) {
    if acting(view.derived.filter()) == Some(act) {
        let filter = cleared(view.derived.filter());
        checklist::set_filter(view, filter, cx);
        return;
    }
    let filter = applying(view.derived.filter(), &view.search.pattern(), act);
    match act {
        // The matches are drawn where the document places them, so narrowing to
        // them alone can leave them anywhere in the extent the whole graph
        // spans; the view moves onto them rather than onto the space the rest
        // of the graph occupied.
        Act::Only => checklist::set_filter_framed(view, filter, cx),
        Act::Hide => checklist::set_filter(view, filter, cx),
    }
}

/// Takes the search back to what it holds over a graph nothing has been typed
/// against: a pattern and the labels it named describe the graph they were
/// matched over, so neither outlives it.
pub(crate) fn reset(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    view.search.card = Card::default();
    view.search._matching = Task::ready(());
    clear(view, cx);
}

/// Empties the field, which restores the rules the search displaced.
fn clear(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    view.search
        .field
        .update(cx, |field, cx| field.set_content("", cx));
}

/// `filter` with everything `pattern` names acted on by `act`.
pub(crate) fn applying(filter: &Filter, pattern: &Pattern, act: Act) -> Filter {
    let mut rules: Vec<Rule> = Vec::with_capacity(filter.rules.len() + 1);
    rules.push(Rule {
        predicate: Predicate {
            label: Some(pattern.clone()),
            ..Predicate::default()
        },
        visibility: act.visibility(),
    });
    rules.extend(
        filter
            .rules
            .iter()
            .filter(|rule| written(rule).is_none())
            .cloned(),
    );
    Filter {
        base: act.base(),
        rules,
        ..filter.clone()
    }
}

/// `filter` with the search's rule taken back out and the base it wrote restored.
pub(crate) fn cleared(filter: &Filter) -> Filter {
    Filter {
        base: Base::default(),
        rules: filter
            .rules
            .iter()
            .filter(|rule| written(rule).is_none())
            .cloned()
            .collect(),
        ..filter.clone()
    }
}

/// What `filter`'s search rule does; `None` where it holds none.
pub(crate) fn acting(filter: &Filter) -> Option<Act> {
    filter.rules.iter().find_map(written)
}

/// What `rule` does with the labels it names, where it is one the search wrote:
/// a rule over the label term and nothing besides.
fn written(rule: &Rule) -> Option<Act> {
    let label = rule.predicate.label.clone()?;
    let alone = Predicate {
        label: Some(label),
        ..Predicate::default()
    };
    if rule.predicate != alone {
        return None;
    }
    Act::of(rule.visibility)
}

/// The field a pattern is typed into, with the button that empties it.
pub(crate) fn field(search: &Search, cx: &mut Context<GraphPanel>) -> impl IntoElement {
    let typed = !search.card.text.is_empty();
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(
            div()
                .key_context(KEY_CONTEXT)
                .on_action(cx.listener(|view, _: &DismissResults, _, cx| dismiss(view, cx)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|view, _: &MouseDownEvent, _, cx| pressed(view, cx)),
                )
                .w(FIELD_WIDTH)
                .flex_none()
                .child(search.field.clone()),
        )
        .children(typed.then(|| {
            Button::new(
                "search-clear",
                cx.listener(|view, _: &ClickEvent, _, cx| clear(view, cx)),
            )
            .ghost()
            .icon("x")
        }))
}

/// What the typed pattern reaches, floating over the drawing: how many labels it
/// names, the actions over all of them, and the first few as a sample; `None`
/// while the card is away.
pub(crate) fn card(
    view: &GraphPanel,
    palette: &Palette,
    cx: &mut Context<GraphPanel>,
) -> Option<Div> {
    let search = &view.search;
    if !search.card.listing() {
        return None;
    }
    let acting = acting(view.derived.filter());
    let sample = search.card.found.iter().flat_map(|found| &found.sample);
    Some(
        div()
            .absolute()
            .left_2()
            .top_2()
            .w(RESULTS_WIDTH)
            .max_w(relative(CARD_MAX_SHARE))
            .occlude()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_md()
            .bg(palette.container)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(palette.label)
                            .child(tally(search.card.found.as_ref())),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .children(Act::ALL.map(|act| {
                                Button::new(
                                    ("search-act", act as usize),
                                    cx.listener(move |view, _: &ClickEvent, _, cx| {
                                        act_on_matches(view, act, cx)
                                    }),
                                )
                                .label(act.label())
                                .compact()
                                .selected(acting == Some(act))
                            })),
                    ),
            )
            .children(sample.map(|matched| row(matched, view.derived.filter(), palette, cx))),
    )
}

/// One match, which a click pins so that it is drawn whatever the rules say.
fn row(
    matched: &Match,
    filter: &Filter,
    palette: &Palette,
    cx: &mut Context<GraphPanel>,
) -> Stateful<Div> {
    let target = matched.target;
    let kept = pins::pinned(filter, target) == Some(Visibility::Shown);
    div()
        .id(("search-match", target.0 as usize))
        .flex()
        .items_center()
        .gap_1()
        .cursor_pointer()
        .text_xs()
        .text_color(palette.label)
        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
            pins::set(view, target, Visibility::Shown, cx)
        }))
        .child(pins::mark(kept, palette))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(SharedString::from(matched.label.clone())),
        )
}

/// How many labels the pattern names, and what the list below it leaves out.
fn tally(found: Option<&Found>) -> SharedString {
    let Some(found) = found else {
        return "matching…".into();
    };
    if found.total == 0 {
        return "no matches".into();
    }
    let counted = match found.total {
        1 => "1 match".to_string(),
        total => format!("{total} matches"),
    };
    match found.total - found.sample.len() {
        0 => counted.into(),
        beyond => format!("{counted}, {beyond} below").into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{self, Value};
    use bviz_graph_layout::facets::FacetsBuilder;
    use bviz_graph_layout::graph::{Graph, Node};
    use bviz_graph_layout::select::Verdict;
    use bviz_graph_layout::{Facets, Kind, Names};

    /// A graph of one target per package, in packages enough that the matches
    /// run well past the sample the result list holds, plus one target of a
    /// module repository, which a reader writes by the module name.
    struct Fixture {
        graph: Graph,
        names: Names,
        facets: Facets,
    }

    /// How many packages under `//deep` the fixture holds, chosen so a pattern
    /// over them names more labels than the list samples.
    const DEEP: usize = PREVIEW + 5;

    impl Fixture {
        fn new() -> Self {
            let mut graph = Graph::new();
            let mut names = Names::default();
            let mut builder = FacetsBuilder::default();
            let mut describe = |label: &str, reads: &str| {
                let id = names.intern(&format!("target:{label}"));
                graph.add_node(Node::new(id, reads, Kind::Target, None));
                builder.describe_label(id, None, label);
            };
            for i in 0..DEEP {
                describe(&format!("//deep/pkg{i}:lib"), ":lib");
            }
            describe("//app:main", ":main");
            describe("@@rules_foo+//bar:lib", ":lib");
            let (facets, _) = builder.seal();
            Fixture {
                graph,
                names,
                facets,
            }
        }

        fn index(&self) -> LabelIndex {
            LabelIndex::of(&self.graph, &self.names, &self.facets)
        }

        /// The labels `filter` draws a box for.
        fn shown(&self, filter: &Filter) -> Vec<&str> {
            let verdicts = filter.evaluate(&self.graph, &self.names, &self.facets);
            self.graph
                .nodes
                .iter()
                .filter(|node| verdicts.get(node.id) == Verdict::Shown)
                .map(|node| self.names.resolve(node.id))
                .collect()
        }
    }

    fn pattern(text: &str) -> Pattern {
        Pattern::parse(text)
    }

    /// A filter a control other than the search wrote, which the search must
    /// leave standing.
    fn hiding(kind: Kind) -> Filter {
        rules::set_hidden(&Filter::default(), &Value::Kind(kind), true)
    }

    #[test]
    fn showing_only_the_matches_hides_everything_the_pattern_does_not_name() {
        let fixture = Fixture::new();
        let filter = applying(&Filter::default(), &pattern("//app/..."), Act::Only);

        assert_eq!(filter.base, Base::HideAll);
        assert_eq!(fixture.shown(&filter), ["target://app:main"]);
    }

    /// The rule names the pattern, so it reaches every match rather than the
    /// labels the result list samples.
    #[test]
    fn an_action_reaches_the_matches_the_result_list_leaves_out() {
        let fixture = Fixture::new();
        let deep = pattern("//deep/...");
        let found = matched(&fixture.index(), &deep);

        assert_eq!(found.total, DEEP);
        assert_eq!(found.sample.len(), PREVIEW);

        let filter = applying(&Filter::default(), &deep, Act::Only);
        assert_eq!(fixture.shown(&filter).len(), DEEP);
    }

    /// A row names a box of the drawing, so it reads the name that box is
    /// captioned by; what the row pins is the target the canonical label
    /// identifies.
    #[test]
    fn a_result_row_reads_the_label_as_a_reader_writes_it() {
        let fixture = Fixture::new();
        let found = matched(&fixture.index(), &pattern("@rules_foo//..."));

        assert_eq!(
            found.sample,
            [Match {
                target: fixture
                    .names
                    .get("target:@@rules_foo+//bar:lib")
                    .expect("the module repository's target"),
                label: "@rules_foo//bar:lib".to_string(),
            }]
        );
    }

    #[test]
    fn hiding_the_matches_leaves_every_other_node_drawn() {
        let fixture = Fixture::new();
        let filter = applying(&Filter::default(), &pattern("//deep/..."), Act::Hide);

        assert_eq!(filter.base, Base::ShowAll);
        assert_eq!(
            fixture.shown(&filter),
            ["target://app:main", "target:@@rules_foo+//bar:lib"]
        );
    }

    /// Every rule but the search's own belongs to another control, so a search
    /// narrows what those hide and taking it off gives that filter back whole.
    #[test]
    fn a_search_taken_off_leaves_the_rules_it_was_laid_over_as_they_were() {
        let held = hiding(Kind::Source);

        for act in Act::ALL {
            let searching = applying(&held, &pattern("//deep/..."), act);
            assert_eq!(searching.rules[1..], held.rules[..]);
            assert_eq!(acting(&searching), Some(act));
            assert_eq!(cleared(&searching), held, "{act:?}");
        }
    }

    /// The pattern's rule stands ahead of the rules over a node's other facets,
    /// so showing only the matches selects within what a checklist leaves rather
    /// than drawing back what it hid — whichever of the two controls acted last.
    #[test]
    fn a_pattern_never_draws_back_what_another_rule_hides() {
        let fixture = Fixture::new();
        let deep = pattern("//deep/...");

        let searched_last = applying(&hiding(Kind::Target), &deep, Act::Only);
        let hidden_last = rules::set_hidden(
            &applying(&Filter::default(), &deep, Act::Only),
            &Value::Kind(Kind::Target),
            true,
        );

        assert_eq!(searched_last.rules, hidden_last.rules);
        assert!(fixture.shown(&searched_last).is_empty());
    }

    /// Emptying the field where no action was taken leaves the filter untouched,
    /// so typing alone never moves what is drawn.
    #[test]
    fn a_pattern_typed_and_no_action_taken_leaves_the_filter_alone() {
        let held = hiding(Kind::Source);

        assert_eq!(acting(&held), None);
        assert_eq!(cleared(&held), held);
    }

    /// One search rule stands at a time, so acting again restates it rather than
    /// laying a second beside it.
    #[test]
    fn acting_again_restates_the_one_rule_the_search_holds() {
        let only = applying(&Filter::default(), &pattern("//deep/..."), Act::Only);
        let hidden = applying(&only, &pattern("//app/..."), Act::Hide);

        assert_eq!(hidden.rules.len(), 1);
        assert_eq!(acting(&hidden), Some(Act::Hide));
        assert_eq!(hidden.base, Base::ShowAll);
    }

    /// A rule constraining more than the label term is another control's, so the
    /// search neither reads it as its own nor rewrites it.
    #[test]
    fn a_rule_over_more_than_the_label_term_is_left_alone() {
        let untouched = Filter {
            rules: vec![Rule {
                predicate: Predicate {
                    label: Some(pattern("//deep/...")),
                    kinds: bviz_graph_layout::select::Values::new([Kind::Target]),
                    ..Predicate::default()
                },
                visibility: Visibility::Hidden,
            }],
            ..Filter::default()
        };

        assert_eq!(acting(&untouched), None);
        assert_eq!(cleared(&untouched), untouched);
    }

    /// A card standing over the matches for `text`.
    fn listing(text: &str) -> Card {
        let mut card = Card::default();
        card.typed(text.into());
        card.found = Some(Found {
            total: 1,
            sample: Vec::new(),
        });
        card
    }

    /// The card holds no filter, so putting it away leaves what the search drew
    /// standing; it keeps the pattern and what that pattern named, so bringing
    /// it back asks for neither to be typed or matched again to read it.
    #[test]
    fn a_card_put_away_keeps_the_pattern_and_what_it_named() {
        let mut card = listing("//deep/...");
        let found = card.found.clone();

        card.dismiss();
        assert!(!card.listing());

        card.reveal();
        assert!(card.listing());
        assert_eq!(card.text, "//deep/...");
        assert_eq!(card.found, found);
    }

    /// The field notifies on every repaint it makes, so a card put away stays
    /// away until something is actually typed — at which point it is back over
    /// the matches for what is being typed.
    #[test]
    fn a_card_put_away_comes_back_when_the_pattern_is_typed_over() {
        let mut card = listing("//deep/...");
        card.dismiss();

        assert!(!card.typed("//deep/...".into()));
        assert!(!card.listing());
        assert!(card.found.is_some());

        assert!(card.typed("//app/...".into()));
        assert!(card.listing());
        assert_eq!(card.found, None);
    }

    /// A pattern is what the card stands for, so an emptied field lists nothing
    /// however the card was last left.
    #[test]
    fn an_emptied_field_lists_nothing() {
        let mut card = listing("//deep/...");

        assert!(card.typed(SharedString::default()));
        assert!(!card.listing());
        card.reveal();
        assert!(!card.listing());
    }

    #[test]
    fn the_tally_says_what_the_list_below_it_leaves_out() {
        let fixture = Fixture::new();
        let index = fixture.index();

        assert_eq!(tally(None), "matching…");
        assert_eq!(
            tally(Some(&matched(&index, &pattern("//nowhere/...")))),
            "no matches"
        );
        assert_eq!(
            tally(Some(&matched(&index, &pattern("//app/...")))),
            "1 match"
        );
        assert_eq!(
            tally(Some(&matched(&index, &pattern("//deep/...")))),
            format!("{DEEP} matches, 5 below")
        );
    }
}
