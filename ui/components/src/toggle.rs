use crate::CONTROL_HEIGHT;
use gpui::{
    App, ClickEvent, ElementId, IntoElement, RenderOnce, SharedString, Window, div, prelude::*,
};

/// A labelled on/off control, sized to line up in a [`Toolbar`](crate::Toolbar)
/// row.
#[derive(IntoElement)]
pub struct Toggle {
    id: ElementId,
    label: SharedString,
    on: bool,
    on_click: Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>,
}

impl Toggle {
    pub fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        on: bool,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            on,
            on_click: Box::new(on_click),
        }
    }
}

impl RenderOnce for Toggle {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let colors = bviz_ui_theme::colors(window);
        let el = div()
            .id(self.id)
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .h(CONTROL_HEIGHT)
            .px_3()
            .rounded_md()
            .border_1()
            .cursor_pointer()
            .active(|s| s.opacity(0.8))
            .on_click(self.on_click)
            .child(self.label);
        if self.on {
            el.bg(colors.selected)
                // Bordered in its own fill, so both states measure the same width.
                .border_color(colors.selected)
                .text_color(colors.selected_text)
                .hover(|s| s.opacity(0.92))
        } else {
            let hover_border = colors.text;
            el.border_color(colors.border)
                .text_color(colors.text)
                .hover(move |s| s.border_color(hover_border))
        }
    }
}
