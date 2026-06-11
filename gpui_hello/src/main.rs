use gpui::{
    App, Bounds, Context, IntoElement, Render, SharedString, Window, WindowBounds, WindowOptions,
    div, prelude::*, px, rgb, size,
};
use gpui_platform::application;

struct HelloWorld {
    text: SharedString,
}

impl Render for HelloWorld {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .size_full()
            .justify_center()
            .items_center()
            .bg(rgb(0x1e1e1e))
            .text_xl()
            .text_color(rgb(0xffffff))
            .child(format!("Hello, {}!", self.text))
    }
}

fn main() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(400.), px(300.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_window, cx| {
                cx.new(|_cx| HelloWorld {
                    text: "bazel_viz".into(),
                })
            },
        )
        .unwrap();
        cx.activate(true);
    });
}
