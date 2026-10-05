use crate::summary::Summary;
use bviz_execlog::graph::{DatedNodes, GraphLog};
use bviz_invocation::spawn::{DatedSpawns, LoggedSpawns, Placement};
use bviz_invocation::{AttemptSource, Attempts, Revision, Session};
use bviz_ui_components::{Pending, load_in_background};
use bviz_ui_playback::{DocumentClock, OnClock, Span};
use gpui::{Context, Entity, Subscription, Task, WeakEntity};
use std::sync::Arc;

pub(crate) type Source = bviz_ui_components::Source<Summary>;

/// What reading an execution log yields.
pub(crate) struct Read {
    pub(crate) summary: Summary,
    pub(crate) spawns: LoggedSpawns,
}

/// What reading an execution graph log yields.
pub(crate) struct GraphRead {
    pub(crate) summary: Summary,
    pub(crate) log: GraphLog,
}

/// The summary of a log, held apart from the panel showing it so the panel
/// can close and be shown again without the log being read afresh.
pub(crate) struct ExecLogModel {
    log: Log,
    source: Source,
    on_clock: Option<OnClock>,
    /// The read that fills `source`, held so that it is not cancelled.
    _load: Task<()>,
}

/// Which kind of log a model reads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LogKind {
    Execution,
    ExecutionGraph,
}

/// The log a model reads.
enum Log {
    Execution,
    /// Joining once read, where it is read as one of an invocation's outputs.
    ExecutionGraph(Option<Joining>),
}

/// A graph log's attempts as reported to its invocation's session, joined to
/// the spawns of the execution log the session holds, which may be read after
/// the graph log: the attempts are dated again whenever those spawns change.
struct Joining {
    log: Arc<GraphLog>,
    /// Which report of the session's logged spawns the attempts last dated
    /// were joined to.
    joined: Revision,
    _observer: Subscription,
    /// The dating last started, held so that it is not cancelled.
    _dating: Task<()>,
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
            log: Log::Execution,
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

    /// The summary of the execution graph log `log` reads, and the attempts its
    /// nodes make, reported to `session` where it is read as one of an
    /// invocation's outputs, and played on `clock` where its document plays
    /// back.
    pub(crate) fn reading_graph(
        log: Pending<GraphRead>,
        session: Option<Entity<Session>>,
        clock: Option<DocumentClock>,
        cx: &mut Context<Self>,
    ) -> Self {
        let dating = clock.is_some();
        Self {
            log: Log::ExecutionGraph(None),
            source: Source::Loading,
            on_clock: clock.map(OnClock::new),
            _load: load_in_background(
                cx,
                move || {
                    log.join().map(|read| {
                        let dated = dating.then(|| read.log.dated(None)).flatten();
                        (read, dated)
                    })
                },
                move |model, result, cx| {
                    let (GraphRead { summary, log }, dated) = match result {
                        Ok(read) => read,
                        Err(why) => {
                            model.source = Source::Failed(why);
                            return;
                        }
                    };
                    model.source = Source::Loaded(summary);
                    if let Some(on_clock) = &mut model.on_clock {
                        let span = dated
                            .as_ref()
                            .and_then(|dated| Span::new(dated.span.start(), dated.span.end()));
                        on_clock.place(span.map(bviz_ui_playback::Source::new), cx);
                    }
                    if let Some(session) = session {
                        model.join(Arc::new(log), &session, cx);
                    }
                },
            ),
        }
    }

    /// Reports the attempts `log` makes to `session`, and again each time the
    /// session's logged spawns change.
    fn join(&mut self, log: Arc<GraphLog>, session: &Entity<Session>, cx: &mut Context<Self>) {
        let observer = cx.observe(session, |model, session, cx| {
            let reported = session.read(cx).logged_spawns_revision();
            if let Log::ExecutionGraph(Some(joining)) = &mut model.log
                && joining.joined != reported
            {
                joining.date(&session, cx);
            }
        });
        let mut joining = Joining {
            log,
            joined: Revision::default(),
            _observer: observer,
            _dating: Task::ready(()),
        };
        joining.date(session, cx);
        self.log = Log::ExecutionGraph(Some(joining));
    }

    pub(crate) fn source(&self) -> &Source {
        &self.source
    }

    pub(crate) fn log(&self) -> LogKind {
        match self.log {
            Log::Execution => LogKind::Execution,
            Log::ExecutionGraph(_) => LogKind::ExecutionGraph,
        }
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

impl Joining {
    /// Dates the graph log's nodes off the UI thread, joined to the spawns
    /// `session` holds, and reports the attempts to it.
    fn date(&mut self, session: &Entity<Session>, cx: &mut Context<ExecLogModel>) {
        let held = session.read(cx);
        self.joined = held.logged_spawns_revision();
        let logged = held.logged_spawns().cloned();
        let log = self.log.clone();
        let session = session.downgrade();
        self._dating = load_in_background(
            cx,
            move || log.dated(logged.as_deref()),
            move |_, dated, cx| {
                session
                    .update(cx, |session, cx| {
                        report_graph(session, dated);
                        cx.notify();
                    })
                    .ok();
            },
        );
    }
}

/// Tells `session` of the attempts the nodes of a graph log make.
fn report_graph(session: &mut Session, dated: Option<DatedNodes>) {
    let attempts = dated.map_or_else(Attempts::default, |dated| dated.attempts);
    session.report_attempts(AttemptSource::ExecutionGraphLog, attempts);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_invocation::spawn::{Builder, Logged, Phase, PhaseTiming, REPORTED_PHASES};
    use bviz_invocation::{EpochMicros, Interval};
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
    fn a_graph_log_tells_the_session_its_attempts_as_its_own() {
        let start = EpochMicros::from_millis(1_781_439_245_231);
        let end = start.plus_micros(1_000);
        let mut attempts = Attempts::default();
        let up_to_date = bviz_invocation::Done::Cached(bviz_invocation::Cache::Action);
        let attempt = bviz_invocation::Attempt::ended(start, end, [], up_to_date);
        attempts.record(Some(OUTPUT), None, attempt);
        let mut session = Session::default();
        report_graph(
            &mut session,
            Some(DatedNodes {
                attempts,
                span: Interval::new(start, end).expect("ordered"),
            }),
        );
        let merged = session.attempts();
        let action = merged.action(OUTPUT).expect("the node's action");
        assert_eq!(action.source(), AttemptSource::ExecutionGraphLog);

        report_graph(&mut session, None);
        assert!(session.attempts().is_empty());
    }

    #[test]
    fn an_undated_log_tells_the_session_only_its_spawns() {
        let mut session = Session::default();
        report(&mut session, sandboxed(), None);
        assert!(session.logged_spawns().is_some());
        assert!(session.attempts().is_empty());
    }
}
