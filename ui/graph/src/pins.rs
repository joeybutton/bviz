//! Per-node overrides: the nodes a reader has shown or hidden on their own.
//!
//! A pin decides the node it names whatever the rules say, which is what lets
//! one node kept or dropped by hand outlast a change to a rule that has nothing
//! to do with it. Nothing derives a pin, so every one in force is listed and can
//! be taken back, or an override would be a state a reader could reach and not
//! leave.

use crate::column::scrolling;
use crate::point::written_name;
use crate::{GraphPanel, Palette, derive};
use bviz_graph_layout::facets::WrittenRepos;
use bviz_graph_layout::select::{Filter, Pin, Visibility};
use bviz_graph_layout::{Facets, Id, Names};
use bviz_ui_components::LabelLine;
use bviz_ui_components::gesture::{
    Button as PointerButton, Entry, GestureRow, Gestures, Input, Place, Pointer,
};
use bviz_ui_components::{Button, ListScroll};
use gpui::{
    ClickEvent, Context, Div, IntoElement, Pixels, SharedString, Stateful, div, prelude::*, px,
    relative,
};
use std::collections::HashMap;

/// What the card does for a gesture in [`GESTURES`].
#[derive(Clone, Copy)]
enum PinIntent {
    Unpin,
}

const GESTURES: Gestures<PinIntent> = Gestures::new(&[Entry::new(
    Input::click(PointerButton::Left),
    Place::On("a pinned node"),
    "Unpin node",
    PinIntent::Unpin,
)]);

/// The gestures the card answers to.
pub(crate) fn gestures() -> impl Iterator<Item = GestureRow> {
    GESTURES.rows()
}

/// Width the list of pins is drawn at, over the drawing it names boxes of.
const PANEL_WIDTH: Pixels = px(320.);

/// The share of the height it is placed in the card runs to before its rows
/// scroll, so that however many overrides are in force the card stays clear of
/// the canvas's foot and every row it lists can be reached.
const MAX_HEIGHT_SHARE: f32 = 0.6;

/// How the override `pin` is listed.
fn label(pin: Pin) -> &'static str {
    match pin {
        Pin::Set(Visibility::Shown) => "always show",
        Pin::Set(Visibility::Hidden) => "always hide",
        Pin::Set(Visibility::Faded) => "always fade",
        Pin::Revealed => "revealed",
    }
}

/// The menu row pinning a box to `visibility`, worded as the pin it sets is
/// listed.
pub(crate) fn offer(visibility: Visibility) -> String {
    let listed = label(Pin::Set(visibility));
    let mut rest = listed.chars();
    let first = rest.next().map(|first| first.to_ascii_uppercase());
    format!("{}{} box", String::from_iter(first), rest.as_str())
}

/// `filter` with `node` pinned to `visibility`, or with its pin dropped where
/// it already carries that one, so the same gesture puts the override away.
pub(crate) fn toggle(filter: &Filter, node: Id, visibility: Visibility) -> Filter {
    let pin = Pin::Set(visibility);
    let mut pins = filter.pins.clone();
    if pins.get(&node) == Some(&pin) {
        pins.remove(&node);
    } else {
        pins.insert(node, pin);
    }
    Filter {
        pins,
        ..filter.clone()
    }
}

/// `filter` with `node` revealed beside every node revealed before it, each
/// standing until it is withdrawn from the pins card. A pin the reader set on
/// `node` stands, and decides it.
pub(crate) fn revealing(filter: &Filter, node: Id) -> Filter {
    let mut pins = filter.pins.clone();
    pins.entry(node).or_insert(Pin::Revealed);
    Filter {
        pins,
        ..filter.clone()
    }
}

/// `filter` without the nodes it reveals, its reader's own pins standing.
fn unrevealed(filter: &Filter) -> Filter {
    let mut pins = filter.pins.clone();
    pins.retain(|_, pin| *pin != Pin::Revealed);
    Filter {
        pins,
        ..filter.clone()
    }
}

/// `filter` without the pin it holds for `node`.
fn unpinned(filter: &Filter, node: Id) -> Filter {
    let mut pins = filter.pins.clone();
    pins.remove(&node);
    Filter {
        pins,
        ..filter.clone()
    }
}

/// `filter` with every override dropped, leaving each node to the rules again.
pub(crate) fn cleared(filter: &Filter) -> Filter {
    Filter {
        pins: HashMap::new(),
        ..filter.clone()
    }
}

/// How far the card listing the overrides in force has scrolled, and the drag
/// on its scroll bar.
pub(crate) struct Pins {
    scroll: ListScroll,
}

impl Pins {
    pub(crate) fn new() -> Self {
        Pins {
            scroll: ListScroll::new(),
        }
    }
}

/// One override in force.
struct Held {
    node: Id,
    /// The node's name as it is shown.
    label: SharedString,
    /// The node's name as a reader writes it.
    written: SharedString,
    pin: Pin,
}

/// The overrides `filter` holds, by the name each is listed under.
fn held(filter: &Filter, names: &Names, facets: &Facets, repos: &WrittenRepos) -> Vec<Held> {
    let mut held: Vec<Held> = Vec::with_capacity(filter.pins.len());
    held.extend(filter.pins.iter().map(|(&node, &pin)| {
        let written = written_name(names, facets, repos, node);
        Held {
            node,
            label: repos.names().show(&written).into_text().into(),
            written: written.into(),
            pin,
        }
    }));
    held.sort_by(|one, other| (&one.label, one.node.0).cmp(&(&other.label, other.node.0)));
    held
}

/// Pins `node` to `pin`, or takes that override back off.
pub(crate) fn set(view: &mut GraphPanel, node: Id, pin: Visibility, cx: &mut Context<GraphPanel>) {
    let filter = toggle(view.derived.filter(), node, pin);
    derive::set_filter(view, filter, cx);
}

/// Shows `node` whatever the rules say, as [`revealing`] pins it.
pub(crate) fn reveal(view: &mut GraphPanel, node: Id, cx: &mut Context<GraphPanel>) {
    let filter = revealing(view.derived.filter(), node);
    derive::set_filter(view, filter, cx);
}

/// Drops the pin `node` carries, leaving it to the rules.
pub(crate) fn unpin(view: &mut GraphPanel, node: Id, cx: &mut Context<GraphPanel>) {
    let filter = unpinned(view.derived.filter(), node);
    derive::set_filter(view, filter, cx);
}

fn clear_all(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let filter = cleared(view.derived.filter());
    derive::set_filter(view, filter, cx);
}

fn clear_revealed(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let filter = unrevealed(view.derived.filter());
    derive::set_filter(view, filter, cx);
}

/// The overrides in force, for the caller to place over the drawing; `None`
/// while there are none.
pub(crate) fn panel(view: &GraphPanel, palette: &Palette, cx: &Context<GraphPanel>) -> Option<Div> {
    let loaded = view.model.read(cx).loaded()?;
    let held = held(
        view.derived.filter(),
        &loaded.shared.names,
        &loaded.shared.facets,
        loaded.shared.labels.repos(),
    );
    if held.is_empty() {
        return None;
    }
    let reveals = held.iter().any(|held| held.pin == Pin::Revealed);
    Some(
        div()
            .w(PANEL_WIDTH)
            .max_w_full()
            .max_h(relative(MAX_HEIGHT_SHARE))
            .occlude()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_md()
            .bg(palette.container)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(palette.label)
                            .child(format!("{} pinned", held.len())),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .children(reveals.then(|| {
                                Button::new(
                                    "pins-clear-revealed",
                                    cx.listener(|view, _: &ClickEvent, _, cx| {
                                        clear_revealed(view, cx)
                                    }),
                                )
                                .label("clear revealed")
                                .tooltip("Unpin revealed nodes")
                                .compact()
                            }))
                            .child(
                                Button::new(
                                    "pins-clear",
                                    cx.listener(|view, _: &ClickEvent, _, cx| clear_all(view, cx)),
                                )
                                .label("clear")
                                .tooltip("Unpin all nodes")
                                .compact(),
                            ),
                    ),
            )
            .child(listed(view, held, palette, cx)),
    )
}

/// A row for each override in force.
fn listed(
    view: &GraphPanel,
    held: Vec<Held>,
    palette: &Palette,
    cx: &Context<GraphPanel>,
) -> impl IntoElement {
    let rows = div()
        .id("pins-list")
        .flex()
        .flex_col()
        .gap_1()
        .overflow_y_scroll()
        .track_scroll(view.pins.scroll.handle())
        .children(held.into_iter().map(|held| row(held, palette, cx)));
    scrolling(
        rows,
        |view| &view.pins.scroll,
        |view| &mut view.pins.scroll,
        palette,
        cx,
    )
}

/// One listed override, which a click takes back off.
fn row(held: Held, palette: &Palette, cx: &Context<GraphPanel>) -> Stateful<Div> {
    let node = held.node;
    div()
        .id(("pinned", node.0 as usize))
        .flex()
        .items_center()
        .gap_1()
        .cursor_pointer()
        .text_xs()
        .text_color(palette.label)
        .on_click(cx.listener(move |view, event: &ClickEvent, _, cx| {
            GESTURES.dispatch(Pointer::click(event), |intent| match intent {
                PinIntent::Unpin => {
                    unpin(view, node, cx);
                    true
                }
            });
        }))
        .child(div().flex_1().min_w_0().child(
            LabelLine::new(held.label, palette.colors.text_muted).hinted("label", held.written),
        ))
        .child(div().flex_none().child(label(held.pin)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_gesture_is_reachable_and_fits_the_help() {
        assert_eq!(GESTURES.unreachable(), []);
        assert_eq!(GESTURES.overlong(), []);
    }

    /// The override `filter` holds for `node`.
    fn pinned(filter: &Filter, node: Id) -> Option<Pin> {
        filter.pins.get(&node).copied()
    }
    use bviz_graph_layout::Kind;
    use bviz_graph_layout::facets::FacetsBuilder;
    use bviz_graph_layout::graph::{Graph, Node};
    use bviz_graph_layout::select::{Predicate, Rule, Values, Verdict};

    /// A graph of one target per package, each with an action under it.
    struct Fixture {
        graph: Graph,
        names: Names,
        facets: Facets,
    }

    impl Fixture {
        fn new() -> Self {
            let mut graph = Graph::new();
            let mut names = Names::default();
            let mut builder = FacetsBuilder::default();
            for package in ["app", "lib"] {
                let label = format!("//{package}:main");
                let target = names.intern(&format!("target:{label}"));
                graph.add_node(Node::new(target, ":main", Kind::Target, None));
                builder.describe_label(target, None, &label);
                let action = names.intern(&format!("{label}#compile"));
                graph.add_node(Node::new(action, "CppCompile", Kind::Action, Some(target)));
                builder.facets().inherit(action, target);
            }
            let (facets, _) = builder.seal();
            Fixture {
                graph,
                names,
                facets,
            }
        }

        fn id(&self, name: &str) -> Id {
            self.names.get(name).expect("an interned node")
        }

        fn verdict(&self, filter: &Filter, name: &str) -> Verdict {
            filter.evaluate(&self.graph).get(self.id(name))
        }
    }

    /// A filter hiding every action, which a pin has to stand against.
    fn hiding_actions() -> Filter {
        Filter {
            rules: vec![Rule {
                predicate: Predicate {
                    kinds: Values::new([Kind::Action]),
                    ..Predicate::default()
                },
                visibility: Visibility::Hidden,
            }],
            ..Filter::default()
        }
    }

    #[test]
    fn a_pin_shows_a_node_a_rule_hides() {
        let fixture = Fixture::new();
        let kept = fixture.id("//app:main#compile");
        let filter = toggle(&hiding_actions(), kept, Visibility::Shown);

        assert_eq!(
            fixture.verdict(&filter, "//app:main#compile"),
            Verdict::Shown
        );
        assert_eq!(
            fixture.verdict(&filter, "//lib:main#compile"),
            Verdict::Hidden
        );
    }

    /// A pin is read after the rules, so a rule written over a term it has
    /// nothing to do with leaves it deciding its node as before.
    #[test]
    fn a_pin_outlives_a_rule_change_that_has_nothing_to_do_with_it() {
        let fixture = Fixture::new();
        let dropped = fixture.id("target://lib:main");
        let pinning = toggle(&Filter::default(), dropped, Visibility::Hidden);

        assert_eq!(
            fixture.verdict(&pinning, "target://lib:main"),
            Verdict::Hidden
        );

        let reduced = Filter {
            rules: hiding_actions().rules,
            ..pinning.clone()
        };
        assert_eq!(
            pinned(&reduced, dropped),
            Some(Pin::Set(Visibility::Hidden))
        );
        assert_eq!(
            fixture.verdict(&reduced, "target://lib:main"),
            Verdict::Hidden
        );
        assert_eq!(
            fixture.verdict(&reduced, "target://app:main"),
            Verdict::Shown
        );
    }

    /// Dropping the pins leaves every node to the rules, and leaves the rules
    /// themselves standing.
    #[test]
    fn clearing_the_pins_gives_every_node_back_to_the_rules() {
        let fixture = Fixture::new();
        let kept = fixture.id("//app:main#compile");
        let filter = toggle(&hiding_actions(), kept, Visibility::Shown);

        let cleared = cleared(&filter);

        assert_eq!(cleared, hiding_actions());
        assert_eq!(
            fixture.verdict(&cleared, "//app:main#compile"),
            Verdict::Hidden
        );
    }

    #[test]
    fn a_pin_is_offered_on_a_box_in_the_words_it_is_listed_by() {
        assert_eq!(offer(Visibility::Hidden), "Always hide box");
    }

    /// Pinning what is already pinned that way is how an override is taken back
    /// off, and pinning it the other way replaces it.
    #[test]
    fn pinning_a_node_the_same_way_again_takes_the_override_off() {
        let node = Id(0);
        let shown = toggle(&Filter::default(), node, Visibility::Shown);

        assert_eq!(pinned(&shown, node), Some(Pin::Set(Visibility::Shown)));
        assert_eq!(
            pinned(&toggle(&shown, node, Visibility::Hidden), node),
            Some(Pin::Set(Visibility::Hidden))
        );
        assert_eq!(toggle(&shown, node, Visibility::Shown), Filter::default());
    }

    /// Every pin is listed under the label of the target its node belongs to, so
    /// an override on an action is found by the target it builds.
    #[test]
    fn a_pin_is_listed_under_the_label_of_the_target_it_belongs_to() {
        let fixture = Fixture::new();
        let filter = toggle(
            &toggle(
                &Filter::default(),
                fixture.id("//lib:main#compile"),
                Visibility::Shown,
            ),
            fixture.id("target://app:main"),
            Visibility::Hidden,
        );

        let listed: Vec<(String, Pin)> = held(
            &filter,
            &fixture.names,
            &fixture.facets,
            &WrittenRepos::default(),
        )
        .into_iter()
        .map(|held| (held.label.to_string(), held.pin))
        .collect();

        assert_eq!(
            listed,
            [
                ("//app:main".to_string(), Pin::Set(Visibility::Hidden)),
                ("//lib:main".to_string(), Pin::Set(Visibility::Shown)),
            ]
        );
    }

    /// A node revealed shows whatever the rules say, and is told from a pin
    /// the reader set; revealing another node keeps both, and withdrawing the
    /// reveals leaves the reader's pins alone.
    #[test]
    fn reveals_accumulate_beside_the_readers_pins() {
        let fixture = Fixture::new();
        let first = fixture.id("//app:main#compile");
        let second = fixture.id("//lib:main#compile");
        let kept = fixture.id("target://app:main");
        let pinned_by_reader = toggle(&hiding_actions(), kept, Visibility::Faded);

        let once = revealing(&pinned_by_reader, first);
        assert_eq!(pinned(&once, first), Some(Pin::Revealed));
        assert_eq!(fixture.verdict(&once, "//app:main#compile"), Verdict::Shown);

        let twice = revealing(&once, second);
        assert_eq!(pinned(&twice, first), Some(Pin::Revealed));
        assert_eq!(pinned(&twice, second), Some(Pin::Revealed));
        assert_eq!(pinned(&twice, kept), Some(Pin::Set(Visibility::Faded)));
        assert_eq!(
            fixture.verdict(&twice, "//app:main#compile"),
            Verdict::Shown
        );

        let withdrawn = unrevealed(&twice);
        assert_eq!(pinned(&withdrawn, first), None);
        assert_eq!(pinned(&withdrawn, second), None);
        assert_eq!(pinned(&withdrawn, kept), Some(Pin::Set(Visibility::Faded)));
    }

    /// Revealing a node the reader pinned leaves their pin deciding it, and
    /// asking to always show a revealed node makes the pin the reader's own.
    #[test]
    fn a_reveal_never_replaces_a_readers_pin() {
        let node = Id(0);
        let hidden = toggle(&Filter::default(), node, Visibility::Hidden);

        let revealed = revealing(&hidden, node);
        assert_eq!(revealed, hidden);

        let revealed = revealing(&Filter::default(), node);
        assert_eq!(pinned(&revealed, node), Some(Pin::Revealed));
        let kept = toggle(&revealed, node, Visibility::Shown);
        assert_eq!(pinned(&kept, node), Some(Pin::Set(Visibility::Shown)));
        assert_eq!(pinned(&unpinned(&kept, node), node), None);
    }
}
