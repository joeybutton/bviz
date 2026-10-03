//! What the fill of a box says, and the toolbar control choosing it.

use crate::GraphPanel;
use crate::timing::{NodeTimes, Untimed};
use bviz_ui_components::{Dropdown, TextTooltip, TooltipColors};
use bviz_ui_theme::ThemeColors;
use gpui::{Context, IntoElement, SharedString, div, prelude::*};

/// What a box's fill says.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub(crate) enum Colouring {
    /// The kind of node the box stands for.
    #[default]
    Kind,
    /// How long the actions landing on the box and on all it contains took.
    Time,
}

impl Colouring {
    /// Every colouring, in the order the picker offers them.
    pub(crate) const ALL: [Self; 2] = [Self::Kind, Self::Time];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Kind => "colour: kind",
            Self::Time => "colour: time",
        }
    }

    /// This colouring's place in [`ALL`](Self::ALL), which the picker reports
    /// its choice by.
    pub(crate) fn index(self) -> usize {
        self as usize
    }

    pub(crate) fn at(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }

    /// Whether the colouring reads the time placed on the graph's nodes.
    fn timed(self) -> bool {
        !matches!(self, Self::Kind)
    }
}

/// The profile flag naming each action's primary output.
const PRIMARY_OUTPUT_FLAG: &str = "--experimental_profile_include_primary_output";
/// The profile flag naming each action's owning target.
const TARGET_LABEL_FLAG: &str = "--experimental_profile_include_target_label";

/// What the picker says of the times it colours by: how much of the profile
/// the graph places, or why it places none.
fn note(times: Result<&NodeTimes, Untimed>) -> SharedString {
    match times {
        Ok(times) => {
            let coverage = times.coverage();
            format!(
                "{} of {} profiled actions placed on this graph",
                coverage.placed, coverage.profiled
            )
        }
        Err(Untimed::NoProfile) => {
            "Open the build's profile with the graph to colour its boxes by their actions"
                .to_owned()
        }
        Err(Untimed::NoActions) => "The build's profile records no actions".to_owned(),
        Err(Untimed::Unnamed) => format!(
            "The build's profile names no action; record one with \
             {PRIMARY_OUTPUT_FLAG} or {TARGET_LABEL_FLAG}"
        ),
        Err(Untimed::Unplaced) => {
            "None of the actions the build's profile names is in this graph".to_owned()
        }
    }
    .into()
}

/// Makes the picker offer the colourings the loaded graph can answer, and
/// falls back to colouring by kind where the one chosen is no longer among
/// them.
pub(crate) fn sync(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let timed = view
        .model
        .read(cx)
        .loaded()
        .is_some_and(|loaded| loaded.times().is_ok());
    view.colour_picker
        .update(cx, |picker, cx| offer(picker, timed, cx));
    if !timed && view.colouring.timed() {
        set(view, Colouring::Kind, cx);
    }
}

/// Makes `picker` offer the colourings by time only where the graph is
/// `timed`.
pub(crate) fn offer(picker: &mut Dropdown, timed: bool, cx: &mut Context<Dropdown>) {
    for colouring in Colouring::ALL {
        picker.set_available(colouring.index(), timed || !colouring.timed(), cx);
    }
}

/// Colours the boxes by `colouring`.
pub(crate) fn set(view: &mut GraphPanel, colouring: Colouring, cx: &mut Context<GraphPanel>) {
    if view.colouring == colouring {
        return;
    }
    view.colouring = colouring;
    view.colour_picker
        .update(cx, |picker, cx| picker.set_selected(colouring.index(), cx));
    view.invalidate_canvas(cx);
}

/// The picker, carrying what [`note`] says of the loaded graph's times.
pub(crate) fn control(
    view: &GraphPanel,
    colors: &ThemeColors,
    cx: &Context<GraphPanel>,
) -> impl IntoElement + use<> {
    let times = view
        .model
        .read(cx)
        .loaded()
        .map_or(Err(Untimed::NoProfile), |loaded| loaded.times());
    div()
        .id("colour-picker")
        .flex_none()
        .tooltip(TextTooltip::builder(
            note(times),
            TooltipColors::from_colors(colors),
        ))
        .child(view.colour_picker.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_colouring_sits_at_the_index_it_reports() {
        for colouring in Colouring::ALL {
            assert_eq!(Colouring::at(colouring.index()), Some(colouring));
        }
        assert_eq!(Colouring::at(Colouring::ALL.len()), None);
    }

    #[test]
    fn a_profile_naming_no_action_says_which_flags_name_them() {
        let said = note(Err(Untimed::Unnamed));
        assert!(said.contains(PRIMARY_OUTPUT_FLAG), "{said}");
        assert!(said.contains(TARGET_LABEL_FLAG), "{said}");
    }
}
