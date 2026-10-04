//! How long the actions of a build took and where they ran.

use std::collections::HashMap;
use std::time::Duration;

use crate::outcomes::PerTarget;
use crate::{ConfiguredTarget, Runner, Spawns};

/// What a profile says of one action, its executions taken together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    pub wall: Duration,
    pub runner: Runner,
}

impl Timing {
    fn of(wall: Duration, spawns: &Spawns) -> Self {
        Timing {
            wall,
            runner: spawns.runner(),
        }
    }
}

/// The executions of one action known by its primary output, taken together.
struct Executions {
    owner: Option<ConfiguredTarget>,
    wall: Duration,
    spawns: Spawns,
}

impl Executions {
    fn add(&mut self, wall: Duration, spawns: &Spawns) {
        self.wall = self.wall.saturating_add(wall);
        self.spawns.add(spawns);
    }

    fn timing(&self) -> Timing {
        Timing::of(self.wall, &self.spawns)
    }
}

/// Some actions taken together, summed over.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Share {
    pub actions: u32,
    pub wall: Duration,
}

impl Share {
    fn add(&mut self, other: Share) {
        self.actions = self.actions.saturating_add(other.actions);
        self.wall = self.wall.saturating_add(other.wall);
    }
}

/// The [`Share`] each runner ran of some actions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Spent([Share; Runner::ALL.len()]);

impl Spent {
    pub fn of(&self, runner: Runner) -> Share {
        self.0[runner.index()]
    }

    /// The share of every runner together.
    pub fn total(&self) -> Share {
        let mut total = Share::default();
        for share in self.0 {
            total.add(share);
        }
        total
    }

    /// Each runner that ran any of the actions, in the order of
    /// [`Runner::ALL`].
    pub fn runners(&self) -> impl Iterator<Item = (Runner, Share)> + '_ {
        Runner::ALL
            .into_iter()
            .map(|runner| (runner, self.of(runner)))
            .filter(|(_, share)| share.actions > 0)
    }

    pub fn add_action(&mut self, timing: Timing) {
        self.0[timing.runner.index()].add(Share {
            actions: 1,
            wall: timing.wall,
        });
    }

    pub fn add(&mut self, other: &Spent) {
        for (share, more) in self.0.iter_mut().zip(other.0) {
            share.add(more);
        }
    }
}

/// One action as a profile records it, which names it by its primary output
/// and its owner only when Bazel was asked to.
#[derive(Debug, Clone, Copy)]
pub struct Profiled<'a> {
    /// Its exec path, named under `--experimental_profile_include_primary_output`.
    pub primary_output: Option<&'a str>,
    /// Named under `--experimental_profile_include_target_label`.
    pub owner: Option<ConfiguredTarget<&'a str>>,
    /// How long the action's span lasted.
    pub wall: Duration,
    pub spawns: Spawns,
}

/// How long the actions of a build took and where they ran. An action is known
/// by its primary output where its profile names one, kept with its owner
/// where the profile names that too; one named only by its owner counts toward
/// the owner's [`Spent`]; one named by neither counts toward
/// [`unnamed`](Self::unnamed).
#[derive(Default)]
pub struct Timings {
    actions: HashMap<String, Executions>,
    by_owner: PerTarget<Spent>,
    unnamed: Spent,
    names_primary_outputs: bool,
    names_owners: bool,
}

impl Timings {
    pub fn with_capacity(actions: usize) -> Self {
        Self {
            actions: HashMap::with_capacity(actions),
            ..Self::default()
        }
    }

    /// Records the time `action` took. An action recorded again under the
    /// same primary output ran again, and is read as one action: its
    /// executions' times summed and its runner read from all their spawns.
    pub fn record(&mut self, action: Profiled) {
        self.names_primary_outputs |= action.primary_output.is_some();
        self.names_owners |= action.owner.is_some();
        match (action.primary_output, action.owner) {
            (Some(primary_output), owner) => {
                self.actions
                    .entry(primary_output.to_owned())
                    .and_modify(|executions| executions.add(action.wall, &action.spawns))
                    .or_insert_with(|| Executions {
                        owner: owner.map(ConfiguredTarget::owned),
                        wall: action.wall,
                        spawns: action.spawns,
                    });
            }
            (None, Some(owner)) => {
                let mut spent = Spent::default();
                spent.add_action(Timing::of(action.wall, &action.spawns));
                self.by_owner
                    .report(owner.owned(), spent, |spent, more| spent.add(&more));
            }
            (None, None) => self
                .unnamed
                .add_action(Timing::of(action.wall, &action.spawns)),
        }
    }

    /// Whether no action is recorded.
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty() && self.by_owner.is_empty() && self.unnamed.total().actions == 0
    }

    /// The action whose primary output has the exec path `primary_output`.
    pub fn action(&self, primary_output: &str) -> Option<Timing> {
        self.actions.get(primary_output).map(Executions::timing)
    }

    /// Each action known by its primary output, with that output's exec path
    /// and the action's owner where the profile named it.
    pub fn actions(
        &self,
    ) -> impl ExactSizeIterator<Item = (&str, Option<ConfiguredTarget<&str>>, Timing)> {
        self.actions.iter().map(|(primary_output, executions)| {
            (
                primary_output.as_str(),
                executions.owner.as_ref().map(ConfiguredTarget::borrowed),
                executions.timing(),
            )
        })
    }

    /// The actions of `owner` known by no primary output.
    pub fn owned_by(&self, owner: ConfiguredTarget<&str>) -> Option<&Spent> {
        self.by_owner.get(owner)
    }

    /// Each owner of actions known by no primary output, with those actions.
    pub fn by_owner(&self) -> impl Iterator<Item = (ConfiguredTarget<&str>, &Spent)> {
        self.by_owner.iter()
    }

    /// The actions known by neither a primary output nor an owner.
    pub fn unnamed(&self) -> &Spent {
        &self.unnamed
    }

    /// Every action recorded.
    pub fn spent(&self) -> Spent {
        let mut spent = self.unnamed.clone();
        for (_, owned) in self.by_owner() {
            spent.add(owned);
        }
        for (_, _, timing) in self.actions() {
            spent.add_action(timing);
        }
        spent
    }

    /// Whether any action recorded is named by its primary output.
    pub fn names_primary_outputs(&self) -> bool {
        self.names_primary_outputs
    }

    /// Whether any action recorded is named by its owner.
    pub fn names_owners(&self) -> bool {
        self.names_owners
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "55817e2200e2";

    fn timing(millis: u64, runner: Runner) -> Timing {
        Timing {
            wall: Duration::from_millis(millis),
            runner,
        }
    }

    fn owner(label: &str) -> Option<ConfiguredTarget<&str>> {
        Some(ConfiguredTarget::new(label, Some(CONFIG)))
    }

    fn profiled<'a>(
        primary_output: Option<&'a str>,
        owner: Option<ConfiguredTarget<&'a str>>,
        timing: Timing,
    ) -> Profiled<'a> {
        Profiled {
            primary_output,
            owner,
            wall: timing.wall,
            spawns: Spawns::on(timing.runner, timing.wall),
        }
    }

    fn share(actions: u32, millis: u64) -> Share {
        Share {
            actions,
            wall: Duration::from_millis(millis),
        }
    }

    #[test]
    fn an_action_named_by_its_primary_output_is_found_by_it() {
        let mut timings = Timings::default();
        let compiled = timing(30, Runner::RemoteExecution);
        timings.record(profiled(Some("bazel-out/a.o"), owner("//a"), compiled));

        assert_eq!(timings.action("bazel-out/a.o"), Some(compiled));
        assert_eq!(timings.action("bazel-out/b.o"), None);
        assert_eq!(timings.owned_by(owner("//a").unwrap()), None);
        assert_eq!(
            timings.actions().collect::<Vec<_>>(),
            [("bazel-out/a.o", owner("//a"), compiled)]
        );
    }

    #[test]
    fn an_action_run_again_sums_its_time_and_ran_where_its_executions_held_longest() {
        let mut timings = Timings::default();
        timings.record(profiled(
            Some("t.log"),
            None,
            timing(5, Runner::RemoteExecution),
        ));
        timings.record(profiled(Some("t.log"), None, timing(20, Runner::Sandboxed)));
        timings.record(profiled(
            Some("t.log"),
            None,
            timing(1, Runner::DiskCacheHit),
        ));

        assert_eq!(timings.action("t.log"), Some(timing(26, Runner::Sandboxed)));
    }

    #[test]
    fn executions_of_one_action_pool_their_spawns_before_a_runner_is_read() {
        let mut timings = Timings::default();
        let mut sandboxed = Spawns::on(Runner::Sandboxed, Duration::from_millis(40));
        sandboxed.ran(Runner::RemoteExecution, Duration::from_millis(10));
        let wall = Duration::from_millis(60);
        let first = Profiled {
            primary_output: Some("t.log"),
            owner: None,
            wall,
            spawns: sandboxed,
        };
        assert_eq!(first.spawns.runner(), Runner::Sandboxed);
        timings.record(first);
        timings.record(profiled(
            Some("t.log"),
            None,
            timing(35, Runner::RemoteExecution),
        ));

        assert_eq!(
            timings.action("t.log"),
            Some(timing(95, Runner::RemoteExecution))
        );
    }

    #[test]
    fn actions_named_only_by_their_owner_sum_under_it_per_runner() {
        let mut timings = Timings::default();
        timings.record(profiled(None, owner("//a"), timing(10, Runner::Sandboxed)));
        timings.record(profiled(None, owner("//a"), timing(5, Runner::Sandboxed)));
        timings.record(profiled(
            None,
            owner("//a"),
            timing(2, Runner::NoSpawnRecorded),
        ));
        let other_config = ConfiguredTarget::new("//a", None);
        timings.record(profiled(None, Some(other_config), timing(7, Runner::Local)));

        let a = timings.owned_by(owner("//a").unwrap()).expect("recorded");
        assert_eq!(
            a.runners().collect::<Vec<_>>(),
            [
                (Runner::Sandboxed, share(2, 15)),
                (Runner::NoSpawnRecorded, share(1, 2))
            ]
        );
        assert_eq!(a.total(), share(3, 17));
        let unconfigured = timings.owned_by(other_config).expect("recorded");
        assert_eq!(unconfigured.total(), share(1, 7));
        assert_eq!(timings.by_owner().count(), 2);
    }

    #[test]
    fn every_action_is_spent_however_it_is_named() {
        let mut timings = Timings::default();
        assert!(timings.is_empty());
        timings.record(profiled(Some("a.o"), None, timing(1, Runner::Worker)));
        timings.record(profiled(None, owner("//a"), timing(2, Runner::Worker)));
        timings.record(profiled(None, None, timing(4, Runner::Local)));

        assert!(!timings.is_empty());
        assert_eq!(timings.unnamed().total(), share(1, 4));
        let spent = timings.spent();
        assert_eq!(spent.of(Runner::Worker), share(2, 3));
        assert_eq!(spent.of(Runner::Local), share(1, 4));
        assert_eq!(spent.total(), share(3, 7));
    }

    #[test]
    fn timings_tell_which_names_their_profile_gave_actions() {
        let names = |actions: &[Profiled]| {
            let mut timings = Timings::default();
            for action in actions {
                timings.record(*action);
            }
            (timings.names_primary_outputs(), timings.names_owners())
        };
        let run = timing(1, Runner::Local);
        assert_eq!(names(&[]), (false, false));
        assert_eq!(names(&[profiled(None, None, run)]), (false, false));
        assert_eq!(names(&[profiled(None, owner("//a"), run)]), (false, true));
        assert_eq!(names(&[profiled(Some("a.o"), None, run)]), (true, false));
        assert_eq!(
            names(&[profiled(Some("a.o"), owner("//a"), run)]),
            (true, true)
        );
    }
}
