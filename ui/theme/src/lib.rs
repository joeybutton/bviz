//! Colors and appearance helpers shared across the UI crates.

mod registry;
mod schema;
mod theme;

pub use registry::{ThemeRegistry, built_in_themes, init, theme};
pub use schema::ThemeFamilyContent;
pub use theme::{Appearance, DataColors, TerminalColors, Theme, ThemeColors};

use gpui::{Hsla, Rgba, hsla};
use std::f32::consts::TAU;

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
    255.0 * ((2.0 + mean_red) * dr * dr + 4.0 * dg * dg + (3.0 - mean_red) * db * db).sqrt()
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
    0.2126 * to_linear(c.r) + 0.7152 * to_linear(c.g) + 0.0722 * to_linear(c.b)
}

/// The light the gamma-encoded sRGB channel `u` stands for: the transfer
/// function of IEC 61966-2-1, which [`from_linear`] undoes.
fn to_linear(u: f32) -> f32 {
    if u <= 0.04045 {
        u / 12.92
    } else {
        ((u + 0.055) / 1.055).powf(2.4)
    }
}

/// The gamma-encoded sRGB channel standing for the linear-light value `u`.
fn from_linear(u: f32) -> f32 {
    if u <= 0.003_130_8 {
        12.92 * u
    } else {
        1.055 * u.max(0.0).powf(1.0 / 2.4) - 0.055
    }
}

/// A color in the polar form of the Oklab space Björn Ottosson gives in "A
/// perceptual color space for image processing",
/// <https://bottosson.github.io/posts/oklab/>. Its three coordinates are close
/// to independent to the eye, so a color can be moved along one of them and go
/// on looking like itself in the other two.
#[derive(Clone, Copy, Debug)]
pub struct Oklch {
    /// Perceptual lightness, 0 at black and 1 at white.
    pub lightness: f32,
    /// Distance from the grey axis, 0 for a neutral and about a third at the
    /// most saturated color sRGB can show.
    pub chroma: f32,
    /// Position on the hue wheel, as a turn in [0, 1).
    pub hue: f32,
}

/// `color` in [`Oklch`].
pub fn oklch(color: impl Into<Rgba>) -> Oklch {
    let (l, a, b) = oklab(color.into());
    Oklch {
        lightness: l,
        chroma: a.hypot(b),
        hue: b.atan2(a).rem_euclid(TAU) / TAU,
    }
}

/// `color` carried `step` along the lightness of [`Oklch`] — toward white for a
/// positive step, toward black for a negative one — holding the hue and chroma
/// that give it its identity. Where the sRGB gamut at the new lightness cannot
/// hold that chroma the result takes as much of it as the gamut does, so the
/// color pales rather than shifting hue.
pub fn lighten(color: impl Into<Rgba>, step: f32) -> Rgba {
    let color = color.into();
    let (l, a, b) = oklab(color);
    let l = (l + step).clamp(0.0, 1.0);
    let (mut held, mut beyond) = (0.0, 1.0);
    for _ in 0..GAMUT_HALVINGS {
        let part = (held + beyond) / 2.0;
        match oklab_to_linear(l, a * part, b * part)
            .iter()
            .all(displayable)
        {
            true => held = part,
            false => beyond = part,
        }
    }
    let linear = oklab_to_linear(l, a * held, b * held);
    Rgba {
        r: from_linear(linear[0]).clamp(0.0, 1.0),
        g: from_linear(linear[1]).clamp(0.0, 1.0),
        b: from_linear(linear[2]).clamp(0.0, 1.0),
        a: color.a,
    }
}

/// Halvings [`lighten`] spends finding how much chroma the sRGB gamut holds.
/// Each one halves the share of the color's chroma still in question, and this
/// many leave that share far under what a rounded 8-bit channel can show.
const GAMUT_HALVINGS: u32 = 16;

/// Whether a linear-light channel is one a display can show.
fn displayable(channel: &f32) -> bool {
    (-GAMUT_SLACK..=1.0 + GAMUT_SLACK).contains(channel)
}

/// Slack allowed on a linear-light channel before it counts as outside the
/// gamut, covering the rounding of the conversion into and out of Oklab so that
/// a color already sitting on the edge is not read as being past it.
const GAMUT_SLACK: f32 = 1e-6;

/// `color` as Oklab lightness and the two opponent axes, of which [`Oklch`]'s
/// chroma and hue are the polar reading.
#[allow(clippy::excessive_precision)]
fn oklab(color: Rgba) -> (f32, f32, f32) {
    let (r, g, b) = (to_linear(color.r), to_linear(color.g), to_linear(color.b));
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    (
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    )
}

/// The linear-light sRGB channels of the Oklab color `(l, a, b)`, which fall in
/// [0, 1] exactly for a color sRGB can show.
#[allow(clippy::excessive_precision)]
fn oklab_to_linear(l: f32, a: f32, b: f32) -> [f32; 3] {
    let long = (l + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let medium = (l - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let short = (l - 0.0894841775 * a - 1.2914855480 * b).powi(3);
    [
        4.0767416621 * long - 3.3077115913 * medium + 0.2309699292 * short,
        -1.2684380046 * long + 2.6097574011 * medium - 0.3413193965 * short,
        -0.0041960863 * long - 0.7034186147 * medium + 1.7076147010 * short,
    ]
}

/// The contrast body text holds against the surface it is set on: the 4.5:1 that
/// WCAG 2.2 success criterion 1.4.3 Contrast (Minimum) asks for at level AA.
pub const MIN_TEXT_CONTRAST: f32 = 4.5;

/// The contrast held by what is seen rather than read — an icon, a rule, a focus
/// ring, a status dot: the 3:1 of WCAG 2.2 success criterion 1.4.11 Non-text
/// Contrast.
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
    if contrast_ratio(white, fill) > contrast_ratio(black, fill) {
        white
    } else {
        black
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Slack allowed on an [`Oklch`] coordinate compared across a conversion to
    /// sRGB and back, covering the rounding of the f32 arithmetic between them.
    const COORDINATE_SLACK: f32 = 2e-3;

    /// Slack allowed on an sRGB channel, one 8-bit level: the finest step a
    /// color is written and shown at.
    const CHANNEL_SLACK: f32 = 1.0 / 255.0;

    /// Colors spanning the wheel and the lightness range, each written the way a
    /// theme file writes one and each carrying a hue of its own.
    const HUED: [&str; 6] = [
        "#73ade9ff",
        "#6eb4bfff",
        "#bf956aff",
        "#a1c181ff",
        "#b477cfff",
        "#282c33ff",
    ];

    /// Colors carrying no hue, at both ends of the lightness range and between
    /// them, for which a lift has nothing to hold but the lightness.
    const NEUTRAL: [&str; 3] = ["#000000ff", "#ffffffff", "#7f7f7fff"];

    /// Every sample, hued or not.
    fn every_sample() -> Vec<&'static str> {
        HUED.into_iter().chain(NEUTRAL).collect()
    }

    /// Shares of the room a color has left toward black, negative, or toward
    /// white, positive, by which a sample is lifted a step that lands inside the
    /// lightness range whatever the sample.
    const SHARES: [f32; 5] = [-0.75, -0.25, 0.0, 0.25, 0.75];

    fn sample(hex: &str) -> Rgba {
        Rgba::try_from(hex).expect("a sample is a color")
    }

    /// Each of `hexes` lifted by every share, as the color it was and the color
    /// it became.
    fn lifts(hexes: Vec<&'static str>) -> impl Iterator<Item = (&'static str, f32, Oklch, Oklch)> {
        hexes.into_iter().flat_map(|hex| {
            let before = oklch(sample(hex));
            SHARES.into_iter().map(move |share| {
                let room = match share < 0.0 {
                    true => before.lightness,
                    false => 1.0 - before.lightness,
                };
                let step = share * room;
                (hex, step, before, oklch(lighten(sample(hex), step)))
            })
        })
    }

    /// The lift is what a caller asks for in lightness, so it is what the result
    /// has moved by.
    #[test]
    fn a_lift_moves_a_color_by_the_step_it_is_given() {
        for (hex, step, before, after) in lifts(every_sample()) {
            let want = before.lightness + step;
            assert!(
                (after.lightness - want).abs() < COORDINATE_SLACK,
                "{hex} by {step}: lightness {} of {want}",
                after.lightness
            );
        }
    }

    /// What a color is, as against how light it is, lives in its hue and chroma:
    /// a lift that spent either would hand back a paler, blanker color rather
    /// than the same one under a different light.
    #[test]
    fn a_lift_holds_the_hue_and_chroma_it_can() {
        for (hex, step, before, after) in lifts(HUED.to_vec()) {
            let turned = (after.hue - before.hue).abs();
            assert!(
                turned.min(1.0 - turned) < COORDINATE_SLACK,
                "{hex} by {step}: hue {} to {}",
                before.hue,
                after.hue
            );
            assert!(
                after.chroma <= before.chroma + COORDINATE_SLACK,
                "{hex} by {step}: chroma {} to {}",
                before.chroma,
                after.chroma
            );
        }
    }

    /// A lightness the sRGB gamut has no room for the whole of a color's chroma
    /// at is the one case a lift parts with any, and even there it parts with
    /// only what the gamut takes: what it lands on is the most saturated color
    /// that hue and lightness can be shown in.
    #[test]
    fn a_lift_parts_with_only_the_chroma_the_gamut_takes() {
        let vivid = sample("#0000ffff");
        let held = oklch(lighten(vivid, 0.25));
        assert!(
            held.chroma < oklch(vivid).chroma,
            "a lightened pure blue keeps all its chroma"
        );
        let angle = held.hue * TAU;
        let asking = held.chroma * (1.0 + 2.0 * COORDINATE_SLACK);
        let linear = oklab_to_linear(held.lightness, asking * angle.cos(), asking * angle.sin());
        assert!(
            !linear.iter().all(displayable),
            "there was chroma left to keep: {linear:?}"
        );
    }

    /// A step past the end of the lightness range leaves nothing of a color but
    /// the end it ran into, which is what a display can show there.
    #[test]
    fn a_lift_past_the_end_of_the_range_stops_at_it() {
        for hex in every_sample() {
            for (step, end) in [(1.0, 1.0), (-1.0, 0.0)] {
                let lifted = lighten(sample(hex), step);
                for channel in [lifted.r, lifted.g, lifted.b] {
                    assert!(
                        (channel - end).abs() < CHANNEL_SLACK,
                        "{hex} by {step}: {channel}"
                    );
                }
            }
        }
    }

    /// Alpha says how much of the surface below shows through, which is no part
    /// of what color the paint is, so a lift leaves it alone.
    #[test]
    fn a_lift_leaves_the_opacity_alone() {
        let translucent = Rgba {
            a: 0.4,
            ..sample("#73ade9ff")
        };
        assert_eq!(lighten(translucent, 0.2).a, translucent.a);
    }
}
