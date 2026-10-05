//! A build's actions scheduled by the critical-path method (Kelley and
//! Walker, "Critical-path planning and scheduling", 1959): each action starts
//! as soon as the last of its dependencies finishes, with no bound on how many
//! run at once.

use bviz_invocation::{ActionGraph, ActionIndex};
use std::time::Duration;

/// When an action finishes in a [`Schedule`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Finish {
    /// As soon as its dependencies and its own duration allow.
    pub earliest: Duration,
    /// As late as it can without delaying the end of the build.
    pub latest: Duration,
}

impl Finish {
    /// How long the action can be delayed without delaying the build.
    pub fn slack(self) -> Duration {
        self.latest.saturating_sub(self.earliest)
    }
}

/// The actions of a graph, each lasting its duration, scheduled as early as
/// their dependencies allow and as late as the build's length allows.
pub struct Schedule<'g> {
    graph: &'g ActionGraph,
    /// Every action that does not wait on a dependency cycle, each after its
    /// dependencies.
    order: Vec<ActionIndex>,
    durations: Vec<Duration>,
    /// By action; `None` for one that waits on a dependency cycle, which never
    /// starts.
    finishes: Vec<Option<Finish>>,
    length: Duration,
}

impl<'g> Schedule<'g> {
    /// `graph`'s actions, each lasting what `duration` gives it.
    pub fn new(graph: &'g ActionGraph, duration: impl Fn(ActionIndex) -> Duration) -> Self {
        let durations: Vec<Duration> = graph.actions().map(duration).collect();
        let order = dependencies_first(graph);
        let mut earliest = vec![Duration::ZERO; graph.len()];
        let length = finish_earliest(graph, &order, &durations, &mut earliest);
        let mut latest = vec![length; graph.len()];
        for &action in order.iter().rev() {
            let start = latest[action.index()].saturating_sub(durations[action.index()]);
            for &dependency in graph.dependencies(action) {
                let finish = &mut latest[dependency.index()];
                *finish = (*finish).min(start);
            }
        }
        let mut finishes = vec![None; graph.len()];
        for &action in &order {
            let i = action.index();
            finishes[i] = Some(Finish {
                earliest: earliest[i],
                latest: latest[i],
            });
        }
        Schedule {
            graph,
            order,
            durations,
            finishes,
            length,
        }
    }

    /// When the last action finishes.
    pub fn length(&self) -> Duration {
        self.length
    }

    pub fn duration(&self, action: ActionIndex) -> Duration {
        self.durations[action.index()]
    }

    /// When `action` finishes; `None` where it waits on a dependency cycle.
    pub fn finish(&self, action: ActionIndex) -> Option<Finish> {
        self.finishes[action.index()]
    }

    /// The actions that wait on a dependency cycle, and so are left out.
    pub fn cyclic(&self) -> usize {
        self.graph.len() - self.order.len()
    }

    /// How much the build shortens by when some actions take no time.
    pub fn ceilings(&self) -> Ceilings<'_, 'g> {
        Ceilings {
            schedule: self,
            durations: self.durations.clone(),
            earliest: vec![Duration::ZERO; self.graph.len()],
        }
    }
}

/// See [`Schedule::ceilings`].
pub struct Ceilings<'s, 'g> {
    schedule: &'s Schedule<'g>,
    durations: Vec<Duration>,
    earliest: Vec<Duration>,
}

impl Ceilings<'_, '_> {
    /// How much shorter the build is when `zeroed` take no time: the most
    /// that shortening them by any amount can save, as other chains of
    /// actions then bound the build's length.
    pub fn of(&mut self, zeroed: &[ActionIndex]) -> Duration {
        let schedule = self.schedule;
        for &action in zeroed {
            self.durations[action.index()] = Duration::ZERO;
        }
        let length = finish_earliest(
            schedule.graph,
            &schedule.order,
            &self.durations,
            &mut self.earliest,
        );
        for &action in zeroed {
            self.durations[action.index()] = schedule.durations[action.index()];
        }
        schedule.length.saturating_sub(length)
    }
}

/// Fills `earliest` with when each of `order` finishes at the earliest, and
/// returns when the last does.
fn finish_earliest(
    graph: &ActionGraph,
    order: &[ActionIndex],
    durations: &[Duration],
    earliest: &mut [Duration],
) -> Duration {
    let mut length = Duration::ZERO;
    for &action in order {
        let ready = graph
            .dependencies(action)
            .iter()
            .map(|dependency| earliest[dependency.index()])
            .max()
            .unwrap_or_default();
        let finish = ready.saturating_add(durations[action.index()]);
        earliest[action.index()] = finish;
        length = length.max(finish);
    }
    length
}

/// The actions of `graph` each after its dependencies (Kahn's algorithm),
/// leaving out those that wait on a dependency cycle.
fn dependencies_first(graph: &ActionGraph) -> Vec<ActionIndex> {
    let mut waiting_on: Vec<usize> = graph
        .actions()
        .map(|action| graph.dependencies(action).len())
        .collect();
    let mut order: Vec<ActionIndex> = graph
        .actions()
        .filter(|action| waiting_on[action.index()] == 0)
        .collect();
    order.reserve(graph.len() - order.len());
    let mut next = 0;
    while let Some(&action) = order.get(next) {
        next += 1;
        for &dependent in graph.dependents(action) {
            let waiting = &mut waiting_on[dependent.index()];
            *waiting -= 1;
            if *waiting == 0 {
                order.push(dependent);
            }
        }
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    /// A graph of actions lasting `durations`, action `i` depending on each
    /// `j` that `dependencies` pairs it with as `(i, j)`.
    fn graph(durations: &[u64], dependencies: &[(usize, usize)]) -> (ActionGraph, Vec<Duration>) {
        let graph = ActionGraph::new(vec![None; durations.len()], dependencies.to_vec());
        (graph, durations.iter().copied().map(ms).collect())
    }

    /// The action at position `i` of `graph`.
    fn action(graph: &ActionGraph, i: usize) -> ActionIndex {
        graph.actions().nth(i).expect("an action at the position")
    }

    #[test]
    fn a_diamond_finishes_with_its_longer_side_and_the_shorter_has_slack() {
        // 0 feeds 1 and 2, which both feed 3.
        let (graph, durations) = graph(&[10, 30, 20, 5], &[(1, 0), (2, 0), (3, 1), (3, 2)]);
        let schedule = Schedule::new(&graph, |a| durations[a.index()]);
        assert_eq!(schedule.length(), ms(45));
        let finish = |i| schedule.finish(action(&graph, i)).expect("acyclic");
        assert_eq!(
            finish(1),
            Finish {
                earliest: ms(40),
                latest: ms(40)
            }
        );
        assert_eq!(finish(2).slack(), ms(10));
        assert_eq!(finish(0).slack(), Duration::ZERO);
        assert_eq!(finish(3).slack(), Duration::ZERO);
        assert_eq!(schedule.cyclic(), 0);
    }

    #[test]
    fn taking_out_a_step_saves_only_what_a_parallel_chain_leaves() {
        // Two chains into 4: 0 → 1 (10 + 50) and 2 → 3 (20 + 25).
        let (graph, durations) = graph(&[10, 50, 20, 25, 5], &[(1, 0), (3, 2), (4, 1), (4, 3)]);
        let schedule = Schedule::new(&graph, |a| durations[a.index()]);
        assert_eq!(schedule.length(), ms(65));
        let mut ceilings = schedule.ceilings();
        assert_eq!(
            ceilings.of(&[action(&graph, 1)]),
            ms(15),
            "the other chain takes over"
        );
        assert_eq!(
            ceilings.of(&[action(&graph, 0)]),
            ms(10),
            "within the other chain's slack"
        );
        assert_eq!(
            ceilings.of(&[action(&graph, 3)]),
            Duration::ZERO,
            "off the longest chain"
        );
        assert_eq!(ceilings.of(&[action(&graph, 4)]), ms(5));
        assert_eq!(ceilings.of(&[action(&graph, 1), action(&graph, 3)]), ms(40));
        assert_eq!(
            ceilings.of(&[action(&graph, 1)]),
            ms(15),
            "a set taken out before leaves no mark"
        );
    }

    #[test]
    fn an_action_zeroed_twice_counts_once_and_is_restored() {
        let (graph, durations) = graph(&[10, 50, 20, 25, 5], &[(1, 0), (3, 2), (4, 1), (4, 3)]);
        let schedule = Schedule::new(&graph, |a| durations[a.index()]);
        let mut ceilings = schedule.ceilings();
        let once = ceilings.of(&[action(&graph, 1)]);
        assert_eq!(ceilings.of(&[action(&graph, 1), action(&graph, 1)]), once);
        assert_eq!(ceilings.of(&[]), Duration::ZERO, "every duration restored");
    }

    #[test]
    fn actions_of_no_time_take_none_and_disconnected_ones_stand_alone() {
        let (graph, durations) = graph(&[0, 40, 0, 15], &[(1, 0), (2, 1)]);
        let schedule = Schedule::new(&graph, |a| durations[a.index()]);
        assert_eq!(schedule.length(), ms(40));
        let finish = |i| schedule.finish(action(&graph, i)).expect("acyclic");
        assert_eq!(
            finish(0),
            Finish {
                earliest: Duration::ZERO,
                latest: Duration::ZERO
            }
        );
        assert_eq!(
            finish(2),
            Finish {
                earliest: ms(40),
                latest: ms(40)
            }
        );
        assert_eq!(
            finish(3),
            Finish {
                earliest: ms(15),
                latest: ms(40)
            }
        );
        assert_eq!(finish(3).slack(), ms(25));
        assert_eq!(schedule.ceilings().of(&[action(&graph, 1)]), ms(25));
    }

    #[test]
    fn actions_waiting_on_a_cycle_are_left_out_of_the_schedule() {
        // 1 and 2 wait on each other, 3 waits on 2; 0 stands apart.
        let (graph, durations) = graph(&[7, 10, 10, 10], &[(1, 2), (2, 1), (3, 2)]);
        let schedule = Schedule::new(&graph, |a| durations[a.index()]);
        assert_eq!(schedule.length(), ms(7));
        assert_eq!(schedule.cyclic(), 3);
        for i in 1..=3 {
            assert_eq!(schedule.finish(action(&graph, i)), None);
        }
        assert_eq!(
            schedule.finish(action(&graph, 0)).map(Finish::slack),
            Some(Duration::ZERO)
        );
        assert_eq!(schedule.ceilings().of(&[action(&graph, 2)]), Duration::ZERO);
    }

    #[test]
    fn an_empty_graph_takes_no_time() {
        let graph = ActionGraph::default();
        let schedule = Schedule::new(&graph, |_| ms(1));
        assert_eq!(schedule.length(), Duration::ZERO);
        assert_eq!(schedule.ceilings().of(&[]), Duration::ZERO);
    }
}
