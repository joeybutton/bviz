---
name: gpui
description: How to write, build, and run gpui (Zed's GPU-accelerated UI framework) code in the bazel_viz repo — opening an application window, defining views and elements/widgets, styling with the div() builder, and wiring click/keyboard interaction. Use this whenever a task involves gpui, a `Render` view, `div()` element styling, the desktop UI layer, or the eventual Rust visualization frontend — even if "gpui" is not named explicitly.
---

# Writing gpui apps

gpui is Zed's GPU-accelerated UI framework. This repo will use it for the Rust
visualization frontend. The authoritative reference is real gpui code at a known release
tag — read it rather than guessing APIs, since the API surface shifts between versions.
The links below pin to **Zed v1.6.3**. If a local Zed checkout is available, prefer reading
it over fetching online; confirm its tag matches the version this repo targets.

- Runnable examples: https://github.com/zed-industries/zed/tree/v1.6.3/crates/gpui/examples
- Framework source: https://github.com/zed-industries/zed/tree/v1.6.3/crates/gpui/src
- Reusable components (Button, Label, …): https://github.com/zed-industries/zed/tree/v1.6.3/crates/ui/src

See `references/examples.md` for which example demonstrates what.

## Mental model: retained state, immediate element tree

This is the one thing to internalize; everything else follows from it.

- **Views/entities are retained.** A struct implementing `Render` is an *entity* held by
  gpui across frames. It owns your mutable state (selection, scroll, the parsed graph).
  Create one with `cx.new(|cx| MyView { .. })`, which returns an `Entity<MyView>`.
- **The element tree is immediate.** `render()` returns a fresh tree of elements
  (`div()`, text, widgets) describing *this frame*. The tree is rebuilt and dropped each
  frame — never store element references; store state in the view struct or child entities.
- **Rendering is notify-driven, not a vsync loop.** `render()` runs only when the view is
  marked dirty: call `cx.notify()` after mutating state, and the framework re-renders. Idle
  views do no work.

Consequence: mutate `self` in an event handler, call `cx.notify()`, and the UI updates.

## Minimal app: window + widgets + a click counter

```rust
use gpui::{
    div, prelude::*, px, rgb, App, Bounds, Context, IntoElement, Render, Window,
    WindowBounds, WindowOptions, size,
};
use gpui_platform::application;

struct Counter {
    count: usize,
}

impl Render for Counter {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()                         // enable flex layout (required before flex_col)
            .flex_col()
            .gap_3()
            .size_full()                    // fill the window; unsized content collapses
            .justify_center()
            .items_center()
            .bg(rgb(0x1e1e1e))
            .text_color(rgb(0xffffff))
            .child(format!("Count: {}", self.count))
            .child(
                div()
                    .id("increment")        // .id() makes the element interactive
                    .px_4()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(0x3b82f6))
                    .cursor_pointer()
                    .on_click(_cx.listener(|this, _event, _window, cx| {
                        this.count += 1;
                        cx.notify();         // re-render after mutating state
                    }))
                    .child("Increment"),
            )
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
            |_window, cx| cx.new(|_cx| Counter { count: 0 }),
        )
        .unwrap();
        cx.activate(true);
    });
}
```

Note: the entrypoint (`gpui_platform::application()` vs `gpui::Application::new()`) is
version-dependent — confirm against `examples/hello_world.rs` at the referenced tag.

## Building UI: `div()` and the `Styled` builder

`div()` is the workhorse element. `use gpui::prelude::*` brings in the traits that provide
the fluent methods. The vocabulary is Tailwind-like (defined in `gpui/src/styled.rs`):

- Layout: `.flex()`, `.flex_col()` / `.flex_row()`, `.gap_2()`, `.justify_center()`, `.items_center()`, `.flex_1()`
- Box: `.size_full()`, `.w_full()`, `.h(px(40.))`, `.size(px(80.))`, `.p_4()`, `.px_2()`, `.py_1()`, `.m_2()`
- Paint: `.bg(rgb(0x..))`, `.text_color(..)`, `.rounded_md()`, `.border_1()`, `.border_color(..)`, `.shadow_lg()`
- Text: `.text_xl()`, `.text_sm()`
- Children: `.child(x)` for one, `.children(iter)` for many; `x` can be a string, another element, or any `impl IntoElement`.

## Colors and dark/light

Prefer gpui's built-in palette over hardcoded `rgb(0x…)`, so the UI follows the system
appearance for free. `gpui::colors::Colors` has fields like `background`, `text`, `border`,
`container`, `selected`; `Colors::for_appearance(window)` returns the light or dark set from
`window.appearance()` (on Linux, the xdg-desktop-portal color scheme). Use it in `render`:

```rust
let colors = gpui::colors::Colors::for_appearance(window);
div().bg(colors.background).text_color(colors.text)
```

To follow live OS theme switches, re-render on appearance change (once, at window setup):

```rust
window.observe_window_appearance(|window, _cx| window.refresh()).detach();
```

For many views, set `GlobalColors` from the appearance once and read `cx.default_colors()`
instead of threading `window` everywhere — but note the registered global defaults to light
until you set it. Zed's full `theme` crate (`cx.theme()`, named themes, registry) is much
heavier and not needed here. Raw `rgb(0x…)` / `gpui::white()` are still fine for one-off accents.

Fonts: register font bytes with `cx.text_system().add_fonts(vec![bytes…])` at startup, then
select them with `.font_family("Inter")` (UI) / `.font_family("JetBrains Mono")` (mono), which
cascade to children; glyphs missing from the chosen family fall back to system fonts
automatically. In this repo the font files are downloaded by a Bazel `http_archive`, listed as
`data`, and **embedded into the binary** at build time — the `app_assets` library's build
script copies them into `OUT_DIR` so `include_bytes!` can bake them in — so there are no runtime
file dependencies. See `app_assets` and the `inter` / `jetbrains_mono` archives in `MODULE.bazel`.

Icons: gpui renders SVGs with `svg().path("…").size(px(16.)).text_color(…)`, loading the path
through the app's `AssetSource` (`application().with_assets(MyAssets)`) and tinting the result
with the text color (so single-color icon sets like Phosphor just work). In this repo the
Phosphor SVGs come from an `http_archive` and are embedded the same way; the `AssetSource`
serves them from the baked-in bytes — see the `app_assets` library and the `phosphor` archive.

## Window chrome: title, decorations, controls, status bar

Set the OS title and grouping id in `WindowOptions` (the title shows in the taskbar and in
any server-drawn title bar):

```rust
WindowOptions {
    titlebar: Some(TitlebarOptions { title: Some("bazel_viz".into()), ..Default::default() }),
    app_id: Some("bazel_viz".into()),
    window_decorations: Some(WindowDecorations::Server), // prefer the compositor's chrome
    ..Default::default()
}
```

Decorations on Linux are negotiated: some compositors draw server-side decorations (SSD),
others (notably GNOME) only support client-side (CSD), where **the app must draw its own
title bar**. Query `window.window_decorations()` each render and adapt:

- `Decorations::Server` — the compositor drew the title bar; draw only your content.
- `Decorations::Client { tiling }` — draw your own title bar (and ideally a border).

A title bar is just a `div`: drag it with `window.start_window_move()` on mouse-down, and wire
control buttons to `window.minimize_window()`, `window.zoom_window()` (maximize), and
`window.remove_window()` (close). Put `cx.stop_propagation()` on the controls' `on_mouse_down`
so pressing a button doesn't also start a window move. A status bar is likewise just a `div` of
app content — gpui has no dedicated widget for it. `//gpui_hello` shows the full pattern.

Resize, rounded corners, and a drop shadow (the rest of client-side decorations) need a
two-layer structure, which `//gpui_hello` implements in full:

- An outer **backdrop** `div` reserves room for the shadow with padding and calls
  `window.set_client_inset(shadow)`. On mouse-down near an edge it calls
  `window.start_window_resize(edge)` (compute the edge from the cursor vs. window bounds);
  a behind-the-content `canvas` sets the resize cursor as the mouse nears an edge.
- An inner `div` draws the border, rounded corners, and the `BoxShadow`.
- gpui's content mask is **rectangular**, so rounding a parent does not clip children — give
  the title bar and status bar matching rounded corners rather than relying on `overflow_hidden`.
- Respect `Decorations::Client { tiling }`: skip the border/rounding/shadow on any tiled edge
  (e.g. when a tiling WM snaps the window), and skip them entirely for `Decorations::Server`.

`examples/window_shadow.rs` in the gpui repo is the upstream reference for the same pattern.

For client-side chrome, reflect focus: read `window.is_window_active()` in `render` (gpui
re-renders automatically when activation changes) and dim the title bar / soften the shadow
when it returns false. Defer the title-bar move to the first mouse-move after the press (a flag
set on `on_mouse_down`, acted on in `on_mouse_move`) so a press near a corner that the
compositor takes for an edge resize doesn't also become a window move.

## Interactivity

To handle input, an element needs an `.id(..)` (this makes it a `Stateful` element).
Then attach handlers; wrap them in `cx.listener(..)` to get mutable access to your view:

```rust
.on_click(cx.listener(|this, event: &ClickEvent, window, cx| { /* mutate this; cx.notify() */ }))
```

`cx.listener` adapts `|&mut View, &Event, &mut Window, &mut Context<View>|` into the raw
handler signature gpui expects. Other handlers (same pattern): `.on_mouse_down(button, ..)`,
`.on_key_down(..)`, `.hover(|style| style.bg(..))`, `.on_scroll_wheel(..)`. For focus, add
`.focusable()` / `.track_focus(&handle)`. See `examples/a11y.rs` for a focusable counter.

## Actions, keybindings, and focus

Keyboard interaction goes through actions, not raw key handling. Declare unit actions with
`actions!(namespace, [Quit, CloseWindow])`, bind keys with `cx.bind_keys([KeyBinding::new(
"secondary-w", CloseWindow, Some("Context"))])` (the `secondary-` modifier is cmd on macOS and
ctrl elsewhere; the optional context scopes the binding to a `key_context`), and handle them
with `.on_action(|_: &CloseWindow, window, cx| …)` on an element in the focus path. Actions
dispatch up from the focused element, so a view needs `.track_focus(&handle)` (with
`focus_handle: FocusHandle` created via `cx.focus_handle()`) to receive them.

Focus and tab order: configure a handle with `.tab_index(n).tab_stop(true)`, bind `tab` /
`shift-tab` to actions that call `window.focus_next(cx)` / `window.focus_prev(cx)`, and style
the focused state with `.focus(|s| …)`. App lifecycle: closing a window does not quit — wire
`cx.on_window_closed(|cx, _| if cx.windows().is_empty() { cx.quit() })`.

Text fields aren't a built-in widget: a focusable view implements `EntityInputHandler` (the
platform routes typed text and IME through it) and a custom `Element` shapes the line and
paints the cursor, registering itself via `window.handle_input(&focus_handle,
ElementInputHandler::new(bounds, entity), cx)`. `//gpui_hello`'s `text_input` module is a
minimal one; `examples/input.rs` is the full version with selection and clipboard.

## Stateful views vs stateless components

- **`impl Render for Foo`** — a stateful view (`fn render(&mut self, ..)`), held as an
  `Entity<Foo>`. Use for anything with its own state.
- **`#[derive(IntoElement)]` + `impl RenderOnce`** — a stateless, reusable component
  (`fn render(self, window, cx) -> impl IntoElement`, consumes `self`). This is how the
  `ui` crate builds Button/Label/Avatar. Use for parameterized, stateless widgets.

For the first widgets, plain `div()` trees inside a `Render` view are enough; reach for
`RenderOnce` only once you're repeating a widget shape.

## Child entities

Compose larger UIs from child entities rather than one giant view:

- Create: `let child = cx.new(|cx| Child::new(cx));` (store the `Entity<Child>` in a field).
- Render it: add the entity (cloned) as a `.child(self.child.clone())`.
- Update from outside: `self.child.update(cx, |child, cx| { child.foo = 1; cx.notify(); })`.

## Building and running in this repo

Build and run through Bazel (`rust_binary` via `rules_rs`), never cargo — see `CLAUDE.md`.
Crate deps come from the Cargo workspace and are exposed to Bazel as `@crates//:<name>`
through the `crate.from_cargo` extension in `MODULE.bazel`.

To add a dependency: add it under `[workspace.dependencies]` in the root `Cargo.toml` and
the consuming crate's `Cargo.toml`, repin with the Bazel-provided cargo (never system
cargo), commit `Cargo.lock`, then list it in the target's `deps` (e.g. `@crates//:gpui`).
Run with `bazel run //bazel_viz`.

## Gotchas

- No `.id()` → no interactivity. `on_click`/focus/state need a `Stateful` element.
- Forgetting `cx.notify()` after a state change → the UI silently won't update.
- `.flex_col()`/`.flex_row()` do nothing without `.flex()` first.
- Unsized content collapses; set an explicit size or `size_full()`/`flex_1()` in a flex parent.
- The element tree is rebuilt every render — hold `Entity<_>` references, never element ones.
