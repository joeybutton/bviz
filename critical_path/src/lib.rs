//! Where the wall time of a Bazel build went along its critical path, as the
//! build's `--profile` records it.
//!
//! Bazel's critical path is a chain of actions, each waiting on the one
//! before. The profile's critical-path lane holds one span per step, timed
//! from when the action won its execution slot to when its outputs were
//! checked.

mod spans;

use bviz_trace_profile::{LaneClass, OptStr, Profile, TimeSpan};
use spans::Spans;
use std::borrow::Cow;
use std::time::Duration;

/// What Bazel wraps an action's description in to name its step.
const ACTION_NAME_PREFIX: &str = "action '";
const ACTION_NAME_SUFFIX: &str = "'";
/// What Bazel puts before a target's label to name the step building the
/// target's runfiles tree.
const RUNFILES_NAME_PREFIX: &str = "runfiles for ";
/// The mnemonic of the action building a runfiles tree, which also opens the
/// action's description.
const RUNFILES_MNEMONIC: &str = "RunfilesTree";

/// The profile's wall time, partitioned along its critical path: the
/// lead-in, then each step's wait and own time, then the tail add up to the
/// wall time exactly.
#[derive(Debug, Clone, PartialEq)]
pub struct CriticalPath {
    wall: Duration,
    steps: Vec<Step>,
    by_part: [Duration; Part::ALL.len()],
}

/// One step of the critical path.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    name: String,
    mnemonic: Option<String>,
    target: Option<String>,
    primary_output: Option<String>,
    start_us: i64,
    gap: Duration,
    work: Work,
}

/// What a step did with its own time, as far as the profile tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Work {
    /// An action whose span the profile holds.
    Spanned(SpanSplit),
    /// An action the profile holds no span of. Bazel opens none for an
    /// action its action cache answers, and a slim profile folds short spans
    /// into anonymous ones, so the profile alone cannot tell the two apart.
    Unspanned(Duration),
    /// An action Bazel records as a step taking no time: change pruning found
    /// its inputs unchanged, so it was not run.
    ChangePruned,
}

/// A spanned step's own time, around the action span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpanSplit {
    /// From the step's start until the span starts: input discovery and the
    /// action-cache check.
    pub before: Duration,
    /// From the span's start until the step's end. Bazel ends the step from
    /// within the span, so the span outlasts the step by only that ending.
    pub span: Duration,
}

/// A share of the wall time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Part {
    /// Until the first step starts.
    LeadIn,
    /// Between one step's end and the next's start.
    Waiting,
    BeforeAction,
    Action,
    /// The own time of a step the profile holds no action span of.
    Unspanned,
    /// After the last step ends.
    Tail,
}

impl Part {
    /// Every part, in the order time passes through them.
    pub const ALL: [Part; 6] = [
        Part::LeadIn,
        Part::Waiting,
        Part::BeforeAction,
        Part::Action,
        Part::Unspanned,
        Part::Tail,
    ];

    /// The part's place in [`ALL`](Self::ALL).
    pub const fn index(self) -> usize {
        self as usize
    }
}

impl Work {
    pub fn own(self) -> Duration {
        self.parts().iter().map(|&(_, time)| time).sum()
    }

    /// The own time by part, parts of no time included.
    fn parts(self) -> [(Part, Duration); 3] {
        let (before, span, unspanned) = match self {
            Work::Spanned(split) => (split.before, split.span, Duration::ZERO),
            Work::Unspanned(own) => (Duration::ZERO, Duration::ZERO, own),
            Work::ChangePruned => (Duration::ZERO, Duration::ZERO, Duration::ZERO),
        };
        [
            (Part::BeforeAction, before),
            (Part::Action, span),
            (Part::Unspanned, unspanned),
        ]
    }
}

impl Step {
    /// An action's description, or the name Bazel gives a runfiles tree's
    /// step.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn mnemonic(&self) -> Option<&str> {
        self.mnemonic.as_deref()
    }

    pub fn target(&self) -> Option<&str> {
        self.target.as_deref()
    }

    pub fn primary_output(&self) -> Option<&str> {
        self.primary_output.as_deref()
    }

    /// When the step started, on the profile's clock: microseconds since
    /// Bazel began running the command.
    pub fn start_us(&self) -> i64 {
        self.start_us
    }

    /// How long the step waited after the step before it ended; zero for the
    /// first step, whose wait is the path's lead-in.
    pub fn gap(&self) -> Duration {
        self.gap
    }

    /// The time the step adds to the path: of a step that starts before the
    /// one before it ends, only what runs past that end.
    pub fn own(&self) -> Duration {
        self.work.own()
    }

    pub fn work(&self) -> Work {
        self.work
    }

    /// The wait and own time by part, in the order they passed; parts the
    /// step spent no time in are left out.
    pub fn parts(&self) -> impl Iterator<Item = (Part, Duration)> {
        std::iter::once((Part::Waiting, self.gap))
            .chain(self.work.parts())
            .filter(|(_, time)| !time.is_zero())
    }
}

impl CriticalPath {
    /// The critical path the profile records, or `None` where it has no
    /// critical-path lane. Steps stand in the order they started.
    pub fn of(profile: &Profile) -> Option<CriticalPath> {
        let lane = profile
            .threads()
            .iter()
            .find(|track| track.lane_class == LaneClass::CriticalPath)?;
        let recorded: Vec<RecordedStep> = profile
            .slices(lane)
            .iter()
            .zip(profile.details(lane))
            .map(|(slice, detail)| {
                let name = profile.str(slice.name);
                let kind = match name.strip_prefix(RUNFILES_NAME_PREFIX) {
                    Some(label) => Recorded::Runfiles { name, label },
                    None => Recorded::Action {
                        description: name
                            .strip_prefix(ACTION_NAME_PREFIX)
                            .and_then(|name| name.strip_suffix(ACTION_NAME_SUFFIX))
                            .unwrap_or(name),
                    },
                };
                RecordedStep {
                    start_us: slice.start_us,
                    end_us: slice.end_us(),
                    mnemonic: detail.mnemonic,
                    target: detail.target,
                    primary_output: profile.opt_str(detail.out),
                    kind,
                }
            })
            .collect();
        let actions = profile.actions();
        let mut spans = Spans::new(profile, &actions, &recorded);
        let mut cursor = Cursor::new(profile.span());
        let wall = cursor.left();
        let lead_in = recorded
            .first()
            .map_or(Duration::ZERO, |first| cursor.advance_to(first.start_us));
        let steps = recorded
            .iter()
            .map(|recorded| recorded.resolve(profile, &mut spans, &mut cursor))
            .collect();
        let tail = cursor.left();
        Some(CriticalPath::new(wall, lead_in, steps, tail))
    }

    fn new(wall: Duration, lead_in: Duration, steps: Vec<Step>, tail: Duration) -> CriticalPath {
        let mut by_part = [Duration::ZERO; Part::ALL.len()];
        by_part[Part::LeadIn.index()] = lead_in;
        by_part[Part::Tail.index()] = tail;
        for (part, time) in steps.iter().flat_map(Step::parts) {
            by_part[part.index()] += time;
        }
        CriticalPath {
            wall,
            steps,
            by_part,
        }
    }

    /// The profile's wall time, from its first event to its last.
    pub fn wall(&self) -> Duration {
        self.wall
    }

    /// From the start of the profile until the first step starts: launching
    /// Bazel, loading, analysis, and whatever else ran before any action of
    /// the path could.
    pub fn lead_in(&self) -> Duration {
        self.time_in(Part::LeadIn)
    }

    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// From the end of the last step until the end of the profile.
    pub fn tail(&self) -> Duration {
        self.time_in(Part::Tail)
    }

    /// The steps' own time summed.
    pub fn path(&self) -> Duration {
        self.steps.iter().map(Step::own).sum()
    }

    /// The wall time spent in `part`; the parts add up to [`Self::wall`].
    pub fn time_in(&self, part: Part) -> Duration {
        self.by_part[part.index()]
    }

    /// The step that waited longest after the step before it ended, if any
    /// waited at all.
    pub fn longest_wait(&self) -> Option<&Step> {
        self.steps
            .iter()
            .filter(|step| !step.gap.is_zero())
            .max_by_key(|step| step.gap)
    }
}

/// A point walking forward through the profile's wall time, so the times it
/// passes partition the wall time.
struct Cursor {
    at: i64,
    end: i64,
}

impl Cursor {
    fn new(wall: TimeSpan) -> Cursor {
        Cursor {
            at: wall.start_us,
            end: wall.end_us.max(wall.start_us),
        }
    }

    /// Moves on to `to`, held within the wall time left, and returns the time
    /// passed.
    fn advance_to(&mut self, to: i64) -> Duration {
        let to = to.clamp(self.at, self.end);
        let passed = Duration::from_micros(to.abs_diff(self.at));
        self.at = to;
        passed
    }

    /// The wall time left.
    fn left(&self) -> Duration {
        Duration::from_micros(self.end.abs_diff(self.at))
    }
}

/// A step as the critical-path lane records it.
struct RecordedStep<'p> {
    start_us: i64,
    end_us: i64,
    mnemonic: OptStr,
    target: OptStr,
    primary_output: Option<&'p str>,
    kind: Recorded<'p>,
}

enum Recorded<'p> {
    Action { description: &'p str },
    Runfiles { name: &'p str, label: &'p str },
}

impl<'p> RecordedStep<'p> {
    /// The label of the target whose runfiles tree the step built.
    fn runfiles_label(&self) -> Option<&'p str> {
        match self.kind {
            Recorded::Runfiles { label, .. } => Some(label),
            Recorded::Action { .. } => None,
        }
    }

    /// The description of the action span that ran the step, as far as the
    /// step's name gives it. A runfiles tree's action is described by the
    /// tree's directory, named for the target's executable; this takes the
    /// executable to be named for the target, and knows the directory only for
    /// a target of the main repository, whose outputs carry no repository.
    fn span_description(&self) -> Option<Cow<'p, str>> {
        match self.kind {
            Recorded::Action { description } => Some(Cow::Borrowed(description)),
            Recorded::Runfiles { label, .. } => {
                let (package, name) = label.strip_prefix("//")?.split_once(':')?;
                let directory = if package.is_empty() {
                    format!("{name}.runfiles")
                } else {
                    format!("{package}/{name}.runfiles")
                };
                Some(Cow::Owned(format!("{RUNFILES_MNEMONIC} {directory}")))
            }
        }
    }

    /// The step waiting from `cursor` until it starts, its own time running
    /// on from there, its action joined to the span of `spans` that ran it.
    fn resolve(&self, profile: &Profile, spans: &mut Spans, cursor: &mut Cursor) -> Step {
        let gap = cursor.advance_to(self.start_us);
        let (work, span) = if self.end_us == self.start_us {
            (Work::ChangePruned, None)
        } else if let Some(action) = spans.claim(self) {
            let split = SpanSplit {
                before: cursor.advance_to(action.span.start_us),
                span: cursor.advance_to(self.end_us),
            };
            (Work::Spanned(split), Some(action))
        } else {
            (Work::Unspanned(cursor.advance_to(self.end_us)), None)
        };
        // What the step records of itself, else what the span that ran it does.
        let said = |of_step: OptStr, of_span: Option<OptStr>| {
            profile
                .opt_str(of_step)
                .or_else(|| profile.opt_str(of_span?))
                .map(str::to_owned)
        };
        let name = match self.kind {
            Recorded::Action { description } => description,
            Recorded::Runfiles { name, .. } => name,
        };
        Step {
            name: name.to_owned(),
            mnemonic: said(self.mnemonic, span.map(|action| action.mnemonic)),
            target: said(self.target, span.map(|action| action.target))
                .or_else(|| self.runfiles_label().map(str::to_owned)),
            primary_output: self
                .primary_output
                .or_else(|| profile.opt_str(span?.out))
                .map(str::to_owned),
            start_us: self.start_us,
            gap,
            work,
        }
    }
}

#[cfg(test)]
mod tests;
