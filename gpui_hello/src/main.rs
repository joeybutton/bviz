mod fonts;
mod text_input;

use gpui::{
    App, Bounds, BoxShadow, Context, CursorStyle, Decorations, Entity, FocusHandle, HitboxBehavior,
    Hsla, IntoElement, KeyBinding, MouseButton, Pixels, Point, ResizeEdge, Render, Rgba,
    SharedString, Size, Tiling, TitlebarOptions, Window, WindowBounds, WindowDecorations,
    WindowOptions, actions, canvas, colors::Colors, div, point, prelude::*, px, size,
    transparent_black,
};
use gpui_platform::application;
use std::cell::Cell;
use std::rc::Rc;
use text_input::TextInput;

actions!(gpui_hello, [Quit, CloseWindow, Tab, TabPrev, Greet]);

/// Room left around the window for the drop shadow, and the resize-edge hit zone.
const SHADOW: Pixels = px(10.0);
const ROUNDING: Pixels = px(10.0);
const BORDER: Pixels = px(1.0);

struct HelloWorld {
    text: SharedString,
    focus_handle: FocusHandle,
    input: Entity<TextInput>,
    button_focus: FocusHandle,
    /// Set when the title bar is pressed; the move starts on the first drag so a
    /// press that the compositor claims for an edge resize never turns into a move.
    should_move: Rc<Cell<bool>>,
}

impl HelloWorld {
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

    /// A focusable button that reads the field and updates the greeting.
    fn greet_button(&self, colors: &Colors, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.button_focus.is_focused(window);
        let ring = if focused { colors.text } else { colors.selected };
        div()
            .id("greet")
            .key_context("GreetButton")
            .track_focus(&self.button_focus)
            .on_action(cx.listener(|this, _: &Greet, _, cx| this.greet(cx)))
            .on_click(cx.listener(|this, _, _, cx| this.greet(cx)))
            .flex()
            .items_center()
            .justify_center()
            .px_4()
            .py_2()
            .rounded_md()
            .cursor_pointer()
            .bg(colors.selected)
            .text_color(colors.selected_text)
            .border_2()
            .border_color(ring)
            .child("Greet")
    }
}

impl HelloWorld {
    /// A client-side title bar: draggable, with minimize/maximize/close controls.
    /// Only drawn when the compositor doesn't provide server-side decorations.
    fn title_bar(&self, colors: &Colors, radius: Pixels, bar_bg: Rgba) -> impl IntoElement {
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
            .rounded_tl(radius)
            .rounded_tr(radius)
            .bg(bar_bg)
            .border_b_1()
            .border_color(colors.border)
            .on_mouse_down(MouseButton::Left, {
                let should_move = self.should_move.clone();
                move |_, _, _| should_move.set(true)
            })
            .on_mouse_up(MouseButton::Left, {
                let should_move = self.should_move.clone();
                move |_, _, _| should_move.set(false)
            })
            .on_mouse_move({
                let should_move = self.should_move.clone();
                move |_, window, _| {
                    if should_move.replace(false) {
                        window.start_window_move();
                    }
                }
            })
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

    fn status_bar(&self, colors: &Colors, radius: Pixels, bar_bg: Rgba) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(24.))
            .px_2()
            .rounded_bl(radius)
            .rounded_br(radius)
            .bg(bar_bg)
            .border_t_1()
            .border_color(colors.border)
            .text_color(colors.disabled)
            .text_sm()
            .font_family(fonts::MONO)
            .child("Ready")
    }
}

impl Render for HelloWorld {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // gpui's built-in palette, light or dark per the system appearance.
        let colors = Colors::for_appearance(window);
        let decorations = window.window_decorations();
        let tiling = match decorations {
            Decorations::Server => Tiling::default(),
            Decorations::Client { tiling } => tiling,
        };
        let client = matches!(decorations, Decorations::Client { .. });
        // Reserve room for the shadow (client side) so it isn't clipped by the window edge.
        window.set_client_inset(if client { SHADOW } else { px(0.) });
        let radius = if client && !tiling.is_tiled() {
            ROUNDING
        } else {
            px(0.)
        };

        // Dim the chrome and soften the shadow when the window isn't focused.
        let active = window.is_window_active();
        let bar_bg = if active {
            colors.container
        } else {
            mix(colors.container, colors.background, 0.6)
        };
        let shadow_alpha = if active { 0.45 } else { 0.18 };

        // The window contents: title bar (client only), body, status bar.
        let inner = div()
            .flex()
            .flex_col()
            .size_full()
            .bg(colors.background)
            .text_color(colors.text)
            .when(client, |this| {
                this.border_color(colors.border)
                    .when(!tiling.top, |d| d.border_t(BORDER))
                    .when(!tiling.bottom, |d| d.border_b(BORDER))
                    .when(!tiling.left, |d| d.border_l(BORDER))
                    .when(!tiling.right, |d| d.border_r(BORDER))
                    .when(!tiling.top && !tiling.left, |d| d.rounded_tl(ROUNDING))
                    .when(!tiling.top && !tiling.right, |d| d.rounded_tr(ROUNDING))
                    .when(!tiling.bottom && !tiling.left, |d| d.rounded_bl(ROUNDING))
                    .when(!tiling.bottom && !tiling.right, |d| d.rounded_br(ROUNDING))
                    .when(!tiling.is_tiled(), |d| {
                        d.shadow(vec![BoxShadow {
                            color: Hsla {
                                h: 0.,
                                s: 0.,
                                l: 0.,
                                a: shadow_alpha,
                            },
                            blur_radius: SHADOW / 2.,
                            spread_radius: px(0.),
                            offset: point(px(0.), px(0.)),
                            inset: false,
                        }])
                    })
            })
            .when(client, |this| {
                this.child(self.title_bar(&colors, radius, bar_bg))
            })
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
                    .child(self.greet_button(&colors, window, cx)),
            )
            .child(self.status_bar(&colors, radius, bar_bg));

        div()
            .id("window-backdrop")
            .font_family(fonts::UI)
            .track_focus(&self.focus_handle)
            .on_action(|_: &Quit, _, cx| cx.quit())
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .on_action(|_: &Tab, window, cx| window.focus_next(cx))
            .on_action(|_: &TabPrev, window, cx| window.focus_prev(cx))
            .size_full()
            .bg(transparent_black())
            .when(client, |this| {
                this.when(!tiling.top, |d| d.pt(SHADOW))
                    .when(!tiling.bottom, |d| d.pb(SHADOW))
                    .when(!tiling.left, |d| d.pl(SHADOW))
                    .when(!tiling.right, |d| d.pr(SHADOW))
                    .on_mouse_move(|_, window, _| window.refresh())
                    .on_mouse_down(MouseButton::Left, move |e, window, _| {
                        let size = window.window_bounds().get_bounds().size;
                        if let Some(edge) = resize_edge(e.position, SHADOW, size, tiling) {
                            window.start_window_resize(edge);
                        }
                    })
                    // Behind the content; updates the resize cursor near the edges.
                    .child(
                        canvas(
                            |_, window, _| {
                                window.insert_hitbox(
                                    Bounds::new(
                                        point(px(0.), px(0.)),
                                        window.window_bounds().get_bounds().size,
                                    ),
                                    HitboxBehavior::Normal,
                                )
                            },
                            move |_, hitbox, window, _| {
                                let size = window.window_bounds().get_bounds().size;
                                let Some(edge) =
                                    resize_edge(window.mouse_position(), SHADOW, size, tiling)
                                else {
                                    return;
                                };
                                window.set_cursor_style(
                                    match edge {
                                        ResizeEdge::Top | ResizeEdge::Bottom => {
                                            CursorStyle::ResizeUpDown
                                        }
                                        ResizeEdge::Left | ResizeEdge::Right => {
                                            CursorStyle::ResizeLeftRight
                                        }
                                        ResizeEdge::TopLeft | ResizeEdge::BottomRight => {
                                            CursorStyle::ResizeUpLeftDownRight
                                        }
                                        ResizeEdge::TopRight | ResizeEdge::BottomLeft => {
                                            CursorStyle::ResizeUpRightDownLeft
                                        }
                                    },
                                    &hitbox,
                                );
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
            })
            .child(inner)
    }
}

/// Linearly blend `a` toward `b` by `t` in [0, 1].
fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    Rgba {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

/// Which resize edge or corner the position is on, within `shadow_size` of the window border.
fn resize_edge(
    pos: Point<Pixels>,
    shadow_size: Pixels,
    window_size: Size<Pixels>,
    tiling: Tiling,
) -> Option<ResizeEdge> {
    let bounds = Bounds::new(Point::default(), window_size).inset(shadow_size * 1.5);
    if bounds.contains(&pos) {
        return None;
    }

    let corner = size(shadow_size * 1.5, shadow_size * 1.5);
    if !tiling.top && Bounds::new(point(px(0.), px(0.)), corner).contains(&pos) {
        return Some(ResizeEdge::TopLeft);
    }
    if !tiling.top
        && Bounds::new(point(window_size.width - corner.width, px(0.)), corner).contains(&pos)
    {
        return Some(ResizeEdge::TopRight);
    }
    if !tiling.bottom
        && Bounds::new(point(px(0.), window_size.height - corner.height), corner).contains(&pos)
    {
        return Some(ResizeEdge::BottomLeft);
    }
    if !tiling.bottom
        && Bounds::new(
            point(
                window_size.width - corner.width,
                window_size.height - corner.height,
            ),
            corner,
        )
        .contains(&pos)
    {
        return Some(ResizeEdge::BottomRight);
    }

    if !tiling.top && pos.y < shadow_size {
        Some(ResizeEdge::Top)
    } else if !tiling.bottom && pos.y > window_size.height - shadow_size {
        Some(ResizeEdge::Bottom)
    } else if !tiling.left && pos.x < shadow_size {
        Some(ResizeEdge::Left)
    } else if !tiling.right && pos.x > window_size.width - shadow_size {
        Some(ResizeEdge::Right)
    } else {
        None
    }
}

fn main() {
    application().run(|cx: &mut App| {
        fonts::load(cx);
        cx.bind_keys([
            KeyBinding::new("secondary-q", Quit, None),
            KeyBinding::new("secondary-w", CloseWindow, None),
            KeyBinding::new("tab", Tab, None),
            KeyBinding::new("shift-tab", TabPrev, None),
            KeyBinding::new("enter", Greet, Some("GreetButton")),
            KeyBinding::new("space", Greet, Some("GreetButton")),
        ]);
        text_input::bind_keys(cx);
        // Quit once the last window is gone (closing a window doesn't quit by default).
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

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
                cx.new(|cx| {
                    let focus_handle = cx.focus_handle();
                    focus_handle.focus(window, cx);
                    let input = cx.new(|cx| TextInput::new(cx, "Type a name…", 1));
                    HelloWorld {
                        text: "bazel_viz".into(),
                        focus_handle,
                        input,
                        button_focus: cx.focus_handle().tab_index(2).tab_stop(true),
                        should_move: Rc::new(Cell::new(false)),
                    }
                })
            },
        )
        .unwrap();
        cx.activate(true);
    });
}
