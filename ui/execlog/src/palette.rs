//! The colours the spawn-time view paints with, resolved from the theme.

use bviz_execlog::Phase;
use bviz_ui_components::{ScrollbarColors, TooltipColors};
use bviz_ui_theme::{Appearance, DataColors, Theme, ThemeColors};
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

/// How a fill departs from a data colour in one appearance.
#[derive(Clone, Copy)]
struct Step {
    /// Added to the HSL lightness.
    lightness: f32,
    /// Multiplies the HSL saturation.
    saturation: f32,
}

const fn step(lightness: f32, saturation: f32) -> Step {
    Step {
        lightness,
        saturation,
    }
}

const AS_IS: Step = step(0.0, 1.0);

/// A phase's fill: a data colour of the theme, stepped per appearance.
struct Fill {
    data: fn(&DataColors) -> Hsla,
    light: Step,
    dark: Step,
}

impl Fill {
    fn resolve(&self, theme: &Theme) -> Hsla {
        let step = match theme.appearance {
            Appearance::Light => self.light,
            Appearance::Dark => self.dark,
        };
        let base = (self.data)(&theme.data);
        Hsla {
            l: (base.l + step.lightness).clamp(0.0, 1.0),
            s: (base.s * step.saturation).clamp(0.0, 1.0),
            ..base
        }
    }
}

/// A phase's fill by its role in a spawn's run: execution, which the spawn
/// exists for, is green; waiting in the queue is yellow; moving inputs to the
/// executor and outputs back share blue; a retry is an alarm; the unaccounted
/// remainder carries no hue. The other phases take the hues left, and every
/// step is set so that phases drawn side by side in a bar stay apart for
/// normal and colour-deficient vision alike, in the One themes' light and
/// dark data colours, as the dataviz palette validator measures it.
fn phase_fill(phase: Phase) -> Fill {
    let fill = |data: fn(&DataColors) -> Hsla, light: Step, dark: Step| Fill { data, light, dark };
    match phase {
        Phase::Parse => fill(|d| d.cyan, AS_IS, step(-0.15, 1.5)),
        Phase::Upload => fill(|d| d.blue, step(-0.075, 1.25), step(-0.2, 1.5)),
        Phase::Network => fill(|d| d.purple, step(0.075, 1.0), AS_IS),
        Phase::Queue => fill(|d| d.yellow, AS_IS, step(-0.25, 1.0)),
        Phase::Setup => fill(|d| d.purple, step(-0.05, 1.0), step(-0.075, 1.25)),
        Phase::Execute => fill(|d| d.green, AS_IS, step(-0.175, 1.25)),
        Phase::ProcessOutputs => fill(|d| d.orange, step(-0.05, 1.0), step(-0.175, 1.5)),
        Phase::Fetch => fill(|d| d.blue, AS_IS, step(-0.1, 1.0)),
        Phase::Retry => fill(|d| d.red, step(-0.025, 1.0), step(-0.075, 1.0)),
        Phase::Other => fill(|d| d.neutral, step(-0.1, 1.0), step(0.15, 1.0)),
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
