use gpui::{Context, Div, Entity, Length, Rgba, Stateful, div, prelude::*};

/// A pill's colors: the on-state accent (label and border and filled dot), and
/// the off-state label, border, and dot fill.
#[derive(Clone, Copy)]
pub struct PillColors {
    pub accent: Rgba,
    pub off_label: Rgba,
    pub off_border: Rgba,
    pub fill: Rgba,
}

/// A rounded pill toggling one boolean field, pinned to the top-left corner.
/// On reads in the accent with a filled dot; off reads in the off label/border
/// with a hollow dot. Clicking runs `mutate`, then redraws.
pub fn toggle_pill<V: 'static>(
    id: &'static str,
    top: impl Into<Length>,
    label: &str,
    on: bool,
    colors: PillColors,
    entity: Entity<V>,
    mutate: impl Fn(&mut V, &mut Context<V>) + 'static,
) -> Stateful<Div> {
    let PillColors { accent, off_label, off_border, fill } = colors;
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
