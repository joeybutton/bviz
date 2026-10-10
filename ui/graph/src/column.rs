//! The cards standing down the canvas's right edge.

use crate::{CARD_MAX_SHARE, GraphPanel, Palette};
use bviz_ui_components::{ListScroll, SCROLLBAR_THICKNESS, list_scrollbar};
use gpui::{
    Context, Div, MouseButton, MouseDownEvent, Pixels, SharedString, div, prelude::*, px, relative,
};

/// Width of the column, wide enough for a package path and a target.
const WIDTH: Pixels = px(360.);

/// One line of a card, cut short at its end where it runs past the card.
pub(crate) fn line(text: impl Into<SharedString>) -> Div {
    div()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .child(text.into())
}

/// A card's `rows`, as tall as they stand until the card runs out of room for
/// them and from there scrolling under a bar over their right edge that they
/// keep clear of, where `scroll` and `scroll_mut` reach the [`ListScroll`]
/// they scroll by.
pub(crate) fn scrolling(
    rows: impl Styled + IntoElement,
    scroll: fn(&GraphPanel) -> &ListScroll,
    scroll_mut: fn(&mut GraphPanel) -> &mut ListScroll,
    palette: &Palette,
    cx: &Context<GraphPanel>,
) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .text_xs()
        .text_color(palette.label)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |view, e: &MouseDownEvent, _, cx| {
                scroll_mut(view).press(e.position);
                cx.notify();
            }),
        )
        .child(rows.w_full().pr(px(SCROLLBAR_THICKNESS)))
        .child(list_scrollbar(
            cx.entity(),
            scroll,
            scroll_mut,
            palette.scroll,
        ))
}

/// A card that gives way where the column runs short of height: its head
/// stands whole, and what it lists under that scrolls in the height the
/// column has left for it.
pub(crate) struct Folding {
    head: Div,
    listed: Option<Div>,
}

impl Folding {
    /// A card of `head` over `listed`, which scrolls what it holds within
    /// whatever height it is given.
    pub(crate) fn new(head: Div, listed: Option<Div>, palette: &Palette) -> Self {
        let head = div()
            .w_full()
            .flex_none()
            .occlude()
            .px_2()
            .pt_2()
            .bg(palette.container)
            .child(head);
        let head = match listed {
            Some(_) => head.rounded_t_md(),
            None => head.pb_2().rounded_md(),
        };
        let listed = listed.map(|listed| {
            div()
                .w_full()
                .min_h_0()
                .occlude()
                .flex()
                .flex_col()
                .px_2()
                .pt_1()
                .pb_2()
                .rounded_b_md()
                .bg(palette.container)
                .child(listed)
        });
        Self { head, listed }
    }
}

/// The column as it is stood, card by card from the top. A folding card's
/// head and what it lists are items of the column each: one item holding
/// both would not shrink below the height of all it holds.
pub(crate) struct Column {
    column: Div,
    begun: bool,
}

impl Column {
    pub(crate) fn new() -> Self {
        let column = div()
            .absolute()
            .right_2()
            .top_2()
            .bottom_2()
            .w(WIDTH)
            .max_w(relative(CARD_MAX_SHARE))
            .overflow_hidden()
            .flex()
            .flex_col()
            .items_end();
        Self {
            column,
            begun: false,
        }
    }

    /// Parts the next card from the one above it, and where `grows` takes up
    /// ahead of it all the height the column has to spare.
    fn part(mut self, grows: bool) -> Self {
        let gap = match (self.begun, grows) {
            (true, true) => Some(div().flex_1().min_h_2()),
            (true, false) => Some(div().flex_none().h_2()),
            (false, true) => Some(div().flex_1()),
            (false, false) => None,
        };
        self.column = self.column.children(gap);
        self.begun = true;
        self
    }

    /// Stands `card` next, which bounds its own height.
    pub(crate) fn held(self, card: Option<Div>) -> Self {
        let Some(card) = card else {
            return self;
        };
        let mut next = self.part(false);
        next.column = next.column.child(card.flex_none());
        next
    }

    fn stand(self, card: Option<Folding>, at_foot: bool) -> Self {
        let Some(card) = card else {
            return self;
        };
        let mut next = self.part(at_foot);
        next.column = next.column.child(card.head).children(card.listed);
        next
    }

    /// Stands `card` next.
    pub(crate) fn folding(self, card: Option<Folding>) -> Self {
        self.stand(card, false)
    }

    /// Stands `card` at the column's foot, under every card stood before it.
    pub(crate) fn foot(self, card: Option<Folding>) -> Div {
        self.stand(card, true).column
    }
}
