use std::fmt;
use std::io::{BufReader, Seek, SeekFrom};
use std::path::Path;

use serde::de::DeserializeSeed;
use serde_json::{Map, Value};

use crate::concurrent_interner::{ConcurrentInterner, Writer};
use crate::model::Profile;

mod build;
mod decode;
mod pipeline;
mod scanner;
mod serde_events;

use build::{seal, Builder, Merge};
use decode::{decode, gzip_isize_from_file};
use pipeline::{
    find_trace_events_array, parse_gz_pipelined, parse_gz_pipelined_reader, parse_parallel,
    TraceEventsArray,
};
use scanner::object_end;
use serde_events::{ArgEntry, ProfileVisitor, RawEventSeed};

/// Estimated distinct-string density of a Bazel profile — about one interned
/// string per this many decoded bytes — used to pre-size the shared interner's
/// dedup index. Measured near one distinct string per 167 bytes on a large
/// profile; rounding down errs toward a larger index, which only wastes a
/// transient, freed-at-seal allocation, whereas over-counting the bytes
/// under-sizes the shard tables and costs a rehash.
const BYTES_PER_DISTINCT: usize = 160;

const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

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

/// Streams the top-level profile object straight into a [`Builder`]: each
/// `traceEvents` element is interned into the arena as it is parsed, so no
/// intermediate `Vec<RawEvent>` is materialized and each event's strings are
/// visited once. `otherData` may appear before or after the array; it is
/// independent of the events, so it is folded in whenever it is seen.
fn parse_streaming(text: &str) -> Result<Profile, serde_json::Error> {
    let mut de = serde_json::Deserializer::from_str(text);
    let profile =
        serde::Deserializer::deserialize_map(&mut de, ProfileVisitor { text_len: text.len() })?;
    de.end()?;
    Ok(profile)
}

/// Wall-clock timing of a load stage, printed to stderr when `BVIZ_TRACE_TIMING`
/// is set. Returns the closure's value so a stage can be wrapped inline.
fn timed<T>(label: &str, f: impl FnOnce() -> T) -> T {
    if std::env::var_os("BVIZ_TRACE_TIMING").is_none() {
        return f();
    }
    let start = std::time::Instant::now();
    let out = f();
    eprintln!("[timing] {label}: {:.3}s", start.elapsed().as_secs_f64());
    out
}

impl Profile {
    /// Loads a profile from a file, streaming the pipelined gzip decode directly
    /// from disk so the file read overlaps decompression and parsing (see
    /// [`parse_gz_pipelined`]). The uncompressed size the pipeline needs is read
    /// from the gzip ISIZE trailer without pulling the whole file in. Anything
    /// the pipeline declines — plain JSON, a small or oddly shaped stream, a
    /// decode or parse failure — reads the whole file and goes through
    /// [`from_bytes`], which owns the whole-buffer fallback chain.
    pub fn from_path(path: &Path) -> Result<Profile, LoadError> {
        if let Ok(mut file) = std::fs::File::open(path) {
            if let Ok(Some(decoded_size)) = gzip_isize_from_file(&mut file) {
                if file.seek(SeekFrom::Start(0)).is_ok() {
                    if let Some(profile) = timed("decode+parse (pipelined, streamed)", || {
                        parse_gz_pipelined_reader(BufReader::new(&mut file), decoded_size)
                    }) {
                        return Ok(profile);
                    }
                }
            }
        }
        let bytes = timed("read file", || std::fs::read(path).map_err(LoadError::Io))?;
        Profile::from_bytes(&bytes)
    }

    /// Parses a profile, preferring the pipelined gzip path (see
    /// [`parse_gz_pipelined`]) that overlaps decompression with the parallel
    /// parse. Anything the pipeline declines — plain JSON, a small or oddly
    /// shaped stream, a decode or parse failure — decodes whole into memory and
    /// parses that text: the parser works over a slice (`SliceRead`), far faster
    /// than the byte-at-a-time `from_reader` (`IoRead`) on a trace of millions of
    /// events, and a profile compresses small enough that the decoded text fits
    /// comfortably. A parse failure — most often a profile truncated by an
    /// interrupted Bazel — falls back to the best-effort [`recover`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Profile, LoadError> {
        if bytes.starts_with(&GZIP_MAGIC) {
            if let Some(profile) = timed("decode+parse (pipelined)", || parse_gz_pipelined(bytes))
            {
                return Ok(profile);
            }
        }
        let decoded = timed("decode (decompress+transcode)", || decode(bytes))?;
        if let Some(profile) = timed("parse+build (parallel)", || parse_parallel(&decoded)) {
            return Ok(profile);
        }
        match timed("parse+build (serial)", || parse_streaming(&decoded)) {
            Ok(profile) => Ok(profile),
            Err(_) => recover(&decoded),
        }
    }
}

/// Best-effort parse of a truncated profile: read otherData if present and pull
/// whole events out of the traceEvents array until the first incomplete one.
fn recover(text: &str) -> Result<Profile, LoadError> {
    let (events_at, array_start) = match find_trace_events_array(text.as_bytes()) {
        TraceEventsArray::Found { key_at, content_start } => (key_at, content_start),
        TraceEventsArray::NoKey => {
            return Err(LoadError::Corrupted("no traceEvents array".into()))
        }
        TraceEventsArray::NoArray => {
            return Err(LoadError::Corrupted("traceEvents not an array".into()))
        }
    };

    let other_data = recover_other_data(&text[..events_at]);

    let interner = ConcurrentInterner::new(text.len(), text.len() / BYTES_PER_DISTINCT);
    let mut builder = Builder::new();
    let recovered = {
        let mut writer = interner.writer();
        recover_events(text, array_start, &mut builder, &mut writer)
    };

    if !recovered {
        return Err(LoadError::Corrupted("no complete events recovered".into()));
    }
    builder.seal_slices();
    let mut merge = Merge::new();
    merge.fold(builder);
    Ok(seal(interner, merge, other_data))
}

/// Folds whole event objects from the traceEvents array into `builder`, starting
/// at `array_start`, until the array closes or an object is cut off mid-way.
/// Returns whether any complete event was recovered.
fn recover_events(
    text: &str,
    array_start: usize,
    builder: &mut Builder,
    writer: &mut Writer,
) -> bool {
    let mut recovered = false;
    let mut scratch: Vec<ArgEntry> = Vec::new();
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
        // A missing close means the array was cut mid-object: stop.
        let start = i;
        let Some(end) = object_end(bytes, start) else { break };
        let mut de = serde_json::Deserializer::from_str(&text[start..=end]);
        if let Ok(ev) = RawEventSeed(&mut scratch)
            .deserialize(&mut de)
            .and_then(|ev| de.end().map(|()| ev))
        {
            builder.event(writer, ev, &scratch);
            recovered = true;
        }
        i = end + 1;
    }
    recovered
}

fn recover_other_data(prefix: &str) -> Option<Map<String, Value>> {
    let key = prefix.find("\"otherData\"")?;
    let obj_start = prefix[key..].find('{').map(|i| key + i)?;
    let end = object_end(prefix.as_bytes(), obj_start)?;
    serde_json::from_str(&prefix[obj_start..=end]).ok()
}

#[cfg(test)]
use crate::model::{Metadata, PhaseMarker, ProfileParts, ThreadTrack, TimeSpan};

/// Seals an interner and the given top-level vectors into a profile with empty
/// slice/counter pools, for model tests that need only phase markers and threads.
#[cfg(test)]
pub(crate) fn build_for_test(
    interner: ConcurrentInterner,
    phase_markers: Vec<PhaseMarker>,
    span: TimeSpan,
    threads: Vec<ThreadTrack>,
) -> Profile {
    let distinct_strings = interner.distinct();
    Profile::from_parts(ProfileParts {
        text: interner.into_text(),
        slice_pool: Vec::new(),
        detail_pool: Vec::new(),
        end_max_pool: Vec::new(),
        args_pool: Vec::new(),
        series_pool: Vec::new(),
        point_pool: Vec::new(),
        metadata: Metadata::default(),
        threads,
        counters: Vec::new(),
        phase_markers,
        span,
        distinct_strings,
    })
}

#[cfg(test)]
mod tests {
    use super::decode::latin1_to_utf8;
    use super::pipeline::{parse_chunks, parse_gz_stream};
    use super::scanner::{find_special, scan_event};
    use super::serde_events::{arg_i64, arg_str};
    use super::*;
    use std::io::Write;

    fn parse(json: &str) -> Profile {
        Profile::from_bytes(json.as_bytes()).expect("parse")
    }

    #[test]
    fn escaped_strings_resolve_correctly() {
        // Escaped JSON strings reach the parser as owned values rather than
        // slices borrowed from the input, exercising a distinct interning
        // path. The \u escape keeps the raw bytes ASCII, which the Latin-1
        // transcode passes through untouched.
        let json = r#"{
            "traceEvents": [
                {"name":"thread_name","ph":"M","tid":1,"args":{"name":"L"}},
                {"cat":"c","name":"quote \" caf\u00e9","ph":"X","ts":0,"dur":1,"tid":1,"args":{"target":"//a:b\"c","note":"tab\there","ke\u0079 2":"v"}}
            ]
        }"#;
        let p = parse(json);
        let t = &p.threads()[0];
        let s = &p.slices(t)[0];
        let d = &p.details(t)[0];
        assert_eq!(p.str(s.name), "quote \" café");
        assert_eq!(p.opt_str(d.target), Some("//a:b\"c"));
        let others: Vec<_> = p
            .slice_others(d)
            .iter()
            .map(|&(k, v)| (p.str(k), p.str(v)))
            .collect();
        // The second key arrives escaped, exercising the owned-key path.
        assert_eq!(others, vec![("note", "tab\there"), ("key 2", "v")]);
    }

    #[test]
    fn negative_duration_clamps_to_zero() {
        let json = r#"{
            "traceEvents": [
                {"ph":"X","cat":"c","name":"broken","ts":100,"dur":-40,"tid":1}
            ]
        }"#;
        let p = parse(json);
        let t = &p.threads()[0];
        assert_eq!(p.slices(t)[0].dur_us, 0);
        assert_eq!(p.end_maxes(t), [100]);
    }

    #[test]
    fn null_fields_read_as_absent() {
        let json = r#"{
            "traceEvents": [
                {"ph":"X","cat":null,"name":null,"ts":0,"dur":1,"tid":1,"args":null}
            ]
        }"#;
        let p = parse(json);
        let t = &p.threads()[0];
        assert_eq!(p.slices(t).len(), 1);
        let s = &p.slices(t)[0];
        assert_eq!(p.str(s.name), "");
        assert_eq!(p.str(p.details(t)[0].category), "");
        assert!(p.slice_others(&p.details(t)[0]).is_empty());
    }

    /// A canonical, interning-order-independent dump of everything a profile
    /// carries, so two profiles built by different paths can be compared by
    /// meaning rather than by `Str` offsets (which depend on intern order).
    fn semantic(p: &Profile) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        let m = p.metadata();
        writeln!(
            out,
            "meta {:?} {:?} {:?} {:?}",
            p.opt_str(m.bazel_version),
            p.opt_str(m.build_id),
            p.opt_str(m.output_base),
            p.opt_str(m.date)
        )
        .unwrap();
        writeln!(out, "span {} {}", p.span().start_us, p.span().end_us).unwrap();
        for t in p.threads() {
            writeln!(
                out,
                "thread tid={} name={:?} sort={} depth={} class={:?}",
                t.tid,
                p.str(t.name),
                t.sort_index,
                t.max_depth,
                t.lane_class
            )
            .unwrap();
            for (s, d) in p.slices(t).iter().zip(p.details(t)) {
                writeln!(
                    out,
                    "  slice {:?} cat={:?} ts={} dur={} depth={} role={:?} tgt={:?} mn={:?} cfg={:?}",
                    p.str(s.name),
                    p.str(d.category),
                    s.start_us,
                    s.dur_us,
                    s.depth,
                    s.role,
                    p.opt_str(d.target),
                    p.opt_str(d.mnemonic),
                    p.opt_str(d.configuration),
                )
                .unwrap();
            }
        }
        for marker in p.phase_markers() {
            writeln!(out, "marker {:?} {}", p.str(marker.name), marker.start_us).unwrap();
        }
        for lane in p.counters() {
            writeln!(out, "counter {:?}", p.str(lane.name)).unwrap();
            for series in p.series(lane) {
                let pts: Vec<_> = p.points(series).iter().map(|pt| (pt.ts_us, pt.value)).collect();
                writeln!(out, "  series {:?} {:?}", p.str(series.name), pts).unwrap();
            }
        }
        out
    }

    /// One event per line, several threads, nested slices, a phase marker, and
    /// a counter sampled at two times — so chunking at newlines splits a
    /// thread's slices and a counter's points across chunks, exercising merge.
    fn chunkable_profile_json() -> String {
        "{\"otherData\":{\"bazel_version\":\"9.0.0\"},\"traceEvents\":[\n".to_string()
            + "{\"name\":\"thread_name\",\"ph\":\"M\",\"tid\":1,\"args\":{\"name\":\"Lane A\"}},\n"
            + "{\"name\":\"thread_sort_index\",\"ph\":\"M\",\"tid\":1,\"args\":{\"sort_index\":10}},\n"
            + "{\"cat\":\"skyframe evaluator\",\"name\":\"parent\",\"ph\":\"X\",\"ts\":0,\"dur\":100,\"tid\":1,\"args\":{\"mnemonic\":\"CppCompile\",\"target\":\"//a:b\"}},\n"
            + "{\"cat\":\"skyframe evaluator\",\"name\":\"child\",\"ph\":\"X\",\"ts\":0,\"dur\":40,\"tid\":1},\n"
            + "{\"name\":\"Usage\",\"ph\":\"C\",\"ts\":0,\"tid\":1,\"args\":{\"cpu\":1.0}},\n"
            + "{\"cat\":\"build phase marker\",\"name\":\"Analyzing\",\"ph\":\"i\",\"ts\":5,\"tid\":1},\n"
            + "{\"name\":\"thread_name\",\"ph\":\"M\",\"tid\":2,\"args\":{\"name\":\"Lane B\"}},\n"
            + "{\"cat\":\"remote action execution\",\"name\":\"work\",\"ph\":\"X\",\"ts\":10,\"dur\":80,\"tid\":2},\n"
            + "{\"name\":\"Usage\",\"ph\":\"C\",\"ts\":20,\"tid\":1,\"args\":{\"cpu\":2.0}},\n"
            + "{\"cat\":\"local action execution\",\"name\":\"more\",\"ph\":\"X\",\"ts\":30,\"dur\":10,\"tid\":2}\n"
            + "]}"
    }

    #[test]
    fn parallel_matches_serial() {
        let json = chunkable_profile_json();
        let serial = parse_streaming(&json).expect("serial");
        // Force four chunks regardless of size so the merge path is exercised.
        let parallel = parse_chunks(&json, 4, 0).expect("parallel");
        assert_eq!(semantic(&serial), semantic(&parallel));
    }

    /// Constructs the chunked path can only take through the scanner's serde
    /// fallback (floats, `\uXXXX` escapes, huge magnitudes) mixed with
    /// fast-path constructs (nulls, duplicate keys, ignored composites, arg
    /// scalars of every kept shape), so the equality check covers both routes
    /// and their interleaving.
    fn scanner_edge_case_json() -> String {
        "{\"traceEvents\":[\n".to_string()
            + "{\"name\":\"thread_name\",\"ph\":\"M\",\"tid\":2,\"args\":{\"name\":\"L\\u0061ne \\\"B\\\"\"}},\n"
            // serde reads -0 as the float -0.0, which arg_i64 must not treat
            // as sort index 0.
            + "{\"name\":\"thread_sort_index\",\"ph\":\"M\",\"tid\":2,\"args\":{\"sort_index\":-0}},\n"
            + "{\"cat\":\"c\",\"name\":\"caf\\u00e9 \\\"x\\\"\",\"ph\":\"X\",\"ts\":1.5,\"dur\":2.5,\"tid\":1},\n"
            + "{\"cat\":null,\"name\":\"n1\",\"ph\":\"X\",\"ts\":0,\"dur\":1,\"tid\":2,\"args\":null},\n"
            + "{\"name\":\"first\",\"name\":\"second\",\"ph\":\"X\",\"ts\":3,\"dur\":1,\"tid\":2,\"args\":{\"a\":\"x\"},\"args\":{\"b\":\"y\"}},\n"
            + "{\"pid\":7,\"weird\":[1,{\"q\":\"]\"},\"s\\\"t\"],\"other\":{\"deep\":[true,null]},\"name\":\"u1\",\"ph\":\"X\",\"ts\":4,\"dur\":1,\"tid\":2},\n"
            + "{\"name\":\"argsy\",\"ph\":\"X\",\"ts\":5,\"dur\":1,\"tid\":2,\"args\":{\"s\":\"v\",\"i\":-3,\"big\":9223372036854775808,\"b\":true,\"nb\":false,\"z\":null,\"arr\":[1,2],\"obj\":{\"k\":\"v\"},\"esc\":\"a\\tb\"}},\n"
            + "{\"name\":\"floatarg\",\"ph\":\"X\",\"ts\":5,\"dur\":1,\"tid\":2,\"args\":{\"f\":2.75}},\n"
            // The saturated i64::MAX start plus a positive dur exercises the
            // depth pass's saturating end computation.
            + "{\"name\":\"big\",\"ph\":\"X\",\"ts\":92233720368547758070,\"dur\":1,\"tid\":2},\n"
            + "{\"name\":\"neg\",\"ph\":\"X\",\"ts\":6,\"dur\":1,\"tid\":-2},\n"
            + "{ \"name\" : \"ws\" , \"ph\" : \"X\" , \"ts\" : 7 , \"dur\" : 1 , \"tid\" : 2 },\n"
            + "{\"name\":\"Empty\",\"ph\":\"C\",\"ts\":8,\"tid\":1,\"args\":{}},\n"
            + "{\"name\":\"Usage\",\"ph\":\"C\",\"ts\":9,\"tid\":1,\"args\":{\"cpu\":0.5}},\n"
            + "{}\n"
            + "]}"
    }

    #[test]
    fn scanner_edge_cases_match_serial() {
        let json = scanner_edge_case_json();
        let serial = parse_streaming(&json).expect("serial");
        let parallel = parse_chunks(&json, 4, 0).expect("parallel");
        assert_eq!(semantic(&serial), semantic(&parallel));
    }

    #[test]
    fn scan_event_reads_a_typical_event() {
        let json = r#"{"cat":"skyframe evaluator","name":"a b","ph":"X","ts":10,"dur":20,"tid":3,"args":{"target":"//a:b","n":4}} ,"#;
        let mut scratch = Vec::new();
        let (ev, end) = scan_event(json, 0, &mut scratch).expect("fast path");
        assert_eq!(&json[end..], " ,");
        assert_eq!(ev.cat.as_deref(), Some("skyframe evaluator"));
        assert_eq!(ev.name.as_deref(), Some("a b"));
        assert_eq!(ev.ph.as_deref(), Some("X"));
        assert_eq!(ev.ts, Some(10.0));
        assert_eq!(ev.dur, Some(20.0));
        assert_eq!(ev.tid, Some(3));
        assert!(ev.has_args);
        assert_eq!(scratch.len(), 2);
        assert_eq!(arg_str(&scratch, "target"), Some("//a:b"));
        assert_eq!(arg_i64(&scratch, "n"), Some(4));
    }

    #[test]
    fn scan_event_unescapes_and_defers() {
        let mut scratch = Vec::new();
        let (ev, _) = scan_event(r#"{"name":"q\"\t\\z"}"#, 0, &mut scratch).expect("escapes");
        assert_eq!(ev.name.as_deref(), Some("q\"\t\\z"));
        // Constructs whose acceptance or value must come from serde defer.
        for deferred in [
            r#"{"ts":1.5}"#,
            r#"{"ts":1e3}"#,
            r#"{"ts":01}"#,
            r#"{"name":"caf\u00e9"}"#,
            r#"{"tid":9223372036854775808}"#,
            r#"{"tid":-0}"#,
            r#"{"ph":7}"#,
            r#"{"args":[1]}"#,
        ] {
            assert!(scan_event(deferred, 0, &mut scratch).is_none(), "{deferred}");
        }
    }

    #[test]
    fn find_special_matches_a_naive_scan() {
        let mut bytes: Vec<u8> = Vec::new();
        for i in 0..64u8 {
            bytes.extend_from_slice(b"plain text ");
            bytes.push(match i % 5 {
                0 => b'"',
                1 => b'\\',
                2 => 0x1f,
                3 => 0xc3, // a multibyte UTF-8 lead, never special
                _ => b'x',
            });
        }
        for start in 0..bytes.len() {
            let naive =
                (start..bytes.len()).find(|&j| matches!(bytes[j], b'"' | b'\\' | 0..=0x1f));
            assert_eq!(find_special(&bytes, start), naive, "from {start}");
        }
        assert_eq!(find_special(b"no specials at all", 0), None);
    }

    #[test]
    fn pipelined_matches_serial() {
        let json = chunkable_profile_json();
        let serial = parse_streaming(&json).expect("serial");
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(json.as_bytes()).unwrap();
        let gz = enc.finish().unwrap();
        // Chunks far smaller than an event line force many pieces, boundary
        // carries, and the no-newline buffer doubling.
        let pipelined = parse_gz_stream(&gz, 4, 64).expect("pipelined");
        assert_eq!(semantic(&serial), semantic(&pipelined));
    }

    #[test]
    fn merge_keeps_event_order_for_equal_keys() {
        // One thread with several slices sharing an identical (ts, dur) key,
        // interleaved with distinct ones and spread across chunks. Slices with
        // equal keys keep event order in the lane, so every parse path must
        // produce the serial path's order.
        let json = "{\"traceEvents\":[\n".to_string()
            + "{\"name\":\"thread_name\",\"ph\":\"M\",\"tid\":1,\"args\":{\"name\":\"L\"}},\n"
            + "{\"cat\":\"c\",\"name\":\"outer\",\"ph\":\"X\",\"ts\":0,\"dur\":100,\"tid\":1},\n"
            + "{\"cat\":\"c\",\"name\":\"tie1\",\"ph\":\"X\",\"ts\":10,\"dur\":5,\"tid\":1},\n"
            + "{\"cat\":\"c\",\"name\":\"tie2\",\"ph\":\"X\",\"ts\":10,\"dur\":5,\"tid\":1},\n"
            + "{\"cat\":\"c\",\"name\":\"mid\",\"ph\":\"X\",\"ts\":12,\"dur\":2,\"tid\":1},\n"
            + "{\"cat\":\"c\",\"name\":\"tie3\",\"ph\":\"X\",\"ts\":10,\"dur\":5,\"tid\":1},\n"
            + "{\"cat\":\"c\",\"name\":\"tie4\",\"ph\":\"X\",\"ts\":10,\"dur\":5,\"tid\":1},\n"
            + "{\"cat\":\"c\",\"name\":\"tie5\",\"ph\":\"X\",\"ts\":10,\"dur\":5,\"tid\":1},\n"
            + "{\"cat\":\"c\",\"name\":\"last\",\"ph\":\"X\",\"ts\":60,\"dur\":10,\"tid\":1}\n"
            + "]}";
        let serial = parse_streaming(&json).expect("serial");
        let lane = &serial.threads()[0];
        let names: Vec<_> = serial.slices(lane).iter().map(|s| serial.str(s.name)).collect();
        assert_eq!(names, ["outer", "tie1", "tie2", "tie3", "tie4", "tie5", "mid", "last"]);
        // Chunking at newlines splits the ties across several builders.
        let parallel = parse_chunks(&json, 4, 0).expect("parallel");
        assert_eq!(semantic(&serial), semantic(&parallel));
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(json.as_bytes()).unwrap();
        let gz = enc.finish().unwrap();
        let pipelined = parse_gz_stream(&gz, 4, 64).expect("pipelined");
        assert_eq!(semantic(&serial), semantic(&pipelined));
    }

    #[test]
    fn pipelined_reads_trailing_other_data() {
        let json = "{\"traceEvents\":[\n".to_string()
            + "{\"name\":\"thread_name\",\"ph\":\"M\",\"tid\":1,\"args\":{\"name\":\"L\"}},\n"
            + "{\"cat\":\"c\",\"name\":\"a\",\"ph\":\"X\",\"ts\":0,\"dur\":1,\"tid\":1}\n"
            + "],\"otherData\":{\"bazel_version\":\"9.0.0\"}}";
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(json.as_bytes()).unwrap();
        let gz = enc.finish().unwrap();
        let p = parse_gz_stream(&gz, 2, 64).expect("pipelined");
        assert_eq!(p.opt_str(p.metadata().bazel_version), Some("9.0.0"));
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
        assert_eq!(p.opt_str(p.metadata().bazel_version), Some("7.0.0"));
        assert_eq!(p.opt_str(p.metadata().build_id), Some("abc"));
        assert_eq!(p.opt_str(p.metadata().output_base), Some("/ob"));
        assert_eq!(p.opt_str(p.metadata().date), Some("today"));
        let names: Vec<_> = p.threads().iter().map(|t| p.str(t.name)).collect();
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
        let t = &p.threads()[0];
        let depth =
            |name: &str| p.slices(t).iter().find(|s| p.str(s.name) == name).unwrap().depth;
        assert_eq!(depth("parent"), 0);
        assert_eq!(depth("child1"), 1);
        assert_eq!(depth("child2"), 1);
        assert_eq!(t.max_depth, 2);
    }

    #[test]
    fn end_maxes_hold_running_max_and_reset_per_thread() {
        // tid 1: "outer" outlasts the short slices nested after it, so the
        // running max stays flat at outer's end until "late" passes it; tid 2
        // is its own segment, whose max restarts at its first slice's end.
        // One event per line so the chunked paths split the lanes across
        // builders and exercise the k-way merge's end-max accumulation.
        let json = "{\"traceEvents\":[\n".to_string()
            + "{\"cat\":\"c\",\"name\":\"outer\",\"ph\":\"X\",\"ts\":0,\"dur\":100,\"tid\":1},\n"
            + "{\"cat\":\"c\",\"name\":\"a\",\"ph\":\"X\",\"ts\":10,\"dur\":5,\"tid\":1},\n"
            + "{\"cat\":\"c\",\"name\":\"b\",\"ph\":\"X\",\"ts\":30,\"dur\":5,\"tid\":1},\n"
            + "{\"cat\":\"c\",\"name\":\"c\",\"ph\":\"X\",\"ts\":60,\"dur\":10,\"tid\":1},\n"
            + "{\"cat\":\"c\",\"name\":\"late\",\"ph\":\"X\",\"ts\":150,\"dur\":30,\"tid\":1},\n"
            + "{\"cat\":\"c\",\"name\":\"w\",\"ph\":\"X\",\"ts\":5,\"dur\":10,\"tid\":2},\n"
            + "{\"cat\":\"c\",\"name\":\"z\",\"ph\":\"X\",\"ts\":50,\"dur\":5,\"tid\":2}\n"
            + "]}";
        let check = |p: &Profile| {
            let maxes: Vec<Vec<i64>> =
                p.threads().iter().map(|t| p.end_maxes(t).to_vec()).collect();
            assert_eq!(maxes, [vec![100, 100, 100, 100, 180], vec![15, 55]]);
            for t in p.threads() {
                assert_eq!(p.end_maxes(t).len(), p.slices(t).len());
            }
        };
        check(&parse_streaming(&json).expect("serial"));
        check(&parse_chunks(&json, 4, 0).expect("parallel"));
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(json.as_bytes()).unwrap();
        let gz = enc.finish().unwrap();
        check(&parse_gz_stream(&gz, 4, 64).expect("pipelined"));
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
        let marker_names: Vec<_> = p.phase_markers().iter().map(|m| p.str(m.name)).collect();
        assert_eq!(marker_names, vec!["Launch Blaze", "Loading"]);
        assert_eq!(p.phase_markers()[0].start_us, -2463000);
        let slice_names: Vec<_> = p
            .slices(&p.threads()[0])
            .iter()
            .map(|s| p.str(s.name))
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
        let crit = p.threads().iter().find(|t| t.tid == 0).expect("tid 0 lane");
        assert_eq!(p.str(crit.name), "Critical Path");
        assert_eq!(p.slices(crit).len(), 1);
        assert!(!p.threads().iter().any(|t| t.tid == 7));
    }

    #[test]
    fn counters_split_per_series_skipping_non_numeric_keys() {
        let json = r#"{
            "traceEvents": [
                {"name":"Usage","ph":"C","ts":0,"pid":1,"tid":1,"args":{"cpu":1.0}},
                {"name":"Usage","ph":"C","ts":10,"pid":1,"tid":1,"args":{"cpu":2.0,"mem":5.0,"tag":"x"}}
            ]
        }"#;
        let p = parse(json);
        // One lane per numeric series; the non-numeric "tag" arg is dropped. A
        // multi-series counter labels each lane with its arg key.
        let labels: Vec<_> = p.counters().iter().map(|l| p.str(l.name)).collect();
        assert_eq!(labels, ["Usage · cpu", "Usage · mem"]);
        let cpu = &p.counters()[0];
        assert_eq!(p.series(cpu).len(), 1);
        assert_eq!(p.points(&p.series(cpu)[0]).len(), 2);
        assert_eq!(p.series(cpu)[0].max_value, 2.0);
        let mem = &p.counters()[1];
        let mem_points = p.points(&p.series(mem)[0]);
        assert_eq!(mem_points.len(), 1);
        assert_eq!(mem_points[0].ts_us, 10);
        assert_eq!(mem_points[0].value, 5.0);
        assert_eq!(p.series(mem)[0].max_value, 5.0);
    }

    #[test]
    fn counter_points_are_sorted_by_time() {
        let json = r#"{
            "traceEvents": [
                {"name":"Usage","ph":"C","ts":20,"pid":1,"tid":1,"args":{"cpu":3.0}},
                {"name":"Usage","ph":"C","ts":0,"pid":1,"tid":1,"args":{"cpu":1.0}},
                {"name":"Usage","ph":"C","ts":10,"pid":1,"tid":1,"args":{"cpu":2.0}}
            ]
        }"#;
        let p = parse(json);
        let series = &p.series(&p.counters()[0])[0];
        let times: Vec<_> = p.points(series).iter().map(|pt| pt.ts_us).collect();
        assert_eq!(times, [0, 10, 20]);
        assert_eq!(series.max_value, 3.0);
    }

    #[test]
    fn counters_cluster_by_kind_and_keep_empty_lanes() {
        // Emitted out of cluster order, with a worker-memory counter that (as on a
        // build with no persistent workers) carries only empty args.
        let json = r#"{
            "traceEvents": [
                {"name":"Memory usage (Bazel)","ph":"C","ts":0,"pid":1,"tid":1,"args":{"memory":100.0}},
                {"name":"CPU usage (total)","ph":"C","ts":0,"pid":1,"tid":1,"args":{"system cpu":2.0}},
                {"name":"action count","ph":"C","ts":0,"pid":1,"tid":1,"args":{"action":3.0,"local action cache":0.1}},
                {"name":"Total worker memory usage","ph":"C","ts":0,"pid":1,"tid":1,"args":{}},
                {"name":"CPU usage (Bazel)","ph":"C","ts":0,"pid":1,"tid":1,"args":{"cpu":1.0}},
                {"name":"action count (local)","ph":"C","ts":0,"pid":1,"tid":1,"args":{"local action":1.0}},
                {"name":"Memory usage (total)","ph":"C","ts":0,"pid":1,"tid":1,"args":{"system memory":500.0}}
            ]
        }"#;
        let p = parse(json);
        let labels: Vec<_> = p.counters().iter().map(|l| p.str(l.name)).collect();
        assert_eq!(
            labels,
            [
                "action count · action",
                "action count (local)",
                "action count · local action cache",
                "CPU usage (total)",
                "CPU usage (Bazel)",
                "Memory usage (total)",
                "Memory usage (Bazel)",
                "Total worker memory usage",
            ]
        );
        let worker = p.counters().last().unwrap();
        assert!(p.series(worker).is_empty());
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
        assert_eq!(p.span().start_us, -2463000);
        assert_eq!(p.span().end_us, 2000);
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
        assert_eq!(p.opt_str(p.metadata().bazel_version), Some("7.0.0"));
        let names: Vec<_> = p
            .slices(&p.threads()[0])
            .iter()
            .map(|s| p.str(s.name))
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

    #[test]
    fn from_path_rejects_sub_frame_gzip() {
        // A gzip-magic file shorter than a full frame: its last four bytes
        // overlap the header, so read as a trailer they decode to a ~4 GiB bogus
        // ISIZE. from_path must not size the streamed parse from that trailer; it
        // falls back to the whole-buffer path and reports a clean error rather
        // than over-allocating or panicking.
        let dir = std::env::var("TEST_TMPDIR").unwrap_or_else(|_| "/tmp".into());
        let path = std::path::Path::new(&dir).join("sub_frame.profile.gz");
        std::fs::write(&path, [0x1f, 0x8b, 0xff, 0xff, 0xff, 0xff]).unwrap();
        assert!(Profile::from_path(&path).is_err());
    }
}
