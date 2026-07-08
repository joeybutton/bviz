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
    pub const fn new(off: u32, len: u32) -> Str {
        Str { off, len }
    }

    /// The byte range into the owning text pool: `&pool[s.range()]`.
    pub fn range(self) -> std::ops::Range<usize> {
        self.off as usize..(self.off + self.len) as usize
    }
}

/// An optional interned-string handle in the size of a bare [`Str`]: offset 0
/// encodes absence. Every interner seeds its pool with a placeholder byte, so
/// no live handle ever has offset 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(transparent)]
pub struct OptStr(Str);

impl OptStr {
    pub const NONE: OptStr = OptStr(Str { off: 0, len: 0 });

    pub fn get(self) -> Option<Str> {
        (self.0.off != 0).then_some(self.0)
    }
}

impl From<Option<Str>> for OptStr {
    fn from(s: Option<Str>) -> OptStr {
        match s {
            Some(s) => {
                debug_assert!(s.off != 0, "offset 0 is the absence encoding");
                OptStr(s)
            }
            None => OptStr::NONE,
        }
    }
}

impl From<OptStr> for Option<Str> {
    fn from(s: OptStr) -> Option<Str> {
        s.get()
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
    pub(crate) detail_pool: Vec<SliceDetail>,
    pub(crate) end_max_pool: Vec<i64>,
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

/// The hot per-slice record: only what culling, hit-testing, and painting read
/// per frame, sized to fit two slices per cache line. The cold companion fields
/// live in the parallel [`SliceDetail`] at the same index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slice {
    pub name: Str,
    pub start_us: i64,
    pub dur_us: i64,
    pub depth: u32,
    pub work_class: WorkClass,
}

const _: () = assert!(std::mem::size_of::<Slice>() == 32);

/// The cold companion of a [`Slice`], read only on demand (tooltips, grouping):
/// the profile's detail pool is exactly parallel to its slice pool, so a
/// thread's `slices` span resolves both. The three typed args carry the Bazel
/// keys callers query directly; `others` is a range into the profile's args
/// pool of the remaining key/value pairs, resolved with
/// [`Profile::slice_others`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SliceDetail {
    pub category: Str,
    pub target: OptStr,
    pub mnemonic: OptStr,
    pub configuration: OptStr,
    pub others: Span32,
}

const _: () = assert!(std::mem::size_of::<SliceDetail>() == 40);

/// Coarse kind of work a slice represents, derived from its Bazel `cat`
/// description string ([`work_class`]). Lets a flame graph group/scope by the
/// nature of the work rather than the dozens of raw categories.
///
/// Two descriptions are judgment calls: "VFS md5" is a digest computation that
/// is both CPU- and I/O-bound — classified [`WorkClass::Io`] alongside the rest
/// of VFS; "action dependency checking" sits between compute and waiting on the
/// action cache — classified [`WorkClass::Wait`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

/// A counter graph. The parser splits every counter into one lane per numeric
/// series, so `series` spans at most one entry (zero for a counter that
/// carried no numeric points).
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
    /// Largest point value, precomputed so a painter scales the lane without
    /// walking the points.
    pub max_value: f64,
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
    pub fn opt_str(&self, s: impl Into<Option<Str>>) -> Option<&str> {
        s.into().map(|s| self.str(s))
    }

    /// The slices of a thread track, in lane order: start ascending, ties
    /// longest-duration-first, so an enclosing span precedes the spans it
    /// contains.
    pub fn slices(&self, track: &ThreadTrack) -> &[Slice] {
        &self.slice_pool[track.slices.range()]
    }

    /// The details of a thread track's slices, index-parallel to
    /// [`Profile::slices`].
    pub fn details(&self, track: &ThreadTrack) -> &[SliceDetail] {
        &self.detail_pool[track.slices.range()]
    }

    /// The running maximum end time (`start_us + dur_us`) of a thread track's
    /// slices: element `i` is the max over the track's slices `0..=i`, reset at
    /// each track. Index-parallel to [`Profile::slices`] and monotone
    /// nondecreasing within a track, so the leftmost slice that can still
    /// overlap a view is a binary search (`partition_point`) for the first
    /// element reaching the view's left cull bound, instead of a scan from 0.
    pub fn end_maxes(&self, track: &ThreadTrack) -> &[i64] {
        &self.end_max_pool[track.slices.range()]
    }

    /// The extra (non-typed) args of a slice, as interned key/value pairs.
    pub fn slice_others(&self, detail: &SliceDetail) -> &[(Str, Str)] {
        &self.args_pool[detail.others.range()]
    }

    /// The series of a counter lane.
    pub fn series(&self, lane: &CounterLane) -> &[CounterSeries] {
        &self.series_pool[lane.series.range()]
    }

    /// The points of a counter series, sorted by `ts_us` ascending so callers
    /// can binary-search by time.
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
            detail_pool,
            end_max_pool,
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
            detail_pool,
            end_max_pool,
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
    pub detail_pool: Vec<SliceDetail>,
    pub end_max_pool: Vec<i64>,
    pub args_pool: Vec<(Str, Str)>,
    pub series_pool: Vec<CounterSeries>,
    pub point_pool: Vec<CounterPoint>,
    pub metadata: Metadata,
    pub threads: Vec<ThreadTrack>,
    pub counters: Vec<CounterLane>,
    pub phase_markers: Vec<PhaseMarker>,
    pub span: TimeSpan,
}

/// Hashes an interner key. Non-cryptographic: the keys are short,
/// non-adversarial profile strings interned tens of millions of times, where
/// the default SipHash is the dominant lookup cost.
pub(crate) fn intern_hash(s: &str) -> u64 {
    use std::hash::BuildHasher;
    rustc_hash::FxBuildHasher.hash_one(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::concurrent_interner::ConcurrentInterner;
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
        let interner = ConcurrentInterner::new(1024, 16);
        let phase_markers = {
            let mut writer = interner.writer();
            markers
                .into_iter()
                .map(|(name, start_us)| PhaseMarker {
                    name: writer.intern(name),
                    start_us,
                })
                .collect()
        };
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
            ConcurrentInterner::new(1024, 16),
            Vec::new(),
            p.span(),
            vec![track(5, LaneClass::Execution)],
        )
        .has_critical_path());
        assert!(build_for_test(
            ConcurrentInterner::new(1024, 16),
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
