use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;
use std::path::Path;

use flate2::read::MultiGzDecoder;
use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

use crate::model::{
    lane_class, work_class, CounterLane, CounterPoint, CounterSeries, Metadata, PhaseMarker,
    Profile, Slice, SliceArgs, ThreadTrack, TimeSpan,
};

const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
const MAX_SORT_INDEX: i64 = 1_000_000;
const PHASE_MARKER_CATEGORY: &str = "build phase marker";

#[derive(Debug)]
pub enum LoadError {
    Io(std::io::Error),
    Decompress(std::io::Error),
    Json(serde_json::Error),
    Corrupted(String),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "i/o error: {e}"),
            LoadError::Decompress(e) => write!(f, "decompression error: {e}"),
            LoadError::Json(e) => write!(f, "json error: {e}"),
            LoadError::Corrupted(m) => write!(f, "corrupted profile: {m}"),
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LoadError::Io(e) | LoadError::Decompress(e) => Some(e),
            LoadError::Json(e) => Some(e),
            LoadError::Corrupted(_) => None,
        }
    }
}

#[derive(Deserialize)]
struct RawProfile {
    #[serde(rename = "otherData")]
    other_data: Option<Map<String, Value>>,
    #[serde(rename = "traceEvents")]
    trace_events: Vec<RawEvent>,
}

#[derive(Deserialize)]
struct RawEvent {
    #[serde(default)]
    ph: Option<String>,
    #[serde(default)]
    cat: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    ts: Option<f64>,
    #[serde(default)]
    dur: Option<f64>,
    #[serde(default)]
    tid: Option<i64>,
    #[serde(default)]
    args: Option<RawArgs>,
}

/// One kept argument value: a string or a number. Other JSON shapes (array,
/// object, null) are dropped, matching what every `args` consumer reads — they
/// only ever look at string or numeric fields.
enum ArgVal {
    Str(String),
    Int(i64),
    Float(f64),
}

impl ArgVal {
    /// The value as the display string the profile's `others` list carries,
    /// matching how a JSON scalar formats.
    fn to_display(&self) -> String {
        match self {
            ArgVal::Str(s) => s.clone(),
            ArgVal::Int(n) => n.to_string(),
            ArgVal::Float(n) => n.to_string(),
        }
    }

    /// The value as an `f64` for counter series, or `None` for a non-numeric.
    fn as_f64(&self) -> Option<f64> {
        match self {
            ArgVal::Int(n) => Some(*n as f64),
            ArgVal::Float(n) => Some(*n),
            ArgVal::Str(_) => None,
        }
    }
}

/// An event's `args` object, deserialized as a flat key/value list rather than
/// the `serde_json::Map<String, Value>` the generic path would build. Over the
/// millions of slice events in a large profile the saved per-arg `Value`/`Map`
/// allocations are a large share of parse time, and every consumer (slice args,
/// thread metadata, counters) reads this list directly.
struct RawArgs {
    entries: Vec<(String, ArgVal)>,
}

impl RawArgs {
    /// The string value for `key`, if present and a string.
    fn get_str(&self, key: &str) -> Option<&str> {
        self.entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
            ArgVal::Str(s) => Some(s.as_str()),
            _ => None,
        })
    }

    /// The integer value for `key`, if present and an integer.
    fn get_i64(&self, key: &str) -> Option<i64> {
        self.entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
            ArgVal::Int(n) => Some(*n),
            _ => None,
        })
    }
}

impl<'de> Deserialize<'de> for RawArgs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(RawArgsVisitor)
    }
}

struct RawArgsVisitor;

impl<'de> Visitor<'de> for RawArgsVisitor {
    type Value = RawArgs;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an args object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<RawArgs, A::Error> {
        let mut entries = Vec::new();
        while let Some(key) = map.next_key::<String>()? {
            if let MaybeArg(Some(v)) = map.next_value()? {
                entries.push((key, v));
            }
        }
        Ok(RawArgs { entries })
    }
}

/// An arg value that keeps a scalar and discards a composite, deserialized by
/// dispatching on the JSON token so no intermediate `Value` is built.
struct MaybeArg(Option<ArgVal>);

impl<'de> Deserialize<'de> for MaybeArg {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(MaybeArgVisitor)
    }
}

struct MaybeArgVisitor;

impl<'de> Visitor<'de> for MaybeArgVisitor {
    type Value = MaybeArg;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_str<E>(self, v: &str) -> Result<MaybeArg, E> {
        Ok(MaybeArg(Some(ArgVal::Str(v.to_owned()))))
    }

    fn visit_string<E>(self, v: String) -> Result<MaybeArg, E> {
        Ok(MaybeArg(Some(ArgVal::Str(v))))
    }

    fn visit_i64<E>(self, v: i64) -> Result<MaybeArg, E> {
        Ok(MaybeArg(Some(ArgVal::Int(v))))
    }

    fn visit_u64<E>(self, v: u64) -> Result<MaybeArg, E> {
        Ok(MaybeArg(Some(
            i64::try_from(v).map(ArgVal::Int).unwrap_or(ArgVal::Float(v as f64)),
        )))
    }

    fn visit_f64<E>(self, v: f64) -> Result<MaybeArg, E> {
        Ok(MaybeArg(Some(ArgVal::Float(v))))
    }

    fn visit_bool<E>(self, v: bool) -> Result<MaybeArg, E> {
        Ok(MaybeArg(Some(ArgVal::Str(v.to_string()))))
    }

    fn visit_unit<E>(self) -> Result<MaybeArg, E> {
        Ok(MaybeArg(None))
    }

    fn visit_none<E>(self) -> Result<MaybeArg, E> {
        Ok(MaybeArg(None))
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<MaybeArg, D::Error> {
        d.deserialize_any(self)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<MaybeArg, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(MaybeArg(None))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<MaybeArg, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(MaybeArg(None))
    }
}

fn round_us(v: Option<f64>) -> i64 {
    v.map(|x| x.round() as i64).unwrap_or(0)
}

impl Profile {
    pub fn from_path(path: &Path) -> Result<Profile, LoadError> {
        Profile::from_bytes(&std::fs::read(path).map_err(LoadError::Io)?)
    }

    /// Decompresses (if gzipped) and transcodes the profile to UTF-8, then parses
    /// it with `serde_json::from_str`. The whole text is decoded into memory
    /// rather than streamed: `from_str` works over a slice (`SliceRead`), far
    /// faster than the byte-at-a-time `from_reader` (`IoRead`) on a trace of
    /// millions of events, and a profile compresses small enough that the decoded
    /// text fits comfortably. A parse failure — most often a profile truncated by
    /// an interrupted Bazel — falls back to the best-effort [`recover`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Profile, LoadError> {
        let decoded = decode(bytes)?;
        match serde_json::from_str::<RawProfile>(&decoded) {
            Ok(raw) => Ok(build(raw.other_data, raw.trace_events)),
            Err(_) => recover(&decoded),
        }
    }
}

fn decode(bytes: &[u8]) -> Result<String, LoadError> {
    let raw = if bytes.starts_with(&GZIP_MAGIC) {
        let mut out = Vec::new();
        MultiGzDecoder::new(bytes)
            .read_to_end(&mut out)
            .map_err(LoadError::Decompress)?;
        out
    } else {
        bytes.to_vec()
    };
    Ok(latin1_to_utf8(raw))
}

/// Transcodes an ISO-8859-1 (Latin-1) byte buffer — how Bazel writes profile
/// strings — to UTF-8. A Bazel profile is overwhelmingly ASCII, so the all-ASCII
/// case (already valid UTF-8) reuses the buffer with no per-byte work; otherwise
/// each leading ASCII run is copied wholesale and only a byte >= 0x80 expands to
/// the two-byte UTF-8 encoding of the same code point.
fn latin1_to_utf8(raw: Vec<u8>) -> String {
    if raw.is_ascii() {
        return String::from_utf8(raw).expect("ascii is valid utf-8");
    }
    // High bytes each grow by one; reserve a little slack so a profile with few
    // of them doesn't reallocate.
    let mut out = String::with_capacity(raw.len() + raw.len() / 16 + 16);
    let mut rest = &raw[..];
    while !rest.is_empty() {
        let split = rest.iter().position(|&b| b >= 0x80).unwrap_or(rest.len());
        // SAFETY: the prefix up to the first high byte is ASCII, hence valid UTF-8.
        out.push_str(unsafe { std::str::from_utf8_unchecked(&rest[..split]) });
        rest = &rest[split..];
        if let Some((&b, tail)) = rest.split_first() {
            out.push(b as char);
            rest = tail;
        }
    }
    out
}

/// Best-effort parse of a truncated profile: read otherData if present and pull
/// whole events out of the traceEvents array until the first incomplete one.
fn recover(text: &str) -> Result<Profile, LoadError> {
    let events_at = text
        .find("\"traceEvents\"")
        .ok_or_else(|| LoadError::Corrupted("no traceEvents array".into()))?;
    let array_start = text[events_at..]
        .find('[')
        .map(|i| events_at + i + 1)
        .ok_or_else(|| LoadError::Corrupted("traceEvents not an array".into()))?;

    let other_data = recover_other_data(&text[..events_at]);

    let mut events = Vec::new();
    let bytes = text.as_bytes();
    let mut i = array_start;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i] != b'{' {
            if bytes[i] == b']' {
                i = bytes.len();
                break;
            }
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        // Walk the object's braces, skipping any inside string literals, to find
        // its close. A missing close means the array was cut mid-object: stop.
        let start = i;
        let mut depth = 0i32;
        let mut in_str = false;
        let mut escaped = false;
        let mut end = None;
        for (offset, &c) in bytes[start..].iter().enumerate() {
            if in_str {
                match c {
                    _ if escaped => escaped = false,
                    b'\\' => escaped = true,
                    b'"' => in_str = false,
                    _ => {}
                }
            } else {
                match c {
                    b'"' => in_str = true,
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(start + offset);
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        let Some(end) = end else { break };
        if let Ok(event) = serde_json::from_str::<RawEvent>(&text[start..=end]) {
            events.push(event);
        }
        i = end + 1;
    }

    if events.is_empty() {
        return Err(LoadError::Corrupted("no complete events recovered".into()));
    }
    Ok(build(other_data, events))
}

fn recover_other_data(prefix: &str) -> Option<Map<String, Value>> {
    let key = prefix.find("\"otherData\"")?;
    let obj_start = prefix[key..].find('{').map(|i| key + i)?;
    let mut depth = 0i32;
    for (i, c) in prefix[obj_start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let end = obj_start + i + 1;
                    return serde_json::from_str(&prefix[obj_start..end]).ok();
                }
            }
            _ => {}
        }
    }
    None
}

fn build(other_data: Option<Map<String, Value>>, events: Vec<RawEvent>) -> Profile {
    let metadata = build_metadata(other_data.as_ref());

    let mut thread_meta: BTreeMap<i64, (Option<String>, i64)> = BTreeMap::new();
    let mut slices: BTreeMap<i64, Vec<Slice>> = BTreeMap::new();
    let mut phase_markers: Vec<PhaseMarker> = Vec::new();
    let mut counter_order: Vec<String> = Vec::new();
    let mut counters: BTreeMap<String, CounterLane> = BTreeMap::new();

    for ev in &events {
        let ph = ev.ph.as_deref().unwrap_or("");
        match ph {
            "M" => apply_metadata_event(ev, &mut thread_meta),
            "X" | "i" => {
                let ts = round_us(ev.ts);
                if ev.cat.as_deref() == Some(PHASE_MARKER_CATEGORY) {
                    phase_markers.push(PhaseMarker {
                        name: ev.name.clone().unwrap_or_default(),
                        start_us: ts,
                    });
                } else {
                    let tid = ev.tid.unwrap_or(0);
                    let category = ev.cat.clone().unwrap_or_default();
                    let work_class = work_class(&category);
                    slices.entry(tid).or_default().push(Slice {
                        name: ev.name.clone().unwrap_or_default(),
                        category,
                        start_us: ts,
                        dur_us: round_us(ev.dur),
                        depth: 0,
                        args: build_slice_args(ev.args.as_ref()),
                        work_class,
                    });
                }
            }
            "C" => apply_counter_event(ev, &mut counter_order, &mut counters),
            _ => {}
        }
    }

    let span = compute_span(&slices, &counters, &phase_markers);

    let mut threads: Vec<ThreadTrack> = slices
        .into_iter()
        .map(|(tid, mut tslices)| {
            let max_depth = assign_depths(&mut tslices);
            let (name, sort_index) = match thread_meta.get(&tid) {
                Some((Some(n), s)) => (n.clone(), *s),
                Some((None, s)) => (format!("thread {tid}"), *s),
                None => (format!("thread {tid}"), MAX_SORT_INDEX),
            };
            let lane_class = lane_class(tid, &name);
            ThreadTrack {
                tid,
                name,
                sort_index,
                slices: tslices,
                max_depth,
                lane_class,
            }
        })
        .collect();
    threads.sort_by_key(|t| (t.sort_index, t.tid));

    phase_markers.sort_by_key(|m| m.start_us);

    let counters = counter_order
        .into_iter()
        .filter_map(|n| counters.remove(&n))
        .collect();

    Profile {
        metadata,
        threads,
        counters,
        phase_markers,
        span,
    }
}

fn build_metadata(other: Option<&Map<String, Value>>) -> Metadata {
    let get = |k: &str| {
        other
            .and_then(|m| m.get(k))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    Metadata {
        bazel_version: get("bazel_version"),
        build_id: get("build_id"),
        output_base: get("output_base"),
        date: get("date"),
    }
}

fn apply_metadata_event(ev: &RawEvent, thread_meta: &mut BTreeMap<i64, (Option<String>, i64)>) {
    let tid = ev.tid.unwrap_or(0);
    let args = ev.args.as_ref();
    let entry = thread_meta.entry(tid).or_insert((None, MAX_SORT_INDEX));
    match ev.name.as_deref() {
        Some("thread_name") => {
            if let Some(name) = args.and_then(|a| a.get_str("name")) {
                entry.0 = Some(name.to_owned());
            }
        }
        Some("thread_sort_index") => {
            if let Some(idx) = args.and_then(|a| a.get_i64("sort_index")) {
                entry.1 = idx;
            }
        }
        _ => {}
    }
}

fn build_slice_args(args: Option<&RawArgs>) -> SliceArgs {
    let mut out = SliceArgs::default();
    let Some(args) = args else {
        return out;
    };
    for (k, v) in &args.entries {
        match (k.as_str(), v) {
            ("target", ArgVal::Str(s)) => out.target = Some(s.clone()),
            ("mnemonic", ArgVal::Str(s)) => out.mnemonic = Some(s.clone()),
            ("configuration", ArgVal::Str(s)) => out.configuration = Some(s.clone()),
            // A non-string value under a recognized key is dropped, as the typed
            // fields are strings; everything else is kept for display.
            ("target" | "mnemonic" | "configuration", _) => {}
            _ => out.others.push((k.clone(), v.to_display())),
        }
    }
    out
}

fn apply_counter_event(
    ev: &RawEvent,
    order: &mut Vec<String>,
    counters: &mut BTreeMap<String, CounterLane>,
) {
    let lane_name = ev.name.clone().unwrap_or_default();
    let ts = round_us(ev.ts);
    let Some(args) = ev.args.as_ref() else {
        return;
    };
    let lane = counters.entry(lane_name.clone()).or_insert_with(|| {
        order.push(lane_name.clone());
        CounterLane {
            name: lane_name.clone(),
            series: Vec::new(),
        }
    });
    for (key, value) in &args.entries {
        let Some(value) = value.as_f64() else {
            continue;
        };
        let series = match lane.series.iter_mut().find(|s| &s.name == key) {
            Some(s) => s,
            None => {
                lane.series.push(CounterSeries {
                    name: key.clone(),
                    points: Vec::new(),
                });
                lane.series.last_mut().unwrap()
            }
        };
        series.points.push(CounterPoint { ts_us: ts, value });
    }
}

/// Implicit nesting from a flat per-thread slice list: sort by start then by
/// longest duration, then assign each slice the depth of its nearest ENCLOSING
/// ancestor on a stack of ancestor end-times. A slice is nested only in a span
/// that contains it, so pop while the stack top ends before the slice does
/// (`end < slice_end`); popping on end-before-start alone would keep merely
/// overlapping spans stacked — a span that starts inside another but ends after
/// it is a sibling, not a child — building a runaway staircase of false depth.
fn assign_depths(slices: &mut [Slice]) -> u32 {
    slices.sort_by(|a, b| a.start_us.cmp(&b.start_us).then(b.dur_us.cmp(&a.dur_us)));
    let mut stack: Vec<i64> = Vec::new();
    let mut max_depth = 0u32;
    for slice in slices.iter_mut() {
        let slice_end = slice.start_us + slice.dur_us;
        while stack.last().is_some_and(|&end| end < slice_end) {
            stack.pop();
        }
        slice.depth = stack.len() as u32;
        max_depth = max_depth.max(slice.depth);
        stack.push(slice_end);
    }
    if slices.is_empty() { 0 } else { max_depth + 1 }
}

fn compute_span(
    slices: &BTreeMap<i64, Vec<Slice>>,
    counters: &BTreeMap<String, CounterLane>,
    markers: &[PhaseMarker],
) -> TimeSpan {
    let mut start: Option<i64> = None;
    let mut end: Option<i64> = None;
    let mut note = |s: i64, e: i64| {
        start = Some(start.map_or(s, |cur| cur.min(s)));
        end = Some(end.map_or(e, |cur| cur.max(e)));
    };
    for tslices in slices.values() {
        for sl in tslices {
            note(sl.start_us, sl.start_us + sl.dur_us);
        }
    }
    for lane in counters.values() {
        for series in &lane.series {
            for p in &series.points {
                note(p.ts_us, p.ts_us);
            }
        }
    }
    for m in markers {
        note(m.start_us, m.start_us);
    }
    TimeSpan {
        start_us: start.unwrap_or(0),
        end_us: end.unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn parse(json: &str) -> Profile {
        Profile::from_bytes(json.as_bytes()).expect("parse")
    }

    #[test]
    fn metadata_and_thread_order() {
        let json = r#"{
            "otherData": {"bazel_version":"7.0.0","build_id":"abc","output_base":"/ob","date":"today"},
            "traceEvents": [
                {"name":"thread_name","ph":"M","tid":5,"args":{"name":"Lane B"}},
                {"name":"thread_sort_index","ph":"M","tid":5,"args":{"sort_index":20}},
                {"name":"thread_name","ph":"M","tid":3,"args":{"name":"Lane A"}},
                {"name":"thread_sort_index","ph":"M","tid":3,"args":{"sort_index":10}},
                {"cat":"c","name":"b","ph":"X","ts":0,"dur":1,"pid":1,"tid":5},
                {"cat":"c","name":"a","ph":"X","ts":0,"dur":1,"pid":1,"tid":3}
            ]
        }"#;
        let p = parse(json);
        assert_eq!(p.metadata.bazel_version.as_deref(), Some("7.0.0"));
        assert_eq!(p.metadata.build_id.as_deref(), Some("abc"));
        assert_eq!(p.metadata.output_base.as_deref(), Some("/ob"));
        assert_eq!(p.metadata.date.as_deref(), Some("today"));
        let names: Vec<_> = p.threads.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["Lane A", "Lane B"]);
    }

    #[test]
    fn nesting_depth() {
        let json = r#"{
            "traceEvents": [
                {"name":"thread_name","ph":"M","tid":1,"args":{"name":"L"}},
                {"cat":"c","name":"parent","ph":"X","ts":0,"dur":100,"pid":1,"tid":1},
                {"cat":"c","name":"child1","ph":"X","ts":0,"dur":40,"pid":1,"tid":1},
                {"cat":"c","name":"child2","ph":"X","ts":50,"dur":40,"pid":1,"tid":1}
            ]
        }"#;
        let p = parse(json);
        let t = &p.threads[0];
        let depth = |name: &str| t.slices.iter().find(|s| s.name == name).unwrap().depth;
        assert_eq!(depth("parent"), 0);
        assert_eq!(depth("child1"), 1);
        assert_eq!(depth("child2"), 1);
        assert_eq!(t.max_depth, 2);
    }

    #[test]
    fn phase_markers_are_not_slices() {
        let json = r#"{
            "traceEvents": [
                {"name":"thread_name","ph":"M","tid":1,"args":{"name":"L"}},
                {"cat":"build phase marker","name":"Launch Blaze","ph":"X","ts":-2463000,"dur":0,"pid":1,"tid":1},
                {"cat":"build phase marker","name":"Loading","ph":"i","ts":5,"pid":1,"tid":1},
                {"cat":"c","name":"real","ph":"X","ts":10,"dur":5,"pid":1,"tid":1}
            ]
        }"#;
        let p = parse(json);
        let marker_names: Vec<_> = p.phase_markers.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(marker_names, vec!["Launch Blaze", "Loading"]);
        assert_eq!(p.phase_markers[0].start_us, -2463000);
        let slice_names: Vec<_> = p.threads[0]
            .slices
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(slice_names, vec!["real"]);
    }

    #[test]
    fn critical_path_stays_on_tid_zero() {
        let json = r#"{
            "traceEvents": [
                {"name":"thread_name","ph":"M","tid":0,"args":{"name":"Critical Path"}},
                {"cat":"critical path component","name":"step","ph":"X","ts":0,"dur":3,"pid":1,"tid":0,"args":{"tid":7}}
            ]
        }"#;
        let p = parse(json);
        let crit = p.threads.iter().find(|t| t.tid == 0).expect("tid 0 lane");
        assert_eq!(crit.name, "Critical Path");
        assert_eq!(crit.slices.len(), 1);
        assert!(!p.threads.iter().any(|t| t.tid == 7));
    }

    #[test]
    fn counters_lanes_and_skipped_keys() {
        let json = r#"{
            "traceEvents": [
                {"name":"Usage","ph":"C","ts":0,"pid":1,"tid":1,"args":{"cpu":1.0}},
                {"name":"Usage","ph":"C","ts":10,"pid":1,"tid":1,"args":{"cpu":2.0,"mem":5.0}}
            ]
        }"#;
        let p = parse(json);
        assert_eq!(p.counters.len(), 1);
        let lane = &p.counters[0];
        assert_eq!(lane.name, "Usage");
        let cpu = lane.series.iter().find(|s| s.name == "cpu").unwrap();
        assert_eq!(cpu.points.len(), 2);
        let mem = lane.series.iter().find(|s| s.name == "mem").unwrap();
        assert_eq!(mem.points.len(), 1);
        assert_eq!(mem.points[0].ts_us, 10);
        assert_eq!(mem.points[0].value, 5.0);
    }

    #[test]
    fn span_covers_negative_and_max() {
        let json = r#"{
            "traceEvents": [
                {"cat":"build phase marker","name":"Launch","ph":"X","ts":-2463000,"dur":0,"pid":1,"tid":1},
                {"cat":"c","name":"x","ph":"X","ts":100,"dur":900,"pid":1,"tid":1},
                {"name":"Usage","ph":"C","ts":2000,"pid":1,"tid":1,"args":{"cpu":1.0}}
            ]
        }"#;
        let p = parse(json);
        assert_eq!(p.span.start_us, -2463000);
        assert_eq!(p.span.end_us, 2000);
    }

    #[test]
    fn gzip_round_trip() {
        let json = r#"{
            "otherData": {"bazel_version":"7.0.0"},
            "traceEvents": [
                {"name":"thread_name","ph":"M","tid":1,"args":{"name":"L"}},
                {"cat":"c","name":"a","ph":"X","ts":0,"dur":1,"pid":1,"tid":1}
            ]
        }"#;
        let plain = Profile::from_bytes(json.as_bytes()).expect("plain");
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(json.as_bytes()).unwrap();
        let gz = enc.finish().unwrap();
        let from_gz = Profile::from_bytes(&gz).expect("gz");
        assert_eq!(plain, from_gz);
    }

    #[test]
    fn truncation_recovery() {
        let json = r#"{
            "otherData": {"bazel_version":"7.0.0"},
            "traceEvents": [
                {"name":"thread_name","ph":"M","tid":1,"args":{"name":"L"}},
                {"cat":"c","name":"a","ph":"X","ts":0,"dur":1,"pid":1,"tid":1},
                {"cat":"c","name":"b","ph":"X","ts":2,"dur":1,"pid":1,"tid":1},
                {"cat":"c","name":"trunca"#;
        let p = Profile::from_bytes(json.as_bytes()).expect("recover");
        assert_eq!(p.metadata.bazel_version.as_deref(), Some("7.0.0"));
        let names: Vec<_> = p.threads[0]
            .slices
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn latin1_to_utf8_transcodes_high_bytes() {
        // Pure ASCII passes through unchanged (the moved-buffer fast path).
        assert_eq!(latin1_to_utf8(b"Az0".to_vec()), "Az0");
        // 0xe9 is Latin-1 'é', two bytes in UTF-8; surrounding ASCII runs are kept.
        assert_eq!(latin1_to_utf8(vec![b'A', 0xe9, b'z']), "Aéz");
        // Consecutive leading high bytes (0xe9='é', 0xe8='è') then ASCII, and a
        // trailing high byte (0xff='ÿ').
        assert_eq!(latin1_to_utf8(vec![0xe9, 0xe8, b'x']), "éèx");
        assert_eq!(latin1_to_utf8(vec![b'x', 0xff]), "xÿ");
    }

    #[test]
    fn from_path_reads_gzipped_profile() {
        let json = r#"{
            "otherData": {"bazel_version":"7.0.0"},
            "traceEvents": [
                {"name":"thread_name","ph":"M","tid":1,"args":{"name":"L"}},
                {"cat":"c","name":"a","ph":"X","ts":0,"dur":1,"pid":1,"tid":1}
            ]
        }"#;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(json.as_bytes()).unwrap();
        let gz = enc.finish().unwrap();
        let dir = std::env::var("TEST_TMPDIR").unwrap_or_else(|_| "/tmp".into());
        let path = std::path::Path::new(&dir).join("from_path_streams.profile.gz");
        std::fs::write(&path, &gz).unwrap();
        assert_eq!(
            Profile::from_path(&path).expect("stream"),
            Profile::from_bytes(json.as_bytes()).expect("bytes"),
        );
    }
}
