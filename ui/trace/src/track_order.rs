use bviz_trace_profile::{Profile, ThreadTrack, track_family};
use std::cmp::{Ordering, Reverse};
use std::ops::Range;

/// The order a lane-class group stands its tracks in, in the flame chart's
/// gutter and the tracks menu alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum TrackSort {
    /// Pool by pool, each pool's members under their names.
    #[default]
    Name,
    /// Under the share of the profile each track holds, the heaviest leading,
    /// whichever pools they come from.
    Weight,
}

impl TrackSort {
    /// Stands `tracks` (indices into `threads`) in this order.
    pub(crate) fn order(self, profile: &Profile, threads: &[ThreadTrack], tracks: &mut [usize]) {
        match self {
            TrackSort::Name => sort_tracks_by_name(profile, threads, tracks),
            TrackSort::Weight => sort_tracks_by_activity(profile, threads, tracks),
        }
    }

    /// Drops all but `budget` of `tracks`, sampling as this order reads them:
    /// under the name order every pool holds a seat, so a light pool is not
    /// squeezed out of a list that gathers it; under the weight order the
    /// busiest take the seats outright, the same measure the order itself runs
    /// on. Survivors keep the order they arrived in.
    pub(crate) fn sample(
        self,
        profile: &Profile,
        threads: &[ThreadTrack],
        tracks: &mut Vec<usize>,
        budget: usize,
    ) {
        match self {
            TrackSort::Name => retain_busiest_per_family(profile, threads, tracks, budget),
            TrackSort::Weight => retain_busiest(profile, threads, tracks, budget),
        }
    }
}

/// Orders `tracks` (indices into `threads`) busiest first — most slices first,
/// ties broken by tid so the order is stable frame to frame.
fn sort_tracks_by_activity(profile: &Profile, threads: &[ThreadTrack], tracks: &mut [usize]) {
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
fn retain_busiest(profile: &Profile, threads: &[ThreadTrack], tracks: &mut Vec<usize>, cap: usize) {
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

/// The pools `tracks` runs through, each as its family and the number of members
/// it contributes, in track order. [`sort_tracks_by_name`] gathers a pool into a
/// single run, so over tracks in that order every pool appears once.
pub(crate) fn family_runs<'a>(
    profile: &'a Profile,
    threads: &'a [ThreadTrack],
    tracks: &[usize],
) -> Vec<(&'a str, usize)> {
    let mut runs: Vec<(&str, usize)> = Vec::new();
    for &t in tracks {
        let family = track_family(threads[t].tid, profile.str(threads[t].name));
        match runs.last_mut() {
            Some((run, members)) if *run == family => *members += 1,
            _ => runs.push((family, 1)),
        }
    }
    runs
}

/// A pool's standing in the distribution of a capped group's seats.
struct Pool {
    /// The pool's members, as a run of the group's track list.
    members: Range<usize>,
    /// Slices over the pool's members, the weight its claim scales with.
    weight: usize,
    seats: usize,
}

/// Drops all but `budget` of `tracks`, spreading the budget over the pools they
/// belong to by the D'Hondt highest-averages rule over each pool's slice count,
/// so a pool holds a share of the seats near its share of the work and no pool
/// is squeezed out while seats last. Survivors keep the order they arrived in,
/// and a pool seats its busiest members.
fn retain_busiest_per_family(
    profile: &Profile,
    threads: &[ThreadTrack],
    tracks: &mut Vec<usize>,
    budget: usize,
) {
    if tracks.len() <= budget {
        return;
    }
    let runs = family_runs(profile, threads, tracks);
    let mut pools = Vec::with_capacity(runs.len());
    let mut start = 0;
    for (_, count) in runs {
        let members = start..start + count;
        let weight = tracks[members.clone()]
            .iter()
            .map(|&t| profile.slices(&threads[t]).len())
            .sum();
        start = members.end;
        pools.push(Pool {
            members,
            weight,
            seats: 0,
        });
    }
    allocate_seats(&mut pools, budget);
    let mut seated = Vec::with_capacity(budget);
    for pool in &pools {
        let mut members = tracks[pool.members.clone()].to_vec();
        retain_busiest(profile, threads, &mut members, pool.seats);
        seated.append(&mut members);
    }
    *tracks = seated;
}

/// Hands out `budget` seats over `pools`: one each while they fit, the rest to
/// the pool with the highest weight per seat it would then hold. A budget too
/// small to seat every pool goes to the busiest of them. No pool takes more
/// seats than it has members, so a seat goes unfilled only once every member of
/// every pool holds one.
fn allocate_seats(pools: &mut [Pool], budget: usize) {
    if pools.len() >= budget {
        let mut busiest: Vec<usize> = (0..pools.len()).collect();
        // A stable sort leaves equally busy pools in pool order.
        busiest.sort_by_key(|&pool| Reverse(pools[pool].weight));
        for &pool in &busiest[..budget] {
            pools[pool].seats = 1;
        }
        return;
    }
    for pool in pools.iter_mut() {
        pool.seats = 1;
    }
    for _ in pools.len()..budget {
        let Some(pool) = highest_average(pools) else {
            break;
        };
        pools[pool].seats += 1;
    }
}

/// The pool with room for another member whose weight over the seats it would
/// then hold is highest, ties going to the earliest of them.
fn highest_average(pools: &[Pool]) -> Option<usize> {
    let mut best: Option<(usize, &Pool)> = None;
    for (index, pool) in pools.iter().enumerate() {
        if pool.seats == pool.members.len() {
            continue;
        }
        // weight / (seats + 1) against the same, compared without dividing.
        let outbids = |held: &Pool| {
            pool.weight as u128 * (held.seats + 1) as u128
                > held.weight as u128 * (pool.seats + 1) as u128
        };
        if best.is_none_or(|(_, held)| outbids(held)) {
            best = Some((index, pool));
        }
    }
    best.map(|(index, _)| index)
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

    /// The names left standing once `budget` seats are spread over the pools of
    /// `threads`, which the profile's tracks are taken to be in full.
    fn sampled(profile: &Profile, budget: usize) -> Vec<String> {
        let threads = profile.threads();
        let mut tracks: Vec<usize> = (0..threads.len()).collect();
        sort_tracks_by_name(profile, threads, &mut tracks);
        retain_busiest_per_family(profile, threads, &mut tracks, budget);
        names(profile, &tracks)
    }

    /// How many of `names` belong to the pool named by `prefix`.
    fn from_pool(names: &[String], prefix: &str) -> usize {
        names.iter().filter(|name| name.starts_with(prefix)).count()
    }

    /// `members` tracks of the pool `prefix` names, each carrying `slices`.
    fn pool(prefix: &str, members: usize, slices: usize) -> Vec<(String, usize)> {
        (0..members)
            .map(|i| (format!("{prefix}{i}"), slices))
            .collect()
    }

    #[test]
    fn a_pool_of_light_tracks_keeps_a_seat_beside_a_heavy_one() {
        const BUSY_SLICES: usize = 50;
        let mut threads = pool("skyframe-evaluator-execution-", 16, BUSY_SLICES);
        threads.extend(pool("sandbox-", 100, 1));
        let profile = profile_of(&threads);

        let kept = sampled(&profile, 12);
        assert_eq!(kept.len(), 12);
        assert_eq!(from_pool(&kept, "sandbox-"), 1, "{kept:?}");
        assert_eq!(
            from_pool(&kept, "skyframe-evaluator-execution-"),
            11,
            "{kept:?}"
        );
    }

    #[test]
    fn the_seats_a_pool_cannot_fill_go_to_the_pools_that_can() {
        const BUSY_SLICES: usize = 100;
        let mut threads = pool("skyframe-evaluator-execution-", 2, BUSY_SLICES);
        threads.extend(pool("sandbox-", 20, 1));
        let profile = profile_of(&threads);

        let kept = sampled(&profile, 12);
        assert_eq!(
            (
                from_pool(&kept, "skyframe-evaluator-execution-"),
                from_pool(&kept, "sandbox-")
            ),
            (2, 10),
            "the busiest pool seats every member it has and yields the rest"
        );
    }

    #[test]
    fn equally_busy_pools_split_the_seats_in_pool_order() {
        const SLICES: usize = 5;
        const MEMBERS: usize = 4;
        let mut threads = pool("combined-cache-", MEMBERS, SLICES);
        threads.extend(pool("merkle-tree-", MEMBERS, SLICES));
        threads.extend(pool("sandbox-", MEMBERS, SLICES));
        let profile = profile_of(&threads);

        let kept = sampled(&profile, 7);
        assert_eq!(
            (
                from_pool(&kept, "combined-cache-"),
                from_pool(&kept, "merkle-tree-"),
                from_pool(&kept, "sandbox-")
            ),
            (3, 2, 2)
        );
    }

    #[test]
    fn a_budget_short_of_the_pools_seats_the_busiest_of_them() {
        let mut threads = pool("combined-cache-", 3, 1);
        threads.extend(pool("merkle-tree-", 3, 9));
        threads.extend(pool("sandbox-", 3, 5));
        let profile = profile_of(&threads);

        let kept = sampled(&profile, 2);
        assert_eq!(
            (
                from_pool(&kept, "combined-cache-"),
                from_pool(&kept, "merkle-tree-"),
                from_pool(&kept, "sandbox-")
            ),
            (0, 1, 1)
        );
    }

    #[test]
    fn a_sample_fills_its_budget_without_outgrowing_it() {
        let mut threads = pool("combined-cache-", 1, 7);
        threads.extend(pool("merkle-tree-", 5, 3));
        threads.extend(pool("sandbox-", 30, 1));
        let profile = profile_of(&threads);
        let total = threads.len();

        for budget in [0, 1, 2, 5, 12, total - 1, total, total + 10] {
            let kept = sampled(&profile, budget);
            assert_eq!(kept.len(), budget.min(total), "budget {budget}");
        }
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
