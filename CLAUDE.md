# bviz

Interactive visualization and inspection tool for Bazel builds.

@CONTRIBUTING.md

## Workflow

- In a jj checkout, if a jujutsu (jj) skill is available, load it before the
  first version-control command of a task.
- Inspect the working copy before the first edit of any task, and never edit
  into a change you did not open for this task. In a jj checkout (a `.jj`
  directory at the root) the working copy is itself a commit, so any edit lands
  in whatever change it holds: editing while it carries an unrelated change
  silently fuses the two, and splitting them back apart is error-prone. So
  before touching a file, run `jj st`; if the working copy is non-empty, or its
  description names work other than the task at hand, run `jj new` first. Only
  edit directly into it when it is empty or its description is the very change
  you are continuing — even for a one-line fix. In a git checkout, run
  `git status` and keep unrelated uncommitted changes out of your commits.
- In a jj checkout, `jj restore <path>` resets that path to the PARENT commit
  (`@-`), not to some pre-edit snapshot — and because the working copy IS a
  commit, that discards the CURRENT commit's own changes to the path, not just an
  uncommitted tweak. So never reach for it to strip a small unwanted edit (a
  debug tweak, a temporary flag flip) from a file the working commit is actively
  changing: it silently wipes that commit's work on the whole file. Revert the
  unwanted hunk with a targeted edit instead; if you already ran the restore,
  `jj undo` recovers it.
- Commit frequently, one concern per commit. Group by shared concern — neither
  one giant commit nor a commit per file. A single request may bundle unrelated
  concerns; split them into separate commits anyway — being asked for them
  together does not make them one concern.
  What makes changes one concern is addressing the same problem, NOT sharing a
  file, a function, a feature area, an editing session, or a single round of
  review feedback — distinct problems are distinct commits even when all touch
  one function or all came from one list of comments. Two bug fixes, or a fix
  plus a behavior change plus a tweak, are that many commits. Two tells that you
  are about to under-split: the subject needs an "and" to join unrelated changes,
  or the body is a bulleted list of independent changes — in either case, split
  it, one commit per bullet. When unsure whether two changes are one concern,
  default to splitting; over-splitting is cheap to read, under-splitting is not.
- Only rewrite history that has not been pushed to a remote, and avoid
  rewriting the main branch even then. Reshape history (reword, squash, drop,
  reorder) with the VCS's own operations — `git rebase`/`cherry-pick`, or
  `jj rebase`/`squash`/`describe` — and resolve conflicts through them rather
  than rebuilding commits by hand. Squash a commit that only undoes or replaces
  an earlier one instead of leaving the churn. Confirm the rewritten tree is
  unchanged, or changed only as intended: in git, rewrite on a scratch branch and
  `git diff` it against the original before moving the real branch; in jj,
  `jj diff --from <old> --to <new>` against the pre-rewrite commit (`jj evolog`
  lists it), and `jj undo` a rewrite that went wrong.
- Commit only verified work: run the checks under "Before you open a pull
  request", on the targets you are changing while iterating and in full before
  committing. For toolchain changes, confirm with `--toolchain_resolution_debug`.
  Report outcomes honestly; flag side effects.
- Verify a change to visual output by viewing the rendered result on real
  input; a structural check does not catch visual faults.

## Code

- Before hand-rolling a widget or other capability, check whether the framework
  or its component library already provides one, and reuse it unless it brings
  unacceptable dependencies; framework-specific facts (what the framework does and
  doesn't offer) belong in its skill, not rediscovered each time. An interactive
  control must work end to end — a scroll bar drags and its track clicks, not just
  a drawn indicator.
- Behavior must not be wired to the machine that happened to write it. Never
  hard-code a value that is really a property of one host — a core or thread
  count, a page or cache-line size, a memory ceiling, a screen resolution or DPI,
  a device path, an environment particular to your box: a constant that matched
  the current machine silently mis-sizes every other one. Query the property at
  runtime through the portable, cross-OS API for it (a path via the platform's
  dirs/temp API, not `/home/...`) and derive the value from that. When a bound is
  genuinely intrinsic to the workload rather than the host, express it as that
  structural property, not the number it happens to equal on one machine.
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
  e.g. one lane per distinct thread), a small
  structural bound (per lane, thread, or counter), or a byte-density estimate for
  a collection whose length tracks the input (mirror the `BYTES_PER_*`
  constants in `trace/profile` — measure the density on a representative profile and round it
  down so the reservation over-reserves slightly). Reserve at the point that knows
  the count: when a type's `new` cannot, add a `with_capacity` constructor and
  size it at the call site (grep `fn with_capacity` for the pattern)
  rather than leaving `Vec::new()` to double. Over-reserving costs only transient
  capacity freed when the buffer is dropped or sealed; under-reserving costs
  bounded regrowth — for a hot buffer prefer the slight over-reserve. `reserve`
  the same way for a buffer reused across iterations, and note that `collect` from
  an `ExactSizeIterator` already pre-sizes; the trap is a hand-grown
  `Vec::new()`/`vec![]`/`String::new()`.
- Make invalid states unrepresentable rather than documenting or guarding them.
  When a combination of fields cannot legitimately occur — an `Option` that is
  always `Some` once some other field says so, a flag that is only meaningful
  while a second one is set, an index valid only for one particular buffer —
  reshape the types so the combination cannot be written: move the fields into
  the enum variant that guarantees them, pair them in one struct, or make the
  handle carry what it indexes. An invariant kept only by the order of a few
  assignments is enforced by nothing; it survives until the next edit, and every
  reader downstream must either re-derive it or take it on trust.
  The tell is a guard for a case the author believes cannot happen — an
  `unwrap`, an `unreachable!`, an early return unwrapping something that "will
  always" be set, or a comment asserting a combination never occurs. A reader
  cannot tell such a guard from a live one, so each is both dead weight and a
  silent place for a real case to hide later. Prefer a type that removes the
  guard to a comment that explains it.
  Where the invariant is a property of a single value rather than a combination
  of fields — a range, a non-empty sequence, a normalized or validated form, a
  clamped dimension — carry it in an opaque newtype: a private inner field, a
  constructor that performs the check and reports failure (`Result`/`Option`) or
  coerces into range, and an API whose every operation either preserves the
  property or is itself fallible. The check then happens once, where the value
  enters, and every function downstream receives the invariant as a given
  instead of re-checking it or trusting a comment. Expose no route back — no
  public field, no `From<Inner>` — that would let a caller mint an unchecked
  value, or the guarantee is only advisory.
  This is not a licence to invent elaborate type machinery: the reshaping should
  make the code shorter and the guards fewer. If encoding an invariant in the
  types costs more than it removes — deep generics, a state machine of marker
  types, a conversion at every use — state the invariant once at the definition
  and keep the guard, but reach for that after the reshaping and the newtype,
  not before them.
- Avoid unnamed magic numbers: bind a non-obvious literal to a named constant
  (or a `let` with a descriptive name) at its definition, so its meaning is
  stated once where the value lives. A self-evident literal in context (an index
  `0`, a `* 2`, a percentage `0.5` whose role is clear from the expression) needs
  no name; a bare `86_400`, `4096`, or `0.85` does.

## Comments and commit messages

Code, comments, and commit messages must be self-contained: a reader who has no
access to the chat or task that produced them must fully understand them.

- Never reference the development process — no step/phase numbers, no "as
  discussed", no "next we will", no roadmap of future work ("X comes later"), no
  TODOs that only make sense mid-thread, and no justifying a change by alluding
  to other commits or the work in progress ("the X commit", "when we restructured
  Y"); state the change and its rationale on their own terms, and if you must
  cite a prior commit, identify it by hash or subject.
- Describe the change in absolute terms, not against transient state — not what
  another branch has, nor what the tree "already"/"now"/"yet" contains or was
  "missing" before; such claims hold only at the instant of writing and rot as
  the repo and other branches move on.
- Don't ground a claim in a specific external input the reader has no access to,
  or in one run's raw figures — "the ~1.4GB profile", "the 8M-event trace",
  "takes 4s here" name a data artifact and a machine that live outside the repo.
  Describe the input by its general character ("a large profile") and prefer a
  relative or qualitative result ("about half the time", "roughly 2x faster") or
  a property that holds regardless of the sample; if a concrete number genuinely
  earns its place, state the reproducible conditions behind it.
- Name any external reference in full — its title or identifier (a paper's name
  and arXiv id, a ticket number) — never a bare "the paper"/"the spec".
- Keep references portable: never hard-code a machine-local absolute path
  (`/home/<user>/...`, a personal checkout location) in a checked-in file. Point
  at a repo-relative path, or for external source pin a stable identifier (a
  release tag or permalink URL, not a moving branch); a local checkout may be
  named as a preferred-if-present convenience, never as the sole locator.
- Checked-in test fixtures and generated data must not embed machine- or
  user-identifying values (usernames, home paths, hostnames); generate them in a
  neutral location — e.g. a `/tmp` workspace with `HOME`/`USER` overridden — and
  normalize any residue with a length-preserving substitution before committing.

Comments are terse and only earn their place by adding what the code can't say —
a sentence, not a paragraph. State each fact once, at its definition.

- A comment describes the code as it stands — a value's role, an invariant it
  upholds, the non-obvious reason it is what it is — never the edit that
  produced it or an alternative the code no longer holds. Phrasings that only
  parse for a reader who knows the prior version are the tell: "leave/keep X
  set", "don't clear Y", "now uses", "instead of Z", "X is also Y so we don't
  bother". State the standing fact directly — say what X is and why it must be
  that, not that you refrained from changing it.
- Don't restate the code: a comment that names what the next line already
  names is noise, including one that restates an identifier the declaration
  already gives, or re-derives in one file what another already encodes. Don't
  re-explain what a rule's or tool's own documentation covers, and don't narrate
  what a framework or library call does: its behavior is the API's contract, so
  a comment that a UI helper "re-renders each frame", that a lock "blocks until
  acquired", or that a builder method "sets the title" is noise; comment only
  the non-obvious reason the call is used here, if any.
- Don't write comments that drift as nearby code changes: don't describe which
  of several things a wildcard or shared target covers, and don't restate the
  concrete value of something defined elsewhere — a color, dimension, or
  default; name it by its role or source of truth ("the file-leaf accent", not
  "(green)"). Annotating a literal at its own definition is fine.
- Don't narrate the design: a block comment restating a module's model, data
  flow, or algorithm only duplicates the code and is the first thing a later
  refactor deletes. Citing an external source (paper, spec, issue) is fine.
- BUILD, `.bzl`, and `MODULE.bazel` files follow the same rules: a declarative
  rule and its attributes already state what they do, so a comment that names a
  rule's effect (`# expose the binary` over a `gen_binaries` attribute) or
  restates an attribute is noise. When one rationale spans several files (a
  toolchain and the target that consumes it), state it once at the definition
  or in the commit message, and avoid cross-file pointers
  (`# registered in MODULE.bazel as X`) that drift when either side moves.
- Never document a struct by enumerating what it contains — neither a doc
  comment on the struct that lists its own fields, nor a comment on a field
  whose type names a struct that lists that struct's fields. The declaration
  already gives the inventory, and the copy goes stale the moment a field is
  added, renamed, or moved. Document instead what the declaration cannot state —
  the type's purpose, and on a handle field what this holder reads or writes
  through it and which way the coupling runs. Where the name and type already
  say it, write no comment at all.
- A doc comment binds to exactly one declaration, and everything that surfaces
  it — an editor hover, rustdoc, a reader scanning the declaration — shows it
  only there, so a fact stated on one field is invisible from its siblings.
  Never let one field's doc carry a property that in truth governs several:
  parallel pools indexed alike, a partition they all follow, an invariant tying
  two fields together. Neither copy the sentence onto each sibling nor leave it
  in several plausible homes: put it on the one declaration every reader of
  every governed field must pass through — the handle, span, or accessor they
  are all reached by where one exists, and only failing that the type's own doc,
  where it states what holds between the fields, never what they are. Leave
  each field a doc that stands alone about itself.
- A comment at a construction or call site must not repeat what the constructed
  or called item's own doc says. It earns its place only by adding what is true
  of *this* site and nowhere else — an ordering constraint between two calls, why
  this caller passes what it passes, a consequence local to this context.

## Hermeticity

Everything is pinned and Bazel-managed. No system toolchains, no system tools.

- Rust toolchain via rules_rs; CC via the hermetic llvm toolchain.
- Dev tools (cargo, rustc, rustfmt, rust-analyzer) via `//tools:bazel_env` +
  direnv; other binaries via multitool, sha256-pinned per platform.
- Pin versions in `MODULE.bazel`; commit all lockfiles (`MODULE.bazel.lock`,
  `Cargo.lock`, `multitool.lock.json`). Repin `Cargo.lock` with
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

## Third-party licences

Declare them to Bazel with `package_metadata` + `license`; never copy a licence
text that Bazel already fetches.

- Fetched work: declare in its BUILD overlay, `text` pointing at the file in the
  archive (see `3rd-party/*.BUILD`).
- Work carried in-tree — source written from an upstream example, a constant
  taken verbatim, a patch shipping the lines it replaces — keeps its text in
  `LICENSES/`, declared there and attached to a `filegroup` of just those files
  so the claim does not cover the whole library.
- Patches are resolved when a crate repository is fetched, so their labels must
  be source files: declare per package, one package per upstream.
- `NOTICE` adds only what a declaration cannot state — which part of bviz a work
  reached, and how.

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
