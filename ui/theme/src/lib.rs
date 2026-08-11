//! Colors and appearance helpers shared across the UI crates. For now this wraps
//! gpui's built-in palette (which already follows the system light/dark
//! appearance); richer theming can grow here later.

pub use gpui::colors::Colors;
use gpui::{hsla, Hsla, Rgba, Window};

/// The palette for the window's current appearance (light or dark).
pub fn colors(window: &Window) -> Colors {
    Colors::for_appearance(window)
}

/// Linearly blend `a` toward `b` by `t` in [0, 1].
pub fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    Rgba {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

/// Perceptual luminance of `c`, used to choose contrasting text and theme-fitting
/// lightness.
pub fn luminance(c: Rgba) -> f32 {
    0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b
}

/// The perceptual gap between two fills, in 8-bit sRGB levels.
pub fn level_step(a: Rgba, b: Rgba) -> f32 {
    255.0 * (luminance(a) - luminance(b)).abs()
}

/// The smallest step, in 8-bit sRGB levels, at which one neutral fill still reads
/// as separate from another it lies on.
pub const MIN_VISIBLE_STEP: f32 = 12.0;

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

/// `color` at full opacity, for legible text tinted like its translucent dot.
pub fn opaque(color: Hsla) -> Hsla {
    Hsla { a: 1.0, ..color }
}

/// Black or white, whichever reads on `fill`.
pub fn label_on(fill: Hsla) -> Hsla {
    let rgba = Rgba::from(fill);
    if luminance(rgba) < 0.5 {
        hsla(0.0, 0.0, 1.0, 1.0)
    } else {
        hsla(0.0, 0.0, 0.0, 1.0)
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
