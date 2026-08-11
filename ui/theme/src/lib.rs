//! Colors and appearance helpers shared across the UI crates.

mod registry;
mod schema;
mod theme;

pub use registry::{ThemeRegistry, init, theme};
pub use schema::ThemeFamilyContent;
pub use theme::{Appearance, DataColors, TerminalColors, Theme, ThemeColors};

pub use gpui::colors::Colors;
use gpui::{hsla, Hsla, Rgba, Window};

pub fn colors(window: &Window) -> Colors {
    Colors::for_appearance(window)
}

/// Linearly blend `a` toward `b` by `t` in [0, 1].
pub fn mix(a: impl Into<Rgba>, b: impl Into<Rgba>, t: f32) -> Rgba {
    let (a, b) = (a.into(), b.into());
    Rgba {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

/// The luma (Y') of `c`: its gamma-encoded sRGB channels weighted as the eye
/// weights them, with no linearization. Cheap, and enough to rank two fills by
/// lightness or fit a lightness to the theme; for a legibility judgement reach
/// for [`contrast_ratio`].
pub fn luminance(c: impl Into<Rgba>) -> f32 {
    let c = c.into();
    0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b
}

/// The gap in luma between two fills, in 8-bit sRGB levels. Alpha plays no part,
/// so a translucent fill has to be put on its surface with [`over`] first for
/// this to say anything about what is on screen.
pub fn level_step(a: impl Into<Rgba>, b: impl Into<Rgba>) -> f32 {
    255.0 * (luminance(a) - luminance(b)).abs()
}

/// The smallest step, in 8-bit sRGB levels, at which one neutral fill still reads
/// as separate from another it lies on with no line drawn between them.
pub const MIN_VISIBLE_STEP: f32 = 12.0;

/// The smallest step at which two fills read as separate where an edge already
/// marks the place they meet — a hairline drawn across a surface, a border two
/// surfaces are laid against. The drawn edge is what the eye locks onto, so the
/// fills on either side of it part at half the step they need with nothing
/// between them.
pub const MIN_EDGED_STEP: f32 = MIN_VISIBLE_STEP / 2.0;

/// How far a hover fill stands off the surface it lies on: enough to clear every
/// surface a hoverable row sits on and the rule drawn between rows, enough to
/// stay clear of the selection accent that marks a different state, and no more
/// than that, so the row's own text stays the strongest thing in it.
const HOVER_LIFT: f32 = 0.11;

/// The fill marking the row or control under the cursor, for one that shows the
/// cursor by lifting off its surface rather than by taking the selection accent.
pub fn hover_fill(colors: &Colors) -> Rgba {
    mix(colors.background, colors.text, HOVER_LIFT)
}

/// `overlay` composited over `surface`, which is what a translucent fill drawn on
/// that surface actually looks like. Both may be translucent; the result carries
/// the opacity the pair reaches together, so stacking washes composes.
pub fn over(overlay: impl Into<Rgba>, surface: impl Into<Rgba>) -> Rgba {
    let (overlay, surface) = (overlay.into(), surface.into());
    let covered = surface.a * (1.0 - overlay.a);
    let a = overlay.a + covered;
    if a == 0.0 {
        return overlay;
    }
    let blend = |over: f32, under: f32| (over * overlay.a + under * covered) / a;
    Rgba {
        r: blend(overlay.r, surface.r),
        g: blend(overlay.g, surface.g),
        b: blend(overlay.b, surface.b),
        a,
    }
}

/// How far apart two colors are, counting hue as well as lightness: the
/// "redmean" approximation of perceptual distance described in "Colour metric"
/// at <https://www.compuphase.com/cmetric.htm>, on the same 8-bit sRGB scale
/// [`level_step`] reports in. It compares the two colors as written, alpha and
/// all discarded, so a translucent one measures as the color it is a wash of and
/// not as the wash; put it on its surface with [`over`] to measure the wash.
pub fn distance(a: impl Into<Rgba>, b: impl Into<Rgba>) -> f32 {
    let (a, b) = (a.into(), b.into());
    let mean_red = (a.r + b.r) / 2.0;
    let (dr, dg, db) = (a.r - b.r, a.g - b.g, a.b - b.b);
    255.0
        * ((2.0 + mean_red) * dr * dr + 4.0 * dg * dg + (3.0 - mean_red) * db * db)
            .sqrt()
}

/// The smallest distance at which two fills shown side by side still read as
/// different colors rather than as shades of one: what two neutrals score when
/// they part by [`MIN_VISIBLE_STEP`]. The weights [`distance`] sums under the
/// root come to nine along the grey axis whatever the mean red, so a pair of
/// neutrals scores three for every level between them.
pub const MIN_DISTINCT_COLOR: f32 = 3.0 * MIN_VISIBLE_STEP;

/// The contrast between two colors as WCAG 2.2 defines it, from 1 for a pair
/// that matches to 21 for black against white: see "contrast ratio" in
/// <https://www.w3.org/TR/WCAG22/#dfn-contrast-ratio>. Alpha plays no part, so a
/// translucent color has to be put on its surface with [`over`] first.
pub fn contrast_ratio(a: impl Into<Rgba>, b: impl Into<Rgba>) -> f32 {
    let (a, b) = (relative_luminance(a), relative_luminance(b));
    (a.max(b) + VIEWING_FLARE) / (a.min(b) + VIEWING_FLARE)
}

/// What WCAG adds to both luminances of a pair, standing for the light a screen
/// scatters back at the reader and so never lets a ratio run to infinity.
const VIEWING_FLARE: f32 = 0.05;

/// The light `c` actually emits, its sRGB channels linearized and then weighted:
/// the "relative luminance" of <https://www.w3.org/TR/WCAG22/#dfn-relative-luminance>.
fn relative_luminance(c: impl Into<Rgba>) -> f32 {
    let c = c.into();
    let linear =
        |u: f32| if u <= 0.04045 { u / 12.92 } else { ((u + 0.055) / 1.055).powf(2.4) };
    0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b)
}

/// The contrast body text holds against the surface it is set on: the 4.5:1 that
/// WCAG 2.2 success criterion 1.4.3 Contrast (Minimum) asks for at level AA.
pub const MIN_TEXT_CONTRAST: f32 = 4.5;

/// The contrast held by everything read without being body text — a caption, a
/// link set in a band, an icon, a rule: the 3:1 of WCAG 2.2 success criteria
/// 1.4.3, which relaxes to it for large text, and 1.4.11 Non-text Contrast.
pub const MIN_SECONDARY_CONTRAST: f32 = 3.0;

/// `color` at full opacity, for legible text tinted like its translucent dot.
pub fn opaque(color: Hsla) -> Hsla {
    Hsla { a: 1.0, ..color }
}

/// Black or white, whichever holds the greater [`contrast_ratio`] against
/// `fill`.
pub fn label_on(fill: impl Into<Rgba>) -> Hsla {
    let fill = fill.into();
    let (black, white) = (hsla(0.0, 0.0, 0.0, 1.0), hsla(0.0, 0.0, 1.0, 1.0));
    if contrast_ratio(white, fill) > contrast_ratio(black, fill) { white } else { black }
}

/// A stable hue in [0, 1) for `s`: FNV-1a over the bytes folded onto the wheel.
/// The same string always maps to the same hue.
pub fn hue_of(s: &str) -> f32 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    (hash % 360) as f32 / 360.0
}
