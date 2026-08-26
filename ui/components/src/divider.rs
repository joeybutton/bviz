use gpui::{
    App, Axis, ElementId, Fill, Hsla, IntoElement, MouseButton, MouseDownEvent, Pixels, RenderOnce,
    Window, div, prelude::*, px,
};

type OnPress = Box<dyn Fn(&MouseDownEvent, &mut Window, &mut App) + 'static>;

/// Side of the caret a collapsed side is marked by, small enough to sit inside
/// the band a shut side is left with.
const CARET_SIZE: Pixels = px(10.);

/// One of the two sides a [`Divider`] parts, named by where it lies along the
/// divider's axis.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DividerSide {
    Start,
    End,
}

impl DividerSide {
    /// The icon pointing the way a gesture on a divider along `axis` must go to
    /// bring this side back from collapsed.
    fn caret(self, axis: Axis) -> &'static str {
        match (axis, self) {
            (Axis::Vertical, DividerSide::Start) => "caret-down",
            (Axis::Vertical, DividerSide::End) => "caret-up",
            (Axis::Horizontal, DividerSide::Start) => "caret-right",
            (Axis::Horizontal, DividerSide::End) => "caret-left",
        }
    }
}

/// The band a resize is taken hold of by: it shows a resize cursor, takes a
/// press anywhere across it, can draw a line down its middle, and marks a side
/// that has been shut with a caret. It holds no drag of its own — `on_press`
/// latches the gesture in the caller's state.
#[derive(IntoElement)]
pub struct Divider {
    id: ElementId,
    axis: Axis,
    band: Option<Pixels>,
    line: Option<(Pixels, Fill)>,
    collapsed: Option<(DividerSide, Hsla)>,
    on_press: OnPress,
}

impl Divider {
    /// A divider parting two sides arranged along `axis`: [`Axis::Horizontal`]
    /// for sides side by side, giving a vertical line under a column-resize
    /// cursor, and [`Axis::Vertical`] for sides stacked, giving a horizontal
    /// line under a row-resize cursor. It fills the room it is placed in and
    /// draws nothing.
    pub fn new(
        id: impl Into<ElementId>,
        axis: Axis,
        on_press: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Divider {
            id: id.into(),
            axis,
            band: None,
            line: None,
            collapsed: None,
            on_press: Box::new(on_press),
        }
    }

    /// Fixes the band's extent across the two sides it parts to `extent`.
    pub fn band(mut self, extent: Pixels) -> Self {
        self.band = Some(extent);
        self
    }

    pub fn line(mut self, thickness: Pixels, fill: impl Into<Fill>) -> Self {
        self.line = Some((thickness, fill.into()));
        self
    }

    /// Marks `side` as shut, this band being all that is left to take hold of it
    /// by: a caret in `fill` is drawn across the band, pointing the way a
    /// gesture must go to bring the side back.
    pub fn collapsed(mut self, side: DividerSide, fill: impl Into<Hsla>) -> Self {
        self.collapsed = Some((side, fill.into()));
        self
    }
}

impl RenderOnce for Divider {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let Divider {
            id,
            axis,
            band,
            line,
            collapsed,
            on_press,
        } = self;
        let grip = div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, on_press);
        let grip = match axis {
            Axis::Horizontal => grip.cursor_col_resize(),
            Axis::Vertical => grip.cursor_row_resize(),
        };
        let grip = match (axis, band) {
            (_, None) => grip.size_full(),
            (Axis::Horizontal, Some(extent)) => grip.w(extent).h_full().flex_shrink_0(),
            (Axis::Vertical, Some(extent)) => grip.h(extent).w_full().flex_shrink_0(),
        };
        let line = line.map(|(thickness, fill)| match axis {
            Axis::Horizontal => div().w(thickness).h_full().bg(fill),
            Axis::Vertical => div().h(thickness).w_full().bg(fill),
        });
        match collapsed {
            None => grip.children(line),
            // The line is taken out of flow so the caret centres on the band
            // rather than being pushed aside by a line spanning it.
            Some((side, fill)) => grip
                .relative()
                .children(line.map(|line| line.absolute()))
                .child(
                    bviz_ui_assets::icon(side.caret(axis))
                        .size(CARET_SIZE)
                        .text_color(fill),
                ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shut side leaves only the band to reopen it by, and the caret is all
    /// that says which way the gesture across the band has to go: away from the
    /// side that is shut, along the axis the divider parts.
    #[test]
    fn a_caret_points_away_from_the_side_it_marks() {
        for (axis, toward_start, toward_end) in [
            (Axis::Vertical, "caret-up", "caret-down"),
            (Axis::Horizontal, "caret-left", "caret-right"),
        ] {
            assert_eq!(DividerSide::Start.caret(axis), toward_end);
            assert_eq!(DividerSide::End.caret(axis), toward_start);
        }
    }
}
