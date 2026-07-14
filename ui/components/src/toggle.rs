use gpui::{Context, Div, Entity, Length, Rgba, Stateful, div, prelude::*};

/// A rounded pill toggling one boolean field, pinned to the top-left corner.
/// On reads in `accent` with a filled dot; off reads in `off_label`/`off_border`
/// with a hollow dot. Clicking runs `mutate`, then redraws.
pub fn toggle_pill<V: 'static>(
    id: &'static str,
    top: impl Into<Length>,
    label: &str,
    on: bool,
    accent: Rgba,
    off_label: Rgba,
    off_border: Rgba,
    fill: Rgba,
    entity: Entity<V>,
    mutate: impl Fn(&mut V, &mut Context<V>) + 'static,
) -> Stateful<Div> {
    let top = top.into();
    let (dot, label_color, border_color) = if on {
        ('\u{25CF}', accent, accent)
    } else {
        ('\u{25CB}', off_label, off_border)
    };
    div()
        .id(id)
        .absolute()
        .left_2()
        .top(top)
        .flex()
        .items_center()
        .gap_1()
        .px_2()
        .py_1()
        .rounded_md()
        .bg(fill)
        .border_1()
        .border_color(border_color)
        .text_xs()
        .text_color(label_color)
        .cursor_pointer()
        .child(format!("{dot} {label}"))
        .on_click(move |_, _, cx| {
            entity.update(cx, |this, cx| {
                mutate(this, cx);
                cx.notify();
            });
        })
}
