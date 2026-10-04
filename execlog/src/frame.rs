//! Splitting a stream of length-delimited messages, each a varint byte count
//! followed by that many bytes, into its messages.

use std::io::Read;
use std::sync::mpsc;

use crate::LoadError;
use crate::wire::{MAX_VARINT_BYTES, VARINT_CONTINUES, varint};

/// How the head of a stream frames its next message.
enum Frame {
    /// The message's bytes lie at `start..end`.
    Whole { start: usize, end: usize },
    /// The message continues past the bytes at hand, which must grow to
    /// `at_least` to hold it, where known.
    Partial { at_least: Option<usize> },
}

fn frame(bytes: &[u8]) -> Result<Frame, LoadError> {
    let delimiter_ends = bytes
        .iter()
        .take(MAX_VARINT_BYTES)
        .any(|&b| b & VARINT_CONTINUES == 0);
    if !delimiter_ends && bytes.len() < MAX_VARINT_BYTES {
        return Ok(Frame::Partial { at_least: None });
    }
    let mut rest = bytes;
    let length = varint(&mut rest)?;
    let start = bytes.len() - rest.len();
    let end = usize::try_from(length)
        .ok()
        .and_then(|length| start.checked_add(length))
        .ok_or_else(|| prost::DecodeError::new("message length overflows"))?;
    Ok(match end <= bytes.len() {
        true => Frame::Whole { start, end },
        false => Frame::Partial {
            at_least: Some(end),
        },
    })
}

/// The first whole message of `bytes`, `None` when it holds none.
pub(crate) fn first_message(bytes: &[u8]) -> Option<&[u8]> {
    match frame(bytes).ok()? {
        Frame::Whole { start, end } => Some(&bytes[start..end]),
        Frame::Partial { .. } => None,
    }
}

/// Splits a stream fed in chunks cut anywhere into its messages.
#[derive(Default)]
struct Framer {
    /// The head of a message the chunks fed so far cut off.
    carry: Vec<u8>,
}

impl Framer {
    /// Calls `visit` on each message `chunk` completes, in order.
    fn feed(
        &mut self,
        mut chunk: &[u8],
        visit: &mut impl FnMut(&[u8]) -> Result<(), LoadError>,
    ) -> Result<(), LoadError> {
        while !self.carry.is_empty() {
            match frame(&self.carry)? {
                Frame::Whole { start, end } => {
                    visit(&self.carry[start..end])?;
                    self.carry.clear();
                }
                Frame::Partial { at_least } => {
                    let wanted = at_least.unwrap_or(self.carry.len() + 1) - self.carry.len();
                    let (head, rest) = chunk.split_at(wanted.min(chunk.len()));
                    if head.is_empty() {
                        return Ok(());
                    }
                    self.carry.extend_from_slice(head);
                    chunk = rest;
                }
            }
        }
        while !chunk.is_empty() {
            match frame(chunk)? {
                Frame::Whole { start, end } => {
                    visit(&chunk[start..end])?;
                    chunk = &chunk[end..];
                }
                Frame::Partial { .. } => {
                    self.carry.extend_from_slice(chunk);
                    break;
                }
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<(), LoadError> {
        match self.carry.is_empty() {
            true => Ok(()),
            false => Err(LoadError::Truncated),
        }
    }
}

/// Reads `stream` into `buf` until it is full or the stream ends, returning
/// the bytes read.
fn fill(stream: &mut impl Read, buf: &mut [u8]) -> Result<usize, LoadError> {
    let mut filled = 0;
    while filled < buf.len() {
        match stream.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(LoadError::Io(e)),
        }
    }
    Ok(filled)
}

/// Calls `visit` on each message of `stream`, in order, reading it
/// `chunk_bytes` at a time.
pub(crate) fn for_each_message(
    mut stream: impl Read,
    chunk_bytes: usize,
    mut visit: impl FnMut(&[u8]) -> Result<(), LoadError>,
) -> Result<(), LoadError> {
    let mut framer = Framer::default();
    let mut buf = vec![0; chunk_bytes];
    loop {
        let filled = fill(&mut stream, &mut buf)?;
        if filled == 0 {
            return framer.finish();
        }
        framer.feed(&buf[..filled], &mut visit)?;
    }
}

/// Chunks [`for_each_message_pipelined`] reads ahead of the messages being
/// visited.
const CHUNKS_IN_FLIGHT: usize = 4;

/// [`for_each_message`] with `stream` read on a thread of its own, so reading
/// it, decompression included, overlaps the visits.
pub(crate) fn for_each_message_pipelined(
    mut stream: impl Read + Send,
    chunk_bytes: usize,
    mut visit: impl FnMut(&[u8]) -> Result<(), LoadError>,
) -> Result<(), LoadError> {
    type Chunk = Result<(Vec<u8>, usize), LoadError>;
    let (filled_tx, filled_rx) = mpsc::sync_channel::<Chunk>(CHUNKS_IN_FLIGHT);
    let (empty_tx, empty_rx) = mpsc::sync_channel::<Vec<u8>>(CHUNKS_IN_FLIGHT);
    // Owning the ring's ends, the visiting side drops them on returning, which
    // stops the reader even when a visit fails part way.
    std::thread::scope(move |scope| {
        scope.spawn(move || {
            let mut unallocated = CHUNKS_IN_FLIGHT;
            loop {
                let mut buf = match unallocated {
                    0 => match empty_rx.recv() {
                        Ok(buf) => buf,
                        Err(_) => break,
                    },
                    _ => {
                        unallocated -= 1;
                        vec![0; chunk_bytes]
                    }
                };
                let read = fill(&mut stream, &mut buf);
                let last = !matches!(read, Ok(filled) if filled > 0);
                if filled_tx.send(read.map(|filled| (buf, filled))).is_err() || last {
                    break;
                }
            }
        });
        let mut framer = Framer::default();
        for read in filled_rx {
            let (buf, filled) = read?;
            if filled == 0 {
                break;
            }
            framer.feed(&buf[..filled], &mut visit)?;
            // Fails only once the reader has stopped, when no buffer is wanted.
            let _ = empty_tx.send(buf);
        }
        framer.finish()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delimited(messages: &[&[u8]]) -> Vec<u8> {
        let mut stream = Vec::new();
        for message in messages {
            prost::encode_length_delimiter(message.len(), &mut stream).unwrap();
            stream.extend_from_slice(message);
        }
        stream
    }

    /// The messages of `stream` read `chunk_bytes` at a time, serially and
    /// pipelined.
    fn collect(stream: &[u8], chunk_bytes: usize) -> [Result<Vec<Vec<u8>>, LoadError>; 2] {
        let mut serial = Vec::new();
        let serial = for_each_message(stream, chunk_bytes, |message| {
            serial.push(message.to_vec());
            Ok(())
        })
        .map(|()| serial);
        let mut pipelined = Vec::new();
        let pipelined = for_each_message_pipelined(stream, chunk_bytes, |message| {
            pipelined.push(message.to_vec());
            Ok(())
        })
        .map(|()| pipelined);
        [serial, pipelined]
    }

    #[test]
    fn every_chunk_size_yields_the_same_messages() {
        let long = vec![7; 200];
        let messages: [&[u8]; 4] = [b"a", b"", &long, b"tail"];
        let stream = delimited(&messages);
        for chunk_bytes in 1..=stream.len() {
            for read in collect(&stream, chunk_bytes) {
                assert_eq!(read.unwrap(), messages, "chunk of {chunk_bytes}");
            }
        }
    }

    #[test]
    fn a_stream_ending_inside_a_message_is_truncated() {
        let stream = delimited(&[b"whole", b"cut off"]);
        let cut = &stream[..stream.len() - 1];
        for chunk_bytes in [1, 4, cut.len()] {
            for read in collect(cut, chunk_bytes) {
                assert!(matches!(read, Err(LoadError::Truncated)));
            }
        }
    }

    #[test]
    fn a_failed_visit_stops_the_pipeline() {
        let stream = delimited(&[b"one", b"two", b"three"]);
        let mut visits = 0;
        let result = for_each_message_pipelined(stream.as_slice(), 1, |_| {
            visits += 1;
            Err(LoadError::Truncated)
        });
        assert!(result.is_err());
        assert_eq!(visits, 1);
    }

    #[test]
    fn first_message_needs_it_whole() {
        let stream = delimited(&[b"head", b"next"]);
        assert_eq!(first_message(&stream), Some(&b"head"[..]));
        assert_eq!(first_message(&stream[..3]), None);
        assert_eq!(first_message(&[]), None);
    }
}
