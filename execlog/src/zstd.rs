//! A zstd stream read the way the zstd library reads one, over ruzstd's
//! pure-Rust frame decoder, so a log decodes the same on a desktop and in a
//! browser (where no C toolchain targets):
//!
//! - a stream that ends before any frame is empty;
//! - one that ends inside a frame yields every byte decoded ahead of the end,
//!   then fails with [`ErrorKind::UnexpectedEof`], which a reader takes for a
//!   log cut off by an interrupted build;
//! - frames that follow one another decode as one stream, and skippable frames
//!   are skipped.

use std::io::{self, ErrorKind, Read};

use ruzstd::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};
use ruzstd::decoding::{BlockDecodingStrategy, FrameDecoder};

/// An empty raw block flagged last, followed by room for a frame checksum: what
/// ends a frame whose input stopped between blocks, so that the decoder hands
/// over the window it otherwise keeps until a frame's last block. A frame cut
/// inside a block has decoded nothing of that block's compressed sequences yet,
/// as the decoder reads a compressed block whole before executing it.
const END_OF_FRAME: [u8; 7] = [0x01, 0, 0, 0, 0, 0, 0];

pub(crate) struct Decoder<R> {
    source: Counted<R>,
    frame: FrameDecoder,
    at: At,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum At {
    /// Before a frame header, or before the end of the stream.
    Header,
    Blocks,
    /// The input ended inside a frame.
    Cut,
    End,
}

/// `inner`, counting the bytes taken from it and whether it has run out.
struct Counted<R> {
    inner: R,
    read: u64,
    ended: bool,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        if read == 0 && !buf.is_empty() {
            self.ended = true;
        }
        self.read += read as u64;
        Ok(read)
    }
}

impl<R: Read> Decoder<R> {
    pub(crate) fn new(source: R) -> Self {
        Self {
            source: Counted {
                inner: source,
                read: 0,
                ended: false,
            },
            frame: FrameDecoder::new(),
            at: At::Header,
        }
    }

    /// Ends a frame whose input stopped, so its decoded bytes can be drained.
    fn end_cut_frame(&mut self) {
        let _ = self
            .frame
            .decode_blocks(&END_OF_FRAME[..], BlockDecodingStrategy::All);
        self.at = At::Cut;
    }
}

impl<R: Read> Read for Decoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            let drained = self.frame.read(buf)?;
            if drained > 0 {
                return Ok(drained);
            }
            match self.at {
                At::End => return Ok(0),
                At::Cut => {
                    return Err(io::Error::new(
                        ErrorKind::UnexpectedEof,
                        "the zstd stream ends inside a frame",
                    ));
                }
                At::Header => {
                    let before = self.source.read;
                    match self.frame.reset(&mut self.source) {
                        Ok(()) => self.at = At::Blocks,
                        Err(FrameDecoderError::ReadFrameHeaderError(
                            ReadFrameHeaderError::SkipFrame { length, .. },
                        )) => {
                            let skipped = io::copy(
                                &mut (&mut self.source).take(u64::from(length)),
                                &mut io::sink(),
                            )?;
                            if skipped < u64::from(length) {
                                self.at = At::Cut;
                            }
                        }
                        Err(_) if self.source.ended && self.source.read == before => {
                            self.at = At::End;
                        }
                        // A header cut short: nothing of its frame was decoded.
                        Err(_) if self.source.ended => self.at = At::Cut,
                        Err(e) => return Err(io::Error::other(e)),
                    }
                }
                At::Blocks => match self
                    .frame
                    .decode_blocks(&mut self.source, BlockDecodingStrategy::UptoBlocks(1))
                {
                    Ok(true) => self.at = At::Header,
                    Ok(false) => {}
                    Err(_) if self.source.ended => self.end_cut_frame(),
                    Err(e) => return Err(io::Error::other(e)),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruzstd::encoding::{CompressionLevel, compress_to_vec};

    fn text(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 % 251) as u8).collect()
    }

    fn decode(stream: &[u8]) -> (Vec<u8>, Option<ErrorKind>) {
        let mut out = Vec::new();
        let err = Decoder::new(stream).read_to_end(&mut out).err();
        (out, err.map(|e| e.kind()))
    }

    #[test]
    fn decodes_whole_and_concatenated_frames() {
        let (a, b) = (text(300_000), text(1_000));
        let mut stream = compress_to_vec(&a[..], CompressionLevel::Fastest);
        stream.extend(compress_to_vec(&b[..], CompressionLevel::Fastest));
        let (out, err) = decode(&stream);
        assert_eq!(err, None);
        assert_eq!(out, [a, b].concat());
    }

    #[test]
    fn an_empty_stream_is_empty_and_a_cut_one_ends_early() {
        assert_eq!(decode(&[]), (Vec::new(), None));
        let whole = text(600_000);
        let stream = compress_to_vec(&whole[..], CompressionLevel::Fastest);
        let mut longest = 0;
        for cut in [1, 5, stream.len() / 3, stream.len() / 2, stream.len() - 1] {
            let (out, err) = decode(&stream[..cut]);
            assert_eq!(err, Some(ErrorKind::UnexpectedEof), "cut at {cut}");
            assert_eq!(out[..], whole[..out.len()], "cut at {cut}");
            assert!(out.len() >= longest, "cut at {cut}");
            longest = out.len();
        }
        assert!(longest > 0, "a late cut keeps what was decoded ahead of it");
    }

    #[test]
    fn skippable_frames_are_skipped() {
        let body = text(10_000);
        let mut stream = vec![0x50, 0x2a, 0x4d, 0x18, 3, 0, 0, 0, 1, 2, 3];
        stream.extend(compress_to_vec(&body[..], CompressionLevel::Fastest));
        assert_eq!(decode(&stream), (body, None));
    }

    #[test]
    fn data_that_is_no_zstd_stream_fails() {
        let (_, err) = decode(b"not a zstd stream at all, but long enough");
        assert!(matches!(err, Some(kind) if kind != ErrorKind::UnexpectedEof));
    }
}
