use gpui::{
    App, Bounds, Context, IntoElement, Render, SharedString, Window, WindowBounds, WindowOptions,
    colors::Colors, div, prelude::*, px, size,
};
use gpui_platform::application;

struct HelloWorld {
    text: SharedString,
}

impl Render for HelloWorld {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // gpui's built-in palette, light or dark per the system appearance.
        let colors = Colors::for_appearance(window);
        div()
            .flex()
            .flex_col()
            .gap_3()
            .size_full()
            .justify_center()
            .items_center()
            .bg(colors.background)
            .text_xl()
            .text_color(colors.text)
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
            |window, cx| {
                // Re-render when the system switches between light and dark.
                window
                    .observe_window_appearance(|window, _cx| window.refresh())
                    .detach();
                cx.new(|_cx| HelloWorld {
                    text: "bazel_viz".into(),
                })
            },
        )
        .unwrap();
        cx.activate(true);
    });
}
