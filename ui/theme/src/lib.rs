//! Colors and appearance helpers shared across the UI crates. For now this wraps
//! gpui's built-in palette (which already follows the system light/dark
//! appearance); richer theming can grow here later.

pub use gpui::colors::Colors;
use gpui::{Rgba, Window};

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
