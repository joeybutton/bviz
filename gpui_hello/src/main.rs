use gpui::{
    App, Bounds, Context, Decorations, IntoElement, MouseButton, Render, SharedString,
    TitlebarOptions, Window, WindowBounds, WindowDecorations, WindowOptions, colors::Colors, div,
    prelude::*, px, size,
};
use gpui_platform::application;

struct HelloWorld {
    text: SharedString,
}

impl HelloWorld {
    /// A client-side title bar: draggable, with minimize/maximize/close controls.
    /// Only drawn when the compositor doesn't provide server-side decorations.
    fn title_bar(&self, colors: &Colors) -> impl IntoElement {
        let text = colors.text;
        let hover_bg = colors.selected;
        let button = move |id: &'static str, glyph: &'static str| {
            div()
                .id(id)
                .flex()
                .items_center()
                .justify_center()
                .size(px(28.))
                .rounded_md()
                .text_color(text)
                .hover(move |s| s.bg(hover_bg))
                .child(glyph)
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .h(px(36.))
            .px_2()
            .bg(colors.container)
            .border_b_1()
            .border_color(colors.border)
            .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            .child(div().px_1().child(self.text.clone()))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_1()
                    // Don't let a button press also start a window move.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(button("minimize", "—").on_click(|_, window, _| window.minimize_window()))
                    .child(button("maximize", "▢").on_click(|_, window, _| window.zoom_window()))
                    .child(button("close", "✕").on_click(|_, window, _| window.remove_window())),
            )
    }

    fn status_bar(&self, colors: &Colors) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(24.))
            .px_2()
            .bg(colors.container)
            .border_t_1()
            .border_color(colors.border)
            .text_color(colors.disabled)
            .text_sm()
            .child("Ready")
    }
}

impl Render for HelloWorld {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // gpui's built-in palette, light or dark per the system appearance.
        let colors = Colors::for_appearance(window);
        let client_decorated = matches!(window.window_decorations(), Decorations::Client { .. });

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(colors.background)
            .text_color(colors.text)
            // A border and our own title bar only when we draw the decorations.
            .when(client_decorated, |this| {
                this.border_1()
                    .border_color(colors.border)
                    .child(self.title_bar(&colors))
            })
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .items_center()
                    .text_xl()
                    .child(format!("Hello, {}!", self.text)),
            )
            .child(self.status_bar(&colors))
    }
}

fn main() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(500.), px(360.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("bazel_viz".into()),
                    ..Default::default()
                }),
                app_id: Some("bazel_viz".into()),
                // Prefer the compositor's decorations; fall back to drawing our own
                // when it only supports client-side (e.g. GNOME).
                window_decorations: Some(WindowDecorations::Server),
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
