//! A single-select dropdown: a trigger showing the chosen option and a menu that
//! opens beneath it.

use gpui::{
    deferred, div, prelude::*, px, ClickEvent, Context, ElementId, EventEmitter, FocusHandle,
    IntoElement, MouseButton, Render, SharedString, Subscription, Window,
};

/// Emitted when the user picks a different option. Programmatic
/// [`set_selected`](Dropdown::set_selected) does NOT emit — only a user choice —
/// so a subscriber reacts to selections, not to the entity's other repaints
/// (opening, closing, hover) that a plain `observe` would also deliver.
pub enum DropdownEvent {
    Changed(usize),
}

/// A dropdown over a fixed list of options. The chosen index is read with
/// [`selected`](Self::selected); subscribe to [`DropdownEvent`] to react to a
/// user's selection change.
pub struct Dropdown {
    id: ElementId,
    options: Vec<SharedString>,
    selected: usize,
    open: bool,
    focus_handle: FocusHandle,
    /// Closes the menu when focus leaves it (an outside click); installed lazily on
    /// the first render, where the window is in hand.
    dismiss: Option<Subscription>,
}

impl Dropdown {
    pub fn new(
        id: impl Into<ElementId>,
        options: impl IntoIterator<Item = impl Into<SharedString>>,
        selected: usize,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            id: id.into(),
            options: options.into_iter().map(Into::into).collect(),
            selected,
            open: false,
            focus_handle: cx.focus_handle(),
            dismiss: None,
        }
    }

    /// The index of the chosen option.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Choose an option by index, leaving the menu and notifying observers.
    pub fn set_selected(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.options.len() && index != self.selected {
            self.selected = index;
            cx.notify();
        }
    }

    fn choose(&mut self, index: usize, cx: &mut Context<Self>) {
        self.open = false;
        if index < self.options.len() && index != self.selected {
            self.selected = index;
            cx.emit(DropdownEvent::Changed(index));
        }
        cx.notify();
    }
}

impl EventEmitter<DropdownEvent> for Dropdown {}

impl Render for Dropdown {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Close on an outside click: the menu holds focus while open (see the
        // trigger handler), so losing it means a click landed elsewhere.
        if self.dismiss.is_none() {
            self.dismiss = Some(
                cx.on_focus_out(&self.focus_handle, window, |this, _, _, cx| {
                    if this.open {
                        this.open = false;
                        cx.notify();
                    }
                }),
            );
        }

        let colors = bviz_ui_theme::colors(window);
        let hover = colors.selected;
        let label = self.options.get(self.selected).cloned().unwrap_or_default();

        // Size the label area to the widest option so the trigger keeps a fixed
        // width as the selection changes (the options font is proportional).
        let metrics = bviz_ui_assets::text_metrics(window);
        let measure_color = window.text_style().color;
        let mut measured = px(0.);
        for option in &self.options {
            let width = bviz_ui_assets::shape_line(
                window,
                option.clone(),
                metrics.font.clone(),
                metrics.font_size,
                measure_color,
            )
            .width;
            measured = measured.max(width);
        }
        // Round up and add slack so the chosen label sits on one line: at the exact
        // measured width, sub-pixel rounding between measuring and laying out the
        // glyph run can push the longest option onto a second line.
        let option_width = px(f32::from(measured).ceil() + 6.);

        let trigger = div()
            .id(self.id.clone())
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .h(px(24.))
            // Hold the trigger's measured width in a flex parent (e.g. a toolbar);
            // without this it shrinks and the label wraps to a second line.
            .flex_none()
            .rounded_md()
            .cursor_pointer()
            .text_color(colors.text)
            .hover(move |style| style.bg(hover))
            // Keep the press from reaching an ancestor that focuses on mouse-down
            // (e.g. a toolbar); that would blur the menu and, via the dismiss-on-
            // blur below, close it just before this click reopens it.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.open = !this.open;
                // Hold focus while open so an outside click dismisses the menu.
                if this.open {
                    this.focus_handle.focus(window, cx);
                }
                cx.notify();
            }))
            // flex_none + nowrap so the flex row never shrinks the label below the
            // widest option, which would wrap the text and overflow the trigger.
            .child(div().w(option_width).flex_none().whitespace_nowrap().child(label))
            .child(
                bviz_ui_assets::icon("caret-down")
                    .size(px(12.))
                    .text_color(colors.text),
            );

        let menu = self.open.then(|| {
            let rows = self.options.iter().enumerate().map(|(index, option)| {
                div()
                    .id(("dropdown-option", index))
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .text_color(colors.text)
                    .when(index == self.selected, |row| row.bg(colors.selected))
                    .hover(move |style| style.bg(hover))
                    .on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.choose(index, cx)),
                    )
                    .child(div().whitespace_nowrap().child(option.clone()))
            });
            // Defer the menu so it paints over the content below the trigger; its
            // layout stays in place, anchored just under the trigger.
            deferred(
                div()
                    .occlude()
                    .absolute()
                    .top(px(26.))
                    .left_0()
                    // No explicit width: the menu shrinks to fit its options, so a
                    // row never wraps. A fixed width would have to budget for the row
                    // padding and border, which is error-prone.
                    .flex()
                    .flex_col()
                    .bg(colors.container)
                    .border_1()
                    .border_color(colors.border)
                    .rounded_md()
                    .children(rows),
            )
        });

        div()
            .relative()
            .track_focus(&self.focus_handle)
            .child(trigger)
            .children(menu)
    }
}
