//! The fills the parts of a critical path are drawn in, resolved from the
//! theme.

use bviz_critical_path::{Part, Phase};
use bviz_ui_spawn_phase::phase_fill;
use bviz_ui_theme::{DataColors, DataFill, Shade, Theme};
use gpui::Hsla;

#[derive(Clone, Copy)]
pub(crate) struct Fills {
    parts: [Hsla; Part::ALL.len()],
    /// The spawn-time view's phase fills, so a phase reads alike in both.
    phases: [Hsla; Phase::ALL.len()],
    around_spawns: Hsla,
    folded_phases: Hsla,
    /// The edge drawn round a part outside the path, whose fill recedes into
    /// the canvas.
    pub(crate) outside_edge: Hsla,
}

impl Fills {
    pub(crate) fn from_theme(theme: &Theme) -> Fills {
        Fills {
            parts: Part::ALL.map(|part| part_fill(part).resolve(theme)),
            phases: Phase::ALL.map(|phase| phase_fill(phase).resolve(theme)),
            around_spawns: AROUND_SPAWNS.resolve(theme),
            folded_phases: FOLDED_PHASES.resolve(theme),
            outside_edge: theme.colors.border,
        }
    }

    pub(crate) fn part(&self, part: Part) -> Hsla {
        self.parts[part.index()]
    }

    pub(crate) fn phase(&self, phase: Phase) -> Hsla {
        self.phases[phase.index()]
    }

    /// The fill of an action's time outside its spawns, edged in
    /// [`outside_edge`](Self::outside_edge) like the time outside the path.
    pub(crate) fn around_spawns(&self) -> Hsla {
        self.around_spawns
    }

    /// The fill of the phases too short to draw apart in a step's strip.
    pub(crate) fn folded_phases(&self) -> Hsla {
        self.folded_phases
    }
}

/// An action's time outside its spawns carries no hue, like the time outside
/// the path, and recedes toward the canvas less far, between it and the
/// unaccounted phase. Stepped like the parts.
const AROUND_SPAWNS: DataFill = DataFill {
    data: |d| d.neutral,
    light: Shade::new(0.05, 1.0),
    dark: Shade::new(0.03, 1.0),
};

/// Phases folded together carry no hue either, standing furthest from the
/// canvas of the neutral fills so that a sliver of them still shows.
const FOLDED_PHASES: DataFill = DataFill {
    data: |d| d.neutral,
    light: Shade::new(-0.25, 1.0),
    dark: Shade::new(0.27, 1.0),
};

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

    #[test]
    fn the_time_around_spawns_and_folded_phases_stay_apart_from_every_phase_and_the_outside() {
        for theme in built_in_themes() {
            let fills = Fills::from_theme(&theme);
            let others = Phase::ALL
                .map(|phase| (format!("{phase:?}"), fills.phase(phase)))
                .into_iter()
                .chain([("outside".to_owned(), fills.part(Part::LeadIn))]);
            for (name, other) in others {
                for (fill, of) in [
                    (fills.around_spawns(), "around spawns"),
                    (fills.folded_phases(), "folded phases"),
                ] {
                    let d = distance(fill, other);
                    assert!(
                        d >= MIN_DISTINCT_COLOR,
                        "{}: {of} and {name} at {d}",
                        theme.name
                    );
                }
            }
            let d = distance(fills.around_spawns(), fills.folded_phases());
            assert!(d >= MIN_DISTINCT_COLOR, "{}: at {d}", theme.name);
        }
    }
}
