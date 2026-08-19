//! Trace-specific color mapping: the roles the trace views paint with, and span
//! roles and counter series to fills, resolved from the shared palette.

use bviz_trace_layout::{SpanRole, Workload};
use bviz_ui_components::{LabelBoxStyle, ScrollbarColors, TooltipColors};
use bviz_ui_theme::{DataColors, Theme, ThemeColors, hue_of, mix, over};
use gpui::{Hsla, IntoElement, Rgba, div, hsla, prelude::*, px};

/// Saturation of a counter series' translucent fill.
const CATEGORY_SATURATION: f32 = 0.5;

/// Lightness of a counter series' translucent fill.
const CATEGORY_LIGHTNESS: f32 = 0.55;

/// Alpha of a counter series' filled area.
const COUNTER_AREA_ALPHA: f32 = 0.5;

/// A translucent fill for a counter series, keyed by its name like a category.
/// Counter series are unbounded, so they are spread over the whole wheel rather
/// than assigned one of the palette's slots.
pub(crate) fn counter_color(series: &str) -> Hsla {
    hsla(
        hue_of(series),
        CATEGORY_SATURATION,
        CATEGORY_LIGHTNESS,
        COUNTER_AREA_ALPHA,
    )
}

/// How far an overhead fill is pulled toward the canvas, so Bazel's own machinery
/// sits behind the payload it serves. Bounded on both sides: pulled too little,
/// an overhead fill is still taken for the vivid fill of the next slot round the
/// wheel; pulled to halfway or past it, the overhead fills have converged far
/// enough on the canvas for the closest pair of them to stop reading as two
/// colors.
const OVERHEAD_FADE: f32 = 0.4;

/// Side of a legend swatch and the gap between a swatch and its label.
const LEGEND_SWATCH: f32 = 10.0;
const LEGEND_LABEL_GAP: f32 = 4.0;

/// Leading of a legend label, and so the height of the legend's single row: a
/// wrapped entry starts below that row and is clipped away.
const LEGEND_LEADING: f32 = 1.6;

/// Every workload the reader is shown a name for, paired with the weight its
/// swatch is drawn in, payload first.
const LEGEND_ROLES: [(SpanRole, &str); 8] = [
    (SpanRole { workload: Workload::Subprocess, payload: true }, "action"),
    (SpanRole { workload: Workload::Starlark, payload: true }, "starlark"),
    (SpanRole { workload: Workload::Filesystem, payload: false }, "filesystem"),
    (SpanRole { workload: Workload::RemoteCache, payload: false }, "remote/cache"),
    (SpanRole { workload: Workload::Scheduling, payload: false }, "scheduling"),
    (SpanRole { workload: Workload::Skyframe, payload: false }, "skyframe"),
    (SpanRole { workload: Workload::Gc, payload: false }, "gc"),
    (SpanRole { workload: Workload::Other, payload: false }, "other"),
];

/// A legend of the span-role colors: a swatch and name per role in a row, painted
/// from the same fills the flame boxes take. Squeezed, it sheds whole entries off
/// its right end, so every swatch it shows keeps the size that keys it to the
/// fill.
pub(crate) fn role_legend(palette: &Palette, text_size: f32) -> impl IntoElement {
    let row = px(text_size * LEGEND_LEADING);
    div()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .content_start()
        .h(row)
        .line_height(row)
        .overflow_hidden()
        .gap_3()
        .text_color(palette.label)
        .text_size(px(text_size))
        .children(LEGEND_ROLES.map(|(role, name)| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .flex_shrink_0()
                .gap(px(LEGEND_LABEL_GAP))
                .child(
                    div()
                        .size(px(LEGEND_SWATCH))
                        .rounded_sm()
                        .bg(palette.role_fill(role)),
                )
                .child(name)
        }))
}

/// The colors the trace canvas paints with, owned so the `'static` paint closures
/// can capture them.
#[derive(Clone, Copy)]
pub(crate) struct Palette {
    pub(crate) border: Rgba,
    pub(crate) label: Rgba,
    /// Text on a control that is not the one in force, such as an unselected
    /// tab's label.
    pub(crate) muted_label: Rgba,
    pub(crate) background: Rgba,
    /// The sticky lane-header band fill, shared by the ruler band.
    pub(crate) header_band: Rgba,
    /// Faint vertical line dropped from each ruler tick through the lanes.
    pub(crate) gridline: Rgba,
    /// Accent for the build-phase boundary lines and their labels.
    pub(crate) marker: Rgba,
    pub(crate) scroll: ScrollbarColors,
    pub(crate) tooltip: TooltipColors,
    /// Fill for a row or control that marks the cursor by lifting off its surface.
    pub(crate) hover: Rgba,
    /// The line at a seam between two of the view's surfaces: the gutter's right
    /// edge and the tab bar's top.
    pub(crate) divider: Rgba,
    /// Opaque fill of a panel drawn over the chart.
    pub(crate) panel: Rgba,
    /// The hover crosshair line: a readout the cursor drags rather than a rule
    /// laid on the chart, so it ranks with secondary text and stands over the
    /// scenery it crosses.
    pub(crate) crosshair: Rgba,
    /// The edges and bars bounding what the reader has picked out.
    pub(crate) accent: Rgba,
    /// The wash over what the reader has picked out — a selected lane's gutter
    /// row, the ruler over a chosen time range.
    pub(crate) selection: Rgba,
    /// The slots the span workloads are colored from.
    data: DataColors,
    /// The theme's own roles, for handing to a shared component that resolves
    /// its colors from them; a trace view paints from this palette's roles
    /// rather than from these.
    pub(crate) colors: ThemeColors,
}

impl Palette {
    pub(crate) fn from_theme(theme: &Theme) -> Palette {
        let colors = &theme.colors;
        Palette {
            border: colors.border.into(),
            label: colors.text.into(),
            muted_label: colors.text_muted.into(),
            background: colors.background.into(),
            header_band: colors.surface.into(),
            gridline: colors.gridline.into(),
            marker: colors.highlight.into(),
            scroll: ScrollbarColors::from_colors(colors),
            tooltip: TooltipColors::from_colors(colors),
            hover: colors.element_hover.into(),
            divider: colors.border.into(),
            panel: colors.panel.into(),
            crosshair: colors.text_muted.into(),
            accent: colors.accent.into(),
            selection: colors.selection.into(),
            data: theme.data,
            colors: *colors,
        }
    }

    /// The fill under the cursor on a picked row of the background canvas, so
    /// that pointing at a picked row leaves it marked.
    pub(crate) fn picked_hover(&self) -> Rgba {
        over(self.hover, over(self.selection, self.background))
    }

    /// Fill for a span, keyed by its [`SpanRole`]: the workload picks the data
    /// slot and the payload flag its weight. Both the flame chart and the flame
    /// graph paint with it, so a role is one color in both. The Other workload —
    /// including the flame graph's synthetic root — carries no hue.
    pub(crate) fn role_fill(&self, role: SpanRole) -> Hsla {
        let slot = match role.workload {
            Workload::Subprocess => self.data.orange,
            Workload::Starlark => self.data.green,
            Workload::Filesystem => self.data.cyan,
            Workload::RemoteCache => self.data.blue,
            Workload::Skyframe => self.data.purple,
            Workload::Scheduling => self.data.yellow,
            Workload::Gc => self.data.red,
            Workload::Other => return self.data.neutral,
        };
        if role.payload {
            slot
        } else {
            mix(slot, self.background, OVERHEAD_FADE).into()
        }
    }

    /// The style a labeled box paints with.
    pub(crate) fn label_box(&self) -> LabelBoxStyle {
        LabelBoxStyle {
            background: self.background,
            seam_mix: crate::SLICE_SEAM_MIX,
            label_min_px: crate::LABEL_MIN_PX,
            text_pad: crate::SLICE_TEXT_PAD,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_ui_theme::{
        MIN_DISTINCT_COLOR, MIN_EDGED_STEP, MIN_VISIBLE_STEP, built_in_themes, distance,
        level_step,
    };

    /// The most a hover fill may clear the surface under it, as a multiple of
    /// [`MIN_VISIBLE_STEP`]: hover is momentary feedback, so lifted further it
    /// reads as a surface in its own right and starts competing with the text on
    /// the row.
    const HOVER_HEADROOM: f32 = 3.0;

    /// The workloads that carry a hue, each of which [`Palette::role_fill`] paints
    /// in a vivid and a faded weight. Other is left out: it takes the hueless slot
    /// whichever weight it is asked for.
    const HUED_WORKLOADS: [Workload; 7] = [
        Workload::Subprocess,
        Workload::Starlark,
        Workload::Filesystem,
        Workload::RemoteCache,
        Workload::Skyframe,
        Workload::Scheduling,
        Workload::Gc,
    ];

    /// Every theme's palette, for a rule that has to hold whichever one a window
    /// paints with.
    fn palettes() -> Vec<Palette> {
        built_in_themes().iter().map(|theme| Palette::from_theme(theme)).collect()
    }

    /// The surfaces the trace stacks its rows and controls on.
    fn surfaces(palette: &Palette) -> [Rgba; 3] {
        [palette.background, palette.header_band, palette.panel]
    }

    /// Every fill [`Palette::role_fill`] can return, each once.
    fn role_fills(palette: &Palette) -> Vec<Hsla> {
        let mut fills = Vec::with_capacity(2 * HUED_WORKLOADS.len() + 1);
        for workload in HUED_WORKLOADS {
            for payload in [true, false] {
                fills.push(palette.role_fill(SpanRole { workload, payload }));
            }
        }
        fills.push(palette.role_fill(SpanRole { workload: Workload::Other, payload: false }));
        fills
    }

    /// How far a fill stands off the canvas it is painted on.
    fn stand_off(palette: &Palette, role: SpanRole) -> f32 {
        distance(palette.role_fill(role), palette.background)
    }

    /// A hover marks the row the cursor is on and hands it back on the way out.
    #[test]
    fn the_hover_lifts_off_a_surface_without_becoming_one() {
        for palette in palettes() {
            for surface in surfaces(&palette) {
                let lift = level_step(over(palette.hover, surface), surface);
                assert!(lift <= HOVER_HEADROOM * MIN_VISIBLE_STEP, "{lift} levels");
            }
        }
    }

    /// A row under the cursor and a row the reader has picked out say two
    /// different things about the same strip of screen, so the hover has to be a
    /// different color from the selection rather than a weaker version of it.
    #[test]
    fn the_hover_is_told_from_the_selection() {
        for palette in palettes() {
            for surface in surfaces(&palette) {
                let hovered = over(palette.hover, surface);
                let apart = distance(hovered, over(palette.selection, surface));
                assert!(apart >= MIN_DISTINCT_COLOR, "{apart}");
            }
        }
    }

    /// The fill is the whole of what marks a picked row, so bringing the cursor
    /// onto one must neither wipe the mark nor leave the row reading like any
    /// other row under the cursor.
    #[test]
    fn a_picked_row_stays_marked_under_the_cursor() {
        for palette in palettes() {
            let canvas = palette.background;
            let hovered_pick = palette.picked_hover();
            let apart = distance(hovered_pick, over(palette.hover, canvas));
            assert!(apart >= MIN_DISTINCT_COLOR, "{apart}");
            let step = level_step(hovered_pick, over(palette.selection, canvas));
            assert!(step >= MIN_VISIBLE_STEP, "{step} levels");
        }
    }

    /// The gutter's right edge and the tab bar's top are drawn where the header
    /// band meets the chart canvas, so the line has to be seen against both of the
    /// surfaces it parts and not only against the one it is nearer in tone.
    #[test]
    fn the_divider_parts_the_two_surfaces_it_is_drawn_between() {
        for palette in palettes() {
            for surface in [palette.background, palette.header_band] {
                let step = level_step(over(palette.divider, surface), surface);
                assert!(step >= MIN_EDGED_STEP, "{step} levels");
            }
        }
    }

    /// A workload is read off the color of the box, so no two fills may be taken
    /// for each other and none may sink into the canvas the boxes are drawn on.
    #[test]
    fn every_span_role_is_told_from_the_others_and_from_the_canvas() {
        for palette in palettes() {
            let fills = role_fills(&palette);
            for (i, fill) in fills.iter().enumerate() {
                let over_canvas = distance(*fill, palette.background);
                assert!(over_canvas >= MIN_DISTINCT_COLOR, "{over_canvas}");
                for other in &fills[i + 1..] {
                    let apart = distance(*fill, *other);
                    assert!(apart >= MIN_DISTINCT_COLOR, "{apart}");
                }
            }
        }
    }

    /// The flame views rank the build's own work over Bazel's machinery, which
    /// they can only say by every payload fill standing further off the canvas
    /// than every overhead one, taking each hued workload in both its weights.
    /// The hueless Other fill ranks with neither: it marks the spans that carry
    /// no workload at all.
    #[test]
    fn a_payload_fill_stands_further_off_the_canvas_than_any_overhead_fill() {
        for palette in palettes() {
            let (mut payload, mut overhead) = (f32::INFINITY, 0.0f32);
            for workload in HUED_WORKLOADS {
                payload = payload.min(stand_off(&palette, SpanRole { workload, payload: true }));
                overhead =
                    overhead.max(stand_off(&palette, SpanRole { workload, payload: false }));
            }
            assert!(payload > overhead, "{payload} against {overhead}");
        }
    }
}
