use std::borrow::Cow;
use std::collections::BTreeMap;
use std::io::Read;

use flate2::read::MultiGzDecoder;
use serde::de::{DeserializeSeed, IgnoredAny};

use crate::concurrent_interner::{ConcurrentInterner, Writer};
use crate::model::Profile;
use crate::worker_count;

use super::build::{seal, Builder, Merge};
use super::decode::{gzip_isize, latin1_cow, latin1_to_utf8};
use super::scanner::scan_event;
use super::serde_events::{ArgEntry, RawEventSeed};
use super::{intern_prefault_bytes, recover_other_data, timed, BYTES_PER_DISTINCT, BYTES_PER_SLICE};

/// Outcome of locating the `traceEvents` array in a decoded profile buffer.
pub(crate) enum TraceEventsArray {
    /// No `"traceEvents"` key in the buffer.
    NoKey,
    /// The key is present but no `[` opens an array after it.
    NoArray,
    Found {
        /// Byte index of the `"traceEvents"` key's opening quote.
        key_at: usize,
        /// Byte index just past the `[`, at the start of the array's contents.
        content_start: usize,
    },
}

/// Locates the `traceEvents` array: the `"traceEvents"` key, then the first `[`
/// after it.
pub(crate) fn find_trace_events_array(bytes: &[u8]) -> TraceEventsArray {
    const KEY: &[u8] = b"\"traceEvents\"";
    let Some(key_at) = find_bytes(bytes, KEY) else {
        return TraceEventsArray::NoKey;
    };
    let after_key = key_at + KEY.len();
    match bytes[after_key..].iter().position(|&b| b == b'[') {
        Some(bracket) => TraceEventsArray::Found { key_at, content_start: after_key + bracket + 1 },
        None => TraceEventsArray::NoArray,
    }
}

/// Below this many bytes of `traceEvents` the parallel parser's thread and merge
/// overhead outweighs the gain, so [`parse_parallel`] declines and the serial
/// path runs.
const MIN_PARALLEL_BYTES: usize = 8 << 20;

/// Parses the `traceEvents` array across several threads when the profile is
/// large enough to repay it, returning `None` so the caller runs the serial
/// [`parse_streaming`] for a small input, a single core, or any input whose
/// shape the chunker can't navigate (no array, no usable split points, a chunk
/// that fails to parse).
pub(crate) fn parse_parallel(text: &str) -> Option<Profile> {
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

/// One chunk handed to a worker: its bytes as text, plus the cleanup owed once
/// the parse has read them. The pre-split path borrows the shared buffer and
/// owes nothing; the streamed path hands its slab part back to the producer to
/// refill.
trait Chunk {
    fn text(&self) -> Cow<'_, str>;
    fn release(self);
}

impl Chunk for &str {
    fn text(&self) -> Cow<'_, str> {
        Cow::Borrowed(self)
    }
    fn release(self) {}
}

/// Parses one chunk into a fresh [`Builder`] on a worker, pre-sizing its
/// staging buffer from the chunk's byte length (via [`BYTES_PER_SLICE`]) so it
/// never regrows, and its arg pool from the previous chunk's count. Both are
/// live data the builder carries to the merge — the staging slices become the
/// folded run's payloads, the arg pool the base its detail spans rebase by — so
/// neither can be reclaimed; chunks are near-equal in bytes and so in arg
/// count, so the `args_cap` hint lets the next builder fill without regrowing
/// from empty. The chunk is released between the parse and the seal:
/// everything the builder keeps was interned during the parse, so a streamed
/// chunk's slab part returns to the producer without the inflate waiting out
/// the per-lane sorts. Returns `None` on a parse failure.
fn parse_chunk(
    chunk: impl Chunk,
    writer: &mut Writer,
    args_cap: &mut usize,
) -> Option<Builder> {
    let mut builder = Builder::new();
    builder.args_pool.reserve(*args_cap);
    let ok = {
        let text = chunk.text();
        builder.staging.reserve(text.len() / BYTES_PER_SLICE);
        let ok = parse_range_into(&mut builder, writer, &text);
        *args_cap = builder.args_pool.len();
        ok
    };
    chunk.release();
    if ok {
        builder.seal_slices();
    }
    ok.then_some(builder)
}

/// Splits the `traceEvents` array into byte chunks at newline boundaries — a
/// literal newline never falls inside a JSON string, so it always lies between
/// events — parses each chunk into its own [`Builder`] on a pool of `n` worker
/// threads pulling chunks from a shared queue, then assembles the builders in
/// array order. `min_bytes` gates the array size below which it
/// declines. Returns `None` if the array can't be located, no split yields more
/// than one chunk, or any chunk fails to parse.
pub(crate) fn parse_chunks(text: &str, n: usize, min_bytes: usize) -> Option<Profile> {
    let TraceEventsArray::Found { content_start: array_start, .. } =
        find_trace_events_array(text.as_bytes())
    else {
        return None;
    };
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
    let prefault_bytes = intern_prefault_bytes(region.len());
    let builders: Vec<Option<Builder>> = timed("  parse chunks", || {
        let next = std::sync::atomic::AtomicUsize::new(0);
        let interner = &interner;
        let workers = n.min(chunks.len());
        let parsed: Vec<(usize, Option<Builder>)> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..workers)
                .map(|shard| {
                    let next = &next;
                    let chunks = &chunks;
                    scope.spawn(move || {
                        // Fault this worker's arena share up front, in parallel
                        // with the others, before entering the steal loop.
                        interner.prefault_shard(shard, workers, prefault_bytes);
                        let mut writer = interner.writer();
                        let mut parsed = Vec::new();
                        let mut args_cap = 0;
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let Some(&chunk) = chunks.get(i) else { break };
                            let builder = parse_chunk(chunk, &mut writer, &mut args_cap);
                            parsed.push((i, builder));
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
        let mut merge = Merge::with_capacity(
            builders.len(),
            builders.iter().map(|b| b.args_pool.len()).sum(),
        );
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

/// The pipelined decode of a gzipped profile held whole in memory, sized from
/// that buffer's own ISIZE trailer. Returns `None` — leaving the input to the
/// whole-buffer path — for a missing or zero trailer, a size
/// [`pipeline_params`] declines, or any decode or parse failure.
pub(crate) fn parse_gz_pipelined(bytes: &[u8]) -> Option<Profile> {
    let decoded_size = gzip_isize(bytes)?;
    let (workers, chunk_bytes) = pipeline_params(decoded_size)?;
    parse_gz_stream_from(bytes, workers, chunk_bytes, decoded_size)
}

/// The pipelined decode over a streaming reader whose ISIZE trailer was read
/// ahead of the stream: [`parse_gz_pipelined`] without the whole compressed
/// buffer in hand. `None` for the same size gates.
pub(crate) fn parse_gz_pipelined_reader(
    reader: impl Read + Send,
    decoded_size: usize,
) -> Option<Profile> {
    let (workers, chunk_bytes) = pipeline_params(decoded_size)?;
    parse_gz_stream_from(reader, workers, chunk_bytes, decoded_size)
}

/// Worker count and per-chunk byte target for a pipelined decode of a stream
/// declaring `decoded_size` uncompressed bytes, or `None` when the pipeline
/// declines — a single core, or a stream under the parallel threshold.
pub(crate) fn pipeline_params(decoded_size: usize) -> Option<(usize, usize)> {
    let workers = worker_count();
    if workers < 2 || decoded_size < MIN_PARALLEL_BYTES {
        return None;
    }
    let chunk_bytes = (decoded_size / (workers * CHUNKS_PER_WORKER))
        .clamp(MIN_CHUNK_BYTES, PIPELINE_MAX_CHUNK_BYTES);
    Some((workers, chunk_bytes))
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

/// A streamed chunk: its decoded bytes read as Latin-1, and a slab part that
/// goes back to the producer's ring once parsed (an owned run keeps nothing to
/// return).
struct StreamChunk<'buf, 'pool> {
    buf: PieceBuf<'buf>,
    pool: &'pool std::sync::mpsc::Sender<&'buf mut [u8]>,
}

impl Chunk for StreamChunk<'_, '_> {
    fn text(&self) -> Cow<'_, str> {
        latin1_cow(self.buf.bytes())
    }

    fn release(self) {
        if let PieceBuf::Slab { data, .. } = self.buf {
            let _ = self.pool.send(data);
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
/// is in flight (a return is then already on its way). `None` on a decode
/// error, an input the chunker can't navigate (no array, no closing bracket,
/// an over-long line), or a closed queue.
fn stream_pieces<'a>(
    reader: impl Read + Send,
    chunk_bytes: usize,
    tx: crossbeam_channel::Sender<Piece<'a>>,
    pool: std::sync::mpsc::Receiver<&'a mut [u8]>,
) -> Option<StreamEnds> {
    let mut dec = MultiGzDecoder::new(reader);

    // Grow the first buffer until the traceEvents array opens inside it; the
    // bytes before the bracket become `head`, the rest seed the chunk loop.
    let mut buf = vec![0u8; chunk_bytes];
    let mut filled = fill(&mut dec, &mut buf, 0).ok()?;
    let array_start = loop {
        if let TraceEventsArray::Found { content_start, .. } =
            find_trace_events_array(&buf[..filled])
        {
            break content_start;
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

/// Takes the decoded size from the stream's own ISIZE trailer, or zero when it
/// declares none.
#[cfg(test)]
pub(crate) fn parse_gz_stream(bytes: &[u8], workers: usize, chunk_bytes: usize) -> Option<Profile> {
    parse_gz_stream_from(bytes, workers, chunk_bytes, gzip_isize(bytes).unwrap_or(0))
}

/// Runs a decode producer over `reader`, `workers` chunk parsers, and a merger,
/// cutting pieces of about `chunk_bytes` from a stream declaring `decoded_size`
/// uncompressed bytes. The piece queue is bounded by the worker count, so the
/// producer can decode no further than that many pieces ahead of the parsers;
/// transcoding runs per chunk on the workers, off the decode leg. Chunk buffers
/// are reused as pieces finish parsing, so the whole decoded text never exists
/// in memory at once. The merger reorders finished builders back into array
/// order and folds each into the running [`Merge`] while later chunks are still
/// decoding and parsing, so after the last chunk only the final seal remains.
/// `None` if the decode or any chunk parse fails, if the folded builders do not
/// account for every piece cut, or if no piece was cut at all — an empty
/// `traceEvents` array.
fn parse_gz_stream_from(
    reader: impl Read + Send,
    workers: usize,
    chunk_bytes: usize,
    decoded_size: usize,
) -> Option<Profile> {
    // The buffer ring is one slab split into fixed-size parts — a single
    // zeroed allocation whose pages are first touched by the inflate's own
    // writes — holding one part more than the piece queue and the workers can
    // have in flight together, so whenever the producer blocks on the pool a
    // return is already on its way.
    let ring = 2 * workers + 1;
    let mut slab: Vec<u8> = vec![0u8; ring * chunk_bytes];
    let (tx, rx) = crossbeam_channel::bounded::<Piece>(workers);
    let (builder_tx, builder_rx) = std::sync::mpsc::channel::<(usize, Option<Builder>)>();
    let (pool_tx, pool_rx) = std::sync::mpsc::channel::<&mut [u8]>();
    for part in slab.chunks_mut(chunk_bytes) {
        let _ = pool_tx.send(part);
    }
    // Every worker interns into this one arena, so a builder's handles are
    // global and the merger never rebases them. The distinct pool is bounded by
    // the decoded size the ISIZE trailer declares.
    let interner = ConcurrentInterner::new(decoded_size, decoded_size / BYTES_PER_DISTINCT);
    let prefault_bytes = intern_prefault_bytes(decoded_size);
    let (ends, merged) = timed("  decode+parse+fold chunks", || {
        let interner = &interner;
        std::thread::scope(|scope| {
            let producer = scope.spawn(move || stream_pieces(reader, chunk_bytes, tx, pool_rx));
            for shard in 0..workers {
                let builder_tx = builder_tx.clone();
                let pool_tx = pool_tx.clone();
                let rx = &rx;
                scope.spawn(move || {
                    // Fault this worker's arena share before the receive loop,
                    // overlapping the producer's initial inflate.
                    interner.prefault_shard(shard, workers, prefault_bytes);
                    let mut writer = interner.writer();
                    let mut args_cap = 0;
                    loop {
                        let piece = match rx.recv() {
                            Ok(piece) => piece,
                            Err(_) => break,
                        };
                        let chunk = StreamChunk { buf: piece.buf, pool: &pool_tx };
                        let builder = parse_chunk(chunk, &mut writer, &mut args_cap);
                        if builder_tx.send((piece.idx, builder)).is_err() {
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
                let mut merge = Merge::with_capacity(decoded_size.div_ceil(chunk_bytes), 0);
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
