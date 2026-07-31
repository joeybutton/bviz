use gpui::{
    App, ClickEvent, Div, ElementId, Entity, FocusHandle, IntoElement, KeyDownEvent, KeyUpEvent,
    RenderOnce, SharedString, Stateful, Window, div, prelude::*, px, transparent_black,
};

/// How far a toggle's hover fill sits from the window background toward the text
/// color: enough to show the pointer is on it, and clear of the selection color,
/// which is what an on toggle fills with.
const TOGGLE_HOVER_TINT: f32 = 0.18;

/// Whether a key activates a focused button (enter or space).
fn is_activate(key: &str) -> bool {
    key == "enter" || key == "space"
}

/// A clickable button: filled with a label by default, or `.ghost().icon(..)` for
/// an icon-only control. Add `.track_focus(handle)` to make it keyboard-focusable,
/// in which case enter/space activate it and show the same pressed state as the mouse.
#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: Option<SharedString>,
    icon: Option<SharedString>,
    ghost: bool,
    /// Whether the button reads as on, for one built as a toggle; `None` for a
    /// button that is not one.
    selected: Option<bool>,
    focus_handle: Option<FocusHandle>,
    on_click: Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>,
}

impl Button {
    pub fn new(
        id: impl Into<ElementId>,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            label: None,
            icon: None,
            ghost: false,
            selected: None,
            focus_handle: None,
            on_click: Box::new(on_click),
        }
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn icon(mut self, icon: impl Into<SharedString>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    pub fn ghost(mut self) -> Self {
        self.ghost = true;
        self
    }

    /// Makes the button a toggle standing at `selected`, filling in the selection
    /// color while on.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = Some(selected);
        self
    }

    pub fn track_focus(mut self, handle: &FocusHandle) -> Self {
        self.focus_handle = Some(handle.clone());
        self
    }
}

fn sized(el: Stateful<Div>, button: &Button) -> Stateful<Div> {
    if button.ghost && button.label.is_none() {
        // A square hit target, so a bare icon is no harder to hit than a label.
        el.size(px(28.))
    } else {
        el.px_4().py_2()
    }
}

/// Ghost buttons read as flat text; filled ones have a solid fill and a border that
/// brightens on focus. An on toggle takes the filled look, so it reads as on
/// whichever variant it was built as.
fn styled_variant(
    el: Stateful<Div>,
    button: &Button,
    focused: bool,
    colors: &bviz_ui_theme::Colors,
) -> Stateful<Div> {
    let el = sized(el, button).rounded_md();
    if button.ghost && button.selected != Some(true) {
        let toggle = button.selected.is_some();
        let hover_bg = if toggle {
            bviz_ui_theme::mix(colors.background, colors.text, TOGGLE_HOVER_TINT)
        } else {
            colors.selected
        };
        el
            // A toggle holds the filled variant's border box while off, so
            // switching it on does not resize it and shift its neighbours.
            .when(toggle, |el| el.border_2().border_color(transparent_black()))
            .text_color(colors.text)
            .hover(move |s| s.bg(hover_bg))
            .active(|s| s.opacity(0.6))
    } else {
        el.bg(colors.selected)
            .text_color(colors.selected_text)
            .border_2()
            .border_color(if focused { colors.text } else { colors.selected })
            .hover(|s| s.opacity(0.92))
            .active(|s| s.opacity(0.8))
    }
}

/// Wire enter/space to drive the persisted `pressed` state so a keyboard-held button
/// shows the same press as the mouse. A button without a focus handle is unfocusable
/// and gets no key handlers.
fn with_held_state_handlers(
    el: Stateful<Div>,
    focus_handle: Option<&FocusHandle>,
    pressed: Entity<bool>,
) -> Stateful<Div> {
    let Some(handle) = focus_handle else {
        return el;
    };
    el.track_focus(handle)
        .on_key_down({
            let pressed = pressed.clone();
            move |e: &KeyDownEvent, _, cx| {
                if is_activate(&e.keystroke.key) {
                    pressed.update(cx, |p, cx| {
                        *p = true;
                        cx.notify();
                    });
                }
            }
        })
        .on_key_up({
            let pressed = pressed.clone();
            move |e: &KeyUpEvent, _, cx| {
                if is_activate(&e.keystroke.key) {
                    pressed.update(cx, |p, cx| {
                        *p = false;
                        cx.notify();
                    });
                }
            }
        })
}

impl RenderOnce for Button {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = bviz_ui_theme::colors(window);
        let focused = self
            .focus_handle
            .as_ref()
            .is_some_and(|handle| handle.is_focused(window));
        // Persist the keyboard-held state across frames so enter/space show a press
        // (gpui's `.active` only tracks the mouse).
        let pressed = window.use_keyed_state(self.id.clone(), cx, |_, _| false);
        let is_pressed = *pressed.read(cx);

        let el = div()
            .id(self.id.clone())
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .gap_1()
            .cursor_pointer()
            .when(is_pressed, |el| el.opacity(0.8));

        let el = styled_variant(el, &self, focused, &colors);
        let el = with_held_state_handlers(el, self.focus_handle.as_ref(), pressed);
        let el = el.on_click(self.on_click);

        let el = match self.label {
            Some(label) => el.child(label),
            None => el,
        };
        match self.icon {
            Some(name) => el.child(
                bviz_ui_assets::icon(name.as_ref())
                    .size(px(16.))
                    .text_color(colors.text),
            ),
            None => el,
        }
    }
}
