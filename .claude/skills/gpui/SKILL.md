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
