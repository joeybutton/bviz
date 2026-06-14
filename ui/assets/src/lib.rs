//! Application assets — the Inter and JetBrains Mono fonts and the Phosphor icon
//! set — embedded into the binary at build time. The files are downloaded by
//! Bazel `http_archive`s and copied into `OUT_DIR` by `build.rs`, then baked in
//! with `include_bytes!`, so there is nothing to load from disk at runtime.

use gpui::{App, AssetSource, Result, SharedString, Svg, svg};
use std::borrow::Cow;

/// UI font family.
pub const UI: &str = "Inter";
/// Monospace font family.
pub const MONO: &str = "JetBrains Mono";

macro_rules! embedded {
    ($name:literal) => {
        include_bytes!(concat!(env!("OUT_DIR"), "/", $name))
    };
}

const FONTS: &[&[u8]] = &[
    embedded!("Inter-Regular.ttf"),
    embedded!("Inter-Bold.ttf"),
    embedded!("Inter-Italic.ttf"),
    embedded!("Inter-BoldItalic.ttf"),
    embedded!("JetBrainsMono-Regular.ttf"),
    embedded!("JetBrainsMono-Bold.ttf"),
    embedded!("JetBrainsMono-Italic.ttf"),
    embedded!("JetBrainsMono-BoldItalic.ttf"),
];

/// Register the embedded fonts with gpui's text system.
pub fn load_fonts(cx: &App) {
    let fonts = FONTS.iter().map(|bytes| Cow::Borrowed(*bytes)).collect();
    cx.text_system().add_fonts(fonts).expect("register fonts");
}

fn icon_svg(file: &str) -> Option<&'static [u8]> {
    Some(match file {
        "circle-notch.svg" => embedded!("circle-notch.svg"),
        "minus.svg" => embedded!("minus.svg"),
        "square.svg" => embedded!("square.svg"),
        "x.svg" => embedded!("x.svg"),
        _ => return None,
    })
}

/// Serves the embedded Phosphor icons to gpui's `svg()` element.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let file = path.rsplit('/').next().unwrap_or(path);
        Ok(icon_svg(file).map(Cow::Borrowed))
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

/// A regular-weight Phosphor icon, tinted by the element's text color.
pub fn icon(name: &str) -> Svg {
    svg().path(format!("assets/regular/{name}.svg"))
}
