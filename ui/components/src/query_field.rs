//! A text field with a button that empties it, which says what it holds once
//! typing settles.

use crate::text_input::{CursorMoved, TextEdited};
use crate::{Button, TextInput};
use gpui::{
    App, ClickEvent, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    Pixels, Render, SharedString, Subscription, Task, Window, div, prelude::*,
};
use std::ops::Range;
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
    /// The cursor moved over text unchanged.
    CursorMoved,
}

/// A one-line text field with a button that empties it, shown while it holds
/// anything.
pub struct QueryField {
    input: Entity<TextInput>,
    width: Pixels,
    /// Where set, the field fills its holder up to `width` wide and narrows
    /// with it to this width.
    least: Option<Pixels>,
    clear_id: ElementId,
    /// Waits for the text to settle; an edit replaces it, and so gives up the
    /// wait for the text it changed.
    settling: Task<()>,
    _edited: Subscription,
    _moved: Subscription,
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
        let moved = cx.subscribe(&input, |_, _, _: &CursorMoved, cx| {
            cx.emit(QueryFieldEvent::CursorMoved)
        });
        Self {
            input,
            width,
            least: None,
            clear_id: ElementId::Name(format!("{id}-clear").into()),
            settling: Task::ready(()),
            _edited: edited,
            _moved: moved,
        }
    }

    /// The field narrowing with its holder down to `least` wide, for a holder
    /// whose room gives way as its window narrows.
    pub fn narrowing_to(mut self, least: Pixels) -> Self {
        self.least = Some(least);
        self
    }

    pub fn text(&self, cx: &App) -> SharedString {
        self.input.read(cx).content()
    }

    /// Replaces what the field holds, which settles as an edit typed does.
    pub fn set_text(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_content(text, cx));
    }

    /// Where the cursor stands, as a byte offset into the text.
    pub fn cursor(&self, cx: &App) -> usize {
        self.input.read(cx).cursor()
    }

    /// Replaces the byte range `range` of the text with `text`, which settles
    /// as an edit typed does.
    pub fn replace(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.replace(range, text, cx));
    }

    /// Moves the cursor to the byte offset `offset` into the text.
    pub fn set_cursor(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_cursor(offset, cx));
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
        let input = div().child(self.input.clone());
        let input = match self.least {
            None => input.w(self.width).flex_none(),
            Some(_) => input.flex_1().min_w_0().max_w(self.width),
        };
        div()
            .when_some(self.least, |field, least| field.w_full().min_w(least))
            .flex()
            .items_center()
            .gap_1()
            .child(input)
            .children(typed.then(|| {
                let clear = cx.listener(|field, _: &ClickEvent, _, cx| field.clear(cx));
                Button::new(self.clear_id.clone(), clear)
                    .ghost()
                    .icon("x")
                    .tooltip("Clear field")
            }))
    }
}
