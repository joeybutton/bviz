use crate::tooltip::{CardAnchor, defer_tooltip};
use bviz_ui_theme::TooltipColors;
use gpui::{App, Bounds, Canvas, Context, Pixels, Point, Styled, Subscription, Window, canvas};
use std::rc::Rc;

/// Shows `lines` in a hover card beside `anchor` over the panel at `panel`:
/// beside the anchor inside the panel where it holds the card, and only where
/// it does not past the panel's edges, inside the window. Called as the
/// element showing it prepaints.
pub fn defer_hover_card(
    lines: impl Into<Rc<[String]>>,
    anchor: CardAnchor,
    panel: Bounds<Pixels>,
    colors: TooltipColors,
    window: &mut Window,
    cx: &mut App,
) {
    let place = move |extent, frame| anchor.place(extent, panel, frame);
    defer_tooltip(lines, place, colors, window, cx);
}

/// A canvas filling its parent from the parent's origin, showing the hover
/// card `card` names for the panel the canvas fills, where it names one. It
/// takes no press, so what it covers goes on hit-testing as it would without
/// it.
pub fn hover_card_canvas(
    colors: TooltipColors,
    card: impl 'static + FnOnce(Bounds<Pixels>, &mut Window, &mut App) -> Option<ShownCard>,
) -> Canvas<()> {
    let prepaint = move |bounds, window: &mut Window, cx: &mut App| {
        if let Some(ShownCard { lines, anchor }) = card(bounds, window, cx) {
            defer_hover_card(lines, anchor, bounds, colors, window, cx);
        }
    };
    canvas(prepaint, |_, (), _, _| {})
        .absolute()
        .left_0()
        .top_0()
        .size_full()
}

/// A hover card [`hover_card_canvas`] shows: what it reads, and what it
/// stands beside.
pub struct ShownCard {
    pub lines: Rc<[String]>,
    pub anchor: CardAnchor,
}

/// When the card describing a canvas item, an item of type `K`, is shown: at
/// once while the pointer rests on the item or the keys pick it out, and not
/// while a drag carries the pointer, whose release brings it back. Escape puts
/// it away until the pointer reaches another item or leaves, and still goes on
/// to whatever else it does where it is pressed.
pub struct HoverCard<K> {
    /// Where the pointer rests over the panel, while it does.
    pointer: Option<Point<Pixels>>,
    /// Whether the pointer last rested on an item, whose card may be shown.
    over_item: bool,
    dragging: bool,
    /// The item whose card Escape put away.
    escaped: Option<K>,
    _escape: Subscription,
}

impl<K: PartialEq + 'static> HoverCard<K> {
    /// The card of the view `cx` builds, which `card` reaches it through.
    /// `described` names the item that view's card describes, where it shows
    /// one, which is the item an Escape puts it away for.
    pub fn new<V: 'static>(
        cx: &mut Context<V>,
        card: fn(&mut V) -> &mut HoverCard<K>,
        described: fn(&V, &App) -> Option<K>,
    ) -> Self {
        let view = cx.weak_entity();
        // Taken ahead of dispatch, so the card put away is the one shown as
        // the key went down, before whatever else it does changes that. Every
        // window's keys come here, and a card shows only in the window under
        // the pointer.
        let escape = cx.intercept_keystrokes(move |event, window, cx| {
            let stroke = &event.keystroke;
            let plain_escape = stroke.key == "escape" && !stroke.modifiers.modified();
            if !plain_escape || !window.is_window_hovered() {
                return;
            }
            let escaped = view.update(cx, |view, cx| {
                let item = described(view, cx);
                if card(view).escape(item) {
                    cx.notify();
                }
            });
            escaped.ok();
        });
        Self::with_escape(escape)
    }

    fn with_escape(escape: Subscription) -> Self {
        HoverCard {
            pointer: None,
            over_item: false,
            dragging: false,
            escaped: None,
            _escape: escape,
        }
    }

    /// Puts away the card describing `item`, reporting whether one was shown.
    fn escape(&mut self, item: Option<K>) -> bool {
        let shown = self.pointer.is_some() && item.as_ref().is_some_and(|item| self.shows(item));
        if shown {
            self.escaped = item;
        }
        shown
    }

    /// Where the pointer rests over the panel, while it does.
    pub fn pointer(&self) -> Option<Point<Pixels>> {
        self.pointer
    }

    /// Follows the pointer to `at`, over `item`, with no drag under way: a
    /// move, or the release ending a drag. Reports whether the card needs
    /// repainting, which it does wherever the pointer is over an item or was
    /// over one before.
    pub fn hover(&mut self, at: Point<Pixels>, item: Option<&K>) -> bool {
        self.pointer = Some(at);
        self.dragging = false;
        if self.escaped.as_ref() != item {
            self.escaped = None;
        }
        let was_over_item = std::mem::replace(&mut self.over_item, item.is_some());
        was_over_item || self.over_item
    }

    /// Follows the pointer to `at` as it carries a drag.
    pub fn drag(&mut self, at: Point<Pixels>) {
        self.pointer = Some(at);
        self.dragging = true;
    }

    /// Lets go of the pointer, which has left the panel or whose panel is no
    /// longer shown, reporting whether it rested there.
    pub fn leave(&mut self) -> bool {
        self.escaped = None;
        self.over_item = false;
        self.pointer.take().is_some()
    }

    /// Brings back a card Escape put away, for an item the keys picked out.
    pub fn pick(&mut self) {
        self.escaped = None;
    }

    /// Whether the card describing `item` is shown.
    pub fn shows(&self, item: &K) -> bool {
        !self.dragging && self.escaped.as_ref() != Some(item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px};

    fn card() -> HoverCard<u32> {
        HoverCard::with_escape(Subscription::new(|| {}))
    }

    fn at(x: f32) -> Point<Pixels> {
        point(px(x), px(0.))
    }

    #[test]
    fn escape_puts_a_card_away_until_the_pointer_reaches_another_item() {
        let mut card = card();
        card.hover(at(1.), Some(&1));
        assert!(card.escape(Some(1)));
        card.hover(at(2.), Some(&1));
        assert!(!card.shows(&1));
        card.hover(at(3.), Some(&2));
        assert!(card.shows(&2));
        card.hover(at(4.), Some(&1));
        assert!(card.shows(&1));
    }

    #[test]
    fn escape_puts_a_card_away_until_the_pointer_leaves() {
        let mut card = card();
        card.hover(at(1.), Some(&1));
        card.escape(Some(1));
        assert!(card.leave());
        card.hover(at(1.), Some(&1));
        assert!(card.shows(&1));
    }

    #[test]
    fn a_drag_hides_the_card_until_its_release() {
        let mut card = card();
        card.hover(at(1.), Some(&1));
        card.drag(at(2.));
        assert!(!card.shows(&1));
        assert!(!card.escape(Some(1)), "Escape puts away no hidden card");
        card.hover(at(3.), Some(&1));
        assert!(card.shows(&1));
        assert_eq!(card.pointer(), Some(at(3.)));
    }

    #[test]
    fn escape_puts_away_no_card_once_the_pointer_has_left() {
        let mut card = card();
        card.hover(at(1.), Some(&1));
        card.leave();
        assert!(!card.escape(Some(1)));
    }

    #[test]
    fn only_a_move_onto_or_off_an_item_asks_for_a_repaint() {
        let mut card = card();
        assert!(!card.hover(at(1.), None));
        assert!(card.hover(at(2.), Some(&1)));
        assert!(card.hover(at(3.), Some(&1)));
        assert!(card.hover(at(4.), None));
        assert!(!card.hover(at(5.), None));
    }

    #[test]
    fn keys_picking_an_item_bring_its_card_back() {
        let mut card = card();
        card.escape(Some(1));
        card.pick();
        assert!(card.shows(&1));
    }
}
