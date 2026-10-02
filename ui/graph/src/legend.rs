use crate::Palette;
use crate::derive::{Drawn, ViewKey};
use crate::paint::{BADGE_FLOOR, STATUS_FLOOR, STATUS_RIM, paint_diamond, paint_status};
use crate::status::{Status, Statuses};
use bviz_graph_layout::transition::Transition;
use bviz_graph_layout::{EdgeKind, LaidOut};
use bviz_invocation::Revision;
use bviz_ui_components::overlay_canvas;
use gpui::{Div, div, prelude::*, px};

/// Width of the line shown beside a kind's name.
const SWATCH_WIDTH: f32 = 16.;

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
fn status_swatch(status: Status, palette: &Palette) -> Div {
    let palette = *palette;
    div()
        .relative()
        .w(px(SWATCH_WIDTH))
        .h(px(2. * (STATUS_FLOOR + STATUS_RIM)))
        .child(overlay_canvas(move |bounds, window, _| {
            paint_status(bounds.center(), STATUS_FLOOR, status, 1.0, &palette, window)
        }))
}

/// The edge kinds, transitions and statuses present in a view, found once per
/// view so that a frame drawn over an unchanged view does not scan it again.
#[derive(Default)]
pub(crate) struct Legend {
    found: Option<Found>,
}

/// What is present in the view `view` identifies, each in enum order.
struct Found {
    view: ViewKey,
    /// Which of a session's outcomes the statuses found were resolved from.
    resolved: Revision,
    kinds: Vec<EdgeKind>,
    transitions: Vec<Transition>,
    standing: Vec<Status>,
}

impl Legend {
    /// Lists the edge kinds `drawn` draws at `zoom`, the transitions its
    /// connectors cross and the statuses `statuses` gives its boxes, so it
    /// explains only what is present in this view; the caller places it. `None`
    /// when nothing is drawn, or the view is yet to be derived.
    ///
    /// `resolved` says which of a session's outcomes `statuses` were resolved
    /// from. Outcomes land without the view changing, so the statuses found
    /// stand for as long as both the view and `resolved` do.
    pub(crate) fn card(
        &mut self,
        drawn: &Drawn,
        zoom: f32,
        statuses: &Statuses,
        resolved: Revision,
        palette: &Palette,
    ) -> Option<Div> {
        let found = self.found(drawn, zoom, statuses, resolved)?;
        card(&found.kinds, &found.transitions, &found.standing, palette)
    }

    fn found(
        &mut self,
        drawn: &Drawn,
        zoom: f32,
        statuses: &Statuses,
        resolved: Revision,
    ) -> Option<&Found> {
        let view = drawn.view_key(zoom);
        let found = match self.found.take() {
            Some(found) if found.view == view && found.resolved == resolved => found,
            _ => {
                let g = drawn.view(zoom)?;
                Found {
                    view,
                    resolved,
                    kinds: present(g),
                    transitions: crossed(g),
                    standing: standing(g, statuses),
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

fn card(
    kinds: &[EdgeKind],
    transitions: &[Transition],
    standing: &[Status],
    palette: &Palette,
) -> Option<Div> {
    let row = |swatch: Div, name: &'static str| {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(swatch)
            .child(div().text_xs().text_color(palette.label).child(name))
    };
    (!kinds.is_empty() || !transitions.is_empty() || !standing.is_empty()).then(|| {
        div()
            .block_mouse_except_scroll()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_md()
            .bg(palette.container)
            .children(kinds.iter().map(|&k| row(swatch(k, palette), k.label())))
            .children(
                transitions
                    .iter()
                    .map(|&t| row(transition_swatch(t, palette), t.label())),
            )
            .children(
                standing
                    .iter()
                    .map(|&s| row(status_swatch(s, palette), s.label())),
            )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_graph_layout::graph::Node;
    use bviz_graph_layout::reduce::transitive_reduction;
    use bviz_graph_layout::{Connector, Graph, Id, Kind, Names, map_layout};
    use bviz_invocation::{BuildOutcome, ConfiguredTarget, Outcomes, Session};
    use std::sync::Arc;

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
                .found(drawn, zoom, &Statuses::default(), unreported)?
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
        Statuses::resolve(&outcomes, &graph, &names)
    }

    fn standing_in(legend: &mut Legend, drawn: &Drawn, statuses: &Statuses) -> Vec<Status> {
        let found = legend.found(drawn, 1.0, statuses, Revision::default());
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
            let found = legend.found(&drawn, 1.0, statuses, resolved);
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
                .found(drawn, 1.0, &Statuses::default(), Revision::default())
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
