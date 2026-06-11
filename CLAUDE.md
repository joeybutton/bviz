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
- A change to visual output is verified by viewing the rendered result on real
  input, not just by checking that the output is structurally well-formed —
  visual faults (overlaps, mis-routed or detached connectors, clipping) do not
  show up in a structural check.
- Code, comments, and commit messages must be self-contained: a reader who has
  no access to the chat or task that produced them must fully understand them.
  Never reference the development process — no step/phase numbers, no "as
  discussed", no "next we will", no roadmap of future work ("X comes later"),
  no TODOs that only make sense mid-thread, and no justifying a change by
  alluding to other commits or the work in progress ("the X commit", "when we
  restructured Y"); state the change and its rationale on their own terms, and
  if you must cite a prior commit, identify it by hash or subject. Name any
  external reference in full — its title or identifier (a paper's name and arXiv
  id, a ticket number) — never a bare "the paper"/"the reference"/"the spec"
  that assumes shared context. References must also be portable: never hard-code a
  machine-local absolute path (`/home/<user>/...`, a personal checkout location) in
  a checked-in file — it resolves only on the machine that wrote it. Point at a
  repo-relative path, or for external source pin a stable identifier (a release tag
  or permalink URL, not a moving branch); a local checkout may be named as a
  preferred-if-present convenience, never as the sole locator.
- Comments are terse and only earn their place by adding what the code can't
  say — a sentence, not a paragraph. Don't restate the code (a comment that
  names what the next line already names is noise), don't say the same thing
  twice (a rationale belongs in one place — usually a definition — not repeated
  where the symbol is used), don't re-explain what a rule's or tool's own
  documentation already covers, and don't write comments that drift as nearby
  code changes (e.g. describing which of several things a wildcard or shared
  target covers). Don't narrate the design: a block comment
  restating a module's model, data flow, or algorithm only duplicates the code
  and is the first thing a later refactor deletes — if a comment is not
  load-bearing, leave it out. Citing an external source (paper, spec, issue) is
  fine.
- Only rewrite history that has not been pushed to a remote, and avoid
  rewriting the main branch even then. When you do reshape history (reword,
  squash, drop, reorder), use rebase or cherry-pick and resolve conflicts through
  git rather than rebuilding commits by hand; squash a commit that only undoes or
  replaces an earlier one instead of leaving the churn; and do it on a scratch
  branch, confirming the rewritten tree is unchanged (or changed only as
  intended, via `git diff`) before moving the real branch.

## Hermeticity

Everything is pinned and Bazel-managed. No system toolchains, no system tools.

- Build, test, and run with Bazel. Never `cargo build`. Never `rustup`.
- Rust toolchain via rules_rs; CC via the hermetic llvm toolchain.
- Dev tools (cargo, rustc, rustfmt, rust-analyzer) via `//tools:bazel_env` +
  direnv; other binaries via multitool, sha256-pinned for all four supported dev
  platforms (linux/macos × x86_64/arm64).
- Pin versions in `MODULE.bazel`; commit all lockfiles (`MODULE.bazel.lock`,
  `Cargo.lock`, `pnpm-lock.yaml`, `multitool.lock.json`). Repin `Cargo.lock` with
  the Bazel-provided cargo, never system cargo.
- JavaScript is built with aspect_rules_js (Node via rules_nodejs); build/test/
  run through Bazel targets, never a bare `node`/`npm`/`pnpm`. Manage npm deps
  with pnpm — repin via `bazel run -- @pnpm --dir $PWD install --lockfile-only`
  and commit `pnpm-lock.yaml`. Generate protobuf bindings with protobuf-es.

## Bazel conventions

- Prefer specific visibility (`//pkg:__pkg__`) over `//visibility:public`.
- Prefer symbolic macros over legacy macros.
- Never compute runfiles paths by hand; use the language's `rlocation` helper and
  `$(rlocationpath LABEL)` in BUILD files.
- `sh_binary`/`sh_library`/`sh_test` come from `rules_shell`; the built-ins are
  deprecated.
