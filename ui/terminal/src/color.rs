//! Maps alacritty cell colors and flags to concrete gpui appearance: resolves
//! foreground/background against the default colors and the appearance-selected
//! ANSI palette, and derives font weight, style, and decorations.

use bviz_terminal::{Cell, Color, Flags, NamedColor, Rgb};
use gpui::{
    FontStyle, FontWeight, Rgba, StrikethroughStyle, UnderlineStyle, Window, WindowAppearance, px,
    rgb,
};

/// Cells are grouped into shaped runs by identical color and styling.
pub(crate) fn style_key(cell: &Cell) -> (Color, Color, Flags) {
    (cell.fg, cell.bg, cell.flags)
}

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
    pub(crate) fn resolve(cell: &Cell, fg: Rgba, bg: Rgba, ansi: &[u32; 16]) -> CellStyle {
        let mut foreground = to_rgba(cell.fg, fg, bg, ansi);
        let mut background = to_rgba(cell.bg, fg, bg, ansi);
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut foreground, &mut background);
        }
        if cell.flags.contains(Flags::DIM) {
            foreground = bviz_ui_theme::mix(foreground, background, 0.45);
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

// gpui's base palette has no ANSI colors, so the 16 named colors come from an
// established terminal theme — Zed's One — chosen to stay legible on gpui's
// default dark and light backgrounds. The default foreground and background
// still follow the gpui theme (see [`to_rgba`]).
const ONE_DARK: [u32; 16] = [
    0x282c34, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xabb2bf, 0x636d83,
    0xea858b, 0xaad581, 0xffd885, 0x85c1ff, 0xd398eb, 0x6ed5de, 0xfafafa,
];
const ONE_LIGHT: [u32; 16] = [
    0x000000, 0xde3e35, 0x3f953a, 0xd2b67c, 0x2f5af3, 0x950095, 0x0997b3, 0xbbbbbb, 0x000000,
    0xde3e35, 0x3f953a, 0xd2b67c, 0x2f5af3, 0xa00095, 0x0bbcd6, 0xffffff,
];

/// The ANSI palette for the window's current light/dark appearance.
pub(crate) fn ansi_palette(window: &Window) -> &'static [u32; 16] {
    match window.appearance() {
        WindowAppearance::Light | WindowAppearance::VibrantLight => &ONE_LIGHT,
        WindowAppearance::Dark | WindowAppearance::VibrantDark => &ONE_DARK,
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

/// Resolve an alacritty [`Color`] against the terminal's default fore/background
/// and the appearance-selected ANSI palette.
fn to_rgba(color: Color, fg: Rgba, bg: Rgba, ansi: &[u32; 16]) -> Rgba {
    match color {
        Color::Spec(c) => rgb_to_rgba(c),
        Color::Indexed(i) => indexed(i, ansi),
        Color::Named(named) => match named {
            NamedColor::Foreground
            | NamedColor::BrightForeground
            | NamedColor::DimForeground
            | NamedColor::Cursor => fg,
            NamedColor::Background => bg,
            other => {
                let idx = other as usize;
                if idx < 16 {
                    rgb(ansi[idx])
                } else if (259..=266).contains(&idx) {
                    // The dim ANSI block (DimBlack..DimWhite) maps to colors 0..7.
                    rgb(ansi[idx - 259])
                } else {
                    fg
                }
            }
        },
    }
}

/// Map a 256-color palette index to RGB: 0..15 ANSI, 16..231 cube, 232..255 gray.
fn indexed(i: u8, ansi: &[u32; 16]) -> Rgba {
    match i {
        0..=15 => rgb(ansi[i as usize]),
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
