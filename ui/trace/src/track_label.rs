use bviz_trace_profile::{Profile, Str, track_family};
use std::borrow::Cow;
use std::collections::HashMap;

/// Which of `profile`'s tracks carry a thread name another of its tracks also
/// carries, indexed by `profile.threads()` order. Bazel names a worker pool's
/// threads afresh each time it recreates the pool and reuses the same names, so
/// one profile holds several tids under one name.
pub(crate) fn shared_names(profile: &Profile) -> Vec<bool> {
    let threads = profile.threads();
    let mut counts: HashMap<Str, usize> = HashMap::with_capacity(threads.len());
    for track in threads {
        *counts.entry(track.name).or_insert(0) += 1;
    }
    threads
        .iter()
        .map(|track| counts.get(&track.name).is_some_and(|&n| n > 1))
        .collect()
}

/// How a track reads in the flame chart's gutter and the tracks menu: its
/// thread name, qualified by tid where the name is one several tracks carry, so
/// rows that would otherwise read identically name the thread they stand for.
/// `shared` is [`shared_names`] over this same `profile`, indexed alike.
pub(crate) fn track_label<'a>(profile: &'a Profile, shared: &[bool], track: usize) -> Cow<'a, str> {
    let thread = &profile.threads()[track];
    qualified(profile, shared, track, profile.str(thread.name))
}

/// How a track reads under a header naming its pool, which spells the run's
/// shared name once so each lane under it need only carry
/// [`distinguishing_part`] — what a gutter too narrow for the full name would
/// otherwise clip away.
pub(crate) fn pool_member_label<'a>(
    profile: &'a Profile,
    shared: &[bool],
    track: usize,
) -> Cow<'a, str> {
    let thread = &profile.threads()[track];
    let name = profile.str(thread.name);
    qualified(
        profile,
        shared,
        track,
        distinguishing_part(thread.tid, name),
    )
}

/// `name`, read for `track` as [`track_label`] reads a whole thread name.
fn qualified<'a>(profile: &Profile, shared: &[bool], track: usize, name: &'a str) -> Cow<'a, str> {
    if shared[track] {
        Cow::Owned(format!("{name} (tid {})", profile.threads()[track].tid))
    } else {
        Cow::Borrowed(name)
    }
}

/// What tells the thread named by `tid`/`name` from the rest of its pool: the
/// name past its [`track_family`] prefix and any single `-`, `_`, or space
/// binding the two. `name` itself where the family does not lead it or where
/// nothing would be left.
fn distinguishing_part(tid: i64, name: &str) -> &str {
    let Some(rest) = name.strip_prefix(track_family(tid, name)) else {
        return name;
    };
    let rest = rest.strip_prefix(['-', '_', ' ']).unwrap_or(rest);
    if rest.is_empty() { name } else { rest }
}

#[cfg(test)]
mod tests {
    use super::{pool_member_label, shared_names, track_label};
    use crate::test_profile::profile_of;
    use bviz_trace_profile::Profile;
    use std::borrow::Cow;

    /// The track `tid` names, which the profile is expected to hold exactly one
    /// of.
    fn track_of(profile: &Profile, tid: i64) -> usize {
        profile
            .threads()
            .iter()
            .position(|t| t.tid == tid)
            .unwrap_or_else(|| panic!("no track for tid {tid}"))
    }

    #[test]
    fn a_name_two_tracks_carry_reads_with_the_tid_of_each() {
        const POOL: &str = "skyframe-evaluator-memoizing-0";
        let profile = profile_of(&[
            (POOL.to_owned(), 1),
            (POOL.to_owned(), 1),
            ("main".to_owned(), 1),
        ]);
        let shared = shared_names(&profile);

        assert_eq!(
            track_label(&profile, &shared, track_of(&profile, 1)),
            format!("{POOL} (tid 1)")
        );
        assert_eq!(
            track_label(&profile, &shared, track_of(&profile, 2)),
            format!("{POOL} (tid 2)")
        );
        assert_eq!(
            track_label(&profile, &shared, track_of(&profile, 3)),
            "main"
        );
    }

    #[test]
    fn a_lane_under_a_pool_header_reads_as_what_tells_it_from_its_siblings() {
        const REPO_TRACK: &str =
            "starlark-repository-rules_rs++crate+crates__hashbrown-0.15.5-0 (Virtual)";
        let profile = profile_of(&[
            ("skyframe-evaluator-cpu-heavy-9".to_owned(), 1),
            (REPO_TRACK.to_owned(), 1),
            ("downloads[3]".to_owned(), 1),
            ("Main Thread".to_owned(), 1),
        ]);
        let shared = shared_names(&profile);
        let read = |tid| pool_member_label(&profile, &shared, track_of(&profile, tid));

        assert_eq!(read(1), "9");
        assert_eq!(
            read(2),
            "rules_rs++crate+crates__hashbrown-0.15.5-0 (Virtual)",
            "the pool's prefix goes, the repo that distinguishes the track stays"
        );
        assert_eq!(
            read(3),
            "[3]",
            "a family the name binds without a separator"
        );
        assert_eq!(
            read(4),
            "Main Thread",
            "a track that is its whole pool keeps its name"
        );
        for tid in 1..=4 {
            assert!(
                matches!(read(tid), Cow::Borrowed(_)),
                "a bare label costs no allocation"
            );
        }
    }

    #[test]
    fn a_pool_members_tid_qualifier_survives_the_pool_prefix_going() {
        const MEMBER: &str = "skyframe-evaluator-memoizing-1";
        let profile = profile_of(&[(MEMBER.to_owned(), 1), (MEMBER.to_owned(), 1)]);
        let shared = shared_names(&profile);

        assert_eq!(
            pool_member_label(&profile, &shared, track_of(&profile, 1)),
            "1 (tid 1)"
        );
        assert_eq!(
            pool_member_label(&profile, &shared, track_of(&profile, 2)),
            "1 (tid 2)"
        );
    }

    #[test]
    fn tracks_of_distinct_names_share_none_and_read_bare() {
        let profile = profile_of(&[
            ("main".to_owned(), 1),
            ("grpc-command-0".to_owned(), 1),
            ("skyframe-evaluator-memoizing-0".to_owned(), 1),
        ]);
        let shared = shared_names(&profile);

        assert_eq!(shared.len(), profile.threads().len());
        assert!(shared.iter().all(|s| !s), "{shared:?}");
        for (track, thread) in profile.threads().iter().enumerate() {
            let label = track_label(&profile, &shared, track);
            assert_eq!(label, profile.str(thread.name));
            assert!(
                matches!(label, Cow::Borrowed(_)),
                "a bare label costs no allocation"
            );
        }
    }
}
