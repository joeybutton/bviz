//! Per-node overrides: the nodes a reader has shown or hidden on their own.
//!
//! A pin decides the node it names whatever the rules say, which is what lets
//! one node kept or dropped by hand outlast a change to a rule that has nothing
//! to do with it. Nothing derives a pin, so every one in force is listed and can
//! be taken back, or an override would be a state a reader could reach and not
//! leave.

use crate::{GraphPanel, Palette, checklist};
use bviz_graph_layout::select::labels::carried_label;
use bviz_graph_layout::select::{Filter, Visibility};
use bviz_graph_layout::{Facets, Id, Names};
use bviz_ui_components::{Button, ListScroll, SCROLLBAR_THICKNESS, list_scrollbar};
use gpui::{
    ClickEvent, Context, Div, IntoElement, MouseButton, MouseDownEvent, Pixels, SharedString,
    Stateful, div, prelude::*, px, relative,
};
use std::collections::HashMap;

/// Width the list of pins is drawn at, over the drawing it names boxes of.
const PANEL_WIDTH: Pixels = px(320.);

/// The share of the height it is placed in the card runs to before its rows
/// scroll, so that however many overrides are in force the card stays clear of
/// the canvas's foot and every row it lists can be reached.
const MAX_HEIGHT_SHARE: f32 = 0.6;

/// Side of the mark a pinned row carries, and of the space a row without one
/// leaves so the labels line up.
const MARK: Pixels = px(12.);

/// How a reader is offered the override `pin`.
fn label(pin: Visibility) -> &'static str {
    match pin {
        Visibility::Shown => "always show",
        Visibility::Hidden => "always hide",
        Visibility::Faded => "always fade",
    }
}

/// `filter` with `node` pinned to `pin`, or with its pin dropped where it
/// already carries that one, so the same gesture puts the override away.
pub(crate) fn toggle(filter: &Filter, node: Id, pin: Visibility) -> Filter {
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

/// The override `filter` holds for `node`.
pub(crate) fn pinned(filter: &Filter, node: Id) -> Option<Visibility> {
    filter.pins.get(&node).copied()
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
    /// The canonical label of the target the node belongs to, and its node name
    /// where it belongs to none.
    label: SharedString,
    pin: Visibility,
}

/// The overrides `filter` holds, by the name each is listed under.
fn held(filter: &Filter, names: &Names, facets: &Facets) -> Vec<Held> {
    let mut held: Vec<Held> = Vec::with_capacity(filter.pins.len());
    held.extend(filter.pins.iter().map(|(&node, &pin)| {
        Held {
            node,
            label: carried_label(names, facets, node)
                .unwrap_or_else(|| names.resolve(node))
                .to_string()
                .into(),
            pin,
        }
    }));
    held.sort_by(|one, other| (&one.label, one.node.0).cmp(&(&other.label, other.node.0)));
    held
}

/// Pins `node` to `pin`, or takes that override back off.
pub(crate) fn set(view: &mut GraphPanel, node: Id, pin: Visibility, cx: &mut Context<GraphPanel>) {
    let filter = toggle(view.derived.filter(), node, pin);
    checklist::set_filter(view, filter, cx);
}

fn clear_all(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let filter = cleared(view.derived.filter());
    checklist::set_filter(view, filter, cx);
}

/// The mark a row carries while it is pinned, and the space it leaves where it
/// is not, so the labels of a list line up either way.
pub(crate) fn mark(pinned: bool, palette: &Palette) -> Div {
    div().size(MARK).flex_none().children(pinned.then(|| {
        bviz_ui_assets::icon("check")
            .size(MARK)
            .text_color(palette.label)
    }))
}

/// The override offered on the box a secondary click has selected: dropping it
/// from the drawing, which is what a reader reaches for on a box in front of
/// them. Its opposite is offered where it can be aimed at a box the drawing does
/// not hold, on a search result. `None` while no box is selected.
pub(crate) fn control(view: &GraphPanel, cx: &mut Context<GraphPanel>) -> Option<Button> {
    let node = view.selection.node()?;
    Some(
        Button::new(
            "pin-selected",
            cx.listener(move |view, _: &ClickEvent, _, cx| set(view, node, Visibility::Hidden, cx)),
        )
        .ghost()
        .label(label(Visibility::Hidden)),
    )
}

/// The overrides in force, for the caller to place over the drawing; `None`
/// while there are none.
pub(crate) fn panel(
    view: &GraphPanel,
    palette: &Palette,
    cx: &mut Context<GraphPanel>,
) -> Option<Div> {
    let loaded = view.model.read(cx).loaded()?;
    let held = held(
        view.derived.filter(),
        &loaded.shared.names,
        &loaded.shared.facets,
    );
    if held.is_empty() {
        return None;
    }
    Some(
        div()
            .w(PANEL_WIDTH)
            .max_w_full()
            .max_h(relative(MAX_HEIGHT_SHARE))
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, e: &MouseDownEvent, _, cx| {
                    view.pins.scroll.press(e.position);
                    cx.notify();
                }),
            )
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
                        Button::new(
                            "pins-clear",
                            cx.listener(|view, _: &ClickEvent, _, cx| clear_all(view, cx)),
                        )
                        .label("clear")
                        .compact(),
                    ),
            )
            .child(listed(view, held, palette, cx)),
    )
}

/// The rows, scrollable within whatever height the card is left, with a scroll
/// bar over their right edge that the rows keep clear of.
fn listed(
    view: &GraphPanel,
    held: Vec<Held>,
    palette: &Palette,
    cx: &mut Context<GraphPanel>,
) -> impl IntoElement {
    let rows = div()
        .id("pins-list")
        .flex()
        .flex_col()
        .gap_1()
        .w_full()
        .pr(px(SCROLLBAR_THICKNESS))
        .overflow_y_scroll()
        .track_scroll(view.pins.scroll.handle())
        .children(held.into_iter().map(|held| row(held, palette, cx)));
    div()
        .relative()
        .flex()
        .flex_1()
        .min_h_0()
        .child(rows)
        .child(list_scrollbar(
            cx.entity(),
            |view: &GraphPanel| &view.pins.scroll,
            |view: &mut GraphPanel| &mut view.pins.scroll,
            palette.scroll,
        ))
}

/// One listed override, which a click takes back off.
fn row(held: Held, palette: &Palette, cx: &mut Context<GraphPanel>) -> Stateful<Div> {
    let (node, pin) = (held.node, held.pin);
    div()
        .id(("pinned", node.0 as usize))
        .flex()
        .items_center()
        .gap_1()
        .cursor_pointer()
        .text_xs()
        .text_color(palette.label)
        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| set(view, node, pin, cx)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(held.label),
        )
        .child(div().flex_none().child(label(pin)))
}

#[cfg(test)]
mod tests {
    use super::*;
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
            filter
                .evaluate(&self.graph, &self.names, &self.facets)
                .get(self.id(name))
        }
    }

    /// A filter hiding every action, which is the sort of reduction a checklist
    /// writes and a pin has to stand against.
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
        assert_eq!(pinned(&reduced, dropped), Some(Visibility::Hidden));
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

    /// Pinning what is already pinned that way is how an override is taken back
    /// off, and pinning it the other way replaces it.
    #[test]
    fn pinning_a_node_the_same_way_again_takes_the_override_off() {
        let node = Id(0);
        let shown = toggle(&Filter::default(), node, Visibility::Shown);

        assert_eq!(pinned(&shown, node), Some(Visibility::Shown));
        assert_eq!(
            pinned(&toggle(&shown, node, Visibility::Hidden), node),
            Some(Visibility::Hidden)
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

        let listed: Vec<(String, Visibility)> = held(&filter, &fixture.names, &fixture.facets)
            .into_iter()
            .map(|held| (held.label.to_string(), held.pin))
            .collect();

        assert_eq!(
            listed,
            [
                ("//app:main".to_string(), Visibility::Hidden),
                ("//lib:main".to_string(), Visibility::Shown),
            ]
        );
    }
}
