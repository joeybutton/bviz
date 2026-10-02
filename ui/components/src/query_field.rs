//! A text field with a button that empties it, which says what it holds once
//! typing settles.

use crate::{Button, TextInput, text_input::TextEdited};
use gpui::{
    App, ClickEvent, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    Pixels, Render, SharedString, Subscription, Task, Window, div, prelude::*,
};
use std::time::Duration;

/// How long what a [`QueryField`] holds must stand before the field settles,
/// so a reader acting on a settled field acts once on a word typed.
const SETTLE: Duration = Duration::from_millis(150);

/// What a [`QueryField`] tells of the text typed into it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryFieldEvent {
    /// The text changed.
    Edited,
    /// The text has stood for [`SETTLE`], or is empty, which settles at once.
    Settled,
}

/// A one-line text field with a button that empties it, shown while it holds
/// anything.
pub struct QueryField {
    input: Entity<TextInput>,
    width: Pixels,
    clear_id: ElementId,
    /// Waits for the text to settle; an edit replaces it, and so gives up the
    /// wait for the text it changed.
    settling: Task<()>,
    _edited: Subscription,
}

impl QueryField {
    /// A field `width` wide reading `placeholder` while empty, whose button
    /// takes its element id from `id`.
    pub fn new(
        id: &'static str,
        placeholder: impl Into<SharedString>,
        width: Pixels,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| TextInput::new(cx, placeholder, 0));
        let edited = cx.subscribe(&input, |field, _, _: &TextEdited, cx| field.edited(cx));
        Self {
            input,
            width,
            clear_id: ElementId::Name(format!("{id}-clear").into()),
            settling: Task::ready(()),
            _edited: edited,
        }
    }

    pub fn text(&self, cx: &App) -> SharedString {
        self.input.read(cx).content()
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.set_content("", cx));
    }

    fn edited(&mut self, cx: &mut Context<Self>) {
        cx.emit(QueryFieldEvent::Edited);
        cx.notify();
        if self.text(cx).is_empty() {
            self.settling = Task::ready(());
            cx.emit(QueryFieldEvent::Settled);
            return;
        }
        self.settling = cx.spawn(async move |field, cx| {
            cx.background_executor().timer(SETTLE).await;
            let _ = field.update(cx, |_, cx| cx.emit(QueryFieldEvent::Settled));
        });
    }
}

impl EventEmitter<QueryFieldEvent> for QueryField {}

impl Focusable for QueryField {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Render for QueryField {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let typed = !self.text(cx).is_empty();
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(div().w(self.width).flex_none().child(self.input.clone()))
            .children(typed.then(|| {
                let clear = cx.listener(|field, _: &ClickEvent, _, cx| field.clear(cx));
                Button::new(self.clear_id.clone(), clear).ghost().icon("x")
            }))
    }
}
