//! Test-only builders that assemble a [`Profile`] through the real Chrome-trace
//! parser, so the layout tests exercise the same public model the UI sees rather
//! than a hand-rolled one. A slice is described declaratively here, emitted as a
//! trace event, and parsed back; the parser owns interning, depth assignment, and
//! lane classification.

use bviz_trace_profile::{LaneClass, Profile};

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
/// it.
pub fn profile(tracks: Vec<TestTrack>) -> Profile {
    profile_with_counters(tracks, &[])
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
