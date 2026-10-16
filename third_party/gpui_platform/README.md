# gpui_platform (zed v1.6.3), vendored

The source of `crates/gpui_platform` at zed's `v1.6.3` tag (Apache-2.0, see
`LICENSE-APACHE`), with one change to `Cargo.toml`: it depends on `gpui_web`
with `default-features = false`.

`gpui_web`'s default `multithreaded` feature runs gpui's background executor
on Web Workers through `wasm_thread`, which compiles only with a nightly
compiler and a standard library rebuilt with atomics, and needs the page
served cross-origin isolated. Without it, gpui's web platform runs every task
on the browser's main thread, on the same stable toolchain the desktop app
builds with. Desktop targets resolve exactly the crates they did.
