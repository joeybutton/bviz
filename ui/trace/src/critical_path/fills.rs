//! The fills the parts of a critical path are drawn in, resolved from the
//! theme.

use bviz_critical_path::Part;
use bviz_ui_theme::{DataColors, DataFill, Shade, Theme};
use gpui::Hsla;

#[derive(Clone, Copy)]
pub(crate) struct Fills {
    parts: [Hsla; Part::ALL.len()],
    /// The edge drawn round a part outside the path, whose fill recedes into
    /// the canvas.
    pub(crate) outside_edge: Hsla,
}

impl Fills {
    pub(crate) fn from_theme(theme: &Theme) -> Fills {
        Fills {
            parts: Part::ALL.map(|part| part_fill(part).resolve(theme)),
            outside_edge: theme.colors.border,
        }
    }

    pub(crate) fn part(&self, part: Part) -> Hsla {
        self.parts[part.index()]
    }
}

/// Whether `part` lies outside the path: before its first step or after its
/// last.
pub(crate) fn outside(part: Part) -> bool {
    matches!(part, Part::LeadIn | Part::Tail)
}

/// A part's fill by its role: the action, which a step exists for, is green
/// and waiting is yellow; time outside the path carries no hue and recedes
/// toward the canvas, which its edge stands it off from. Every part a bar can
/// draw beside another is stepped until the two stay apart for normal and
/// colour-deficient vision alike, in the One themes' light and dark data
/// colours, as the dataviz palette validator measures it.
fn part_fill(part: Part) -> DataFill {
    let fill =
        |data: fn(&DataColors) -> Hsla, light: Shade, dark: Shade| DataFill { data, light, dark };
    match part {
        Part::LeadIn | Part::Tail => fill(
            |d| d.neutral,
            Shade::new(0.2, 1.0),
            Shade::new(-0.0875, 1.5),
        ),
        Part::Waiting => fill(|d| d.yellow, Shade::new(0.0, 1.25), Shade::new(-0.35, 1.25)),
        Part::BeforeAction => fill(|d| d.cyan, Shade::new(0.0625, 1.5), Shade::new(-0.15, 1.5)),
        Part::Action => fill(|d| d.green, Shade::new(-0.225, 1.5), Shade::new(-0.2, 1.5)),
        Part::Unspanned => fill(
            |d| d.blue,
            Shade::new(-0.15, 1.5),
            Shade::new(-0.0875, 0.75),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_ui_theme::{
        MIN_DISTINCT_COLOR, MIN_SECONDARY_CONTRAST, built_in_themes, contrast_ratio, distance,
    };

    /// Parts no bar draws side by side, or that share a fill: a step's time
    /// before its action span and a step's time with no span are never one
    /// step's, and in the path's bar the action stands between them.
    fn never_adjacent(a: Part, b: Part) -> bool {
        matches!(
            (a, b),
            (Part::BeforeAction, Part::Unspanned) | (Part::LeadIn, Part::Tail)
        )
    }

    #[test]
    fn parts_stay_apart_and_those_on_the_path_stand_off_the_canvas() {
        for theme in built_in_themes() {
            let fills = Fills::from_theme(&theme);
            for (i, a) in Part::ALL.into_iter().enumerate() {
                for b in Part::ALL[i + 1..].iter().copied() {
                    if never_adjacent(a, b) {
                        continue;
                    }
                    let d = distance(fills.part(a), fills.part(b));
                    assert!(
                        d >= MIN_DISTINCT_COLOR,
                        "{}: {a:?} {b:?} at {d}",
                        theme.name
                    );
                }
                let contrast = contrast_ratio(fills.part(a), theme.colors.background);
                assert!(
                    outside(a) || contrast >= MIN_SECONDARY_CONTRAST,
                    "{}: {a:?} at {contrast}",
                    theme.name
                );
            }
        }
    }
}
