use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io::Read;
use std::marker::PhantomData;
use std::path::Path;

use flate2::read::MultiGzDecoder;
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

use crate::model::{
    lane_class, work_class, CounterLane, CounterPoint, CounterSeries, Interner, Metadata,
    PhaseMarker, Profile, ProfileParts, Slice, SliceArgs, Span32, Str, ThreadTrack, TimeSpan,
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

/// Streams the top-level profile object straight into a [`Builder`]: each
/// `traceEvents` element is interned into the arena as it is parsed, so no
/// intermediate `Vec<RawEvent>` is materialized and each event's strings are
/// visited once rather than allocated by the parser and re-interned by a second
/// pass. `otherData` may appear before or after the array; it is independent of
/// the events, so it is folded in whenever it is seen.
fn parse_streaming(text: &str) -> Result<Profile, serde_json::Error> {
    let mut de = serde_json::Deserializer::from_str(text);
    let profile = serde::Deserializer::deserialize_map(&mut de, ProfileVisitor)?;
    de.end()?;
    Ok(profile)
}

struct ProfileVisitor;

impl<'de> Visitor<'de> for ProfileVisitor {
    type Value = Profile;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a trace profile object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Profile, A::Error> {
        let mut builder = Builder::new(0);
        let mut other_data = None;
        while let Some(key) = map.next_key::<Cow<str>>()? {
            match key.as_ref() {
                "otherData" => other_data = map.next_value::<Option<Map<String, Value>>>()?,
                "traceEvents" => map.next_value_seed(EventsSeed(&mut builder))?,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(assemble(vec![builder], other_data))
    }
}

/// Deserializes the `traceEvents` array element by element into the borrowed
/// [`Builder`], discarding each event once consumed.
struct EventsSeed<'a>(&'a mut Builder);

impl<'de, 'a> DeserializeSeed<'de> for EventsSeed<'a> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(EventsVisitor(self.0))
    }
}

struct EventsVisitor<'a>(&'a mut Builder);

impl<'de, 'a> Visitor<'de> for EventsVisitor<'a> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a traceEvents array")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq.next_element_seed(EventSeed(self.0))?.is_some() {}
        Ok(())
    }
}

struct EventSeed<'a>(&'a mut Builder);

impl<'de, 'a> DeserializeSeed<'de> for EventSeed<'a> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        self.0.event(RawEvent::deserialize(d)?);
        Ok(())
    }
}

#[derive(Deserialize)]
struct RawEvent<'a> {
    #[serde(default, borrow)]
    ph: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    cat: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    name: Option<Cow<'a, str>>,
    #[serde(default)]
    ts: Option<f64>,
    #[serde(default)]
    dur: Option<f64>,
    #[serde(default)]
    tid: Option<i64>,
    #[serde(default, borrow)]
    args: Option<RawArgs<'a>>,
}

/// One kept argument value: a string or a number. Other JSON shapes (array,
/// object, null) are dropped, matching what every `args` consumer reads — they
/// only ever look at string or numeric fields.
enum ArgVal<'a> {
    Str(Cow<'a, str>),
    Int(i64),
    Float(f64),
}

impl ArgVal<'_> {
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
struct RawArgs<'a> {
    entries: Vec<(Cow<'a, str>, ArgVal<'a>)>,
}

impl<'a> RawArgs<'a> {
    /// The string value for `key`, if present and a string.
    fn get_str(&self, key: &str) -> Option<&str> {
        self.entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
            ArgVal::Str(s) => Some(s.as_ref()),
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

impl<'de: 'a, 'a> Deserialize<'de> for RawArgs<'a> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(RawArgsVisitor(PhantomData))
    }
}

struct RawArgsVisitor<'a>(PhantomData<&'a ()>);

impl<'de: 'a, 'a> Visitor<'de> for RawArgsVisitor<'a> {
    type Value = RawArgs<'a>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an args object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<RawArgs<'a>, A::Error> {
        let mut entries = Vec::new();
        while let Some(key) = map.next_key::<Cow<'a, str>>()? {
            if let MaybeArg(Some(v)) = map.next_value::<MaybeArg<'a>>()? {
                entries.push((key, v));
            }
        }
        Ok(RawArgs { entries })
    }
}

/// An arg value that keeps a scalar and discards a composite, deserialized by
/// dispatching on the JSON token so no intermediate `Value` is built.
struct MaybeArg<'a>(Option<ArgVal<'a>>);

impl<'de: 'a, 'a> Deserialize<'de> for MaybeArg<'a> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(MaybeArgVisitor(PhantomData))
    }
}

struct MaybeArgVisitor<'a>(PhantomData<&'a ()>);

impl<'de: 'a, 'a> Visitor<'de> for MaybeArgVisitor<'a> {
    type Value = MaybeArg<'a>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON value")
    }

    /// A string the parser can point straight into the input (no escapes) is
    /// borrowed; an escaped one arrives through [`Self::visit_str`] owned.
    fn visit_borrowed_str<E>(self, v: &'de str) -> Result<MaybeArg<'a>, E> {
        Ok(MaybeArg(Some(ArgVal::Str(Cow::Borrowed(v)))))
    }

    fn visit_str<E>(self, v: &str) -> Result<MaybeArg<'a>, E> {
        Ok(MaybeArg(Some(ArgVal::Str(Cow::Owned(v.to_owned())))))
    }

    fn visit_string<E>(self, v: String) -> Result<MaybeArg<'a>, E> {
        Ok(MaybeArg(Some(ArgVal::Str(Cow::Owned(v)))))
    }

    fn visit_i64<E>(self, v: i64) -> Result<MaybeArg<'a>, E> {
        Ok(MaybeArg(Some(ArgVal::Int(v))))
    }

    fn visit_u64<E>(self, v: u64) -> Result<MaybeArg<'a>, E> {
        Ok(MaybeArg(Some(
            i64::try_from(v).map(ArgVal::Int).unwrap_or(ArgVal::Float(v as f64)),
        )))
    }

    fn visit_f64<E>(self, v: f64) -> Result<MaybeArg<'a>, E> {
        Ok(MaybeArg(Some(ArgVal::Float(v))))
    }

    fn visit_bool<E>(self, v: bool) -> Result<MaybeArg<'a>, E> {
        Ok(MaybeArg(Some(ArgVal::Str(Cow::Owned(v.to_string())))))
    }

    fn visit_unit<E>(self) -> Result<MaybeArg<'a>, E> {
        Ok(MaybeArg(None))
    }

    fn visit_none<E>(self) -> Result<MaybeArg<'a>, E> {
        Ok(MaybeArg(None))
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<MaybeArg<'a>, D::Error> {
        d.deserialize_any(self)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<MaybeArg<'a>, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(MaybeArg(None))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<MaybeArg<'a>, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(MaybeArg(None))
    }
}

fn round_us(v: Option<f64>) -> i64 {
    v.map(|x| x.round() as i64).unwrap_or(0)
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
    pub fn from_path(path: &Path) -> Result<Profile, LoadError> {
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

fn decode(bytes: &[u8]) -> Result<String, LoadError> {
    let raw = if bytes.starts_with(&GZIP_MAGIC) {
        timed("  decompress", || gunzip(bytes))?
    } else {
        bytes.to_vec()
    };
    Ok(timed("  latin1->utf8", || latin1_to_utf8(raw)))
}

/// Inflates a gzip stream into one buffer, reserved from the ISIZE trailer so
/// the common single-member case decodes without growth reallocations (a
/// multi-member stream merely grows past the reservation).
fn gunzip(bytes: &[u8]) -> Result<Vec<u8>, LoadError> {
    let mut out = Vec::with_capacity(gzip_isize(bytes).unwrap_or(0));
    MultiGzDecoder::new(bytes)
        .read_to_end(&mut out)
        .map_err(LoadError::Decompress)?;
    Ok(out)
}

/// The uncompressed length a gzip stream declares in its ISIZE trailer (the
/// length mod 2^32, exact under 4 GiB). `None` for input too short to frame or
/// a zero trailer.
fn gzip_isize(bytes: &[u8]) -> Option<usize> {
    const GZIP_FRAMING_MIN: usize = 18; // 10-byte header + 8-byte trailer
    if bytes.len() < GZIP_FRAMING_MIN {
        return None;
    }
    let isize_trailer = u32::from_le_bytes(bytes[bytes.len() - 4..].try_into().ok()?) as usize;
    if isize_trailer == 0 {
        return None;
    }
    Some(isize_trailer)
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

/// A counter lane during build: owned series with owned point lists, before
/// flattening into the profile's series/point pools.
struct RawLane {
    name: Str,
    series: Vec<RawSeries>,
}

struct RawSeries {
    name: Str,
    points: Vec<CounterPoint>,
}

/// Accumulates the arena pools as `traceEvents` stream in, so each event's
/// strings are interned in the one parse pass rather than collected into a
/// `Vec<RawEvent>` and re-walked. [`assemble`] folds one or more builders and
/// `otherData` into the profile's flat pools.
struct Builder {
    interner: Interner,
    args_pool: Vec<(Str, Str)>,
    thread_meta: BTreeMap<i64, (Option<String>, i64)>,
    // Slices accumulate per thread (depth assignment sorts each list) before
    // being concatenated into the flat slice pool.
    slices: BTreeMap<i64, Vec<Slice>>,
    phase_markers: Vec<PhaseMarker>,
    counter_order: Vec<Str>,
    counters: HashMap<Str, RawLane>,
}

impl Builder {
    /// `bytes_hint` is the length of the JSON this builder will consume; the
    /// interner is pre-sized from it with distinct-string and text densities
    /// that bound what large Bazel profiles exhibit per chunk (about one
    /// distinct string per 700 bytes and one text byte per 4.4), so bulk
    /// interning avoids rehash and regrow copies without gross over-reservation.
    fn new(bytes_hint: usize) -> Builder {
        Builder {
            interner: Interner::with_capacity(bytes_hint / 512, bytes_hint / 8),
            args_pool: Vec::new(),
            thread_meta: BTreeMap::new(),
            slices: BTreeMap::new(),
            phase_markers: Vec::new(),
            counter_order: Vec::new(),
            counters: HashMap::new(),
        }
    }

    fn event(&mut self, ev: RawEvent) {
        let ph = ev.ph.as_deref().unwrap_or("");
        match ph {
            "M" => apply_metadata_event(&ev, &mut self.thread_meta),
            "X" | "i" => {
                let ts = round_us(ev.ts);
                if ev.cat.as_deref() == Some(PHASE_MARKER_CATEGORY) {
                    self.phase_markers.push(PhaseMarker {
                        name: self.interner.intern(ev.name.as_deref().unwrap_or("")),
                        start_us: ts,
                    });
                } else {
                    let tid = ev.tid.unwrap_or(0);
                    let category = ev.cat.as_deref().unwrap_or("");
                    let work_class = work_class(category);
                    let args = build_slice_args(&mut self.interner, &mut self.args_pool, ev.args);
                    self.slices.entry(tid).or_default().push(Slice {
                        name: self.interner.intern(ev.name.as_deref().unwrap_or("")),
                        category: self.interner.intern(category),
                        start_us: ts,
                        dur_us: round_us(ev.dur),
                        depth: 0,
                        args,
                        work_class,
                    });
                }
            }
            "C" => apply_counter_event(
                &mut self.interner,
                &ev,
                &mut self.counter_order,
                &mut self.counters,
            ),
            _ => {}
        }
    }

}

/// The incremental builder merge: [`Merge::fold`] absorbs one builder at a
/// time — in array order — so everything except the final slice copy happens
/// as builders arrive, and the pipelined parse hides that work under the
/// decode∥parse phase. A fold concatenates the builder's text pool onto the
/// joined interner and rebases its args and phase markers; counter lanes
/// re-intern by name so a lane sampled across chunks unifies into one; thread
/// metadata unions per tid. Only the builder's slices survive a fold (as a
/// [`FoldedRun`] with the pool bases its handles rebase by): their copy into
/// the flat pool needs every thread's total, so it waits for [`finish_merge`].
struct Merge {
    interner: Interner,
    args_pool: Vec<(Str, Str)>,
    phase_markers: Vec<PhaseMarker>,
    thread_meta: BTreeMap<i64, (Option<String>, i64)>,
    counter_order: Vec<Str>,
    counters: HashMap<Str, RawLane>,
    runs: Vec<FoldedRun>,
}

struct FoldedRun {
    text_base: u32,
    args_base: u32,
    slices: BTreeMap<i64, Vec<Slice>>,
}

impl Merge {
    fn new() -> Merge {
        Merge {
            interner: Interner::new(),
            args_pool: Vec::new(),
            phase_markers: Vec::new(),
            thread_meta: BTreeMap::new(),
            counter_order: Vec::new(),
            counters: HashMap::new(),
            runs: Vec::new(),
        }
    }

    fn fold(&mut self, builder: Builder) {
        let Builder {
            interner: b_interner,
            args_pool,
            thread_meta,
            slices,
            phase_markers,
            counter_order,
            mut counters,
        } = builder;
        let text_base = self.interner.push_pool(b_interner.text());
        let args_base = self.args_pool.len() as u32;
        self.args_pool.extend(
            args_pool.iter().map(|&(k, v)| (k.shifted(text_base), v.shifted(text_base))),
        );
        self.phase_markers.extend(phase_markers.iter().map(|m| PhaseMarker {
            name: m.name.shifted(text_base),
            start_us: m.start_us,
        }));
        // Per thread: the first folded name wins, the last explicit sort index wins.
        for (tid, (name, sort_index)) in thread_meta {
            let entry = self.thread_meta.entry(tid).or_insert((None, MAX_SORT_INDEX));
            if entry.0.is_none() {
                entry.0 = name;
            }
            if sort_index != MAX_SORT_INDEX {
                entry.1 = sort_index;
            }
        }
        for lane_key in counter_order {
            let lane = counters.remove(&lane_key).expect("ordered lane exists");
            let lane_name = self.interner.intern(b_interner.resolve(lane.name));
            let dst = match self.counters.entry(lane_name) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    self.counter_order.push(lane_name);
                    e.insert(RawLane { name: lane_name, series: Vec::new() })
                }
            };
            for series in lane.series {
                let series_name = self.interner.intern(b_interner.resolve(series.name));
                match dst.series.iter_mut().find(|s| s.name == series_name) {
                    Some(s) => s.points.extend(series.points),
                    None => dst.series.push(RawSeries {
                        name: series_name,
                        points: series.points,
                    }),
                }
            }
        }
        self.runs.push(FoldedRun { text_base, args_base, slices });
    }
}

/// Folds one or more builders — the ordered chunk builders of a parallel parse,
/// or a single serial builder — into a [`Profile`], for the paths that hold all
/// builders at once; the pipelined parse folds incrementally instead and calls
/// [`finish_merge`] directly.
fn assemble(builders: Vec<Builder>, other_data: Option<Map<String, Value>>) -> Profile {
    let mut merge = Merge::new();
    merge.interner.reserve(builders.iter().map(|b| b.interner.text().len()).sum());
    timed("    fold", || {
        for builder in builders {
            merge.fold(builder);
        }
    });
    finish_merge(merge, other_data)
}

/// Copies every folded slice exactly once, straight into its final
/// `slice_pool` position with its handles rebased during the copy — the
/// per-thread copy + depth pass runs across worker threads — then seals the
/// merged pools into a [`Profile`].
fn finish_merge(merge: Merge, other_data: Option<Map<String, Value>>) -> Profile {
    let Merge {
        mut interner,
        args_pool,
        mut phase_markers,
        thread_meta,
        counter_order,
        mut counters,
        runs,
    } = merge;
    // The pool is handed to the copy phase as uninitialized capacity rather
    // than pre-filled: a fill pass would touch every page serially just to be
    // overwritten, so the first (and only) write of each slot happens on the
    // worker that copies it.
    let slice_total: usize = runs.iter().flat_map(|r| r.slices.values()).map(Vec::len).sum();
    let mut slice_pool: Vec<Slice> = Vec::with_capacity(slice_total);

    // One source run per (thread, builder): that builder's slices for the
    // thread, with the bases its handles rebase by.
    struct Run<'a> {
        src: &'a [Slice],
        text_base: u32,
        args_base: u32,
    }
    // A thread's contiguous destination segment, its runs in builder order, and
    // the copy phase's per-thread results. The segment starts uninitialized;
    // the runs' lengths sum to exactly its length, so the copy phase writes
    // every slot exactly once.
    struct TidTask<'a> {
        dest: &'a mut [std::mem::MaybeUninit<Slice>],
        runs: Vec<Run<'a>>,
        max_depth: u32,
        min_start_us: i64,
        max_end_us: i64,
    }
    let mut tid_runs: BTreeMap<i64, Vec<Run>> = BTreeMap::new();
    for folded in &runs {
        for (&tid, tslices) in &folded.slices {
            tid_runs.entry(tid).or_default().push(Run {
                src: tslices,
                text_base: folded.text_base,
                args_base: folded.args_base,
            });
        }
    }
    let mut segments: Vec<(i64, Span32)> = Vec::with_capacity(tid_runs.len());
    let mut tasks: Vec<TidTask> = Vec::with_capacity(tid_runs.len());
    let mut rest = slice_pool.spare_capacity_mut();
    let mut off = 0u32;
    for (tid, runs) in tid_runs {
        let len: usize = runs.iter().map(|r| r.src.len()).sum();
        let (dest, tail) = std::mem::take(&mut rest).split_at_mut(len);
        rest = tail;
        segments.push((tid, Span32::new(off, len as u32)));
        off += len as u32;
        tasks.push(TidTask {
            dest,
            runs,
            max_depth: 0,
            min_start_us: i64::MAX,
            max_end_us: i64::MIN,
        });
    }
    debug_assert_eq!(off as usize, slice_total);

    timed("    slices", || {
        // Thread sizes are heavily skewed toward a few big evaluator lanes, so
        // workers steal tasks one thread at a time.
        let workers = worker_count().min(tasks.len());
        let stealer = std::sync::Mutex::new(tasks.iter_mut());
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| loop {
                    let Some(task) = stealer.lock().unwrap().next() else {
                        break;
                    };
                    let mut cursor = 0usize;
                    for run in &task.runs {
                        let dest = &mut task.dest[cursor..cursor + run.src.len()];
                        for (dst, src) in dest.iter_mut().zip(run.src) {
                            let mut s = *src;
                            s.name = s.name.shifted(run.text_base);
                            s.category = s.category.shifted(run.text_base);
                            if let Some(t) = &mut s.args.target {
                                *t = t.shifted(run.text_base);
                            }
                            if let Some(m) = &mut s.args.mnemonic {
                                *m = m.shifted(run.text_base);
                            }
                            if let Some(c) = &mut s.args.configuration {
                                *c = c.shifted(run.text_base);
                            }
                            s.args.others = s.args.others.shifted(run.args_base);
                            dst.write(s);
                        }
                        cursor += run.src.len();
                    }
                    // SAFETY: the runs' lengths sum to the segment length, so
                    // every slot was just written.
                    let dest: &mut [Slice] = unsafe {
                        &mut *((&mut *task.dest) as *mut [std::mem::MaybeUninit<Slice>] as *mut [Slice])
                    };
                    task.max_depth = assign_depths(dest);
                    for s in dest.iter() {
                        task.min_start_us = task.min_start_us.min(s.start_us);
                        task.max_end_us = task.max_end_us.max(s.start_us + s.dur_us);
                    }
                });
            }
        });
    });
    let results: Vec<(u32, i64, i64)> = tasks
        .into_iter()
        .map(|t| (t.max_depth, t.min_start_us, t.max_end_us))
        .collect();
    // SAFETY: the segments partition the first `slice_total` slots of the
    // pool's spare capacity, and the copy scope joined all their writers.
    unsafe { slice_pool.set_len(slice_total) };

    let profile = timed("    tail", || {
        phase_markers.sort_by_key(|m| m.start_us);

        let metadata = build_metadata(&mut interner, other_data.as_ref());

        let mut start: Option<i64> = None;
        let mut end: Option<i64> = None;
        let mut note = |s: i64, e: i64| {
            start = Some(start.map_or(s, |cur| cur.min(s)));
            end = Some(end.map_or(e, |cur| cur.max(e)));
        };
        for (&(_, span), &(_, min_start, max_end)) in segments.iter().zip(&results) {
            if !span.is_empty() {
                note(min_start, max_end);
            }
        }
        for lane in counters.values() {
            for series in &lane.series {
                for p in &series.points {
                    note(p.ts_us, p.ts_us);
                }
            }
        }
        for m in &phase_markers {
            note(m.start_us, m.start_us);
        }
        let span = TimeSpan {
            start_us: start.unwrap_or(0),
            end_us: end.unwrap_or(0),
        };

        let mut threads: Vec<ThreadTrack> = segments
            .iter()
            .zip(&results)
            .map(|(&(tid, slices), &(max_depth, ..))| {
                let (name, sort_index) = match thread_meta.get(&tid) {
                    Some((Some(n), s)) => ((*n).to_owned(), *s),
                    Some((None, s)) => (format!("thread {tid}"), *s),
                    None => (format!("thread {tid}"), MAX_SORT_INDEX),
                };
                let lane_class = lane_class(tid, &name);
                ThreadTrack {
                    tid,
                    name: interner.intern(&name),
                    sort_index,
                    slices,
                    max_depth,
                    lane_class,
                }
            })
            .collect();
        threads.sort_by_key(|t| (t.sort_index, t.tid));

        // Split each counter into one lane per numeric series: overlaid series
        // are hard to read, so each becomes its own graph, labelled with its arg
        // key when a counter carries more than one. Lanes are ordered by kind
        // (action, system load, CPU, memory, network) with each cluster's
        // aggregate ahead of its Bazel-only breakdown; a series this ordering
        // does not recognise keeps its first-seen position via the stable sort. A
        // counter that carried no numeric series (e.g. worker memory on a build
        // with no persistent workers) keeps an empty lane so its absence stays
        // visible rather than being silently dropped.
        struct SplitLane {
            label: String,
            rank: (u8, u8),
            series: Option<RawSeries>,
        }
        let mut split: Vec<SplitLane> = Vec::new();
        for name in counter_order {
            let Some(lane) = counters.remove(&name) else {
                continue;
            };
            let counter_name = interner.resolve(lane.name).to_string();
            if lane.series.is_empty() {
                let rank = counter_rank(&counter_name, "");
                split.push(SplitLane {
                    label: counter_name,
                    rank,
                    series: None,
                });
                continue;
            }
            let multi = lane.series.len() > 1;
            for s in lane.series {
                let key = interner.resolve(s.name).to_string();
                let rank = counter_rank(&counter_name, &key);
                let label = if multi {
                    format!("{counter_name} · {key}")
                } else {
                    counter_name.clone()
                };
                split.push(SplitLane {
                    label,
                    rank,
                    series: Some(s),
                });
            }
        }
        split.sort_by_key(|l| l.rank);

        let split_series = || split.iter().filter_map(|l| l.series.as_ref());
        let mut series_pool: Vec<CounterSeries> = Vec::with_capacity(split_series().count());
        let mut point_pool: Vec<CounterPoint> =
            Vec::with_capacity(split_series().map(|s| s.points.len()).sum());
        let counters: Vec<CounterLane> = split
            .into_iter()
            .map(|l| {
                let name = interner.intern(&l.label);
                let series_off = series_pool.len() as u32;
                if let Some(s) = l.series {
                    let points_off = point_pool.len() as u32;
                    let points_len = s.points.len() as u32;
                    point_pool.extend(s.points);
                    series_pool.push(CounterSeries {
                        name: s.name,
                        points: Span32::new(points_off, points_len),
                    });
                }
                let series_len = series_pool.len() as u32 - series_off;
                CounterLane {
                    name,
                    series: Span32::new(series_off, series_len),
                }
            })
            .collect();

        Profile::from_parts(ProfileParts {
            text: interner.into_text(),
            slice_pool,
            args_pool,
            series_pool,
            point_pool,
            metadata,
            threads,
            counters,
            phase_markers,
            span,
        })
    });

    // The folded runs' teardown (per-thread slice vectors) is pure
    // page-return work; detached so the load doesn't wait on it.
    std::thread::spawn(move || drop(runs));
    profile
}

/// Builds a profile from already-collected events, for the [`recover`] path that
/// pulls whole events out of a truncated array before parsing them.
fn build(other_data: Option<Map<String, Value>>, events: Vec<RawEvent>) -> Profile {
    let mut builder = Builder::new(0);
    for ev in events {
        builder.event(ev);
    }
    assemble(vec![builder], other_data)
}

/// Below this many bytes of `traceEvents` the parallel parser's thread and merge
/// overhead outweighs the gain, so [`parse_parallel`] declines and the serial
/// path runs.
const MIN_PARALLEL_BYTES: usize = 8 << 20;

/// Worker count for the parallel passes over profile data: the machine's
/// physical core count rather than its logical (SMT) thread count, because the
/// passes are memory-bandwidth bound, so threads past the cores that own the
/// memory channels mostly stall on bandwidth, burning CPU for a wall-time gain
/// that has flattened. Clamped to the parallelism the process is actually
/// granted, so a CPU quota or affinity mask is honored.
fn worker_count() -> usize {
    let granted = std::thread::available_parallelism().map_or(1, |n| n.get());
    num_cpus::get_physical().min(granted)
}

/// Parses the `traceEvents` array across several threads when the profile is
/// large enough to repay it, returning `None` so the caller runs the serial
/// [`parse_streaming`] for a small input, a single core, or any input whose
/// shape the chunker can't navigate (no array, no usable split points, a chunk
/// that fails to parse).
fn parse_parallel(text: &str) -> Option<Profile> {
    let workers = worker_count();
    if workers < 2 {
        return None;
    }
    parse_chunks(text, workers, MIN_PARALLEL_BYTES)
}

/// How many chunks each worker gets on average. Event density varies across a
/// profile, so equal-byte chunks parse in unequal time; finer chunks pulled
/// from a shared queue bound the idle tail at roughly one chunk's parse time.
const CHUNKS_PER_WORKER: usize = 8;

/// Floor on chunk size: below this, per-chunk builder overhead (maps, pools)
/// outweighs the balancing gain from finer chunks.
const MIN_CHUNK_BYTES: usize = 4 << 20;

/// Splits the `traceEvents` array into byte chunks at newline boundaries — a
/// literal newline never falls inside a JSON string, so it always lies between
/// events — parses each chunk into its own [`Builder`] on a pool of `n` worker
/// threads pulling chunks from a shared queue, then assembles the builders in
/// array order. `min_bytes` gates the array size below which it
/// declines. Returns `None` if the array can't be located, no split yields more
/// than one chunk, or any chunk fails to parse.
fn parse_chunks(text: &str, n: usize, min_bytes: usize) -> Option<Profile> {
    let events_key = text.find("\"traceEvents\"")?;
    let array_start = text[events_key..].find('[')? + events_key + 1;
    // Bound the region to the array's closing bracket (the last `]`, since only
    // the outer object's `}` follows it) so a chunk never picks up the trailing
    // `]}` as event content. `traceEvents` is the profile's last key.
    let array_end = text.rfind(']')?;
    if array_end <= array_start {
        return None;
    }
    let region = &text[array_start..array_end];
    if region.len() < min_bytes {
        return None;
    }
    let target_chunks = (n * CHUNKS_PER_WORKER).min(region.len() / MIN_CHUNK_BYTES).max(n);
    // Cut at the first newline at or after each evenly spaced target; each cut
    // sits between two events, so a chunk holds only whole events.
    let mut bounds = vec![0usize];
    for k in 1..target_chunks {
        let target = region.len() * k / target_chunks;
        if let Some(off) = region[target..].find('\n') {
            let cut = target + off + 1;
            if cut > *bounds.last().unwrap() {
                bounds.push(cut);
            }
        }
    }
    bounds.push(region.len());
    if bounds.len() < 3 {
        // Only one chunk — nothing to parallelize, let the serial path handle it.
        return None;
    }
    let chunks: Vec<&str> = bounds.windows(2).map(|w| &region[w[0]..w[1]]).collect();

    let other_data = recover_other_data(text);
    let builders: Vec<Option<Builder>> = timed("  parse chunks", || {
        let next = std::sync::atomic::AtomicUsize::new(0);
        let parsed: Vec<(usize, Option<Builder>)> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..n.min(chunks.len()))
                .map(|_| {
                    scope.spawn(|| {
                        let mut parsed = Vec::new();
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let Some(&chunk) = chunks.get(i) else { break };
                            let mut builder = Builder::new(chunk.len());
                            let ok = parse_range_into(&mut builder, chunk);
                            parsed.push((i, ok.then_some(builder)));
                        }
                        parsed
                    })
                })
                .collect();
            handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
        });
        // Chunks were stolen out of order; restore array order for the merge.
        let mut builders: Vec<Option<Builder>> = Vec::new();
        builders.resize_with(chunks.len(), || None);
        for (i, b) in parsed {
            builders[i] = b;
        }
        builders
    });

    let builders: Vec<Builder> = builders.into_iter().collect::<Option<_>>()?;
    if builders.is_empty() {
        return None;
    }
    Some(timed("  assemble", || assemble(builders, other_data)))
}

/// Ceiling on a pipelined chunk buffer, bounding the memory the bounded queue
/// plus in-flight worker chunks can hold on an arbitrarily large profile.
const PIPELINE_MAX_CHUNK_BYTES: usize = 32 << 20;

/// A run of decoded bytes without a newline longer than this aborts the
/// pipelined decode: chunks are cut at newlines, so a profile that is not
/// one-event-per-line would otherwise grow one chunk without bound. The
/// whole-buffer fallback parses such input instead.
const PIPELINE_MAX_EVENT_BYTES: usize = 64 << 20;

/// Decompresses a gzipped profile and parses it concurrently: a producer
/// thread streams the inflate, cuts the decoded bytes into newline-aligned
/// chunks, and feeds them through a bounded queue to workers running the same
/// per-chunk builders as [`parse_chunks`], so the decode leg hides under the
/// parse leg. Each chunk buffer is dropped right after its parse — everything
/// a builder keeps is interned — so the whole decoded text never exists in
/// memory at once. Returns `None` — leaving the input to the whole-buffer path
/// — for a single core, a stream whose ISIZE trailer is missing, zero, or
/// under the parallel threshold, or any decode or parse failure.
fn parse_gz_pipelined(bytes: &[u8]) -> Option<Profile> {
    let workers = worker_count();
    if workers < 2 {
        return None;
    }
    let decoded_size = gzip_isize(bytes)?;
    if decoded_size < MIN_PARALLEL_BYTES {
        return None;
    }
    let chunk_bytes = (decoded_size / (workers * CHUNKS_PER_WORKER))
        .clamp(MIN_CHUNK_BYTES, PIPELINE_MAX_CHUNK_BYTES);
    parse_gz_stream(bytes, workers, chunk_bytes)
}

/// One newline-aligned run of decoded profile bytes, numbered so builders can
/// be reassembled in array order after out-of-order parsing.
struct Piece {
    idx: usize,
    bytes: Vec<u8>,
}

/// What the producer keeps besides the event chunks: the text before the
/// `traceEvents` array and from its closing bracket to the end of the stream —
/// one of which carries `otherData` — and how many pieces it sent.
struct StreamEnds {
    head: Vec<u8>,
    tail: Vec<u8>,
    pieces: usize,
}

/// Reads until `buf` is full or the stream ends; returns the filled length
/// (short of `buf.len()` exactly at end of stream).
fn fill(dec: &mut impl Read, buf: &mut [u8], mut filled: usize) -> std::io::Result<usize> {
    while filled < buf.len() {
        match dec.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The producer side of the pipelined decode: streams the inflate and sends
/// newline-aligned pieces of the `traceEvents` array, blocking on the bounded
/// queue when the workers are behind. `None` on a decode error, an input the
/// chunker can't navigate (no array, no closing bracket, an over-long line),
/// or a closed queue.
fn stream_pieces(
    bytes: &[u8],
    chunk_bytes: usize,
    tx: std::sync::mpsc::SyncSender<Piece>,
) -> Option<StreamEnds> {
    let mut dec = MultiGzDecoder::new(bytes);

    // Grow the first buffer until the traceEvents array opens inside it; the
    // bytes before the bracket become `head`, the rest seed the chunk loop.
    let mut buf = vec![0u8; chunk_bytes];
    let mut filled = fill(&mut dec, &mut buf, 0).ok()?;
    let array_start = loop {
        if let Some(key) = find_bytes(&buf[..filled], b"\"traceEvents\"") {
            let after_key = key + "\"traceEvents\"".len();
            if let Some(bracket) = buf[after_key..filled].iter().position(|&b| b == b'[') {
                break after_key + bracket + 1;
            }
        }
        if filled < buf.len() || buf.len() >= PIPELINE_MAX_EVENT_BYTES {
            return None;
        }
        buf.resize(buf.len() * 2, 0);
        filled = fill(&mut dec, &mut buf, filled).ok()?;
    };
    let head = buf[..array_start].to_vec();
    let mut carry = buf[array_start..filled].to_vec();

    let mut pieces = 0usize;
    loop {
        // Sizing from the carry lets a chunk double when no newline fit in it.
        let mut buf = vec![0u8; chunk_bytes.max(carry.len() * 2)];
        buf[..carry.len()].copy_from_slice(&carry);
        let filled = fill(&mut dec, &mut buf, carry.len()).ok()?;
        if filled < buf.len() {
            // End of stream: everything before the last `]` is event content,
            // the bracket onward may carry a trailing otherData.
            buf.truncate(filled);
            let close = buf.iter().rposition(|&b| b == b']')?;
            let tail = buf[close..].to_vec();
            buf.truncate(close);
            if !buf.is_empty() {
                tx.send(Piece { idx: pieces, bytes: buf }).ok()?;
                pieces += 1;
            }
            return Some(StreamEnds { head, tail, pieces });
        }
        match buf[..filled].iter().rposition(|&b| b == b'\n') {
            Some(newline) => {
                let cut = newline + 1;
                carry.clear();
                carry.extend_from_slice(&buf[cut..filled]);
                buf.truncate(cut);
                tx.send(Piece { idx: pieces, bytes: buf }).ok()?;
                pieces += 1;
            }
            None if filled >= PIPELINE_MAX_EVENT_BYTES => return None,
            None => {
                buf.truncate(filled);
                carry = buf;
            }
        }
    }
}

/// Runs the decode producer, `workers` chunk parsers, and a merger over
/// bounded queues. The piece-queue bound keeps at most about two chunks per
/// worker decoded-but-unparsed, so peak memory scales with the worker count,
/// not the profile; transcoding runs per chunk on the workers, off the decode
/// leg. The merger reorders finished builders back into array order and folds
/// each into the running [`Merge`] while later chunks are still decoding and
/// parsing, so after the last chunk only [`finish_merge`] remains.
fn parse_gz_stream(bytes: &[u8], workers: usize, chunk_bytes: usize) -> Option<Profile> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<Piece>(workers);
    let rx = std::sync::Mutex::new(rx);
    let (builder_tx, builder_rx) = std::sync::mpsc::channel::<(usize, Option<Builder>)>();
    let (ends, merged) = timed("  decode+parse+fold chunks", || {
        std::thread::scope(|scope| {
            let producer = scope.spawn(|| stream_pieces(bytes, chunk_bytes, tx));
            for _ in 0..workers {
                let builder_tx = builder_tx.clone();
                let rx = &rx;
                scope.spawn(move || loop {
                    let piece = match rx.lock().unwrap().recv() {
                        Ok(piece) => piece,
                        Err(_) => break,
                    };
                    let text = latin1_to_utf8(piece.bytes);
                    let mut builder = Builder::new(text.len());
                    let ok = parse_range_into(&mut builder, &text);
                    if builder_tx.send((piece.idx, ok.then_some(builder))).is_err() {
                        break;
                    }
                });
            }
            // The workers hold the only remaining senders, so the merger's
            // channel closes when the last of them finishes.
            drop(builder_tx);
            let merger = scope.spawn(move || {
                let mut merge = Merge::new();
                let mut pending: BTreeMap<usize, Builder> = BTreeMap::new();
                let mut next = 0usize;
                let mut failed = false;
                while let Ok((idx, builder)) = builder_rx.recv() {
                    match builder {
                        Some(builder) if !failed => {
                            pending.insert(idx, builder);
                        }
                        Some(_) => {}
                        None => {
                            failed = true;
                            pending.clear();
                        }
                    }
                    while let Some(builder) = pending.remove(&next) {
                        merge.fold(builder);
                        next += 1;
                    }
                }
                (!failed).then_some((merge, next))
            });
            let ends = producer.join().unwrap();
            (ends, merger.join().unwrap())
        })
    });
    let ends = ends?;
    let (merge, folded) = merged?;
    if folded != ends.pieces || folded == 0 {
        return None;
    }
    let head = latin1_to_utf8(ends.head);
    let tail = latin1_to_utf8(ends.tail);
    let other_data = recover_other_data(&head).or_else(|| recover_other_data(&tail));
    Some(timed("  finish", || finish_merge(merge, other_data)))
}

/// Parses every `{...}` event object in one chunk of the `traceEvents` array
/// into `builder`, in place: each event is deserialized from its own offset
/// with a `StreamDeserializer` (which reports the bytes it consumed), and the
/// comma/whitespace separators between events are skipped by hand. Returns
/// `false` on malformed content so the caller falls back to the serial parser
/// rather than silently dropping events.
fn parse_range_into(builder: &mut Builder, chunk: &str) -> bool {
    let bytes = chunk.as_bytes();
    let mut pos = 0;
    while pos < bytes.len() {
        match bytes[pos] {
            b' ' | b'\t' | b'\n' | b'\r' | b',' => pos += 1,
            b'{' => {
                let mut stream =
                    serde_json::Deserializer::from_str(&chunk[pos..]).into_iter::<RawEvent>();
                match stream.next() {
                    Some(Ok(ev)) => {
                        builder.event(ev);
                        pos += stream.byte_offset();
                    }
                    _ => return false,
                }
            }
            _ => return false,
        }
    }
    true
}

fn build_metadata(interner: &mut Interner, other: Option<&Map<String, Value>>) -> Metadata {
    let mut get = |k: &str| {
        let v = other.and_then(|m| m.get(k)).and_then(Value::as_str);
        interner.intern_opt(v)
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

fn build_slice_args(
    interner: &mut Interner,
    args_pool: &mut Vec<(Str, Str)>,
    args: Option<RawArgs>,
) -> SliceArgs {
    let mut out = SliceArgs::default();
    let Some(args) = args else {
        return out;
    };
    let others_off = args_pool.len() as u32;
    for (k, v) in args.entries {
        // The typed fields are strings; a non-string value under one of those
        // keys is dropped, everything else is kept for display.
        match k.as_ref() {
            "target" => {
                if let ArgVal::Str(s) = v {
                    out.target = Some(interner.intern(&s));
                }
            }
            "mnemonic" => {
                if let ArgVal::Str(s) = v {
                    out.mnemonic = Some(interner.intern(&s));
                }
            }
            "configuration" => {
                if let ArgVal::Str(s) = v {
                    out.configuration = Some(interner.intern(&s));
                }
            }
            _ => {
                let key = interner.intern(&k);
                // A string value interns straight from the borrowed input; a
                // numeric one formats to its display string first.
                let val = match v {
                    ArgVal::Str(s) => interner.intern(&s),
                    ArgVal::Int(n) => interner.intern(&n.to_string()),
                    ArgVal::Float(n) => interner.intern(&n.to_string()),
                };
                args_pool.push((key, val));
            }
        }
    }
    out.others = Span32::new(others_off, args_pool.len() as u32 - others_off);
    out
}

fn apply_counter_event(
    interner: &mut Interner,
    ev: &RawEvent,
    order: &mut Vec<Str>,
    counters: &mut HashMap<Str, RawLane>,
) {
    let lane_name = interner.intern(ev.name.as_deref().unwrap_or(""));
    let ts = round_us(ev.ts);
    let Some(args) = ev.args.as_ref() else {
        return;
    };
    let lane = counters.entry(lane_name).or_insert_with(|| {
        order.push(lane_name);
        RawLane {
            name: lane_name,
            series: Vec::new(),
        }
    });
    for (key, value) in &args.entries {
        let Some(value) = value.as_f64() else {
            continue;
        };
        let key = interner.intern(key);
        let series = match lane.series.iter_mut().find(|s| s.name == key) {
            Some(s) => s,
            None => {
                lane.series.push(RawSeries {
                    name: key,
                    points: Vec::new(),
                });
                lane.series.last_mut().unwrap()
            }
        };
        series.points.push(CounterPoint { ts_us: ts, value });
    }
}

/// Preferred display order for a Bazel profile counter series, as
/// `(cluster, within)`: group by kind — action counts, system load, CPU,
/// memory, network — and within a cluster place the aggregate ("total") ahead
/// of the Bazel-only breakdown. A series whose counter this does not recognise
/// lands in a trailing cluster and, via a stable sort, keeps its first-seen
/// order. `name` is the counter event's name, `series` its arg key.
fn counter_rank(name: &str, series: &str) -> (u8, u8) {
    let name = name.to_lowercase();
    let series = series.to_lowercase();
    if name.contains("action count") {
        // Total actions, then local execution, then local cache.
        let within = if series == "action" {
            0
        } else if series.contains("cache") {
            2
        } else {
            1
        };
        (0, within)
    } else if name.contains("load average") {
        (1, 0)
    } else if name.contains("cpu usage") {
        (2, if name.contains("total") { 0 } else { 1 })
    } else if name.contains("memory") {
        // System total, then Bazel, then worker memory.
        let within = if name.contains("worker") {
            2
        } else if name.contains("total") {
            0
        } else {
            1
        };
        (3, within)
    } else if name.contains("network") {
        (4, if name.contains("down") { 0 } else { 1 })
    } else {
        (5, 0)
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

/// Seals an interner and the given top-level vectors into a profile with empty
/// slice/counter pools, for model tests that need only phase markers and threads.
#[cfg(test)]
pub(crate) fn build_for_test(
    interner: Interner,
    phase_markers: Vec<PhaseMarker>,
    span: TimeSpan,
    threads: Vec<ThreadTrack>,
) -> Profile {
    Profile::from_parts(ProfileParts {
        text: interner.into_text(),
        slice_pool: Vec::new(),
        args_pool: Vec::new(),
        series_pool: Vec::new(),
        point_pool: Vec::new(),
        metadata: Metadata::default(),
        threads,
        counters: Vec::new(),
        phase_markers,
        span,
    })
}

#[cfg(test)]
mod tests {
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
                {"cat":"c","name":"quote \" caf\u00e9","ph":"X","ts":0,"dur":1,"tid":1,"args":{"target":"//a:b\"c","note":"tab\there"}}
            ]
        }"#;
        let p = parse(json);
        let t = &p.threads()[0];
        let s = &p.slices(t)[0];
        assert_eq!(p.str(s.name), "quote \" café");
        assert_eq!(p.opt_str(s.args.target), Some("//a:b\"c"));
        let others: Vec<_> = p
            .slice_others(s)
            .iter()
            .map(|&(k, v)| (p.str(k), p.str(v)))
            .collect();
        assert_eq!(others, vec![("note", "tab\there")]);
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
            for s in p.slices(t) {
                writeln!(
                    out,
                    "  slice {:?} cat={:?} ts={} dur={} depth={} wc={:?} tgt={:?} mn={:?} cfg={:?}",
                    p.str(s.name),
                    p.str(s.category),
                    s.start_us,
                    s.dur_us,
                    s.depth,
                    s.work_class,
                    p.opt_str(s.args.target),
                    p.opt_str(s.args.mnemonic),
                    p.opt_str(s.args.configuration),
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
        let mem = &p.counters()[1];
        let mem_points = p.points(&p.series(mem)[0]);
        assert_eq!(mem_points.len(), 1);
        assert_eq!(mem_points[0].ts_us, 10);
        assert_eq!(mem_points[0].value, 5.0);
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
}
