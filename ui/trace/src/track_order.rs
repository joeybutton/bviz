use bviz_trace_profile::{Profile, ThreadTrack, track_family};
use std::cmp::Ordering;

/// Orders `tracks` (indices into `threads`) busiest first — most slices first,
/// ties broken by tid so the order is stable frame to frame.
pub(crate) fn sort_tracks_by_activity(
    profile: &Profile,
    threads: &[ThreadTrack],
    tracks: &mut [usize],
) {
    tracks.sort_by(|&a, &b| {
        profile
            .slices(&threads[b])
            .len()
            .cmp(&profile.slices(&threads[a]).len())
            .then(threads[a].tid.cmp(&threads[b].tid))
    });
}

/// Drops all but the `cap` busiest of `tracks`, leaving the survivors in the
/// order they arrived in, so a capped list is a sample of the group's weight
/// rather than a prefix of its names.
pub(crate) fn retain_busiest(
    profile: &Profile,
    threads: &[ThreadTrack],
    tracks: &mut Vec<usize>,
    cap: usize,
) {
    if tracks.len() <= cap {
        return;
    }
    let mut busiest = tracks.clone();
    sort_tracks_by_activity(profile, threads, &mut busiest);
    busiest.truncate(cap);
    let mut kept = vec![false; threads.len()];
    for &t in &busiest {
        kept[t] = true;
    }
    tracks.retain(|&t| kept[t]);
}

/// Orders `tracks` (indices into `threads`) by pool, then by name under
/// [`natural_cmp`], then by tid — a pool reuses a name as it is recreated, so
/// the name alone is not a total order.
pub(crate) fn sort_tracks_by_name(
    profile: &Profile,
    threads: &[ThreadTrack],
    tracks: &mut [usize],
) {
    let mut keyed: Vec<(&str, &str, i64, usize)> = tracks
        .iter()
        .map(|&t| {
            let name = profile.str(threads[t].name);
            (track_family(threads[t].tid, name), name, threads[t].tid, t)
        })
        .collect();
    keyed.sort_by(
        |(family, name, tid, _), (other_family, other_name, other_tid, _)| {
            natural_cmp(family, other_family)
                .then_with(|| natural_cmp(name, other_name))
                .then(tid.cmp(other_tid))
        },
    );
    for (slot, (_, _, _, track)) in tracks.iter_mut().zip(keyed) {
        *slot = track;
    }
}

/// Compares two names run by run, a run being a maximal stretch of ASCII digits
/// or of anything else: digit runs compare by numeric value, so `worker-2`
/// precedes `worker-10`, and other runs case-insensitively. Names that compare
/// equal that way fall back to their bytes, making the order total.
pub(crate) fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a_runs, mut b_runs) = (runs(a), runs(b));
    loop {
        match (a_runs.next(), b_runs.next()) {
            (Some(a_run), Some(b_run)) => match cmp_run(a_run, b_run) {
                Ordering::Equal => continue,
                ord => return ord,
            },
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (None, None) => return a.as_bytes().cmp(b.as_bytes()),
        }
    }
}

/// The maximal runs of ASCII digits and of non-digits `name` is made of, in
/// order.
fn runs(name: &str) -> impl Iterator<Item = &[u8]> {
    let mut rest = name.as_bytes();
    std::iter::from_fn(move || {
        let digits = rest.first()?.is_ascii_digit();
        let end = rest
            .iter()
            .position(|byte| byte.is_ascii_digit() != digits)
            .unwrap_or(rest.len());
        let (run, tail) = rest.split_at(end);
        rest = tail;
        Some(run)
    })
}

/// Compares two runs, a digit run leading a non-digit one. Digit runs compare
/// by the value they spell, which is their significant length and then their
/// digits — a run can spell a number wider than any integer type.
fn cmp_run(a: &[u8], b: &[u8]) -> Ordering {
    let digits = |run: &[u8]| run.first().is_some_and(u8::is_ascii_digit);
    match (digits(a), digits(b)) {
        (true, true) => {
            let (a, b) = (significant(a), significant(b));
            a.len().cmp(&b.len()).then_with(|| a.cmp(b))
        }
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => a
            .iter()
            .map(u8::to_ascii_lowercase)
            .cmp(b.iter().map(u8::to_ascii_lowercase)),
    }
}

/// A digit run without its leading zeros.
fn significant(run: &[u8]) -> &[u8] {
    let zeros = run
        .iter()
        .position(|&byte| byte != b'0')
        .unwrap_or(run.len());
    &run[zeros..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_profile::profile_of;

    #[test]
    fn a_number_in_a_name_compares_by_value() {
        assert_eq!(natural_cmp("worker-2", "worker-10"), Ordering::Less);
        assert_eq!(natural_cmp("worker-10", "worker-2"), Ordering::Greater);
        assert_eq!(natural_cmp("worker-9", "worker-9"), Ordering::Equal);
    }

    #[test]
    fn each_number_decides_in_turn() {
        assert_eq!(natural_cmp("a-2-b-10", "a-10-b-2"), Ordering::Less);
        assert_eq!(natural_cmp("a-10-b-2", "a-10-b-10"), Ordering::Less);
        assert_eq!(natural_cmp("a-10-b-2", "a-10-c-1"), Ordering::Less);
    }

    #[test]
    fn leading_zeros_leave_the_value_equal_and_the_order_decided_by_the_bytes() {
        let padded = natural_cmp("x-007", "x-7");
        assert_ne!(padded, Ordering::Equal);
        assert_eq!(natural_cmp("x-7", "x-007"), padded.reverse());
        assert_eq!(natural_cmp("x-007", "x-08"), Ordering::Less, "7 before 8");
    }

    #[test]
    fn a_number_leads_the_text_that_could_follow_it() {
        assert_eq!(
            natural_cmp("skyframe-evaluator-0", "skyframe-evaluator-cpu-heavy-0"),
            Ordering::Less
        );
        assert_eq!(natural_cmp("pool-1", "pool-a"), Ordering::Less);
        assert_eq!(natural_cmp("2-worker", "worker-2"), Ordering::Less);
    }

    #[test]
    fn text_compares_without_regard_to_case() {
        assert_eq!(natural_cmp("Alpha-2", "alpha-10"), Ordering::Less);
        assert_eq!(natural_cmp("beta", "Alpha"), Ordering::Greater);
        assert_ne!(natural_cmp("beta", "Beta"), Ordering::Equal);
    }

    #[test]
    fn the_order_is_total_and_antisymmetric() {
        let names = [
            "worker-2",
            "worker-10",
            "worker-0002",
            "Worker-2",
            "worker",
            "2-worker",
            "globbing pool-3",
            "skyframe-evaluator-cpu-heavy-3",
            "",
        ];
        for a in names {
            for b in names {
                assert_eq!(
                    natural_cmp(a, b),
                    natural_cmp(b, a).reverse(),
                    "{a:?} {b:?}"
                );
                assert_eq!(natural_cmp(a, b) == Ordering::Equal, a == b, "{a:?} {b:?}");
            }
        }
    }

    /// The names of `tracks`, in the order they stand in.
    fn names(profile: &Profile, tracks: &[usize]) -> Vec<String> {
        let threads = profile.threads();
        tracks
            .iter()
            .map(|&t| profile.str(threads[t].name).to_owned())
            .collect()
    }

    #[test]
    fn tracks_group_by_pool_with_their_worker_numbers_ascending() {
        // Slice counts descend against the name order, so the two orderings
        // are visibly different.
        let profile = profile_of(&[
            ("globbing pool-10".to_owned(), 4),
            ("skyframe-evaluator-cpu-heavy-2".to_owned(), 3),
            ("globbing pool-2".to_owned(), 2),
            ("skyframe-evaluator-cpu-heavy-10".to_owned(), 1),
        ]);
        let threads = profile.threads();
        let mut tracks: Vec<usize> = (0..threads.len()).collect();
        sort_tracks_by_name(&profile, threads, &mut tracks);
        assert_eq!(
            names(&profile, &tracks),
            [
                "globbing pool-2",
                "globbing pool-10",
                "skyframe-evaluator-cpu-heavy-2",
                "skyframe-evaluator-cpu-heavy-10",
            ]
        );

        sort_tracks_by_activity(&profile, threads, &mut tracks);
        assert_eq!(
            names(&profile, &tracks),
            [
                "globbing pool-10",
                "skyframe-evaluator-cpu-heavy-2",
                "globbing pool-2",
                "skyframe-evaluator-cpu-heavy-10",
            ]
        );
    }

    #[test]
    fn tracks_sharing_a_name_keep_their_tid_order() {
        let profile = profile_of(&[
            ("globbing pool-2".to_owned(), 1),
            ("globbing pool-2".to_owned(), 3),
            ("globbing pool-2".to_owned(), 2),
        ]);
        let threads = profile.threads();
        let mut tracks: Vec<usize> = (0..threads.len()).collect();
        tracks.reverse();
        sort_tracks_by_name(&profile, threads, &mut tracks);
        let tids: Vec<i64> = tracks.iter().map(|&t| threads[t].tid).collect();
        assert_eq!(tids.len(), 3, "one track per tid");
        assert!(tids.windows(2).all(|w| w[0] < w[1]), "{tids:?}");
    }
}
