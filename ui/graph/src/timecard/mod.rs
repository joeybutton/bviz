//! Where a profile's action time went, as the card over the graph lists it.

use crate::status::Landings;
use bviz_graph_layout::Id;
use bviz_invocation::{ConfiguredTarget, Spent, Timing, Timings};
use std::cmp::Reverse;
use std::ops::Range;
use std::time::Duration;

mod card;
mod words;

pub(crate) use card::{Card, card, control};

/// How many of the slowest actions the card lists: enough to scroll through
/// past the few that dominate, few enough to keep the card's own cost
/// negligible beside the graph's.
const LISTED: usize = 100;

/// Where in the graph the card shows one of the slowest actions.
#[derive(Clone, Debug, PartialEq)]
enum Shown {
    /// On the action's own node.
    Action(Id),
    /// On the node of `owner`, the configured target owning the action,
    /// which the graph holds where it holds no node for the action.
    Owner { node: Id, owner: String },
    /// Nowhere: the graph holds neither the action nor its owner.
    Off,
}

impl Shown {
    /// Where `landings` shows the action writing `primary_output`, owned by
    /// `owner` where the profile names it.
    fn of<'a>(
        landings: &Landings<'a>,
        primary_output: &str,
        owner: Option<ConfiguredTarget<&'a str>>,
    ) -> Self {
        if let Some(node) = landings.action(primary_output, None) {
            return Shown::Action(node);
        }
        let Some(owner) = owner else {
            return Shown::Off;
        };
        match landings.landing(owner).node() {
            Some(node) => Shown::Owner {
                node,
                owner: owner.label().to_owned(),
            },
            None => Shown::Off,
        }
    }

    /// The node a click on the action's row reveals.
    fn node(&self) -> Option<Id> {
        match *self {
            Shown::Action(node) | Shown::Owner { node, .. } => Some(node),
            Shown::Off => None,
        }
    }
}

/// One of the slowest actions a profile recorded.
#[derive(Clone, Debug, PartialEq)]
struct Slow {
    /// The exec path of the primary output the action writes.
    primary_output: String,
    timing: Timing,
    shown: Shown,
}

/// Where a profile's action time went.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Timecard {
    /// Every action the profile recorded, by runner.
    profiled: Spent,
    /// The slowest actions the profile names by primary output, slowest
    /// first.
    slowest: Vec<Slow>,
}

impl Timecard {
    /// Where the actions `timings` records went, `landings` finding the
    /// node each lands on.
    pub(crate) fn of<'a>(timings: &'a Timings, landings: &Landings<'a>) -> Self {
        type Listed<'a> = (&'a str, Option<ConfiguredTarget<&'a str>>, Timing);
        fn slowest_first<'a>(&(path, _, timing): &Listed<'a>) -> (Reverse<Duration>, &'a str) {
            (Reverse(timing.wall), path)
        }
        let mut actions: Vec<Listed> = timings.actions().collect();
        if actions.len() > LISTED {
            actions.select_nth_unstable_by_key(LISTED - 1, slowest_first);
            actions.truncate(LISTED);
        }
        actions.sort_unstable_by_key(slowest_first);
        let slowest = actions
            .into_iter()
            .map(|(primary_output, owner, timing)| Slow {
                primary_output: primary_output.to_owned(),
                timing,
                shown: Shown::of(landings, primary_output, owner),
            })
            .collect();
        Timecard {
            profiled: timings.spent(),
            slowest,
        }
    }

    /// Whether the profile recorded no action.
    pub(crate) fn is_empty(&self) -> bool {
        self.profiled.total().actions == 0
    }

    fn rows(&self) -> usize {
        self.slowest.len()
    }

    /// The slowest actions at `range`, as many of them as there are.
    fn listed(&self, range: Range<usize>) -> impl Iterator<Item = &Slow> {
        self.slowest.iter().skip(range.start).take(range.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timing::landings;
    use bviz_graph_layout::graph::Node;
    use bviz_graph_layout::{Graph, Kind, Names};
    use bviz_invocation::{Profiled, Runner, Share, Spawns};

    fn record(
        timings: &mut Timings,
        primary_output: Option<&str>,
        owner: Option<&str>,
        millis: u64,
        runner: Runner,
    ) {
        let wall = Duration::from_millis(millis);
        timings.record(Profiled {
            primary_output,
            owner: owner.map(|label| ConfiguredTarget::new(label, None)),
            wall,
            spawns: Spawns::on(runner, wall),
        });
    }

    #[test]
    fn the_slowest_actions_are_listed_slowest_first_where_each_is_shown() {
        let mut graph = Graph::new();
        let mut names = Names::default();
        let bin = names.intern("target://app:bin");
        graph.add_node(Node::new(bin, "bin", Kind::Target, None));
        let held = names.intern("action:out/held");
        graph.add_node(Node::new(held, "CppCompile", Kind::Action, Some(bin)));
        let mut timings = Timings::default();
        record(
            &mut timings,
            Some("out/held"),
            Some("//app:bin"),
            30,
            Runner::RemoteExecution,
        );
        record(
            &mut timings,
            Some("out/elsewhere"),
            None,
            50,
            Runner::Sandboxed,
        );
        record(
            &mut timings,
            Some("out/tied"),
            Some("//app:bin"),
            30,
            Runner::Sandboxed,
        );
        record(&mut timings, None, None, 900, Runner::Local);

        let card = Timecard::of(&timings, &landings(&graph, &names, &timings));

        let listed: Vec<(&str, u128, Shown)> = card
            .listed(0..card.rows())
            .map(|slow| {
                let millis = slow.timing.wall.as_millis();
                (slow.primary_output.as_str(), millis, slow.shown.clone())
            })
            .collect();
        let in_bin = Shown::Owner {
            node: bin,
            owner: "//app:bin".to_owned(),
        };
        assert_eq!(
            listed,
            [
                ("out/elsewhere", 50, Shown::Off),
                ("out/held", 30, Shown::Action(held)),
                ("out/tied", 30, in_bin),
            ],
            "an action named by no primary output is not listed"
        );
        assert_eq!(
            card.profiled.of(Runner::Local),
            Share {
                actions: 1,
                wall: Duration::from_millis(900)
            }
        );
        assert_eq!(card.profiled.total().actions, 4);
    }

    #[test]
    fn only_the_slowest_few_are_listed() {
        let mut timings = Timings::with_capacity(LISTED + 1);
        for millis in 0..=LISTED as u64 {
            let path = format!("out/{millis}");
            record(&mut timings, Some(&path), None, millis, Runner::Local);
        }
        let (graph, names) = (Graph::new(), Names::default());
        let card = Timecard::of(&timings, &landings(&graph, &names, &timings));

        assert_eq!(card.rows(), LISTED);
        let fastest = card.listed(LISTED - 1..LISTED).next().expect("a row");
        assert_eq!(fastest.timing.wall, Duration::from_millis(1));
    }
}
