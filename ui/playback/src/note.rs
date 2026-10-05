use crate::TEXT_SIZE;
use bviz_ui_theme::ThemeColors;
use gpui::{IntoElement, ParentElement, Styled, div};

/// Says a view of `file` does not move with the document's other views, the
/// file recording no wall-clock time to stand beside theirs on the clock.
pub fn unaligned_note(file: &str, colors: &ThemeColors) -> impl IntoElement {
    div()
        .flex_none()
        .px_3()
        .py_1()
        .bg(colors.surface)
        .border_b_1()
        .border_color(colors.border)
        .font_family(bviz_ui_assets::UI)
        .text_size(TEXT_SIZE)
        .text_color(colors.text_muted)
        .child(format!(
            "Not in step with the other files: this {file} records no wall-clock time."
        ))
}
