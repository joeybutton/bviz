//! The colours the spawn-time view paints with, resolved from the theme.

use bviz_execlog::Phase;
use bviz_ui_components::{ScrollbarColors, TooltipColors};
use bviz_ui_theme::{DataColors, DataFill, Shade, Theme, ThemeColors};
use gpui::Hsla;

#[derive(Clone, Copy)]
pub(crate) struct Palette {
    pub(crate) colors: ThemeColors,
    phases: [Hsla; Phase::ALL.len()],
    /// The fill of a bar of execution alone, outlined in the execution fill: a
    /// neutral wash, which no phase's fill can be taken for.
    pub(crate) unbroken: Hsla,
    pub(crate) tooltip: TooltipColors,
    pub(crate) scroll: ScrollbarColors,
}

impl Palette {
    pub(crate) fn from_theme(theme: &Theme) -> Self {
        let phases = Phase::ALL.map(|phase| phase_fill(phase).resolve(theme));
        Self {
            colors: theme.colors,
            phases,
            unbroken: theme.colors.element_hover,
            tooltip: TooltipColors::from_colors(&theme.colors),
            scroll: ScrollbarColors::from_colors(&theme.colors),
        }
    }

    pub(crate) fn phase(&self, phase: Phase) -> Hsla {
        self.phases[phase.index()]
    }
}

/// A phase's fill by its role in a spawn's run: execution, which the spawn
/// exists for, is green; waiting in the queue is yellow; moving inputs to the
/// executor and outputs back share blue; a retry is an alarm; the unaccounted
/// remainder carries no hue. The other phases take the hues left, and every
/// step is set so that phases drawn side by side in a bar stay apart for
/// normal and colour-deficient vision alike, in the One themes' light and
/// dark data colours, as the dataviz palette validator measures it.
fn phase_fill(phase: Phase) -> DataFill {
    let fill =
        |data: fn(&DataColors) -> Hsla, light: Shade, dark: Shade| DataFill { data, light, dark };
    match phase {
        Phase::Parse => fill(|d| d.cyan, Shade::AS_IS, Shade::new(-0.15, 1.5)),
        Phase::Upload => fill(|d| d.blue, Shade::new(-0.075, 1.25), Shade::new(-0.2, 1.5)),
        Phase::Network => fill(|d| d.purple, Shade::new(0.075, 1.0), Shade::AS_IS),
        Phase::Queue => fill(|d| d.yellow, Shade::AS_IS, Shade::new(-0.25, 1.0)),
        Phase::Setup => fill(
            |d| d.purple,
            Shade::new(-0.05, 1.0),
            Shade::new(-0.075, 1.25),
        ),
        Phase::Execute => fill(|d| d.green, Shade::AS_IS, Shade::new(-0.175, 1.25)),
        Phase::ProcessOutputs => fill(
            |d| d.orange,
            Shade::new(-0.05, 1.0),
            Shade::new(-0.175, 1.5),
        ),
        Phase::Fetch => fill(|d| d.blue, Shade::AS_IS, Shade::new(-0.1, 1.0)),
        Phase::Retry => fill(|d| d.red, Shade::new(-0.025, 1.0), Shade::new(-0.075, 1.0)),
        Phase::Other => fill(|d| d.neutral, Shade::new(-0.1, 1.0), Shade::new(0.15, 1.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_ui_theme::{
        MIN_DISTINCT_COLOR, MIN_SECONDARY_CONTRAST, built_in_themes, contrast_ratio, distance,
    };

    #[test]
    fn phases_side_by_side_stay_apart_and_stand_off_the_canvas() {
        for theme in built_in_themes() {
            let palette = Palette::from_theme(&theme);
            let canvas = theme.colors.background;
            for pair in Phase::ALL.windows(2) {
                let (a, b) = (palette.phase(pair[0]), palette.phase(pair[1]));
                assert!(
                    distance(a, b) >= MIN_DISTINCT_COLOR,
                    "{}: {pair:?}",
                    theme.name
                );
            }
            for phase in Phase::ALL {
                let contrast = contrast_ratio(palette.phase(phase), canvas);
                assert!(
                    contrast >= MIN_SECONDARY_CONTRAST,
                    "{}: {phase:?} at {contrast}",
                    theme.name
                );
            }
        }
    }
}
