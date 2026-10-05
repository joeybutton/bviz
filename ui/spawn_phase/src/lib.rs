//! How the phases of a spawn's run read on screen, alike in every view that
//! shows them.

use bviz_invocation::spawn::Phase;
use bviz_ui_theme::{DataColors, DataFill, Shade};
use gpui::Hsla;

pub fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Parse => "parse",
        Phase::Upload => "upload",
        Phase::Network => "network",
        Phase::Queue => "queue",
        Phase::Setup => "setup",
        Phase::Execute => "execute",
        Phase::ProcessOutputs => "process outputs",
        Phase::Fetch => "fetch",
        Phase::Retry => "retry",
        Phase::Other => "other",
    }
}

/// A phase's fill by its role in a spawn's run: execution, which the spawn
/// exists for, is green; waiting in the queue is yellow; moving inputs to the
/// executor and outputs back share blue; a retry is an alarm; the unaccounted
/// remainder carries no hue. The other phases take the hues left, and every
/// step is set so that phases drawn side by side in a bar stay apart for
/// normal and colour-deficient vision alike, in the One themes' light and
/// dark data colours, as the dataviz palette validator measures it.
pub fn phase_fill(phase: Phase) -> DataFill {
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
            let fills = Phase::ALL.map(|phase| phase_fill(phase).resolve(&theme));
            let canvas = theme.colors.background;
            for (pair, fill) in Phase::ALL.windows(2).zip(fills.windows(2)) {
                assert!(
                    distance(fill[0], fill[1]) >= MIN_DISTINCT_COLOR,
                    "{}: {pair:?}",
                    theme.name
                );
            }
            for (phase, fill) in Phase::ALL.into_iter().zip(fills) {
                let contrast = contrast_ratio(fill, canvas);
                assert!(
                    contrast >= MIN_SECONDARY_CONTRAST,
                    "{}: {phase:?} at {contrast}",
                    theme.name
                );
            }
        }
    }
}
