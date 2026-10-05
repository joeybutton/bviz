//! Moving the view onto one node another view names, and what to do when the
//! panel's filter or its dependency paths leave that node undrawn.

use crate::derive::{self, Hidden};
use crate::framing::Frame;
use crate::mode::LayoutMode;
use crate::rules::{self, Term};
use crate::search::{self, Act};
use crate::{GraphPanel, Palette, column, pins, why};
use bviz_graph_layout::Id;
use bviz_graph_layout::select::Hiding;
use bviz_ui_components::Button;
use bviz_ui_workspace::PanelEvent;
use gpui::{ClickEvent, Context, Div, div, prelude::*};

/// What keeps a node out of the drawing, as a reader names it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HiddenBy {
    Paths,
    /// The reader's own pin.
    Pin,
    /// The search's pattern, hiding what it names or every node it does not.
    Search,
    /// The rule a toolbar control writes over its term.
    Control(Term),
    /// A rule no control wrote, at its index among the filter's rules.
    Rule(u32),
    /// The filter's base, which hides every node no rule shows.
    Base,
    /// The node frames others and none of them is drawn.
    NothingToFrame,
}

impl HiddenBy {
    /// Whether a reveal can bring the node into the drawing.
    pub(crate) fn revealable(self) -> bool {
        matches!(self, HiddenBy::Paths | HiddenBy::Pin)
    }

    pub(crate) fn describe(self) -> &'static str {
        match self {
            HiddenBy::Paths => "hidden by the dependency paths drawn",
            HiddenBy::Pin => "hidden by its pin",
            HiddenBy::Search => "hidden by the search",
            HiddenBy::Control(Term::Kind) => "hidden by the class filter",
            HiddenBy::Control(Term::RepoCategory | Term::Repo) => "hidden by the repository filter",
            HiddenBy::Control(Term::RuleCategory | Term::Mnemonic | Term::ToolConfiguration) => {
                "hidden by a preset"
            }
            HiddenBy::Rule(_) => "hidden by a filter rule",
            HiddenBy::Base => "hidden by the filter",
            HiddenBy::NothingToFrame => "hidden: nothing it holds is drawn",
        }
    }
}

/// What a reveal waiting on a restriction does once it lands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Ask {
    /// Show the node where it is drawn, and offer to reveal it otherwise.
    Show,
    /// Show the node whatever hides it, where anything can.
    Anyway,
}

/// Where a request to show one node stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Reveal {
    Idle,
    /// A restriction in flight decides whether the node is drawn, so the
    /// request is answered once it lands.
    Waiting(Id, Ask),
    /// The node was found hidden by what this names, and the panel offers to
    /// reveal it anyway.
    Offered(Id, HiddenBy),
}

/// What keeps the node `id` out of the drawing the panel holds; `None` where it
/// is drawn, or folded into a box a reveal opens.
pub(crate) fn hidden_by(view: &GraphPanel, id: Id) -> Option<HiddenBy> {
    view.derived.hidden(id).map(named)
}

/// What `hidden` names, as a reader names it.
fn named(hidden: Hidden<'_>) -> HiddenBy {
    match hidden {
        Hidden::OffPaths => HiddenBy::Paths,
        Hidden::Filtered { filter, hiding } => match hiding {
            Hiding::Pin => HiddenBy::Pin,
            Hiding::Rule(index) => {
                let rule = &filter.rules[index as usize];
                if search::wrote(rule) {
                    HiddenBy::Search
                } else {
                    rules::control(rule).map_or(HiddenBy::Rule(index), HiddenBy::Control)
                }
            }
            Hiding::Base if search::acting(filter) == Some(Act::Only) => HiddenBy::Search,
            Hiding::Base => HiddenBy::Base,
            Hiding::NothingToFrame => HiddenBy::NothingToFrame,
        },
    }
}

/// Selects `id` and moves the view onto it, opening what holds it in a layered
/// drawing, and asks for the panel to be shown. A node the drawing leaves out is
/// offered to be revealed anyway instead.
pub(crate) fn reveal(view: &mut GraphPanel, id: Id, cx: &mut Context<GraphPanel>) {
    ask(view, id, Ask::Show, cx);
    cx.emit(PanelEvent::Activate);
    view.invalidate_canvas(cx);
}

/// As [`reveal`], bringing a node the drawing leaves out into it: a node off
/// the dependency paths drawn returns the drawing to the filter, and one the
/// reader's own pin hides loses that pin. A node that cannot be revealed stays
/// offered, with the reason.
pub(crate) fn reveal_anyway(view: &mut GraphPanel, id: Id, cx: &mut Context<GraphPanel>) {
    ask(view, id, Ask::Anyway, cx);
    view.invalidate_canvas(cx);
}

/// Selects and frames the box standing for `id` in the drawing as it is: in
/// a layered drawing the innermost drawn box holding it, which opens none and
/// so lays nothing out afresh.
pub(crate) fn show_in_place(view: &mut GraphPanel, id: Id, cx: &mut Context<GraphPanel>) {
    let shown = match view.mode {
        LayoutMode::Layered => derive::drawn_holder(view, id),
        LayoutMode::Map => Some(id),
    };
    let Some(shown) = shown else {
        return;
    };
    view.selection.select(shown);
    view.framing.request_frame(Frame::Node(shown));
    view.invalidate_canvas(cx);
}

fn ask(view: &mut GraphPanel, id: Id, asked: Ask, cx: &mut Context<GraphPanel>) {
    if view.derived.restricting() {
        view.reveal = Reveal::Waiting(id, asked);
        return;
    }
    match (hidden_by(view, id), asked) {
        (None, _) => {
            view.reveal = Reveal::Idle;
            show(view, id, cx);
        }
        (Some(HiddenBy::Pin), Ask::Anyway) => {
            // What the rules make of the node once unpinned is asked afresh.
            view.reveal = Reveal::Waiting(id, Ask::Show);
            pins::unpin(view, id, cx);
        }
        (Some(HiddenBy::Paths), Ask::Anyway) => {
            view.reveal = Reveal::Waiting(id, Ask::Anyway);
            // Set ahead of the request below, whose derivation reads it.
            if view.mode == LayoutMode::Layered {
                view.derived.open_onto_next(id);
            }
            why::clear(view, cx);
        }
        (Some(by), _) => view.reveal = Reveal::Offered(id, by),
    }
}

fn show(view: &mut GraphPanel, id: Id, cx: &mut Context<GraphPanel>) {
    view.selection.select(id);
    if view.mode == LayoutMode::Layered {
        derive::expand_to(view, id, cx);
    }
    view.framing.request_frame(Frame::Node(id));
}

/// Answers a reveal that waited on the restriction that has landed, and puts
/// away an offer to reveal a node the drawing that landed holds.
pub(crate) fn landed(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    if view.derived.restricting() {
        return;
    }
    match view.reveal {
        Reveal::Waiting(id, asked) => ask(view, id, asked, cx),
        Reveal::Offered(id, _) => {
            view.reveal = match hidden_by(view, id) {
                Some(by) => Reveal::Offered(id, by),
                None => Reveal::Idle,
            }
        }
        Reveal::Idle => {}
    }
}

/// Puts away the offer to reveal a hidden node, and a reveal still waiting on
/// a restriction; reports whether either was pending.
pub(crate) fn dismiss(view: &mut GraphPanel) -> bool {
    let pending = view.reveal != Reveal::Idle;
    view.reveal = Reveal::Idle;
    pending
}

/// Forgets the reveal, which named a node of a graph that has been replaced.
pub(crate) fn reset(view: &mut GraphPanel) {
    view.reveal = Reveal::Idle;
}

/// The offer to reveal a node the drawing leaves out, naming what hides it, for
/// the caller to place over the drawing; `None` while none is offered.
pub(crate) fn card(
    view: &GraphPanel,
    palette: &Palette,
    cx: &mut Context<GraphPanel>,
) -> Option<Div> {
    let Reveal::Offered(node, by) = view.reveal else {
        return None;
    };
    let loaded = view.model.read(cx).loaded()?;
    let name = pins::name(&loaded.shared.names, &loaded.shared.facets, node);
    Some(
        div()
            .w_full()
            .occlude()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_md()
            .bg(palette.container)
            .text_xs()
            .text_color(palette.label)
            .child(column::line(name))
            .child(column::line(by.describe()))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_1()
                    .children(by.revealable().then(|| {
                        Button::new(
                            "reveal-anyway",
                            cx.listener(move |view, _: &ClickEvent, _, cx| {
                                reveal_anyway(view, node, cx)
                            }),
                        )
                        .label(match by {
                            HiddenBy::Pin => "unpin",
                            _ => "reveal anyway",
                        })
                        .compact()
                    }))
                    .child(
                        Button::new(
                            "reveal-close",
                            cx.listener(|view, _: &ClickEvent, _, cx| {
                                dismiss(view);
                                cx.notify();
                            }),
                        )
                        .label("close")
                        .compact(),
                    ),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::Value;
    use bviz_graph_layout::Kind;
    use bviz_graph_layout::select::pattern::Pattern;
    use bviz_graph_layout::select::{Filter, Predicate, Rule, Visibility};

    fn filtered(filter: &Filter, hiding: Hiding) -> HiddenBy {
        named(Hidden::Filtered { filter, hiding })
    }

    /// A rule is named by the control that wrote it, so the reader is told
    /// which control to turn back.
    #[test]
    fn a_rule_is_named_by_the_control_that_wrote_it() {
        let classes = rules::set_hidden(&Filter::default(), &Value::Kind(Kind::Action), true);
        let searched = search::applying(&classes, &Pattern::parse("//foo"), Act::Hide);
        let class_rule = searched
            .rules
            .iter()
            .position(|rule| rules::control(rule) == Some(Term::Kind))
            .expect("the class rule") as u32;
        let search_rule = searched
            .rules
            .iter()
            .position(search::wrote)
            .expect("the search rule") as u32;

        assert_eq!(
            filtered(&searched, Hiding::Rule(class_rule)),
            HiddenBy::Control(Term::Kind)
        );
        assert_eq!(
            filtered(&searched, Hiding::Rule(search_rule)),
            HiddenBy::Search
        );
    }

    #[test]
    fn a_rule_no_control_wrote_is_named_by_its_index() {
        let filter = Filter {
            rules: vec![Rule {
                predicate: Predicate {
                    kinds: bviz_graph_layout::select::Values::new([Kind::Action]),
                    testonly: Some(true),
                    ..Predicate::default()
                },
                visibility: Visibility::Hidden,
            }],
            ..Filter::default()
        };

        assert_eq!(filtered(&filter, Hiding::Rule(0)), HiddenBy::Rule(0));
    }

    /// Showing only what the search names hides the rest by the base it
    /// writes, which is the search's doing.
    #[test]
    fn the_base_a_search_showing_only_its_matches_writes_is_the_search() {
        let only = search::applying(&Filter::default(), &Pattern::parse("//foo"), Act::Only);

        assert_eq!(filtered(&only, Hiding::Base), HiddenBy::Search);
        assert_eq!(filtered(&Filter::default(), Hiding::Base), HiddenBy::Base);
    }

    #[test]
    fn only_the_paths_and_a_readers_pin_can_be_undone_by_a_reveal() {
        for revealable in [HiddenBy::Paths, filtered(&Filter::default(), Hiding::Pin)] {
            assert!(revealable.revealable());
        }
        for kept in [Hiding::Base, Hiding::NothingToFrame] {
            assert!(!filtered(&Filter::default(), kept).revealable());
        }
    }
}
