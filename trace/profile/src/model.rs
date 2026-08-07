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
/// heap lives in this type's text and element pools — dropping a `Profile`
/// frees a handful of large allocations rather than millions of tiny ones, and
/// the type is `Send + Sync` with no lifetime parameters.
#[derive(Debug, PartialEq)]
pub struct Profile {
    /// The one text pool every [`Str`] in this profile indexes.
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
    distinct_strings: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Metadata {
    pub bazel_version: OptStr,
    pub build_id: OptStr,
    pub output_base: OptStr,
    pub date: OptStr,
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
    /// This track's contiguous run in the slice, detail, and end-max pools
    /// alike — the runs partition all three, but not necessarily in the order
    /// of [`Profile::threads`], so a track's run is reached only through this
    /// span and never through the track's position. Resolve with
    /// [`Profile::slices`], [`Profile::details`], and [`Profile::end_maxes`].
    pub slices: Span32,
    /// Slice rows the track's nesting occupies: one past the deepest
    /// [`Slice::depth`], and zero for a track with no slices.
    pub max_depth: u32,
    pub lane_class: LaneClass,
}

/// The hot per-slice record: only what culling, hit-testing, and painting read
/// per frame, sized to fit two slices per cache line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slice {
    pub name: Str,
    pub start_us: i64,
    pub dur_us: i64,
    pub depth: u32,
    pub role: SpanRole,
}

const _: () = assert!(std::mem::size_of::<Slice>() == 32);

/// The cold companion of a [`Slice`], read only on demand (tooltips, grouping):
/// the profile's detail pool is exactly parallel to its slice pool, so a
/// slice's detail is the entry at the same index and a thread's `slices` span
/// resolves both. The typed args carry the Bazel keys callers query directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SliceDetail {
    pub category: Str,
    pub target: OptStr,
    pub mnemonic: OptStr,
    pub configuration: OptStr,
    /// Range into the profile's args pool of the key/value pairs beyond the
    /// typed args; resolve with [`Profile::slice_others`].
    pub others: Span32,
}

const _: () = assert!(std::mem::size_of::<SliceDetail>() == 40);

/// The kind of work a span represents, giving the base hue in the flame views.
/// Derived from the Bazel `cat` plus the span name ([`span_role`]) because a
/// single category can span several kinds: one "general information" `cat` holds
/// the spawned process, the sandbox I/O around it, and the skyframe evaluation
/// envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Workload {
    /// The spawned action body: a local/remote/worker process, the frames that
    /// envelope it (the mnemonic-bearing action frame, the local resource slot
    /// the action holds for the length of its body), and its replay on the
    /// critical path.
    Subprocess,
    /// Build logic run by the interpreter: Starlark calls, rule bodies, module
    /// and repository evaluation.
    Starlark,
    /// Filesystem work: VFS operations, sandbox construction, globbing, input
    /// and output staging, include scanning.
    Filesystem,
    /// Remote and cache transfer: action-cache checks, up/download, RPC, and
    /// repository fetching.
    RemoteCache,
    /// Scheduling and contention: resource locks, throttle semaphores, queuing,
    /// and thread waits.
    Scheduling,
    /// Skyframe graph evaluation and its action-execution envelopes.
    Skyframe,
    /// Java garbage collection.
    Gc,
    /// Whole-build umbrellas, module lifecycle, and anything unrecognized.
    Other,
}

/// A span's coloring role: a [`Workload`] for the base hue, and whether the span
/// is build payload — the user's own work (the compiler, the Starlark that
/// defines the build), drawn vivid — rather than Bazel overhead (scheduling,
/// caching, graph bookkeeping, the sandbox), drawn muted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpanRole {
    pub workload: Workload,
    pub payload: bool,
}

/// Classify a slice for coloring from its Bazel `cat` and name. A category whose
/// spans are not all one kind of work splits on the name; the rest map by `cat`
/// alone. An unknown or future category falls through to [`Workload::Other`]
/// overhead.
///
/// Two placements are judgment calls: skyfunction frames (including
/// `CONFIGURED_TARGET`) are [`Workload::Skyframe`] overhead — the payload is the
/// Starlark they nest, not the graph node itself; and include scanning and input
/// discovery are [`Workload::Filesystem`] overhead rather than action body,
/// since they compute an action's inputs rather than run it.
pub fn span_role(category: &str, name: &str) -> SpanRole {
    use Workload::*;
    let (workload, payload) = match category {
        "general information" => general_information_role(name),
        "Remote execution setup" | "Remote execution upload time"
        | "action dependency checking" => remote_cache_role(name),

        "action processing"
        | "local action execution"
        | "Local execution process wall time"
        | "local execution in worker"
        | "Local parse to prepare for remote execution"
        | "remote action execution"
        | "Remote execution process wall time"
        | "critical path component" => (Subprocess, true),

        "action count (local)" => (Subprocess, false),

        "Starlark Parser"
        | "Starlark user function call"
        | "Starlark builtin function call"
        | "Starlark compiled user function call"
        | "Starlark repository function call"
        | "Starlark thread context"
        | "bazel module processing"
        | "load WebAssembly module"
        | "execute WebAssembly function" => (Starlark, true),

        "VFS stat" | "VFS readdir" | "VFS readlink" | "VFS md5" | "VFS xattr"
        | "VFS delete" | "VFS open" | "VFS read" | "VFS write"
        | "VMFS stat" | "VMFS readdir" | "VMFS read"
        | "globbing" | "filesystem traversal"
        | "Staging per-action file system"
        | "setting up inputs for worker" | "copying outputs from worker"
        | "include scanner" | "discover inputs"
        | "Vendoring repository" | "logging spawn" => (Filesystem, false),

        "Remote execution file fetching" | "remote output download"
        | "remote action cache check" | "remote network" | "RPC"
        | "Fetching repository" => (RemoteCache, false),

        "Remote execution queuing time" | "thread wait" | "action resource lock"
        | "Acquiring dynamic execution output lock"
        | "borrowing a worker" | "waiting for response from worker"
        | "blocked on repo contents cache GC" => (Scheduling, false),

        "skyfunction" | "skyframe evaluator" | "package creation"
        | "complete action execution" | "update action information"
        | "prepare action rewind plan" | "Conflict checking" => (Skyframe, false),

        "gc notification" => (Gc, false),

        _ => (Other, false),
    };
    SpanRole { workload, payload }
}

/// The role of a span in a category that hosts `acquiring semaphore` alongside
/// [`Workload::RemoteCache`] work: that name is a throttle wait, which Bazel
/// profiles under the category of the work it gates rather than one of its own.
fn remote_cache_role(name: &str) -> (Workload, bool) {
    use Workload::*;
    if name == "acquiring semaphore" {
        (Scheduling, false)
    } else {
        (RemoteCache, false)
    }
}

/// The role of a "general information" span, which alone spans several workloads:
/// the real spawn (`subprocess.run`, the one payload here), the sandbox and I/O
/// helpers around it, the remote-cache and repository transfer that the
/// profiler's no-task overload files here, and the skyframe/action-execution
/// envelopes. Whole-build umbrellas (`buildTargets`) and module lifecycle fall
/// through to [`Workload::Other`].
fn general_information_role(name: &str) -> (Workload, bool) {
    use Workload::*;
    if name == "subprocess.run" {
        (Subprocess, true)
    } else if name.starts_with("sandbox.")
        || name.starts_with("trashPath")
        || name == "context.prefetchInputs"
        || name == "AbstractSpawnStrategy.getInputMapping"
        || name == "checkOutputs"
        || name == "action.prepare"
        || name == "Create symlink tree"
    {
        (Filesystem, false)
    } else if name.starts_with("Remote.")
        || name.starts_with("MerkleTree")
        || name.starts_with("fetching: repository")
        || name.starts_with("extracting: repository")
        || matches!(
            name,
            "findMissingDigests"
                | "upload"
                | "build upload manifest"
                | "checkForConcurrentModifications"
                | "doExtendLeases"
                | "getServerCapabilities"
                | "getAndVerifyServerCapabilities"
        )
    {
        (RemoteCache, false)
    } else if name == "Action.execute"
        || name == "postprocessing.run"
        || name == "runAnalysisAndExecutionPhase"
        || name == "ParallelEvaluator.eval"
        || name.starts_with("skyframeExecutor")
    {
        (Skyframe, false)
    } else {
        (Other, false)
    }
}

/// A counter graph. The parser splits every counter into one lane per numeric
/// series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CounterLane {
    pub name: Str,
    /// Range into the profile's series pool; resolve with [`Profile::series`].
    /// Spans at most one entry — the lane's own series — and none for a counter
    /// that carried no numeric points.
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
/// machinery), distinct from the skyframe evaluator pools. The tree-artifact
/// visitor, symlink planter, and tree deleter materialize and clean up an
/// action's outputs.
const EXECUTION_PREFIXES: [&str; 12] = [
    "dynamic-execution",
    "Include scanner",
    "remote-execution",
    "remote-executor",
    "lease-extension",
    "combined-cache-",
    "merkle-tree-",
    "virtual-action-input-pipe-",
    "jdeps-reader-",
    "tree-artifact-visitor",
    "Non-eager Symlink planter",
    "tree-deleter",
];

/// tid 0 is Bazel's synthetic critical-path thread; the rest are matched on the
/// readable thread name Bazel emits.
///
/// The skyframe-evaluator tests run longest-match-first: the `-execution`/
/// `-memoizing` pools run actions, the `-cpu-heavy` and plain pools run analysis
/// (configured targets/aspects, plus loading and light analysis), so the plain
/// `skyframe-evaluator` substring must be tested only after the execution
/// infixes are ruled out.
///
/// Module-extension evaluation resolves external dependencies alongside
/// repository fetching, so it is loading work; the `globbing pool` threads run
/// glob I/O for package loading, which merges into analysis like the plain
/// skyframe pools.
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
    } else if name.contains("skyframe-evaluator") || name.starts_with("globbing pool") {
        LaneClass::Analysis
    } else if name.starts_with("starlark-repository")
        || name.starts_with("module-extension-")
        || name.starts_with("downloads[")
    {
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

    /// The thread tracks in display order — `sort_index` ascending, ties by
    /// `tid` — the position that per-track UI state is indexed by.
    pub fn threads(&self) -> &[ThreadTrack] {
        &self.threads
    }

    /// The counter lanes in display order: grouped by kind — action counts,
    /// system load, CPU, memory, network — with each cluster's aggregate ahead
    /// of its Bazel-only breakdown. Kinds the grouping does not recognise come
    /// last, in first-seen order.
    pub fn counters(&self) -> &[CounterLane] {
        &self.counters
    }

    /// The build's phase markers, sorted by `start_us` ascending.
    pub fn phase_markers(&self) -> &[PhaseMarker] {
        &self.phase_markers
    }

    /// The extent covering every thread slice, counter sample, and phase
    /// marker, collapsed to zero for a profile carrying none of them.
    pub fn span(&self) -> TimeSpan {
        self.span
    }

    /// The number of distinct interned strings — an upper bound on the distinct
    /// slice names and mnemonics an aggregation keys on, so a scan or stats
    /// pass sizes its per-text maps once instead of rehashing up from empty.
    pub fn distinct_strings(&self) -> usize {
        self.distinct_strings
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
        let markers = &self.phase_markers;
        markers
            .iter()
            .enumerate()
            .map(|(i, marker)| {
                let end = markers
                    .get(i + 1)
                    .map_or(self.span.end_us, |next| next.start_us);
                (marker.name, marker.start_us, end)
            })
            .collect()
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
            mut phase_markers,
            span,
            distinct_strings,
        } = parts;
        phase_markers.sort_by_key(|m| m.start_us);
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
            distinct_strings,
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
    pub distinct_strings: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::concurrent_interner::ConcurrentInterner;
    use crate::parse::build_for_test;

    #[test]
    fn span_role_maps_categories_and_names_to_workload_and_payload() {
        let role = |cat, name| span_role(cat, name);
        let payload = |w| SpanRole { workload: w, payload: true };
        let overhead = |w| SpanRole { workload: w, payload: false };
        // The action body and Starlark are payload.
        assert_eq!(role("action processing", "Rustc"), payload(Workload::Subprocess));
        assert_eq!(
            role("remote action execution", ""),
            payload(Workload::Subprocess)
        );
        assert_eq!(
            role("critical path component", "action 'Compiling foo.cc'"),
            payload(Workload::Subprocess)
        );
        assert_eq!(
            role("Starlark user function call", "_cc_library_impl"),
            payload(Workload::Starlark)
        );
        // Overhead workloads.
        assert_eq!(role("VFS stat", ""), overhead(Workload::Filesystem));
        assert_eq!(role("VFS md5", ""), overhead(Workload::Filesystem));
        assert_eq!(role("include scanner", ""), overhead(Workload::Filesystem));
        assert_eq!(role("RPC", ""), overhead(Workload::RemoteCache));
        // The throttle wait splits away from the transfer it gates.
        assert_eq!(
            role("Remote execution setup", "acquiring semaphore"),
            overhead(Workload::Scheduling)
        );
        assert_eq!(
            role("Remote execution upload time", "acquiring semaphore"),
            overhead(Workload::Scheduling)
        );
        assert_eq!(
            role("Remote execution setup", "prepare remote action"),
            overhead(Workload::RemoteCache)
        );
        assert_eq!(
            role("Remote execution upload time", "upload outputs"),
            overhead(Workload::RemoteCache)
        );
        // The local action-cache lookup is named after the action it checks.
        assert_eq!(
            role("action dependency checking", "action 'Compiling foo.cc'"),
            overhead(Workload::RemoteCache)
        );
        assert_eq!(
            role("action dependency checking", "acquiring semaphore"),
            overhead(Workload::Scheduling)
        );
        // The held resource slot envelopes the action body, so it takes the
        // action's hue but stays overhead.
        assert_eq!(
            role("action count (local)", "Resources acquired"),
            overhead(Workload::Subprocess)
        );
        // The skyfunction frame is overhead; its Starlark children carry the work.
        assert_eq!(role("skyfunction", "CONFIGURED_TARGET"), overhead(Workload::Skyframe));
        assert_eq!(role("gc notification", "minor GC"), overhead(Workload::Gc));
        // "general information" splits by name.
        assert_eq!(
            role("general information", "subprocess.run"),
            payload(Workload::Subprocess)
        );
        assert_eq!(
            role("general information", "sandbox.createInputs"),
            overhead(Workload::Filesystem)
        );
        assert_eq!(
            role("general information", "Action.execute"),
            overhead(Workload::Skyframe)
        );
        assert_eq!(
            role("general information", "postprocessing.run"),
            overhead(Workload::Skyframe)
        );
        for name in [
            "Remote.download",
            "MerkleTreeComputer.buildForSpawn",
            "findMissingDigests",
            "upload",
            "build upload manifest",
            "checkForConcurrentModifications",
            "doExtendLeases",
            "getServerCapabilities",
            "getAndVerifyServerCapabilities",
            "fetching: repository @@rules_m4++default_toolchain_ext+m4_v1.4.18",
            "extracting: repository @@rules_m4++default_toolchain_ext+m4_v1.4.18",
        ] {
            assert_eq!(
                role("general information", name),
                overhead(Workload::RemoteCache),
                "{name}"
            );
        }
        assert_eq!(
            role("general information", "buildTargets"),
            overhead(Workload::Other)
        );
        // An unknown category is Other overhead.
        assert_eq!(role("some future task", ""), overhead(Workload::Other));
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
        assert_eq!(
            lane_class(16, "module-extension-@@rules_rs+//rs:extensions.bzl%crate-0 (Virtual)"),
            LaneClass::RepoFetch
        );
        assert_eq!(lane_class(17, "globbing pool-3"), LaneClass::Analysis);
        assert_eq!(lane_class(18, "tree-artifact-visitor-2"), LaneClass::Execution);
        assert_eq!(
            lane_class(19, "Non-eager Symlink planter 1"),
            LaneClass::Execution
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
