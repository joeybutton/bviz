"""Keep bazel_env's compiled dev tools off remote execution.

bazel_env pins its tools' execution to the host platform (its `_flip_output_dir`
transition), which under remote execution carries no container image, so the
rustc-built tools would run on BuildBuddy's default image and fail to load. The
failing rustc actions are shared external targets that per-target tags and
strategy filters can't single out; transitioning `--modify_execution_info` onto
a tool adds `no-remote-exec` to its rustc actions so they build locally.

The transition wraps individual tools rather than the bazel_env target itself:
bazel_env keeps its output directory at exactly `bazel_env-opt` (a tested
invariant its generated `.envrc` paths depend on), and any extra transitioned
setting would suffix that with a nonreproducible `ST-` hash.
"""

load("@with_cfg.bzl", "with_cfg")

# An alias whose target (and its transitive deps) build with no-remote-exec.
local_exec_tool, _local_exec_tool_internal = with_cfg(native.alias, executable = True).extend(
    "modify_execution_info",
    ["Rustc=+no-remote-exec"],
).build()
