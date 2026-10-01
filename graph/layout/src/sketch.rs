//! Estimated reach counts from bottom-k min-hash sketches: Cohen,
//! "Size-Estimation Framework with Applications to Transitive Closure and
//! Reachability", JCSS 55(3), 1997.
//!
//! Every counted node draws a rank from a hash of its key, and each component of
//! a condensation keeps the [`RANKS`] smallest ranks among its own counted nodes
//! and all it reaches. Fewer ranks than that are the whole set, so a count
//! below [`RANKS`] is exact.

use rayon::prelude::*;

use crate::reduce::{Condensation, Count};

/// How many of the smallest ranks a sketch keeps: the estimate's relative
/// standard error is about `1 / sqrt(RANKS - 2)`, and a sketch takes this many
/// words.
pub(crate) const RANKS: usize = 128;

/// The fewest components one parallel task sketches, so a height of few
/// components is sketched on one thread.
const COMPONENTS_PER_TASK: usize = 64;

/// By component of `condensation`, how many counted nodes it holds and
/// reaches together, `height` giving each component's height. `key` gives by
/// index of the condensed graph the key of each node that counts, which ranks
/// it.
pub(crate) fn sketched(
    condensation: &Condensation,
    height: &[u32],
    key: impl Fn(usize) -> Option<u64>,
) -> Vec<Count> {
    let components = condensation.succ.len();
    let mut own: Vec<Vec<u64>> = vec![Vec::new(); components];
    for (i, &c) in condensation.of.iter().enumerate() {
        if let Some(key) = key(i) {
            own[c as usize].push(rank(key));
        }
    }
    for ranks in own.iter_mut().filter(|ranks| ranks.len() > 1) {
        ranks.sort_unstable();
        ranks.dedup();
        ranks.truncate(RANKS);
    }
    let by_height = by_height(height);
    // A sketch is dropped once every component reaching it has read it.
    let mut unread: Vec<u32> = vec![0; components];
    for successors in &condensation.succ {
        for &s in successors {
            unread[s as usize] += 1;
        }
    }
    // Boxed, so that a sketch kept for later holds no more than its ranks.
    let mut sketch: Vec<Box<[u64]>> = vec![Box::default(); components];
    let mut count: Vec<Count> = vec![Count::Exact(0); components];
    bviz_parallel::workers().install(|| {
        for level in by_height {
            let built: Vec<Vec<u64>> = level
                .par_iter()
                .with_min_len(COMPONENTS_PER_TASK)
                .map_init(Vec::new, |scratch, &c| {
                    let successors = condensation.succ[c as usize].iter();
                    let reached = successors.map(|&s| &sketch[s as usize][..]);
                    union(&own[c as usize], reached, scratch)
                })
                .collect();
            for (&c, built) in level.iter().zip(built) {
                count[c as usize] = size(&built);
                if unread[c as usize] > 0 {
                    sketch[c as usize] = built.into_boxed_slice();
                }
                for &s in &condensation.succ[c as usize] {
                    unread[s as usize] -= 1;
                    if unread[s as usize] == 0 {
                        sketch[s as usize] = Box::default();
                    }
                }
            }
        }
    });
    count
}

/// The components of each height, lowest first, `height` giving it by
/// component.
fn by_height(height: &[u32]) -> Vec<Vec<u32>> {
    let heights = height.iter().max().map_or(0, |&h| h as usize + 1);
    let mut size = vec![0usize; heights];
    for &h in height {
        size[h as usize] += 1;
    }
    let mut levels: Vec<Vec<u32>> = size.into_iter().map(Vec::with_capacity).collect();
    for (c, &h) in height.iter().enumerate() {
        levels[h as usize].push(c as u32);
    }
    levels
}

/// The [`RANKS`] smallest distinct ranks among the sketches `own` and
/// `reached`. `scratch` is reused across calls.
fn union<'a>(
    own: &[u64],
    reached: impl Iterator<Item = &'a [u64]>,
    scratch: &mut Vec<u64>,
) -> Vec<u64> {
    let mut acc = own.to_vec();
    for other in reached {
        merge(&acc, other, scratch);
        std::mem::swap(&mut acc, scratch);
    }
    acc
}

/// Leaves in `into` the [`RANKS`] smallest distinct ranks of the sorted `a`
/// and `b`.
fn merge(a: &[u64], b: &[u64], into: &mut Vec<u64>) {
    into.clear();
    let (mut i, mut j) = (0, 0);
    while into.len() < RANKS {
        let next = match (a.get(i), b.get(j)) {
            (Some(&x), Some(&y)) if x < y => {
                i += 1;
                x
            }
            (Some(&x), Some(&y)) if y < x => {
                j += 1;
                y
            }
            (Some(&x), Some(_)) => {
                i += 1;
                j += 1;
                x
            }
            (Some(&x), None) => {
                i += 1;
                x
            }
            (None, Some(&y)) => {
                j += 1;
                y
            }
            (None, None) => break,
        };
        into.push(next);
    }
}

/// How many ranks the set `sketch` samples holds: exactly its length where it
/// keeps fewer than [`RANKS`], and otherwise estimated from its largest.
fn size(sketch: &[u64]) -> Count {
    match sketch.get(RANKS - 1) {
        None => Count::Exact(sketch.len() as u32),
        Some(&largest) => {
            // The largest rank as a fraction of the whole range of ranks.
            let fraction = (largest as f64 + 1.0) / 2f64.powi(64);
            let estimate = (RANKS - 1) as f64 / fraction;
            Count::Estimated(estimate.round().clamp(RANKS as f64, u32::MAX as f64) as u32)
        }
    }
}

/// A node's rank, uniform over `u64` and fixed by its key, so a node sampled by
/// one estimate is sampled by every other. SplitMix64's finalizer (Steele, Lea
/// and Flood, "Fast Splittable Pseudorandom Number Generators", OOPSLA 2014).
fn rank(key: u64) -> u64 {
    let mut z = key.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}
