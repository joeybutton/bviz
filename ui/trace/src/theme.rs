//! Trace-specific color mapping: span roles and counter series to fills, built on
//! the shared palette helpers in [`bviz_ui_theme`].

use bviz_trace_layout::{SpanRole, Workload};
use bviz_ui_theme::{hue_of, luminance};
use gpui::{div, hsla, prelude::*, px, Hsla, IntoElement, Rgba};

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
