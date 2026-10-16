# bviz in the browser (wasm mode)

This fork of [yume-ag/bviz](https://github.com/yume-ag/bviz) compiles bviz's
trace libraries to WebAssembly and ships a single-page example that loads a
Bazel `--profile` in the browser. The profile is parsed in the tab; nothing is
uploaded.

## What runs in wasm, and what does not

bviz's desktop views are written with [GPUI](https://www.gpui.rs), which has no
browser backend, so the app itself does not run on the web. Everything beneath
those views is UI-free Rust and does:

| Library | Used for |
|---|---|
| `trace/profile` | Parsing gzipped or plain JSON profiles into bviz's interned model, span roles and lane classes |
| `trace/layout` | Lane layout, slice culling and merging, flame graph aggregation (kind/name labels, top-down/bottom-up, duration or wall-share weight), remainders, stats |
| `critical_path` | The wall-time partition along Bazel's critical path, and its levers |

`wasm/` is a [wasm-bindgen](https://rustwasm.github.io/wasm-bindgen/) crate
over those libraries. `web/index.html` draws what it returns on `<canvas>`; it
reimplements none of the model. Geometry crosses the boundary as packed
`Float32Array`s (seven floats per box), everything else as JSON.

The example covers the timeline, flame graph, stats and critical path. The
desktop app's dependency graph, execution log, console replay and build event
views are not ported.

## Build and run

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-pack
web/build.sh                        # writes web/pkg/
python3 -m http.server -d web 8080  # open http://localhost:8080
node --test web/test/               # binding tests over a synthetic profile
```

Use rustup's toolchain: a distribution `cargo` (e.g. Homebrew's) ahead of it
on `PATH` has no `wasm32-unknown-unknown` standard library.

`wasm/` is its own Cargo workspace with its own `Cargo.lock`, so the root
lockfile that Bazel's crate pins come from is unchanged. Its CI and GitHub
Pages deployment are in `.github/workflows/wasm.yaml`.

## Changes to upstream code

`wasm32-unknown-unknown` cannot spawn threads, and bviz's parser and layout fan
work out over several. These changes keep native behaviour as it was and run
single-worker passes on the calling thread:

- `util/parallel/src/lib.rs`: on wasm, the worker pools are the calling thread
  alone (`num_threads(1).use_current_thread()`, as rayon's own global pool falls
  back to), so passes that enter them run inline. `worker_count()` is already 1
  there.
- `trace/profile/src/parse/pipeline.rs`: a parse with one worker runs its
  chunks on the calling thread instead of spawning one.
- `trace/profile/src/parse/build.rs`: lane merging with one worker runs on the
  calling thread, and the detached teardown drops inline on wasm.
- `trace/layout/src/flame.rs`: the 48-byte row size assertion applies to 64-bit
  targets only, since `wasm32` rows are narrower.
- `critical_path/Cargo.toml`: declares its `bviz_invocation` dependency, which
  its Bazel `BUILD` file already had and Cargo builds were missing.

The streamed gzip pipeline needs concurrent decoder and parser threads.
`pipeline_params` already declines it on a single core, so wasm takes the
whole-buffer path.

## Limits

- Parsing runs on the page's main thread, so a very large profile freezes the
  tab while it loads. Moving `Trace` into a Web Worker would avoid that.
- wasm32's memory tops out at 4 GiB, and the whole decoded profile is held in
  it, so the largest profiles may not load.
- One thread: there is no parallel speedup. Enabling wasm threads would need
  `SharedArrayBuffer`, cross-origin isolation and a rayon thread pool such as
  `wasm-bindgen-rayon`.

The fork is licensed Apache-2.0 like upstream (see `LICENSE` and `NOTICE`).
