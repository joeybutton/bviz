//! Where the wall time of the profile's build went along its critical path.

mod fills;
mod words;

use crate::chart::format_duration;
use crate::model::{CriticalPathDerived, Derived, SourceReplaced, TraceModel};
use crate::stats_panel::table_cell;
use crate::theme::Palette;
use crate::{
    STATS_HEADER_HEIGHT, STATS_NUM_COL_WIDTH, STATS_ROW_HEIGHT, STATS_TEXT_SIZE, profile_status,
};
use bviz_critical_path::{CriticalPath, Part, Step};
use bviz_ui_components::{
    ListScroll, SCROLLBAR_THICKNESS, TextTooltip, TooltipColors, centered_notice, centered_spinner,
    list_scrollbar,
};
use bviz_ui_workspace::{Panel, PanelEvent, PanelKind, ViewKind};
use fills::{Fills, outside};
use gpui::{
    App, Context, Div, ElementId, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    MouseButton, MouseDownEvent, Pixels, Render, SharedString, Stateful, Subscription, Window, div,
    prelude::*, px, relative, uniform_list,
};
use std::time::Duration;

const KEY_CONTEXT: &str = "TraceCriticalPath";

const BAR_HEIGHT: Pixels = px(12.);
const PATH_BAR_HEIGHT: Pixels = px(14.);
/// The canvas showing between two segments of a bar, so neighbouring parts
/// part by more than their colours.
const SEGMENT_GAP: Pixels = px(2.);
/// The narrowest a step's bar is drawn, so even the shortest step's bar can
/// be hovered for its parts.
const MIN_BAR_WIDTH: Pixels = px(4.);
const SWATCH: Pixels = px(10.);
/// Width of the column holding a step's share of the path.
const SHARE_WIDTH: f32 = 72.0;
/// Width of the column holding a step's bar of parts.
const BAR_COLUMN_WIDTH: f32 = 200.0;

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
            cx.notify();
        });
        Self {
            focus: cx.focus_handle(),
            model,
            scroll: ListScroll::new(),
            _model_observer: model_observer,
            _derived: derived,
            _source_replaced: source_replaced,
        }
    }

    /// The path in sum over its table of steps.
    fn body(
        &self,
        path: &CriticalPath,
        palette: &Palette,
        fills: &Fills,
        cx: &Context<Self>,
    ) -> Div {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(summary(path, palette, fills))
            .child(header_row(palette))
            .child(self.steps(path, palette, fills, cx))
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
                let Some(Derived::Derived(Some(path))) = model.critical_path() else {
                    return Vec::new();
                };
                let whole = path.path();
                range
                    .map(|index| {
                        step_row(
                            index,
                            &path.steps()[index],
                            whole,
                            longest,
                            &palette,
                            &fills,
                        )
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
            .flex_1()
            .min_h_0()
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

/// The wall time partitioned along the path as one bar and its legend, each
/// part's segment and entry hovered for what its time went to.
fn summary(path: &CriticalPath, palette: &Palette, fills: &Fills) -> Div {
    let parts: Vec<(Part, Duration, SharedString)> = Part::ALL
        .into_iter()
        .map(|part| (part, path.time_in(part)))
        .filter(|(_, time)| !time.is_zero())
        .map(|(part, time)| (part, time, words::part_account(part, path).into()))
        .collect();
    let legend = parts.iter().map(|(part, time, account)| {
        div()
            .id(("critical-path-legend", part.index()))
            .flex()
            .items_center()
            .gap_1()
            .tooltip(TextTooltip::builder(account.clone(), palette.tooltip))
            .child(swatch(*part, fills))
            .child(format!(
                "{} {}",
                words::part_name(*part),
                words::duration(*time)
            ))
    });
    div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .gap_1()
        .px_3()
        .py_2()
        .child(div().text_base().child(words::headline(path)))
        .child(
            parts_bar(
                "path",
                parts.clone().into_iter(),
                path.wall(),
                fills,
                palette.tooltip,
            )
            .w_full()
            .h(PATH_BAR_HEIGHT),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_x_3()
                .text_color(palette.muted_label)
                .children(legend),
        )
}

fn swatch(part: Part, fills: &Fills) -> Div {
    div()
        .size(SWATCH)
        .flex_shrink_0()
        .bg(fills.part(part))
        .when(outside(part), |swatch| {
            swatch.border_1().border_color(fills.outside_edge)
        })
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

/// Row `index` of the steps table, showing `step`, its own time a share of
/// the path's `whole` and its bar as long against `longest` as its wait and
/// own time are.
fn step_row(
    index: usize,
    step: &Step,
    whole: Duration,
    longest: Duration,
    palette: &Palette,
    fills: &Fills,
) -> Stateful<Div> {
    let [name, start, waited, own, share, bar] = &COLUMNS;
    let ran: Vec<&str> = [step.mnemonic(), step.target()]
        .into_iter()
        .flatten()
        .collect();
    let tag = words::work_tag(step.work()).map(|tag| {
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
            words::step_tip(step).into(),
            palette.tooltip,
        ))
        .child(line(step.name().to_owned()))
        .children(tag)
        .when(!ran.is_empty(), |described| {
            described.child(line(ran.join(" · ")).text_color(palette.muted_label))
        });
    let total = step.gap() + step.own();
    let parts = step.parts().map(|(part, time)| {
        let tip = words::part_tip(part, time, total, "the step's wait and own time");
        (part, time, tip.into())
    });
    let bar_of = parts_bar(
        ("critical-path-step-bar", index),
        parts,
        total,
        fills,
        palette.tooltip,
    )
    .h(BAR_HEIGHT)
    .min_w(MIN_BAR_WIDTH)
    .w(relative(words::fraction(total, longest) as f32));
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
        .child(table_cell(bar.width, [div().w_full().child(bar_of)]))
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

/// `parts` as a bar, each segment as long against the bar as its time is
/// against `total`, and hovered for its tooltip.
fn parts_bar(
    id: impl Into<ElementId>,
    parts: impl Iterator<Item = (Part, Duration, SharedString)>,
    total: Duration,
    fills: &Fills,
    tooltip: TooltipColors,
) -> Stateful<Div> {
    let fills = *fills;
    let segments = parts.map(move |(part, time, tip)| {
        div()
            .id(part.index())
            .h_full()
            .w(relative(words::fraction(time, total) as f32))
            .bg(fills.part(part))
            .when(outside(part), |segment| {
                segment.border_1().border_color(fills.outside_edge)
            })
            .tooltip(TextTooltip::builder(tip, tooltip))
    });
    div()
        .id(id)
        .flex()
        .flex_row()
        .gap(SEGMENT_GAP)
        .children(segments)
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
            Some(Derived::Deriving { .. }) => panel.child(centered_spinner(
                "Tracing the critical path…",
                &theme.colors,
            )),
            Some(Derived::Derived(None)) => panel.child(centered_notice(
                "This profile records no critical path.",
                &theme.colors,
            )),
            Some(Derived::Derived(Some(path))) => {
                panel.child(self.body(path, &palette, &fills, cx))
            }
        }
    }
}
