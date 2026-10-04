use crate::format::{duration, exact_duration, phase_name, share};
use crate::model::ExecLogModel;
use crate::model::Source;
use crate::palette::Palette;
use crate::summary::{GroupRow, Summary};
use bviz_execlog::{Phase, Timing};
use bviz_ui_components::{ListScroll, SCROLLBAR_THICKNESS, TextTooltip, list_scrollbar};
use bviz_ui_workspace::{Panel, PanelEvent, PanelKind, ViewKind};
use gpui::{
    App, Context, Div, ElementId, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    MouseButton, MouseDownEvent, Pixels, Render, SharedString, Stateful, Subscription, Window, div,
    prelude::*, px, relative,
};
use std::time::Duration;

const ROW_HEIGHT: Pixels = px(24.);
const BAR_HEIGHT: Pixels = px(14.);
/// The canvas showing between two segments of a bar, so neighbouring phases
/// part by more than their colours.
const SEGMENT_GAP: Pixels = px(2.);
/// The narrowest a segment is drawn, so even a sliver of a phase can be
/// hovered for its time.
const MIN_SEGMENT_WIDTH: Pixels = px(4.);
const CELL_PAD: Pixels = px(6.);
const SWATCH: Pixels = px(10.);
const NAME_WIDTH: Pixels = px(220.);
const NUMBER_WIDTH: Pixels = px(110.);
const NOTE_WIDTH: Pixels = px(110.);

/// The spawn time of an execution log by phase, summed per runner.
pub struct SpawnTimePanel {
    focus: FocusHandle,
    model: Entity<ExecLogModel>,
    /// `None` until the log is loaded, the model's placeholder showing instead.
    shown: Option<Summary>,
    scroll: ListScroll,
    _model_observer: Subscription,
}

impl SpawnTimePanel {
    pub(crate) fn new(model: Entity<ExecLogModel>, cx: &mut Context<Self>) -> Self {
        let model_observer = cx.observe(&model, |this, _, cx| {
            this.show_loaded(cx);
            cx.notify();
        });
        let mut panel = Self {
            focus: cx.focus_handle(),
            model,
            shown: None,
            scroll: ListScroll::new(),
            _model_observer: model_observer,
        };
        panel.show_loaded(cx);
        panel
    }

    fn show_loaded(&mut self, cx: &mut Context<Self>) {
        if self.shown.is_some() {
            return;
        }
        if let Source::Loaded(summary) = self.model.read(cx).source() {
            self.shown = Some(summary.clone());
        }
    }

    fn body(&self, summary: &Summary, palette: &Palette, cx: &mut Context<Self>) -> Div {
        let content = div()
            .id("spawn-time")
            .flex()
            .flex_col()
            .gap_5()
            .p_3()
            .pr(px(SCROLLBAR_THICKNESS) + CELL_PAD)
            .size_full()
            .overflow_y_scroll()
            .track_scroll(self.scroll.handle())
            .child(header(summary, palette))
            .map(|content| {
                if summary.spawns == 0 {
                    return content.child(
                        div()
                            .text_color(palette.colors.text_muted)
                            .child("The log holds no spawn."),
                    );
                }
                content.child(section(
                    "By runner",
                    group_table("runner", "Runner", &summary.runners, palette),
                    palette,
                ))
            });
        div()
            .relative()
            .flex()
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
            .child(content)
            .child(list_scrollbar(
                cx.entity(),
                |panel: &SpawnTimePanel| &panel.scroll,
                |panel: &mut SpawnTimePanel| &mut panel.scroll,
                palette.scroll,
            ))
    }
}

/// What the log holds in sum, how to read a sum of spawn time, and which
/// colour is which phase.
fn header(summary: &Summary, palette: &Palette) -> Div {
    let legend = Phase::ALL.into_iter().map(|phase| {
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(div().size(SWATCH).bg(palette.phase(phase)))
            .child(phase_name(phase))
    });
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_base().child(format!(
            "{} spawns, {} of spawn time",
            summary.spawns,
            duration(summary.timing.total())
        )))
        .child(
            div()
                .text_color(palette.colors.text_muted)
                .child("Spawns overlap, so summed spawn time is not wall time.")
                .child("Spawns answered from the action cache are not logged."),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_x_3()
                .pt_1()
                .text_color(palette.colors.text_muted)
                .children(legend),
        )
}

/// A titled part of the panel.
fn section(title: &'static str, content: impl IntoElement, palette: &Palette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .pb_1()
                .border_b_1()
                .border_color(palette.colors.border)
                .child(div().text_base().child(title)),
        )
        .child(content)
}

/// How wide a column of a table is.
#[derive(Clone, Copy)]
enum Width {
    Fixed(Pixels),
    /// Takes what the fixed columns leave.
    Rest,
}

#[derive(Clone, Copy)]
struct Column {
    width: Width,
    /// Right-aligned, so the digits of a column line up.
    numeric: bool,
}

const fn text(width: Width) -> Column {
    Column {
        width,
        numeric: false,
    }
}

const fn number(width: Width) -> Column {
    Column {
        width,
        numeric: true,
    }
}

/// The columns of a table of summed spawn time: its name, spawns, spawn time,
/// the bar of its phases, and a note on what the bar can show.
const GROUP_COLUMNS: [Column; 5] = [
    text(Width::Fixed(NAME_WIDTH)),
    number(Width::Fixed(NUMBER_WIDTH)),
    number(Width::Fixed(NUMBER_WIDTH)),
    text(Width::Rest),
    text(Width::Fixed(NOTE_WIDTH)),
];

/// One cell, header or body, laid out by its column so the header and every
/// row agree on the column's width at any window width.
fn cell(column: Column, content: impl IntoElement) -> Div {
    let cell = div()
        .flex()
        .items_center()
        .h_full()
        .px(CELL_PAD)
        .overflow_hidden()
        .whitespace_nowrap()
        .when(column.numeric, |cell| cell.justify_end())
        .child(content);
    match column.width {
        Width::Fixed(width) => cell.w(width).flex_shrink_0(),
        Width::Rest => cell.flex_1().min_w_0(),
    }
}

/// A row of a table.
fn row(id: impl Into<ElementId>, cells: impl IntoIterator<Item = Div>) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_row()
        .w_full()
        .h(ROW_HEIGHT)
        .children(cells)
}

fn header_row(titles: [&'static str; GROUP_COLUMNS.len()], palette: &Palette) -> Div {
    div()
        .flex()
        .flex_row()
        .w_full()
        .h(ROW_HEIGHT)
        .text_color(palette.colors.text_muted)
        .children(
            GROUP_COLUMNS
                .into_iter()
                .zip(titles)
                .map(|(column, title)| cell(column, title)),
        )
}

/// One row per group of `rows`, each bar as long against the longest as its
/// spawn time is.
fn group_table(
    id: &'static str,
    name_title: &'static str,
    rows: &[GroupRow],
    palette: &Palette,
) -> Div {
    let longest = rows
        .iter()
        .map(|row| row.timing.total())
        .max()
        .unwrap_or_default();
    let body = rows.iter().enumerate().map(|(index, group)| {
        let [name, spawns, total, bar, note] = GROUP_COLUMNS;
        let execution_only = group.timing.reports_execution_only();
        row(
            (id, index),
            [
                cell(name, div().text_ellipsis().child(group.name.clone())),
                cell(spawns, group.spawns.to_string()),
                cell(total, duration(group.timing.total())),
                cell(bar, phase_bar(&group.timing, longest, palette)),
                cell(
                    note,
                    div()
                        .text_color(palette.colors.text_muted)
                        .when(execution_only, |note| note.child("execution only")),
                ),
            ],
        )
    });
    div()
        .flex()
        .flex_col()
        .child(header_row(
            [name_title, "Spawns", "Spawn time", "By phase", ""],
            palette,
        ))
        .children(body)
}

/// `timing` as a bar of its phases, as long against the bar for `longest` as
/// its total is, each phase hovered for its exact duration and its share of
/// the total.
///
/// Where the reported phases overlap to sum past the total, the segments
/// divide the bar by that sum, and the tooltip says so.
///
/// The phases of a runner that reports only execution are drawn as one
/// unbroken bar, apart from a breakdown whose other phases took no time.
fn phase_bar(timing: &Timing, longest: Duration, palette: &Palette) -> Div {
    let total = timing.total();
    let summed: Duration = Phase::ALL.into_iter().map(|p| timing.phase(p)).sum();
    let span = total.max(summed);
    let execution_only = timing.reports_execution_only();
    let segments = Phase::ALL
        .into_iter()
        .filter(|&phase| !timing.phase(phase).is_zero())
        .map(|phase| {
            let time = timing.phase(phase);
            let mut tip = format!(
                "{}: {} ({} of {})",
                phase_name(phase),
                exact_duration(time),
                share(time, total),
                exact_duration(total),
            );
            if summed > total {
                tip.push_str(&format!(
                    "\nThe reported phases overlap, summing to {}.",
                    exact_duration(summed)
                ));
            }
            let segment = div()
                .id(("phase", phase.index()))
                .h_full()
                .min_w(MIN_SEGMENT_WIDTH)
                .w(relative(fraction(time, span)));
            let segment = if execution_only && phase == Phase::Execute {
                tip.push_str("\nThis runner reports no other phase.");
                segment
                    .bg(palette.unbroken)
                    .border_2()
                    .border_dashed()
                    .border_color(palette.phase(phase))
            } else {
                segment.bg(palette.phase(phase))
            };
            segment.tooltip(TextTooltip::builder(tip.into(), palette.tooltip))
        });
    div()
        .flex()
        .flex_row()
        .gap(SEGMENT_GAP)
        .h(BAR_HEIGHT)
        .w(relative(fraction(total, longest)))
        .children(segments)
}

/// `part` of `whole`, as a length relative to the length `whole` takes.
fn fraction(part: Duration, whole: Duration) -> f32 {
    if whole.is_zero() {
        0.0
    } else {
        (part.as_secs_f64() / whole.as_secs_f64()) as f32
    }
}

impl EventEmitter<PanelEvent> for SpawnTimePanel {}

impl Focusable for SpawnTimePanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Panel for SpawnTimePanel {
    fn kind() -> PanelKind {
        PanelKind {
            view: ViewKind::new("execlog-spawn-time"),
            label: "Spawn time by phase",
            key_context: "ExecLogSpawnTime",
        }
    }

    fn title(&self, _cx: &App) -> SharedString {
        Self::kind().label.into()
    }

    fn status(&self, cx: &App) -> Option<SharedString> {
        let status = self
            .model
            .read(cx)
            .source()
            .status("No execution log", |summary| {
                format!("{} spawns", summary.spawns)
            });
        Some(status.into())
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if active {
            self.focus.focus(window, cx);
        }
    }
}

impl Render for SpawnTimePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::from_theme(&bviz_ui_theme::theme(window, cx));
        let panel = div()
            .key_context(Self::kind().key_context)
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .bg(palette.colors.background)
            .text_color(palette.colors.text)
            .text_sm();
        match &self.shown {
            Some(shown) => panel.child(self.body(shown, &palette, cx)),
            None => panel.children(self.model.read(cx).source().placeholder(&palette.colors)),
        }
    }
}
