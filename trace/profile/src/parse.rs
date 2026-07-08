use std::borrow::Cow;
use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BinaryHeap, HashMap};
use std::fmt;
use std::io::Read;
use std::marker::PhantomData;
use std::path::Path;

use flate2::read::MultiGzDecoder;
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

use crate::concurrent_interner::{ConcurrentInterner, Writer};
use crate::model::{
    lane_class, work_class, CounterLane, CounterPoint, CounterSeries, Metadata, PhaseMarker,
    Profile, ProfileParts, Slice, SliceDetail, Span32, Str, ThreadTrack, TimeSpan,
};

/// A [`Writer`] paired with the interner it writes into, so [`finish_merge`]'s
/// late interning and its resolution of existing handles share one writer while
/// the owned interner is sealed by the caller after this is dropped.
struct WriterPool<'a> {
    writer: Writer<'a>,
    interner: &'a ConcurrentInterner,
}

/// Estimated distinct-string density of a Bazel profile — about one interned
/// string per this many decoded bytes — used to pre-size the shared interner's
/// dedup index. Measured near one distinct string per 167 bytes on a large
/// profile; rounding down errs toward a larger index, which only wastes a
/// transient, freed-at-seal allocation, whereas over-counting the bytes
/// under-sizes the shard tables and costs a rehash.
const BYTES_PER_DISTINCT: usize = 160;

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
    let profile =
        serde::Deserializer::deserialize_map(&mut de, ProfileVisitor { text_len: text.len() })?;
    de.end()?;
    Ok(profile)
}

/// Carries the decoded text's byte length so [`Self::visit_map`] can size the
/// interner it fills, since the map visitor never sees the whole text.
struct ProfileVisitor {
    text_len: usize,
}

impl<'de> Visitor<'de> for ProfileVisitor {
    type Value = Profile;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a trace profile object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Profile, A::Error> {
        let interner = ConcurrentInterner::new(self.text_len, self.text_len / BYTES_PER_DISTINCT);
        let mut builder = Builder::new();
        let mut other_data = None;
        {
            let mut writer = interner.writer();
            while let Some(key) = map.next_key::<Cow<str>>()? {
                match key.as_ref() {
                    "otherData" => other_data = map.next_value::<Option<Map<String, Value>>>()?,
                    "traceEvents" => {
                        map.next_value_seed(EventsSeed(&mut builder, &mut writer))?
                    }
                    _ => {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
            }
        }
        builder.seal_slices();
        let mut merge = Merge::new();
        merge.fold(builder);
        Ok(seal(interner, merge, other_data))
    }
}

/// Deserializes the `traceEvents` array element by element into the borrowed
/// [`Builder`], interning through the borrowed [`Writer`] and discarding each
/// event once consumed.
struct EventsSeed<'a, 'w>(&'a mut Builder, &'a mut Writer<'w>);

impl<'de, 'a, 'w> DeserializeSeed<'de> for EventsSeed<'a, 'w> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(EventsVisitor(self.0, self.1))
    }
}

struct EventsVisitor<'a, 'w>(&'a mut Builder, &'a mut Writer<'w>);

impl<'de, 'a, 'w> Visitor<'de> for EventsVisitor<'a, 'w> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a traceEvents array")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let mut scratch = Vec::new();
        while seq
            .next_element_seed(EventSeed(&mut *self.0, &mut *self.1, &mut scratch))?
            .is_some()
        {}
        Ok(())
    }
}

struct EventSeed<'b, 'de, 'w>(&'b mut Builder, &'b mut Writer<'w>, &'b mut Vec<ArgEntry<'de>>);

impl<'de, 'w> DeserializeSeed<'de> for EventSeed<'_, 'de, 'w> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        let ev = RawEventSeed(&mut *self.2).deserialize(d)?;
        self.0.event(self.1, ev, self.2);
        Ok(())
    }
}

struct RawEvent<'a> {
    ph: Option<Cow<'a, str>>,
    cat: Option<Cow<'a, str>>,
    name: Option<Cow<'a, str>>,
    ts: Option<f64>,
    dur: Option<f64>,
    tid: Option<i64>,
    /// Whether the event carried an `args` object (even an empty one — a
    /// counter keeps an empty lane for it); the entries themselves live in the
    /// caller's scratch buffer.
    has_args: bool,
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

/// One kept entry of an event's `args` object: a scalar keyed by its arg name,
/// deserialized as a flat key/value list rather than the
/// `serde_json::Map<String, Value>` the generic path would build. Over the
/// millions of slice events in a large profile the saved per-arg `Value`/`Map`
/// allocations are a large share of parse time, and every consumer (slice args,
/// thread metadata, counters) reads this list directly. Entries accumulate into
/// a caller-owned scratch vector reused across events, so an event's args cost
/// no per-event allocation either.
type ArgEntry<'a> = (Cow<'a, str>, ArgVal<'a>);

/// The string value for `key` among an event's arg entries, if present and a
/// string.
fn arg_str<'e>(entries: &'e [ArgEntry], key: &str) -> Option<&'e str> {
    entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
        ArgVal::Str(s) => Some(s.as_ref()),
        _ => None,
    })
}

/// The integer value for `key` among an event's arg entries, if present and an
/// integer.
fn arg_i64(entries: &[ArgEntry], key: &str) -> Option<i64> {
    entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
        ArgVal::Int(n) => Some(*n),
        _ => None,
    })
}

/// Deserializes one event object; its `args` entries land in the borrowed
/// scratch (cleared first), so the scratch holds exactly this event's entries
/// when `has_args` is set.
struct RawEventSeed<'s, 'a>(&'s mut Vec<ArgEntry<'a>>);

impl<'de: 'a, 'a> DeserializeSeed<'de> for RawEventSeed<'_, 'a> {
    type Value = RawEvent<'a>;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<RawEvent<'a>, D::Error> {
        d.deserialize_map(RawEventVisitor(self.0))
    }
}

struct RawEventVisitor<'s, 'a>(&'s mut Vec<ArgEntry<'a>>);

impl<'de: 'a, 'a> Visitor<'de> for RawEventVisitor<'_, 'a> {
    type Value = RawEvent<'a>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an event object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<RawEvent<'a>, A::Error> {
        let mut ev = RawEvent {
            ph: None,
            cat: None,
            name: None,
            ts: None,
            dur: None,
            tid: None,
            has_args: false,
        };
        while let Some(CowKey(key)) = map.next_key::<CowKey>()? {
            match key.as_ref() {
                "ph" => ev.ph = map.next_value::<MaybeCowStr>()?.0,
                "cat" => ev.cat = map.next_value::<MaybeCowStr>()?.0,
                "name" => ev.name = map.next_value::<MaybeCowStr>()?.0,
                "ts" => ev.ts = map.next_value::<MaybeF64>()?.0,
                "dur" => ev.dur = map.next_value::<MaybeF64>()?.0,
                "tid" => ev.tid = map.next_value::<MaybeI64>()?.0,
                "args" => {
                    self.0.clear();
                    ev.has_args = map.next_value_seed(ArgsSeed(&mut *self.0))?;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(ev)
    }
}

/// Fills the borrowed scratch with an args object's kept entries; `true` when
/// an object was present, `false` for JSON `null` (matching the absent case).
struct ArgsSeed<'s, 'a>(&'s mut Vec<ArgEntry<'a>>);

impl<'de: 'a, 'a> DeserializeSeed<'de> for ArgsSeed<'_, 'a> {
    type Value = bool;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<bool, D::Error> {
        d.deserialize_any(ArgsVisitor(self.0))
    }
}

struct ArgsVisitor<'s, 'a>(&'s mut Vec<ArgEntry<'a>>);

impl<'de: 'a, 'a> Visitor<'de> for ArgsVisitor<'_, 'a> {
    type Value = bool;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an args object or null")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<bool, A::Error> {
        while let Some(CowKey(key)) = map.next_key::<CowKey<'a>>()? {
            if let MaybeArg(Some(v)) = map.next_value::<MaybeArg<'a>>()? {
                self.0.push((key, v));
            }
        }
        Ok(true)
    }

    fn visit_unit<E>(self) -> Result<bool, E> {
        Ok(false)
    }
}

/// An optional string field that borrows from the input where the parser can
/// (like [`CowKey`]); JSON `null` reads as `None`.
struct MaybeCowStr<'a>(Option<Cow<'a, str>>);

impl<'de: 'a, 'a> Deserialize<'de> for MaybeCowStr<'a> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // deserialize_any, not deserialize_str: serde_json rejects null in
        // deserialize_str before reaching the visitor, and null must read as
        // None here. The any-path string arms borrow exactly like the str path.
        deserializer.deserialize_any(MaybeCowStrVisitor(PhantomData))
    }
}

struct MaybeCowStrVisitor<'a>(PhantomData<&'a ()>);

impl<'de: 'a, 'a> Visitor<'de> for MaybeCowStrVisitor<'a> {
    type Value = MaybeCowStr<'a>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a string or null")
    }

    fn visit_borrowed_str<E>(self, v: &'de str) -> Result<MaybeCowStr<'a>, E> {
        Ok(MaybeCowStr(Some(Cow::Borrowed(v))))
    }

    fn visit_str<E>(self, v: &str) -> Result<MaybeCowStr<'a>, E> {
        Ok(MaybeCowStr(Some(Cow::Owned(v.to_owned()))))
    }

    fn visit_string<E>(self, v: String) -> Result<MaybeCowStr<'a>, E> {
        Ok(MaybeCowStr(Some(Cow::Owned(v))))
    }

    fn visit_unit<E>(self) -> Result<MaybeCowStr<'a>, E> {
        Ok(MaybeCowStr(None))
    }
}

/// An optional numeric field read as `f64` (an integer token casts); JSON
/// `null` reads as `None`.
struct MaybeF64(Option<f64>);

impl<'de> Deserialize<'de> for MaybeF64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(MaybeF64Visitor)
    }
}

struct MaybeF64Visitor;

impl Visitor<'_> for MaybeF64Visitor {
    type Value = MaybeF64;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a number or null")
    }

    fn visit_i64<E>(self, v: i64) -> Result<MaybeF64, E> {
        Ok(MaybeF64(Some(v as f64)))
    }

    fn visit_u64<E>(self, v: u64) -> Result<MaybeF64, E> {
        Ok(MaybeF64(Some(v as f64)))
    }

    fn visit_f64<E>(self, v: f64) -> Result<MaybeF64, E> {
        Ok(MaybeF64(Some(v)))
    }

    fn visit_unit<E>(self) -> Result<MaybeF64, E> {
        Ok(MaybeF64(None))
    }
}

/// An optional integer field; JSON `null` reads as `None`.
struct MaybeI64(Option<i64>);

impl<'de> Deserialize<'de> for MaybeI64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(MaybeI64Visitor)
    }
}

struct MaybeI64Visitor;

impl Visitor<'_> for MaybeI64Visitor {
    type Value = MaybeI64;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an integer or null")
    }

    fn visit_i64<E>(self, v: i64) -> Result<MaybeI64, E> {
        Ok(MaybeI64(Some(v)))
    }

    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<MaybeI64, E> {
        i64::try_from(v)
            .map(|n| MaybeI64(Some(n)))
            .map_err(|_| E::custom(format!("integer out of range: {v}")))
    }

    fn visit_unit<E>(self) -> Result<MaybeI64, E> {
        Ok(MaybeI64(None))
    }
}

/// A map key that borrows from the input where the parser can. serde's
/// blanket `Cow<str>` impl always deserializes through an owned `String`;
/// this seed's visitor takes the zero-copy path for an unescaped key.
struct CowKey<'a>(Cow<'a, str>);

impl<'de: 'a, 'a> Deserialize<'de> for CowKey<'a> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_str(CowKeyVisitor(PhantomData))
    }
}

struct CowKeyVisitor<'a>(PhantomData<&'a ()>);

impl<'de: 'a, 'a> Visitor<'de> for CowKeyVisitor<'a> {
    type Value = CowKey<'a>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a string key")
    }

    fn visit_borrowed_str<E>(self, v: &'de str) -> Result<CowKey<'a>, E> {
        Ok(CowKey(Cow::Borrowed(v)))
    }

    fn visit_str<E>(self, v: &str) -> Result<CowKey<'a>, E> {
        Ok(CowKey(Cow::Owned(v.to_owned())))
    }

    fn visit_string<E>(self, v: String) -> Result<CowKey<'a>, E> {
        Ok(CowKey(Cow::Owned(v)))
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

/// Hand-rolled scanner for one `traceEvents` event object: the event schema is
/// fixed and tiny, so matching keys and cutting string and integer tokens
/// straight out of the input skips the per-field visitor dispatch, generic
/// number parsing, and ignored-value machinery that dominate the serde path's
/// CPU. It handles the constructs that dominate a Bazel profile; on anything
/// else — a `\uXXXX` escape, a non-integer number, a type the schema doesn't
/// expect, malformed JSON (excepting unread scalar tokens; see
/// [`Self::skip_value`]) — [`scan_event`] returns `None` and the caller
/// re-parses the event through serde, whose acceptance, errors, and results
/// stay authoritative. For an event the scanner does accept, its result is
/// identical to the serde path's.
struct EventScanner<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

/// Scans the event object opening at `text[start..]`; `Some` holds the event
/// and the position one past its closing brace, with its kept `args` entries
/// in `scratch`. `None` defers the event to serde (`scratch` may then hold a
/// partial fill; the serde path clears it again on the `args` key).
fn scan_event<'a>(
    text: &'a str,
    start: usize,
    scratch: &mut Vec<ArgEntry<'a>>,
) -> Option<(RawEvent<'a>, usize)> {
    let mut s = EventScanner { text, bytes: text.as_bytes(), pos: start };
    s.eat(b'{').then_some(())?;
    let mut ev = RawEvent {
        ph: None,
        cat: None,
        name: None,
        ts: None,
        dur: None,
        tid: None,
        has_args: false,
    };
    s.skip_ws();
    if s.eat(b'}') {
        return Some((ev, s.pos));
    }
    loop {
        let key = s.scan_string()?;
        s.skip_ws();
        s.eat(b':').then_some(())?;
        s.skip_ws();
        match key.as_ref() {
            "ph" => ev.ph = s.scan_opt_str()?,
            "cat" => ev.cat = s.scan_opt_str()?,
            "name" => ev.name = s.scan_opt_str()?,
            "ts" => ev.ts = s.scan_opt_f64()?,
            "dur" => ev.dur = s.scan_opt_f64()?,
            "tid" => ev.tid = s.scan_opt_i64()?,
            "args" => {
                scratch.clear();
                ev.has_args = s.scan_args(scratch)?;
            }
            _ => s.skip_value()?,
        }
        s.skip_ws();
        if s.eat(b',') {
            s.skip_ws();
            continue;
        }
        if s.eat(b'}') {
            return Some((ev, s.pos));
        }
        return None;
    }
}

/// One JSON integer token: sign and magnitude, so each field converts with
/// the exact visit sequence serde would use for it.
struct IntTok {
    neg: bool,
    mag: u64,
}

impl IntTok {
    /// The token as serde delivers it to an `f64` field: through the i64
    /// (negative) or u64 (positive) visit, then a cast.
    fn to_f64(&self) -> Option<f64> {
        Some(if self.neg { self.to_i64()? as f64 } else { self.mag as f64 })
    }

    /// The token as an `i64`, `None` when out of range.
    fn to_i64(&self) -> Option<i64> {
        if self.neg {
            (self.mag <= i64::MIN.unsigned_abs()).then(|| (self.mag as i64).wrapping_neg())
        } else {
            (self.mag <= i64::MAX as u64).then_some(self.mag as i64)
        }
    }
}

impl<'a> EventScanner<'a> {
    fn skip_ws(&mut self) {
        while let Some(&b) = self.bytes.get(self.pos) {
            if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.bytes.get(self.pos) == Some(&byte) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect_keyword(&mut self, kw: &str) -> Option<()> {
        let end = self.pos + kw.len();
        (self.bytes.get(self.pos..end)? == kw.as_bytes()).then(|| self.pos = end)
    }

    /// Cuts one string token starting at its opening quote, leaving the
    /// cursor past the closing one: a borrow of the input when the token has
    /// no escapes, an owned unescape otherwise, `None` on a `\uXXXX` escape
    /// or a control byte (which serde rejects).
    fn scan_string(&mut self) -> Option<Cow<'a, str>> {
        self.eat(b'"').then_some(())?;
        let start = self.pos;
        let mut escaped = false;
        loop {
            self.pos = find_special(self.bytes, self.pos)?;
            match self.bytes[self.pos] {
                b'"' => break,
                b'\\' => {
                    escaped = true;
                    self.pos += 2;
                    if self.pos > self.bytes.len() {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        let raw = &self.text[start..self.pos];
        self.pos += 1;
        if escaped {
            unescape(raw).map(Cow::Owned)
        } else {
            Some(Cow::Borrowed(raw))
        }
    }

    /// One integer token, `None` on a fraction, exponent, leading zero,
    /// negative zero, or a magnitude past u64 — serde takes those over:
    /// floats because serde_json parses them correctly rounded only with its
    /// `float_roundtrip` feature, and the scanner must not produce values a
    /// whisker off the serde path's; a leading zero because the rejection
    /// must be serde's; `-0` because serde reads it as the float -0.0.
    fn scan_int(&mut self) -> Option<IntTok> {
        let neg = self.eat(b'-');
        let start = self.pos;
        let mut mag: u64 = 0;
        while let Some(&b) = self.bytes.get(self.pos) {
            match b {
                b'0'..=b'9' => {
                    mag = mag.checked_mul(10)?.checked_add((b - b'0') as u64)?;
                    self.pos += 1;
                }
                b'.' | b'e' | b'E' => return None,
                _ => break,
            }
        }
        if self.pos == start || (self.bytes[start] == b'0' && self.pos - start > 1) {
            return None;
        }
        if neg && mag == 0 {
            return None;
        }
        Some(IntTok { neg, mag })
    }

    /// A string-or-null field value; the outer `None` defers to serde.
    fn scan_opt_str(&mut self) -> Option<Option<Cow<'a, str>>> {
        match *self.bytes.get(self.pos)? {
            b'"' => Some(Some(self.scan_string()?)),
            b'n' => {
                self.expect_keyword("null")?;
                Some(None)
            }
            _ => None,
        }
    }

    /// An integer-or-null field value read as `f64`.
    fn scan_opt_f64(&mut self) -> Option<Option<f64>> {
        match *self.bytes.get(self.pos)? {
            b'-' | b'0'..=b'9' => Some(Some(self.scan_int()?.to_f64()?)),
            b'n' => {
                self.expect_keyword("null")?;
                Some(None)
            }
            _ => None,
        }
    }

    /// An integer-or-null field value; an out-of-range magnitude defers (and
    /// serde then errors, failing the chunk).
    fn scan_opt_i64(&mut self) -> Option<Option<i64>> {
        match *self.bytes.get(self.pos)? {
            b'-' | b'0'..=b'9' => Some(Some(self.scan_int()?.to_i64()?)),
            b'n' => {
                self.expect_keyword("null")?;
                Some(None)
            }
            _ => None,
        }
    }

    /// The `args` value: `false` for `null`, `true` for an object whose kept
    /// entries were pushed into `scratch` — a scalar per entry, with a bool
    /// stringified and a null or composite dropped, mirroring [`MaybeArg`].
    fn scan_args(&mut self, scratch: &mut Vec<ArgEntry<'a>>) -> Option<bool> {
        match *self.bytes.get(self.pos)? {
            b'n' => {
                self.expect_keyword("null")?;
                Some(false)
            }
            b'{' => {
                self.pos += 1;
                self.skip_ws();
                if self.eat(b'}') {
                    return Some(true);
                }
                loop {
                    let key = self.scan_string()?;
                    self.skip_ws();
                    self.eat(b':').then_some(())?;
                    self.skip_ws();
                    match *self.bytes.get(self.pos)? {
                        b'"' => {
                            let v = self.scan_string()?;
                            scratch.push((key, ArgVal::Str(v)));
                        }
                        b'-' | b'0'..=b'9' => {
                            let tok = self.scan_int()?;
                            let val = match tok.to_i64() {
                                Some(n) => ArgVal::Int(n),
                                // A positive magnitude past i64 falls back to
                                // a float, as serde's u64 visit does.
                                None if !tok.neg => ArgVal::Float(tok.mag as f64),
                                None => return None,
                            };
                            scratch.push((key, val));
                        }
                        b't' => {
                            self.expect_keyword("true")?;
                            scratch.push((key, ArgVal::Str(Cow::Owned("true".to_owned()))));
                        }
                        b'f' => {
                            self.expect_keyword("false")?;
                            scratch.push((key, ArgVal::Str(Cow::Owned("false".to_owned()))));
                        }
                        b'n' => {
                            self.expect_keyword("null")?;
                        }
                        b'{' | b'[' => self.skip_value()?,
                        _ => return None,
                    }
                    self.skip_ws();
                    if self.eat(b',') {
                        self.skip_ws();
                        continue;
                    }
                    if self.eat(b'}') {
                        return Some(true);
                    }
                    return None;
                }
            }
            _ => None,
        }
    }

    /// Skips one JSON value the event schema ignores. Composites are tracked
    /// for balanced brackets with string awareness only; scalar tokens are not
    /// validated, so this accepts some malformed content serde would reject —
    /// affecting only values no consumer reads.
    fn skip_value(&mut self) -> Option<()> {
        match *self.bytes.get(self.pos)? {
            b'"' => self.skip_string(),
            b'{' | b'[' => {
                let mut depth = 0usize;
                while let Some(&b) = self.bytes.get(self.pos) {
                    match b {
                        b'"' => {
                            self.skip_string()?;
                            continue;
                        }
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => {
                            depth -= 1;
                            if depth == 0 {
                                self.pos += 1;
                                return Some(());
                            }
                        }
                        _ => {}
                    }
                    self.pos += 1;
                }
                None
            }
            b'-' | b'0'..=b'9' => {
                while let Some(&b) = self.bytes.get(self.pos) {
                    if matches!(b, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                Some(())
            }
            b't' => self.expect_keyword("true"),
            b'f' => self.expect_keyword("false"),
            b'n' => self.expect_keyword("null"),
            _ => None,
        }
    }

    /// [`Self::scan_string`] without materializing the token.
    fn skip_string(&mut self) -> Option<()> {
        self.eat(b'"').then_some(())?;
        loop {
            self.pos = find_special(self.bytes, self.pos)?;
            match self.bytes[self.pos] {
                b'"' => {
                    self.pos += 1;
                    return Some(());
                }
                b'\\' => self.pos += 2,
                _ => return None,
            }
        }
    }
}

/// Index of the next byte a string scan must stop at — a quote, a backslash,
/// or a control byte — or `None` at end of input. Word-at-a-time via the
/// haszero/hasless formulas of Sean Anderson's "Bit Twiddling Hacks"; this is
/// the parser's hottest loop. Spurious high bits from borrow ripple only ever
/// sit above a genuine match, so the lowest set bit is always exact.
fn find_special(bytes: &[u8], mut i: usize) -> Option<usize> {
    const LO: u64 = 0x0101_0101_0101_0101;
    const HI: u64 = 0x8080_8080_8080_8080;
    /// Exclusive upper bound of the control bytes JSON forbids raw in strings.
    const CONTROL: u8 = 0x20;
    while i + 8 <= bytes.len() {
        let w = u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        let quote = w ^ (LO * u64::from(b'"'));
        let bslash = w ^ (LO * u64::from(b'\\'));
        let special = (quote.wrapping_sub(LO) & !quote)
            | (bslash.wrapping_sub(LO) & !bslash)
            | (w.wrapping_sub(LO * u64::from(CONTROL)) & !w);
        let special = special & HI;
        if special != 0 {
            return Some(i + special.trailing_zeros() as usize / 8);
        }
        i += 8;
    }
    while i < bytes.len() {
        if bytes[i] == b'"' || bytes[i] == b'\\' || bytes[i] < CONTROL {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Unescapes a string token holding only the single-character JSON escapes,
/// `None` on `\uXXXX` (deferred to serde) or an invalid escape (rejected by
/// serde).
fn unescape(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            out.push(match *bytes.get(i + 1)? {
                b'"' => '"',
                b'\\' => '\\',
                b'/' => '/',
                b'b' => '\u{8}',
                b'f' => '\u{c}',
                b'n' => '\n',
                b'r' => '\r',
                b't' => '\t',
                _ => return None,
            });
            i += 2;
        } else {
            let start = i;
            while i < bytes.len() && bytes[i] != b'\\' {
                i += 1;
            }
            out.push_str(&raw[start..i]);
        }
    }
    Some(out)
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
/// [`latin1_expand`] rewrites it.
fn latin1_to_utf8(raw: Vec<u8>) -> String {
    if raw.is_ascii() {
        return String::from_utf8(raw).expect("ascii is valid utf-8");
    }
    latin1_expand(&raw)
}

/// [`latin1_to_utf8`] over a borrowed buffer: the all-ASCII case borrows it
/// outright.
fn latin1_cow(raw: &[u8]) -> Cow<'_, str> {
    if raw.is_ascii() {
        // SAFETY: ASCII is valid UTF-8.
        return Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(raw) });
    }
    Cow::Owned(latin1_expand(raw))
}

/// Rewrites Latin-1 bytes as UTF-8: each leading ASCII run is copied wholesale
/// and only a byte >= 0x80 expands to the two-byte UTF-8 encoding of the same
/// code point.
fn latin1_expand(raw: &[u8]) -> String {
    // High bytes each grow by one; reserve a little slack so a profile with few
    // of them doesn't reallocate.
    let mut out = String::with_capacity(raw.len() + raw.len() / 16 + 16);
    let mut rest = raw;
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

    let interner = ConcurrentInterner::new(text.len(), text.len() / BYTES_PER_DISTINCT);
    let mut builder = Builder::new();
    let mut recovered = false;
    {
        let mut writer = interner.writer();
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
            // Walk the object's braces, skipping any inside string literals, to
            // find its close. A missing close means the array was cut
            // mid-object: stop.
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
            let mut de = serde_json::Deserializer::from_str(&text[start..=end]);
            if let Ok(ev) = RawEventSeed(&mut scratch)
                .deserialize(&mut de)
                .and_then(|ev| de.end().map(|()| ev))
            {
                builder.event(&mut writer, ev, &scratch);
                recovered = true;
            }
            i = end + 1;
        }
    }

    if !recovered {
        return Err(LoadError::Corrupted("no complete events recovered".into()));
    }
    builder.seal_slices();
    let mut merge = Merge::new();
    merge.fold(builder);
    Ok(seal(interner, merge, other_data))
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
/// `Vec<RawEvent>` and re-walked. [`Merge::fold`] folds one or more builders,
/// and [`finish_merge`] seals them and `otherData` into the profile's flat
/// pools.
struct Builder {
    args_pool: Vec<(Str, Str)>,
    thread_meta: BTreeMap<i64, (Option<String>, i64)>,
    /// Slices in event order, each tagged with its thread — one flat vector
    /// whose allocation the chunk parse loops reuse across chunks, so
    /// accumulating a slice is one push into pre-grown capacity.
    /// [`Builder::seal_slices`] regroups it into `lane_pool` and `lanes`
    /// before the builder is folded.
    staging: Vec<(i64, Slice, SliceDetail)>,
    /// After [`Builder::seal_slices`]: every staged slice, contiguous per
    /// thread in lane order, sized exactly.
    lane_pool: Vec<(Slice, SliceDetail)>,
    /// After [`Builder::seal_slices`]: each thread's range of `lane_pool`, in
    /// first-appearance order.
    lanes: Vec<(i64, Span32)>,
    phase_markers: Vec<PhaseMarker>,
    counter_order: Vec<Str>,
    counters: HashMap<Str, RawLane>,
}

impl Builder {
    fn new() -> Builder {
        Builder {
            args_pool: Vec::new(),
            thread_meta: BTreeMap::new(),
            staging: Vec::new(),
            lane_pool: Vec::new(),
            lanes: Vec::new(),
            phase_markers: Vec::new(),
            counter_order: Vec::new(),
            counters: HashMap::new(),
        }
    }

    /// Folds one event in, interning its strings into `interner` — the shared
    /// concurrent arena on the parallel paths, so the handles are global. When
    /// `ev.has_args` is set `scratch` holds the event's args entries; they are
    /// consumed here, and deserializing the next event reuses the buffer.
    fn event(&mut self, interner: &mut Writer, ev: RawEvent, scratch: &[ArgEntry]) {
        let args = ev.has_args.then_some(scratch);
        let ph = ev.ph.as_deref().unwrap_or("");
        match ph {
            "M" => apply_metadata_event(&ev, args, &mut self.thread_meta),
            "X" | "i" => {
                let ts = round_us(ev.ts);
                if ev.cat.as_deref() == Some(PHASE_MARKER_CATEGORY) {
                    self.phase_markers.push(PhaseMarker {
                        name: interner.intern(ev.name.as_deref().unwrap_or("")),
                        start_us: ts,
                    });
                } else {
                    let tid = ev.tid.unwrap_or(0);
                    let category = ev.cat.as_deref().unwrap_or("");
                    let work_class = work_class(category);
                    let category = interner.intern(category);
                    let detail = build_slice_detail(interner, &mut self.args_pool, category, args);
                    self.staging.push((
                        tid,
                        Slice {
                            name: interner.intern(ev.name.as_deref().unwrap_or("")),
                            start_us: ts,
                            // A malformed dur must not make the slice end
                            // before it starts: every consumer — the end-max
                            // cull bound, the depth stack, the containment
                            // walk — assumes end >= start.
                            dur_us: round_us(ev.dur).max(0),
                            depth: 0,
                            work_class,
                        },
                        detail,
                    ));
                }
            }
            "C" => apply_counter_event(
                interner,
                &ev,
                args,
                &mut self.counter_order,
                &mut self.counters,
            ),
            _ => {}
        }
    }

    /// Regroups the staged slices into per-thread runs and sorts each run
    /// into lane order — the form [`Merge::fold`] requires of a folded run.
    /// The pool is allocated at its exact final size and filled through
    /// per-thread cursors, preserving event order within a thread. On the
    /// parallel paths this runs on the parse workers, so the per-chunk work
    /// hides under the decode∥parse phase instead of adding to the final
    /// merge.
    fn seal_slices(&mut self) {
        // The lane bookkeeping (counts, spans, cursors) is u32; wrapping it
        // would break the exactly-once scatter argument below.
        assert!(u32::try_from(self.staging.len()).is_ok(), "chunk slice count exceeds u32");
        let mut lane_of: rustc_hash::FxHashMap<i64, usize> = Default::default();
        // Per lane in first-appearance order: the slice count, becoming the
        // scatter cursor once the spans are laid out.
        let mut counts: Vec<u32> = Vec::new();
        let mut tids: Vec<i64> = Vec::new();
        for &(tid, ..) in &self.staging {
            let lane = *lane_of.entry(tid).or_insert_with(|| {
                tids.push(tid);
                counts.push(0);
                counts.len() - 1
            });
            counts[lane] += 1;
        }
        let mut off = 0u32;
        self.lanes = tids
            .iter()
            .zip(&counts)
            .map(|(&tid, &count)| {
                let span = Span32::new(off, count);
                off += count;
                (tid, span)
            })
            .collect();
        let mut cursors: Vec<u32> =
            self.lanes.iter().map(|&(_, s)| s.range().start as u32).collect();
        let total = self.staging.len();
        let mut pool: Vec<(Slice, SliceDetail)> = Vec::with_capacity(total);
        let spare = pool.spare_capacity_mut();
        for &(tid, s, d) in &self.staging {
            let lane = lane_of[&tid];
            spare[cursors[lane] as usize].write((s, d));
            cursors[lane] += 1;
        }
        // Every slot in [0, total) was written exactly once: the cursors
        // advance through disjoint per-lane ranges whose lengths sum to the
        // staging count.
        unsafe { pool.set_len(total) };
        for &(_, span) in &self.lanes {
            pool[span.range()].sort_by(|a, b| slice_order(&a.0, &b.0));
        }
        self.lane_pool = pool;
        self.staging.clear();
    }
}

/// Lane order for a thread's slices: by start ascending, then longer duration
/// first, so an enclosing span sorts before the spans it contains. Equal keys
/// keep their relative order under a stable sort, which the merge's run-index
/// tie-break preserves across builders.
fn slice_order(a: &Slice, b: &Slice) -> Ordering {
    a.start_us.cmp(&b.start_us).then(b.dur_us.cmp(&a.dur_us))
}

/// The incremental builder merge: [`Merge::fold`] absorbs one builder at a
/// time — in array order — so everything except the final slice copy happens
/// as builders arrive, and the pipelined parse hides that work under the
/// decode∥parse phase. Every builder's handles are already global (all chunk
/// workers interned into one shared arena), so a fold only concatenates the
/// builder's per-worker args pool — rebasing the arg-pool indices its slice
/// details carry — unions its counters by their (global) name handle, and
/// unions thread metadata per tid; slice, phase-marker, and counter/series
/// handles carry straight through. Only the builder's slices survive a fold
/// (as a [`FoldedRun`] with the arg-pool base its detail indices rebase by):
/// their copy into the flat pool needs every thread's total, so it waits for
/// [`finish_merge`].
struct Merge {
    args_pool: Vec<(Str, Str)>,
    phase_markers: Vec<PhaseMarker>,
    thread_meta: BTreeMap<i64, (Option<String>, i64)>,
    counter_order: Vec<Str>,
    counters: HashMap<Str, RawLane>,
    runs: Vec<FoldedRun>,
}

struct FoldedRun {
    args_base: u32,
    lane_pool: Vec<(Slice, SliceDetail)>,
    lanes: Vec<(i64, Span32)>,
}

impl Merge {
    fn new() -> Merge {
        Merge {
            args_pool: Vec::new(),
            phase_markers: Vec::new(),
            thread_meta: BTreeMap::new(),
            counter_order: Vec::new(),
            counters: HashMap::new(),
            runs: Vec::new(),
        }
    }

    /// Absorbs one builder. The builder's slices must already be sealed into
    /// per-thread lane order ([`Builder::seal_slices`]): [`finish_merge`]
    /// k-way merges the folded runs rather than re-sorting whole lanes.
    fn fold(&mut self, builder: Builder) {
        let Builder {
            args_pool,
            thread_meta,
            staging: _,
            lane_pool,
            lanes,
            phase_markers,
            counter_order,
            mut counters,
        } = builder;
        // The args pool is per-worker, concatenated here; a detail's `others`
        // span indexes it, so it rebases by the base this builder lands at. The
        // entries themselves are global text handles, unchanged.
        let args_base = self.args_pool.len() as u32;
        self.args_pool.extend(args_pool);
        self.phase_markers.extend(phase_markers);
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
        // A counter name is a global handle, so a lane sampled across chunks
        // keys the same map entry in every builder and unifies without a
        // re-intern.
        for lane_key in counter_order {
            let lane = counters.remove(&lane_key).expect("ordered lane exists");
            let dst = match self.counters.entry(lane.name) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    self.counter_order.push(lane.name);
                    e.insert(RawLane { name: lane.name, series: Vec::new() })
                }
            };
            for series in lane.series {
                match dst.series.iter_mut().find(|s| s.name == series.name) {
                    Some(s) => s.points.extend(series.points),
                    None => dst.series.push(series),
                }
            }
        }
        self.runs.push(FoldedRun { args_base, lane_pool, lanes });
    }
}

/// Seals a folded [`Merge`] whose handles are global into the shared interner:
/// interns the late strings ([`finish_merge`]'s thread names, counter labels,
/// and metadata) through one writer into that same arena, then consumes the
/// interner into the profile's text pool.
fn seal(
    interner: ConcurrentInterner,
    merge: Merge,
    other_data: Option<Map<String, Value>>,
) -> Profile {
    let mut parts = {
        let mut pool = WriterPool { writer: interner.writer(), interner: &interner };
        finish_merge(merge, &mut pool, other_data)
    };
    parts.distinct_strings = interner.distinct();
    parts.text = interner.into_text();
    Profile::from_parts(parts)
}

/// Writes every folded slice exactly once, straight into its final
/// `slice_pool` position: each thread's sorted runs are k-way merged into lane
/// order, with handle rebasing, depth assignment, and span tracking fused into
/// that single write pass, which runs per thread across worker threads. Then
/// seals the merged pools into a [`Profile`].
fn finish_merge(
    merge: Merge,
    pool: &mut WriterPool,
    other_data: Option<Map<String, Value>>,
) -> ProfileParts {
    let Merge {
        args_pool,
        mut phase_markers,
        thread_meta,
        counter_order,
        mut counters,
        runs,
    } = merge;
    // The pools are handed to the merge phase as uninitialized capacity rather
    // than pre-filled: a fill pass would touch every page serially just to be
    // overwritten, so the first (and only) write of each slot happens on the
    // worker that merges it.
    let slice_total: usize = runs.iter().map(|r| r.lane_pool.len()).sum();
    let mut slice_pool: Vec<Slice> = Vec::with_capacity(slice_total);
    let mut detail_pool: Vec<SliceDetail> = Vec::with_capacity(slice_total);
    let mut end_max_pool: Vec<i64> = Vec::with_capacity(slice_total);

    // One source run per (thread, builder): that builder's slices for the
    // thread, in lane order, with the arg-pool base its detail spans rebase by.
    struct Run<'a> {
        src: &'a [(Slice, SliceDetail)],
        args_base: u32,
    }
    // A thread's contiguous destination segments — one per pool, the same
    // index range — its runs in builder order, and the merge phase's
    // per-thread results. The segments start uninitialized; the runs' lengths
    // sum to exactly their length, so the merge phase writes every slot of
    // each exactly once.
    struct TidTask<'a> {
        dest: &'a mut [std::mem::MaybeUninit<Slice>],
        dest_detail: &'a mut [std::mem::MaybeUninit<SliceDetail>],
        dest_end_max: &'a mut [std::mem::MaybeUninit<i64>],
        runs: Vec<Run<'a>>,
        max_depth: u32,
        min_start_us: i64,
        max_end_us: i64,
    }
    /// K-way merges a thread's runs into its destination segments, in
    /// whole-lane [`slice_order`] — on an equal key the lowest run index goes
    /// first, which reproduces a stable whole-lane sort of the runs
    /// concatenated in fold order. Each slot is written exactly once, and that
    /// one write also rebases the detail's arg-pool span by its run's base
    /// (text handles are already global), assigns the slice's depth from the
    /// ancestor stack, and accumulates the lane's time span — whose running end
    /// also fills the slice's end-max slot, giving [`Profile::end_maxes`] its
    /// segment-local prefix maximum. Slice, detail, and end max travel as one
    /// triple, so the three pools stay index-parallel by construction.
    fn merge_lane(task: &mut TidTask) {
        let TidTask { dest, dest_detail, dest_end_max, runs, max_depth, min_start_us, max_end_us } =
            task;
        let mut slots = dest.iter_mut();
        let mut detail_slots = dest_detail.iter_mut();
        let mut end_max_slots = dest_end_max.iter_mut();
        // Ancestor end-times. A slice is nested only in a span that contains
        // it, so pop while the stack top ends before the slice does
        // (`end < slice_end`); popping on end-before-start alone would keep
        // merely overlapping spans stacked — a span that starts inside another
        // but ends after it is a sibling, not a child — building a runaway
        // staircase of false depth.
        let mut stack: Vec<i64> = Vec::new();
        let mut emit = |run: &Run, src: &(Slice, SliceDetail)| {
            let (mut s, mut d) = *src;
            d.others = d.others.shifted(run.args_base);
            let end = s.start_us.saturating_add(s.dur_us);
            while stack.last().is_some_and(|&e| e < end) {
                stack.pop();
            }
            s.depth = stack.len() as u32;
            *max_depth = (*max_depth).max(s.depth + 1);
            stack.push(end);
            *min_start_us = (*min_start_us).min(s.start_us);
            *max_end_us = (*max_end_us).max(end);
            slots.next().expect("segment sized to the runs' total").write(s);
            detail_slots.next().expect("segment sized to the runs' total").write(d);
            end_max_slots.next().expect("segment sized to the runs' total").write(*max_end_us);
        };
        if let [run] = &runs[..] {
            for src in run.src {
                emit(run, src);
            }
        } else {
            let mut cursors = vec![0usize; runs.len()];
            let mut heap: BinaryHeap<Reverse<(i64, Reverse<i64>, usize)>> =
                BinaryHeap::with_capacity(runs.len());
            for (i, run) in runs.iter().enumerate() {
                if let Some((s, _)) = run.src.first() {
                    heap.push(Reverse((s.start_us, Reverse(s.dur_us), i)));
                }
            }
            while let Some(Reverse((.., i))) = heap.pop() {
                let run = &runs[i];
                let src = &run.src[cursors[i]];
                cursors[i] += 1;
                emit(run, src);
                if let Some((next, _)) = run.src.get(cursors[i]) {
                    heap.push(Reverse((next.start_us, Reverse(next.dur_us), i)));
                }
            }
        }
    }
    let mut tid_runs: BTreeMap<i64, Vec<Run>> = BTreeMap::new();
    for folded in &runs {
        for &(tid, span) in &folded.lanes {
            tid_runs.entry(tid).or_default().push(Run {
                src: &folded.lane_pool[span.range()],
                args_base: folded.args_base,
            });
        }
    }
    let mut segments: Vec<(i64, Span32)> = Vec::with_capacity(tid_runs.len());
    let mut tasks: Vec<TidTask> = Vec::with_capacity(tid_runs.len());
    let mut rest = slice_pool.spare_capacity_mut();
    let mut rest_detail = detail_pool.spare_capacity_mut();
    let mut rest_end_max = end_max_pool.spare_capacity_mut();
    let mut off = 0u32;
    for (tid, runs) in tid_runs {
        let len: usize = runs.iter().map(|r| r.src.len()).sum();
        let (dest, tail) = std::mem::take(&mut rest).split_at_mut(len);
        rest = tail;
        let (dest_detail, tail) = std::mem::take(&mut rest_detail).split_at_mut(len);
        rest_detail = tail;
        let (dest_end_max, tail) = std::mem::take(&mut rest_end_max).split_at_mut(len);
        rest_end_max = tail;
        segments.push((tid, Span32::new(off, len as u32)));
        off += len as u32;
        tasks.push(TidTask {
            dest,
            dest_detail,
            dest_end_max,
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
                    merge_lane(task);
                });
            }
        });
    });
    let results: Vec<(u32, i64, i64)> = tasks
        .into_iter()
        .map(|t| (t.max_depth, t.min_start_us, t.max_end_us))
        .collect();
    // SAFETY: the segments partition the first `slice_total` slots of each
    // pool's spare capacity, each merge wrote its whole segment in all three
    // pools (the runs' lengths sum to the segment length, and every emit
    // writes one slot of each), and the merge scope joined all their writers.
    unsafe { slice_pool.set_len(slice_total) };
    unsafe { detail_pool.set_len(slice_total) };
    unsafe { end_max_pool.set_len(slice_total) };

    let parts = timed("    tail", || {
        phase_markers.sort_by_key(|m| m.start_us);

        let metadata = build_metadata(&mut pool.writer, other_data.as_ref());

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
                    name: pool.writer.intern(&name),
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
            let counter_name = pool.interner.resolve(lane.name).to_string();
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
                let key = pool.interner.resolve(s.name).to_string();
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
                let name = pool.writer.intern(&l.label);
                let series_off = series_pool.len() as u32;
                if let Some(mut s) = l.series {
                    let points_off = point_pool.len() as u32;
                    let points_len = s.points.len() as u32;
                    // Consumers binary-search points by time, so the sealed
                    // series must be sorted; the trace format does not promise
                    // that counter samples arrive in time order.
                    if !s.points.is_sorted_by_key(|p| p.ts_us) {
                        s.points.sort_by_key(|p| p.ts_us);
                    }
                    let max_value = s
                        .points
                        .iter()
                        .map(|p| p.value)
                        .fold(f64::NEG_INFINITY, f64::max);
                    point_pool.extend(s.points);
                    series_pool.push(CounterSeries {
                        name: s.name,
                        points: Span32::new(points_off, points_len),
                        max_value,
                    });
                }
                let series_len = series_pool.len() as u32 - series_off;
                CounterLane {
                    name,
                    series: Span32::new(series_off, series_len),
                }
            })
            .collect();

        // The caller seals its owned interner into `text` once this returns and
        // the late-interning writer is dropped.
        ProfileParts {
            text: String::new(),
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
            // Filled by `seal` from the interner before it is consumed.
            distinct_strings: 0,
        }
    });

    // The folded runs' teardown (per-chunk lane pools) is pure
    // page-return work; detached so the load doesn't wait on it.
    std::thread::spawn(move || drop(runs));
    parts
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
pub fn worker_count() -> usize {
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
    // Every worker interns into this one arena, so a builder's handles are
    // global and merging never rebases them. The distinct pool is bounded by
    // the region's byte length.
    let interner = ConcurrentInterner::new(region.len(), region.len() / BYTES_PER_DISTINCT);
    let builders: Vec<Option<Builder>> = timed("  parse chunks", || {
        let next = std::sync::atomic::AtomicUsize::new(0);
        let interner = &interner;
        let parsed: Vec<(usize, Option<Builder>)> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..n.min(chunks.len()))
                .map(|_| {
                    scope.spawn(|| {
                        let mut writer = interner.writer();
                        let mut parsed = Vec::new();
                        let mut staging = Vec::new();
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let Some(&chunk) = chunks.get(i) else { break };
                            let mut builder = Builder::new();
                            builder.staging = std::mem::take(&mut staging);
                            let ok = parse_range_into(&mut builder, &mut writer, chunk);
                            if ok {
                                builder.seal_slices();
                            }
                            staging = std::mem::take(&mut builder.staging);
                            staging.clear();
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
    Some(timed("  assemble", || {
        let mut merge = Merge::new();
        for builder in builders {
            merge.fold(builder);
        }
        seal(interner, merge, other_data)
    }))
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
struct Piece<'a> {
    idx: usize,
    buf: PieceBuf<'a>,
}

/// The bytes behind a piece: normally a fixed-size part of the shared slab,
/// carrying its filled length; a run too long for a part (an event line
/// longer than a chunk) rides in a one-off owned buffer instead, dropped
/// after its parse rather than returned to the ring.
enum PieceBuf<'a> {
    Slab { data: &'a mut [u8], len: usize },
    Owned(Vec<u8>),
}

impl PieceBuf<'_> {
    /// The whole writable region, independent of the filled length.
    fn region(&mut self) -> &mut [u8] {
        match self {
            PieceBuf::Slab { data, .. } => data,
            PieceBuf::Owned(v) => v,
        }
    }

    /// The filled prefix a consumer may read.
    fn bytes(&self) -> &[u8] {
        match self {
            PieceBuf::Slab { data, len } => &data[..*len],
            PieceBuf::Owned(v) => v,
        }
    }

    fn set_len(&mut self, n: usize) {
        match self {
            PieceBuf::Slab { len, .. } => *len = n,
            PieceBuf::Owned(v) => v.truncate(n),
        }
    }
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
/// queue when the workers are behind and on the buffer ring when every part
/// is in flight (a return is then already on its way; see [`parse_gz_stream`]
/// on the ring size). `None` on a decode error, an input the chunker can't
/// navigate (no array, no closing bracket, an over-long line), or a closed
/// queue.
fn stream_pieces<'a>(
    bytes: &[u8],
    chunk_bytes: usize,
    tx: std::sync::mpsc::SyncSender<Piece<'a>>,
    pool: std::sync::mpsc::Receiver<&'a mut [u8]>,
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
    // A slab part put aside when its chunk held no newline (the run moved to
    // `carry` for the owned-buffer path), reused ahead of the pool.
    let mut stash: Vec<&'a mut [u8]> = Vec::new();
    loop {
        // A carry too long for a slab part means the previous chunk held no
        // newline; doubling the owned buffer lets the run keep growing.
        let mut buf = if carry.len() * 2 > chunk_bytes {
            PieceBuf::Owned(vec![0u8; carry.len() * 2])
        } else {
            let data = match stash.pop() {
                Some(data) => data,
                None => pool.recv().ok()?,
            };
            PieceBuf::Slab { data, len: 0 }
        };
        let (filled, end_of_stream) = {
            let region = buf.region();
            region[..carry.len()].copy_from_slice(&carry);
            let filled = fill(&mut dec, region, carry.len()).ok()?;
            (filled, filled < region.len())
        };
        buf.set_len(filled);
        if end_of_stream {
            // End of stream: everything before the last `]` is event content,
            // the bracket onward may carry a trailing otherData.
            let close = buf.bytes().iter().rposition(|&b| b == b']')?;
            let tail = buf.bytes()[close..].to_vec();
            buf.set_len(close);
            if close > 0 {
                tx.send(Piece { idx: pieces, buf }).ok()?;
                pieces += 1;
            }
            return Some(StreamEnds { head, tail, pieces });
        }
        match buf.bytes().iter().rposition(|&b| b == b'\n') {
            Some(newline) => {
                let cut = newline + 1;
                carry.clear();
                carry.extend_from_slice(&buf.bytes()[cut..]);
                buf.set_len(cut);
                tx.send(Piece { idx: pieces, buf }).ok()?;
                pieces += 1;
            }
            None if filled >= PIPELINE_MAX_EVENT_BYTES => return None,
            None => {
                carry.clear();
                carry.extend_from_slice(buf.bytes());
                if let PieceBuf::Slab { data, .. } = buf {
                    stash.push(data);
                }
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
    // The buffer ring is one slab split into fixed-size parts — a single
    // zeroed allocation whose pages are first touched by the inflate's own
    // writes — holding one part more than the piece queue and the workers can
    // have in flight together, so whenever the producer blocks on the pool a
    // return is already on its way.
    let ring = 2 * workers + 1;
    let mut slab: Vec<u8> = vec![0u8; ring * chunk_bytes];
    let (tx, rx) = std::sync::mpsc::sync_channel::<Piece>(workers);
    let rx = std::sync::Mutex::new(rx);
    let (builder_tx, builder_rx) = std::sync::mpsc::channel::<(usize, Option<Builder>)>();
    let (pool_tx, pool_rx) = std::sync::mpsc::channel::<&mut [u8]>();
    for part in slab.chunks_mut(chunk_bytes) {
        let _ = pool_tx.send(part);
    }
    // Every worker interns into this one arena, so a builder's handles are
    // global and the merger never rebases them. The distinct pool is bounded by
    // the decoded size the ISIZE trailer declares.
    let decoded_size = gzip_isize(bytes).unwrap_or(0);
    let interner = ConcurrentInterner::new(decoded_size, decoded_size / BYTES_PER_DISTINCT);
    let (ends, merged) = timed("  decode+parse+fold chunks", || {
        let interner = &interner;
        std::thread::scope(|scope| {
            let producer = scope.spawn(move || stream_pieces(bytes, chunk_bytes, tx, pool_rx));
            for _ in 0..workers {
                let builder_tx = builder_tx.clone();
                let pool_tx = pool_tx.clone();
                let rx = &rx;
                scope.spawn(move || {
                    let mut writer = interner.writer();
                    let mut staging = Vec::new();
                    loop {
                        let piece = match rx.lock().unwrap().recv() {
                            Ok(piece) => piece,
                            Err(_) => break,
                        };
                        let text = latin1_cow(piece.buf.bytes());
                        let mut builder = Builder::new();
                        builder.staging = std::mem::take(&mut staging);
                        let ok = parse_range_into(&mut builder, &mut writer, &text);
                        drop(text);
                        // Hand a slab part back to the producer before
                        // sealing; everything the builder keeps was interned
                        // during the parse, and the inflate must not wait out
                        // the per-lane sorts.
                        if let PieceBuf::Slab { data, .. } = piece.buf {
                            let _ = pool_tx.send(data);
                        }
                        if ok {
                            builder.seal_slices();
                        }
                        staging = std::mem::take(&mut builder.staging);
                        staging.clear();
                        if builder_tx.send((piece.idx, ok.then_some(builder))).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(pool_tx);
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
    Some(timed("  finish", || seal(interner, merge, other_data)))
}

/// Parses every `{...}` event object in one chunk of the `traceEvents` array
/// into `builder`, in place: each event goes through [`scan_event`], with
/// serde deserialization from the same offset as the fallback for whatever
/// the scanner defers, and the comma/whitespace separators between events are
/// skipped by hand. Returns `false` on malformed content so the caller falls
/// back to the serial parser rather than silently dropping events.
fn parse_range_into(builder: &mut Builder, interner: &mut Writer, chunk: &str) -> bool {
    let bytes = chunk.as_bytes();
    let mut scratch: Vec<ArgEntry> = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        match bytes[pos] {
            b' ' | b'\t' | b'\n' | b'\r' | b',' => pos += 1,
            b'{' => {
                if let Some((ev, end)) = scan_event(chunk, pos, &mut scratch) {
                    builder.event(interner, ev, &scratch);
                    pos = end;
                    continue;
                }
                let mut de = serde_json::Deserializer::from_str(&chunk[pos..]);
                match RawEventSeed(&mut scratch).deserialize(&mut de) {
                    Ok(ev) => {
                        builder.event(interner, ev, &scratch);
                        // The deserializer doesn't expose its consumed-byte
                        // count directly, but converting it into a stream
                        // iterator carries the count over to `byte_offset`.
                        pos += de.into_iter::<IgnoredAny>().byte_offset();
                    }
                    Err(_) => return false,
                }
            }
            _ => return false,
        }
    }
    true
}

fn build_metadata(interner: &mut Writer, other: Option<&Map<String, Value>>) -> Metadata {
    let mut get = |k: &str| {
        let v = other.and_then(|m| m.get(k)).and_then(Value::as_str);
        v.map(|v| interner.intern(v))
    };
    Metadata {
        bazel_version: get("bazel_version"),
        build_id: get("build_id"),
        output_base: get("output_base"),
        date: get("date"),
    }
}

fn apply_metadata_event(
    ev: &RawEvent,
    args: Option<&[ArgEntry]>,
    thread_meta: &mut BTreeMap<i64, (Option<String>, i64)>,
) {
    let tid = ev.tid.unwrap_or(0);
    let entry = thread_meta.entry(tid).or_insert((None, MAX_SORT_INDEX));
    match ev.name.as_deref() {
        Some("thread_name") => {
            if let Some(name) = args.and_then(|a| arg_str(a, "name")) {
                entry.0 = Some(name.to_owned());
            }
        }
        Some("thread_sort_index") => {
            if let Some(idx) = args.and_then(|a| arg_i64(a, "sort_index")) {
                entry.1 = idx;
            }
        }
        _ => {}
    }
}

fn build_slice_detail(
    interner: &mut Writer,
    args_pool: &mut Vec<(Str, Str)>,
    category: Str,
    args: Option<&[ArgEntry]>,
) -> SliceDetail {
    let mut out = SliceDetail {
        category,
        ..SliceDetail::default()
    };
    let Some(args) = args else {
        return out;
    };
    let others_off = args_pool.len() as u32;
    for (k, v) in args {
        // The typed fields are strings; a non-string value under one of those
        // keys is dropped, everything else is kept for display.
        match k.as_ref() {
            "target" => {
                if let ArgVal::Str(s) = v {
                    out.target = Some(interner.intern(s)).into();
                }
            }
            "mnemonic" => {
                if let ArgVal::Str(s) = v {
                    out.mnemonic = Some(interner.intern(s)).into();
                }
            }
            "configuration" => {
                if let ArgVal::Str(s) = v {
                    out.configuration = Some(interner.intern(s)).into();
                }
            }
            _ => {
                let key = interner.intern(k);
                // A string value interns straight from the borrowed input; a
                // numeric one formats to its display string first.
                let val = match v {
                    ArgVal::Str(s) => interner.intern(s),
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
    interner: &mut Writer,
    ev: &RawEvent,
    args: Option<&[ArgEntry]>,
    order: &mut Vec<Str>,
    counters: &mut HashMap<Str, RawLane>,
) {
    let lane_name = interner.intern(ev.name.as_deref().unwrap_or(""));
    let ts = round_us(ev.ts);
    let Some(args) = args else {
        return;
    };
    let lane = counters.entry(lane_name).or_insert_with(|| {
        order.push(lane_name);
        RawLane {
            name: lane_name,
            series: Vec::new(),
        }
    });
    for (key, value) in args {
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
                    "  slice {:?} cat={:?} ts={} dur={} depth={} wc={:?} tgt={:?} mn={:?} cfg={:?}",
                    p.str(s.name),
                    p.str(d.category),
                    s.start_us,
                    s.dur_us,
                    s.depth,
                    s.work_class,
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
}
