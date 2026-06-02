# bazel_viz

Interactive visualization and inspection tool for Bazel builds.

## Working style

- Incremental: small steps, one concern at a time.
- Commit frequently, one concern per commit. Group by shared concern — neither
  one giant commit nor a commit per file. Imperative subject; body explains why.
  A single request may bundle unrelated concerns; split them into separate
  commits anyway — being asked for them together does not make them one concern.
  If the subject needs an "and" to join two unrelated changes, that is two
  commits.
- Commit only verified work: build and run/test it first. For toolchain changes,
  confirm with `--toolchain_resolution_debug`. Report outcomes honestly; flag
  side effects.
- Code, comments, and commit messages must be self-contained: a reader who has
  no access to the chat or task that produced them must fully understand them.
  Never reference the development process — no step/phase numbers, no "as
  discussed", no "next we will", no roadmap of future work ("X comes later"),
  no TODOs that only make sense mid-thread.
- Comments are terse and only earn their place by adding what the code can't
  say — a sentence, not a paragraph. Don't restate the code (a comment that
  names what the next line already names is noise), don't re-explain what a
  rule's or tool's own documentation already covers, and don't write comments
  that drift as nearby code changes (e.g. describing which of several things a
  wildcard or shared target covers). Citing an external source (paper, spec,
  issue) is fine.

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
