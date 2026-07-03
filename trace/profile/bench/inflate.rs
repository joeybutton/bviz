//! Interleaved A/B benchmark of gzip inflate strategies on a profile file:
//! libdeflate one-shot against streaming flate2, printing per-round wall time
//! and output throughput. Alternating the strategies within one process keeps
//! background-load drift out of the comparison. The streaming side inflates
//! into one reused buffer — the throughput a chunked decode-and-drop pipeline
//! sees, without accumulating the whole output.

#[global_allocator]
static GLOBAL: bviz_alloc::MiMalloc = bviz_alloc::MiMalloc;

use std::io::Read;

const STREAM_BUF_BYTES: usize = 32 << 20;
const ROUNDS: usize = 4;

fn report(label: &str, round: usize, start: std::time::Instant, out_bytes: usize) {
    let dt = start.elapsed().as_secs_f64();
    eprintln!(
        "[{round}] {label}: {dt:.3}s  {:.2} GB/s out ({out_bytes} bytes)",
        out_bytes as f64 / dt / 1e9
    );
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: bench_inflate <profile.gz>");
    let bytes = std::fs::read(&path).expect("read input");
    let mut buf = vec![0u8; STREAM_BUF_BYTES];
    for round in 0..ROUNDS {
        let start = std::time::Instant::now();
        let out = bviz_trace_profile::gzip_decompress_oneshot(&bytes)
            .expect("one-shot gzip decode");
        report("libdeflate one-shot", round, start, out.len());
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
        report("flate2 streaming", round, start, n);
    }
}
