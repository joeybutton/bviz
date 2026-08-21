use bviz_trace_profile::{Profile, Str};
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
/// thread name, qualified by tid where [`shared_names`] marks the name as one
/// several tracks carry, so rows that would otherwise read identically name the
/// thread they stand for.
pub(crate) fn track_label<'a>(profile: &'a Profile, shared: &[bool], track: usize) -> Cow<'a, str> {
    let thread = &profile.threads()[track];
    let name = profile.str(thread.name);
    match shared.get(track) {
        Some(true) => Cow::Owned(format!("{name} (tid {})", thread.tid)),
        _ => Cow::Borrowed(name),
    }
}

#[cfg(test)]
mod tests {
    use super::{shared_names, track_label};
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
