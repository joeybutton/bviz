use crate::schema::ThemeFamilyContent;
use crate::theme::{Appearance, Theme};
use gpui::{App, Global, Window, WindowAppearance};
use std::sync::Arc;

/// The theme family shipped in the binary.
pub(crate) const BUILT_IN: &str = include_str!("../themes/one.json");

/// The themes the application can paint with. Each answers to the name it was
/// registered under and no two share one, and whatever a window resolves to is
/// one of them.
pub struct ThemeRegistry {
    themes: Vec<Arc<Theme>>,
    light: Arc<Theme>,
    dark: Arc<Theme>,
    selected: Option<Arc<Theme>>,
}

impl Global for ThemeRegistry {}

impl ThemeRegistry {
    /// Loads the built-in family and takes its first theme of each appearance as
    /// that appearance's default.
    pub fn new() -> Result<ThemeRegistry, String> {
        let themes: Vec<Arc<Theme>> = ThemeFamilyContent::parse(BUILT_IN)?
            .themes()?
            .into_iter()
            .map(Arc::new)
            .collect();
        let first = |appearance| {
            themes
                .iter()
                .find(|theme| theme.appearance == appearance)
                .cloned()
                .ok_or_else(|| format!("built-in themes include none for {appearance:?}"))
        };
        Ok(ThemeRegistry {
            light: first(Appearance::Light)?,
            dark: first(Appearance::Dark)?,
            themes,
            selected: None,
        })
    }

    /// Takes `theme` in under its own name. Errors on a name already taken and
    /// leaves the theme holding it in place: a name reaches one theme, so a
    /// second one under it could never be resolved.
    pub fn register(&mut self, theme: Theme) -> Result<(), String> {
        if self.get(&theme.name).is_some() {
            return Err(format!("a theme named '{}' is already registered", theme.name));
        }
        self.themes.push(Arc::new(theme));
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&Arc<Theme>> {
        self.themes.iter().find(|theme| theme.name == name)
    }

    /// Paints everything with the registered theme called `name`, whatever the
    /// system appearance, or with `None` hands the choice back to the system.
    /// Errors on a name no theme was registered under, leaving the choice as it
    /// was.
    pub fn select(&mut self, name: Option<&str>) -> Result<(), String> {
        self.selected = match name {
            Some(name) => {
                Some(self.get(name).ok_or_else(|| format!("no theme named '{name}'"))?.clone())
            }
            None => None,
        };
        Ok(())
    }

    /// What a window of `appearance` paints with: whatever was selected, and
    /// failing that the default for that appearance.
    pub fn resolve(&self, appearance: Appearance) -> &Arc<Theme> {
        self.selected.as_ref().unwrap_or_else(|| self.default_for(appearance))
    }

    fn default_for(&self, appearance: Appearance) -> &Arc<Theme> {
        match appearance {
            Appearance::Light => &self.light,
            Appearance::Dark => &self.dark,
        }
    }
}

/// Every theme of the built-in family, for measuring a rule that has to hold
/// whichever of them a window paints with.
pub fn built_in_themes() -> Vec<Arc<Theme>> {
    ThemeRegistry::new().expect("built-in themes load").themes
}

/// Loads the built-in themes, which every later theme lookup reads.
pub fn init(cx: &mut App) -> Result<(), String> {
    cx.set_global(ThemeRegistry::new()?);
    Ok(())
}

/// The theme `window` paints with.
pub fn theme(window: &Window, cx: &App) -> Arc<Theme> {
    cx.global::<ThemeRegistry>().resolve(appearance(window)).clone()
}

fn appearance(window: &Window) -> Appearance {
    match window.appearance() {
        WindowAppearance::Light | WindowAppearance::VibrantLight => Appearance::Light,
        WindowAppearance::Dark | WindowAppearance::VibrantDark => Appearance::Dark,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        MIN_DISTINCT_COLOR, MIN_EDGED_STEP, MIN_SECONDARY_CONTRAST, MIN_TEXT_CONTRAST,
        MIN_VISIBLE_STEP, contrast_ratio, distance, label_on, level_step, luminance, over,
    };
    use gpui::Hsla;

    /// The saturation under which a color reads as a tinted grey rather than as
    /// a color of its own, so a ramp that only varies lightness cannot pass for
    /// a set of categories.
    const MIN_CHROMATIC_SATURATION: f32 = 0.2;

    /// How far a role that carries the accent may drift from it in hue, as a
    /// fraction of the wheel. The wheel carries twelve named hues, and half a
    /// band is as far as a color can move and still be called the same one.
    const MAX_ACCENT_HUE_DRIFT: f32 = 1.0 / 24.0;

    /// The ANSI slots carrying a hue, standard bank then bright bank. The four
    /// left out are the blacks, the grey and the white, which a terminal aligns
    /// with its own background and foreground, so nothing can be asked of how
    /// they sit against a surface.
    const ANSI_HUE_BANKS: [[usize; 6]; 2] = [[1, 2, 3, 4, 5, 6], [9, 10, 11, 12, 13, 14]];

    /// The built-in light theme under a name of its own, standing in for a theme
    /// registered from outside the crate.
    fn renamed(name: &str) -> Theme {
        ThemeFamilyContent::parse(&BUILT_IN.replace("One Light", name))
            .expect("a renamed family parses")
            .themes()
            .expect("a renamed family builds")
            .into_iter()
            .find(|theme| theme.appearance == Appearance::Light)
            .expect("a renamed family carries the renamed theme")
    }

    /// The surfaces a control, a state layer or a run of text can be drawn on.
    fn surfaces(theme: &Theme) -> [Hsla; 3] {
        [theme.colors.background, theme.colors.surface, theme.colors.panel]
    }

    /// How far apart two hues sit on the wheel, the shorter way round.
    fn hue_gap(a: Hsla, b: Hsla) -> f32 {
        let gap = (a.h - b.h).abs();
        gap.min(1.0 - gap)
    }

    /// Registering a theme is only worth anything if it can then be painted, so
    /// a selected theme has to displace the appearance default for every
    /// appearance, and giving the choice back has to restore it.
    #[test]
    fn the_selected_theme_is_what_a_window_resolves_to() {
        let mut registry = ThemeRegistry::new().expect("built-in themes load");
        registry.register(renamed("Under Test")).expect("a free name is taken");
        registry.select(Some("Under Test")).expect("a registered theme can be selected");
        for appearance in [Appearance::Light, Appearance::Dark] {
            assert_eq!(registry.resolve(appearance).name, "Under Test");
        }

        registry.select(None).expect("the system can always be given the choice back");
        assert_eq!(registry.resolve(Appearance::Dark).name, "One Dark");
        assert_eq!(registry.resolve(Appearance::Light).name, "One Light");
    }

    /// A name nothing was registered under names no theme to paint with, so it
    /// has to be reported rather than quietly leaving the window unpainted.
    #[test]
    fn selecting_a_theme_that_was_never_registered_is_an_error() {
        let mut registry = ThemeRegistry::new().expect("built-in themes load");
        registry.select(Some("One Dark")).expect("a built-in theme can be selected");

        assert!(registry.select(Some("Under Test")).is_err());
        assert_eq!(registry.resolve(Appearance::Light).name, "One Dark");
    }

    /// A clash passed over would go unnoticed until whichever theme lost the
    /// name failed to appear, so it is reported and the name keeps the theme it
    /// already reached.
    #[test]
    fn registering_a_theme_under_a_name_already_taken_is_an_error() {
        let mut registry = ThemeRegistry::new().expect("built-in themes load");

        assert!(registry.register(renamed("One Dark")).is_err());
        let held = registry.get("One Dark").expect("the name still reaches a theme");
        assert_eq!(held.appearance, Appearance::Dark);
    }

    /// A surface only reads as a plane of its own if it is separated from the
    /// others, whichever of them it is drawn against.
    #[test]
    fn each_surface_reads_as_a_plane_of_its_own() {
        for theme in built_in_themes() {
            let planes = surfaces(&theme);
            for (i, upper) in planes.iter().enumerate() {
                for lower in &planes[i + 1..] {
                    let step = level_step(*upper, *lower);
                    assert!(step >= MIN_EDGED_STEP, "{}: {step} levels", theme.name);
                }
            }
        }
    }

    /// A state layer has to register on every surface a stateful control can sit
    /// on, including the one it separates least well from.
    #[test]
    fn each_state_layer_registers_on_every_surface() {
        for theme in built_in_themes() {
            let c = &theme.colors;
            for layer in [c.element_hover, c.element_active] {
                for surface in surfaces(&theme) {
                    let step = level_step(over(layer, surface), surface);
                    assert!(step >= MIN_VISIBLE_STEP, "{}: {step} levels", theme.name);
                }
            }
        }
    }

    /// Hover and active mark two different things about one control, so a
    /// pressed control must not merely look hovered.
    #[test]
    fn a_pressed_control_is_told_from_a_hovered_one() {
        for theme in built_in_themes() {
            let c = &theme.colors;
            for surface in surfaces(&theme) {
                let step = level_step(over(c.element_active, surface), over(c.element_hover, surface));
                assert!(step >= MIN_VISIBLE_STEP, "{}: {step} levels", theme.name);
            }
        }
    }

    /// Selection marks a row by washing the accent over it, so it has to be a
    /// wash — of the accent, not of any color to hand — and the row's own text
    /// has to survive it.
    #[test]
    fn a_selected_row_reads_as_accented_and_keeps_its_text() {
        for theme in built_in_themes() {
            let c = &theme.colors;
            assert!(c.selection.a < 1.0, "{}: selection is opaque", theme.name);
            assert_eq!(
                c.selection,
                Hsla { a: c.selection.a, ..c.accent },
                "{}: selection is a wash of some other color",
                theme.name
            );

            for surface in surfaces(&theme) {
                let selected = over(c.selection, surface);
                let apart = distance(selected, surface);
                assert!(apart >= MIN_DISTINCT_COLOR, "{}: {apart}", theme.name);
                let contrast = contrast_ratio(c.text, selected);
                assert!(contrast >= MIN_TEXT_CONTRAST, "{}: {contrast}:1", theme.name);
            }
        }
    }

    /// Body text is read, not merely seen, on whichever surface it lands on;
    /// text that is secondary by design still has to be read to be worth setting.
    #[test]
    fn text_is_legible_on_every_surface() {
        for theme in built_in_themes() {
            let c = &theme.colors;
            for surface in surfaces(&theme) {
                let body = contrast_ratio(c.text, surface);
                assert!(body >= MIN_TEXT_CONTRAST, "{}: {body}:1", theme.name);
                let muted = contrast_ratio(c.text_muted, surface);
                assert!(muted >= MIN_SECONDARY_CONTRAST, "{}: {muted}:1", theme.name);
            }
        }
    }

    /// Disabled text says a control is there and cannot be used. It says that by
    /// being weaker than text that can be used — on the same surface, or the
    /// ranking is only true of the surface it was picked on — but it still has
    /// to be read to say what the control would have done.
    #[test]
    fn disabled_text_reads_as_the_weakest_text_without_vanishing() {
        for theme in built_in_themes() {
            let c = &theme.colors;
            for surface in surfaces(&theme) {
                let disabled = contrast_ratio(c.text_disabled, surface);
                assert!(disabled >= MIN_SECONDARY_CONTRAST, "{}: {disabled}:1", theme.name);
                assert!(
                    disabled < contrast_ratio(c.text_muted, surface),
                    "{}: {disabled}:1",
                    theme.name
                );
            }
        }
    }

    /// A link has to be picked out of the body text around it by its color,
    /// carry the accent it is named for, and be read as text wherever a run of
    /// text can be laid out.
    #[test]
    fn accent_text_is_told_from_body_text_and_read_on_every_surface() {
        for theme in built_in_themes() {
            let c = &theme.colors;
            let apart = distance(c.text_accent, c.text);
            assert!(apart >= MIN_DISTINCT_COLOR, "{}: {apart}", theme.name);
            let drift = hue_gap(c.text_accent, c.accent);
            assert!(drift <= MAX_ACCENT_HUE_DRIFT, "{}: {drift} of the wheel", theme.name);
            for surface in surfaces(&theme) {
                let contrast = contrast_ratio(c.text_accent, surface);
                assert!(contrast >= MIN_SECONDARY_CONTRAST, "{}: {contrast}:1", theme.name);
            }
        }
    }

    /// A label set on an accent fill — a filled button, a chip — is read against
    /// that fill and nothing else, so the accent is the only thing it has to
    /// carry against. A view painting a fill of its own reaches for [`label_on`]
    /// instead, and the two have to land on the same ink for the accent or the
    /// same button is labelled two ways.
    #[test]
    fn text_set_on_the_accent_is_legible_against_it() {
        for theme in built_in_themes() {
            let c = &theme.colors;
            let contrast = contrast_ratio(c.text_on_accent, c.accent);
            assert!(contrast >= MIN_TEXT_CONTRAST, "{}: {contrast}:1", theme.name);
            assert_eq!(label_on(c.accent), c.text_on_accent, "{}", theme.name);
        }
    }

    /// The lines laid on a surface rank by how much they ask to be noticed: the
    /// gridline is scenery, the variant divides one surface, the border bounds
    /// it. The faintest still has to register on every surface, and each has to
    /// stay under the next or the ranking is one the reader cannot see. The
    /// heaviest line drawn across a surface rather than around it stays under
    /// the fill marking a row hovered, which is a state and not scenery.
    #[test]
    fn the_lines_on_a_surface_register_and_keep_their_ranking() {
        for theme in built_in_themes() {
            let c = &theme.colors;
            for surface in surfaces(&theme) {
                let step = |line| level_step(over(line, surface), surface);
                let (gridline, variant, border) =
                    (step(c.gridline), step(c.border_variant), step(c.border));
                assert!(gridline >= MIN_EDGED_STEP, "{}: {gridline} levels", theme.name);
                assert!(
                    gridline < variant && variant < border,
                    "{}: {gridline} / {variant} / {border} levels",
                    theme.name
                );
                let hover = step(c.element_hover);
                assert!(variant < hover, "{}: {variant} levels against {hover}", theme.name);
            }
        }
    }

    /// The focus ring says which control the keyboard is aimed at, which it can
    /// only do by being told from the border every other control already has and
    /// by standing against the surface it is drawn on as WCAG 2.2 success
    /// criterion 2.4.11 Focus Appearance asks a focus indicator to.
    #[test]
    fn the_focus_ring_is_told_from_an_ordinary_border_and_seen_on_every_surface() {
        for theme in built_in_themes() {
            let c = &theme.colors;
            let apart = distance(c.border_focused, c.border);
            assert!(apart >= MIN_DISTINCT_COLOR, "{}: {apart}", theme.name);
            for surface in surfaces(&theme) {
                let contrast = contrast_ratio(c.border_focused, surface);
                assert!(contrast >= MIN_SECONDARY_CONTRAST, "{}: {contrast}:1", theme.name);
            }
        }
    }

    /// A status color carries its meaning in the color itself, so no two may be
    /// taken for each other; and the dot or bar it paints is read off whichever
    /// surface the thing it marks sits on.
    #[test]
    fn the_status_colors_are_told_apart_and_seen_on_every_surface() {
        for theme in built_in_themes() {
            let c = &theme.colors;
            let statuses = [c.error, c.warning, c.success, c.info];
            for (i, status) in statuses.iter().enumerate() {
                for surface in surfaces(&theme) {
                    let contrast = contrast_ratio(*status, surface);
                    assert!(contrast >= MIN_SECONDARY_CONTRAST, "{}: {contrast}:1", theme.name);
                }
                for other in &statuses[i + 1..] {
                    let apart = distance(*status, *other);
                    assert!(apart >= MIN_DISTINCT_COLOR, "{}: {apart}", theme.name);
                }
            }
        }
    }

    /// The highlight and the warning are both warm marks laid over content and
    /// they say opposite things about it, so seeing one must not leave the
    /// reader wondering which it was. The mark itself is a rule with a label set
    /// in the same color, so it is read as text against every surface it can be
    /// drawn over.
    #[test]
    fn the_highlight_is_told_from_the_warning_and_read_on_every_surface() {
        for theme in built_in_themes() {
            let c = &theme.colors;
            let apart = distance(c.highlight, c.warning);
            assert!(apart >= MIN_DISTINCT_COLOR, "{}: {apart}", theme.name);
            for surface in surfaces(&theme) {
                let contrast = contrast_ratio(c.highlight, surface);
                assert!(contrast >= MIN_TEXT_CONTRAST, "{}: {contrast}:1", theme.name);
            }
        }
    }

    /// A shadow reads as depth by darkening what it falls across, which means it
    /// has to let that surface through and take it downward, not toward some
    /// lightness of its own.
    #[test]
    fn a_shadow_darkens_whatever_it_falls_across() {
        for theme in built_in_themes() {
            let c = &theme.colors;
            assert!(c.shadow.a < 1.0, "{}: shadow is opaque", theme.name);
            for surface in surfaces(&theme) {
                let cast = over(c.shadow, surface);
                assert!(luminance(cast) < luminance(surface), "{}", theme.name);
                let step = level_step(cast, surface);
                assert!(step >= MIN_VISIBLE_STEP, "{}: {step} levels", theme.name);
            }
        }
    }

    /// The data slots color categories shown side by side, so each has to carry
    /// a hue of its own — a ramp of one hue would tell the reader only which
    /// category is lighter — and read as a different color from every other and
    /// from the canvas they are drawn on.
    #[test]
    fn the_data_slots_are_told_apart_from_each_other_and_the_canvas() {
        for theme in built_in_themes() {
            let slots = theme.data.chromatic();
            for (i, a) in slots.iter().enumerate() {
                assert!(a.s >= MIN_CHROMATIC_SATURATION, "{}: {} saturation", theme.name, a.s);
                let over_canvas = distance(*a, theme.colors.background);
                assert!(over_canvas >= MIN_DISTINCT_COLOR, "{}: {over_canvas}", theme.name);
                for b in &slots[i + 1..] {
                    let apart = distance(*a, *b);
                    assert!(apart >= MIN_DISTINCT_COLOR, "{}: {apart}", theme.name);
                }
            }
        }
    }

    /// The neutral slot marks the category that means nothing in particular, so
    /// it has to be the one slot without a hue while still being visible and
    /// never mistaken for a slot that does mean something.
    #[test]
    fn the_neutral_slot_carries_no_hue_of_its_own() {
        for theme in built_in_themes() {
            let neutral = theme.data.neutral;
            assert!(
                neutral.s < MIN_CHROMATIC_SATURATION,
                "{}: {} saturation",
                theme.name,
                neutral.s
            );
            for surface in surfaces(&theme) {
                let apart = distance(neutral, surface);
                assert!(apart >= MIN_DISTINCT_COLOR, "{}: {apart}", theme.name);
            }
            for slot in theme.data.chromatic() {
                let apart = distance(neutral, slot);
                assert!(apart >= MIN_DISTINCT_COLOR, "{}: {apart}", theme.name);
            }
        }
    }

    /// A terminal paints a cell in an ANSI color and expects the reader to know
    /// which one it was, so within a bank no two may collide and none may sink
    /// into the canvas the cells are painted on.
    #[test]
    fn every_ansi_hue_is_told_from_its_bank_and_from_the_canvas() {
        for theme in built_in_themes() {
            let ansi = &theme.terminal.ansi;
            for bank in ANSI_HUE_BANKS {
                for (i, slot) in bank.iter().enumerate() {
                    let over_canvas = distance(ansi[*slot], theme.colors.background);
                    assert!(over_canvas >= MIN_DISTINCT_COLOR, "{}: {over_canvas}", theme.name);
                    for other in &bank[i + 1..] {
                        let apart = distance(ansi[*slot], ansi[*other]);
                        assert!(apart >= MIN_DISTINCT_COLOR, "{}: {apart}", theme.name);
                    }
                }
            }
        }
    }
}
