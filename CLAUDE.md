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
  if you must cite a prior commit, identify it by hash or subject. Describe the
  change in absolute terms, not against transient state — not what another branch
  has, nor what the tree "already"/"now"/"yet" contains or was "missing" before;
  such claims hold only at the instant of writing and rot as the repo and other
  branches move on. Name any
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
  target covers, or restating the concrete value of something defined elsewhere
  — a color, dimension, or default — which goes stale the moment that definition
  changes; name it by its role or source of truth, e.g. "the file-leaf accent",
  not "(green)"). Annotating a literal at its own definition is fine — the
  comment and the value live together — but a reference to it from afar must not
  pin the value. This applies equally to BUILD and `.bzl`/`MODULE.bazel`
  files: a declarative rule and its attributes already state what they do, so a
  comment that names a rule's effect (`# expose the binary` over a
  `gen_binaries` attribute) or restates an attribute is noise; when one
  rationale spans several files (a toolchain and the target that consumes it),
  state it once at the definition or in the commit message rather than echoing
  it in every file that participates, and avoid cross-file pointers
  (`# registered in MODULE.bazel as X`) that drift when either side moves.
  Don't narrate the design: a block comment
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
  the Bazel-provided cargo, never system cargo. Repinning with cargo writes a
  `target/` tree (cargo's, not Bazel's); it is gitignored — never commit it.
- When a build needs a native/system C library (e.g. a Rust `*-sys` crate or a
  `#[link]`/pkg-config dependency on the host), do not link the host's copy or
  vendor sources by hand. First check the Bazel Central Registry
  (https://registry.bazel.build) for a module that builds it, and if one exists
  add it as a `bazel_dep` and link the Bazel-built library. This keeps the system
  dependency pinned and hermetic like everything else.
- JavaScript is built with aspect_rules_js (Node via rules_nodejs); build/test/
  run through Bazel targets, never a bare `node`/`npm`/`pnpm`. Manage npm deps
  with pnpm — repin via `bazel run -- @pnpm --dir $PWD install --lockfile-only`
  and commit `pnpm-lock.yaml`. Generate protobuf bindings with protobuf-es.

## Bazel conventions

- Prefer specific visibility (`//pkg:__pkg__`) over `//visibility:public`.
- Set `visibility` only on targets other packages depend on. A leaf target such as
  a `rust_binary` nothing depends on needs none — omit it (default private) rather
  than writing `//:__pkg__`. The exception is a target referenced by an `alias` in
  another package, which must be visible to that package.
- Give every crate a project-namespaced `crate_name` carrying the `bviz_` prefix,
  matching its Cargo package name (e.g. `bviz_graph_svg`, not a bare `graph_svg`).
- Prefer symbolic macros over legacy macros.
- Never compute runfiles paths by hand; use the language's `rlocation` helper and
  `$(rlocationpath LABEL)` in BUILD files.
- Never hardcode canonical repository names (`@@name+`, `@@name~`); they are
  Bazel-internal and depend on the Bazel version and module graph. Reference a
  repo by its apparent name, or where that is not resolvable (e.g. from a
  generated repo that lacks the mapping) alias the target in a package and
  reference the alias as a main-repo label.
- `sh_binary`/`sh_library`/`sh_test` come from `rules_shell`; the built-ins are
  deprecated.
