# bazel_viz

Interactive visualization and inspection tool for Bazel builds.

## Working style

- Incremental: small steps, one concern at a time.
- Commit frequently, one concern per commit. Group by shared concern — neither
  one giant commit nor a commit per file. Imperative subject; body explains why.
- Commit only verified work: build and run/test it first. For toolchain changes,
  confirm with `--toolchain_resolution_debug`. Report outcomes honestly; flag
  side effects.
- Comments are terse and only earn their place by adding what the code can't say.
  No restating code, no time-dependent or conversation-specific context.

## Hermeticity

Everything is pinned and Bazel-managed. No system toolchains, no system tools.

- Build, test, and run with Bazel. Never `cargo build`. Never `rustup`.
- Rust toolchain via rules_rs; CC via the hermetic llvm toolchain.
- Dev tools (cargo, rustc, rustfmt, rust-analyzer) via `//tools:bazel_env` +
  direnv; other binaries via multitool, sha256-pinned for all four supported dev
  platforms (linux/macos × x86_64/arm64).
- Pin versions in `MODULE.bazel`; commit all lockfiles (`MODULE.bazel.lock`,
  `Cargo.lock`, `multitool.lock.json`). Repin `Cargo.lock` with the
  Bazel-provided cargo, never system cargo.

## Bazel conventions

- Prefer specific visibility (`//pkg:__pkg__`) over `//visibility:public`.
- Prefer symbolic macros over legacy macros.
- Never compute runfiles paths by hand; use the language's `rlocation` helper and
  `$(rlocationpath LABEL)` in BUILD files.
- `sh_binary`/`sh_library`/`sh_test` come from `rules_shell`; the built-ins are
  deprecated.
