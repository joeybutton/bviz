use super::Timeline;
use super::input::{TimelineDrag, timeline_end_drag, timeline_on_drag};
use super::selection::{EDGE_GRAB_PX, selection_pixels};
use super::slices::{DrawnSlices, cached_slices};
use super::view::{framing, held_zoom};
use crate::counters::{StripCounters, counter_readouts, paint_crosshair, track_count};
use crate::groups::{ChartLanes, GroupChrome, chart_rows};
use crate::regions::{CountersStrip, LaneBand, Regions};
use crate::theme::Palette;
use crate::{
    EDGE_MARGIN_PX, GROUP_BAND_HEIGHT, METRICS, paint_gridlines, paint_gutter, paint_marker_band,
    paint_markers, paint_ruler, paint_slice, ruler_ticks, scrollbar_geom,
};
use bviz_trace_layout::{TimeSelection, TimeView, VisibleSlice};
use bviz_trace_profile::{Profile, SliceAt};
use bviz_ui_components::track_drag;
use gpui::{
    App, Bounds, ContentMask, Context, Entity, IntoElement, Pixels, Render, Rgba, WeakEntity,
    Window, canvas, div, fill, point, prelude::*, px, size,
};
use std::rc::Rc;
use std::sync::Arc;

/// The layer painting the timeline's slices and chrome, from the view state,
/// document, and lanes it reads through `parent`.
pub(super) struct SliceCanvas {
    pub(super) parent: WeakEntity<Timeline>,
}

impl Render for SliceCanvas {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(parent) = self.parent.upgrade() else {
            return div().into_any_element();
        };
        let (profile, lanes, chrome, counters, gutter_width) = {
            let timeline = parent.read(cx);
            let model = timeline.model.read(cx);
            let Some(profile) = model.profile() else {
                return div().into_any_element();
            };
            let (Some(lanes), Some(chrome)) = (model.chart_lanes(), model.group_chrome()) else {
                return div().into_any_element();
            };
            let (counters, gutter_width) = timeline.shell_layout(cx);
            (profile.clone(), lanes, chrome, counters, gutter_width)
        };
        let palette = Palette::from_theme(&bviz_ui_theme::theme(window, cx));
        let prepaint = fit_prepaint(
            profile.clone(),
            lanes.clone(),
            parent.clone(),
            counters,
            gutter_width,
        );
        let paint = slice_paint(profile, lanes, chrome, parent, palette);
        canvas(prepaint, paint)
            .absolute()
            .inset_0()
            .size_full()
            .into_any_element()
    }
}

/// Prepaint fit: capture the painted viewport bounds, then fit the time view to
/// the span before the first paint and, on a later width change, rescale the zoom
/// relative to the window so a resize stays invertible; finally hold the zoom
/// within its range and clamp the pan to the span.
fn fit_prepaint(
    profile: Arc<Profile>,
    lanes: Rc<ChartLanes>,
    parent: Entity<Timeline>,
    counters: CountersStrip,
    gutter_width: f32,
) -> impl FnOnce(Bounds<Pixels>, &mut Window, &mut App) {
    move |bounds: Bounds<Pixels>, _: &mut Window, cx: &mut App| {
        let old_bounds = parent.read(cx).viewport;
        let heights = lanes.heights();
        let regions_of =
            |b| Regions::new(b, track_count(&profile), counters, gutter_width, heights);
        let regions = regions_of(bounds);
        let time_w = regions.time_w;
        let old_time_w = regions_of(old_bounds).time_w;
        let span = profile.span();
        parent.update(cx, |timeline, _| {
            timeline.viewport = bounds;
            if !timeline.fitted {
                timeline.view = match timeline.framed_on_fit.take() {
                    Some(time) => framing(time, span, time_w),
                    None => TimeView::fit(span, time_w, EDGE_MARGIN_PX),
                };
                timeline.fitted = true;
            } else if old_bounds.size.width > px(0.) && (old_time_w - time_w).abs() > f32::EPSILON {
                timeline
                    .view
                    .rescale_to_width(span, old_time_w, time_w, EDGE_MARGIN_PX);
            }
            timeline.view.px_per_us = held_zoom(timeline.view.px_per_us, span, time_w);
            timeline.view.clamp_to(span, time_w, EDGE_MARGIN_PX);
        });
    }
}

/// Width of the line around each slice standing for what is chosen of the
/// critical path, drawn outside the slice so its fill and label stay whole.
const CHOSEN_OUTLINE_PX: f32 = 2.0;

/// Outlines the slices standing for what is chosen of the critical path that
/// `drawn` holds, each within the band of `regions` it is drawn in, the lanes
/// scrolled up by `scroll`.
fn paint_chosen_slices(
    chosen: &[SliceAt],
    drawn: &DrawnSlices,
    regions: Regions,
    scroll: f32,
    palette: &Palette,
    window: &mut Window,
) {
    for (band, slices) in drawn.in_bands(regions, scroll) {
        let bounds = band.rect(regions.content_left, regions.time_right);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            paint_outlines(chosen, slices, band, regions, palette, window);
        });
    }
}

/// Outlines the slices of `slices`, drawn in `band`, that `chosen` holds.
fn paint_outlines(
    chosen: &[SliceAt],
    slices: &[VisibleSlice],
    band: LaneBand,
    regions: Regions,
    palette: &Palette,
    window: &mut Window,
) {
    for v in slices {
        if !chosen
            .iter()
            .any(|at| at.track() == v.track && at.slice() == v.slice)
        {
            continue;
        }
        let around = Bounds::new(
            point(
                px(regions.content_left + v.x - CHOSEN_OUTLINE_PX),
                px(band.top + v.y - CHOSEN_OUTLINE_PX),
            ),
            size(
                px(v.w + 2.0 * CHOSEN_OUTLINE_PX),
                px(v.h + 2.0 * CHOSEN_OUTLINE_PX),
            ),
        );
        window.paint_quad(
            gpui::outline(around, palette.label, gpui::BorderStyle::Solid)
                .border_widths(px(CHOSEN_OUTLINE_PX)),
        );
    }
}

/// The slice layer's paint closure: the flame timeline — gridlines, lanes and
/// slices, phase markers, gutter, scroll bars, ruler, and the counter plots —
/// reading the live view state through `parent`.
fn slice_paint(
    profile: Arc<Profile>,
    lanes: Rc<ChartLanes>,
    chrome: Rc<GroupChrome>,
    parent: Entity<Timeline>,
    palette: Palette,
) -> impl FnOnce(Bounds<Pixels>, (), &mut Window, &mut App) {
    move |bounds, _, window, cx| {
        let n_counters = track_count(&profile);
        let (
            tv,
            scroll,
            counter_scroll,
            gutter_width,
            selected,
            shared_names,
            regions,
            show_counters,
        ) = {
            let timeline = parent.read(cx);
            let tv = timeline.view;
            let scroll = timeline.lane_scroll(cx);
            let counter_scroll = timeline.counter_scroll(cx);
            let (counters, gutter_width) = timeline.shell_layout(cx);
            let selected = timeline.model.read(cx).selected_tracks().to_vec();
            let shared_names = timeline.model.read(cx).shared_track_names().to_vec();
            let regions = Regions::new(bounds, n_counters, counters, gutter_width, lanes.heights());
            let show_counters = regions.shows_counters();
            (
                tv,
                scroll,
                counter_scroll,
                gutter_width,
                selected,
                shared_names,
                regions,
                show_counters,
            )
        };
        let pinned = regions.pinned_band();
        let slices = parent.update(cx, |timeline, _| {
            cached_slices(
                &mut timeline.slice_draw,
                &profile,
                &lanes,
                tv,
                pinned.viewport(regions.time_w),
                regions.scrolled_band(scroll).viewport(regions.time_w),
            )
        });

        let ticks = ruler_ticks(tv, regions.time_w);
        paint_gridlines(
            &ticks,
            regions.content_left,
            regions.flame_top,
            regions.flame_bottom,
            &palette,
            window,
        );
        if show_counters {
            paint_gridlines(
                &ticks,
                regions.content_left,
                regions.counters_top,
                regions.counters_top + regions.counters_h(),
                &palette,
                window,
            );
        }

        let stacks = [(&lanes.pinned, None), (&lanes.scrolled, Some(&*chrome))];
        for ((band, drawn), (layout, headed)) in
            slices.in_bands(regions, scroll).into_iter().zip(stacks)
        {
            let bounds = band.rect(regions.content_left, regions.time_right);
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                for lane in layout.lanes.iter().skip(1) {
                    let ry = band.y_of(lane.top) - METRICS.lane_gap / 2.0;
                    window.paint_quad(fill(
                        Bounds::new(
                            point(px(regions.content_left), px(ry)),
                            size(px(regions.time_w), px(1.0)),
                        ),
                        palette.gridline,
                    ));
                }
                if let Some(chrome) = headed {
                    for header in chart_rows(chrome).filter(|row| row.kind.is_header()) {
                        window.paint_quad(fill(
                            Bounds::new(
                                point(px(regions.content_left), px(band.y_of(header.top))),
                                size(px(regions.time_w), px(GROUP_BAND_HEIGHT)),
                            ),
                            palette.header_band,
                        ));
                    }
                }
                for vs in drawn {
                    let track = &profile.threads()[vs.track];
                    let slice = profile.slices(track)[vs.slice];
                    paint_slice(
                        &profile,
                        &slice,
                        vs.x,
                        vs.y + band.top,
                        vs.w,
                        vs.h,
                        regions.content_left,
                        &palette,
                        window,
                        cx,
                    );
                }
            });
        }

        if show_counters {
            let working_lanes_pending = {
                let working_lanes = parent.read(cx).model.read(cx).working_lanes();
                let strip = StripCounters::new(&profile, working_lanes);
                crate::counters::paint_graph(strip, tv, regions, counter_scroll, &palette, window);
                working_lanes.is_none()
            };
            if working_lanes_pending {
                crate::counters::paint_working_lanes_pending(
                    regions,
                    counter_scroll,
                    &palette,
                    window,
                    cx,
                );
            }
        }
        paint_markers(&profile, tv, regions, show_counters, &palette, window);
        scrollbar_geom(
            profile.span(),
            tv,
            lanes.scrolled.total_height,
            regions,
            scroll,
            counter_scroll,
        )
        .paint(&palette, window);

        paint_gutter(
            &profile,
            &lanes,
            &chrome,
            regions,
            gutter_width,
            scroll,
            counter_scroll,
            show_counters,
            &selected,
            &shared_names,
            &palette,
            window,
            cx,
        );

        if pinned.height > 0.0 {
            let gutter_left = regions.content_left - gutter_width;
            window.paint_quad(fill(
                Bounds::new(
                    point(
                        px(gutter_left),
                        px(pinned.bottom() - METRICS.lane_gap / 2.0),
                    ),
                    size(px(regions.time_right - gutter_left), px(1.0)),
                ),
                palette.divider,
            ));
        }

        paint_ruler(&ticks, regions, bounds, &palette, window, cx);
        paint_marker_band(&profile, tv, regions, &palette, window, cx);
    }
}

/// Alpha of the wash laid over unselected time: heavy enough that the selection
/// reads at a glance, light enough that the chart under it stays legible.
const VEIL_ALPHA: f32 = 0.62;

/// Alpha of the selection accent tinting a selected region's stretch of the
/// ruler band. The ruler's ticks and time labels are painted beneath it, so the
/// tint stays translucent enough to read them through while still marking the
/// stretch the regions are dragged in.
const RULER_BAND_ALPHA: f32 = 0.5;

/// Width of a region edge's hairline.
const EDGE_LINE_PX: f32 = 1.0;

/// Width of an edge's grip, spanning exactly the horizontal zone
/// [`EDGE_GRAB_PX`] gives a press either side of the edge to take hold of it.
const GRIP_WIDTH: f32 = 2.0 * EDGE_GRAB_PX;

/// Share of the ruler band's height a grip stands in, centred, so it reads as a
/// handle rather than a bar filling the band.
const GRIP_BAND_FRACTION: f32 = 0.55;

/// Paints `sel` over the timeline: a wash over the time outside it, an accent
/// fill and edge grips in the ruler band, and a hairline down each region edge.
/// Covers the time-axis sections the crosshair does, so a selection reads across
/// the lanes and the counter plots alike.
fn paint_time_selection(
    sel: &TimeSelection,
    tv: TimeView,
    regions: Regions,
    show_counters: bool,
    palette: &Palette,
    window: &mut Window,
) {
    if sel.is_empty() {
        return;
    }
    let painted = selection_pixels(sel, tv, regions);
    let quad = |x: f32, w: f32, top: f32, h: f32, color: Rgba, window: &mut Window| {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        window.paint_quad(fill(
            Bounds::new(point(px(x), px(top)), size(px(w), px(h))),
            color,
        ));
    };

    let veil = Rgba {
        a: VEIL_ALPHA,
        ..palette.background
    };
    let veil_span = |from: f32, to: f32, window: &mut Window| {
        quad(
            from,
            to - from,
            regions.flame_top,
            regions.flame_h(),
            veil,
            window,
        );
        if show_counters {
            quad(
                from,
                to - from,
                regions.counters_top,
                regions.counters_h(),
                veil,
                window,
            );
        }
    };
    let mut unselected_from = regions.content_left;
    for r in &painted {
        veil_span(unselected_from, r.left, window);
        unselected_from = r.right;
    }
    veil_span(unselected_from, regions.time_right, window);

    let band = regions.ruler_rect();
    let band_top = f32::from(band.origin.y);
    let band_h = f32::from(band.size.height);
    let tint = Rgba {
        a: RULER_BAND_ALPHA,
        ..palette.accent
    };
    for r in &painted {
        quad(r.left, r.right - r.left, band_top, band_h, tint, window);
    }

    let accent = palette.accent;
    let grip_h = band_h * GRIP_BAND_FRACTION;
    let grip_top = band_top + (band_h - grip_h) / 2.0;
    for r in &painted {
        // An end edge's line is inset by its own width, so both hairlines fall
        // inside the region they bound.
        for (on_axis, edge_x, line_inset) in
            [(r.starts, r.left, 0.0), (r.ends, r.right, EDGE_LINE_PX)]
        {
            if !on_axis {
                continue;
            }
            let line_x = edge_x - line_inset;
            quad(
                line_x,
                EDGE_LINE_PX,
                band_top,
                regions.flame_bottom - band_top,
                accent,
                window,
            );
            if show_counters {
                quad(
                    line_x,
                    EDGE_LINE_PX,
                    regions.counters_top,
                    regions.counters_h(),
                    accent,
                    window,
                );
            }
            quad(
                edge_x - EDGE_GRAB_PX,
                GRIP_WIDTH,
                grip_top,
                grip_h,
                accent,
                window,
            );
        }
    }
}

/// Width of the playhead's rule, broader than the crosshair's hairline so the
/// two read apart where they cross.
const PLAYHEAD_WIDTH_PX: f32 = 2.0;

/// Paints the playback time, profile instant `at_us`, as a rule down the
/// time-axis sections the crosshair covers, where it falls within the shown
/// time.
fn paint_playhead(
    at_us: i64,
    tv: TimeView,
    regions: Regions,
    show_counters: bool,
    palette: &Palette,
    window: &mut Window,
) {
    let x = regions.content_left + tv.x_of(at_us);
    if !(regions.content_left..=regions.time_right).contains(&x) {
        return;
    }
    let rule = |top: f32, height: f32, window: &mut Window| {
        window.paint_quad(fill(
            Bounds::new(
                point(px(x - PLAYHEAD_WIDTH_PX / 2.0), px(top)),
                size(px(PLAYHEAD_WIDTH_PX), px(height)),
            ),
            palette.playhead,
        ));
    };
    rule(regions.flame_top, regions.flame_h(), window);
    if show_counters {
        rule(regions.counters_top, regions.counters_h(), window);
    }
}

/// The overlay's paint closure: the time selection, the chosen step's
/// outlines, the playhead, the crosshair, the hover tooltip, and — while a drag
/// runs — the window-global drag handlers, from the selection, choice, clock,
/// crosshair, hover, and drag state it reads through `parent`.
pub(super) fn overlay_paint(
    profile: Arc<Profile>,
    lanes: Rc<ChartLanes>,
    parent: Entity<Timeline>,
    palette: Palette,
) -> impl FnOnce(Bounds<Pixels>, &mut Window, &mut App) {
    move |bounds, window, cx| {
        let n_counters = track_count(&profile);
        let chosen_drawn = {
            let timeline = parent.read(cx);
            let chosen: Vec<SliceAt> = timeline
                .model
                .read(cx)
                .chosen(cx)
                .map(|(_, item)| item.slices())
                .unwrap_or_default();
            match (&timeline.slice_draw, chosen.is_empty()) {
                (Some(cache), false) => Some((chosen, cache.slices.clone())),
                _ => None,
            }
        };
        let (
            tv,
            counters,
            scroll,
            counter_scroll,
            gutter_width,
            selection,
            playhead,
            crosshair,
            dragging,
        ) = {
            let timeline = parent.read(cx);
            let tv = timeline.view;
            let scroll = timeline.lane_scroll(cx);
            let counter_scroll = timeline.counter_scroll(cx);
            let (counters, gutter_width) = timeline.shell_layout(cx);
            let model = timeline.model.read(cx);
            let selection = timeline.drag.preview(model.time_selection());
            (
                tv,
                counters,
                scroll,
                counter_scroll,
                gutter_width,
                selection,
                model.playhead_us(cx),
                timeline.crosshair,
                !matches!(timeline.drag, TimelineDrag::None),
            )
        };
        let regions = Regions::new(bounds, n_counters, counters, gutter_width, lanes.heights());
        let show_counters = regions.shows_counters();
        paint_time_selection(&selection, tv, regions, show_counters, &palette, window);
        if let Some((chosen, drawn)) = chosen_drawn {
            paint_chosen_slices(&chosen, &drawn, regions, scroll, &palette, window);
        }
        if let Some(at_us) = playhead {
            paint_playhead(at_us, tv, regions, show_counters, &palette, window);
        }
        if let Some(at) = crosshair
            && f32::from(at.x) >= regions.content_left
        {
            let readouts = match show_counters {
                true => {
                    let working_lanes = parent.read(cx).model.read(cx).working_lanes();
                    let strip = StripCounters::new(&profile, working_lanes);
                    counter_readouts(strip, tv, regions, at, counter_scroll)
                }
                false => Vec::new(),
            };
            paint_crosshair(
                &readouts,
                tv,
                regions,
                at,
                show_counters,
                &palette,
                window,
                cx,
            );
        }
        if dragging {
            track_drag(window, &parent, timeline_on_drag, timeline_end_drag);
        }
    }
}
