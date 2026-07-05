/// An interned-string handle: a byte slice into a text pool. `Copy` and
/// lifetime-free; resolve to `&str` with [`Profile::str`] or by indexing the
/// owning pool with [`Str::range`]. Equality is by `(off, len)` and so is only
/// meaningful within the single pool that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Str {
    off: u32,
    len: u32,
}

impl Str {
    pub fn new(off: u32, len: u32) -> Str {
        Str { off, len }
    }

    /// The byte range into the owning text pool: `&pool[s.range()]`.
    pub fn range(self) -> std::ops::Range<usize> {
        self.off as usize..(self.off + self.len) as usize
    }

    /// The same span relocated by `by` bytes, for rebasing a handle onto a text
    /// pool that has been concatenated after another.
    pub fn shifted(self, by: u32) -> Str {
        Str { off: self.off + by, len: self.len }
    }
}

/// A half-open `[off, off + len)` index range into a flat pool. Which pool it
/// indexes depends on the field that holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span32 {
    off: u32,
    len: u32,
}

impl Span32 {
    pub fn new(off: u32, len: u32) -> Span32 {
        Span32 { off, len }
    }

    /// The element range into the owning pool: `&pool[span.range()]`.
    pub fn range(self) -> std::ops::Range<usize> {
        self.off as usize..(self.off + self.len) as usize
    }

    /// The same span relocated by `by` elements, for rebasing onto a pool that
    /// has been concatenated after another.
    pub fn shifted(self, by: u32) -> Span32 {
        Span32 { off: self.off + by, len: self.len }
    }

    pub fn len(self) -> usize {
        self.len as usize
    }

    pub fn is_empty(self) -> bool {
        self.len == 0
    }
}

/// A parsed Bazel profile as flat, string-interned arenas. Every model type
/// holds only `Copy` handles ([`Str`], [`Span32`]) and scalars, so all owned
/// heap lives in the pools below — dropping a `Profile` frees a handful of large
/// allocations rather than millions of tiny ones, and the type is `Send + Sync`
/// with no lifetime parameters.
#[derive(Debug, PartialEq)]
pub struct Profile {
    pub(crate) text: String,
    pub(crate) slice_pool: Vec<Slice>,
    pub(crate) args_pool: Vec<(Str, Str)>,
    pub(crate) series_pool: Vec<CounterSeries>,
    pub(crate) point_pool: Vec<CounterPoint>,
    metadata: Metadata,
    threads: Vec<ThreadTrack>,
    counters: Vec<CounterLane>,
    phase_markers: Vec<PhaseMarker>,
    span: TimeSpan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Metadata {
    pub bazel_version: Option<Str>,
    pub build_id: Option<Str>,
    pub output_base: Option<Str>,
    pub date: Option<Str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeSpan {
    pub start_us: i64,
    pub end_us: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadTrack {
    pub tid: i64,
    pub name: Str,
    pub sort_index: i64,
    /// Range into the profile's slice pool; resolve with [`Profile::slices`].
    pub slices: Span32,
    pub max_depth: u32,
    pub lane_class: LaneClass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slice {
    pub name: Str,
    pub category: Str,
    pub start_us: i64,
    pub dur_us: i64,
    pub depth: u32,
    pub args: SliceArgs,
    pub work_class: WorkClass,
}

/// Coarse kind of work a slice represents, derived from its Bazel `cat`
/// description string ([`work_class`]). Lets a flame graph group/scope by the
/// nature of the work rather than the dozens of raw categories.
///
/// Two descriptions are judgment calls: "VFS md5" is a digest computation that
/// is both CPU- and I/O-bound — classified [`WorkClass::Io`] alongside the rest
/// of VFS; "action dependency checking" sits between compute and waiting on the
/// action cache — classified [`WorkClass::Wait`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkClass {
    Cpu,
    Remote,
    Io,
    Wait,
    Gc,
    Meta,
}

/// Maps a Bazel `ProfilerTask` description string (the Chrome-trace `cat`) to a
/// [`WorkClass`]. An unknown or future description falls through to
/// [`WorkClass::Meta`].
pub fn work_class(category: &str) -> WorkClass {
    match category {
        "local action execution"
        | "Local execution process wall time"
        | "local execution in worker"
        | "Local parse to prepare for remote execution"
        | "include scanner"
        | "discover inputs"
        | "package creation"
        | "bazel module processing"
        | "Starlark Parser"
        | "Starlark user function call"
        | "Starlark builtin function call"
        | "Starlark compiled user function call"
        | "Starlark repository function call"
        | "Starlark thread context"
        | "skyframe evaluator"
        | "skyfunction"
        | "action processing"
        | "complete action execution"
        | "update action information"
        | "prepare action rewind plan"
        | "Conflict checking"
        | "load WebAssembly module"
        | "execute WebAssembly function" => WorkClass::Cpu,

        "remote action execution"
        | "Remote execution process wall time"
        | "Remote execution upload time"
        | "Remote execution file fetching"
        | "remote output download"
        | "remote action cache check"
        | "Remote execution setup"
        | "remote network"
        | "RPC"
        | "Fetching repository" => WorkClass::Remote,

        "VFS stat"
        | "VFS readdir"
        | "VFS readlink"
        | "VFS md5"
        | "VFS xattr"
        | "VFS delete"
        | "VFS open"
        | "VFS read"
        | "VFS write"
        | "globbing"
        | "VMFS stat"
        | "VMFS readdir"
        | "VMFS read"
        | "Staging per-action file system"
        | "setting up inputs for worker"
        | "copying outputs from worker"
        | "filesystem traversal"
        | "Vendoring repository"
        | "logging spawn" => WorkClass::Io,

        "Remote execution queuing time"
        | "thread wait"
        | "action resource lock"
        | "Acquiring dynamic execution output lock"
        | "action dependency checking"
        | "borrowing a worker"
        | "waiting for response from worker"
        | "blocked on repo contents cache GC" => WorkClass::Wait,

        "gc notification" => WorkClass::Gc,

        _ => WorkClass::Meta,
    }
}

/// A slice's `args`. The three typed fields carry the Bazel keys callers query
/// directly; `others` is a range into the profile's args pool of the remaining
/// key/value pairs, resolved with [`Profile::slice_others`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SliceArgs {
    pub target: Option<Str>,
    pub mnemonic: Option<Str>,
    pub configuration: Option<Str>,
    pub others: Span32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CounterLane {
    pub name: Str,
    /// Range into the profile's series pool; resolve with [`Profile::series`].
    pub series: Span32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CounterSeries {
    pub name: Str,
    /// Range into the profile's point pool; resolve with [`Profile::points`].
    pub points: Span32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CounterPoint {
    pub ts_us: i64,
    pub value: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhaseMarker {
    pub name: Str,
    pub start_us: i64,
}

/// Coarse role a thread plays in the build, derived from its tid and readable
/// name ([`lane_class`]). Lets the UI scope to e.g. the critical path or to the
/// loading/analysis/execution role a lane carries — which under Skymeld is the
/// only way to separate analysis from execution, since the two phases merge into
/// one `ANALYZE_AND_EXECUTE` phase marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneClass {
    CriticalPath,
    Main,
    Gc,
    /// Repository-rule fetching (a loading-phase activity).
    RepoFetch,
    /// Configured-target/aspect analysis.
    Analysis,
    /// Action execution and its support threads.
    Execution,
    Other,
}

/// Skyframe evaluator pools whose names carry the execution role. Bazel's
/// `QuiescingExecutorsImpl` routes actions to the `-execution` and `-memoizing`
/// pools (via `NodeEntryVisitor`), so these run execution work.
const EXECUTION_SKYFRAME_INFIXES: [&str; 2] =
    ["skyframe-evaluator-execution", "skyframe-evaluator-memoizing"];

/// Thread-name prefixes for execution support (action processing and its
/// machinery), distinct from the skyframe evaluator pools.
const EXECUTION_PREFIXES: [&str; 9] = [
    "dynamic-execution",
    "Include scanner",
    "remote-execution",
    "remote-executor",
    "lease-extension",
    "combined-cache-",
    "merkle-tree-",
    "virtual-action-input-pipe-",
    "jdeps-reader-",
];

/// tid 0 is Bazel's synthetic critical-path thread; the rest are matched on the
/// readable thread name Bazel emits.
///
/// The skyframe-evaluator tests run longest-match-first: the `-execution`/
/// `-memoizing` pools run actions, the `-cpu-heavy` and plain pools run analysis
/// (configured targets/aspects, plus loading and light analysis), so the plain
/// `skyframe-evaluator` substring must be tested only after the execution
/// infixes are ruled out.
pub fn lane_class(tid: i64, name: &str) -> LaneClass {
    if tid == 0 || name == "Critical Path" {
        LaneClass::CriticalPath
    } else if name == "Main Thread" {
        LaneClass::Main
    } else if name == "Garbage Collector" {
        LaneClass::Gc
    } else if EXECUTION_SKYFRAME_INFIXES
        .iter()
        .any(|infix| name.contains(infix))
    {
        LaneClass::Execution
    } else if name.contains("skyframe-evaluator") {
        LaneClass::Analysis
    } else if name.starts_with("starlark-repository") || name.starts_with("downloads[") {
        LaneClass::RepoFetch
    } else if EXECUTION_PREFIXES.iter().any(|p| name.starts_with(p))
        || (name.starts_with("disk-cache-") && !name.starts_with("disk-cache-gc"))
        || name.starts_with("sandbox-")
    {
        LaneClass::Execution
    } else {
        LaneClass::Other
    }
}

impl Profile {
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    pub fn threads(&self) -> &[ThreadTrack] {
        &self.threads
    }

    pub fn counters(&self) -> &[CounterLane] {
        &self.counters
    }

    pub fn phase_markers(&self) -> &[PhaseMarker] {
        &self.phase_markers
    }

    pub fn span(&self) -> TimeSpan {
        self.span
    }

    /// Resolves an interned handle to its text.
    pub fn str(&self, s: Str) -> &str {
        &self.text[s.range()]
    }

    /// Resolves an optional interned handle to its text, if present.
    pub fn opt_str(&self, s: Option<Str>) -> Option<&str> {
        s.map(|s| self.str(s))
    }

    /// The slices of a thread track, in start order.
    pub fn slices(&self, track: &ThreadTrack) -> &[Slice] {
        &self.slice_pool[track.slices.range()]
    }

    /// The extra (non-typed) args of a slice, as interned key/value pairs.
    pub fn slice_others(&self, slice: &Slice) -> &[(Str, Str)] {
        &self.args_pool[slice.args.others.range()]
    }

    /// The series of a counter lane.
    pub fn series(&self, lane: &CounterLane) -> &[CounterSeries] {
        &self.series_pool[lane.series.range()]
    }

    /// The points of a counter series.
    pub fn points(&self, series: &CounterSeries) -> &[CounterPoint] {
        &self.point_pool[series.points.range()]
    }

    /// Whether the profile has a critical-path lane; the UI disables the
    /// critical-path view when absent (e.g. `--noexperimental_collect_*`).
    pub fn has_critical_path(&self) -> bool {
        self.threads
            .iter()
            .any(|t| t.lane_class == LaneClass::CriticalPath)
    }

    /// Phase markers as half-open `[start, end)` intervals: each runs to the next
    /// marker's start, the last to `span.end_us`. Mirrors Bazel's `JsonProfile`
    /// phase-summary reconstruction from consecutive markers. Each name is an
    /// interned handle; resolve with [`Profile::str`].
    pub fn phase_intervals(&self) -> Vec<(Str, i64, i64)> {
        let mut markers = self.phase_markers.clone();
        markers.sort_by_key(|m| m.start_us);
        let mut intervals = Vec::with_capacity(markers.len());
        for (i, marker) in markers.iter().enumerate() {
            let end = markers
                .get(i + 1)
                .map_or(self.span.end_us, |next| next.start_us);
            intervals.push((marker.name, marker.start_us, end));
        }
        intervals
    }
}

impl Profile {
    /// Assembles a profile from filled pools and resolved top-level vectors. The
    /// flat-pool fields are crate-internal; the parser builds them.
    pub(crate) fn from_parts(parts: ProfileParts) -> Profile {
        let ProfileParts {
            text,
            slice_pool,
            args_pool,
            series_pool,
            point_pool,
            metadata,
            threads,
            counters,
            phase_markers,
            span,
        } = parts;
        Profile {
            text,
            slice_pool,
            args_pool,
            series_pool,
            point_pool,
            metadata,
            threads,
            counters,
            phase_markers,
            span,
        }
    }
}

/// The owned components the parser fills before sealing them into a [`Profile`].
pub(crate) struct ProfileParts {
    pub text: String,
    pub slice_pool: Vec<Slice>,
    pub args_pool: Vec<(Str, Str)>,
    pub series_pool: Vec<CounterSeries>,
    pub point_pool: Vec<CounterPoint>,
    pub metadata: Metadata,
    pub threads: Vec<ThreadTrack>,
    pub counters: Vec<CounterLane>,
    pub phase_markers: Vec<PhaseMarker>,
    pub span: TimeSpan,
}

/// Deduplicating string interner: owns the growing text pool and an auxiliary
/// map from content to handle so a repeated string returns the existing [`Str`]
/// instead of appending a copy. Bazel traces repeat names (mnemonics, target
/// labels, categories) heavily, so dedup is a large memory win. The map is a
/// build-time temporary; only the [`String`] pool survives, sealed into the
/// owner that resolves its handles. Any consumer that needs its own interned
/// text pool (e.g. a derived flame graph) reuses this rather than duplicating
/// it; the produced [`Str`] handles index that consumer's [`Interner::into_text`]
/// pool.
pub struct Interner {
    text: String,
    // The map entries are bare handles, hashed and compared through the text
    // pool, so each distinct string is stored once — in the pool — rather than
    // duplicated as a boxed key, and dropping the map frees a single table.
    seen: hashbrown::HashTable<Str>,
}

/// Hashes an interner key. Non-cryptographic: the keys are short,
/// non-adversarial profile strings interned tens of millions of times, where
/// the default SipHash is the dominant lookup cost.
fn intern_hash(s: &str) -> u64 {
    use std::hash::BuildHasher;
    rustc_hash::FxBuildHasher.hash_one(s)
}

impl Interner {
    pub fn new() -> Interner {
        Interner {
            text: String::new(),
            seen: hashbrown::HashTable::new(),
        }
    }

    /// An interner pre-sized for roughly `distinct` strings totalling
    /// `text_bytes` of pool, so bulk interning grows neither the map nor the
    /// pool through repeated doublings.
    pub fn with_capacity(distinct: usize, text_bytes: usize) -> Interner {
        Interner {
            text: String::with_capacity(text_bytes),
            seen: hashbrown::HashTable::with_capacity(distinct),
        }
    }

    pub fn intern(&mut self, s: &str) -> Str {
        let hash = intern_hash(s);
        let Interner { text, seen } = self;
        if let Some(&handle) = seen.find(hash, |&h| &text[h.range()] == s) {
            return handle;
        }
        let off = text.len() as u32;
        text.push_str(s);
        let handle = Str {
            off,
            len: s.len() as u32,
        };
        seen.insert_unique(hash, handle, |&h| intern_hash(&text[h.range()]));
        handle
    }

    pub fn intern_opt(&mut self, s: Option<&str>) -> Option<Str> {
        s.map(|s| self.intern(s))
    }

    /// The interned bytes for a handle this interner produced.
    pub fn resolve(&self, s: Str) -> &str {
        &self.text[s.range()]
    }

    /// The whole text pool, for appending to another with [`Self::push_pool`].
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Reserves room for `additional` more bytes of text pool.
    pub fn reserve(&mut self, additional: usize) {
        self.text.reserve(additional);
    }

    /// Appends another interner's text pool to this one and returns the base
    /// offset it was placed at, so handles into that pool can be rebased with
    /// [`Str::shifted`]. The appended strings are not added to the dedup map (the
    /// caller is merging already-deduplicated pools), so two pools that both
    /// interned a string keep both copies; the duplication is a handful of bytes
    /// per distinct string and avoids re-hashing every handle.
    pub fn push_pool(&mut self, other: &str) -> u32 {
        let base = self.text.len() as u32;
        self.text.push_str(other);
        base
    }

    /// Consumes the interner, yielding the retained text pool and dropping the
    /// auxiliary map.
    pub fn into_text(self) -> String {
        self.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::build_for_test;

    #[test]
    fn work_class_covers_each_class_and_defaults_to_meta() {
        assert_eq!(work_class("local action execution"), WorkClass::Cpu);
        assert_eq!(work_class("remote action execution"), WorkClass::Remote);
        assert_eq!(work_class("VFS stat"), WorkClass::Io);
        assert_eq!(work_class("VFS md5"), WorkClass::Io);
        assert_eq!(work_class("thread wait"), WorkClass::Wait);
        assert_eq!(work_class("action dependency checking"), WorkClass::Wait);
        assert_eq!(work_class("gc notification"), WorkClass::Gc);
        assert_eq!(work_class("build phase marker"), WorkClass::Meta);
        assert_eq!(work_class("some future task"), WorkClass::Meta);
    }

    #[test]
    fn lane_class_derivation() {
        assert_eq!(lane_class(0, "Critical Path"), LaneClass::CriticalPath);
        assert_eq!(lane_class(7, "Main Thread"), LaneClass::Main);
        assert_eq!(lane_class(8, "Garbage Collector"), LaneClass::Gc);
        // The execution pools must win over the plain skyframe substring.
        assert_eq!(
            lane_class(12, "skyframe-evaluator-execution-3"),
            LaneClass::Execution
        );
        assert_eq!(
            lane_class(13, "skyframe-evaluator-cpu-heavy-2"),
            LaneClass::Analysis
        );
        assert_eq!(lane_class(14, "skyframe-evaluator-5"), LaneClass::Analysis);
        assert_eq!(
            lane_class(15, "starlark-repository-foo (Virtual)"),
            LaneClass::RepoFetch
        );
        assert_eq!(lane_class(99, "collect-local-resources"), LaneClass::Other);
    }

    /// Builds a profile from phase markers and a span end via the parser's flat
    /// pools, so tests inspect the real public model rather than a hand-rolled one.
    fn profile_with(markers: Vec<(&str, i64)>, span_end: i64) -> Profile {
        let mut interner = Interner::new();
        let phase_markers = markers
            .into_iter()
            .map(|(name, start_us)| PhaseMarker {
                name: interner.intern(name),
                start_us,
            })
            .collect();
        build_for_test(
            interner,
            phase_markers,
            TimeSpan {
                start_us: 0,
                end_us: span_end,
            },
            Vec::new(),
        )
    }

    #[test]
    fn phase_intervals_abut_and_last_runs_to_span_end() {
        let p = profile_with(vec![("Loading", 30), ("Launch", 0), ("Execute", 100)], 250);
        let resolved: Vec<_> = p
            .phase_intervals()
            .into_iter()
            .map(|(name, start, end)| (p.str(name).to_owned(), start, end))
            .collect();
        assert_eq!(
            resolved,
            vec![
                ("Launch".to_owned(), 0, 30),
                ("Loading".to_owned(), 30, 100),
                ("Execute".to_owned(), 100, 250),
            ]
        );
    }

    #[test]
    fn phase_intervals_handles_empty_and_single() {
        assert!(profile_with(vec![], 250).phase_intervals().is_empty());
        let p = profile_with(vec![("only", 10)], 250);
        let resolved: Vec<_> = p
            .phase_intervals()
            .into_iter()
            .map(|(name, start, end)| (p.str(name).to_owned(), start, end))
            .collect();
        assert_eq!(resolved, vec![("only".to_owned(), 10, 250)]);
    }

    #[test]
    fn has_critical_path_reflects_lane_presence() {
        let track = |tid, lane_class| ThreadTrack {
            tid,
            name: Str::default(),
            sort_index: 0,
            slices: Span32::default(),
            max_depth: 0,
            lane_class,
        };
        let p = profile_with(vec![], 0);
        assert!(!build_for_test(
            Interner::new(),
            Vec::new(),
            p.span(),
            vec![track(5, LaneClass::Execution)],
        )
        .has_critical_path());
        assert!(build_for_test(
            Interner::new(),
            Vec::new(),
            p.span(),
            vec![
                track(5, LaneClass::Execution),
                track(0, LaneClass::CriticalPath),
            ],
        )
        .has_critical_path());
    }
}
