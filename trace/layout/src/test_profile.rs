//! Test-only builders that assemble a [`Profile`] through the real Chrome-trace
//! parser, so the layout tests exercise the same public model the UI sees rather
//! than a hand-rolled one. A slice is described declaratively here, emitted as a
//! trace event, and parsed back; the parser owns interning, depth assignment, and
//! lane classification.

use std::sync::Arc;

use bviz_trace_profile::{LaneClass, Profile};
use fastrand::Rng;

/// A slice to emit as a `ph:"X"` trace event. `mnemonic` rides in `args` when set,
/// mirroring Bazel's action slices the mnemonic index reads.
#[derive(Clone)]
pub struct TestSlice {
    pub name: String,
    pub category: String,
    pub start_us: i64,
    pub dur_us: i64,
    pub mnemonic: Option<String>,
}

/// A slice with no action mnemonic; its name is its own informative label.
pub fn slice(name: &str, category: &str, start_us: i64, dur_us: i64) -> TestSlice {
    TestSlice {
        name: name.to_owned(),
        category: category.to_owned(),
        start_us,
        dur_us,
        mnemonic: None,
    }
}

/// A CPU-class action whose name equals its mnemonic — a worker action whose
/// own args carry the mnemonic.
pub fn action(mnemonic: &str, start_us: i64, dur_us: i64) -> TestSlice {
    TestSlice {
        mnemonic: Some(mnemonic.to_owned()),
        ..slice(mnemonic, "local action execution", start_us, dur_us)
    }
}

/// An action-processing span: the name carries the action description and the
/// mnemonic rides in args, so the mnemonic index can recover the mnemonic for a
/// critical-path component that names the same description.
pub fn action_named(name: &str, mnemonic: &str, start_us: i64, dur_us: i64) -> TestSlice {
    TestSlice {
        mnemonic: Some(mnemonic.to_owned()),
        ..slice(name, "action processing", start_us, dur_us)
    }
}

/// A span that holds its lane parked (see
/// [`parks_thread`](bviz_trace_profile::parks_thread)).
pub fn wait(start_us: i64, dur_us: i64) -> TestSlice {
    slice(
        "Parallel Evaluator evaluation",
        "skyframe evaluator",
        start_us,
        dur_us,
    )
}

/// A thread track carrying the slices of one lane. The lane class is requested
/// directly; [`profile`] gives the thread a name the parser's `lane_class`
/// derivation maps back to that class.
pub struct TestTrack {
    pub tid: i64,
    pub lane_class: LaneClass,
    pub slices: Vec<TestSlice>,
}

pub fn track(tid: i64, lane_class: LaneClass, slices: Vec<TestSlice>) -> TestTrack {
    TestTrack {
        tid,
        lane_class,
        slices,
    }
}

/// A thread name the parser's `lane_class` derivation maps to `class`. The
/// critical-path class is carried by tid 0 instead of a name, so any name does.
fn lane_name(class: LaneClass) -> &'static str {
    match class {
        LaneClass::CriticalPath => "Critical Path",
        LaneClass::Main => "Main Thread",
        LaneClass::Gc => "Garbage Collector",
        LaneClass::RepoFetch => "starlark-repository-foo",
        LaneClass::Analysis => "skyframe-evaluator-3",
        LaneClass::Execution => "skyframe-evaluator-execution-1",
        LaneClass::Other => "collect-local-resources",
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Assemble the tracks into a profile by emitting a Chrome-trace JSON and parsing
/// it. Returned as an `Arc`, the form flame-graph construction takes (the graph
/// retains the profile it aggregates); borrowing consumers coerce through it.
pub fn profile(tracks: Vec<TestTrack>) -> Arc<Profile> {
    Arc::new(profile_with_counters(tracks, &[]))
}

/// A counter `ph:"C"` series: a lane name and its `(ts_us, value)` points under a
/// single series key.
pub struct TestCounter {
    pub name: String,
    pub points: Vec<(i64, f64)>,
}

pub fn counter(name: &str, points: Vec<(i64, f64)>) -> TestCounter {
    TestCounter {
        name: name.to_owned(),
        points,
    }
}

/// Assemble tracks and counter lanes into a profile. Each track gets a
/// `thread_name` metadata event so the parser derives the requested lane class;
/// each counter emits `ph:"C"` events under one series key `v`.
pub fn profile_with_counters(tracks: Vec<TestTrack>, counters: &[TestCounter]) -> Profile {
    let mut events = Vec::new();
    for t in &tracks {
        events.push(format!(
            r#"{{"name":"thread_name","ph":"M","tid":{},"args":{{"name":"{}"}}}}"#,
            t.tid,
            escape(lane_name(t.lane_class)),
        ));
        for s in &t.slices {
            let args = match &s.mnemonic {
                Some(m) => format!(r#","args":{{"mnemonic":"{}"}}"#, escape(m)),
                None => String::new(),
            };
            events.push(format!(
                r#"{{"cat":"{}","name":"{}","ph":"X","ts":{},"dur":{},"tid":{}{}}}"#,
                escape(&s.category),
                escape(&s.name),
                s.start_us,
                s.dur_us,
                t.tid,
                args,
            ));
        }
    }
    for c in counters {
        for (ts, value) in &c.points {
            events.push(format!(
                r#"{{"name":"{}","ph":"C","ts":{},"tid":0,"args":{{"v":{}}}}}"#,
                escape(&c.name),
                ts,
                value,
            ));
        }
    }
    let json = format!(r#"{{"traceEvents":[{}]}}"#, events.join(","));
    Profile::from_bytes(json.as_bytes()).expect("parse test profile")
}

/// A small random profile: lanes of any class with at most one critical path,
/// each holding a few spans that begin before `horizon_us`, run for up to half
/// of it, and nest or overlap as they fall. Zero-length spans, parking waits,
/// actions and plain work are all among them.
pub fn random_profile(rng: &mut Rng, horizon_us: i64) -> Arc<Profile> {
    const MAX_LANES: i64 = 6;
    const MAX_SLICES: usize = 8;
    const NAMES: [&str; 4] = ["a", "b", "c", "d"];
    // Equally likely outcomes of each draw; a `match` on one gives those it
    // does not name to its last arm.
    const LANE_CLASS_DRAWS: u32 = 8;
    const DURATION_DRAWS: u32 = 5;
    const SPAN_KIND_DRAWS: u32 = 4;
    let lanes = rng.i64(1..=MAX_LANES);
    let mut tracks = Vec::with_capacity(lanes as usize);
    let mut has_critical_path = false;
    for lane in 0..lanes {
        let lane_class = match rng.u32(..LANE_CLASS_DRAWS) {
            0 => LaneClass::Gc,
            1 => LaneClass::Main,
            2 if !has_critical_path => LaneClass::CriticalPath,
            3 => LaneClass::Analysis,
            _ => LaneClass::Execution,
        };
        has_critical_path |= lane_class == LaneClass::CriticalPath;
        let tid = match lane_class {
            LaneClass::CriticalPath => 0,
            _ => lane + 1,
        };
        let slices = (0..rng.usize(..=MAX_SLICES))
            .map(|_| {
                let start_us = rng.i64(0..horizon_us);
                let dur_us = match rng.u32(..DURATION_DRAWS) {
                    0 => 0,
                    1 => 1,
                    _ => rng.i64(0..horizon_us / 2),
                };
                let name = NAMES[rng.usize(..NAMES.len())];
                match rng.u32(..SPAN_KIND_DRAWS) {
                    0 => wait(start_us, dur_us),
                    1 => action_named(&format!("Compiling {name}"), "Rustc", start_us, dur_us),
                    2 => action(name, start_us, dur_us),
                    _ => slice(name, "generic", start_us, dur_us),
                }
            })
            .collect();
        tracks.push(track(tid, lane_class, slices));
    }
    profile(tracks)
}
