use crate::tooltip::{beside_anchor, beside_item, paint_tooltip};
use bviz_ui_theme::TooltipColors;
use gpui::{App, Bounds, Pixels, Point, Window};

/// What a hover card stands beside.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CardAnchor {
    /// The pointer, which the card follows within the item it describes.
    Pointer(Point<Pixels>),
    /// The item the card describes, which the card stands clear of wherever
    /// the pointer is.
    Item(Bounds<Pixels>),
}

/// Paints `lines` in a hover card beside `anchor` inside `bounds`.
pub fn paint_hover_card(
    lines: &[String],
    anchor: CardAnchor,
    bounds: Bounds<Pixels>,
    colors: TooltipColors,
    window: &mut Window,
    cx: &mut App,
) {
    let place = |extent| match anchor {
        CardAnchor::Pointer(at) => beside_anchor(at, extent, bounds),
        CardAnchor::Item(rect) => beside_item(rect, extent, bounds),
    };
    paint_tooltip(lines, place, colors, window, cx);
}
