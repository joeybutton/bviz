//! The palettes the component tests measure against.

use bviz_ui_theme::{Appearance, ThemeColors, ThemeRegistry};

pub(crate) fn default_palettes() -> [ThemeColors; 2] {
    let registry = ThemeRegistry::new().expect("built-in themes load");
    [Appearance::Light, Appearance::Dark].map(|appearance| registry.resolve(appearance).colors)
}
