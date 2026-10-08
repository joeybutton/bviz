//! The toolbar's sources menu: which of what the graph was read from the panel
//! reads.
//!
//! Every checkmark is read off the panel's [`Sources`] each time the options are
//! stated, so nothing here records what is read.

use crate::chips::command;
use crate::{GraphPanel, derive};
use bviz_graph_layout::{Input, Sources};
use bviz_ui_components::{MultiOption, MultiSelectEvent};
use gpui::Context;

/// The inputs the menu's rows switch over `sources`, in the order they stand.
fn rows(sources: Sources) -> impl Iterator<Item = Input> {
    sources.loaded().inputs()
}

/// The options the menu draws for `sources`.
fn options(sources: Sources) -> Vec<MultiOption> {
    rows(sources)
        .map(|input| MultiOption::new(command(input).name(), sources.inputs().contains(input)))
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
    let inputs: Vec<Input> = rows(sources).collect();
    (picked.iter())
        .filter_map(|&index| inputs.get(index))
        .fold(sources, |sources, &input| {
            sources.with_input(input, on).unwrap_or(sources)
        })
}

/// Whether the menu offers a choice over `sources`, which it does only over a
/// graph read from more than one input.
fn offers_choice(sources: Sources) -> bool {
    rows(sources).nth(1).is_some()
}

/// Whether the menu offers the panel a choice.
pub(crate) fn offered(view: &GraphPanel) -> bool {
    view.derived.sources().is_some_and(offers_choice)
}

/// States the menu's rows afresh from the sources the panel reads.
pub(crate) fn restate(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let options = view.derived.sources().map_or_else(Vec::new, options);
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

    fn checked(sources: Sources) -> Vec<bool> {
        options(sources)
            .iter()
            .map(|option| option.checked)
            .collect()
    }

    fn toggle(index: usize, checked: bool) -> MultiSelectEvent {
        MultiSelectEvent::Toggled { index, checked }
    }

    #[test]
    fn the_menu_offers_each_input_read_and_checks_those_the_panel_reads() {
        let labels: Vec<String> = (options(every()).iter())
            .map(|option| option.label.to_string())
            .collect();
        assert_eq!(labels, ["query", "cquery", "aquery"]);
        assert_eq!(checked(every()), [true; 3]);

        let without_aquery = chosen(every(), &toggle(2, false));
        assert_eq!(checked(without_aquery), [true, true, false]);
        assert_eq!(chosen(without_aquery, &toggle(2, true)), every());
    }

    #[test]
    fn only_the_inputs_the_graph_was_read_from_are_offered() {
        let loaded = InputSet::of([Input::Query, Input::Aquery]).unwrap();
        let labels: Vec<String> = (options(reading(loaded)).iter())
            .map(|option| option.label.to_string())
            .collect();
        assert_eq!(labels, ["query", "aquery"]);
        assert!(offers_choice(reading(loaded)));
        assert!(!offers_choice(reading(Input::Cquery.into())));
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
        assert_eq!(checked(query), [true, false, false]);
        assert_eq!(chosen(query, &toggle(0, false)), query);

        let cleared = chosen(
            every(),
            &MultiSelectEvent::SetAll {
                rows: vec![0, 1, 2],
                checked: false,
            },
        );
        assert_eq!(checked(cleared), [false, false, true]);
    }
}
