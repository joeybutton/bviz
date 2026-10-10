//! What the loaded inputs cannot filter on, and the Bazel flag that would
//! supply it.
//!
//! The queries run outside bviz and their output arrives as files, so an axis
//! can be absent because a flag was not passed. [`Capabilities`] tells that from
//! an axis the inputs state and the build carries no values on, and only the
//! former is something a reader can act on: it is named here with the flag to
//! pass when the dump is regenerated.

use crate::column::Folding;
use crate::{GraphPanel, Palette};
use bviz_bazel_flags::Flag;
use bviz_graph_layout::{Axis, Capabilities, Support};
use bviz_ui_components::gesture::{
    Button as PointerButton, Entry, GestureRow, Gestures, Input, Place, Pointer,
};
use bviz_ui_components::{ListScroll, SCROLLBAR_THICKNESS, list_scrollbar};
use gpui::{ClickEvent, Context, Div, MouseButton, MouseDownEvent, div, prelude::*, px};

/// What the card does for a gesture in [`GESTURES`].
#[derive(Clone, Copy)]
enum MissingIntent {
    Toggle,
}

const GESTURES: Gestures<MissingIntent> = Gestures::new(&[Entry::new(
    Input::click(PointerButton::Left),
    Place::On("the unavailable filters"),
    "Show or hide missing flags",
    MissingIntent::Toggle,
)]);

/// The gestures the card answers to.
pub(crate) fn gestures() -> impl Iterator<Item = GestureRow> {
    GESTURES.rows()
}

/// Whether the card is unfolded past its one-line reading, and how far what
/// it unfolds has scrolled.
pub(crate) struct Missing {
    open: bool,
    scroll: ListScroll,
}

impl Missing {
    pub(crate) fn new() -> Self {
        Self {
            open: false,
            scroll: ListScroll::new(),
        }
    }
}

/// How a reader knows the property `axis` selects nodes by.
fn axis_label(axis: Axis) -> &'static str {
    match axis {
        Axis::Repo => "repository origins",
        Axis::Package => "packages",
        Axis::Label => "labels",
        Axis::RuleClass => "rule classes",
        Axis::RuleCategory => "rule categories",
        Axis::Configuration => "configurations",
        Axis::ExecConfiguration => "per-dependency configurations",
        Axis::Mnemonic => "action mnemonics",
        Axis::Aspect => "aspects",
        Axis::Tags => "target tags",
        Axis::Testonly => "testonly targets",
        Axis::Artifacts => "action inputs and outputs",
    }
}

/// One flag the inputs were not run with, and what passing it would let a reader
/// filter on.
#[derive(PartialEq, Eq, Debug)]
pub(crate) struct Gap {
    flag: Flag,
    axes: Vec<Axis>,
}

impl Gap {
    /// The gap as one line: what it would supply, and how to ask for it.
    fn reading(&self) -> String {
        let axes: Vec<&str> = self.axes.iter().copied().map(axis_label).collect();
        format!("{} — run the query with {}", axes.join(", "), self.flag)
    }
}

/// How many axes the gaps cover, which is what a reader loses rather than the
/// number of flags that would restore them.
fn unavailable(gaps: &[Gap]) -> String {
    match gaps.iter().map(|gap| gap.axes.len()).sum::<usize>() {
        1 => "1 filter unavailable".to_string(),
        axes => format!("{axes} filters unavailable"),
    }
}

/// The axes no description in the inputs states, gathered under the flag that
/// governs each so that one flag is asked for once.
pub(crate) fn gaps(capabilities: &Capabilities) -> Vec<Gap> {
    let mut gaps: Vec<Gap> = Vec::new();
    for (axis, support) in capabilities.axes() {
        let Support::Unavailable { flag } = support else {
            continue;
        };
        match gaps.iter_mut().find(|gap| gap.flag == flag) {
            Some(gap) => gap.axes.push(axis),
            None => gaps.push(Gap {
                flag,
                axes: vec![axis],
            }),
        }
    }
    gaps
}

fn toggle(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    view.missing.open = !view.missing.open;
    cx.notify();
}

/// What the loaded inputs cannot be filtered on, as a card the reader unfolds;
/// `None` where they support every axis, which is nothing to say.
pub(crate) fn card(
    view: &GraphPanel,
    palette: &Palette,
    cx: &Context<GraphPanel>,
) -> Option<Folding> {
    let loaded = view.model.read(cx).loaded()?;
    let gaps = gaps(&loaded.shared.capabilities);
    if gaps.is_empty() {
        return None;
    }
    let head = div().text_xs().text_color(palette.label).child(
        div()
            .id("missing-data")
            .w_full()
            .cursor_pointer()
            .on_click(cx.listener(|view, event: &ClickEvent, _, cx| {
                GESTURES.dispatch(Pointer::click(event), |intent| match intent {
                    MissingIntent::Toggle => {
                        toggle(view, cx);
                        true
                    }
                });
            }))
            .child(unavailable(&gaps)),
    );
    let readings = view.missing.open.then(|| listed(view, &gaps, palette, cx));
    Some(Folding::new(head, readings, palette))
}

/// The reading of each gap, scrollable within whatever height the card is
/// left, with a scroll bar over their right edge that they keep clear of.
fn listed(view: &GraphPanel, gaps: &[Gap], palette: &Palette, cx: &Context<GraphPanel>) -> Div {
    let readings = div()
        .id("missing-list")
        .flex()
        .flex_col()
        .gap_1()
        .w_full()
        .pr(px(SCROLLBAR_THICKNESS))
        .overflow_y_scroll()
        .track_scroll(view.missing.scroll.handle())
        .text_xs()
        .text_color(palette.label)
        .children(gaps.iter().map(Gap::reading));
    div()
        .relative()
        .flex()
        .flex_1()
        .min_h_0()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|view, e: &MouseDownEvent, _, cx| {
                view.missing.scroll.press(e.position);
                cx.notify();
            }),
        )
        .child(readings)
        .child(list_scrollbar(
            cx.entity(),
            |view: &GraphPanel| &view.missing.scroll,
            |view: &mut GraphPanel| &mut view.missing.scroll,
            palette.scroll,
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_gesture_is_reachable_and_fits_the_help() {
        assert_eq!(GESTURES.unreachable(), []);
        assert_eq!(GESTURES.overlong(), []);
    }
    use bviz_graph_layout::capability::CapabilitiesBuilder;

    /// The capabilities of inputs that stated `carried` of every axis they were
    /// read for, over `relevant` descriptions each.
    fn observed(carried: bool, relevant: usize) -> Capabilities {
        let mut capabilities = CapabilitiesBuilder::default();
        for axis in Axis::ALL {
            for _ in 0..relevant {
                capabilities.observe(axis, carried);
            }
        }
        capabilities.seal()
    }

    #[test]
    fn the_folded_reading_counts_the_axes_rather_than_the_flags() {
        assert_eq!(
            unavailable(&gaps(&observed(false, 4))),
            "5 filters unavailable"
        );
        assert_eq!(
            unavailable(&[Gap {
                flag: bviz_bazel_flags::TRANSITIONS,
                axes: vec![Axis::ExecConfiguration],
            }]),
            "1 filter unavailable"
        );
    }

    #[test]
    fn an_axis_no_description_states_is_named_with_the_flag_that_supplies_it() {
        let gaps = gaps(&observed(false, 4));

        assert_eq!(
            gaps.iter().map(Gap::reading).collect::<Vec<String>>(),
            [
                "repository origins — run the query with --consistent_labels",
                "per-dependency configurations — run the query with --transitions=lite",
                "target tags, testonly targets — run the query with --proto:output_rule_attrs=all",
                "action inputs and outputs — run the query with --include_artifacts",
            ]
        );
    }

    /// One flag supplies several axes, and is asked for once.
    #[test]
    fn axes_one_flag_governs_are_gathered_under_it() {
        assert_eq!(
            gaps(&observed(false, 1))
                .into_iter()
                .find(|gap| gap.flag == bviz_bazel_flags::OUTPUT_RULE_ATTRS)
                .map(|gap| gap.axes),
            Some(vec![Axis::Tags, Axis::Testonly])
        );
    }

    /// An axis the inputs state but this build carries no value on — a dump of a
    /// build that ran no aspect action — is support with nothing to offer, and
    /// no flag would add to it.
    #[test]
    fn an_axis_the_inputs_state_and_this_build_is_empty_on_is_no_gap() {
        assert!(gaps(&observed(true, 0)).is_empty());
        assert!(gaps(&observed(true, 6)).is_empty());
    }

    /// An axis every description states leaves nothing to report, so inputs that
    /// support all of them say nothing at all.
    #[test]
    fn inputs_that_state_every_axis_report_nothing() {
        assert!(gaps(&CapabilitiesBuilder::default().seal()).is_empty());
    }
}
