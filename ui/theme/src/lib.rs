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
