use std::borrow::Cow;
use std::fmt;
use std::marker::PhantomData;

use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

use crate::concurrent_interner::{ConcurrentInterner, Writer};
use crate::model::Profile;

use super::build::{Builder, Merge, seal};
use super::{BYTES_PER_DISTINCT, merge_other_data};

/// Carries the decoded text's byte length so [`Self::visit_map`] can size the
/// interner it fills, since the map visitor never sees the whole text.
pub(crate) struct ProfileVisitor {
    pub(crate) text_len: usize,
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
                    "otherData" => {
                        let seen = map.next_value::<Option<Map<String, Value>>>()?;
                        other_data = merge_other_data(other_data, seen);
                    }
                    "traceEvents" => map.next_value_seed(EventsSeed(&mut builder, &mut writer))?,
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

struct EventSeed<'b, 'de, 'w>(
    &'b mut Builder,
    &'b mut Writer<'w>,
    &'b mut Vec<ArgEntry<'de>>,
);

impl<'de, 'w> DeserializeSeed<'de> for EventSeed<'_, 'de, 'w> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        let ev = RawEventSeed(&mut *self.2).deserialize(d)?;
        self.0.event(self.1, ev, self.2);
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct RawEvent<'a> {
    pub(crate) ph: Option<Cow<'a, str>>,
    pub(crate) cat: Option<Cow<'a, str>>,
    pub(crate) name: Option<Cow<'a, str>>,
    pub(crate) ts: Option<f64>,
    pub(crate) dur: Option<f64>,
    pub(crate) tid: Option<i64>,
    /// Whether the event carried an `args` object (even an empty one — a
    /// counter keeps an empty lane for it); the entries themselves live in the
    /// caller's scratch buffer.
    pub(crate) has_args: bool,
}

/// One kept argument value: a string or a number. Other JSON shapes (array,
/// object, null) are dropped, matching what every `args` consumer reads — they
/// only ever look at string or numeric fields.
pub(crate) enum ArgVal<'a> {
    Str(Cow<'a, str>),
    Int(i64),
    Float(f64),
}

impl ArgVal<'_> {
    /// The value as an `f64` for counter series, or `None` for a non-numeric.
    pub(crate) fn as_f64(&self) -> Option<f64> {
        match self {
            ArgVal::Int(n) => Some(*n as f64),
            ArgVal::Float(n) => Some(*n),
            ArgVal::Str(_) => None,
        }
    }
}

/// One kept entry of an event's `args` object: a scalar keyed by its arg name.
/// Args are a flat key/value list every consumer (slice args, thread metadata,
/// counters) reads directly. Entries accumulate into a caller-owned scratch
/// vector reused across events, so an event's args cost no per-event allocation.
pub(crate) type ArgEntry<'a> = (Cow<'a, str>, ArgVal<'a>);

/// The value for `key` among an event's arg entries, if present.
fn find_arg<'e>(entries: &'e [ArgEntry], key: &str) -> Option<&'e ArgVal<'e>> {
    entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// The string value for `key` among an event's arg entries, if present and a
/// string.
pub(crate) fn arg_str<'e>(entries: &'e [ArgEntry], key: &str) -> Option<&'e str> {
    match find_arg(entries, key)? {
        ArgVal::Str(s) => Some(s.as_ref()),
        _ => None,
    }
}

/// The integer value for `key` among an event's arg entries, if present and an
/// integer.
pub(crate) fn arg_i64(entries: &[ArgEntry], key: &str) -> Option<i64> {
    match find_arg(entries, key)? {
        ArgVal::Int(n) => Some(*n),
        _ => None,
    }
}

/// Deserializes one event object; its `args` entries land in the borrowed
/// scratch (cleared first), so the scratch holds exactly this event's entries
/// when `has_args` is set.
pub(crate) struct RawEventSeed<'s, 'a>(pub(crate) &'s mut Vec<ArgEntry<'a>>);

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
        let mut ev = RawEvent::default();
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
            i64::try_from(v)
                .map(ArgVal::Int)
                .unwrap_or(ArgVal::Float(v as f64)),
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
