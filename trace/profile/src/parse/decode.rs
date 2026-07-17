use std::borrow::Cow;
use std::io::Read;

use flate2::read::MultiGzDecoder;

use super::{timed, GZIP_MAGIC, LoadError};

pub(crate) fn decode(bytes: &[u8]) -> Result<String, LoadError> {
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
pub(crate) fn gzip_isize(bytes: &[u8]) -> Option<usize> {
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
/// case (already valid UTF-8) borrows the buffer with no per-byte work; otherwise
/// [`latin1_expand`] rewrites it.
pub(crate) fn latin1_cow(raw: &[u8]) -> Cow<'_, str> {
    if raw.is_ascii() {
        // SAFETY: ASCII is valid UTF-8.
        return Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(raw) });
    }
    Cow::Owned(latin1_expand(raw))
}

/// [`latin1_cow`] over an owned buffer: the all-ASCII case reuses it in place,
/// otherwise it takes the expanded string.
pub(crate) fn latin1_to_utf8(raw: Vec<u8>) -> String {
    match latin1_cow(&raw) {
        Cow::Borrowed(_) => String::from_utf8(raw).expect("ascii is valid utf-8"),
        Cow::Owned(expanded) => expanded,
    }
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
