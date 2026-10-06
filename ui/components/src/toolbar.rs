use crate::CONTROL_HEIGHT;
use gpui::{
    AnyElement, App, Div, ElementId, IntoElement, Pixels, RenderOnce, Window, div, prelude::*, px,
};

/// Space the strip leaves above and below its controls.
const PADDING: Pixels = px(4.);

/// A horizontal strip of controls acting on the content below it.
#[derive(IntoElement, Default)]
pub struct Toolbar {
    leading: Vec<AnyElement>,
    trailing: Vec<AnyElement>,
    /// Holds the leading group to one row, scrolling it sideways past the
    /// strip's width, where set; the id keeps the scroll.
    one_row: Option<ElementId>,
}

impl Toolbar {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add to the group held against the left edge. The group wraps onto as many
    /// rows as it takes to hold what it is given, so a control is never cut off
    /// at the window edge however narrow the window, unless the strip is held
    /// to [`one_row`](Self::one_row).
    pub fn child(mut self, child: impl IntoElement) -> Self {
        self.leading.push(child.into_any_element());
        self
    }

    /// Holds the strip to one row of one height, the leading group scrolling
    /// sideways where it outgrows the strip, so that what the strip holds
    /// never moves the content below it. `id` keeps the group's scroll.
    pub fn one_row(mut self, id: impl Into<ElementId>) -> Self {
        self.one_row = Some(id.into());
        self
    }

    /// Add to the group held against the right edge, which keeps its room
    /// whatever the leading group holds.
    pub fn trailing(mut self, child: impl IntoElement) -> Self {
        self.trailing.push(child.into_any_element());
        self
    }
}

impl RenderOnce for Toolbar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = bviz_ui_theme::theme(window, cx).colors;
        div()
            .flex()
            .flex_row()
            .items_start()
            .gap_2()
            // Hold the strip's height in a flex column, so a growing sibling
            // (the content it acts on) cannot squeeze the controls.
            .flex_none()
            .py(PADDING)
            .px_2()
            .bg(colors.surface)
            .border_b_1()
            .border_color(colors.border)
            .child(match self.one_row {
                Some(id) => group(self.leading)
                    .id(id)
                    .h(CONTROL_HEIGHT)
                    .flex_1()
                    .min_w_0()
                    .overflow_x_scroll()
                    .into_any_element(),
                None => group(self.leading)
                    .flex_wrap()
                    .flex_1()
                    .min_w_0()
                    .into_any_element(),
            })
            .child(group(self.trailing).flex_none())
    }
}

/// One group's controls, each row of it as tall as a control so the two groups
/// line up on the strip's first row.
fn group(items: Vec<AnyElement>) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .min_h(CONTROL_HEIGHT)
        .children(items)
}
