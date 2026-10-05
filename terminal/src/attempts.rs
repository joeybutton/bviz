//! The execution attempts a build event stream records of its build's
//! actions.

use bviz_invocation::{Attempt, Attempts, Cache, ConfiguredTarget, Done, EpochMicros, SpawnCache};

use crate::bep::build_event_stream::{ActionExecuted, build_event_id::Id};
use crate::outcomes::target;
use crate::output::{Began, Position, epoch_opt};
use crate::replay::Dating;

/// When a recorded attempt took place, as far as its stream tells.
#[derive(Debug, Clone, Copy, PartialEq)]
enum When {
    /// From `start` until `end`, both instants of this build.
    Took {
        start: EpochMicros,
        end: EpochMicros,
    },
    /// Ended where the event at this position stands: the stream gives the
    /// attempt no span, or one a cache recorded, in whole or in part, of
    /// another run.
    EndedAt(Position),
}

/// One attempt as its stream records it.
#[derive(Debug)]
struct Recorded {
    primary_output: Option<String>,
    owner: Option<ConfiguredTarget>,
    when: When,
    done: Done,
}

/// The attempts a stream records, in stream order.
#[derive(Debug, Default)]
pub struct RecordedAttempts(Vec<Recorded>);

impl RecordedAttempts {
    /// Records the attempt an `ActionExecuted` event at `position` reports,
    /// which a stream holds of a failed action and, under
    /// `--build_event_publish_all_actions`, of every action executed. Its
    /// span runs from the first of its spawns' starts to the last of their
    /// ends, which for a spawn a cache answered are those of the run the
    /// cache recorded, so only a span ending before the build began is one a
    /// cache answered whole:
    /// <https://github.com/bazelbuild/bazel/blob/9.1.0/src/main/java/com/google/devtools/build/lib/skyframe/SkyframeActionExecutor.java#L1904-L1931>
    pub(crate) fn action(
        &mut self,
        id: Option<&Id>,
        action: &ActionExecuted,
        began: Began,
        position: Position,
    ) {
        let Some(Id::ActionCompleted(id)) = id else {
            return;
        };
        let start = epoch_opt(action.start_time);
        let end = epoch_opt(action.end_time).or(start);
        let (when, answered) = match start.zip(end) {
            Some((start, end)) if began.took(Some(start)) => (
                When::Took {
                    start: EpochMicros::from_secs_f64(start),
                    end: EpochMicros::from_secs_f64(end),
                },
                Done::Executed,
            ),
            Some((_, end)) if !began.took(Some(end)) => (
                When::EndedAt(position),
                Done::Cached(Cache::Spawn(SpawnCache::RemoteOrDisk)),
            ),
            Some(_) | None => (When::EndedAt(position), Done::Executed),
        };
        self.0.push(Recorded {
            primary_output: Some(id.primary_output.clone()).filter(|output| !output.is_empty()),
            owner: (!id.label.is_empty())
                .then(|| target(id.label.clone(), id.configuration.clone())),
            when,
            done: match action.success {
                true => answered,
                false => Done::Failed,
            },
        });
    }

    /// The attempts on the wall clock `dating` places the stream's events
    /// on. One the stream gives no span of this build's lasts no time, and
    /// ends where its event stands.
    pub fn dated(self, dating: Dating) -> Attempts {
        let mut attempts = Attempts::with_capacity(self.0.len());
        for recorded in self.0 {
            let (start, end) = match recorded.when {
                When::Took { start, end } => (start, end),
                When::EndedAt(position) => {
                    let at = dating.instant_at(position);
                    (at, at)
                }
            };
            attempts.record(
                recorded.primary_output.as_deref(),
                recorded.owner.as_ref().map(ConfiguredTarget::borrowed),
                Attempt::ended(start, end, [], recorded.done),
            );
        }
        attempts
    }
}

#[cfg(test)]
mod tests {
    use prost::Message;

    use super::*;
    use crate::bep::build_event_stream::{
        self as bes, BuildEvent, BuildEventId,
        build_event::Payload,
        build_event_id::{ActionCompletedId, ConfigurationId},
    };
    use crate::bep::timestamp_proto::google::protobuf::Timestamp;
    use crate::{Replay, parse};

    const BEGAN: i64 = 1_790_952_715;
    const CONFIG: &str = "a7a71fd16b14";

    fn at(seconds: i64) -> Option<Timestamp> {
        Some(Timestamp { seconds, nanos: 0 })
    }

    fn event(id: Option<Id>, payload: Payload) -> Vec<u8> {
        BuildEvent {
            id: id.map(|id| BuildEventId { id: Some(id) }),
            payload: Some(payload),
            ..Default::default()
        }
        .encode_length_delimited_to_vec()
    }

    fn progress() -> Vec<u8> {
        event(
            None,
            Payload::Progress(bes::Progress {
                stderr: "x".into(),
                ..Default::default()
            }),
        )
    }

    fn executed(primary_output: &str, executed: ActionExecuted) -> Vec<u8> {
        let id = Id::ActionCompleted(ActionCompletedId {
            primary_output: primary_output.into(),
            label: "//app:cli".into(),
            configuration: Some(ConfigurationId { id: CONFIG.into() }),
        });
        event(Some(id), Payload::Action(executed))
    }

    #[test]
    fn an_action_is_attempted_over_its_span_of_this_build_and_otherwise_where_it_ended() {
        let stream = [
            event(
                None,
                Payload::Started(bes::BuildStarted {
                    start_time: at(BEGAN),
                    ..Default::default()
                }),
            ),
            progress(),
            executed(
                "bazel-out/bin/ran",
                ActionExecuted {
                    success: true,
                    start_time: at(BEGAN + 1),
                    end_time: at(BEGAN + 3),
                    ..Default::default()
                },
            ),
            progress(),
            executed(
                "bazel-out/bin/cached",
                ActionExecuted {
                    success: true,
                    start_time: at(BEGAN - 600),
                    end_time: at(BEGAN - 590),
                    ..Default::default()
                },
            ),
            executed(
                "bazel-out/bin/partly_cached",
                ActionExecuted {
                    success: true,
                    start_time: at(BEGAN - 600),
                    end_time: at(BEGAN + 2),
                    ..Default::default()
                },
            ),
            executed("bazel-out/bin/broken", ActionExecuted::default()),
            progress(),
            event(
                None,
                Payload::Finished(bes::BuildFinished {
                    finish_time: at(BEGAN + 10),
                    ..Default::default()
                }),
            ),
        ]
        .concat();
        let mut output = parse(&stream).expect("parse stream");
        let recorded = std::mem::take(&mut output.attempts);
        let replay = Replay::from_output(output);
        let dating = replay.dating().expect("on the wall clock");
        let attempts = recorded.dated(dating);
        let instant = |seconds| EpochMicros::from_millis(seconds * 1_000);

        let ran = attempts.action("bazel-out/bin/ran").expect("ran");
        assert_eq!(
            ran,
            [Attempt::ended(
                instant(BEGAN + 1),
                instant(BEGAN + 3),
                [],
                Done::Executed
            )]
        );
        let ended = dating.instant_at(Position(2));
        assert_eq!(
            attempts.action("bazel-out/bin/cached"),
            Some(
                &[Attempt::ended(
                    ended,
                    ended,
                    [],
                    Done::Cached(Cache::Spawn(SpawnCache::RemoteOrDisk))
                )][..]
            )
        );
        assert_eq!(
            attempts.action("bazel-out/bin/partly_cached"),
            Some(&[Attempt::ended(ended, ended, [], Done::Executed)][..])
        );
        assert_eq!(
            attempts.action("bazel-out/bin/broken"),
            Some(&[Attempt::ended(ended, ended, [], Done::Failed)][..])
        );
        let (_, owner, _) = (attempts.actions())
            .find(|(output, _, _)| *output == "bazel-out/bin/ran")
            .expect("held");
        assert_eq!(
            owner,
            Some(ConfiguredTarget::new("//app:cli", Some(CONFIG)))
        );
    }
}
