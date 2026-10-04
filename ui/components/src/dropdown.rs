//! A single-select dropdown: a trigger showing the chosen option and a menu that
//! opens beneath it.

use crate::menu_chrome;
use bviz_ui_theme::ThemeColors;
use gpui::{
    ClickEvent, Context, ElementId, EventEmitter, FocusHandle, IntoElement, Pixels, Render,
    SharedString, Subscription, Window, deferred, div, prelude::*,
};

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

/// A dropdown over a fixed list of options. The chosen index is read with
/// [`selected`](Self::selected); subscribe to [`DropdownEvent`] to react to a
/// user's selection change.
pub struct Dropdown {
    id: ElementId,
    options: Vec<Choice>,
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
            options: options
                .into_iter()
                .map(|label| Choice {
                    label: label.into(),
                    available: true,
                })
                .collect(),
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

    fn choose(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self
            .options
            .get(index)
            .is_some_and(|choice| choice.available)
        {
            return;
        }
        self.open = false;
        if self.select(index) {
            cx.emit(DropdownEvent::Changed(index));
        }
        cx.notify();
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
        menu_chrome::trigger(self.id.clone(), colors)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.open = !this.open;
                // Hold focus while open so an outside click dismisses the menu.
                if this.open {
                    this.focus_handle.focus(window, cx);
                }
                cx.notify();
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
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.choose(index, cx)))
            .child(div().whitespace_nowrap().child(option.label.clone()))
        });
        // Defer the menu so it paints over the content below the trigger.
        deferred(menu_chrome::sheet(colors).children(rows))
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

        let colors = bviz_ui_theme::theme(window, cx).colors;
        let option_width = menu_chrome::widest(
            window,
            self.options.iter().map(|choice| choice.label.clone()),
        );

        div()
            .relative()
            .track_focus(&self.focus_handle)
            .child(self.trigger(&colors, option_width, cx))
            .children(self.open.then(|| self.menu(&colors, cx)))
    }
}
