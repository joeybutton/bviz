use crate::Palette;
use crate::colour::Colouring;
use crate::derive::{Drawn, ViewKey};
use crate::duration::lasted;
use crate::paint::{BADGE_FLOOR, Fills, STATUS_FLOOR, STATUS_RIM, paint_diamond, paint_status};
use crate::playback::{NodeState, NodeStates, States};
use crate::status::{Status, Statuses};
use crate::timing::{Extent, NodeTimes, Ran};
use bviz_graph_layout::transition::Transition;
use bviz_graph_layout::{EdgeKind, Kind, LaidOut};
use bviz_invocation::{Revision, Runner};
use bviz_ui_components::overlay_canvas;
use gpui::{Div, Rgba, div, prelude::*, px};

/// Width of the line shown beside a kind's name.
const SWATCH_WIDTH: f32 = 16.;

/// Height of a box fill shown beside what the fill says.
const FILL_HEIGHT: f32 = 10.;

/// Width of the time ramp, wide enough for its steps to read as one
/// gradient, and of each fill shown beside it so their names line up.
const RAMP_WIDTH: f32 = 48.;

/// Steps the time ramp is shown in, each a fill a box can wear.
const RAMP_STEPS: usize = 12;

/// Dash and gap of the line shown for a bridge. A swatch is too short to
/// reproduce the lengths a route is dashed at, so it shows the pattern at a size
/// that reads as dashed over the width it has.
const SWATCH_DASH: f32 = 4.;
const SWATCH_GAP: f32 = 2.;

/// The line drawn beside a kind's name, as a route of that kind is drawn: solid
/// for a dependency the graph states, dashed for a bridge standing in for a path
/// through boxes the drawing does not hold.
fn swatch(kind: EdgeKind, palette: &Palette) -> Div {
    let color = palette.edge_color(kind, false);
    let line = |width: f32| div().w(px(width)).h(px(2.)).bg(color);
    match kind {
        EdgeKind::Transitive => {
            let dashes = ((SWATCH_WIDTH + SWATCH_GAP) / (SWATCH_DASH + SWATCH_GAP)) as usize;
            div()
                .flex()
                .items_center()
                .gap(px(SWATCH_GAP))
                .children((0..dashes).map(|_| line(SWATCH_DASH)))
        }
        _ => line(SWATCH_WIDTH),
    }
}

/// The badge marking `transition`, as a route wears it at its smallest, centred
/// where a line would run.
fn transition_swatch(transition: Transition, palette: &Palette) -> Div {
    let color = palette.transition_color(transition);
    div()
        .relative()
        .w(px(SWATCH_WIDTH))
        .h(px(2. * BADGE_FLOOR))
        .child(overlay_canvas(move |bounds, window, _| {
            paint_diamond(bounds.center(), BADGE_FLOOR, color, window)
        }))
}

/// The badge of `status`, as a box wears it at its smallest.
pub(crate) fn status_swatch(status: Status, palette: &Palette) -> Div {
    let palette = *palette;
    div()
        .relative()
        .w(px(SWATCH_WIDTH))
        .h(px(2. * (STATUS_FLOOR + STATUS_RIM)))
        .child(overlay_canvas(move |bounds, window, _| {
            paint_status(bounds.center(), STATUS_FLOOR, status, 1.0, &palette, window)
        }))
}

/// A box's fill, as a box of `width` wears it.
fn fill_swatch(fill: Rgba, width: f32, palette: &Palette) -> Div {
    div()
        .flex_none()
        .w(px(width))
        .h(px(FILL_HEIGHT))
        .rounded_sm()
        .border_1()
        .border_color(palette.border)
        .bg(fill)
}

/// The fill a box wears whose actions ran as `ran` says.
pub(crate) fn ran_swatch(ran: Ran, palette: &Palette) -> Div {
    fill_swatch(palette.ran_fill(ran), SWATCH_WIDTH, palette)
}

/// The fills a box's time is read off, from the least its kind holds to the
/// most.
fn ramp_swatch(palette: &Palette) -> Div {
    let last = (RAMP_STEPS - 1) as f32;
    fill_swatch(palette.background, RAMP_WIDTH, palette)
        .flex()
        .overflow_hidden()
        .children((0..RAMP_STEPS).map(|step| {
            let fill = palette.time_fill(step as f32 / last);
            div().flex_1().h_full().bg(fill)
        }))
}

/// How the legend names the boxes of `kind`.
fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Package => "packages",
        Kind::Target => "targets",
        Kind::ConfiguredTarget => "configured targets",
        Kind::Action => "actions",
        Kind::Artifact => "generated files",
        Kind::Source => "source files",
        Kind::TargetGroup => "package and environment groups",
        Kind::MergeGroup => "merged file groups",
    }
}

/// How a view's boxes are filled, and what they are filled from. `timed` and
/// `attempted` are the revisions of the session's timings and attempts
/// `times` and `states` were read from.
#[derive(Clone, Copy)]
pub(crate) struct Coloured<'a> {
    pub(crate) colouring: Colouring,
    pub(crate) times: Option<&'a NodeTimes>,
    pub(crate) timed: Revision,
    pub(crate) states: Option<&'a NodeStates>,
    pub(crate) attempted: Revision,
}

/// What the fills of a view's boxes say beyond each box's kind.
#[derive(Clone, Debug, PartialEq)]
enum Reading {
    /// Nothing: they are filled by kind.
    Kinds,
    /// The time of each box, each kind of box placed on a scale of its own.
    Times {
        /// The scale of each kind some box of the view holds time of, in
        /// enum order.
        scales: Vec<(Kind, Extent)>,
        /// Whether some box of the view wears the untimed fill.
        untimed: bool,
    },
    /// Where each box's actions ran.
    Runners {
        /// Each reading of runners some box of the view wears, in the order of
        /// [`Runner::ALL`] and the mix last.
        ran: Vec<Ran>,
        /// Whether some box of the view wears the untimed fill.
        untimed: bool,
    },
    /// Where the execution attempts landing on each box stand at the
    /// playback's time.
    States {
        /// Each state some box of the view may stand at over the playback, in
        /// the order of [`NodeState::ALL`].
        reached: Vec<NodeState>,
        /// Whether some box of the view wears the untimed fill.
        untimed: bool,
    },
}

impl Reading {
    /// What the [fills](Fills::of) `coloured` gives the boxes of `g` say.
    fn of(g: &LaidOut, coloured: Coloured) -> Self {
        let untimed_leaf = |times: &NodeTimes| {
            g.boxes
                .iter()
                .any(|b| !b.compound && times.get(b.id).is_none())
        };
        match Fills::of(coloured.colouring, coloured.times, coloured.states) {
            Fills::Kind => Reading::Kinds,
            Fills::Playback(states) => {
                let reached = g.boxes.iter().fold(States::default(), |reached, b| {
                    reached.union(states.reached(b.id))
                });
                Reading::States {
                    reached: NodeState::ALL
                        .into_iter()
                        .filter(|&state| reached.contains(state))
                        .collect(),
                    untimed: g
                        .boxes
                        .iter()
                        .any(|b| !b.compound && states.reached(b.id) == States::default()),
                }
            }
            Fills::Time(times) => {
                let mut kinds: Vec<Kind> = g
                    .boxes
                    .iter()
                    .filter(|b| times.get(b.id).is_some())
                    .map(|b| b.kind)
                    .collect();
                kinds.sort_unstable();
                kinds.dedup();
                let scales = kinds
                    .into_iter()
                    .filter_map(|kind| Some((kind, times.extent(kind)?)))
                    .collect();
                Reading::Times {
                    scales,
                    untimed: untimed_leaf(times),
                }
            }
            Fills::Runner(times) => {
                let worn: Vec<Ran> = g
                    .boxes
                    .iter()
                    .filter_map(|b| times.get(b.id))
                    .map(|timed| Ran::of(&timed.held))
                    .collect();
                let ran = Runner::ALL
                    .map(Ran::On)
                    .into_iter()
                    .chain([Ran::Mixed])
                    .filter(|ran| worn.contains(ran))
                    .collect();
                Reading::Runners {
                    ran,
                    untimed: untimed_leaf(times),
                }
            }
        }
    }
}

/// The edge kinds, transitions and statuses present in a view, and what its
/// fills say, found once per view so that a frame drawn over an unchanged view
/// does not scan it again.
#[derive(Default)]
pub(crate) struct Legend {
    found: Option<Found>,
}

/// What is present in the view `view` identifies, each in enum order.
struct Found {
    view: ViewKey,
    /// Which of a session's outcomes the statuses found were resolved from.
    resolved: Revision,
    /// Which of a session's timings the reading was found from.
    timed: Revision,
    /// Which of a session's attempts the reading was found from.
    attempted: Revision,
    colouring: Colouring,
    kinds: Vec<EdgeKind>,
    transitions: Vec<Transition>,
    standing: Vec<Status>,
    reading: Reading,
}

impl Legend {
    /// Lists the edge kinds `drawn` draws at `zoom`, the transitions its
    /// connectors cross, the statuses `statuses` gives its boxes and what
    /// their fills say as `coloured` fills them, so it explains only what is
    /// present in this view; the caller places it. `None` when nothing is
    /// drawn, or the view is yet to be derived.
    ///
    /// `resolved` says which of a session's outcomes `statuses` were resolved
    /// from. Outcomes and timings land without the view changing, so what is
    /// found stands for as long as the view, `resolved` and `coloured` do.
    pub(crate) fn card(
        &mut self,
        drawn: &Drawn,
        zoom: f32,
        statuses: &Statuses,
        resolved: Revision,
        coloured: Coloured,
        palette: &Palette,
    ) -> Option<Div> {
        let found = self.found(drawn, zoom, statuses, resolved, coloured)?;
        card(found, coloured.times, palette)
    }

    fn found(
        &mut self,
        drawn: &Drawn,
        zoom: f32,
        statuses: &Statuses,
        resolved: Revision,
        coloured: Coloured,
    ) -> Option<&Found> {
        let view = drawn.view_key(zoom);
        let found = match self.found.take() {
            Some(found)
                if found.view == view
                    && found.resolved == resolved
                    && found.timed == coloured.timed
                    && found.attempted == coloured.attempted
                    && found.colouring == coloured.colouring =>
            {
                found
            }
            _ => {
                let g = drawn.view(zoom)?;
                Found {
                    view,
                    resolved,
                    timed: coloured.timed,
                    attempted: coloured.attempted,
                    colouring: coloured.colouring,
                    kinds: present(g),
                    transitions: crossed(g),
                    standing: standing(g, statuses),
                    reading: Reading::of(g, coloured),
                }
            }
        };
        Some(self.found.insert(found))
    }
}

/// The statuses some box of `g` stands at, whether or not a frame finds the
/// ground to paint that box's badge on.
fn standing(g: &LaidOut, statuses: &Statuses) -> Vec<Status> {
    Status::ALL
        .into_iter()
        .filter(|&status| g.boxes.iter().any(|b| statuses.get(b.id) == Some(status)))
        .collect()
}

fn present(g: &LaidOut) -> Vec<EdgeKind> {
    [
        EdgeKind::Query,
        EdgeKind::Cquery,
        EdgeKind::Aquery,
        EdgeKind::Transitive,
    ]
    .into_iter()
    .filter(|k| g.connectors.iter().any(|c| c.kinds.contains(k)))
    .collect()
}

/// The transitions some connector of `g` crosses.
fn crossed(g: &LaidOut) -> Vec<Transition> {
    Transition::ALL
        .into_iter()
        .filter(|&kind| g.connectors.iter().any(|c| c.transition == Some(kind)))
        .collect()
}

/// A row of the legend: `swatch` beside what it stands for.
fn row(swatch: Div, name: impl IntoElement, palette: &Palette) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(swatch)
        .child(div().text_xs().text_color(palette.label).child(name))
}

/// A line of the legend saying how the rows below it, or above it, are to be
/// read.
fn note(text: String, palette: &Palette) -> Div {
    div()
        .text_xs()
        .text_color(palette.colors.text_muted)
        .child(text)
}

/// The rows explaining what the fills read as `reading` says, and how much of
/// the profile `times` places; empty for fills by kind.
fn reading(reading: &Reading, times: Option<&NodeTimes>, palette: &Palette) -> Vec<Div> {
    let untimed_row = |width, said: &'static str| {
        let fill = palette.untimed_fill(Kind::Action, false);
        row(fill_swatch(fill, width, palette), said, palette)
    };
    let mut rows = match reading {
        Reading::Kinds => return Vec::new(),
        Reading::Times { scales, untimed } => {
            let mut rows = Vec::with_capacity(scales.len() + 2);
            rows.push(note(
                "action time, on a log scale per kind".to_owned(),
                palette,
            ));
            rows.extend(scales.iter().map(|&(kind, extent)| {
                let (least, most) = (lasted(extent.least()), lasted(extent.most()));
                let said = format!("{}: {least} to {most}", kind_name(kind));
                row(ramp_swatch(palette), said, palette)
            }));
            rows.extend(untimed.then(|| untimed_row(RAMP_WIDTH, "no action time")));
            rows
        }
        Reading::Runners { ran, untimed } => {
            let mut rows = Vec::with_capacity(ran.len() + 2);
            rows.extend(
                ran.iter()
                    .map(|&ran| row(ran_swatch(ran, palette), ran.label(), palette)),
            );
            rows.extend(untimed.then(|| untimed_row(SWATCH_WIDTH, "no action time")));
            rows
        }
        Reading::States { reached, untimed } => {
            let mut rows = Vec::with_capacity(reached.len() + 2);
            rows.extend(reached.iter().map(|&state| {
                let fill = fill_swatch(palette.state_fill(state), SWATCH_WIDTH, palette);
                row(fill, state.label(), palette)
            }));
            rows.extend(untimed.then(|| untimed_row(SWATCH_WIDTH, "no record")));
            rows
        }
    };
    rows.extend(times.map(|times| note(times.coverage().line(), palette)));
    rows
}

fn card(found: &Found, times: Option<&NodeTimes>, palette: &Palette) -> Option<Div> {
    let Found {
        kinds,
        transitions,
        standing,
        ..
    } = found;
    let reading = reading(&found.reading, times, palette);
    let present =
        !kinds.is_empty() || !transitions.is_empty() || !standing.is_empty() || !reading.is_empty();
    present.then(|| {
        div()
            .block_mouse_except_scroll()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_md()
            .bg(palette.container)
            .children(
                kinds
                    .iter()
                    .map(|&k| row(swatch(k, palette), k.label(), palette)),
            )
            .children(
                transitions
                    .iter()
                    .map(|&t| row(transition_swatch(t, palette), t.label(), palette)),
            )
            .children(
                standing
                    .iter()
                    .map(|&s| row(status_swatch(s, palette), s.label(), palette)),
            )
            .children(reading)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::tests::attempt;
    use crate::playback::{Ended, Running, Stage};
    use crate::status::Nodes;
    use crate::timing::landings;
    use bviz_graph_layout::graph::Node;
    use bviz_graph_layout::reduce::transitive_reduction;
    use bviz_graph_layout::{Connector, Graph, Id, Kind, Names, map_layout};
    use bviz_invocation::{
        Attempts, BuildOutcome, ConfiguredTarget, Outcomes, Phase, Profiled, Session, Spawns,
        Timings, Venue,
    };
    use std::sync::Arc;
    use std::time::Duration;

    /// Boxes filled by kind, over a graph no profile times.
    fn uncoloured() -> Coloured<'static> {
        Coloured {
            colouring: Colouring::Kind,
            times: None,
            timed: Revision::default(),
            states: None,
            attempted: Revision::default(),
        }
    }

    /// A zoom far enough out that a map draws its coarsest level.
    const COARSEST: f32 = 0.0;
    /// A zoom far enough in that a map draws its finest level.
    const FINEST: f32 = f32::MAX;

    /// The edge kinds `legend` lists for `drawn` at `zoom`, no box of which the
    /// build reports on.
    fn kinds<'a>(legend: &'a mut Legend, drawn: &Drawn, zoom: f32) -> Option<&'a [EdgeKind]> {
        let unreported = Revision::default();
        Some(
            &legend
                .found(drawn, zoom, &Statuses::default(), unreported, uncoloured())?
                .kinds,
        )
    }

    /// A layered drawing numbered `drawing` of the boxes `Id(0)` and `Id(1)`.
    fn boxes(drawing: u64) -> Drawn {
        let leaf = |id: u32| bviz_graph_layout::Box {
            id: Id(id),
            label: String::new(),
            kind: Kind::Target,
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
            compound: false,
            group: false,
        };
        Drawn::Layered {
            view: Arc::new(LaidOut {
                boxes: vec![leaf(0), leaf(1)],
                ..LaidOut::default()
            }),
            drawing,
        }
    }

    /// What a build reporting `first` of the node `Id(0)` and `second` of
    /// `Id(1)` gives the two, `Id(2)` being a node it also reports as built.
    fn reported(first: BuildOutcome, second: Option<BuildOutcome>) -> Statuses {
        let labels = ["//app:first", "//app:second", "//app:elsewhere"];
        let mut graph = Graph::new();
        let mut names = Names::default();
        for label in labels {
            let id = names.intern(&format!("target:{label}"));
            graph.add_node(Node::new(id, label, Kind::Target, None));
        }
        let mut outcomes = Outcomes::default();
        let outcome = [Some(first), second, Some(BuildOutcome::Built)];
        for (label, outcome) in labels.into_iter().zip(outcome) {
            if let Some(outcome) = outcome {
                let target = ConfiguredTarget::new(label.to_owned(), None);
                outcomes.report_build(target, outcome);
            }
        }
        let nodes = Nodes::new(&graph, &names, &outcomes);
        Statuses::resolve(&nodes, &graph)
    }

    fn standing_in(legend: &mut Legend, drawn: &Drawn, statuses: &Statuses) -> Vec<Status> {
        let found = legend.found(drawn, 1.0, statuses, Revision::default(), uncoloured());
        found.expect("a derived view").standing.clone()
    }

    #[test]
    fn the_statuses_the_boxes_of_a_view_stand_at_are_listed_from_the_least_severe() {
        let mut legend = Legend::default();
        let drawn = boxes(1);
        let failed = BuildOutcome::Failed { causes: Vec::new() };

        let statuses = reported(failed, Some(BuildOutcome::Skipped));
        assert_eq!(
            standing_in(&mut legend, &drawn, &statuses),
            [Status::Skipped, Status::RootCause],
            "a status only a box outside the view stands at is listed"
        );
        assert_eq!(
            standing_in(&mut legend, &boxes(2), &Statuses::default()),
            []
        );
    }

    /// A build's outcomes land without the view changing, so the view's key
    /// alone does not say the legend is still right.
    #[test]
    fn a_view_is_scanned_again_once_its_statuses_are_resolved_from_other_outcomes() {
        let mut session = Session::default();
        let unreported = session.outcomes_revision();
        session.report_outcomes(Outcomes::default());
        let first = session.outcomes_revision();
        session.report_outcomes(Outcomes::default());
        let second = session.outcomes_revision();
        let mut legend = Legend::default();
        let drawn = boxes(1);
        let mut standing = |statuses: &Statuses, resolved| {
            let found = legend.found(&drawn, 1.0, statuses, resolved, uncoloured());
            found.expect("a derived view").standing.clone()
        };
        assert_eq!(standing(&Statuses::default(), unreported), []);

        let landed = reported(BuildOutcome::NotBuilt, None);
        assert_eq!(standing(&landed, first), [Status::Unfinished]);
        let changed = reported(BuildOutcome::Built, None);
        assert_eq!(
            standing(&changed, first),
            [Status::Unfinished],
            "a view under an unchanged key and unchanged outcomes was scanned again"
        );
        assert_eq!(standing(&changed, second), [Status::Succeeded]);
    }

    /// A package holding a target that holds a fast remotely executed action
    /// and a slow remote cache hit, beside a target no action lands on, laid
    /// out each as a box of one layered drawing; with the times its profile
    /// places on them and the states their attempts stand at.
    fn timed() -> (Drawn, NodeTimes, NodeStates) {
        let mut graph = Graph::new();
        let mut names = Names::default();
        let mut add = |name: &str, kind, parent| {
            let id = names.intern(name);
            graph.add_node(Node::new(id, name, kind, parent));
            id
        };
        let package = add("pkg:app", Kind::Package, None);
        let target = add("target://app:bin", Kind::Target, Some(package));
        let fast = add("action:fast", Kind::Action, Some(target));
        let slow = add("action:slow", Kind::Action, Some(target));
        let idle = add("target://app:idle", Kind::Target, Some(package));
        let mut timings = Timings::default();
        for (primary_output, millis, runner) in [
            ("fast", 10, Runner::RemoteExecution),
            ("slow", 1_000, Runner::RemoteCacheHit),
        ] {
            timings.record(Profiled {
                primary_output: Some(primary_output),
                owner: None,
                wall: Duration::from_millis(millis),
                spawns: Spawns::on(runner, Duration::from_millis(millis)),
            });
        }
        let landings = landings(&graph, &names, &timings);
        let times = NodeTimes::resolve(&landings, &graph, &timings).expect("timed");
        let mut attempts = Attempts::default();
        for (primary_output, millis, phase) in [
            ("fast", 10, Phase::Running(Venue::Remote)),
            ("slow", 1_000, Phase::CheckingCache),
        ] {
            let tried = attempt(0, millis * 1_000, &[(phase, 1)]);
            attempts.record(Some(primary_output), None, tried);
        }
        let states = NodeStates::resolve(&landings, &graph, &attempts).expect("landed");
        let laid = |id: Id, kind, compound| bviz_graph_layout::Box {
            id,
            label: String::new(),
            kind,
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
            compound,
            group: false,
        };
        let view = LaidOut {
            boxes: vec![
                laid(package, Kind::Package, true),
                laid(target, Kind::Target, true),
                laid(fast, Kind::Action, false),
                laid(slow, Kind::Action, false),
                laid(idle, Kind::Target, false),
            ],
            ..LaidOut::default()
        };
        let drawn = Drawn::Layered {
            view: Arc::new(view),
            drawing: 1,
        };
        (drawn, times, states)
    }

    fn reading_of(
        legend: &mut Legend,
        drawn: &Drawn,
        colouring: Colouring,
        times: &NodeTimes,
        timed: Revision,
    ) -> Reading {
        let coloured = Coloured {
            colouring,
            times: Some(times),
            timed,
            states: None,
            attempted: Revision::default(),
        };
        let found = legend.found(
            drawn,
            1.0,
            &Statuses::default(),
            Revision::default(),
            coloured,
        );
        found.expect("a derived view").reading.clone()
    }

    #[test]
    fn the_fills_of_a_view_are_explained_as_its_colouring_reads_them() {
        let (drawn, times, _) = timed();
        let mut legend = Legend::default();
        let timed = Revision::default();
        let extent = |kind| times.extent(kind).expect("a timed kind");

        assert_eq!(
            reading_of(&mut legend, &drawn, Colouring::Time, &times, timed),
            Reading::Times {
                scales: vec![
                    (Kind::Package, extent(Kind::Package)),
                    (Kind::Target, extent(Kind::Target)),
                    (Kind::Action, extent(Kind::Action)),
                ],
                untimed: true,
            }
        );
        assert_eq!(
            extent(Kind::Action).least(),
            Duration::from_millis(10),
            "each kind is read on its own scale"
        );
        assert_eq!(
            reading_of(&mut legend, &drawn, Colouring::Runner, &times, timed),
            Reading::Runners {
                ran: vec![
                    Ran::On(Runner::RemoteExecution),
                    Ran::On(Runner::RemoteCacheHit)
                ],
                untimed: true,
            },
            "a box that ran mostly as a cache hit reads as one"
        );
        assert_eq!(
            reading_of(&mut legend, &drawn, Colouring::Kind, &times, timed),
            Reading::Kinds
        );
    }

    /// A profile's timings land without the view changing, so the view's key
    /// alone does not say the legend is still right.
    #[test]
    fn a_view_is_scanned_again_once_its_times_are_read_from_other_timings() {
        let (drawn, times, _) = timed();
        let mut session = Session::default();
        let untimed = session.timings_revision();
        session.report_timings(Timings::default());
        let timed = session.timings_revision();
        let mut legend = Legend::default();
        let stale = Coloured {
            colouring: Colouring::Runner,
            times: None,
            timed: untimed,
            states: None,
            attempted: Revision::default(),
        };
        let found = legend.found(
            &drawn,
            1.0,
            &Statuses::default(),
            Revision::default(),
            stale,
        );
        assert_eq!(found.map(|found| &found.reading), Some(&Reading::Kinds));

        let read = reading_of(&mut legend, &drawn, Colouring::Runner, &times, timed);
        assert!(matches!(read, Reading::Runners { .. }), "{read:?}");
    }

    /// What the legend reads off a view coloured by playback, the states its
    /// nodes stand at read from the session's attempts at `attempted`.
    fn played(
        legend: &mut Legend,
        drawn: &Drawn,
        states: Option<&NodeStates>,
        attempted: Revision,
    ) -> Reading {
        let coloured = Coloured {
            colouring: Colouring::Playback,
            times: None,
            timed: Revision::default(),
            states,
            attempted,
        };
        let found = legend.found(
            drawn,
            1.0,
            &Statuses::default(),
            Revision::default(),
            coloured,
        );
        found.expect("a derived view").reading.clone()
    }

    #[test]
    fn a_view_coloured_by_playback_lists_every_state_its_boxes_may_stand_at() {
        let (drawn, _, states) = timed();
        let mut legend = Legend::default();
        assert_eq!(
            played(&mut legend, &drawn, Some(&states), Revision::default()),
            Reading::States {
                reached: vec![
                    NodeState::Pending,
                    NodeState::In(Stage::Preparing),
                    NodeState::In(Stage::CheckingCache),
                    NodeState::In(Stage::Running(Running::Container)),
                    NodeState::In(Stage::Running(Running::Action(Venue::Remote))),
                    NodeState::Done(Ended::Executed),
                    NodeState::Done(Ended::Cached),
                ],
                untimed: true,
            }
        );
    }

    /// A profile's attempts land without the view changing, so the view's
    /// key alone does not say the legend is still right; the playback's time
    /// plays no part in it.
    #[test]
    fn a_view_coloured_by_playback_is_scanned_again_once_other_attempts_land() {
        let (drawn, _, states) = timed();
        let mut session = Session::default();
        let unattempted = session.attempts_revision();
        session.report_attempts(Attempts::default());
        let attempted = session.attempts_revision();
        let mut legend = Legend::default();
        let mut reading = |states, attempted| played(&mut legend, &drawn, states, attempted);
        assert_eq!(reading(None, unattempted), Reading::Kinds);
        assert_eq!(
            reading(Some(&states), unattempted),
            Reading::Kinds,
            "a view under an unchanged key and unchanged attempts was scanned again"
        );
        let read = reading(Some(&states), attempted);
        assert!(matches!(read, Reading::States { .. }), "{read:?}");
    }

    /// A layered drawing numbered `drawing`, of one connector carrying `kinds`.
    fn layered(drawing: u64, kinds: Vec<EdgeKind>) -> Drawn {
        crossing(drawing, kinds, None)
    }

    /// [`layered`], its connector crossing `transition`.
    fn crossing(drawing: u64, kinds: Vec<EdgeKind>, transition: Option<Transition>) -> Drawn {
        Drawn::Layered {
            view: Arc::new(LaidOut {
                connectors: vec![Connector {
                    points: Vec::new(),
                    kinds,
                    from: Id(0),
                    to: Id(1),
                    transition,
                }],
                ..LaidOut::default()
            }),
            drawing,
        }
    }

    /// A map numbered `drawing` of two packages whose actions form one chain of
    /// `kind` edges, so it has levels to zoom between, each derived.
    fn map(drawing: u64, kind: EdgeKind) -> Drawn {
        let mut base = Graph::new();
        let mut ids = (0..).map(Id);
        let mut actions = Vec::new();
        for package in ["a", "b"] {
            let package_id = ids.next().expect("an id");
            base.add_node(Node::new(package_id, package, Kind::Package, None));
            let target = ids.next().expect("an id");
            base.add_node(Node::new(target, "t", Kind::Target, Some(package_id)));
            for action in ["compile", "link"] {
                let id = ids.next().expect("an id");
                base.add_node(Node::new(id, action, Kind::Action, Some(target)));
                actions.push(id);
            }
        }
        for pair in actions.windows(2) {
            base.add_edge(pair[0], pair[1], kind);
        }
        let reduced = transitive_reduction(&base);
        let map = map_layout(Arc::new(base), Arc::new(reduced));
        for level in 0..map.level_count() {
            map.level(level);
        }
        Drawn::Map {
            map: Arc::new(map),
            drawing,
        }
    }

    #[test]
    fn a_view_is_scanned_once_under_its_key() {
        let mut legend = Legend::default();
        assert_eq!(
            kinds(&mut legend, &layered(1, vec![EdgeKind::Query]), 1.0),
            Some(&[EdgeKind::Query][..])
        );
        assert_eq!(
            kinds(&mut legend, &layered(1, vec![EdgeKind::Aquery]), 1.0),
            Some(&[EdgeKind::Query][..]),
            "a view under an unchanged key was scanned again"
        );
        assert_eq!(
            kinds(&mut legend, &layered(2, vec![EdgeKind::Aquery]), 1.0),
            Some(&[EdgeKind::Aquery][..])
        );
    }

    #[test]
    fn the_transitions_a_view_crosses_are_listed() {
        let mut legend = Legend::default();
        let transitions = |legend: &mut Legend, drawn: &Drawn| {
            legend
                .found(
                    drawn,
                    1.0,
                    &Statuses::default(),
                    Revision::default(),
                    uncoloured(),
                )
                .map(|found| found.transitions.clone())
        };
        let exec = crossing(1, vec![EdgeKind::Cquery], Some(Transition::Exec));
        assert_eq!(
            transitions(&mut legend, &exec),
            Some(vec![Transition::Exec])
        );
        let none = layered(2, vec![EdgeKind::Cquery]);
        assert_eq!(transitions(&mut legend, &none), Some(vec![]));
    }

    #[test]
    fn a_map_is_scanned_again_at_another_level() {
        let query = map(1, EdgeKind::Query);
        let aquery = map(1, EdgeKind::Aquery);
        assert_ne!(aquery.view_key(COARSEST), aquery.view_key(FINEST));
        let mut legend = Legend::default();
        let before = kinds(&mut legend, &query, COARSEST).map(<[_]>::to_vec);

        let after = kinds(&mut legend, &aquery, FINEST).map(<[_]>::to_vec);

        assert_ne!(before, after);
        assert_eq!(after, aquery.view(FINEST).map(present));
    }
}
