//! The on-disk form of a theme family and its conversion into [`Theme`].

use crate::theme::{Appearance, DataColors, TerminalColors, Theme, ThemeColors};
use gpui::{Hsla, Rgba};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeFamilyContent {
    name: String,
    /// Whom the family is credited to. Required of a family file so the credit
    /// travels with it; nothing in the crate reads it.
    #[allow(dead_code)]
    author: String,
    themes: Vec<ThemeContent>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeContent {
    name: String,
    appearance: AppearanceContent,
    colors: ThemeColorsContent,
    data: DataColorsContent,
    terminal: TerminalColorsContent,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum AppearanceContent {
    Light,
    Dark,
}

#[derive(Deserialize)]
#[serde(transparent)]
struct ColorContent(String);

impl ColorContent {
    fn parse(&self, role: &str) -> Result<Hsla, String> {
        Rgba::try_from(self.0.as_str())
            .map(Hsla::from)
            .map_err(|err| format!("role '{role}': {err}"))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeColorsContent {
    background: ColorContent,
    surface: ColorContent,
    panel: ColorContent,
    #[serde(rename = "element.hover")]
    element_hover: ColorContent,
    #[serde(rename = "element.active")]
    element_active: ColorContent,
    border: ColorContent,
    #[serde(rename = "border.variant")]
    border_variant: ColorContent,
    #[serde(rename = "border.focused")]
    border_focused: ColorContent,
    gridline: ColorContent,
    text: ColorContent,
    #[serde(rename = "text.muted")]
    text_muted: ColorContent,
    #[serde(rename = "text.disabled")]
    text_disabled: ColorContent,
    #[serde(rename = "text.accent")]
    text_accent: ColorContent,
    #[serde(rename = "text.on_accent")]
    text_on_accent: ColorContent,
    accent: ColorContent,
    selection: ColorContent,
    highlight: ColorContent,
    error: ColorContent,
    warning: ColorContent,
    success: ColorContent,
    info: ColorContent,
    shadow: ColorContent,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DataColorsContent {
    red: ColorContent,
    orange: ColorContent,
    yellow: ColorContent,
    green: ColorContent,
    cyan: ColorContent,
    blue: ColorContent,
    purple: ColorContent,
    neutral: ColorContent,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TerminalColorsContent {
    ansi: [ColorContent; TerminalColors::ANSI],
}

impl ThemeFamilyContent {
    pub fn parse(json: &str) -> Result<ThemeFamilyContent, String> {
        serde_json::from_str(json).map_err(|err| format!("parsing theme family: {err}"))
    }

    /// The themes of this family, in the order the family lists them.
    pub fn themes(&self) -> Result<Vec<Theme>, String> {
        self.themes
            .iter()
            .map(|theme| {
                theme
                    .build()
                    .map_err(|err| format!("theme '{}' of family '{}': {err}", theme.name, self.name))
            })
            .collect()
    }
}

impl ThemeContent {
    fn build(&self) -> Result<Theme, String> {
        Ok(Theme {
            name: self.name.clone().into(),
            appearance: match self.appearance {
                AppearanceContent::Light => Appearance::Light,
                AppearanceContent::Dark => Appearance::Dark,
            },
            colors: self.colors.build()?,
            data: self.data.build()?,
            terminal: self.terminal.build()?,
        })
    }
}

impl ThemeColorsContent {
    fn build(&self) -> Result<ThemeColors, String> {
        Ok(ThemeColors {
            background: self.background.parse("background")?,
            surface: self.surface.parse("surface")?,
            panel: self.panel.parse("panel")?,
            element_hover: self.element_hover.parse("element.hover")?,
            element_active: self.element_active.parse("element.active")?,
            border: self.border.parse("border")?,
            border_variant: self.border_variant.parse("border.variant")?,
            border_focused: self.border_focused.parse("border.focused")?,
            gridline: self.gridline.parse("gridline")?,
            text: self.text.parse("text")?,
            text_muted: self.text_muted.parse("text.muted")?,
            text_disabled: self.text_disabled.parse("text.disabled")?,
            text_accent: self.text_accent.parse("text.accent")?,
            text_on_accent: self.text_on_accent.parse("text.on_accent")?,
            accent: self.accent.parse("accent")?,
            selection: self.selection.parse("selection")?,
            highlight: self.highlight.parse("highlight")?,
            error: self.error.parse("error")?,
            warning: self.warning.parse("warning")?,
            success: self.success.parse("success")?,
            info: self.info.parse("info")?,
            shadow: self.shadow.parse("shadow")?,
        })
    }
}

impl DataColorsContent {
    fn build(&self) -> Result<DataColors, String> {
        Ok(DataColors {
            red: self.red.parse("data.red")?,
            orange: self.orange.parse("data.orange")?,
            yellow: self.yellow.parse("data.yellow")?,
            green: self.green.parse("data.green")?,
            cyan: self.cyan.parse("data.cyan")?,
            blue: self.blue.parse("data.blue")?,
            purple: self.purple.parse("data.purple")?,
            neutral: self.neutral.parse("data.neutral")?,
        })
    }
}

impl TerminalColorsContent {
    fn build(&self) -> Result<TerminalColors, String> {
        let mut ansi = [Hsla::default(); TerminalColors::ANSI];
        for (index, (slot, color)) in ansi.iter_mut().zip(&self.ansi).enumerate() {
            *slot = color.parse(&format!("terminal.ansi[{index}]"))?;
        }
        Ok(TerminalColors { ansi })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::BUILT_IN;
    use serde_json::{Value, json};

    /// The built-in family with one edit made to its first theme, standing in
    /// for a hand-written theme file that got something wrong.
    fn family_with(edit: impl FnOnce(&mut Value)) -> String {
        let mut family: Value =
            serde_json::from_str(BUILT_IN).expect("the built-in family is valid JSON");
        edit(&mut family["themes"][0]);
        family.to_string()
    }

    fn rejection(json: &str) -> String {
        ThemeFamilyContent::parse(json)
            .and_then(|family| family.themes().map(|_| ()))
            .expect_err("the family is rejected")
    }

    /// A missing role would otherwise have to be defaulted or filled in at every
    /// paint site, so it stops the family at the door.
    #[test]
    fn a_theme_missing_a_role_is_rejected() {
        let json = family_with(|theme| {
            theme["colors"].as_object_mut().expect("colors is an object").remove("shadow");
        });
        assert!(rejection(&json).contains("shadow"));
    }

    /// A key no role answers to is a misspelling of one that does, and passing
    /// it over would leave the role it was meant for silently unset.
    #[test]
    fn a_theme_carrying_an_unknown_role_is_rejected() {
        let json = family_with(|theme| theme["colors"]["elemnt.hover"] = json!("#ffffffff"));
        assert!(rejection(&json).contains("elemnt.hover"));
    }

    /// A color that is not one names the role it was written for, so the line to
    /// fix can be found without bisecting the file.
    #[test]
    fn a_role_set_to_something_that_is_not_a_color_is_rejected() {
        let json = family_with(|theme| theme["colors"]["accent"] = json!("cornflower"));
        assert!(rejection(&json).contains("role 'accent'"));
    }

    /// A short ANSI palette leaves cells with no color to resolve to, and a long
    /// one means the writer counted the slots differently than the terminal does.
    #[test]
    fn a_terminal_palette_of_the_wrong_length_is_rejected() {
        let json = family_with(|theme| {
            theme["terminal"]["ansi"].as_array_mut().expect("ansi is an array").truncate(8);
        });
        let rejection = rejection(&json);
        assert!(rejection.contains("invalid length 8"), "{rejection}");
    }

    /// One bad ANSI color among sixteen is only fixable if the report says which.
    #[test]
    fn a_bad_ansi_color_is_reported_by_index() {
        let json = family_with(|theme| theme["terminal"]["ansi"][11] = json!("#gggggg"));
        assert!(rejection(&json).contains("terminal.ansi[11]"));
    }
}
