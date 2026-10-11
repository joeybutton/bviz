//! A single-select dropdown: a trigger showing the chosen option and a menu that
//! opens from it.

use crate::{Hint, KeyScope, menu_chrome};
use bviz_ui_theme::ThemeColors;
use gpui::{
    Action, App, ClickEvent, Context, ElementId, EventEmitter, FocusHandle, IntoElement,
    KeyBinding, Pixels, Render, SharedString, Subscription, Window, div, prelude::*,
};

gpui::actions!(
    bviz_dropdown,
    [
        /// Close list
        Dismiss,
    ]
);

/// The key context the dropdown carries, which [`Dismiss`] is scoped to.
pub(crate) const KEY_CONTEXT: KeyScope = KeyScope::new("Dropdown", "Dropdown");

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", Dismiss, Some(KEY_CONTEXT.id()))]);
}

/// Emitted when the user picks a different option. Programmatic
/// [`set_selected`](Dropdown::set_selected) does NOT emit — only a user choice —
/// so a subscriber reacts to selections, not to the entity's other repaints
/// (opening, closing, hover) that a plain `observe` would also deliver.
pub enum DropdownEvent {
    Changed(usize),
}

/// One option a [`Dropdown`] offers.
struct Choice {
    label: SharedString,
    /// Whether picking it does anything. An option the data behind the control
    /// cannot answer is drawn greyed and cannot be picked, so that it says it
    /// can do nothing rather than silently doing nothing.
    available: bool,
}

/// The list a [`Dropdown`] has open.
struct Opened {
    /// Where focus stood when the list opened, which closing it from within
    /// hands focus back to.
    from: Option<FocusHandle>,
}

/// A dropdown over a fixed list of options. The chosen index is read with
/// [`selected`](Self::selected); subscribe to [`DropdownEvent`] to react to a
/// user's selection change.
pub struct Dropdown {
    id: ElementId,
    options: Vec<Choice>,
    selected: usize,
    open: Option<Opened>,
    /// What the trigger's tooltip reads.
    hint: Hint,
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
            options: options
                .into_iter()
                .map(|label| Choice {
                    label: label.into(),
                    available: true,
                })
                .collect(),
            selected,
            open: None,
            hint: Hint::default(),
            focus_handle: cx.focus_handle(),
            dismiss: None,
        }
    }

    /// The tooltip reading `text`.
    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.hint = self.hint.text(text);
        self
    }

    /// The tooltip naming keys as [`Hint::keys_for_focused`] names them.
    pub fn keys_for_focused(mut self, action: &dyn Action) -> Self {
        self.hint = self.hint.keys_for_focused(action);
        self
    }

    /// Has the tooltip read `text`, for a hint that follows a state held
    /// elsewhere.
    pub fn set_tooltip(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let text = text.into();
        if !self.hint.reads(&text) {
            self.hint = std::mem::take(&mut self.hint).text(text);
            cx.notify();
        }
    }

    /// The index of the chosen option.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Makes the option at `index` one that can be picked, or one that cannot.
    pub fn set_available(&mut self, index: usize, available: bool, cx: &mut Context<Self>) {
        if let Some(choice) = self.options.get_mut(index)
            && choice.available != available
        {
            choice.available = available;
            cx.notify();
        }
    }

    /// Names the option at `index` `label`, for an option whose meaning
    /// follows a choice made elsewhere.
    pub fn set_label(
        &mut self,
        index: usize,
        label: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let label = label.into();
        if let Some(choice) = self.options.get_mut(index)
            && choice.label != label
        {
            choice.label = label;
            cx.notify();
        }
    }

    /// Set the chosen index if in range and different, reporting whether it changed.
    fn select(&mut self, index: usize) -> bool {
        let changed = index < self.options.len() && index != self.selected;
        if changed {
            self.selected = index;
        }
        changed
    }

    /// Choose an option by index, leaving the menu and notifying observers.
    pub fn set_selected(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.select(index) {
            cx.notify();
        }
    }

    fn choose(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self
            .options
            .get(index)
            .is_some_and(|choice| choice.available)
        {
            return;
        }
        self.close(window, cx);
        if self.select(index) {
            cx.emit(DropdownEvent::Changed(index));
        }
    }

    fn dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<Self>) {
        if self.open.is_some() {
            self.close(window, cx);
        } else {
            cx.propagate();
        }
    }

    /// Takes focus into the list, where an outside click takes it away.
    fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = Some(Opened {
            from: window.focused(cx),
        });
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// Closes the list from within, handing focus back where it stood when the
    /// list opened.
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Opened { from }) = self.open.take() {
            if let Some(from) = from {
                window.focus(&from, cx);
            }
            cx.notify();
        }
    }

    fn trigger(
        &self,
        colors: &ThemeColors,
        option_width: Pixels,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let label = self
            .options
            .get(self.selected)
            .map(|choice| choice.label.clone())
            .unwrap_or_default();
        let trigger = menu_chrome::trigger(self.id.clone(), colors);
        let open = |dropdown: &Self| dropdown.open.is_some();
        (self.hint.clone())
            .on_picker(trigger, colors.tooltip(), cx.weak_entity(), open)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                if this.open.is_some() {
                    this.close(window, cx);
                } else {
                    this.open(window, cx);
                }
            }))
            .child(menu_chrome::reading(option_width, label))
            .child(menu_chrome::caret(colors))
    }

    fn menu(&self, colors: &ThemeColors, cx: &Context<Self>) -> impl IntoElement {
        let rows = self.options.iter().enumerate().map(|(index, option)| {
            menu_chrome::row(
                ("dropdown-option", index).into(),
                colors,
                index == self.selected,
            )
            .when(!option.available, |row| {
                row.cursor_default().text_color(colors.text_disabled)
            })
            .on_click(
                cx.listener(move |this, _: &ClickEvent, window, cx| this.choose(index, window, cx)),
            )
            .child(div().whitespace_nowrap().child(option.label.clone()))
        });
        menu_chrome::under_trigger(menu_chrome::sheet(colors).children(rows))
    }
}

impl EventEmitter<DropdownEvent> for Dropdown {}

impl Render for Dropdown {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Close on an outside click: the menu holds focus while open, so losing
        // it means a click landed elsewhere.
        if self.dismiss.is_none() {
            self.dismiss = Some(
                cx.on_focus_out(&self.focus_handle, window, |this, _, _, cx| {
                    if this.open.take().is_some() {
                        cx.notify();
                    }
                }),
            );
        }

        let colors = bviz_ui_theme::theme(window, cx).colors;
        let option_width = menu_chrome::widest(
            window,
            self.options.iter().map(|choice| choice.label.clone()),
        );

        div()
            .relative()
            .key_context(KEY_CONTEXT)
            // Focusable only while open, so a press on the closed trigger leaves
            // focus to whatever around it takes it, for the list to hand back.
            .when(self.open.is_some(), |el| el.track_focus(&self.focus_handle))
            .on_action(cx.listener(Self::dismiss))
            .child(self.trigger(&colors, option_width, cx))
            .children(self.open.is_some().then(|| self.menu(&colors, cx)))
    }
}
