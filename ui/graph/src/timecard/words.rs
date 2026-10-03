//! The words the card says where a profile's action time went in.

use super::{Shown, Slow, Timecard};
use crate::colour::PRIMARY_OUTPUT_FLAG;
use crate::duration::lasted;
use crate::timing::Ran;
use bviz_invocation::{Share, Spent};

/// `count` actions, in words.
fn actions(count: u32) -> String {
    match count {
        1 => "1 action".to_owned(),
        count => format!("{count} actions"),
    }
}

/// What part of `whole` the share `part` is, by time, or by count where
/// `whole` took no time. A part that is neither none nor all of the whole
/// never rounds to either, and a part holding an action is never none.
fn part_of(part: Share, whole: Share) -> String {
    let none = part.actions == 0;
    let (part, whole) = match whole.wall.is_zero() {
        true => (f64::from(part.actions), f64::from(whole.actions)),
        false => (part.wall.as_secs_f64(), whole.wall.as_secs_f64()),
    };
    let percent = 100.0 * part / whole.max(f64::MIN_POSITIVE);
    if none {
        "none".to_owned()
    } else if percent < 0.5 {
        "under 1%".to_owned()
    } else if percent >= 99.5 && part < whole {
        "over 99%".to_owned()
    } else {
        format!("{percent:.0}%")
    }
}

/// Each runner that ran any of `spent`, with a line telling its share.
fn shares(spent: &Spent) -> impl Iterator<Item = (Ran, String)> + '_ {
    let whole = spent.total();
    spent.runners().map(move |(runner, share)| {
        let ran = Ran::On(runner);
        let line = format!(
            "{}: {}, {} ({})",
            ran.label(),
            actions(share.actions),
            lasted(share.wall),
            part_of(share, whole)
        );
        (ran, line)
    })
}

impl Timecard {
    pub(super) fn line(&self) -> String {
        let total = self.profiled.total();
        format!(
            "{} profiled, {} of action time",
            actions(total.actions),
            lasted(total.wall)
        )
    }

    /// What heads the list of the slowest actions, or says why there is
    /// none.
    pub(super) fn list_line(&self) -> String {
        match self.slowest.is_empty() {
            true => format!(
                "The profile names no action by its primary output to list; \
                 record one with {PRIMARY_OUTPUT_FLAG}"
            ),
            false => "slowest actions".to_owned(),
        }
    }

    /// Each runner that ran any of the profiled actions, with a line telling
    /// its share of them.
    pub(super) fn runner_lines(&self) -> impl Iterator<Item = (Ran, String)> + '_ {
        shares(&self.profiled)
    }
}

/// What a row adds to its account of an action the graph holds neither a
/// node nor an owner for.
const OFF_GRAPH: &str = "not in this graph";

/// What a row adds, before the owner's label, to its account of an action the
/// graph holds no node for but holds the owner of.
const NOT_DRAWN: &str = "not drawn, in";

impl Slow {
    pub(super) fn subject(&self) -> &str {
        &self.primary_output
    }

    /// How long the action took and where it ran, and where the graph does
    /// not hold it, what holds it in its place, if anything.
    pub(super) fn detail(&self) -> String {
        let ran = Ran::On(self.timing.runner).label();
        let took = lasted(self.timing.wall);
        match &self.shown {
            Shown::Action(_) => format!("{took} · {ran}"),
            Shown::Owner { owner, .. } => format!("{took} · {ran} · {NOT_DRAWN} {owner}"),
            Shown::Off => format!("{took} · {ran} · {OFF_GRAPH}"),
        }
    }

    /// The row's hover: what the action is, `mnemonic` saying of what kind,
    /// over its detail.
    pub(super) fn hover(&self, mnemonic: Option<&str>) -> String {
        let named = match mnemonic {
            Some(mnemonic) => format!("{mnemonic} writing {}", self.primary_output),
            None => self.primary_output.clone(),
        };
        format!("{named}\n{}", self.detail())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_graph_layout::Id;
    use bviz_invocation::{Runner, Timing};
    use std::time::Duration;

    fn timing(millis: u64, runner: Runner) -> Timing {
        Timing {
            wall: Duration::from_millis(millis),
            runner,
        }
    }

    #[test]
    fn each_runner_is_said_to_hold_its_share_of_the_time() {
        let mut profiled = Spent::default();
        for action in [
            timing(3_000, Runner::RemoteExecution),
            timing(5_000, Runner::RemoteExecution),
            timing(2_000, Runner::Sandboxed),
            timing(1, Runner::NoSpawnRecorded),
        ] {
            profiled.add_action(action);
        }
        let card = Timecard {
            profiled,
            slowest: Vec::new(),
        };

        assert_eq!(card.line(), "4 actions profiled, 10.0s of action time");
        let listless = card.list_line();
        assert!(listless.contains(PRIMARY_OUTPUT_FLAG), "{listless}");
        let lines: Vec<(Ran, String)> = card.runner_lines().collect();
        assert_eq!(
            lines,
            [
                (
                    Ran::On(Runner::RemoteExecution),
                    "remote execution: 2 actions, 8.0s (80%)".to_owned()
                ),
                (
                    Ran::On(Runner::Sandboxed),
                    "sandboxed: 1 action, 2.0s (20%)".to_owned()
                ),
                (
                    Ran::On(Runner::NoSpawnRecorded),
                    "no spawn recorded: 1 action, 1ms (under 1%)".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn a_share_short_of_the_whole_is_not_said_to_be_all_of_it() {
        let share = |actions, millis| Share {
            actions,
            wall: Duration::from_millis(millis),
        };
        let whole = share(2, 10_000);
        assert_eq!(part_of(share(1, 9_990), whole), "over 99%");
        assert_eq!(part_of(share(1, 10), whole), "under 1%");
        assert_eq!(part_of(whole, whole), "100%");
        assert_eq!(part_of(share(0, 0), whole), "none");
        assert_eq!(part_of(share(1, 0), whole), "under 1%");
    }

    #[test]
    fn actions_taking_no_time_share_it_by_count() {
        let mut profiled = Spent::default();
        for runner in [
            Runner::NoSpawnRecorded,
            Runner::NoSpawnRecorded,
            Runner::Local,
        ] {
            profiled.add_action(timing(0, runner));
        }
        let lines: Vec<String> = shares(&profiled).map(|(_, line)| line).collect();
        assert_eq!(
            lines,
            [
                "local, unsandboxed: 1 action, under 1ms (33%)",
                "no spawn recorded: 2 actions, under 1ms (67%)",
            ]
        );
    }

    #[test]
    fn a_slow_action_says_how_long_it_took_where_and_what_the_graph_holds_of_it() {
        let held = Slow {
            primary_output: "bazel-out/bin/app/app.o".to_owned(),
            timing: timing(12_300, Runner::RemoteCacheHit),
            shown: Shown::Action(Id(3)),
        };
        assert_eq!(held.detail(), "12.3s · remote cache hit");
        assert_eq!(
            held.hover(Some("CppCompile")),
            "CppCompile writing bazel-out/bin/app/app.o\n12.3s · remote cache hit"
        );
        let in_owner = Slow {
            shown: Shown::Owner {
                node: Id(2),
                owner: "//app:app".to_owned(),
            },
            ..held.clone()
        };
        assert_eq!(
            in_owner.detail(),
            "12.3s · remote cache hit · not drawn, in //app:app"
        );
        let elsewhere = Slow {
            shown: Shown::Off,
            ..held
        };
        assert_eq!(
            elsewhere.detail(),
            "12.3s · remote cache hit · not in this graph"
        );
        assert_eq!(
            elsewhere.hover(None),
            "bazel-out/bin/app/app.o\n12.3s · remote cache hit · not in this graph"
        );
    }
}
