# bviz in the browser

This fork of [yume-ag/bviz](https://github.com/yume-ag/bviz) builds the bviz
desktop app for the browser as well. The browser build is the same
`//bviz:bviz` binary, compiled for `wasm32-unknown-unknown` and drawn by gpui's
web platform with WebGPU. It has the same viewers (graph, trace, console
replay, spawn times), panels, keys and themes. A change to the app reaches the
browser build without anything web-specific to update.

## Build and run

```sh
bazel run --config=wasm //web:serve   # http://127.0.0.1:8080
web/build.sh                          # or just build //web:site
```

Bazel builds it with the repository's own pinned Rust toolchain (stable 1.95,
with its prebuilt `wasm32-unknown-unknown` standard library). The
`wasm-bindgen` CLI is pinned in `multitool.lock.json` at the version of the
`wasm-bindgen` crate `Cargo.lock` pins. No rustup, wasm-pack or Node is
involved. `//web:site` is a static directory: `index.html`, the synthetic
profile it offers, and the app under `pkg/`. Any static host serves it, and no
special headers are needed.

## Using it

A browser has neither a command line nor a filesystem, so the page stands in
for both:

- **Files:** pick or drop files. Each one gets the flag its name and contents
  suggest (`--profile`, `--bep`, `--exec_log`, `--aquery`, `--cquery`,
  `--query`), which you can change.
- **The command line:** the page shows the command line those files make, and
  you can edit it. It goes through the desktop app's own clap parser, so every
  flag, and every usage error, is the desktop app's.
- **Links:** `?profile=URL&bep=URL&…` fetches the files and opens them at once.
  The host serving them must allow the page to fetch them (CORS).

Files are read in the tab and never uploaded. A browser needs WebGPU (current
Chrome, Edge and Safari; Firefox where WebGPU is enabled).

## How the app runs in a browser

| Desktop | Browser | Where |
|---|---|---|
| Files named on the command line | Bytes the page hands over under those names | `util/files` (`bviz_files`): `std::fs` on a desktop, an in-memory store in a browser. Every reader of a named file goes through it. |
| `fn main` parses `argv` | The page calls `start(args)` | `bviz/src/web.rs` runs the same `Cli` parse and `open_window` |
| Worker threads and rayon pools | The page's one thread | `util/parallel`: the one pool is the calling thread, which the page claims first so every parallel pass runs in it, and `background()` runs deferred work when it is joined |
| `std::time::Instant` | `performance.now()` | `web-time`, a re-export of `std::time` on a desktop |
| gpui's macOS/Linux platform | `gpui_web` (WebGPU) | `third_party/gpui_platform`, `patches/gpui_web` |

### Pure-Rust dependencies, on every target

The desktop app now builds from the same Rust-only crates as the browser, so
the two do not diverge:

- **zstd:** `ruzstd` decodes execution logs everywhere, through a stream
  adapter (`execlog/src/zstd.rs`) that reads as the zstd library does: an
  empty input is an empty stream, a cut-off one yields what was decoded ahead
  of the cut and then `UnexpectedEof`, and concatenated frames decode as one
  stream. The C zstd library, its `bazel_dep`, its patch and its `zstd-sys`
  annotation are gone.
- **Fuzzy matching:** the graph's palettes and query completion match with
  `nucleo-matcher` (the matcher `nucleo` is built on, with the same scoring)
  in time slices on the polling thread, rather than on `nucleo`'s own thread
  pool. A query extending the last one rescans only what that one matched.
- **Allocator:** the platform allocator on every target, with no C
  allocator (mimalloc) to build. On glibc, `bviz_alloc` keeps freed memory
  across a load through `mallopt`, which is what mimalloc's purge delay did.
  A 1.5M-slice profile loads in the same time on macOS as before (about 0.23 s
  in `//trace/profile:bench_load`). glibc Linux is not yet measured.
- **Terminal emulator:** `third_party/alacritty_terminal` builds its pty and
  event loop only for unix and windows. bviz uses the emulator alone to replay
  a recorded console.

## Changes to upstream code

bviz:

- `util/files` (new): file reads that work without a filesystem.
- `util/parallel`: one single-thread pool on wasm, shared by `workers()` and
  `background_workers()`; `background()` and `CAN_SPAWN`.
- `trace/profile`, `execlog`, `terminal`, `ui/console`, `ui/graph`: read
  through `bviz_files`. The profile parser and lane merge run single-worker
  passes inline. The execution log decodes with `ruzstd`.
- `ui/components`: `Pending` starts its work through `bviz_parallel::background`.
- `ui/graph/src/fuzzy`: matching on `nucleo-matcher` in time slices.
- `ui/workspace`: a panel drag's mime type names the process with a per-process
  random token, since a browser has no process ids.
- `ui/graph`, `ui/trace`, `ui/playback`: `web_time::Instant`.
- `alloc`: the platform allocator, as above.
- `trace/layout`: the flame row size assertion holds on 64-bit targets only.
- `critical_path/Cargo.toml`: declares its `bviz_invocation` dependency, which
  its Bazel BUILD file already had.
- `bviz`: `src/web.rs`, the browser entry points, and `//bviz:bviz_web`.
- `web/`: the launcher page, `//web:site` and `//web:serve`.

Dependencies:

- `third_party/gpui_platform`: zed's, taking `gpui_web` without its
  `multithreaded` default (see below).
- `third_party/alacritty_terminal`: 0.26.0 with the pty and event loop
  built for desktop targets only.
- `patches/gpui/gpui-web-keep-application-alive.patch`: on the web `run`
  returns at once, which dropped the application, and with it every window,
  as soon as it launched.
- `patches/gpui_web/gpui_web-single-threaded-dispatcher.patch`: the
  single-threaded dispatcher did not compile at zed v1.6.3.
- `patches/gpui_web/gpui_web-embedded-fonts-only.patch`: the app registers its
  own embedded fonts, as it does on Linux.

Build:

- `MODULE.bazel`: crate selects also resolve for `wasm32-unknown-unknown`;
  `rules_rs`'s wasm-bindgen extension; `//tools/wasm_bindgen:toolchain`;
  `gpui_web`'s patches; zstd removed.
- `.bazelrc`: `--config=wasm` (optimized, cross-crate LTO).
- `.github/workflows/wasm.yaml`: builds `//web:site` and deploys it to GitHub
  Pages from `main`.

## Threads

The browser build runs on one thread. gpui's web platform can run its
background executor on Web Workers (`gpui_web`'s `multithreaded` feature), but
that needs:

- a nightly compiler and a standard library rebuilt with atomics, which
  `rules_rs` builds only for tier-3 targets;
- the page served cross-origin isolated (COOP/COEP headers, which GitHub
  Pages cannot set).

It would move only gpui's own tasks off the main thread. bviz's parallel
passes (the profile parse, rayon) would also need `wasm-bindgen-rayon` and a
thread shim. Until then, a large file blocks the tab while it loads.

## Limits

- One thread, as above.
- WebGPU only: the wgpu build has no WebGL fallback.
- wasm32 memory tops out at 4 GiB.
- `bviz bazelrc` writes to a terminal; the browser build returns its text
  (`bazelrc(args)`) instead.

The fork is licensed Apache-2.0 like upstream (see `LICENSE` and `NOTICE`).
Vendored and patched third-party code keeps its own licence (Apache-2.0).
