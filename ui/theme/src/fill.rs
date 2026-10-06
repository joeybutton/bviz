use crate::{Appearance, DataColors, Theme};
use gpui::Hsla;

/// How a fill departs from a data colour in one appearance.
#[derive(Clone, Copy)]
pub struct Shade {
    /// Added to the HSL lightness.
    pub lightness: f32,
    /// Multiplies the HSL saturation.
    pub saturation: f32,
}

impl Shade {
    pub const AS_IS: Shade = Shade::new(0.0, 1.0);

    pub const fn new(lightness: f32, saturation: f32) -> Shade {
        Shade {
            lightness,
            saturation,
        }
    }
}

/// A fill that is a data colour of the theme, stepped per appearance.
#[derive(Clone, Copy)]
pub struct DataFill {
    pub data: fn(&DataColors) -> Hsla,
    pub light: Shade,
    pub dark: Shade,
}

impl DataFill {
    pub fn resolve(&self, theme: &Theme) -> Hsla {
        let shade = match theme.appearance {
            Appearance::Light => self.light,
            Appearance::Dark => self.dark,
        };
        let base = (self.data)(&theme.data);
        Hsla {
            l: (base.l + shade.lightness).clamp(0.0, 1.0),
            s: (base.s * shade.saturation).clamp(0.0, 1.0),
            ..base
        }
    }
}
