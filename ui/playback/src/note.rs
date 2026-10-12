use bviz_ui_components::note_strip;
use bviz_ui_theme::ThemeColors;
use gpui::IntoElement;

/// Says a view of `file` does not move with the document's other views, the
/// file recording no wall-clock time to stand beside theirs on the clock.
pub fn unaligned_note(file: &str, colors: &ThemeColors) -> impl IntoElement {
    note_strip(
        format!("Not in step with the other files: this {file} records no wall-clock time."),
        colors,
    )
}
