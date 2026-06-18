use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use flate2::read::MultiGzDecoder;
use serde::Deserialize;
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
    args: Option<Map<String, Value>>,
}

fn round_us(v: Option<f64>) -> i64 {
    v.map(|x| x.round() as i64).unwrap_or(0)
}

impl Profile {
    /// Streams the profile from disk, decompressing and transcoding to UTF-8 on
    /// the fly so a large trace is never decoded into memory whole. A parse
    /// failure — most often a profile truncated by an interrupted Bazel — falls
    /// back to a full read and the best-effort [`recover`].
    pub fn from_path(path: &Path) -> Result<Profile, LoadError> {
        let mut buffered = BufReader::new(File::open(path).map_err(LoadError::Io)?);
        let gzipped = buffered
            .fill_buf()
            .map_err(LoadError::Io)?
            .starts_with(&GZIP_MAGIC);
        let decompressed: Box<dyn Read> = if gzipped {
            Box::new(MultiGzDecoder::new(buffered))
        } else {
            Box::new(buffered)
        };
        match serde_json::from_reader::<_, RawProfile>(Latin1Reader::new(decompressed)) {
            Ok(raw) => Ok(build(raw.other_data, raw.trace_events)),
            Err(_) => recover(&decode(&std::fs::read(path).map_err(LoadError::Io)?)?),
        }
    }

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
    Ok(raw.iter().map(|&b| b as char).collect())
}

/// Transcodes an ISO-8859-1 (Latin-1) byte stream — how Bazel writes profile
/// strings — to UTF-8 as it is read, so the JSON reader consumes valid UTF-8
/// without the whole input being decoded up front. Each byte >= 0x80 expands to
/// the two-byte UTF-8 encoding of the same code point.
struct Latin1Reader<R> {
    inner: R,
    buf: Vec<u8>,
    pos: usize,
}

impl<R: Read> Latin1Reader<R> {
    fn new(inner: R) -> Latin1Reader<R> {
        Latin1Reader {
            inner,
            buf: Vec::new(),
            pos: 0,
        }
    }
}

impl<R: Read> Read for Latin1Reader<R> {
    fn read(&mut self, dst: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.buf.len() {
            let mut chunk = [0u8; 8192];
            let n = self.inner.read(&mut chunk)?;
            if n == 0 {
                return Ok(0);
            }
            self.buf.clear();
            self.pos = 0;
            for &b in &chunk[..n] {
                if b < 0x80 {
                    self.buf.push(b);
                } else {
                    self.buf.push(0xc0 | (b >> 6));
                    self.buf.push(0x80 | (b & 0x3f));
                }
            }
        }
        let n = (self.buf.len() - self.pos).min(dst.len());
        dst[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
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
            if let Some(name) = args.and_then(|a| a.get("name")).and_then(Value::as_str) {
                entry.0 = Some(name.to_owned());
            }
        }
        Some("thread_sort_index") => {
            if let Some(idx) = args
                .and_then(|a| a.get("sort_index"))
                .and_then(Value::as_i64)
            {
                entry.1 = idx;
            }
        }
        _ => {}
    }
}

fn build_slice_args(args: Option<&Map<String, Value>>) -> SliceArgs {
    let mut out = SliceArgs::default();
    let Some(args) = args else {
        return out;
    };
    let take_str = |v: &Value| v.as_str().map(str::to_owned);
    for (k, v) in args {
        match k.as_str() {
            "target" => out.target = take_str(v),
            "mnemonic" => out.mnemonic = take_str(v),
            "configuration" => out.configuration = take_str(v),
            _ => {
                if let Some(s) = scalar_to_string(v) {
                    out.others.push((k.clone(), s));
                }
            }
        }
    }
    out
}

fn scalar_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
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
    for (key, value) in args {
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
/// longest duration, then track ancestor end-times on a stack.
fn assign_depths(slices: &mut [Slice]) -> u32 {
    slices.sort_by(|a, b| a.start_us.cmp(&b.start_us).then(b.dur_us.cmp(&a.dur_us)));
    let mut stack: Vec<i64> = Vec::new();
    let mut max_depth = 0u32;
    for slice in slices.iter_mut() {
        while stack.last().is_some_and(|&end| end <= slice.start_us) {
            stack.pop();
        }
        slice.depth = stack.len() as u32;
        max_depth = max_depth.max(slice.depth);
        stack.push(slice.start_us + slice.dur_us);
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
    fn latin1_reader_transcodes_to_utf8() {
        // 0xe9 is Latin-1 'é', which is two bytes in UTF-8.
        let input: &[u8] = &[b'A', 0xe9, b'z'];
        let mut out = Vec::new();
        Latin1Reader::new(input).read_to_end(&mut out).unwrap();
        assert_eq!(out, "Aéz".as_bytes());
    }

    #[test]
    fn from_path_streams_gzipped_profile() {
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
