use std::collections::HashMap;

use bviz_trace_profile::Profile;

use crate::flame::FlameKey;

/// One aggregated group of slices, summed across the scope. `total_us` and
/// `max_us` sum/peak the durations of every slice (at any depth) sharing this
/// key. A group's share of the total is `total_us / sum(all groups' total_us)`,
/// which sums to 100% across groups — share of aggregated time, not wall-clock:
/// nested slices overlap in time, so the summed durations exceed the elapsed
/// wall time.
#[derive(Debug, Clone, PartialEq)]
pub struct StatRow {
    pub key: String,
    pub count: u32,
    pub total_us: i64,
    pub max_us: i64,
}

impl StatRow {
    /// Mean duration of the group's slices.
    pub fn avg_us(&self) -> i64 {
        if self.count > 0 {
            self.total_us / self.count as i64
        } else {
            0
        }
    }
}

/// Aggregate the profile's slices by `key`, over `scope` (`Some(thread_index)` =
/// only that thread; `None` = all threads), into one row per distinct key value
/// (count, summed and max duration). Every slice in scope counts, at all depths.
/// Order is unspecified; the caller sorts.
pub fn aggregate_stats(profile: &Profile, key: FlameKey, scope: Option<usize>) -> Vec<StatRow> {
    let threads = match scope {
        Some(i) => &profile.threads[i..=i],
        None => &profile.threads[..],
    };
    let mut groups: HashMap<&str, StatRow> = HashMap::new();
    for thread in threads {
        for s in &thread.slices {
            let k = key.of(s);
            let row = groups.entry(k).or_insert_with(|| StatRow {
                key: k.to_owned(),
                count: 0,
                total_us: 0,
                max_us: 0,
            });
            row.count += 1;
            row.total_us += s.dur_us;
            row.max_us = row.max_us.max(s.dur_us);
        }
    }
    groups.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_trace_profile::{Metadata, Slice, SliceArgs, ThreadTrack, TimeSpan};

    fn slice(name: &str, start_us: i64, dur_us: i64) -> Slice {
        Slice {
            name: name.to_owned(),
            category: "cat".to_owned(),
            start_us,
            dur_us,
            depth: 0,
            args: SliceArgs::default(),
        }
    }

    fn action(name: &str, mnemonic: &str, start_us: i64, dur_us: i64) -> Slice {
        Slice {
            args: SliceArgs {
                mnemonic: Some(mnemonic.to_owned()),
                ..SliceArgs::default()
            },
            ..slice(name, start_us, dur_us)
        }
    }

    fn track(tid: i64, slices: Vec<Slice>) -> ThreadTrack {
        ThreadTrack {
            tid,
            name: format!("t{tid}"),
            sort_index: tid,
            slices,
            max_depth: 0,
        }
    }

    fn profile(threads: Vec<ThreadTrack>) -> Profile {
        Profile {
            metadata: Metadata::default(),
            threads,
            counters: Vec::new(),
            phase_markers: Vec::new(),
            span: TimeSpan {
                start_us: 0,
                end_us: 1000,
            },
        }
    }

    fn row<'a>(rows: &'a [StatRow], key: &str) -> &'a StatRow {
        rows.iter()
            .find(|r| r.key == key)
            .unwrap_or_else(|| panic!("no row {key}"))
    }

    #[test]
    fn mnemonic_grouping_sums_count_total_and_max() {
        let p = profile(vec![track(
            1,
            vec![
                action("compile foo.cc", "CppCompile", 0, 100),
                action("compile bar.cc", "CppCompile", 100, 50),
                action("link a", "CppLink", 200, 30),
            ],
        )]);
        let rows = aggregate_stats(&p, FlameKey::Mnemonic, None);
        assert_eq!(rows.len(), 2);
        let c = row(&rows, "CppCompile");
        assert_eq!(c.count, 2);
        assert_eq!(c.total_us, 150);
        assert_eq!(c.max_us, 100);
        assert_eq!(c.avg_us(), 75);
        assert_eq!(row(&rows, "CppLink").total_us, 30);
    }

    #[test]
    fn scope_excludes_other_threads() {
        let p = profile(vec![
            track(1, vec![slice("a", 0, 100)]),
            track(2, vec![slice("b", 0, 50)]),
        ]);
        let rows = aggregate_stats(&p, FlameKey::Name, Some(1));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, "b");
        assert_eq!(rows[0].total_us, 50);
    }

    #[test]
    fn mnemonic_less_slice_falls_back_to_name() {
        // A slice with no mnemonic keys by its name under Mnemonic grouping,
        // matching FlameKey::of.
        let p = profile(vec![track(
            1,
            vec![
                action("compile foo.cc", "CppCompile", 0, 100),
                slice("evaluate", 200, 30),
            ],
        )]);
        let rows = aggregate_stats(&p, FlameKey::Mnemonic, None);
        assert_eq!(rows.len(), 2);
        assert_eq!(row(&rows, "evaluate").total_us, 30);
        assert_eq!(row(&rows, "CppCompile").count, 1);
    }

    #[test]
    fn aggregation_is_order_independent() {
        let ordered = vec![
            slice("a", 0, 100),
            slice("b", 0, 40),
            slice("a", 200, 60),
        ];
        let mut reversed = ordered.clone();
        reversed.reverse();
        let mut ra = aggregate_stats(&profile(vec![track(1, ordered)]), FlameKey::Name, None);
        let mut rb = aggregate_stats(&profile(vec![track(1, reversed)]), FlameKey::Name, None);
        ra.sort_by(|x, y| x.key.cmp(&y.key));
        rb.sort_by(|x, y| x.key.cmp(&y.key));
        assert_eq!(ra, rb);
        let a = row(&ra, "a");
        assert_eq!(a.count, 2);
        assert_eq!(a.total_us, 160);
        assert_eq!(a.max_us, 100);
    }
}
