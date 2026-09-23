# bviz

Seeing what a Bazel build actually did is hard. While it runs you get scrolling
progress; afterwards, dumps meant for scripts.

bviz gives you a way in: walk the dependency graph, zoom into the action
timeline, scrub back through the console output.

## Viewers

`//bviz` is the application: one window stacking the documents named on its
command line, each in a region of its own.

It is a gpui desktop application, and needs Wayland and a GPU.

## Nightly builds

Every night a binary is built for x86_64 Linux, arm64 macOS and x86_64
macOS, and attached to the run as an artifact. Take one from the
[Nightly workflow](../../actions/workflows/nightly.yaml), whose "Run
workflow" button also builds the set on demand. Each artifact is a
`.tar.gz` holding the `bviz` executable, bviz's `LICENSE` and `NOTICE`, and
`THIRD_PARTY_NOTICES` with the licences of the third-party work built into
that executable.

## Building from source

See [CONTRIBUTING.md](CONTRIBUTING.md) for the build setup, the per-viewer
binaries, and the checks CI runs.

## Running a viewer

`bazel run` starts the binary in its runfiles directory, so give input paths in
full. To obtain good interactive performance, run with `-c opt`.

```sh
bazel aquery --output=streamed_proto //... > /tmp/aquery.pb
bazel cquery --output=streamed_proto --transitions=lite //... > /tmp/cquery.pb
bazel query --output=streamed_proto //... > /tmp/query.pb
bazel run -c opt //ui/graph:graph -- --aquery /tmp/aquery.pb --cquery /tmp/cquery.pb --query /tmp/query.pb
```

At least one query is required; several are merged into one joint graph. Under
`--transitions=lite` or `full`, cquery resolves each dependency's configuration,
so an edge reaches the exact configured target it feeds; without it the edge
lands on the dependency's target container.

```sh
bazel build --config=bazel_profile --profile=/tmp/build.profile.gz //...
bazel run -c opt //ui/trace:trace -- --profile /tmp/build.profile.gz
```

`--config=bazel_profile` widens what the profile records.

```sh
bazel build --show_timestamps --build_event_binary_file=/tmp/build.bep //...
bazel run -c opt //ui/console:console -- /tmp/build.bep
```

Bazel does not timestamp its progress output, so without `--show_timestamps` the
replay interpolates its timing between the events that do carry a clock.

## Running bviz

`//bviz` takes the inputs of all three viewers at once and opens a panel
per document.

```sh
bazel run -c opt //bviz -- \
    --aquery /tmp/aquery.pb --cquery /tmp/cquery.pb --query /tmp/query.pb \
    --profile /tmp/build.profile.gz --bep /tmp/build.bep
```

At least one document is required. The query files merge into a single joint
graph, so each of `--aquery`, `--cquery` and `--query` names at most one file per
invocation; `--profile` and `--bep` each open a panel per occurrence.

## License

bviz is licensed under the Apache License 2.0; see [LICENSE](LICENSE). The
third-party works it bundles or derives from, and their terms, are listed in
[NOTICE](NOTICE).

To contribute, see [CONTRIBUTING.md](CONTRIBUTING.md).
