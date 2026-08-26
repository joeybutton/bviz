//! Maps alacritty cell colors and flags to concrete gpui appearance: resolves
//! foreground/background against the theme's default colors and ANSI palette,
//! and derives font weight, style, and decorations.

use bviz_terminal::{Cell, Color, Flags, NamedColor, Rgb};
use bviz_ui_theme::{TerminalColors, Theme};
use gpui::{FontStyle, FontWeight, Rgba, StrikethroughStyle, UnderlineStyle, px};

/// Cells are grouped into shaped runs by identical color and styling.
pub(crate) fn style_key(cell: &Cell) -> (Color, Color, Flags) {
    (cell.fg, cell.bg, cell.flags)
}

/// The colors an alacritty cell color resolves against.
pub(crate) struct Defaults {
    pub(crate) fg: Rgba,
    pub(crate) bg: Rgba,
    pub(crate) ansi: [Rgba; TerminalColors::ANSI],
}

impl Defaults {
    /// A cell carrying no color of its own is set in the theme's body text on
    /// its canvas, so plain output reads as the window's own text; one naming an
    /// ANSI slot is set in the theme's terminal palette.
    pub(crate) fn from_theme(theme: &Theme) -> Defaults {
        Defaults {
            fg: theme.colors.text.into(),
            bg: theme.colors.background.into(),
            ansi: theme.terminal.ansi.map(Rgba::from),
        }
    }
}

/// How far a faint cell (ECMA-48 SGR 2) is pulled toward its own background:
/// far enough to be told from a normal cell beside it, and short enough that
/// what it says is still read.
const DIM_PULL: f32 = 0.35;

/// A cell's appearance resolved to concrete colors and font attributes.
pub(crate) struct CellStyle {
    pub(crate) foreground: Rgba,
    pub(crate) background: Rgba,
    pub(crate) weight: FontWeight,
    pub(crate) font_style: FontStyle,
    pub(crate) underline: Option<UnderlineStyle>,
    pub(crate) strikethrough: Option<StrikethroughStyle>,
}

impl CellStyle {
    pub(crate) fn resolve(cell: &Cell, defaults: &Defaults) -> CellStyle {
        let mut foreground = to_rgba(cell.fg, defaults);
        let mut background = to_rgba(cell.bg, defaults);
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut foreground, &mut background);
        }
        if cell.flags.contains(Flags::DIM) {
            foreground = bviz_ui_theme::mix(foreground, background, DIM_PULL);
        }
        if cell.flags.contains(Flags::HIDDEN) {
            foreground = background;
        }
        // gpui can render solid and wavy underlines; map undercurl to wavy and
        // the other variants (double, dotted, dashed) to a plain underline.
        let underline = cell
            .flags
            .intersects(Flags::ALL_UNDERLINES)
            .then_some(UnderlineStyle {
                color: Some(foreground.into()),
                thickness: px(1.0),
                wavy: cell.flags.contains(Flags::UNDERCURL),
            });
        let strikethrough = cell
            .flags
            .contains(Flags::STRIKEOUT)
            .then_some(StrikethroughStyle {
                color: Some(foreground.into()),
                thickness: px(1.0),
            });
        CellStyle {
            foreground,
            background,
            weight: if cell.flags.contains(Flags::BOLD) {
                FontWeight::BOLD
            } else {
                FontWeight::NORMAL
            },
            font_style: if cell.flags.contains(Flags::ITALIC) {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
            underline,
            strikethrough,
        }
    }
}

fn rgb_to_rgba(c: Rgb) -> Rgba {
    Rgba {
        r: c.r as f32 / 255.0,
        g: c.g as f32 / 255.0,
        b: c.b as f32 / 255.0,
        a: 1.0,
    }
}

/// Resolve an alacritty [`Color`] against the terminal's [`Defaults`].
fn to_rgba(color: Color, defaults: &Defaults) -> Rgba {
    let Defaults { fg, bg, ansi } = defaults;
    match color {
        Color::Spec(c) => rgb_to_rgba(c),
        Color::Indexed(i) => indexed(i, ansi),
        Color::Named(named) => match named {
            NamedColor::Foreground
            | NamedColor::BrightForeground
            | NamedColor::DimForeground
            | NamedColor::Cursor => *fg,
            NamedColor::Background => *bg,
            other => {
                let idx = other as usize;
                if idx < 16 {
                    ansi[idx]
                } else if (259..=266).contains(&idx) {
                    // The dim ANSI block (DimBlack..DimWhite) maps to colors 0..7.
                    ansi[idx - 259]
                } else {
                    *fg
                }
            }
        },
    }
}

/// Map a 256-color palette index to RGB: 0..15 ANSI, 16..231 cube, 232..255 gray.
fn indexed(i: u8, ansi: &[Rgba; TerminalColors::ANSI]) -> Rgba {
    match i {
        0..=15 => ansi[i as usize],
        16..=231 => {
            let j = i - 16;
            let level = |v: u8| -> f32 {
                let step = if v == 0 { 0 } else { 55 + v as u16 * 40 };
                step as f32 / 255.0
            };
            Rgba {
                r: level(j / 36),
                g: level((j % 36) / 6),
                b: level(j % 6),
                a: 1.0,
            }
        }
        232..=255 => {
            let v = (8 + (i - 232) as u16 * 10) as f32 / 255.0;
            Rgba {
                r: v,
                g: v,
                b: v,
                a: 1.0,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_ui_theme::{
        MIN_TEXT_CONTRAST, MIN_VISIBLE_STEP, built_in_themes, contrast_ratio, level_step, mix,
    };

    /// A faint cell says its text matters less than the text beside it, which it
    /// can only say by being told from that text while still being read itself.
    /// It is body text in a terminal, so it is read to the same bar as any other
    /// cell.
    #[test]
    fn a_faint_cell_is_told_from_a_normal_one_and_still_read() {
        for theme in built_in_themes() {
            let (fg, bg) = (theme.colors.text, theme.colors.background);
            let faint = mix(fg, bg, DIM_PULL);
            let contrast = contrast_ratio(faint, bg);
            assert!(
                contrast >= MIN_TEXT_CONTRAST,
                "{}: {contrast}:1",
                theme.name
            );
            let step = level_step(faint, fg);
            assert!(step >= MIN_VISIBLE_STEP, "{}: {step} levels", theme.name);
        }
    }
}
