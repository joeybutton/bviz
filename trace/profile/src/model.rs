#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub metadata: Metadata,
    pub threads: Vec<ThreadTrack>,
    pub counters: Vec<CounterLane>,
    pub phase_markers: Vec<PhaseMarker>,
    pub span: TimeSpan,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Metadata {
    pub bazel_version: Option<String>,
    pub build_id: Option<String>,
    pub output_base: Option<String>,
    pub date: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeSpan {
    pub start_us: i64,
    pub end_us: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadTrack {
    pub tid: i64,
    pub name: String,
    pub sort_index: i64,
    pub slices: Vec<Slice>,
    pub max_depth: u32,
    pub lane_class: LaneClass,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Slice {
    pub name: String,
    pub category: String,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SliceArgs {
    pub target: Option<String>,
    pub mnemonic: Option<String>,
    pub configuration: Option<String>,
    pub others: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CounterLane {
    pub name: String,
    pub series: Vec<CounterSeries>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CounterSeries {
    pub name: String,
    pub points: Vec<CounterPoint>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CounterPoint {
    pub ts_us: i64,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PhaseMarker {
    pub name: String,
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
    /// Whether the profile has a critical-path lane; the UI disables the
    /// critical-path view when absent (e.g. `--noexperimental_collect_*`).
    pub fn has_critical_path(&self) -> bool {
        self.threads
            .iter()
            .any(|t| t.lane_class == LaneClass::CriticalPath)
    }
}

/// Converts phase markers into half-open `[start, end)` intervals: each runs to
/// the next marker's start, the last to `profile.span.end_us`. Mirrors Bazel's
/// `JsonProfile` phase-summary reconstruction from consecutive markers.
pub fn phase_intervals(profile: &Profile) -> Vec<(String, i64, i64)> {
    let mut markers = profile.phase_markers.clone();
    markers.sort_by_key(|m| m.start_us);
    let mut intervals = Vec::with_capacity(markers.len());
    for (i, marker) in markers.iter().enumerate() {
        let end = markers
            .get(i + 1)
            .map_or(profile.span.end_us, |next| next.start_us);
        intervals.push((marker.name.clone(), marker.start_us, end));
    }
    intervals
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn profile_with(markers: Vec<(&str, i64)>, span_end: i64) -> Profile {
        Profile {
            metadata: Metadata::default(),
            threads: Vec::new(),
            counters: Vec::new(),
            phase_markers: markers
                .into_iter()
                .map(|(name, start_us)| PhaseMarker {
                    name: name.to_owned(),
                    start_us,
                })
                .collect(),
            span: TimeSpan {
                start_us: 0,
                end_us: span_end,
            },
        }
    }

    #[test]
    fn phase_intervals_abut_and_last_runs_to_span_end() {
        let p = profile_with(vec![("Loading", 30), ("Launch", 0), ("Execute", 100)], 250);
        assert_eq!(
            phase_intervals(&p),
            vec![
                ("Launch".to_owned(), 0, 30),
                ("Loading".to_owned(), 30, 100),
                ("Execute".to_owned(), 100, 250),
            ]
        );
    }

    #[test]
    fn phase_intervals_handles_empty_and_single() {
        assert!(phase_intervals(&profile_with(vec![], 250)).is_empty());
        assert_eq!(
            phase_intervals(&profile_with(vec![("only", 10)], 250)),
            vec![("only".to_owned(), 10, 250)],
        );
    }

    #[test]
    fn has_critical_path_reflects_lane_presence() {
        let track = |tid, lane_class| ThreadTrack {
            tid,
            name: String::new(),
            sort_index: 0,
            slices: Vec::new(),
            max_depth: 0,
            lane_class,
        };
        let mut p = profile_with(vec![], 0);
        p.threads.push(track(5, LaneClass::Execution));
        assert!(!p.has_critical_path());
        p.threads.push(track(0, LaneClass::CriticalPath));
        assert!(p.has_critical_path());
    }
}
