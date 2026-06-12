use bviz_ui_components::{Button, TextInput};
use bviz_ui_shell::Chrome;
use gpui::{
    Context, Entity, FocusHandle, IntoElement, Render, SharedString, Window, div, prelude::*, px,
};

/// A small demo view: a greeting driven by a text field and a button, wrapped in
/// the application shell's window chrome.
struct HelloWorld {
    text: SharedString,
    focus_handle: FocusHandle,
    input: Entity<TextInput>,
    button_focus: FocusHandle,
}

impl HelloWorld {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let input = cx.new(|cx| TextInput::new(cx, "Type a name…", 1));
        Self {
            text: "bazel_viz".into(),
            focus_handle,
            input,
            button_focus: cx.focus_handle().tab_index(2).tab_stop(true),
        }
    }

    /// Set the greeting from the text field (or a default when it's empty).
    fn greet(&mut self, cx: &mut Context<Self>) {
        let name = self.input.read(cx).content();
        self.text = if name.is_empty() {
            "bazel_viz".into()
        } else {
            name
        };
        cx.notify();
    }

    fn greet_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        Button::new("greet", cx.listener(|this, _, _, cx| this.greet(cx)))
            .label("Greet")
            .track_focus(&self.button_focus)
    }
}

impl Render for HelloWorld {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = bviz_ui_theme::colors(window);
        Chrome::new(&self.focus_handle)
            .title("bazel_viz")
            .status("Ready")
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .items_center()
                    .gap_4()
                    .p_4()
                    .child(div().text_xl().child(format!("Hello, {}!", self.text)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .w(px(260.))
                            .child(div().text_sm().text_color(colors.disabled).child("Name"))
                            .child(self.input.clone()),
                    )
                    .child(self.greet_button(cx)),
            )
    }
}

fn main() {
    bviz_ui_shell::run("bazel_viz", |window, cx| {
        cx.new(|cx| HelloWorld::new(window, cx))
    });
}
