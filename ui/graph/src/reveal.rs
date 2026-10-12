//! Moving the view onto one node another view names, and what to do when the
//! panel's filter or its dependency paths leave that node undrawn.

use crate::derive::{self, Hidden};
use crate::framing::{Frame, Held};
use crate::history::History;
use crate::mode::LayoutMode;
use crate::point::{point_label, written_name};
use crate::{GraphPanel, Palette, pins, why};
use bviz_graph_layout::Id;
use bviz_graph_layout::select::Hiding;
use bviz_ui_components::{Button, LabelLine, TruncatingLabel};
use bviz_ui_workspace::PanelEvent;
use gpui::{ClickEvent, Context, Div, actions, div, prelude::*};

actions!(
    bviz_graph_reveal,
    [
        /// Reveal offered node
        Accept,
    ]
);

/// What keeps a node out of the drawing, as a reader names it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HiddenBy {
    Paths,
    /// The reader's own pin.
    Pin,
    /// The query the filter is scoped to, which does not name the node.
    Query,
    /// The panel's boxes menu, which unwraps the node.
    Boxes,
    /// The node frames others and none of them is drawn.
    NothingToFrame,
}

impl HiddenBy {
    /// Whether a reveal can bring the node into the drawing.
    pub(crate) fn revealable(self) -> bool {
        self != HiddenBy::NothingToFrame
    }

    pub(crate) fn describe(self) -> &'static str {
        match self {
            HiddenBy::Paths => "hidden by the dependency paths drawn",
            HiddenBy::Pin => "hidden by its pin",
            HiddenBy::Query => "outside the query",
            HiddenBy::Boxes => "unwrapped by the boxes menu",
            HiddenBy::NothingToFrame => "hidden: nothing it holds is drawn",
        }
    }
}

/// What a reveal waiting on a derivation does once it lands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Ask {
    /// Show the node where it is drawn, and offer to reveal it otherwise.
    Show,
    /// Frame the node in the drawing as it is, opening nothing, and offer to
    /// reveal it where the drawing leaves it out.
    InPlace,
    /// Show the node whatever hides it, where anything can.
    Anyway,
}

/// Where a request to show one node stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Reveal {
    Idle,
    /// A derivation in flight decides whether the node is drawn, so the
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
fn named(hidden: Hidden) -> HiddenBy {
    match hidden {
        Hidden::OffPaths => HiddenBy::Paths,
        Hidden::Filtered(hiding) => match hiding {
            Hiding::Pin => HiddenBy::Pin,
            Hiding::Scope => HiddenBy::Query,
            Hiding::Rule(_) => HiddenBy::Boxes,
            Hiding::NothingToFrame => HiddenBy::NothingToFrame,
        },
    }
}

/// One place the panel's view has been moved to.
#[derive(Clone, Copy, PartialEq)]
enum Stop {
    Node(Id),
    /// Where the view stood, with no node selected, as the first jump left it.
    View(Held),
}

/// The places the panel has been moved to by name, walked back and forth.
#[derive(Default)]
pub(crate) struct Jumps(Option<History<Stop>>);

impl Jumps {
    /// Records a jump onto `id`; the first also records `origin`, where the
    /// view stood as it left.
    fn visit(&mut self, id: Id, origin: Option<Stop>) {
        let onto = Stop::Node(id);
        match (&mut self.0, origin) {
            (Some(history), _) => history.push(onto),
            (None, Some(origin)) => {
                let mut history = History::new(origin);
                history.push(onto);
                self.0 = Some(history);
            }
            (None, None) => self.0 = Some(History::new(onto)),
        }
    }

    fn back(&mut self) -> Option<Stop> {
        self.0.as_mut()?.back().copied()
    }

    fn forward(&mut self) -> Option<Stop> {
        self.0.as_mut()?.forward().copied()
    }
}

/// Selects `id` and moves the view onto it, opening what holds it in a layered
/// drawing, and asks for the panel to be shown. A node the drawing leaves out is
/// offered to be revealed anyway instead. The node is the jump [`back`] returns
/// from.
pub(crate) fn reveal(view: &mut GraphPanel, id: Id, cx: &mut Context<GraphPanel>) {
    let origin = match view.selection.node() {
        Some(selected) => Some(Stop::Node(selected)),
        None => view.framing.hold().map(Stop::View),
    };
    view.jumps.visit(id, origin);
    revisit(view, Stop::Node(id), cx);
}

/// Returns to where the view stood before the last jump.
pub(crate) fn back(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    if let Some(stop) = view.jumps.back() {
        revisit(view, stop, cx);
    }
}

/// Returns to where the view stood after the jump [`back`] returned from.
pub(crate) fn forward(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    if let Some(stop) = view.jumps.forward() {
        revisit(view, stop, cx);
    }
}

/// Moves the view to `stop`, leaving the jumps as they stand: a node as
/// [`reveal`] moves onto it, and a view the first jump left by easing back
/// to it with nothing selected.
fn revisit(view: &mut GraphPanel, stop: Stop, cx: &mut Context<GraphPanel>) {
    match stop {
        Stop::Node(id) => ask(view, id, Ask::Show, cx),
        Stop::View(held) => {
            dismiss(view);
            view.selection.clear();
            view.framing.return_to(held);
        }
    }
    cx.emit(PanelEvent::Activate);
    view.invalidate_canvas(cx);
}

/// As [`reveal`], bringing a node the drawing leaves out into it: a node off
/// the dependency paths drawn returns the drawing to the filter, one the
/// reader's own pin hides loses that pin, one the panel's own rules hide is
/// revealed in this panel alone, and one the rest of the filter hides is pinned
/// with [`pins::reveal`]. A node that cannot be revealed stays
/// offered, with the reason.
pub(crate) fn reveal_anyway(view: &mut GraphPanel, id: Id, cx: &mut Context<GraphPanel>) {
    ask(view, id, Ask::Anyway, cx);
    view.invalidate_canvas(cx);
}

/// Moves the view onto `id` as [`Ask::InPlace`] asks, without recording a jump
/// or asking for the panel to be shown.
pub(crate) fn show_in_place(view: &mut GraphPanel, id: Id, cx: &mut Context<GraphPanel>) {
    ask(view, id, Ask::InPlace, cx);
    view.invalidate_canvas(cx);
}

fn ask(view: &mut GraphPanel, id: Id, asked: Ask, cx: &mut Context<GraphPanel>) {
    if view.derived.restricting() {
        view.reveal = Reveal::Waiting(id, asked);
        return;
    }
    match (hidden_by(view, id), asked) {
        (None, Ask::InPlace) => match standing_for(view, id) {
            Some(shown) => {
                view.reveal = Reveal::Idle;
                view.selection.select(shown);
                view.framing.request_frame(Frame::Node(shown));
            }
            None if view.derived.deriving() => view.reveal = Reveal::Waiting(id, Ask::InPlace),
            None => view.reveal = Reveal::Idle,
        },
        (None, _) => {
            view.reveal = Reveal::Idle;
            show(view, id, cx);
        }
        (Some(HiddenBy::Pin), Ask::Anyway) => {
            // What the rules make of the node once unpinned is asked afresh.
            view.reveal = Reveal::Waiting(id, Ask::Show);
            pins::unpin(view, id, cx);
        }
        (Some(by), Ask::Anyway) if by.revealable() => {
            view.reveal = Reveal::Waiting(id, Ask::Anyway);
            // Set ahead of the request below, whose derivation reads it.
            if view.mode == LayoutMode::Layered {
                view.derived.open_onto_next(id);
            }
            match by {
                HiddenBy::Paths => why::clear(view, cx),
                HiddenBy::Boxes => derive::reveal_in_layout(view, id, cx),
                _ => pins::reveal(view, id, cx),
            }
        }
        (Some(by), _) => view.reveal = Reveal::Offered(id, by),
    }
}

/// The box standing for `id` in the drawing as it is: in a layered drawing the
/// innermost drawn box holding it, which opens none and so lays nothing out
/// afresh.
fn standing_for(view: &GraphPanel, id: Id) -> Option<Id> {
    match view.mode {
        LayoutMode::Layered => derive::drawn_holder(view, id),
        LayoutMode::Map => Some(id),
    }
}

fn show(view: &mut GraphPanel, id: Id, cx: &mut Context<GraphPanel>) {
    view.selection.select(id);
    if view.mode == LayoutMode::Layered {
        derive::expand_to(view, id, cx);
    }
    view.framing.request_frame(Frame::Node(id));
}

/// Answers a reveal that waited on the derivation that has landed, and puts
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

/// Takes the card's own action on the node it offers to show, for the key
/// answering it; a key no card stands for is left to what else binds it.
pub(crate) fn accept(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    match view.reveal {
        Reveal::Offered(node, by) if by.revealable() => reveal_anyway(view, node, cx),
        _ => cx.propagate(),
    }
}

/// Puts away the offer to reveal a hidden node, and a reveal still waiting on
/// a derivation; reports whether either was pending.
pub(crate) fn dismiss(view: &mut GraphPanel) -> bool {
    let pending = view.reveal != Reveal::Idle;
    view.reveal = Reveal::Idle;
    pending
}

/// Forgets the reveal and the jumps, which named nodes of a graph that has
/// been replaced.
pub(crate) fn reset(view: &mut GraphPanel) {
    view.reveal = Reveal::Idle;
    view.jumps = Jumps::default();
}

/// The offer to reveal a node the drawing leaves out, naming what hides it, for
/// the caller to place over the drawing; `None` while none is offered.
pub(crate) fn card(view: &GraphPanel, palette: &Palette, cx: &Context<GraphPanel>) -> Option<Div> {
    let Reveal::Offered(node, by) = view.reveal else {
        return None;
    };
    let loaded = view.model.read(cx).loaded()?;
    let shared = &loaded.shared;
    let name = point_label(node, shared);
    let written = written_name(&shared.names, &shared.facets, shared.labels.repos(), node);
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
            .child(LabelLine::new(name, palette.colors.text_muted).hinted("reveal-name", written))
            .child(TruncatingLabel::new("reveal-reason", by.describe()))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_1()
                    .children(by.revealable().then(|| {
                        let reveal = Button::new(
                            "reveal-anyway",
                            cx.listener(move |view, _: &ClickEvent, _, cx| {
                                reveal_anyway(view, node, cx)
                            }),
                        )
                        .keys_for(&Accept, &view.focus_handle)
                        .compact();
                        match by {
                            HiddenBy::Pin => reveal.label("unpin").tooltip("Unpin node"),
                            _ => reveal.label("reveal anyway"),
                        }
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
                        .tooltip("Close card")
                        .keys_for(&crate::Dismiss, &view.focus_handle)
                        .compact(),
                    ),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filtered(hiding: Hiding) -> HiddenBy {
        named(Hidden::Filtered(hiding))
    }

    /// The panel's own rules are the boxes menu's, so the reader is told to
    /// turn that back.
    #[test]
    fn a_rule_is_named_as_the_boxes_menu() {
        assert_eq!(filtered(Hiding::Rule(0)), HiddenBy::Boxes);
    }

    /// A pin outranks the scope, so revealing a node the query leaves out
    /// draws it.
    #[test]
    fn a_node_outside_the_query_is_hidden_by_it_and_revealable() {
        let by = filtered(Hiding::Scope);
        assert_eq!(by, HiddenBy::Query);
        assert!(by.revealable());
    }

    /// The node a stop names, where it names one.
    fn node(stop: Option<Stop>) -> Option<Id> {
        match stop? {
            Stop::Node(id) => Some(id),
            Stop::View(_) => None,
        }
    }

    #[test]
    fn the_jumps_walk_back_and_forth_and_a_new_jump_drops_those_ahead() {
        let mut jumps = Jumps::default();
        assert!(jumps.back().is_none());
        for id in [1, 2, 3] {
            jumps.visit(Id(id), None);
        }
        assert_eq!(node(jumps.back()), Some(Id(2)));
        assert_eq!(node(jumps.back()), Some(Id(1)));
        assert!(jumps.back().is_none());
        assert_eq!(node(jumps.forward()), Some(Id(2)));
        jumps.visit(Id(4), None);
        assert!(jumps.forward().is_none());
        assert_eq!(node(jumps.back()), Some(Id(2)));
    }

    /// The first jump keeps where the view stood as it left, so stepping back
    /// after a single jump returns there; later jumps leave from a stop the
    /// history holds.
    #[test]
    fn the_first_jump_keeps_where_the_view_stood() {
        let mut jumps = Jumps::default();
        jumps.visit(Id(5), Some(Stop::Node(Id(1))));
        jumps.visit(Id(6), Some(Stop::Node(Id(9))));
        assert_eq!(node(jumps.back()), Some(Id(5)));
        assert_eq!(node(jumps.back()), Some(Id(1)));
        assert!(jumps.back().is_none());
    }

    #[test]
    fn only_a_frame_with_nothing_drawn_inside_cannot_be_revealed() {
        assert!(!filtered(Hiding::NothingToFrame).revealable());
        for revealable in [
            HiddenBy::Paths,
            filtered(Hiding::Pin),
            filtered(Hiding::Scope),
        ] {
            assert!(revealable.revealable());
        }
    }
}
