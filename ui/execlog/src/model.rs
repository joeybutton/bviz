use crate::summary::Summary;
use bviz_invocation::spawn::{DatedSpawns, LoggedSpawns, Placement};
use bviz_invocation::{AttemptSource, Session};
use bviz_ui_components::{Pending, load_in_background};
use bviz_ui_playback::{DocumentClock, OnClock, Span};
use gpui::{Context, Task, WeakEntity};

pub(crate) type Source = bviz_ui_components::Source<Summary>;

/// What reading an execution log yields.
pub(crate) struct Read {
    pub(crate) summary: Summary,
    pub(crate) spawns: LoggedSpawns,
}

/// The summary of an execution log, held apart from the panel showing it so
/// the panel can close and be shown again without the log being read afresh.
pub(crate) struct ExecLogModel {
    source: Source,
    on_clock: Option<OnClock>,
    /// The read that fills `source`, held so that it is not cancelled.
    _load: Task<()>,
}

impl ExecLogModel {
    /// The summary of the log `log` reads, its spawns, and the attempts they
    /// make where the log can be dated, reported to `session` where it is
    /// read as one of an invocation's outputs, and played on `clock` where
    /// its document plays back.
    pub(crate) fn reading(
        log: Pending<Read>,
        session: Option<WeakEntity<Session>>,
        clock: Option<DocumentClock>,
        cx: &mut Context<Self>,
    ) -> Self {
        let dating = session.is_some() || clock.is_some();
        Self {
            source: Source::Loading,
            on_clock: clock.map(OnClock::new),
            _load: load_in_background(
                cx,
                move || {
                    log.join().map(|read| {
                        let dated = dating.then(|| read.spawns.dated()).flatten();
                        (read, dated)
                    })
                },
                move |model, result, cx| {
                    let (read, dated) = match result {
                        Ok(read) => read,
                        Err(why) => {
                            model.source = Source::Failed(why);
                            return;
                        }
                    };
                    let Read { summary, spawns } = read;
                    let placement = dated.as_ref().map(|dated| dated.placement);
                    if let Some(session) = session {
                        session
                            .update(cx, |session, cx| {
                                report(session, spawns, dated);
                                cx.notify();
                            })
                            .ok();
                    }
                    model.source = Source::Loaded(summary);
                    if let Some(on_clock) = &mut model.on_clock {
                        on_clock.place(placement.and_then(clock_source), cx);
                    }
                },
            ),
        }
    }

    pub(crate) fn source(&self) -> &Source {
        &self.source
    }
}

/// What a log whose spawns stand at `placement` gives a clock to play: their
/// span, approximate where any spawn's time is interpolated; `None` where
/// they take no time.
fn clock_source(placement: Placement) -> Option<bviz_ui_playback::Source> {
    let placed = placement.span();
    let span = Span::new(placed.start(), placed.end())?;
    let source = bviz_ui_playback::Source::new(span);
    Some(match placement.interpolated() {
        true => source.approximate(),
        false => source,
    })
}

/// Tells `session` of the spawns a log records and of the attempts they make
/// where the log could be dated.
fn report(session: &mut Session, spawns: LoggedSpawns, dated: Option<DatedSpawns>) {
    session.report_logged_spawns(spawns);
    if let Some(dated) = dated {
        session.report_attempts(AttemptSource::ExecutionLog, dated.attempts);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_invocation::EpochMicros;
    use bviz_invocation::spawn::{Builder, Logged, Phase, PhaseTiming, REPORTED_PHASES};
    use std::time::Duration;

    const OUTPUT: &str = "bazel-out/k8-fastbuild/bin/a";

    /// A log of one sandboxed spawn writing [`OUTPUT`].
    fn sandboxed() -> LoggedSpawns {
        let ran = Duration::from_millis(10);
        let mut reported = [Duration::ZERO; REPORTED_PHASES];
        reported[Phase::Execute.index()] = ran;
        let mut builder = Builder::with_capacity(1, 1, OUTPUT.len());
        builder.output(OUTPUT);
        builder.started(EpochMicros::from_millis(1_781_439_245_231));
        builder.push(Logged {
            runner: "linux-sandbox",
            mnemonic: "Genrule",
            target_label: "//p:a",
            cache_hit: false,
            timing: PhaseTiming::new(ran, reported),
        });
        builder.finish()
    }

    #[test]
    fn a_dated_log_tells_the_session_its_spawns_and_their_attempts() {
        let spawns = sandboxed();
        let dated = spawns.dated();
        assert!(dated.is_some());
        let mut session = Session::default();
        report(&mut session, spawns, dated);
        assert!(session.logged_spawns().is_some());
        let attempts = session.attempts();
        let action = attempts.action(OUTPUT).expect("the spawn's action");
        assert_eq!(action.source(), AttemptSource::ExecutionLog);
    }

    #[test]
    fn a_dated_log_plays_for_as_long_as_its_spawns_took() {
        let placement = sandboxed().dated().expect("a spawn ran here").placement;
        assert!(clock_source(placement).is_some());
    }

    #[test]
    fn an_undated_log_tells_the_session_only_its_spawns() {
        let mut session = Session::default();
        report(&mut session, sandboxed(), None);
        assert!(session.logged_spawns().is_some());
        assert!(session.attempts().is_empty());
    }
}
