//! Which layout the view draws, which edge set it draws them over, and the
//! toolbar controls choosing both.

use crate::{GraphPanel, ToggleReduce, derive};
use bviz_ui_components::{Switch, ToggleGroup, ToggleSegment};
use gpui::{Context, IntoElement, div, prelude::*};

/// Which layout the view draws.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub(crate) enum LayoutMode {
    /// The whole graph as one zoom pyramid, the level drawn following the
    /// camera.
    #[default]
    Map,
    /// A layered layout of the expanded view alone, re-run whenever that view
    /// changes.
    Layered,
}

impl LayoutMode {
    /// Every mode, in the order the picker offers them.
    pub(crate) const ALL: [Self; 2] = [Self::Map, Self::Layered];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Map => "map",
            Self::Layered => "layered",
        }
    }

    /// This mode's place in [`ALL`](Self::ALL), which the picker reports its
    /// choice by.
    pub(crate) fn index(self) -> usize {
        match self {
            Self::Map => 0,
            Self::Layered => 1,
        }
    }

    pub(crate) fn at(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }
}

/// Whether the drawing keeps only the transitive reduction of its edges, and
/// whether the reader can choose so.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Reduction {
    /// A layered layout, drawing the reduction where `true` and every edge
    /// otherwise, as the reader chose.
    Chosen(bool),
    /// The map, whose levels have no edge set but the reduced one.
    Always,
    /// Dependency paths drawn in a layered layout, which keep every edge.
    Nothing,
}

impl Reduction {
    /// The reduction drawn in `mode`, where the drawing's own choice of edge
    /// set is `reduced`, `None` where it offers none.
    pub(crate) fn of(mode: LayoutMode, reduced: Option<bool>) -> Self {
        match (mode, reduced) {
            (LayoutMode::Map, _) => Self::Always,
            (LayoutMode::Layered, Some(reduced)) => Self::Chosen(reduced),
            (LayoutMode::Layered, None) => Self::Nothing,
        }
    }

    /// Whether the drawing keeps only the reduction.
    pub(crate) fn on(self) -> bool {
        match self {
            Self::Chosen(reduced) => reduced,
            Self::Always => true,
            Self::Nothing => false,
        }
    }

    /// What switching the reduction chooses, or why the reader cannot switch
    /// it.
    pub(crate) fn switched(self) -> Result<bool, &'static str> {
        match self {
            Self::Chosen(reduced) => Ok(!reduced),
            Self::Always => Err(MAP_REDUCES),
            Self::Nothing => Err(NOTHING_TO_REDUCE),
        }
    }
}

const MAP_REDUCES: &str = "Disabled: map always reduced";
const NOTHING_TO_REDUCE: &str = "Disabled: paths draw every edge";

/// The reduction `view` draws.
pub(crate) fn reduction(view: &GraphPanel) -> Reduction {
    Reduction::of(view.mode, view.derived.reduced())
}

pub(crate) fn toggle_reduction(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    if let Ok(reduce) = reduction(view).switched() {
        derive::set_reduced(view, reduce, cx);
    }
}

/// Makes the picker stand at the layout drawn.
pub(crate) fn sync(view: &GraphPanel, cx: &mut Context<GraphPanel>) {
    let index = view.mode.index();
    view.layout_picker
        .update(cx, |picker, cx| picker.set_selected(index, cx));
}

/// The layout picker.
pub(crate) fn control(view: &GraphPanel) -> impl IntoElement + use<> {
    div()
        .id("layout-picker")
        .flex_none()
        .child(view.layout_picker.clone())
}

/// The toggle switching the reduction, standing as the drawing does and
/// saying why where it cannot be switched.
pub(crate) fn reduce_toggle(view: &GraphPanel, cx: &Context<GraphPanel>) -> ToggleGroup {
    let reduction = reduction(view);
    let segment = ToggleSegment::new(REDUCE);
    let segment = match reduction.switched() {
        Ok(_) => segment.keys_for(&ToggleReduce, &view.focus_handle),
        Err(reason) => segment.unavailable(reason),
    };
    let panel = cx.entity();
    let switch = Switch::new(segment, reduction.on(), move |reduce, _, cx| {
        panel.update(cx, |view, cx| derive::set_reduced(view, reduce, cx));
    });
    ToggleGroup::any_of("graph-reduction", REDUCE, [switch])
}

const REDUCE: &str = "reduce";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_sits_at_the_index_it_reports() {
        for mode in LayoutMode::ALL {
            assert_eq!(LayoutMode::at(mode.index()), Some(mode));
        }
        assert_eq!(LayoutMode::at(LayoutMode::ALL.len()), None);
    }

    #[test]
    fn a_map_stands_reduced_and_locked_whatever_the_layered_layout_draws() {
        for reduced in [None, Some(false), Some(true)] {
            let reduction = Reduction::of(LayoutMode::Map, reduced);
            assert_eq!(reduction, Reduction::Always);
            assert!(reduction.on());
            assert_eq!(reduction.switched(), Err(MAP_REDUCES));
        }
    }

    #[test]
    fn a_layered_layout_stands_at_the_edge_set_chosen_for_it() {
        for reduced in [false, true] {
            let reduction = Reduction::of(LayoutMode::Layered, Some(reduced));
            assert_eq!(reduction, Reduction::Chosen(reduced));
            assert_eq!(reduction.on(), reduced);
            assert_eq!(reduction.switched(), Ok(!reduced));
        }
    }

    #[test]
    fn dependency_paths_stand_unreduced_and_locked() {
        let reduction = Reduction::of(LayoutMode::Layered, None);
        assert_eq!(reduction, Reduction::Nothing);
        assert!(!reduction.on());
        assert_eq!(reduction.switched(), Err(NOTHING_TO_REDUCE));
    }
}
