use gpui::{
    App, Axis, ElementId, Fill, IntoElement, MouseButton, MouseDownEvent, Pixels, RenderOnce,
    Window, div, prelude::*,
};

type OnPress = Box<dyn Fn(&MouseDownEvent, &mut Window, &mut App) + 'static>;

/// The band a resize is taken hold of by: it shows a resize cursor, takes a
/// press anywhere across it, and can draw a line down its middle. It holds no
/// drag of its own — `on_press` latches the gesture in the caller's state.
#[derive(IntoElement)]
pub struct Divider {
    id: ElementId,
    axis: Axis,
    band: Option<Pixels>,
    line: Option<(Pixels, Fill)>,
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
}

impl RenderOnce for Divider {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let Divider {
            id,
            axis,
            band,
            line,
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
        match line {
            None => grip,
            Some((thickness, fill)) => grip.child(match axis {
                Axis::Horizontal => div().w(thickness).h_full().bg(fill),
                Axis::Vertical => div().h(thickness).w_full().bg(fill),
            }),
        }
    }
}
