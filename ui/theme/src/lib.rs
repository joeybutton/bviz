//! Colors and appearance helpers shared across the UI crates. For now this wraps
//! gpui's built-in palette (which already follows the system light/dark
//! appearance); richer theming can grow here later.

pub use gpui::colors::Colors;
use bviz_trace_layout::{SpanRole, Workload};
use gpui::{Hsla, IntoElement, Rgba, Window, div, hsla, prelude::*, px};

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

/// Saturation of a counter series' translucent fill.
const CATEGORY_SATURATION: f32 = 0.5;

/// Alpha of a counter series' filled area.
const COUNTER_AREA_ALPHA: f32 = 0.5;

/// A translucent fill for a counter series, keyed by its name like a category.
pub fn counter_color(series: &str) -> Hsla {
    hsla(hue_of(series), CATEGORY_SATURATION, 0.55, COUNTER_AREA_ALPHA)
}

/// Hue (fraction of the wheel) per span workload, spread so the classes read as
/// distinct on either theme: the two payload workloads are warm (action orange,
/// starlark green) and the overhead workloads cool (filesystem cyan, remote/cache
/// blue, skyframe indigo, scheduling magenta), with gc red set apart. The Other
/// workload carries no hue — it is drawn as neutral grey.
const HUE_SUBPROCESS: f32 = 0.07;
const HUE_STARLARK: f32 = 0.34;
const HUE_FILESYSTEM: f32 = 0.50;
const HUE_REMOTE_CACHE: f32 = 0.60;
const HUE_SKYFRAME: f32 = 0.70;
const HUE_SCHEDULING: f32 = 0.83;
const HUE_GC: f32 = 0.99;

/// Payload fills are vivid (saturated, mid lightness) so real work draws the eye;
/// overhead fills are muted (desaturated, pulled toward the background) so Bazel
/// machinery recedes. Lightness is chosen per background.
const PAYLOAD_SATURATION: f32 = 0.72;
const PAYLOAD_LIGHTNESS_DARK: f32 = 0.60;
const PAYLOAD_LIGHTNESS_LIGHT: f32 = 0.46;
const OVERHEAD_SATURATION: f32 = 0.30;
const OVERHEAD_LIGHTNESS_DARK: f32 = 0.40;
const OVERHEAD_LIGHTNESS_LIGHT: f32 = 0.72;

/// Lightness of a neutral meta fill (the flame graph's synthetic root and any
/// meta-class frame), on a dark vs. a light background.
const FLAME_ROOT_LIGHTNESS_DARK: f32 = 0.30;
const FLAME_ROOT_LIGHTNESS_LIGHT: f32 = 0.78;

/// Side of a legend swatch and the gap between a swatch and its label.
const LEGEND_SWATCH: f32 = 10.0;
const LEGEND_LABEL_GAP: f32 = 4.0;

/// Fill for a span, keyed by its [`SpanRole`]: the workload gives the base hue and
/// the payload flag its weight, so the compiler and the Starlark that drives the
/// build read vivid while Bazel's own overhead (scheduling, caching, graph
/// bookkeeping, the sandbox) recedes. Both the flame chart and the flame graph
/// paint with it, so a role is the same color in both. The Other workload —
/// including the flame graph's synthetic root — is a neutral hueless grey.
pub fn span_role_color(role: SpanRole, background: Rgba) -> Hsla {
    let dark = luminance(background) < 0.5;
    if role.workload == Workload::Other {
        let lightness = if dark {
            FLAME_ROOT_LIGHTNESS_DARK
        } else {
            FLAME_ROOT_LIGHTNESS_LIGHT
        };
        return hsla(0.0, 0.0, lightness, 1.0);
    }
    let hue = match role.workload {
        Workload::Subprocess => HUE_SUBPROCESS,
        Workload::Starlark => HUE_STARLARK,
        Workload::Filesystem => HUE_FILESYSTEM,
        Workload::RemoteCache => HUE_REMOTE_CACHE,
        Workload::Skyframe => HUE_SKYFRAME,
        Workload::Scheduling => HUE_SCHEDULING,
        Workload::Gc => HUE_GC,
        Workload::Other => unreachable!("other handled above"),
    };
    let (saturation, lightness) = if role.payload {
        (
            PAYLOAD_SATURATION,
            if dark { PAYLOAD_LIGHTNESS_DARK } else { PAYLOAD_LIGHTNESS_LIGHT },
        )
    } else {
        (
            OVERHEAD_SATURATION,
            if dark { OVERHEAD_LIGHTNESS_DARK } else { OVERHEAD_LIGHTNESS_LIGHT },
        )
    };
    hsla(hue, saturation, lightness, 1.0)
}

/// A legend of the span-role colors: a swatch and name per workload in a row,
/// matching the fills the flame boxes use. Payload workloads lead, then overhead.
pub fn role_legend(label: Rgba, background: Rgba, text_size: f32) -> impl IntoElement {
    const ROLES: [(Workload, bool, &str); 8] = [
        (Workload::Subprocess, true, "action"),
        (Workload::Starlark, true, "starlark"),
        (Workload::Filesystem, false, "filesystem"),
        (Workload::RemoteCache, false, "remote/cache"),
        (Workload::Scheduling, false, "scheduling"),
        (Workload::Skyframe, false, "skyframe"),
        (Workload::Gc, false, "gc"),
        (Workload::Other, false, "other"),
    ];
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .text_color(label)
        .text_size(px(text_size))
        .children(ROLES.map(|(workload, payload, name)| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(LEGEND_LABEL_GAP))
                .child(
                    div()
                        .size(px(LEGEND_SWATCH))
                        .rounded_sm()
                        .bg(span_role_color(SpanRole { workload, payload }, background)),
                )
                .child(name)
        }))
}
