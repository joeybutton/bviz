//! The short text a control's tooltip reads, with the keys or the gesture
//! that do what the control does named beside it.

use crate::gesture::GestureRow;
use crate::{KeyStyle, TextTooltip, action_label, key_text, key_text_in};
use bviz_ui_theme::TooltipColors;
use gpui::{
    Action, AnyView, App, FocusHandle, SharedString, StatefulInteractiveElement, WeakEntity, Window,
};

/// What sets apart the shortcuts a hint names beside one another.
const SHORTCUT_SEPARATOR: &str = " / ";

/// Where the keys a [`Hint`] names are looked up.
#[derive(Clone)]
enum KeysIn {
    /// The context of that focus handle.
    Focus(FocusHandle),
    /// No context: the keys bound to the action act on something other than
    /// what the control acts on.
    Nowhere,
    /// The context focus stands in when the tooltip is shown.
    Focused,
}

/// A way of doing what a control does without it.
enum Shortcut {
    Keys {
        action: Box<dyn Action>,
        within: KeysIn,
    },
    Gesture(GestureRow),
}

impl Clone for Shortcut {
    fn clone(&self) -> Self {
        match self {
            Shortcut::Keys { action, within } => Shortcut::Keys {
                action: action.boxed_clone(),
                within: within.clone(),
            },
            Shortcut::Gesture(row) => Shortcut::Gesture(*row),
        }
    }
}

impl Shortcut {
    /// How the shortcut reads beside the hint's text; `None` for keys the
    /// keymap binds nowhere they reach.
    fn text(&self, window: &Window) -> Option<SharedString> {
        match self {
            Shortcut::Keys { action, within } => match within {
                KeysIn::Focus(focus) => key_text_in(&**action, focus, window),
                KeysIn::Nowhere => None,
                KeysIn::Focused => key_text(&**action, window),
            },
            Shortcut::Gesture(row) => Some(row.gesture.text(KeyStyle::NATIVE).into()),
        }
    }

    /// What the shortcut does, as the keys help lists it.
    fn does(&self, cx: &App) -> Option<SharedString> {
        match self {
            Shortcut::Keys { action, .. } => action_label(&**action, cx).map(SharedString::from),
            Shortcut::Gesture(row) => Some(row.does.into()),
        }
    }
}

/// What a control's tooltip reads: a short phrase saying what the control
/// does, and beside it each shortcut that does the same. The phrase is the
/// first shortcut's description where no text is given.
#[derive(Clone, Default)]
pub struct Hint {
    text: Option<SharedString>,
    shortcuts: Vec<Shortcut>,
}

impl Hint {
    pub fn text(mut self, text: impl Into<SharedString>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// Whether the hint's own text is `text`.
    pub(crate) fn reads(&self, text: &str) -> bool {
        self.text.as_deref() == Some(text)
    }

    /// Names the keys that run `action` where `focus` stands.
    pub fn keys_for(self, action: &dyn Action, focus: &FocusHandle) -> Self {
        self.keys(action, KeysIn::Focus(focus.clone()))
    }

    /// As [`Hint::keys_for`], naming no keys where `focus` is `None`: for an
    /// action whose keys act on what the control acts on only some of the time.
    pub fn keys_in(self, action: &dyn Action, focus: Option<&FocusHandle>) -> Self {
        let within = focus.map_or(KeysIn::Nowhere, |focus| KeysIn::Focus(focus.clone()));
        self.keys(action, within)
    }

    /// As [`Hint::keys_for`], the keys looked up from wherever focus stands as
    /// the tooltip is shown.
    pub fn keys_for_focused(self, action: &dyn Action) -> Self {
        self.keys(action, KeysIn::Focused)
    }

    /// Names the pointer gesture `row` lists.
    pub fn gesture(mut self, row: GestureRow) -> Self {
        self.shortcuts.push(Shortcut::Gesture(row));
        self
    }

    fn keys(mut self, action: &dyn Action, within: KeysIn) -> Self {
        self.shortcuts.push(Shortcut::Keys {
            action: action.boxed_clone(),
            within,
        });
        self
    }

    /// The tooltip reading the hint; `None` where it has nothing to read. The
    /// tooltip builds nothing where the hint has no text and names no keys
    /// where it is shown.
    pub fn tooltip(
        self,
        colors: TooltipColors,
    ) -> Option<impl Fn(&mut Window, &mut App) -> Option<AnyView> + 'static> {
        if self.text.is_none() && self.shortcuts.is_empty() {
            return None;
        }
        Some(move |window: &mut Window, cx: &mut App| {
            let text = (self.text.clone()).or_else(|| {
                self.shortcuts
                    .first()
                    .and_then(|shortcut| shortcut.does(cx))
            });
            let named: Vec<SharedString> = (self.shortcuts.iter())
                .filter_map(|shortcut| shortcut.text(window))
                .collect();
            if text.is_none() && named.is_empty() {
                return None;
            }
            let named = (!named.is_empty()).then(|| named.join(SHORTCUT_SEPARATOR).into());
            let text = text.unwrap_or_default();
            Some(TextTooltip::view(text, named, colors, cx))
        })
    }

    /// `el`, showing the hint as its tooltip where it has anything to read.
    pub fn on<E: StatefulInteractiveElement>(self, el: E, colors: TooltipColors) -> E {
        match self.tooltip(colors) {
            Some(tooltip) => el.optional_tooltip(tooltip),
            None => el,
        }
    }

    /// `el`, the trigger of `picker`, showing the hint as its tooltip except
    /// while `open` holds of the picker, whose open list the tooltip would
    /// cover. The tooltip stays attached while the list is open: gpui forgets
    /// the state of a tooltip whose element renders without one, so a click
    /// closing the list would bring back the hint the click put away.
    pub fn on_picker<E: StatefulInteractiveElement, V: 'static>(
        self,
        el: E,
        colors: TooltipColors,
        picker: WeakEntity<V>,
        open: fn(&V) -> bool,
    ) -> E {
        let Some(tooltip) = self.tooltip(colors) else {
            return el;
        };
        el.optional_tooltip(move |window, cx| {
            let opened = picker.upgrade().is_some_and(|picker| open(picker.read(cx)));
            if opened {
                return None;
            }
            tooltip(window, cx)
        })
    }
}
