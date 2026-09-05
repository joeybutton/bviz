use std::borrow::Cow;
use std::io::{Read, Seek, SeekFrom};

use flate2::read::MultiGzDecoder;

use super::{GZIP_MAGIC, LoadError, timed};

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

/// The gzip framing alone: a 10-byte header plus an 8-byte trailer, with no
/// deflate payload between them. Below this no ISIZE field is framed at all,
/// whatever the tail bytes hold.
const GZIP_FRAMING_MIN: usize = 18;

/// Width of the gzip ISIZE trailer field (RFC 1952).
const GZIP_ISIZE_BYTES: usize = 4;

/// The uncompressed length declared by a gzip stream's little-endian ISIZE
/// trailer (the length mod 2^32, exact under 4 GiB). `None` for a zero
/// trailer.
fn isize_from_trailer(trailer: &[u8; GZIP_ISIZE_BYTES]) -> Option<usize> {
    match u32::from_le_bytes(*trailer) as usize {
        0 => None,
        n => Some(n),
    }
}

/// The uncompressed length a gzip stream declares in its ISIZE trailer (the
/// length mod 2^32, exact under 4 GiB). `None` for input that is not gzip, is
/// too short to frame, or carries a zero trailer.
pub(crate) fn gzip_isize(bytes: &[u8]) -> Option<usize> {
    if !bytes.starts_with(&GZIP_MAGIC) || bytes.len() < GZIP_FRAMING_MIN {
        return None;
    }
    isize_from_trailer(&bytes[bytes.len() - GZIP_ISIZE_BYTES..].try_into().ok()?)
}

/// Reads a gzip stream's ISIZE trailer directly from an open file without
/// pulling the whole stream into memory: verifies the leading [`GZIP_MAGIC`],
/// then seeks to the ISIZE field. `Ok(None)` if the file isn't gzip, is shorter
/// than a full frame, or the trailer is zero. The file cursor is left wherever
/// the reads and seeks land; the caller re-seeks before use.
pub(crate) fn gzip_isize_from_file(file: &mut std::fs::File) -> std::io::Result<Option<usize>> {
    let mut magic = [0u8; 2];
    if file.read_exact(&mut magic).is_err() || magic != GZIP_MAGIC {
        return Ok(None);
    }
    if file.metadata().map_or(0, |m| m.len()) < GZIP_FRAMING_MIN as u64 {
        return Ok(None);
    }
    if file
        .seek(SeekFrom::End(-(GZIP_ISIZE_BYTES as i64)))
        .is_err()
    {
        return Ok(None);
    }
    let mut trailer = [0u8; GZIP_ISIZE_BYTES];
    if file.read_exact(&mut trailer).is_err() {
        return Ok(None);
    }
    Ok(isize_from_trailer(&trailer))
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
