# bazel_viz

Interactive visualization and inspection tool for Bazel builds.

## Working style

- Incremental: small steps, one concern at a time.
- Inspect the working copy before the first edit of any task, and never edit
  into a change you did not open for this task. Under jj the working copy is
  itself a commit, so any edit lands in whatever change the working copy holds:
  editing while it already carries an unrelated in-progress change silently
  fuses your new work into that change, and the two must then be split back
  apart — an error-prone rescue that is entirely avoided by looking first. So
  before touching a file, run `jj st`; if it reports any change (the working
  copy is non-empty), or the working copy's description names work other than
  the task at hand, run `jj new` to start a fresh change on top before editing.
  Only edit directly into the working copy when it is empty, or when its
  description is the very change you are continuing. This holds even for a
  one-line fix: the size of the edit does not change where it lands.
- Commit frequently, one concern per commit. Group by shared concern — neither
  one giant commit nor a commit per file. Imperative subject; body explains why.
  A single request may bundle unrelated concerns; split them into separate
  commits anyway — being asked for them together does not make them one concern.
  What makes changes one concern is addressing the same problem, NOT sharing a
  file, a function, a feature area, an editing session, or a single round of
  review feedback — distinct problems are distinct commits even when all touch
  one function or all came from one list of comments. Two bug fixes, or a fix
  plus a behavior change plus a tweak, are that many commits. Two tells that you
  are about to under-split: the subject needs an "and" to join unrelated changes,
  or the body is a bulleted list of independent changes — in either case, split
  it, one commit per bullet. When unsure whether two changes are one concern,
  default to splitting; over-splitting is cheap to read, under-splitting is not.
- Commit only verified work: build and run/test it first. For toolchain changes,
  confirm with `--toolchain_resolution_debug`. Report outcomes honestly; flag
  side effects.
- A change to visual output is verified by viewing the rendered result on real
  input, not just by checking that the output is structurally well-formed —
  visual faults (overlaps, mis-routed or detached connectors, clipping) do not
  show up in a structural check.
- Before hand-rolling a widget or other capability, check whether the framework
  or its component library already provides one, and reuse it unless it brings
  unacceptable dependencies; framework-specific facts (what the framework does and
  doesn't offer) belong in its skill, not rediscovered each time. An interactive
  control must work end to end — a scroll bar drags and its track clicks, not just
  a drawn indicator.
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
  branches move on. Just as corrosive is grounding a claim in a specific external
  input the reader has no access to, or in one run's raw figures — "the ~1.4GB
  profile", "the 8M-event trace", "takes 4s here" name a data artifact and a
  machine that live outside the repo and mean nothing to a later reader. Describe
  the input by its general character ("a large profile") and prefer a relative or
  qualitative result ("about half the time", "roughly 2x faster", "a noticeable
  share of load") or a property that holds regardless of the sample; if a concrete
  number genuinely earns its place, state the reproducible conditions behind it
  rather than pinning it to a one-off measurement. A comment must describe the
  code as it stands — a value's role, an invariant it upholds, the non-obvious
  reason it is what it is — never narrate the edit that produced it or contrast
  against an alternative the code no longer holds. Phrasings that only parse for a
  reader who knows the prior version are the tell: "leave/keep X set", "don't
  clear Y", "now uses", "instead of Z", "X is also Y so we don't bother". They
  read as a note on the change rather than the code; state the standing fact
  directly — say what X is and why it must be that, not that you refrained from
  changing it. Name any
  external reference in full — its title or identifier (a paper's name and arXiv
  id, a ticket number) — never a bare "the paper"/"the reference"/"the spec"
  that assumes shared context. References must also be portable: never hard-code a
  machine-local absolute path (`/home/<user>/...`, a personal checkout location) in
  a checked-in file — it resolves only on the machine that wrote it. Point at a
  repo-relative path, or for external source pin a stable identifier (a release tag
  or permalink URL, not a moving branch); a local checkout may be named as a
  preferred-if-present convenience, never as the sole locator. Likewise,
  checked-in test fixtures and generated data must not embed machine- or
  user-identifying values (usernames, home paths, hostnames); generate them in a
  neutral location — e.g. a `/tmp` workspace with `HOME`/`USER` overridden — and
  normalize any residue with a length-preserving substitution before committing.
- The same generality applies to the running code, not just the prose around it:
  behavior and semantics must not be wired to the specifics of the machine that
  happened to write them. Never hard-code a value that is really a property of one
  host — a core or thread count, a page or cache-line size, a memory ceiling, a
  screen resolution or DPI, a device path, an environment particular to your box.
  A constant chosen because it matched the current machine (say a parallelism cap
  set to this host's core count) silently mis-sizes every other machine — too low
  wastes capacity, too high oversubscribes. Query the property at runtime through
  the portable, cross-OS API for it (physical cores via `num_cpus::get_physical()`
  clamped to the process's granted parallelism, not a literal; a path via the
  platform's dirs/temp API, not `/home/...`) and derive the value from that. When
  a bound is genuinely intrinsic to the workload rather than the host — a
  memory-bandwidth-bound stage saturating at the physical-core count regardless of
  SMT — express it as that structural property, not the number it happens to equal
  on one machine.
- Size thread pools and per-core structures (shard fan-out, per-worker buffers)
  by PHYSICAL cores via `worker_count()` (`num_cpus::get_physical()` clamped to
  `available_parallelism()`), not logical/SMT threads: the heavy passes here
  (parse, aggregation) are memory-bandwidth bound and saturate at the physical
  cores that own the memory channels, so SMT oversubscription burns CPU for no
  wall-time gain. Reach for the logical thread count only with a measured reason.
- Pre-allocate a collection's capacity whenever its final size is known or
  cheaply estimable, so a buffer filled by `push` in a loop does not grow from
  empty by repeated reallocation. Each `grow_one` reallocates and moves the whole
  buffer, and the churn hides in `memmove` and allocator time that a profiler only
  later attributes back — a recurring, avoidable cost in the parse/merge path.
  Size from whichever is available: an exact count (one element per known input,
  e.g. one `FoldedRun` per builder, one lane per distinct thread), a small
  structural bound (per lane, thread, or counter), or a byte-density estimate for
  a collection whose length tracks the input (mirror `BYTES_PER_DISTINCT` /
  `BYTES_PER_SLICE` — measure the density on a representative profile and round it
  down so the reservation over-reserves slightly). Reserve at the point that knows
  the count: when a type's `new` cannot, add a `with_capacity` constructor and
  size it at the call site (as `Merge::with_capacity` does from the builder count)
  rather than leaving `Vec::new()` to double. Over-reserving costs only transient
  capacity freed when the buffer is dropped or sealed; under-reserving costs
  bounded regrowth — for a hot buffer prefer the slight over-reserve. `reserve`
  the same way for a buffer reused across iterations, and note that `collect` from
  an `ExactSizeIterator` already pre-sizes; the trap is a hand-grown
  `Vec::new()`/`vec![]`/`String::new()`.
- Comments are terse and only earn their place by adding what the code can't
  say — a sentence, not a paragraph. Don't restate the code (a comment that
  names what the next line already names is noise — including one that restates
  an identifier the declaration already gives, like a repo or target name, or
  re-derives in one file what another already encodes, like spelling out in a
  BUILD overlay the import layout the MODULE sparse-checkout already fixes), don't say the same thing
  twice (a rationale belongs in one place — usually a definition — not repeated
  where the symbol is used), don't re-explain what a rule's or tool's own
  documentation already covers — and likewise don't narrate what a framework or
  library call does: its behavior is the API's contract, plain from the call, so
  a comment that a UI helper "re-renders each frame", that a lock "blocks until
  acquired", or that a builder method "sets the title" restates the call and is
  noise; comment only the non-obvious reason the call is used here, if any. Don't
  write comments that drift as nearby
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
- Never document a struct by enumerating what it contains — neither a doc
  comment on the struct that lists its own fields, nor a comment on a field
  whose type names a struct that lists that struct's fields. The declaration
  right below or beside the comment already gives the inventory, and the copy is
  unverified: it goes stale the moment a field is added, renamed, or moved, and
  a reader who trusts it is then misled about the very thing they could have
  read directly. Document instead what the declaration cannot state — the type's
  purpose, and on a handle field what this holder reads or writes through it and
  which way the coupling runs. Where the name and type already say it, write no
  comment at all.
- Avoid unnamed magic numbers: bind a non-obvious literal to a named constant
  (or a `let` with a descriptive name) at its definition, so its meaning is
  stated once where the value lives. A self-evident literal in context (an index
  `0`, a `* 2`, a percentage `0.5` whose role is clear from the expression) needs
  no name; a bare `86_400`, `4096`, or `0.85` does. This pairs with the comment
  rule above — name the value rather than annotating the literal from afar.
- Only rewrite history that has not been pushed to a remote, and avoid
  rewriting the main branch even then. When you do reshape history (reword,
  squash, drop, reorder), use rebase or cherry-pick and resolve conflicts through
  git rather than rebuilding commits by hand; squash a commit that only undoes or
  replaces an earlier one instead of leaving the churn; and do it on a scratch
  branch, confirming the rewritten tree is unchanged (or changed only as
  intended, via `git diff`) before moving the real branch.
- `jj restore <path>` resets that path to the PARENT commit (`@-`), not to some
  pre-edit snapshot — and because the working copy IS a commit, that discards the
  CURRENT commit's own changes to the path, not just an uncommitted tweak. So
  never reach for it to strip a small unwanted edit (a debug tweak, a temporary
  flag flip) from a file the working commit is actively changing: it silently
  wipes that commit's work on the whole file. Revert the unwanted hunk with a
  targeted edit instead; if you already ran the restore, `jj undo` recovers it.
  See the jujutsu skill.

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
  hand-vendor unpinned sources. The invariant is hermeticity: the library must be
  fetched at a pinned version and built by Bazel from source, never taken from the
  host. Any Bazel-managed fetch satisfies this — a Bazel Central Registry
  (https://registry.bazel.build) module added as a `bazel_dep` when one exists, or
  an `http_archive` (or similar) pinned by sha256 with a BUILD overlay otherwise.
  Prefer a BCR module when available for the maintained build files, but an
  `http_archive` is an equally hermetic path, not a fallback to avoid.
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
