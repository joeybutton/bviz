//! An execution graph log's nodes as execution attempts on the local wall
//! clock, which every node is started on.

use bviz_invocation::spawn::LoggedSpawns;
use bviz_invocation::{Attempt, Attempts, DescribedAction, Done, Interval};
use rustc_hash::FxHashMap;

use super::GraphLog;

/// The execution attempts the nodes of a [`GraphLog`] make.
pub struct DatedNodes {
    pub attempts: Attempts,
    /// From the start of the first attempt to the end of the last.
    pub span: Interval,
}

impl GraphLog {
    /// Each node as an attempt of its action: a spawn as its
    /// [attempt](bviz_invocation::spawn::LoggedSpawn::attempt) lasting its
    /// total time from its start, and an action that ran no spawn as ending
    /// [without one](Done::Spawnless), for as long as it took. `None` for a
    /// log of no node.
    ///
    /// The log names no output, so a spawn is an attempt of the action writing
    /// one only where `logged`, the execution log of the same build, holds a
    /// spawn of its digest, which Bazel computes only for a spawn a remote
    /// executor or a remote or disk cache could answer, and carries on the
    /// attempt of a test's run where that spawn
    /// [follows one](bviz_invocation::spawn::LoggedSpawn::follows_a_test_run);
    /// every other attempt is one of the action its owner, mnemonic and
    /// description describe.
    pub fn dated(&self, logged: Option<&LoggedSpawns>) -> Option<DatedNodes> {
        let spawns = &self.spawns.spawns;
        let keyed = match logged {
            Some(logged) if spawns.iter().any(|spawn| spawn.digest().is_some()) => {
                let mut keyed =
                    FxHashMap::with_capacity_and_hasher(logged.len(), Default::default());
                for spawn in logged.iter() {
                    if let (Some(digest), Some(output)) = (spawn.digest(), spawn.action_output()) {
                        keyed
                            .entry(digest)
                            .or_insert((output, spawn.follows_a_test_run()));
                    }
                }
                keyed
            }
            _ => FxHashMap::default(),
        };
        let joinable = match keyed.is_empty() {
            true => 0,
            false => spawns
                .iter()
                .filter(|spawn| spawn.digest().is_some())
                .count(),
        };
        let mut attempts = Attempts::with_capacity(joinable);
        attempts.reserve_described(spawns.len() - joinable + self.spawnless.len());
        let mut span: Option<Interval> = None;
        let mut spanning = |attempt: &Attempt| {
            span = Some(span.map_or(attempt.span(), |span| span.hull(attempt.span())));
        };
        for (spawn, start, action) in self.spawns_of() {
            let attempt = spawn.attempt(start.after(spawn.timing().total()));
            spanning(&attempt);
            let owner = action.map(DescribedAction::owner_target);
            match (spawn.digest().and_then(|digest| keyed.get(digest)), action) {
                (Some((output, true)), _) => {
                    attempts.record_continued(Some(output), owner, attempt)
                }
                (Some((output, false)), _) => attempts.record(Some(output), owner, attempt),
                (None, Some(action)) => attempts.record_described(action, attempt),
                (None, None) => {}
            }
        }
        for action in self.spawnless() {
            let end = action.start.after(action.took);
            let spawnless = Attempt::ended(action.start, end, [], Done::Spawnless);
            spanning(&spawnless);
            if let Some(described) = action.action {
                attempts.record_described(described, spawnless);
            }
        }
        Some(DatedNodes {
            attempts,
            span: span?,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bazel_spawn_proto::execution_graph::{Metrics, Node};
    use bviz_invocation::spawn::{Builder, Logged, PhaseTiming, REPORTED_PHASES};
    use bviz_invocation::{Cache, ConfiguredTarget, Phase, SpawnCache, State, Venue};

    use super::*;
    use crate::graph::tests::{at, load, node};

    const DIGEST: &str = "9f86d081";
    const OUTPUT: &str = "bazel-out/k8-fastbuild/bin/p/t.out";

    /// An execution log of one spawn of `mnemonic` writing `output`, keyed by
    /// [`DIGEST`].
    fn logged(mnemonic: &str, output: &str) -> LoggedSpawns {
        let mut builder = Builder::with_capacity(1, 1, output.len());
        builder.output(output);
        builder.digest(DIGEST);
        builder.push(Logged {
            runner: "linux-sandbox",
            mnemonic,
            target_label: "//p:t",
            cache_hit: false,
            timing: PhaseTiming::new(Duration::ZERO, [Duration::ZERO; REPORTED_PHASES]),
        });
        builder.finish()
    }

    fn keyed(mut spawn: Node) -> Node {
        spawn.identifier = DIGEST.into();
        spawn
    }

    /// The action of `//p:t` that `description` describes.
    fn described_action(description: &str) -> DescribedAction<&str> {
        DescribedAction {
            owner: "//p:t",
            mnemonic: "Genrule",
            description,
        }
    }

    #[test]
    fn a_sandboxed_spawn_runs_while_its_sandbox_is_built_once_given_resources() {
        let mut spawn = node(
            "linux-sandbox",
            "action 'Executing genrule //p:t'",
            100,
            160,
        );
        spawn.metrics = Some(Metrics {
            setup_millis: 20,
            queue_millis: 30,
            process_millis: 100,
            ..spawn.metrics.expect("timed")
        });
        let dated = load(&[keyed(spawn)])
            .dated(Some(&logged("Genrule", OUTPUT)))
            .expect("a node");
        let tried = dated.attempts.action(OUTPUT).expect("joined by digest");
        assert_eq!(tried.len(), 1);
        let a = tried[0];
        assert_eq!((a.start(), a.end()), (at(100), at(260)));
        assert_eq!(a.entered(Phase::Scheduling), Some(at(110)));
        assert_eq!(a.entered(Phase::Running(Venue::Local)), Some(at(140)));
        assert_eq!(a.done(), Done::Executed);
        assert_eq!(dated.span, a.span());
    }

    #[test]
    fn a_spawn_s_attempt_starts_with_its_action_discovering_inputs() {
        let mut spawn = node("local", "action 'Compiling a.cc'", 100, 60);
        spawn.metrics = Some(Metrics {
            discover_inputs_millis: 10,
            queue_millis: 5,
            process_millis: 40,
            ..spawn.metrics.expect("timed")
        });
        let dated = load(&[spawn]).dated(None).expect("a node");
        let described: Vec<_> = dated.attempts.described().collect();
        let a = described[0].1[0];
        assert_eq!((a.start(), a.end()), (at(90), at(150)));
        assert_eq!(a.entered(Phase::Scheduling), Some(at(105)));
        assert_eq!(a.entered(Phase::Running(Venue::Local)), Some(at(110)));
    }

    #[test]
    fn a_spawn_a_cache_answered_ended_cached_by_that_cache() {
        let dated = load(&[keyed(node("remote cache hit", "action 'a'", 0, 5))])
            .dated(Some(&logged("Genrule", OUTPUT)))
            .expect("a node");
        let a = dated.attempts.action(OUTPUT).expect("joined")[0];
        assert_eq!(a.state_at(at(1)), State::In(Phase::CheckingCache));
        assert_eq!(a.done(), Done::Cached(Cache::Spawn(SpawnCache::Remote)));
    }

    #[test]
    fn a_test_s_spawn_is_an_attempt_of_the_action_writing_its_log() {
        let dated = load(&[keyed(node("linux-sandbox", "action 'Testing //p:t'", 0, 5))])
            .dated(Some(&logged(
                "TestRunner",
                "bazel-out/k8/testlogs/p/t/test.xml",
            )))
            .expect("a node");
        assert!(
            dated
                .attempts
                .action("bazel-out/k8/testlogs/p/t/test.log")
                .is_some()
        );
    }

    #[test]
    fn a_spawn_following_a_test_s_run_carries_on_its_attempt() {
        let log = "bazel-out/k8/testlogs/p/t/test.log";
        let mut run = node("linux-sandbox", "action 'Testing //p:t'", 0, 5);
        run.identifier = "60303ae2".into();
        let report = keyed(node("linux-sandbox", "action 'Testing //p:t'", 10, 5));
        let mut builder = Builder::with_capacity(2, 3, 0);
        for (digest, outputs) in [
            (
                "60303ae2",
                &[
                    "bazel-out/k8/testlogs/p/t/test.xml",
                    "bazel-out/k8/testlogs/p/t/test.exited_prematurely",
                ][..],
            ),
            (DIGEST, &["bazel-out/k8/testlogs/p/t/test.xml"][..]),
        ] {
            for output in outputs {
                builder.output(output);
            }
            builder.digest(digest);
            builder.push(Logged {
                runner: "linux-sandbox",
                mnemonic: "TestRunner",
                target_label: "//p:t",
                cache_hit: false,
                timing: PhaseTiming::new(Duration::ZERO, [Duration::ZERO; REPORTED_PHASES]),
            });
        }
        let dated = load(&[run, report])
            .dated(Some(&builder.finish()))
            .expect("nodes");
        let tried = dated.attempts.action(log).expect("joined by digest");
        assert_eq!(tried.len(), 1);
        assert_eq!((tried[0].start(), tried[0].end()), (at(0), at(15)));
    }

    #[test]
    fn a_spawn_with_no_spawn_of_its_digest_logged_is_of_its_described_action() {
        let log = load(&[
            keyed(node("local", "action 'a'", 0, 5)),
            node("local", "action 'b'", 10, 5),
        ]);
        fn described(dated: &DatedNodes) -> Vec<(DescribedAction<&str>, usize)> {
            let mut described: Vec<_> = dated
                .attempts
                .described()
                .map(|(action, tried)| (action, tried.len()))
                .collect();
            described.sort_unstable_by_key(|(action, _)| action.description);
            described
        }
        let unjoined = log.dated(None).expect("nodes");
        assert_eq!(
            described(&unjoined),
            [(described_action("a"), 1), (described_action("b"), 1)]
        );
        let joined = log.dated(Some(&logged("Genrule", OUTPUT))).expect("nodes");
        assert_eq!(described(&joined), [(described_action("b"), 1)]);
        let owner = ConfiguredTarget::new("//p:t", None);
        assert_eq!(
            joined
                .attempts
                .actions()
                .map(|(output, owner, _)| (output, owner))
                .collect::<Vec<_>>(),
            [(OUTPUT, Some(owner))]
        );
        assert_eq!(joined.attempts.by_owner().count(), 0);
    }

    #[test]
    fn an_action_that_ran_no_spawn_ends_without_one_for_as_long_as_it_took() {
        let dated = load(&[
            node("", "action 'Executing genrule //p:t'", 100, 8),
            node("", "action 'Executing genrule //p:u'", 120, 0),
        ])
        .dated(None)
        .expect("nodes");
        let mut attempts: Vec<_> = dated
            .attempts
            .described()
            .flat_map(|(action, tried)| {
                tried
                    .iter()
                    .map(move |a| (action.description, a.start(), a.end(), a.done()))
            })
            .collect();
        attempts.sort_unstable_by_key(|&(_, start, _, _)| start);
        assert_eq!(
            attempts,
            [
                ("Executing genrule //p:t", at(100), at(108), Done::Spawnless),
                ("Executing genrule //p:u", at(120), at(120), Done::Spawnless)
            ]
        );
        assert_eq!(
            dated.span,
            Interval::new(at(100), at(120)).expect("ordered")
        );
    }

    #[test]
    fn a_log_of_no_node_has_no_attempts() {
        assert!(load(&[]).dated(None).is_none());
    }
}
