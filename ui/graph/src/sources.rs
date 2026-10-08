//! The toolbar's sources menu: which of what the graph was read from the panel
//! reads, and which kinds of dependency it keeps.
//!
//! Every checkmark is read off the panel's [`Sources`] each time the options are
//! stated, so nothing here records what is read.

use crate::chips::command;
use crate::{GraphPanel, derive};
use bviz_graph_layout::{Axis, Capabilities, Deps, Input, Sources, Support};
use bviz_ui_components::{MultiOption, MultiSelectEvent};
use gpui::Context;

/// One row of the menu: an input to read, or a kind of dependency to keep.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Row {
    Input(Input),
    Deps(Deps),
}

impl Row {
    fn label(self) -> &'static str {
        match self {
            Row::Input(input) => command(input).name(),
            Row::Deps(Deps::Implicit) => "implicit deps",
            Row::Deps(Deps::Tool) => "tool deps",
        }
    }

    fn checked(self, sources: Sources) -> bool {
        match self {
            Row::Input(input) => sources.inputs().contains(input),
            Row::Deps(deps) => sources.keeps(deps),
        }
    }

    /// `sources` with this row switched `on`, or as they were where that
    /// cannot be.
    fn switched(self, sources: Sources, on: bool) -> Sources {
        let switched = match self {
            Row::Input(input) => sources.with_input(input, on),
            Row::Deps(deps) => sources.with_deps(deps, on),
        };
        switched.unwrap_or(sources)
    }

    /// Why this row cannot be switched over `sources`, whose graph was read
    /// as `capabilities` count, where it cannot.
    fn unavailable(self, sources: Sources, capabilities: &Capabilities) -> Option<String> {
        let Row::Deps(deps) = self else {
            return None;
        };
        if sources.tells(deps) {
            return None;
        }
        let inputs = sources.inputs();
        let unstated = |axis: Axis| match capabilities.reading(inputs).support(axis) {
            Support::Unavailable { flag } => Some(flag),
            Support::Available | Support::Partial { .. } => None,
        };
        let reads = |input: Input| inputs.contains(input);
        Some(match deps {
            Deps::Implicit => match unstated(Axis::DependencyClass) {
                Some(flag) if reads(Input::Query) || reads(Input::Cquery) => {
                    format!("The dumps read state no rule's attributes; run the query with {flag}")
                }
                _ => "Read query or cquery to leave implicit deps out".to_owned(),
            },
            Deps::Tool => match unstated(Axis::ExecConfiguration) {
                Some(flag) if reads(Input::Cquery) => {
                    format!("The cquery states no dependency's configuration; run it with {flag}")
                }
                _ => "Read cquery to leave tool deps out".to_owned(),
            },
        })
    }
}

/// The menu's rows over `sources`, in the order they stand: each input the
/// graph was read from where there is a choice of them, then each kind of
/// dependency an input read tells.
fn rows(sources: Sources) -> Vec<Row> {
    let loaded = sources.loaded();
    let inputs = (loaded.inputs().nth(1).is_some())
        .then(|| loaded.inputs().map(Row::Input))
        .into_iter()
        .flatten();
    let classified = loaded.contains(Input::Query) || loaded.contains(Input::Cquery);
    let implicit = classified.then_some(Row::Deps(Deps::Implicit));
    let tool = loaded
        .contains(Input::Cquery)
        .then_some(Row::Deps(Deps::Tool));
    inputs.chain(implicit).chain(tool).collect()
}

/// The options the menu draws for `sources`, over a graph `capabilities`
/// counts.
fn options(sources: Sources, capabilities: &Capabilities) -> Vec<MultiOption> {
    (rows(sources).into_iter())
        .map(|row| {
            let option = MultiOption::new(row.label(), row.checked(sources));
            match row.unavailable(sources, capabilities) {
                Some(why) => option.unavailable(why),
                None => option,
            }
        })
        .collect()
}

/// `sources` with the rows the menu's `event` names switched.
///
/// A click over a range switches every row it covers as a single click on that
/// row would, so a switch that would leave nothing read is refused as it is
/// for one row: clearing every row leaves the last input read.
fn chosen(sources: Sources, event: &MultiSelectEvent) -> Sources {
    let (picked, on) = match event {
        MultiSelectEvent::Toggled { index, checked } => (std::slice::from_ref(index), *checked),
        MultiSelectEvent::SetAll { rows, checked } => (rows.as_slice(), *checked),
    };
    let rows = rows(sources);
    (picked.iter())
        .filter_map(|&index| rows.get(index))
        .fold(sources, |sources, row| row.switched(sources, on))
}

/// Whether the menu offers the panel a choice.
pub(crate) fn offered(view: &GraphPanel) -> bool {
    view.derived
        .sources()
        .is_some_and(|sources| !rows(sources).is_empty())
}

/// States the menu's rows afresh from the sources the panel reads.
pub(crate) fn restate(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let model = view.model.read(cx);
    let capabilities = model.loaded().map(|loaded| &loaded.shared.capabilities);
    let options = match (view.derived.sources(), capabilities) {
        (Some(sources), Some(capabilities)) => options(sources, capabilities),
        _ => Vec::new(),
    };
    view.sources
        .update(cx, |control, cx| control.set_options(options, cx));
}

/// Reads the graph as the menu's click leaves the sources.
pub(crate) fn choose(
    view: &mut GraphPanel,
    event: &MultiSelectEvent,
    cx: &mut Context<GraphPanel>,
) {
    let Some(sources) = view.derived.sources() else {
        return;
    };
    derive::set_sources(view, chosen(sources, event), cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_graph_layout::InputSet;
    use bviz_graph_layout::capability::CapabilitiesBuilder;

    /// Every input of `loaded` on, each describing everything it may.
    fn reading(loaded: InputSet) -> Sources {
        Sources::reading(&CapabilitiesBuilder::default().seal(loaded))
    }

    fn every() -> Sources {
        reading(InputSet::ALL)
    }

    /// The capabilities of a graph whose every description states every axis.
    fn stating_all() -> Capabilities {
        CapabilitiesBuilder::default().seal(InputSet::ALL)
    }

    fn checked(sources: Sources) -> Vec<bool> {
        (options(sources, &stating_all()).iter())
            .map(|option| option.checked)
            .collect()
    }

    fn labels(sources: Sources) -> Vec<String> {
        (options(sources, &stating_all()).iter())
            .map(|option| option.label.to_string())
            .collect()
    }

    fn toggle(index: usize, checked: bool) -> MultiSelectEvent {
        MultiSelectEvent::Toggled { index, checked }
    }

    #[test]
    fn the_menu_offers_each_input_read_and_checks_those_the_panel_reads() {
        assert_eq!(
            labels(every()),
            ["query", "cquery", "aquery", "implicit deps", "tool deps"]
        );
        assert_eq!(checked(every()), [true; 5]);

        let without_aquery = chosen(every(), &toggle(2, false));
        assert_eq!(checked(without_aquery), [true, true, false, true, true]);
        assert_eq!(chosen(without_aquery, &toggle(2, true)), every());
    }

    #[test]
    fn only_the_inputs_the_graph_was_read_from_are_offered() {
        let loaded = InputSet::of([Input::Query, Input::Aquery]).unwrap();
        assert_eq!(
            labels(reading(loaded)),
            ["query", "aquery", "implicit deps"]
        );
        assert_eq!(
            labels(reading(Input::Cquery.into())),
            ["implicit deps", "tool deps"]
        );
        assert!(rows(reading(Input::Aquery.into())).is_empty());
    }

    #[test]
    fn the_last_input_read_stays_on() {
        let query = chosen(
            every(),
            &MultiSelectEvent::SetAll {
                rows: vec![1, 2],
                checked: false,
            },
        );
        assert_eq!(checked(query), [true, false, false, true, true]);
        assert_eq!(chosen(query, &toggle(0, false)), query);

        let cleared = chosen(
            every(),
            &MultiSelectEvent::SetAll {
                rows: vec![0, 1, 2],
                checked: false,
            },
        );
        assert_eq!(checked(cleared), [false, false, true, true, true]);
    }

    #[test]
    fn a_dependency_row_leaves_its_dependencies_out() {
        let explicit = chosen(every(), &toggle(3, false));
        assert!(!explicit.keeps(Deps::Implicit));
        let targets = chosen(every(), &toggle(4, false));
        assert!(!targets.keeps(Deps::Tool));
        assert_eq!(checked(targets), [true, true, true, true, false]);
    }

    /// A row the inputs read cannot tell says what it lacks: an input that
    /// tells the dependencies, or the flag that has it state them.
    #[test]
    fn a_dependency_row_the_inputs_read_cannot_tell_says_why() {
        let unavailable = |sources: Sources, capabilities: &Capabilities| {
            (options(sources, capabilities).into_iter())
                .map(|option| option.unavailable.map(|why| why.to_string()))
                .collect::<Vec<_>>()
        };
        let aquery = (every().with_input(Input::Query, false))
            .and_then(|sources| sources.with_input(Input::Cquery, false))
            .unwrap();
        let reasons = unavailable(aquery, &stating_all());
        assert!(reasons[..3].iter().all(Option::is_none));
        assert!(reasons[3].as_deref().unwrap().contains("query or cquery"));
        assert!(reasons[4].as_deref().unwrap().contains("cquery"));

        let mut unstated = CapabilitiesBuilder::default();
        unstated.observe(Input::Query, Axis::DependencyClass, false);
        unstated.observe(Input::Cquery, Axis::DependencyClass, false);
        unstated.observe(Input::Cquery, Axis::ExecConfiguration, false);
        let unstated = unstated.seal(InputSet::ALL);
        let reasons = unavailable(Sources::reading(&unstated), &unstated);
        assert!(
            reasons[3]
                .as_deref()
                .unwrap()
                .contains("--proto:output_rule_attrs")
        );
        assert!(
            reasons[4]
                .as_deref()
                .unwrap()
                .contains("--transitions=lite")
        );
    }

    /// With only an input that cannot tell implicit dependencies left on,
    /// their row stands checked and greyed, whatever the reader chose while
    /// one could.
    #[test]
    fn a_dependency_row_no_input_on_tells_stands_kept() {
        let mut capabilities = CapabilitiesBuilder::default();
        capabilities.observe(Input::Query, Axis::DependencyClass, false);
        capabilities.observe(Input::Cquery, Axis::DependencyClass, true);
        let capabilities = capabilities.seal(InputSet::of([Input::Query, Input::Cquery]).unwrap());
        let explicit = chosen(Sources::reading(&capabilities), &toggle(2, false));
        assert!(!explicit.keeps(Deps::Implicit));

        let query = chosen(explicit, &toggle(1, false));
        let options = options(query, &capabilities);
        assert!(options[2].checked && options[2].unavailable.is_some());
        assert_eq!(chosen(query, &toggle(1, true)), explicit);
    }
}
