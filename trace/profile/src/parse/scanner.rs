use std::borrow::Cow;

use super::serde_events::{ArgEntry, ArgVal, RawEvent};

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
pub(crate) fn scan_event<'a>(
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
            "ts" => ev.ts = s.scan_opt_number(IntTok::to_f64)?,
            "dur" => ev.dur = s.scan_opt_number(IntTok::to_f64)?,
            "tid" => ev.tid = s.scan_opt_number(IntTok::to_i64)?,
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
        let escaped = self.scan_string_body()?;
        let raw = &self.text[start..self.pos];
        self.pos += 1;
        if escaped {
            unescape(raw).map(Cow::Owned)
        } else {
            Some(Cow::Borrowed(raw))
        }
    }

    /// Advances the cursor from a string body to its closing quote (left
    /// unconsumed), reporting whether the body held an escape. `None` on a
    /// control byte or end of input; a `\uXXXX` escape passes here and is
    /// deferred to [`unescape`] by the caller that materializes the token.
    fn scan_string_body(&mut self) -> Option<bool> {
        let mut escaped = false;
        loop {
            self.pos = find_special(self.bytes, self.pos)?;
            match self.bytes[self.pos] {
                b'"' => return Some(escaped),
                b'\\' => {
                    escaped = true;
                    self.pos += 2;
                }
                _ => return None,
            }
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

    /// An integer-or-null field value, converted by `conv`; an out-of-range
    /// magnitude defers (and serde then errors, failing the chunk).
    fn scan_opt_number<T>(&mut self, conv: impl FnOnce(&IntTok) -> Option<T>) -> Option<Option<T>> {
        match *self.bytes.get(self.pos)? {
            b'-' | b'0'..=b'9' => {
                let tok = self.scan_int()?;
                Some(Some(conv(&tok)?))
            }
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
            b'{' => {
                self.pos = object_end(self.bytes, self.pos)? + 1;
                Some(())
            }
            b'[' => {
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
        self.scan_string_body()?;
        self.pos += 1;
        Some(())
    }
}

/// Index of the next byte a string scan must stop at — a quote, a backslash,
/// or a control byte — or `None` at end of input. Word-at-a-time via the
/// haszero/hasless formulas of Sean Anderson's "Bit Twiddling Hacks"; this is
/// the parser's hottest loop. Spurious high bits from borrow ripple only ever
/// sit above a genuine match, so the lowest set bit is always exact.
pub(crate) fn find_special(bytes: &[u8], mut i: usize) -> Option<usize> {
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

/// Byte index of the `}` closing the object whose `{` is at `open`, counting
/// braces while skipping those inside string literals (respecting `\"`
/// escapes). `None` if the object is never closed before end of input.
pub(crate) fn object_end(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for (offset, &c) in bytes[open..].iter().enumerate() {
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
                        return Some(open + offset);
                    }
                }
                _ => {}
            }
        }
    }
    None
}
