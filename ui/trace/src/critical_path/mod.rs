//! Where the wall time of the profile's build went along its critical path.

mod by_height;
mod fills;
mod words;

use crate::chart::format_duration;
use crate::model::{CriticalPathDerived, SourceReplaced, TraceModel, TracedPath};
use crate::stats_panel::table_cell;
use crate::theme::Palette;
use crate::{
    ChooseNextStep, ChoosePreviousStep, STATS_HEADER_HEIGHT, STATS_NUM_COL_WIDTH, STATS_ROW_HEIGHT,
    STATS_TEXT_SIZE, UnchooseStep, profile_status,
};
use bviz_critical_path::{Coverage, CriticalPath, Part, Phase, Spawned, Step};
use bviz_ui_components::{
    ListScroll, SCROLLBAR_THICKNESS, TextTooltip, centered_notice, centered_spinner, list_scrollbar,
};
use bviz_ui_workspace::{Panel, PanelEvent, PanelKind, ViewKind};
use by_height::ByHeight;
use fills::{Fills, outside};
use gpui::{
    App, Context, Div, ElementId, Entity, EventEmitter, FocusHandle, Focusable, Hsla, IntoElement,
    MouseButton, MouseDownEvent, Pixels, Render, ScrollStrategy, SharedString, Stateful,
    Subscription, Window, div, prelude::*, px, relative, uniform_list,
};
use std::time::Duration;

pub(crate) const KEY_CONTEXT: &str = "TraceCriticalPath";

const BAR_HEIGHT: Pixels = px(12.);
const PATH_BAR_HEIGHT: Pixels = px(14.);
/// The steps the table keeps in view before the summary's bars give up room
/// for more of them.
const STEPS_BEFORE_BARS: f32 = 5.0;
/// How much faster the summary's bars give up their height than the table
/// does, so that in a short pane the bars scroll away before the table falls
/// below its few steps.
const BARS_YIELD: f32 = 1000.0;
/// How much faster the table gives up its height than the headline and
/// levers do, so that they clip only once the table is down to a single
/// step. No yield is below one: flex items whose shrink factors sum below
/// one take only that share of the overflow, and leave the rest of it
/// overflowing.
const TABLE_YIELD: f32 = 1000.0;
/// The panel height under which the summary leaves out its legends, whose
/// entries the bars' segments repeat in their tooltips, and the headline
/// and levers take less room.
const LEGENDS_MIN_HEIGHT: Pixels = px(400.);
/// How many levers the summary names.
const LEVERS_SHOWN: usize = 3;
/// A step's bar of parts where a strip of spawn phases may stand beneath it.
const TIER_HEIGHT: Pixels = px(8.);
const TIER_GAP: Pixels = px(2.);
/// A strip of spawn phases.
const STRIP_HEIGHT: Pixels = px(6.);
/// The canvas showing between two segments of a bar, so neighbouring parts
/// part by more than their colours.
const SEGMENT_GAP: Pixels = px(2.);
/// The same between the phases of a step's strip, narrower so that the many
/// phases of a short step still leave room for their fills.
const STRIP_GAP: Pixels = px(1.);
/// The narrowest a step's bar is drawn, so even the shortest step's bar can
/// be hovered for its parts.
const MIN_BAR_WIDTH: Pixels = px(4.);
const SWATCH: Pixels = px(10.);
/// Width of the column holding a step's share of the path.
const SHARE_WIDTH: f32 = 72.0;
/// Width of the column holding a step's bar of parts.
const BAR_COLUMN_WIDTH: f32 = 200.0;
/// The narrowest a phase is drawn in a step's strip, in pixels; narrower
/// phases are folded into one remainder, whose fill and tooltip stand for
/// them together.
const MIN_STRIP_PHASE_WIDTH: f32 = 2.0;

/// One column of the steps table: its title and its fixed width, `None` for
/// the one flexible column.
struct Column {
    title: &'static str,
    width: Option<f32>,
}

const COLUMNS: [Column; 6] = [
    Column {
        title: "Step",
        width: None,
    },
    Column {
        title: "Start",
        width: Some(STATS_NUM_COL_WIDTH),
    },
    Column {
        title: "Waited",
        width: Some(STATS_NUM_COL_WIDTH),
    },
    Column {
        title: "Own",
        width: Some(STATS_NUM_COL_WIDTH),
    },
    Column {
        title: "% of path",
        width: Some(SHARE_WIDTH),
    },
    Column {
        title: "Wait and own time",
        width: Some(BAR_COLUMN_WIDTH),
    },
];

/// The critical path of the profile: the wall time partitioned along it, an
/// account of where the time went, and its steps in the order they started.
pub struct CriticalPathPanel {
    focus: FocusHandle,
    model: Entity<TraceModel>,
    scroll: ListScroll,
    summary_scroll: ListScroll,
    /// Shows the document's load state as it changes.
    _model_observer: Subscription,
    _derived: Subscription,
    /// Returns the steps to the first for the document replacing the shown one.
    _source_replaced: Subscription,
}

impl CriticalPathPanel {
    pub(crate) fn new(model: Entity<TraceModel>, cx: &mut Context<Self>) -> Self {
        let model_observer = cx.observe(&model, |_, _, cx| cx.notify());
        let derived = cx.subscribe(&model, |_, _, _: &CriticalPathDerived, cx| cx.notify());
        let source_replaced = cx.subscribe(&model, |panel, _, _: &SourceReplaced, cx| {
            panel.scroll.to_top();
            panel.summary_scroll.to_top();
            cx.notify();
        });
        Self {
            focus: cx.focus_handle(),
            model,
            scroll: ListScroll::new(),
            summary_scroll: ListScroll::new(),
            _model_observer: model_observer,
            _derived: derived,
            _source_replaced: source_replaced,
        }
    }

    /// Chooses step `index`, keeping its row in view.
    fn choose(&mut self, index: usize, cx: &mut Context<Self>) {
        self.model
            .update(cx, |model, cx| model.choose_step(index, cx));
        self.scroll
            .uniform_handle()
            .scroll_to_item(index, ScrollStrategy::Nearest);
        cx.notify();
    }

    /// Chooses the step `by` rows from the chosen one, or the first where
    /// none is chosen, stopping at either end of the table.
    fn choose_by(&mut self, by: isize, cx: &mut Context<Self>) {
        let model = self.model.read(cx);
        let steps = match model.critical_path() {
            Some(TracedPath::Traced {
                path: Some(path), ..
            }) => path.steps().len(),
            _ => return,
        };
        let Some(last) = steps.checked_sub(1) else {
            return;
        };
        let index = match model.chosen_step(cx) {
            Some((chosen, _)) => chosen.saturating_add_signed(by).min(last),
            None => 0,
        };
        self.choose(index, cx);
    }

    /// The path in sum over its table of steps.
    fn body(
        &self,
        path: &CriticalPath,
        palette: &Palette,
        fills: &Fills,
        cx: &Context<Self>,
    ) -> ByHeight {
        let body = |legends: bool| {
            div()
                .flex()
                .flex_col()
                .size_full()
                .child(lead(path, legends, palette))
                .child(self.summary(path, legends, palette, fills, cx))
                .child(header_row(palette))
                .child(self.steps(path, palette, fills, cx))
                .into_any_element()
        };
        ByHeight::new(LEGENDS_MIN_HEIGHT, body(true), body(false))
    }

    /// The path's bars, with their `legends` or without, scrolling where they
    /// would leave the steps less than their room.
    fn summary(
        &self,
        path: &CriticalPath,
        legends: bool,
        palette: &Palette,
        fills: &Fills,
        cx: &Context<Self>,
    ) -> Div {
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_shrink(BARS_YIELD * TABLE_YIELD)
            .min_h_0()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|panel, e: &MouseDownEvent, _, cx| {
                    if panel.summary_scroll.press(e.position) {
                        cx.stop_propagation();
                    }
                    cx.notify();
                }),
            )
            .child(
                div()
                    .id("critical-path-summary")
                    .flex_1()
                    .min_h_0()
                    .pr(px(SCROLLBAR_THICKNESS))
                    .overflow_y_scroll()
                    .track_scroll(self.summary_scroll.handle())
                    .child(summary(path, legends, palette, fills)),
            )
            .child(list_scrollbar(
                cx.entity(),
                |panel: &CriticalPathPanel| &panel.summary_scroll,
                |panel: &mut CriticalPathPanel| &mut panel.summary_scroll,
                palette.scroll,
            ))
    }

    /// The steps, one row each, every bar as long against the longest as the
    /// step's wait and own time are.
    fn steps(
        &self,
        path: &CriticalPath,
        palette: &Palette,
        fills: &Fills,
        cx: &Context<Self>,
    ) -> Div {
        let entity = cx.entity();
        let longest = path
            .steps()
            .iter()
            .map(|step| step.gap() + step.own())
            .max()
            .unwrap_or_default();
        let (palette, fills) = (*palette, *fills);
        let rows = uniform_list("critical-path-steps", path.steps().len(), {
            let entity = entity.clone();
            move |range, _, cx| {
                let model = entity.read(cx).model.read(cx);
                let Some(TracedPath::Traced {
                    path: Some(path), ..
                }) = model.critical_path()
                else {
                    return Vec::new();
                };
                let whole = path.path();
                let logged = Logged::of(path);
                let chosen = model.chosen_step(cx).map(|(index, _)| index);
                range
                    .map(|index| {
                        let entity = entity.clone();
                        step_row(
                            index,
                            &path.steps()[index],
                            whole,
                            longest,
                            logged,
                            &palette,
                            &fills,
                        )
                        .when(chosen == Some(index), |row| {
                            row.bg(palette.selection)
                                .hover(|row| row.bg(palette.picked_hover()))
                        })
                        .on_click(move |_, window, cx| {
                            entity.update(cx, |panel, cx| {
                                panel.focus.focus(window, cx);
                                panel.choose(index, cx)
                            })
                        })
                    })
                    .collect()
            }
        })
        .flex_1()
        .w_full()
        .track_scroll(self.scroll.uniform_handle());
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_grow_1()
            .flex_basis(px(STEPS_BEFORE_BARS * STATS_ROW_HEIGHT))
            .flex_shrink(TABLE_YIELD)
            .min_h(px(STATS_ROW_HEIGHT))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|panel, e: &MouseDownEvent, _, cx| {
                    if panel.scroll.press(e.position) {
                        cx.stop_propagation();
                    }
                    cx.notify();
                }),
            )
            .child(rows)
            .child(list_scrollbar(
                entity,
                |panel: &CriticalPathPanel| &panel.scroll,
                |panel: &mut CriticalPathPanel| &mut panel.scroll,
                palette.scroll,
            ))
    }
}

/// What the path took and the levers acting on the most of it, each hovered
/// for what pulling it involves: the point of the panel, so kept in view
/// down to a pane too short for it beside a single step, where the levers
/// are clipped first. Where the pane is not `roomy`, the headline is set
/// smaller and the levers keep to one line.
fn lead(path: &CriticalPath, roomy: bool, palette: &Palette) -> Div {
    let headline = div().flex_shrink_0().child(words::headline(path));
    div()
        .flex()
        .flex_col()
        .min_h_0()
        .overflow_hidden()
        .gap_1()
        .px_3()
        .map(|lead| match roomy {
            true => lead.pt_2().child(headline.text_base()),
            false => lead.pt_1().child(headline.text_sm()),
        })
        .child(
            levers(path, palette)
                .flex_shrink_0()
                .when(!roomy, |levers| levers.flex_nowrap().overflow_hidden()),
        )
}

/// The wall time partitioned along the path as one bar and its legend, each
/// part's segment and entry hovered for what its time went to; and where the
/// execution log accounts for steps, their actions' time by spawn phase.
fn summary(path: &CriticalPath, legends: bool, palette: &Palette, fills: &Fills) -> Div {
    let parts: Vec<(Part, Duration, SharedString)> = Part::ALL
        .into_iter()
        .map(|part| (part, path.time_in(part)))
        .filter(|(_, time)| !time.is_zero())
        .map(|(part, time)| (part, time, words::part_account(part, path).into()))
        .collect();
    let legend = parts.iter().map(|(part, time, account)| {
        legend_entry(
            ("critical-path-legend", part.index()),
            part_swatch(*part, fills),
            format!("{} {}", words::part_name(*part), words::duration(*time)),
            account.clone(),
            palette,
        )
    });
    let segments = parts
        .iter()
        .map(|(part, time, account)| part_segment(*part, *time, account.clone(), fills));
    let path_bar = bar("path", segments, path.wall(), SEGMENT_GAP, palette).h(PATH_BAR_HEIGHT);
    let spawned = path
        .coverage()
        .filter(|coverage| coverage.steps() > 0)
        .map(|coverage| spawned_summary(&coverage, legends, palette, fills));
    div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .gap_1()
        .px_3()
        .pt_1()
        .pb_2()
        .child(path_bar.w_full())
        .when(legends, |summary| {
            summary.child(legend_row(palette).children(legend))
        })
        .children(spawned)
}

fn levers(path: &CriticalPath, palette: &Palette) -> Div {
    let levers = path
        .levers()
        .into_iter()
        .take(LEVERS_SHOWN)
        .enumerate()
        .map(|(i, (lever, time))| {
            div()
                .id(("critical-path-lever", i))
                .tooltip(TextTooltip::builder(
                    words::lever_tip(lever).into(),
                    palette.tooltip,
                ))
                .child(words::lever(lever, time))
        });
    legend_row(palette)
        .child(div().text_color(palette.label).child(words::LEVERS))
        .children(levers)
}

/// The time the actions of the steps `coverage` holds spent in each spawn
/// phase and around their spawns, as a titled bar over its legend: a scale
/// of its own, apart from the path's bar above it.
fn spawned_summary(coverage: &Coverage, legends: bool, palette: &Palette, fills: &Fills) -> Div {
    let phases: Vec<(Phase, Duration, SharedString)> = Phase::ALL
        .into_iter()
        .map(|phase| (phase, coverage.phase(phase)))
        .filter(|(_, time)| !time.is_zero())
        .map(|(phase, time)| (phase, time, words::phase_account(phase, coverage).into()))
        .collect();
    let around = (!coverage.around().is_zero()).then(|| {
        (
            coverage.around(),
            SharedString::from(words::around_account(coverage)),
        )
    });
    let segments = phases
        .iter()
        .map(|(phase, time, account)| phase_segment(*phase, *time, account.clone(), fills))
        .chain(
            around
                .clone()
                .map(|(time, account)| around_segment(time, account, fills)),
        );
    let legend = phases
        .iter()
        .map(|(phase, time, account)| {
            legend_entry(
                ("critical-path-phase-legend", phase.index()),
                swatch(fills.phase(*phase), None),
                format!("{} {}", words::phase_name(*phase), words::duration(*time)),
                account.clone(),
                palette,
            )
        })
        .chain(around.map(|(time, account)| {
            legend_entry(
                ("critical-path-phase-legend", Phase::ALL.len()),
                swatch(fills.around_spawns(), Some(fills.outside_edge)),
                format!("{} {}", words::AROUND_SPAWNS, words::duration(time)),
                account,
                palette,
            )
        }))
        .collect::<Vec<_>>();
    div()
        .flex()
        .flex_col()
        .gap_1()
        .mt_1()
        .child(div().child(words::spawned_title(coverage)))
        .child(
            bar("spawned", segments, coverage.action(), SEGMENT_GAP, palette)
                .w_full()
                .h(PATH_BAR_HEIGHT),
        )
        .when(legends, |spawned| {
            spawned.child(legend_row(palette).children(legend))
        })
}

fn legend_row(palette: &Palette) -> Div {
    div()
        .flex()
        .flex_wrap()
        .gap_x_3()
        .text_color(palette.muted_label)
}

fn legend_entry(
    id: impl Into<ElementId>,
    swatch: Div,
    label: String,
    account: SharedString,
    palette: &Palette,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_1()
        .tooltip(TextTooltip::builder(account, palette.tooltip))
        .child(swatch)
        .child(label)
}

fn part_swatch(part: Part, fills: &Fills) -> Div {
    swatch(
        fills.part(part),
        outside(part).then_some(fills.outside_edge),
    )
}

/// A legend's sample of `fill`, edged in `edge` where it has one.
fn swatch(fill: Hsla, edge: Option<Hsla>) -> Div {
    let swatch = div().size(SWATCH).flex_shrink_0().bg(fill);
    match edge {
        Some(edge) => swatch.border_1().border_color(edge),
        None => swatch,
    }
}

fn header_row(palette: &Palette) -> Div {
    div()
        .flex()
        .flex_row()
        .flex_shrink_0()
        .h(px(STATS_HEADER_HEIGHT))
        .w_full()
        .pr(px(SCROLLBAR_THICKNESS))
        .bg(palette.header_band)
        .border_b_1()
        .border_color(palette.border)
        .text_size(px(STATS_TEXT_SIZE))
        .text_color(palette.label)
        .children(
            COLUMNS
                .iter()
                .map(|column| table_cell(column.width, [column.title])),
        )
}

/// What an execution log tells of a path's steps. Where it records spawns of
/// any, every step's bar leaves room beneath it for a strip of their phases.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Logged {
    NoLog,
    NoStepJoined,
    StepsJoined,
}

impl Logged {
    fn of(path: &CriticalPath) -> Logged {
        match path.coverage() {
            None => Logged::NoLog,
            Some(coverage) if coverage.steps() == 0 => Logged::NoStepJoined,
            Some(_) => Logged::StepsJoined,
        }
    }

    fn given(self) -> bool {
        self != Logged::NoLog
    }
}

/// Row `index` of the steps table, showing `step`, its own time a share of
/// the path's `whole` and its bar as long against `longest` as its wait and
/// own time are.
fn step_row(
    index: usize,
    step: &Step,
    whole: Duration,
    longest: Duration,
    logged: Logged,
    palette: &Palette,
    fills: &Fills,
) -> Stateful<Div> {
    let [name, start, waited, own, share, bar_column] = &COLUMNS;
    let ran: Vec<&str> = [step.mnemonic(), step.target()]
        .into_iter()
        .flatten()
        .collect();
    let tag = words::work_tag(step.work(), logged.given()).map(|tag| {
        div()
            .flex_shrink_0()
            .px_1()
            .rounded_sm()
            .border_1()
            .border_color(palette.border)
            .text_color(palette.muted_label)
            .child(tag)
    });
    let described = div()
        .id(ElementId::from(("critical-path-step", index)))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .w_full()
        .min_w_0()
        .tooltip(TextTooltip::builder(
            words::step_tip(step, logged.given()).into(),
            palette.tooltip,
        ))
        .child(line(step.name().to_owned()))
        .children(tag)
        .when(!ran.is_empty(), |described| {
            described.child(line(ran.join(" · ")).text_color(palette.muted_label))
        });
    let total = step.gap() + step.own();
    let parts: Vec<(Part, Duration)> = step.parts().collect();
    let segments = parts.iter().map(|&(part, time)| {
        let tip = words::part_tip(part, time, total, "the step's wait and own time");
        part_segment(part, time, tip.into(), fills)
    });
    let width = relative(words::fraction(total, longest) as f32);
    let parts_bar = bar(
        ("critical-path-step-bar", index),
        segments,
        total,
        SEGMENT_GAP,
        palette,
    );
    let bar_of = match (logged == Logged::StepsJoined, step.work().spawned()) {
        (false, _) => parts_bar.h(BAR_HEIGHT).into_any_element(),
        (true, spawned) => div()
            .flex()
            .flex_col()
            .gap(TIER_GAP)
            .child(parts_bar.h(TIER_HEIGHT))
            .children(spawned.map(|spawned| {
                spawn_strip(index, &parts, total, longest, spawned, palette, fills).h(STRIP_HEIGHT)
            }))
            .h(TIER_HEIGHT + TIER_GAP + STRIP_HEIGHT)
            .into_any_element(),
    };
    let bar_of = div().min_w(MIN_BAR_WIDTH).w(width).child(bar_of);
    div()
        .id(("critical-path-row", index))
        .flex()
        .flex_row()
        .items_center()
        .h(px(STATS_ROW_HEIGHT))
        .w_full()
        .pr(px(SCROLLBAR_THICKNESS))
        .text_size(px(STATS_TEXT_SIZE))
        .hover(|row| row.bg(palette.hover))
        .child(table_cell(name.width, [described]))
        .child(table_cell(start.width, [format_duration(step.start_us())]))
        .child(table_cell(waited.width, [words::waited(index, step.gap())]))
        .child(table_cell(own.width, [words::duration(step.own())]))
        .child(table_cell(share.width, [words::share(step.own(), whole)]))
        .child(table_cell(bar_column.width, [div().w_full().child(bar_of)]))
}

/// A line of a cell, cut short where the cell runs out of room for it.
fn line(text: impl Into<SharedString>) -> Div {
    div()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .child(text.into())
}

/// The spawn phases of a step's action under the step's bar of `parts`,
/// each phase as long against the strip as its time is against the step's
/// `total`, so the strip spans the action's segment above it. The bar is as
/// long against its column as `total` is against `longest`, which sizes the
/// phases folded into the remainder.
fn spawn_strip(
    index: usize,
    parts: &[(Part, Duration)],
    total: Duration,
    longest: Duration,
    spawned: &Spawned,
    palette: &Palette,
    fills: &Fills,
) -> Stateful<Div> {
    let action = spawned.time();
    let of = "the action's time on the path";
    let drawn_width = |time| BAR_COLUMN_WIDTH * words::fraction(time, longest) as f32;
    let (wide, narrow): (Vec<_>, Vec<_>) = Phase::ALL
        .into_iter()
        .map(|phase| (phase, spawned.phase(phase)))
        .filter(|(_, time)| !time.is_zero())
        .partition(|&(_, time)| drawn_width(time) >= MIN_STRIP_PHASE_WIDTH);
    let phases = wide.into_iter().map(|(phase, time)| {
        let tip = words::named_tip(words::phase_name(phase), time, action, of);
        phase_segment(phase, time, tip.into(), fills)
    });
    let folded = (!narrow.is_empty()).then(|| {
        let time = narrow.iter().map(|&(_, time)| time).sum();
        let names: Vec<&str> = narrow
            .iter()
            .map(|&(phase, _)| words::phase_name(phase))
            .collect();
        let tip = words::named_tip(&words::folded(&names), time, action, of);
        Segment {
            fill: fills.folded_phases(),
            edge: None,
            time,
            tip: tip.into(),
        }
    });
    let around = Some(spawned.around())
        .filter(|around| !around.is_zero())
        .map(|time| {
            let tip = words::named_tip(words::AROUND_SPAWNS, time, action, of);
            around_segment(time, tip.into(), fills)
        });
    let mut strip = Some(
        bar(
            ("critical-path-step-phases", index),
            phases.chain(around).chain(folded),
            action,
            STRIP_GAP,
            palette,
        )
        .size_full(),
    );
    let slots = parts.iter().map(|&(part, time)| {
        let slot = div()
            .h_full()
            .w(relative(words::fraction(time, total) as f32));
        match part {
            Part::Action | Part::Unspanned => slot.children(strip.take()),
            _ => slot,
        }
    });
    div()
        .id(("critical-path-step-strip", index))
        .flex()
        .flex_row()
        .gap(SEGMENT_GAP)
        .children(slots.collect::<Vec<_>>())
}

/// A stretch of a bar, hovered for `tip`.
struct Segment {
    fill: Hsla,
    /// Edged as time outside what the bar breaks down.
    edge: Option<Hsla>,
    time: Duration,
    tip: SharedString,
}

fn part_segment(part: Part, time: Duration, tip: SharedString, fills: &Fills) -> Segment {
    Segment {
        fill: fills.part(part),
        edge: outside(part).then_some(fills.outside_edge),
        time,
        tip,
    }
}

fn phase_segment(phase: Phase, time: Duration, tip: SharedString, fills: &Fills) -> Segment {
    Segment {
        fill: fills.phase(phase),
        edge: None,
        time,
        tip,
    }
}

fn around_segment(time: Duration, tip: SharedString, fills: &Fills) -> Segment {
    Segment {
        fill: fills.around_spawns(),
        edge: Some(fills.outside_edge),
        time,
        tip,
    }
}

/// `segments` as a bar, each as long against the bar as its time is against
/// `total`, and hovered for its tooltip.
fn bar(
    id: impl Into<ElementId>,
    segments: impl Iterator<Item = Segment>,
    total: Duration,
    gap: Pixels,
    palette: &Palette,
) -> Stateful<Div> {
    let tooltip = palette.tooltip;
    let segments = segments.enumerate().map(move |(i, segment)| {
        let drawn = div()
            .id(i)
            .h_full()
            .w(relative(words::fraction(segment.time, total) as f32))
            .bg(segment.fill)
            .tooltip(TextTooltip::builder(segment.tip, tooltip));
        match segment.edge {
            Some(edge) => drawn.border_1().border_color(edge),
            None => drawn,
        }
    });
    div().id(id).flex().flex_row().gap(gap).children(segments)
}

impl EventEmitter<PanelEvent> for CriticalPathPanel {}

impl Focusable for CriticalPathPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Panel for CriticalPathPanel {
    fn kind() -> PanelKind {
        PanelKind {
            view: ViewKind::new("trace-critical-path"),
            label: "Critical path",
            key_context: KEY_CONTEXT,
        }
    }

    fn title(&self, _cx: &App) -> SharedString {
        Self::kind().label.into()
    }

    fn status(&self, cx: &App) -> Option<SharedString> {
        Some(profile_status(&self.model, cx))
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if active {
            self.focus.focus(window, cx);
        }
    }
}

impl Render for CriticalPathPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = bviz_ui_theme::theme(window, cx);
        let palette = Palette::from_theme(&theme);
        let fills = Fills::from_theme(&theme);
        let panel = div()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|panel, _: &ChoosePreviousStep, _, cx| panel.choose_by(-1, cx)))
            .on_action(cx.listener(|panel, _: &ChooseNextStep, _, cx| panel.choose_by(1, cx)))
            .on_action(cx.listener(|panel, _: &UnchooseStep, _, cx| {
                panel.model.update(cx, |model, cx| model.unchoose_step(cx));
            }))
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .bg(palette.background)
            .text_color(palette.label)
            .text_size(px(STATS_TEXT_SIZE));
        let model = self.model.read(cx);
        match model.critical_path() {
            None => panel.children(model.source().placeholder(&theme.colors)),
            Some(TracedPath::Tracing { .. }) => panel.child(centered_spinner(
                "Tracing the critical path…",
                &theme.colors,
            )),
            Some(TracedPath::Traced { path: None, .. }) => panel.child(centered_notice(
                "This profile records no critical path.",
                &theme.colors,
            )),
            Some(TracedPath::Traced {
                path: Some(path), ..
            }) => panel.child(self.body(path, &palette, &fills, cx)),
        }
    }
}
