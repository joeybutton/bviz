//! What keeps a node out of a view of the graph, and the card any view stands
//! to offer it shown another way.

use crate::model::GraphModel;
use crate::point::{point_label, written_name};
use crate::{Palette, pins, reveal};
use bviz_graph_layout::Id;
use bviz_graph_layout::query_lang::{Exclusion, Stack};
use bviz_graph_layout::select::Hiding;
use bviz_graph_layout::select::Visibility;
use bviz_ui_components::{Button, LabelLine, TruncatingLabel};
use gpui::{
    Action, App, ClickEvent, Context, Div, Entity, FocusHandle, SharedString, Window, div,
    prelude::*,
};

/// What keeps a node out of a view, as a reader names it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HiddenBy {
    /// The dependency paths a graph panel draws in its filter's place.
    Paths,
    /// The reader's own pin.
    Pin,
    /// The query the filter is scoped to, which does not name the node.
    Query,
    /// A graph panel's boxes menu, which unwraps the node.
    Boxes,
    /// The node frames others and none of them is drawn.
    NothingToFrame,
}

impl HiddenBy {
    /// What a filter hiding a node by `hiding` names; a rule of the filter is
    /// a graph panel's boxes menu, the one control that adds one.
    pub(crate) fn of(hiding: Hiding) -> Self {
        match hiding {
            Hiding::Pin => HiddenBy::Pin,
            Hiding::Scope => HiddenBy::Query,
            Hiding::Rule(_) => HiddenBy::Boxes,
            Hiding::NothingToFrame => HiddenBy::NothingToFrame,
        }
    }

    /// Whether a reveal can bring the node into the view.
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

/// What a card says keeps `node` out of a view, hidden `by` that: for a node
/// outside the query, the chip leaving it out once `model` has named it.
pub(crate) fn reason(by: HiddenBy, node: Id, model: &GraphModel) -> SharedString {
    let named = (by == HiddenBy::Query)
        .then(|| model.exclusion(node))
        .flatten();
    let Some((exclusion, stack)) = named else {
        return by.describe().into();
    };
    excluded(exclusion, stack).map_or_else(|| by.describe().into(), SharedString::from)
}

/// What a card says of a node `exclusion` leaves out of what `stack` names.
fn excluded(exclusion: Exclusion, stack: &Stack) -> Option<String> {
    match exclusion {
        Exclusion::Chip(chip) => Some(format!("left out by chip ‹{}›", stack.chip(chip)?.text())),
        Exclusion::Sources => {
            let mut enabled = stack.sources().iter().filter(|chip| chip.enabled());
            Some(match (enabled.next(), enabled.next()) {
                (Some(only), None) => format!("not named by chip ‹{}›", only.text()),
                _ => "named by no source chip".to_owned(),
            })
        }
    }
}

/// The card offering to show the node `name` names, written `written`, which
/// `reason` says the view leaves out, by the `offers` it stands.
pub(crate) fn card(
    name: impl Into<SharedString>,
    written: impl Into<SharedString>,
    reason: impl Into<SharedString>,
    offers: impl IntoIterator<Item = Button>,
    palette: &Palette,
) -> Div {
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
        .child(LabelLine::new(name, palette.colors.text_muted).hinted("hidden-name", written))
        .child(TruncatingLabel::new("hidden-reason", reason))
        .child(div().flex().justify_end().gap_1().children(offers))
}

/// A node a view drawing the document's filter was asked to land on and
/// shows no part of, offered to be shown another way.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Offer {
    pub(crate) node: Id,
    pub(crate) unshown: Unshown,
}

/// Why a view shows no part of a node.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Unshown {
    /// The filter hides `point`, which the view would draw for the node.
    Hidden { point: Id, by: HiddenBy },
    /// Nothing the view draws stands for the node, for the reason given.
    Absent(&'static str),
}

/// What a reader can do with an [`Offer`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Answer {
    /// Show the point the filter hides.
    Reveal,
    /// Have the graph panel the document defers to show the node.
    ShowInGraph,
    Close,
}

impl Offer {
    /// What the key accepting the offer answers; none where the card offers
    /// only to close.
    fn accepted(self, model: &GraphModel) -> Option<Answer> {
        match self.unshown {
            Unshown::Hidden { by, .. } => by.revealable().then_some(Answer::Reveal),
            Unshown::Absent(_) => model.reveals().then_some(Answer::ShowInGraph),
        }
    }
}

/// The card a view drawing the document's filter stands over a node a jump
/// could not land on, and the jump waiting for the view to draw the filter
/// as it changes, which an answer to the card or a pin can ask for.
pub(crate) struct Offering {
    model: Entity<GraphModel>,
    offer: Option<Offer>,
    pending: Option<Id>,
}

impl Offering {
    pub(crate) fn new(model: Entity<GraphModel>) -> Self {
        Offering {
            model,
            offer: None,
            pending: None,
        }
    }

    pub(crate) fn offer(&self) -> Option<Offer> {
        self.offer
    }

    /// Stands the card offering `node`, which the view shows no part of for
    /// the reason `unshown` gives, naming the chip leaving its point out
    /// where that is the query.
    pub(crate) fn stand(&mut self, node: Id, unshown: Unshown, cx: &mut App) {
        if let Unshown::Hidden {
            point,
            by: HiddenBy::Query,
        } = unshown
        {
            (self.model).update(cx, |model, cx| model.name_exclusion(point, cx));
        }
        self.offer = Some(Offer { node, unshown });
    }

    /// Puts away the card and any jump waiting, as a new jump or a graph read
    /// afresh does.
    pub(crate) fn clear(&mut self) {
        self.offer = None;
        self.pending = None;
    }

    /// Puts away the card, reporting whether one stood.
    pub(crate) fn dismiss(&mut self) -> bool {
        self.offer.take().is_some()
    }

    /// Has the jump to `node` wait for the view to draw the filter afresh.
    pub(crate) fn wait_for(&mut self, node: Id) {
        self.pending = Some(node);
    }

    /// The jump waiting for the view's drawing, which has landed.
    pub(crate) fn take_waiting(&mut self) -> Option<Id> {
        self.pending.take()
    }

    /// Takes up a drawing landed through another filter, through which the
    /// card's node is `unshown`: the card goes where the node now lands, and
    /// stands afresh where the reason changed.
    pub(crate) fn redrawn(&mut self, unshown: Option<Unshown>, cx: &mut App) {
        let Some(offer) = self.offer else {
            return;
        };
        match unshown {
            Some(unshown) if unshown != offer.unshown => self.stand(offer.node, unshown, cx),
            Some(_) => {}
            None => self.offer = None,
        }
    }

    /// Pins `point` to be shown, or drops that pin, and has the jump to
    /// `node`, which `point` stands for, wait for the drawing that follows.
    pub(crate) fn always_show(&mut self, node: Id, point: Id, cx: &mut App) {
        self.offer = None;
        self.pending = Some(node);
        self.model.update(cx, |model, cx| {
            let pins = pins::toggle(model.filter().pins(), point, Visibility::Shown);
            model.set_pins(pins, cx);
        });
    }

    /// Takes the reader's `answer` to the card, reporting whether one stood.
    pub(crate) fn answer(&mut self, answer: Answer, cx: &mut App) -> bool {
        let Some(offer) = self.offer.take() else {
            return false;
        };
        match (answer, offer.unshown) {
            (Answer::Reveal, Unshown::Hidden { point, by }) => {
                self.pending = Some(offer.node);
                self.model.update(cx, |model, cx| {
                    let pins = model.filter().pins();
                    let pins = match by {
                        HiddenBy::Pin => pins::unpinned(pins, point),
                        _ => pins::revealing(pins, point),
                    };
                    model.set_pins(pins, cx);
                });
            }
            (Answer::ShowInGraph, _) => {
                (self.model).update(cx, |model, cx| model.reveal(offer.node, cx));
            }
            _ => {}
        }
        true
    }

    /// Answers the card as its key does, reporting whether that answered it:
    /// a card offering only to close leaves the key to the view.
    pub(crate) fn accept(&mut self, cx: &mut App) -> bool {
        let model = self.model.read(cx);
        match self.offer.and_then(|offer| offer.accepted(model)) {
            Some(answer) => self.answer(answer, cx),
            None => false,
        }
    }
}

/// The card `offering` stands, in a view whose `focus` its keys act through
/// and which `holds` the offering, `close` being the view's key to put the
/// card away; none while no card stands.
pub(crate) fn offer_card<V: 'static>(
    offering: &Offering,
    holds: fn(&mut V) -> &mut Offering,
    palette: &Palette,
    focus: &FocusHandle,
    close: &dyn Action,
    cx: &Context<V>,
) -> Option<Div> {
    let offer = offering.offer?;
    let model = offering.model.read(cx);
    let loaded = model.loaded()?;
    let (shared, node) = (&loaded.shared, offer.node);
    let name = point_label(node, shared);
    let written = written_name(&shared.names, &shared.facets, shared.labels.repos(), node);
    let button = |id: &'static str, answer: Answer| {
        let answered = move |view: &mut V, _: &ClickEvent, _: &mut Window, cx: &mut Context<V>| {
            if holds(view).answer(answer, cx) {
                cx.notify();
            }
        };
        Button::new(id, cx.listener(answered)).compact()
    };
    let accept = offer.accepted(model).map(|answer| {
        let button = button("offer-accept", answer).keys_for(&reveal::Accept, focus);
        match (answer, offer.unshown) {
            (
                Answer::Reveal,
                Unshown::Hidden {
                    by: HiddenBy::Pin, ..
                },
            ) => button.label("unpin").tooltip("Unpin node"),
            (Answer::Reveal, _) => button.label("reveal anyway"),
            _ => button.label("show in graph").tooltip("Show node in graph"),
        }
    });
    let close = button("offer-close", Answer::Close)
        .label("close")
        .tooltip("Close card")
        .keys_for(close, focus);
    let reason = match offer.unshown {
        Unshown::Hidden { point, by } => reason(by, point, model),
        Unshown::Absent(why) => why.into(),
    };
    let offers = accept.into_iter().chain([close]);
    Some(card(name, written, reason, offers, palette))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source leaving a node out is named only where it is the one
    /// enabled; a refinement is named as the chip taking the node out.
    #[test]
    fn the_card_names_the_chip_leaving_a_node_out() {
        let mut stack = Stack::default();
        let first = stack.push("//a/...");
        let second = stack.push("//b/...");
        let refinement = stack.push("- //a:x");
        let excluded = |exclusion, stack: &Stack| super::excluded(exclusion, stack).unwrap();
        assert_eq!(
            excluded(Exclusion::Chip(refinement), &stack),
            "left out by chip ‹- //a:x›"
        );
        assert_eq!(
            excluded(Exclusion::Sources, &stack),
            "named by no source chip"
        );
        stack.toggle_enabled(second);
        assert_eq!(
            excluded(Exclusion::Sources, &stack),
            "not named by chip ‹//a/...›"
        );
        assert!(stack.chip(first).is_some());
    }

    /// A filter's rules are the boxes menu's, so the reader is told to turn
    /// that back.
    #[test]
    fn a_rule_is_named_as_the_boxes_menu() {
        assert_eq!(HiddenBy::of(Hiding::Rule(0)), HiddenBy::Boxes);
    }

    /// A pin outranks the scope, so revealing a node the query leaves out
    /// shows it.
    #[test]
    fn a_node_outside_the_query_is_hidden_by_it_and_revealable() {
        let by = HiddenBy::of(Hiding::Scope);
        assert_eq!(by, HiddenBy::Query);
        assert!(by.revealable());
    }

    #[test]
    fn only_a_frame_with_nothing_drawn_inside_cannot_be_revealed() {
        assert!(!HiddenBy::of(Hiding::NothingToFrame).revealable());
        for revealable in [
            HiddenBy::Paths,
            HiddenBy::of(Hiding::Pin),
            HiddenBy::of(Hiding::Scope),
        ] {
            assert!(revealable.revealable());
        }
    }
}
