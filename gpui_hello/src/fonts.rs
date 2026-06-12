//! Bundled application fonts, downloaded by Bazel and read from runfiles at
//! startup. Inter is the UI face and JetBrains Mono the monospace face; glyphs
//! missing from both fall back to the system fonts gpui already has loaded.

use gpui::App;
use runfiles::{Runfiles, rlocation};

/// Family name for UI text.
pub const UI: &str = "Inter";
/// Family name for monospace text.
pub const MONO: &str = "JetBrains Mono";

const FILES: &[&str] = &[
    "inter/extras/ttf/Inter-Regular.ttf",
    "inter/extras/ttf/Inter-Bold.ttf",
    "inter/extras/ttf/Inter-Italic.ttf",
    "inter/extras/ttf/Inter-BoldItalic.ttf",
    "jetbrains_mono/fonts/ttf/JetBrainsMono-Regular.ttf",
    "jetbrains_mono/fonts/ttf/JetBrainsMono-Bold.ttf",
    "jetbrains_mono/fonts/ttf/JetBrainsMono-Italic.ttf",
    "jetbrains_mono/fonts/ttf/JetBrainsMono-BoldItalic.ttf",
];

/// Register the bundled fonts with gpui's text system.
pub fn load(cx: &App) {
    let runfiles = Runfiles::create().expect("locate runfiles");
    let data = FILES
        .iter()
        .map(|path| {
            let resolved =
                rlocation!(runfiles, path).unwrap_or_else(|| panic!("missing font runfile: {path}"));
            std::fs::read(&resolved)
                .unwrap_or_else(|e| panic!("read {path}: {e}"))
                .into()
        })
        .collect();
    cx.text_system().add_fonts(data).expect("register fonts");
}
