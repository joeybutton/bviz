use crate::CONTROL_HEIGHT;
use gpui::{AnyElement, App, Div, IntoElement, Pixels, RenderOnce, Window, div, prelude::*, px};

/// Space the strip leaves above and below its controls.
const PADDING: Pixels = px(4.);

/// A horizontal strip of controls acting on the content below it.
#[derive(IntoElement, Default)]
pub struct Toolbar {
    leading: Vec<AnyElement>,
    trailing: Vec<AnyElement>,
}

impl Toolbar {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add to the group held against the left edge. The group wraps onto as many
    /// rows as it takes to hold what it is given, so a control is never cut off
    /// at the window edge however narrow the window.
    pub fn child(mut self, child: impl IntoElement) -> Self {
        self.leading.push(child.into_any_element());
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
            .child(group(self.leading).flex_wrap().flex_1().min_w_0())
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
