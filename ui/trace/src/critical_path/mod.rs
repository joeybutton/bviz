//! Where the wall time of the profile's build went along its critical path.

mod by_height;
mod fills;
mod whole;
mod words;

use crate::chart::format_duration;
use crate::model::{CriticalPathDerived, SourceReplaced, TraceModel, TracedPath};
use crate::stats_panel::table_cell;
use crate::theme::Palette;
use crate::{
    ChooseNext, ChoosePrevious, STATS_HEADER_HEIGHT, STATS_NUM_COL_WIDTH, STATS_ROW_HEIGHT,
    STATS_TEXT_SIZE, Unchoose, profile_status,
};
use bviz_critical_path::{
    Coverage, CriticalPath, Modeled, NearlyCritical, Part, Phase, Spawned, Step,
};
use bviz_invocation::Chosen;
use bviz_ui_components::{
    Button, ListScroll, SCROLLBAR_THICKNESS, TextTooltip, centered_notice, centered_spinner,
    list_scrollbar,
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
use whole::whole_or_none;

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

/// One column of a table: its title and its fixed width, `None` for the one
/// flexible column.
struct Column {
    title: &'static str,
    width: Option<f32>,
    /// Shown only for a path held against a modeled build.
    modeled: bool,
}

const fn column(title: &'static str, width: Option<f32>) -> Column {
    Column {
        title,
        width,
        modeled: false,
    }
}

const COLUMNS: [Column; 7] = [
    column("Step", None),
    column("Start", Some(STATS_NUM_COL_WIDTH)),
    column("Waited", Some(STATS_NUM_COL_WIDTH)),
    column("Own", Some(STATS_NUM_COL_WIDTH)),
    Column {
        modeled: true,
        ..column("Could save", Some(STATS_NUM_COL_WIDTH))
    },
    column("% of path", Some(SHARE_WIDTH)),
    column("Wait and own time", Some(BAR_COLUMN_WIDTH)),
];

const NEARLY_CRITICAL_COLUMNS: [Column; 3] = [
    column("Action", None),
    column("Took", Some(STATS_NUM_COL_WIDTH)),
    column("Slack", Some(STATS_NUM_COL_WIDTH)),
];

/// Which list the table under the summary shows.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Listing {
    #[default]
    Steps,
    /// The actions off the path closest to becoming critical, which a path
    /// held against no modeled build has none of.
    NearlyCritical,
}

/// The critical path of the profile: the wall time partitioned along it, an
/// account of where the time went, and its steps in the order they started.
pub struct CriticalPathPanel {
    focus: FocusHandle,
    model: Entity<TraceModel>,
    listing: Listing,
    scroll: ListScroll,
    nearly_critical_scroll: ListScroll,
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
            panel.listing = Listing::Steps;
            panel.scroll.to_top();
            panel.nearly_critical_scroll.to_top();
            panel.summary_scroll.to_top();
            cx.notify();
        });
        Self {
            focus: cx.focus_handle(),
            model,
            listing: Listing::Steps,
            scroll: ListScroll::new(),
            nearly_critical_scroll: ListScroll::new(),
            summary_scroll: ListScroll::new(),
            _model_observer: model_observer,
            _derived: derived,
            _source_replaced: source_replaced,
        }
    }

    /// Chooses `chosen`, showing the list it is among with its row in view.
    fn choose(&mut self, chosen: Chosen, cx: &mut Context<Self>) {
        let (listing, index, scroll) = match chosen {
            Chosen::Step(index) => (Listing::Steps, index, &self.scroll),
            Chosen::NearlyCritical(index) => {
                (Listing::NearlyCritical, index, &self.nearly_critical_scroll)
            }
        };
        scroll
            .uniform_handle()
            .scroll_to_item(index, ScrollStrategy::Nearest);
        self.listing = listing;
        self.model.update(cx, |model, cx| model.choose(chosen, cx));
        cx.notify();
    }

    /// Lets go of `chosen` where it is what is chosen, and chooses it
    /// otherwise.
    fn toggle(&mut self, chosen: Chosen, cx: &mut Context<Self>) {
        let standing = self.model.read(cx).chosen(cx).map(|(standing, _)| standing);
        match standing == Some(chosen) {
            true => {
                self.model.update(cx, |model, cx| model.unchoose(cx));
            }
            false => self.choose(chosen, cx),
        }
    }

    /// Chooses the row `by` rows from the chosen one in the list the table
    /// shows, or its first where none of it is chosen, stopping at either
    /// end of the list.
    fn choose_by(&mut self, by: isize, cx: &mut Context<Self>) {
        let model = self.model.read(cx);
        let Some(TracedPath::Traced {
            path: Some(path), ..
        }) = model.critical_path()
        else {
            return;
        };
        let standing = model.chosen(cx).map(|(standing, _)| standing);
        let (rows, row, chosen): (usize, Option<usize>, fn(usize) -> Chosen) =
            match (shown_listing(self.listing, path), path.modeled()) {
                (Listing::NearlyCritical, Some(modeled)) => (
                    modeled.nearly_critical().len(),
                    match standing {
                        Some(Chosen::NearlyCritical(index)) => Some(index),
                        _ => None,
                    },
                    Chosen::NearlyCritical,
                ),
                _ => (
                    path.steps().len(),
                    match standing {
                        Some(Chosen::Step(index)) => Some(index),
                        _ => None,
                    },
                    Chosen::Step,
                ),
            };
        let Some(last) = rows.checked_sub(1) else {
            return;
        };
        let index = match row {
            Some(row) => row.saturating_add_signed(by).min(last),
            None => 0,
        };
        self.choose(chosen(index), cx);
    }

    /// The path in sum over its table of steps, or where the path is held
    /// against a modeled build, of the steps or the actions nearly critical.
    fn body(
        &self,
        path: &CriticalPath,
        palette: &Palette,
        fills: &Fills,
        cx: &Context<Self>,
    ) -> ByHeight {
        let modeled = path.modeled();
        let listing = shown_listing(self.listing, path);
        let body = |legends: bool| {
            let body = div()
                .flex()
                .flex_col()
                .size_full()
                .child(lead(path, legends, palette))
                .child(self.summary(path, legends, palette, fills, cx))
                .children(modeled.map(|modeled| modeled_caption(path, modeled, palette)));
            let choice =
                || modeled.map(|modeled| self.listing_choice(modeled, listing, palette, cx));
            match (listing, modeled) {
                (Listing::NearlyCritical, Some(modeled)) => {
                    let header = header_row(NEARLY_CRITICAL_COLUMNS.iter(), choice(), palette);
                    match modeled.nearly_critical().is_empty() {
                        true => body.child(header).child(
                            div()
                                .flex_1()
                                .px_3()
                                .py_2()
                                .text_color(palette.muted_label)
                                .child(words::NO_NEARLY_CRITICAL),
                        ),
                        false => body
                            .child(header)
                            .child(self.nearly_critical(modeled, palette, cx)),
                    }
                }
                _ => body
                    .child(header_row(
                        COLUMNS
                            .iter()
                            .filter(|column| modeled.is_some() || !column.modeled),
                        choice(),
                        palette,
                    ))
                    .child(self.steps(path, palette, fills, cx)),
            }
            .into_any_element()
        };
        ByHeight::new(LEGENDS_MIN_HEIGHT, body(true), body(false))
    }

    /// The choice of what the table lists, in place of its first column's
    /// title.
    fn listing_choice(
        &self,
        modeled: &Modeled,
        listing: Listing,
        palette: &Palette,
        cx: &Context<Self>,
    ) -> Div {
        let button = |id: &'static str, label: String, tip: &'static str, shows: Listing| {
            let panel = cx.entity();
            div()
                .id(("critical-path-list-choice", shows as usize))
                .tooltip(TextTooltip::builder(tip.into(), palette.tooltip))
                .child(
                    Button::new(id, move |_, _, cx| {
                        panel.update(cx, |panel, cx| {
                            panel.listing = shows;
                            cx.notify();
                        })
                    })
                    .compact()
                    .selected(listing == shows)
                    .label(label),
                )
        };
        div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .child(button(
                "critical-path-list-steps",
                words::STEPS_TITLE.to_owned(),
                words::STEPS_TIP,
                Listing::Steps,
            ))
            .child(button(
                "critical-path-list-nearly-critical",
                words::nearly_critical_title(modeled),
                words::NEARLY_CRITICAL_TIP,
                Listing::NearlyCritical,
            ))
    }

    /// The actions nearly critical, one row each.
    fn nearly_critical(&self, modeled: &Modeled, palette: &Palette, cx: &Context<Self>) -> Div {
        let entity = cx.entity();
        let palette = *palette;
        let rows = uniform_list(
            "critical-path-nearly-critical",
            modeled.nearly_critical().len(),
            {
                let entity = entity.clone();
                move |range, _, cx| {
                    let model = entity.read(cx).model.read(cx);
                    let Some(TracedPath::Traced {
                        path: Some(path), ..
                    }) = model.critical_path()
                    else {
                        return Vec::new();
                    };
                    let Some(modeled) = path.modeled() else {
                        return Vec::new();
                    };
                    let chosen = model.chosen(cx).map(|(chosen, _)| chosen);
                    range
                        .map(|index| {
                            nearly_critical_row(index, &modeled.nearly_critical()[index], &palette)
                                .map(|row| {
                                    choosable(
                                        row,
                                        Chosen::NearlyCritical(index),
                                        chosen,
                                        entity.clone(),
                                        &palette,
                                    )
                                })
                        })
                        .collect()
                }
            },
        )
        .flex_1()
        .w_full()
        .track_scroll(self.nearly_critical_scroll.uniform_handle());
        list_pane(
            rows,
            entity,
            |panel| &panel.nearly_critical_scroll,
            |panel| &mut panel.nearly_critical_scroll,
            &palette,
            cx,
        )
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
                let against = Against {
                    whole: path.path(),
                    longest,
                    logged: Logged::of(path),
                    modeled: path.modeled().is_some(),
                };
                let chosen = model.chosen(cx).map(|(chosen, _)| chosen);
                range
                    .map(|index| {
                        let entity = entity.clone();
                        step_row(index, &path.steps()[index], against, &palette, &fills).map(
                            |row| choosable(row, Chosen::Step(index), chosen, entity, &palette),
                        )
                    })
                    .collect()
            }
        })
        .flex_1()
        .w_full()
        .track_scroll(self.scroll.uniform_handle());
        list_pane(
            rows,
            entity,
            |panel| &panel.scroll,
            |panel| &mut panel.scroll,
            &palette,
            cx,
        )
    }
}

/// The list the table shows of `path` where `listing` is the one chosen: the
/// steps for a path held against no modeled build.
fn shown_listing(listing: Listing, path: &CriticalPath) -> Listing {
    match path.modeled() {
        Some(_) => listing,
        None => Listing::Steps,
    }
}

/// `row`, standing for `stands_for`, marked where that is `chosen` and
/// toggling it on a click in the panel `entity`.
fn choosable(
    row: Stateful<Div>,
    stands_for: Chosen,
    chosen: Option<Chosen>,
    entity: Entity<CriticalPathPanel>,
    palette: &Palette,
) -> Stateful<Div> {
    let picked_hover = palette.picked_hover();
    row.when(chosen == Some(stands_for), |row| {
        row.bg(palette.selection)
            .hover(move |row| row.bg(picked_hover))
    })
    .on_click(move |_, window, cx| {
        entity.update(cx, |panel, cx| {
            panel.focus.focus(window, cx);
            panel.toggle(stands_for, cx)
        })
    })
}

/// `rows` taking the table's room under its header, scrolled through
/// `scroll` of the panel `entity`.
fn list_pane(
    rows: impl IntoElement,
    entity: Entity<CriticalPathPanel>,
    scroll: fn(&CriticalPathPanel) -> &ListScroll,
    scroll_mut: fn(&mut CriticalPathPanel) -> &mut ListScroll,
    palette: &Palette,
    cx: &Context<CriticalPathPanel>,
) -> Div {
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
            cx.listener(move |panel, e: &MouseDownEvent, _, cx| {
                if scroll_mut(panel).press(e.position) {
                    cx.stop_propagation();
                }
                cx.notify();
            }),
        )
        .child(rows)
        .child(list_scrollbar(entity, scroll, scroll_mut, palette.scroll))
}

/// What the path took and the levers acting on the most of it, each hovered
/// for what pulling it involves: the point of the panel, so kept in view
/// down to a pane too short for it beside a single step, where its lines are
/// left out from the last. Where the pane is not `roomy`, the headline is set
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
            true => lead.pt_2().child(whole_or_none(headline.text_base())),
            false => lead.pt_1().child(whole_or_none(headline.text_sm())),
        })
        .child(whole_or_none(
            levers(path, palette)
                .flex_shrink_0()
                .when(!roomy, |levers| levers.flex_nowrap().overflow_hidden()),
        ))
}

/// The modeled build's length against the observed path's actions, heading
/// the table whose Could save column and nearly critical actions the model
/// gives.
fn modeled_caption(path: &CriticalPath, modeled: &Modeled, palette: &Palette) -> Stateful<Div> {
    div()
        .id("critical-path-modeled")
        .flex_shrink_0()
        .px_3()
        .py_1()
        .border_t_1()
        .border_color(palette.border)
        .text_color(palette.muted_label)
        .tooltip(TextTooltip::builder(
            words::modeled_tip(path, modeled).into(),
            palette.tooltip,
        ))
        .child(line(words::modeled_line(path, modeled)))
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
            let bound = words::lever_bound(path.lever_ceiling(lever), time);
            div()
                .id(("critical-path-lever", i))
                .tooltip(TextTooltip::builder(
                    words::lever_account(lever, bound).into(),
                    palette.tooltip,
                ))
                .child(words::lever(lever, time, bound))
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

/// The header of a table of `columns`, the flexible column titled by
/// `title` where one is given.
fn header_row<'c>(
    columns: impl Iterator<Item = &'c Column>,
    title: Option<Div>,
    palette: &Palette,
) -> Div {
    let mut title = title;
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
        .children(columns.map(|column| match (column.width, title.take()) {
            (None, Some(title)) => table_cell(None, [title.into_any_element()]),
            (width, _) => table_cell(width, [column.title.into_any_element()]),
        }))
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

/// What every row of the steps table is drawn against.
#[derive(Clone, Copy)]
struct Against {
    /// The path's own time, which a step's is a share of.
    whole: Duration,
    /// The longest wait and own time of any step, which a step's bar is as
    /// long against as its own are.
    longest: Duration,
    logged: Logged,
    /// Whether the path is held against a modeled build.
    modeled: bool,
}

/// Row `index` of the steps table, showing `step`.
fn step_row(
    index: usize,
    step: &Step,
    against: Against,
    palette: &Palette,
    fills: &Fills,
) -> Stateful<Div> {
    let Against {
        whole,
        longest,
        logged,
        modeled,
    } = against;
    let [name, start, waited, own, could_save, share, bar_column] = &COLUMNS;
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
        .when(modeled, |row| {
            let saved = div()
                .id(("critical-path-could-save", index))
                .when_some(words::could_save_tip(step), |cell, tip| {
                    cell.tooltip(TextTooltip::builder(tip.into(), palette.tooltip))
                })
                .child(words::could_save(step));
            row.child(table_cell(could_save.width, [saved]))
        })
        .child(table_cell(share.width, [words::share(step.own(), whole)]))
        .child(table_cell(bar_column.width, [div().w_full().child(bar_of)]))
}

/// Row `index` of the table of actions nearly critical, showing `action`.
fn nearly_critical_row(index: usize, action: &NearlyCritical, palette: &Palette) -> Stateful<Div> {
    let [name, took, slack] = &NEARLY_CRITICAL_COLUMNS;
    let ran: Vec<&str> = [action.mnemonic.as_deref(), action.target.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    let described = div()
        .id(("critical-path-nearly-critical-action", index))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .w_full()
        .min_w_0()
        .tooltip(TextTooltip::builder(
            words::nearly_critical_tip(action).into(),
            palette.tooltip,
        ))
        .child(line(action.description.clone()))
        .when(!ran.is_empty(), |described| {
            described.child(line(ran.join(" · ")).text_color(palette.muted_label))
        });
    div()
        .id(("critical-path-nearly-critical-row", index))
        .flex()
        .flex_row()
        .items_center()
        .h(px(STATS_ROW_HEIGHT))
        .w_full()
        .pr(px(SCROLLBAR_THICKNESS))
        .text_size(px(STATS_TEXT_SIZE))
        .hover(|row| row.bg(palette.hover))
        .child(table_cell(name.width, [described]))
        .child(table_cell(took.width, [words::duration(action.duration)]))
        .child(table_cell(slack.width, [words::duration(action.slack)]))
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
            .on_action(cx.listener(|panel, _: &ChoosePrevious, _, cx| panel.choose_by(-1, cx)))
            .on_action(cx.listener(|panel, _: &ChooseNext, _, cx| panel.choose_by(1, cx)))
            .on_action(cx.listener(|panel, _: &Unchoose, _, cx| {
                panel.model.update(cx, |model, cx| model.unchoose(cx));
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
