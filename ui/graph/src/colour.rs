//! What the fill of a box says, and the toolbar control choosing it.

use crate::GraphPanel;
use crate::model::GraphModel;
use crate::timing::{NodeTimes, Untimed};
use bviz_bazel_flags::{PROFILE_PRIMARY_OUTPUT, PROFILE_TARGET_LABEL};
use bviz_ui_components::{Dropdown, TextTooltip};
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
    /// Where those actions ran.
    Runner,
    /// Where the execution attempts of those actions stand at the document's
    /// playback time.
    Playback,
}

impl Colouring {
    /// Every colouring, in the order the picker offers them.
    pub(crate) const ALL: [Self; 4] = [Self::Kind, Self::Time, Self::Runner, Self::Playback];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Kind => "colour: kind",
            Self::Time => "colour: time",
            Self::Runner => "colour: runner",
            Self::Playback => "colour: playback",
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

    /// Whether a graph carrying what `offered` says can be coloured so.
    fn available(self, offered: Offered) -> bool {
        match self {
            Self::Kind => true,
            Self::Time | Self::Runner => offered.timed,
            Self::Playback => offered.playable,
        }
    }
}

/// What the loaded graph carries that a colouring reads.
#[derive(Clone, Copy)]
pub(crate) struct Offered {
    /// Its nodes carry the time of the actions landing on them.
    timed: bool,
    /// Its document plays back, and its nodes carry the execution attempts
    /// landing on them.
    playable: bool,
}

impl Offered {
    pub(crate) fn by(model: &GraphModel) -> Self {
        let loaded = model.loaded();
        Offered {
            timed: loaded.is_some_and(|loaded| loaded.times().is_ok()),
            playable: model.clock().is_some()
                && loaded.is_some_and(|loaded| loaded.states().is_some()),
        }
    }
}

/// What a control reading the graph's action times says of them: how much of
/// the profile the graph places, or why it places none, and where no profile
/// is open, that one opened with the graph lets it `read` them.
pub(crate) fn note(times: Result<&NodeTimes, Untimed>, read: &str) -> SharedString {
    match times {
        Ok(times) => times.coverage().line(),
        Err(Untimed::NoProfile) => {
            format!("Open the build's profile with the graph to {read}")
        }
        Err(Untimed::NoActions) => "The build's profile records no actions".to_owned(),
        Err(Untimed::Unnamed) => format!(
            "The build's profile names no action; record one with \
             {PROFILE_PRIMARY_OUTPUT} or {PROFILE_TARGET_LABEL}"
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
    let offered = Offered::by(view.model.read(cx));
    view.colour_picker
        .update(cx, |picker, cx| offer(picker, offered, cx));
    if !view.colouring.available(offered) {
        set(view, Colouring::Kind, cx);
    }
}

/// Makes `picker` offer the colourings a graph carrying what `offered` says
/// can be coloured by.
pub(crate) fn offer(picker: &mut Dropdown, offered: Offered, cx: &mut Context<Dropdown>) {
    for colouring in Colouring::ALL {
        picker.set_available(colouring.index(), colouring.available(offered), cx);
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
            note(times, "colour its boxes by their actions"),
            colors.tooltip(),
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
        let said = note(Err(Untimed::Unnamed), "read them");
        assert!(said.contains(PROFILE_PRIMARY_OUTPUT.as_str()), "{said}");
        assert!(said.contains(PROFILE_TARGET_LABEL.as_str()), "{said}");
    }
}
