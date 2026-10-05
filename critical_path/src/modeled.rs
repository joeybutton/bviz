//! A critical path held against its build's action graph, scheduled with
//! each action lasting as long as its profile span.

use crate::schedule::{Ceilings, Schedule};
use crate::{CriticalPath, Lever};
use bviz_invocation::{ActionGraph, ActionIndex};
use bviz_trace_profile::{Action, Profile, SliceAt};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

/// How many of the actions off the path closest to becoming critical are
/// kept.
pub const NEARLY_CRITICAL_KEPT: usize = 20;

/// The build as its action graph schedules it with no bound on how many
/// actions run at once, every action lasting as long as its span in the
/// profile, and one the profile holds no span of, as an action-cache hit,
/// taking no time.
#[derive(Debug, Clone, PartialEq)]
pub struct Modeled {
    length: Duration,
    path_ceiling: Duration,
    actions: usize,
    spanned: usize,
    timed: usize,
    cyclic: usize,
    nearly_critical: Vec<NearlyCritical>,
    /// By [lever](Lever::index), [`CriticalPath::lever_ceiling`].
    lever_ceilings: [Option<Duration>; Lever::ALL.len()],
}

/// A step's action as the modeled build holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepModel {
    /// How long the action lasts in the model: none where the profile holds
    /// no span of it.
    pub duration: Duration,
    /// How much shorter the modeled build is were the action to take no
    /// time: the most shortening the step can save before another chain of
    /// actions bounds the build.
    pub ceiling: Duration,
}

/// An action off the critical path that is close to being on it.
#[derive(Debug, Clone, PartialEq)]
pub struct NearlyCritical {
    /// Where the action's first span stands in the profile.
    pub at: SliceAt,
    pub description: String,
    pub mnemonic: Option<String>,
    pub target: Option<String>,
    pub primary_output: String,
    /// Its spans' time summed.
    pub duration: Duration,
    /// How much longer it could have taken without lengthening the build.
    pub slack: Duration,
}

impl Modeled {
    /// When the last action finishes.
    pub fn length(&self) -> Duration {
        self.length
    }

    /// How much shorter the build is were every step's action the graph
    /// holds to take no time: the most that shortening those actions can
    /// save, however it is done.
    pub fn path_ceiling(&self) -> Duration {
        self.path_ceiling
    }

    /// How many actions the graph holds.
    pub fn actions(&self) -> usize {
        self.actions
    }

    /// How many actions the profile holds a span of, by primary output.
    pub fn spanned(&self) -> usize {
        self.spanned
    }

    /// How many of the [spanned](Self::spanned) actions the graph holds, and
    /// so are timed in the model.
    pub fn timed(&self) -> usize {
        self.timed
    }

    /// How many of the graph's actions wait on a dependency cycle, and so are
    /// left out of the schedule.
    pub fn cyclic(&self) -> usize {
        self.cyclic
    }

    /// The actions off the path that took any time and whose slack is at
    /// most the [path ceiling](Self::path_ceiling), least slack first, at
    /// most [`NEARLY_CRITICAL_KEPT`] of them. An action with more slack than
    /// that stays off the path however the path's actions are shortened.
    pub fn nearly_critical(&self) -> &[NearlyCritical] {
        &self.nearly_critical
    }

    pub(crate) fn lever_ceiling(&self, lever: Lever) -> Option<Duration> {
        self.lever_ceilings[lever.index()]
    }
}

/// An action of the graph the profile holds a span of.
struct Timed<'p> {
    /// Its first span, in the order they started.
    span: &'p Action,
    primary_output: &'p str,
    /// Its spans' time summed.
    duration: Duration,
}

/// Models the build `graph` holds the actions of, timed by the spans of
/// `actions` from `profile`, and gives each step of `path` whose action the
/// graph holds its [model](crate::Step::model).
pub(crate) fn model(
    profile: &Profile,
    actions: &[Action],
    graph: &ActionGraph,
    path: &mut CriticalPath,
) -> Modeled {
    let timings = profile.timings_of(actions);
    let mut first_spans: HashMap<&str, &Action> = HashMap::with_capacity(actions.len());
    for action in actions {
        if let Some(out) = profile.opt_str(action.out) {
            first_spans.entry(out).or_insert(action);
        }
    }
    let timed: Vec<Option<Timed>> = graph
        .actions()
        .map(|action| {
            let primary_output = graph.primary_output(action)?;
            Some(Timed {
                span: first_spans.get(primary_output)?,
                primary_output,
                duration: timings.action(primary_output)?.wall,
            })
        })
        .collect();
    let schedule = Schedule::new(graph, |action| {
        timed[action.index()]
            .as_ref()
            .map_or(Duration::ZERO, |timed| timed.duration)
    });
    let by_primary_output: HashMap<&str, ActionIndex> = graph
        .actions()
        .filter_map(|action| Some((graph.primary_output(action)?, action)))
        .collect();
    let step_actions: Vec<Option<ActionIndex>> = path
        .steps
        .iter()
        .map(|step| by_primary_output.get(step.primary_output()?).copied())
        .collect();
    let mut ceilings = schedule.ceilings();
    for (step, action) in path.steps.iter_mut().zip(&step_actions) {
        if let &Some(action) = action {
            step.model = Some(StepModel {
                duration: schedule.duration(action),
                ceiling: ceilings.of(&[action]),
            });
        }
    }
    let path_actions: Vec<ActionIndex> = step_actions.iter().flatten().copied().collect();
    let on_path: HashSet<ActionIndex> = path_actions.iter().copied().collect();
    let path_ceiling = ceilings.of(&path_actions);
    let lever_ceilings =
        Lever::ALL.map(|lever| lever_ceiling(path, lever, &step_actions, &mut ceilings));
    Modeled {
        length: schedule.length(),
        path_ceiling,
        actions: graph.len(),
        spanned: first_spans.len(),
        timed: timed.iter().flatten().count(),
        cyclic: schedule.cyclic(),
        nearly_critical: nearly_critical(profile, &timed, &schedule, &on_path, path_ceiling),
        lever_ceilings,
    }
}

/// [`CriticalPath::lever_ceiling`] of `lever` on `path`, whose steps are
/// modeled, `step_actions` by step the action the graph holds of it.
fn lever_ceiling(
    path: &CriticalPath,
    lever: Lever,
    step_actions: &[Option<ActionIndex>],
    ceilings: &mut Ceilings,
) -> Option<Duration> {
    let mut acted_on = Vec::new();
    for (step, &action) in path.steps.iter().zip(step_actions) {
        if path.acted_on_in(lever, step).is_zero() {
            continue;
        }
        let timed = step.model.is_some_and(|model| !model.duration.is_zero());
        acted_on.push(action.filter(|_| timed)?);
    }
    (!acted_on.is_empty()).then(|| ceilings.of(&acted_on))
}

/// [`Modeled::nearly_critical`], `on_path` the actions of the path's steps
/// and `timed` by action of the schedule's graph.
fn nearly_critical(
    profile: &Profile,
    timed: &[Option<Timed>],
    schedule: &Schedule,
    on_path: &HashSet<ActionIndex>,
    path_ceiling: Duration,
) -> Vec<NearlyCritical> {
    let mut candidates: Vec<(Duration, Reverse<Duration>, &Timed)> = schedule
        .actions()
        .filter(|action| !on_path.contains(action))
        .filter_map(|action| {
            let timed = timed[action.index()].as_ref()?;
            let slack = schedule.finish(action)?.slack();
            Some((slack, Reverse(timed.duration), timed))
        })
        .filter(|&(slack, Reverse(duration), _)| !duration.is_zero() && slack <= path_ceiling)
        .collect();
    candidates.sort_unstable_by(|a, b| {
        (a.0, a.1, a.2.primary_output).cmp(&(b.0, b.1, b.2.primary_output))
    });
    candidates.truncate(NEARLY_CRITICAL_KEPT);
    candidates
        .into_iter()
        .map(|(slack, _, timed)| NearlyCritical {
            at: timed.span.at,
            description: profile.str(timed.span.description).to_owned(),
            mnemonic: profile.opt_str(timed.span.mnemonic).map(str::to_owned),
            target: profile.opt_str(timed.span.target).map(str::to_owned),
            primary_output: timed.primary_output.to_owned(),
            duration: timed.duration,
            slack,
        })
        .collect()
}
