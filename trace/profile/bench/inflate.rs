//! Interleaved A/B benchmark of the two gzip decode shapes the loader uses:
//! inflating into one whole-profile buffer (the fallback path) against
//! streaming into a reused chunk-sized buffer (what the pipelined parse
//! consumes), printing per-round wall time and output throughput. Alternating
//! the shapes within one process keeps background-load drift out of the
//! comparison. The whole-buffer side pays the first touch of a full-size
//! output allocation each round; the streaming side never holds more than one
//! chunk.

#[global_allocator]
static GLOBAL: bviz_alloc::MiMalloc = bviz_alloc::MiMalloc;

use std::io::Read;

const STREAM_BUF_BYTES: usize = 32 << 20;
const ROUNDS: usize = 4;
const GZIP_ISIZE_BYTES: usize = 4;

fn report(label: &str, round: usize, start: std::time::Instant, out_bytes: usize) {
    let dt = start.elapsed().as_secs_f64();
    eprintln!(
        "[{round}] {label}: {dt:.3}s  {:.2} GB/s out ({out_bytes} bytes)",
        out_bytes as f64 / dt / 1e9
    );
}

/// The gzip ISIZE trailer, from which the whole-buffer side reserves its
/// output like the loader's fallback decode does.
fn isize_trailer(bytes: &[u8]) -> usize {
    if bytes.len() < GZIP_ISIZE_BYTES {
        return 0;
    }
    u32::from_le_bytes(bytes[bytes.len() - GZIP_ISIZE_BYTES..].try_into().unwrap()) as usize
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: bench_inflate <profile.gz>");
    let bytes = std::fs::read(&path).expect("read input");
    let mut buf = vec![0u8; STREAM_BUF_BYTES];
    for round in 0..ROUNDS {
        let start = std::time::Instant::now();
        let mut out = Vec::with_capacity(isize_trailer(&bytes));
        flate2::read::MultiGzDecoder::new(&bytes[..])
            .read_to_end(&mut out)
            .expect("whole-buffer inflate");
        report("whole-buffer", round, start, out.len());
        drop(out);

        let start = std::time::Instant::now();
        let mut dec = flate2::read::MultiGzDecoder::new(&bytes[..]);
        let mut n = 0usize;
        loop {
            let k = dec.read(&mut buf).expect("streaming inflate");
            if k == 0 {
                break;
            }
            n += k;
        }
        report("chunked streaming", round, start, n);
    }
}
