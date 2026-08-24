use crate::CONTROL_HEIGHT;
use gpui::{AnyElement, App, Div, IntoElement, Pixels, RenderOnce, Window, div, prelude::*, px};

/// Height the strip adds around its controls.
const MARGIN: Pixels = px(8.);

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

    /// Add to the group held against the left edge.
    pub fn child(mut self, child: impl IntoElement) -> Self {
        self.leading.push(child.into_any_element());
        self
    }

    /// Add to the group held against the right edge.
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
            .items_center()
            .justify_between()
            // Hold the strip's height in a flex column, so a growing sibling
            // (the content it acts on) cannot squeeze the controls.
            .flex_none()
            .h(CONTROL_HEIGHT + MARGIN)
            .px_2()
            .bg(colors.surface)
            .border_b_1()
            .border_color(colors.border)
            .child(group(self.leading))
            .child(group(self.trailing))
    }
}

fn group(items: Vec<AnyElement>) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .children(items)
}
