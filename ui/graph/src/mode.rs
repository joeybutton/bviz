use crate::GraphPanel;
use gpui::Context;

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

    /// Whether the mode's layout answers the `reduce` control. A map draws the
    /// reduced edge set at every level, so there is nothing there to switch.
    pub(crate) fn honors_reduce(self) -> bool {
        matches!(self, Self::Layered)
    }
}

/// Makes the picker stand at the layout drawn.
pub(crate) fn sync(view: &GraphPanel, cx: &mut Context<GraphPanel>) {
    let index = view.mode.index();
    view.layout_picker
        .update(cx, |picker, cx| picker.set_selected(index, cx));
}

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
}
