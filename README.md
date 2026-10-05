# bviz

A graphical Bazel build visualization and inspection tool.

![bviz with a trace profile, a dependency graph and a console replay open side by side](docs/screenshots/bviz.png)

Seeing what a Bazel build actually did is hard. bviz reads the outputs Bazel
generates, such as query results, the trace profile and the build event log,
and lets you explore them visually.

bviz is early in development. Expect rough edges, and expect the interface and
command line to change.

## Features

### Build graph

The target and action graph from `query`, `cquery` and `aquery` output, merged
into one graph to expand, search and filter.

### Trace profile

The timeline of a build's trace profile: what ran when, and flame graphs of
where the time went, weighed by summed time or by share of the wall time.

### Build event protocol

The console output of a build, replayed from its build event log.

### Execution logs

Where a build's spawns spent their time, phase by phase, from its execution
log or execution graph log.

### Playback

A build played back on its graph: each target and action pending, being
analysed, waiting, running or done at every moment of the build.

bviz can open several files at once.

## Install

Download the latest build of `main` for arm64 or x86_64 Linux or macOS from the
[nightly release](../../releases/tag/nightly).

| | Linux | macOS |
| --- | --- | --- |
| Architecture | arm64, x86_64 | arm64, x86_64 |
| Operating system | glibc 2.28 or newer | macOS 14 or newer |
| Display | a Wayland session | |
| Graphics | a Vulkan driver | Metal |

On an X11 session, run bviz inside a nested Wayland compositor such as
[cage](https://github.com/cage-kiosk/cage): `cage -- bviz --profile build.profile.gz`.

The arm64 Linux and the macOS binaries are cross-compiled on x86_64 Linux and are
not tested in CI. The macOS binaries are not signed, and macOS blocks an unsigned
download until you clear its quarantine flag:
`xattr -d com.apple.quarantine bviz`.

To build from source, see [CONTRIBUTING.md](CONTRIBUTING.md).

## Quick start

In a Bazel workspace, profile a build and open the profile:

```sh
bazel build --profile=/tmp/build.profile.gz //...
bviz --profile /tmp/build.profile.gz
```

## Usage

bviz opens every file named on its command line, and needs at least one.

### Dependency graph

```sh
bazel query --output=streamed_proto --consistent_labels //... > /tmp/query.pb
bazel cquery --output=streamed_proto --consistent_labels --transitions=lite //... > /tmp/cquery.pb
bazel aquery --output=streamed_proto --consistent_labels //... > /tmp/aquery.pb
bviz --query /tmp/query.pb --cquery /tmp/cquery.pb --aquery /tmp/aquery.pb
```

Any one of the three is enough. bviz merges them into a single graph, so each
flag takes one file. Under `--transitions=lite` or `full`, cquery resolves each
dependency's configuration, so an edge reaches the exact configured target it
feeds; without it the edge lands on the dependency's target.

`--consistent_labels` spells external repositories' labels canonically, as a
trace profile does, so the profile's actions are associated with their targets
in the graph.

### Trace profile

```sh
bazel build --profile=/tmp/build.profile.gz //...
bviz --profile /tmp/build.profile.gz
```

For a more complete profile, add `--noslim_profile --record_full_profiler_data
--experimental_profile_include_target_label
--experimental_profile_include_target_configuration
--experimental_profile_include_primary_output`. The profile then keeps short
events and names each action's target and primary output, at the cost of a
larger file.

### Console replay

```sh
bazel build --show_timestamps --build_event_binary_file=/tmp/build.bep //...
bviz --bep /tmp/build.bep
```

Bazel does not timestamp its progress output, so without `--show_timestamps` the
replay interpolates its timing between the events that do carry a clock.

### Execution logs

```sh
bazel build --execution_log_compact_file=/tmp/exec_log.binpb.zst \
  --experimental_enable_execution_graph_log \
  --experimental_execution_graph_log_path=/tmp/graph_log.binpb.zst //...
bviz --exec_log /tmp/exec_log.binpb.zst --exec_graph_log /tmp/graph_log.binpb.zst
```

Either log is enough; each opens as a breakdown of spawn time by phase.

`--profile`, `--bep`, `--exec_log` and `--exec_graph_log` can be repeated, and
combined with the graph flags, to open several files grouped by Bazel
invocation.

### Playback

The files one invocation wrote open as one document with a playback bar: play,
step or drag through the build, and pick `colour: playback` to see where each
box of the graph stood at that moment. Each file adds what it records, so
capture all of them from one build, and the graph's `cquery` and `aquery`
results from the same workspace:

| Bazel flags | bviz flag | Adds |
| --- | --- | --- |
| `--profile=FILE --noslim_profile --record_full_profiler_data --experimental_profile_include_target_label --experimental_profile_include_target_configuration --experimental_profile_include_primary_output` | `--profile` | Each action's phases to the microsecond; the analysis of each target whose rule is written in Starlark; the build's phases, as bands along the bar. |
| `--execution_log_compact_file=FILE` | `--exec_log` | Each spawn's runner, cache hit and exit code. A spawn run locally is placed exactly, the others between those by the order they finished in. |
| `--experimental_enable_execution_graph_log --experimental_execution_graph_log_path=FILE` | `--exec_graph_log` | Each action's timing to the millisecond, including the actions that ran no spawn; with a profile of the same build written with the flags above, which of those the build found up to date. |
| `--build_event_binary_file=FILE --build_event_publish_all_actions --show_timestamps` | `--bep` | When each top-level target was analysed, and was built or failed; how each action ended; each test attempt. |

Without `--record_full_profiler_data` the profile keeps only analysis lasting
50 ms or more.

The execution graph log names no action's output. A spawn of it that the
execution log of the same build holds by digest lands on its action; every
other node is found among the aquery's actions by its target, mnemonic and
description, and lands on the target where that finds none or several, as for
an action its target registers in several configurations. An aquery from a
Bazel release before 10.0.0-pre.20260723.1 records no progress message, so with
one an action that sets a progress message is found only where it is its
target's only action of that mnemonic.

## Getting help

Report bugs and ask questions in the [issue tracker](../../issues).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## Acknowledgements

bviz draws with [gpui](https://github.com/zed-industries/zed), the UI framework
of the Zed editor, and lays out graphs with
[elk-rs](https://github.com/openedges/elk-rs), a Rust port of the Eclipse
Layout Kernel.

## License

bviz is licensed under the Apache License 2.0; see [LICENSE](LICENSE). The
third-party works it bundles or derives from, and their terms, are listed in
[NOTICE](NOTICE).
