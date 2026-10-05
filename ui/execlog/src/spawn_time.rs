use crate::format::{duration, exact_duration, phase_name, share};
use crate::model::ExecLogModel;
use crate::model::Source;
use crate::palette::Palette;
use crate::summary::{GroupRow, Runner, Summary, TopSpawn};
use bviz_execlog::{Phase, PhaseTiming};
use bviz_ui_components::{
    Dropdown, DropdownEvent, ListScroll, SCROLLBAR_THICKNESS, TextTooltip, list_scrollbar,
};
use bviz_ui_workspace::{Panel, PanelEvent, PanelKind, ViewKind};
use gpui::{
    AnyElement, App, Context, Div, ElementId, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, MouseButton, MouseDownEvent, Pixels, Render, SharedString, Stateful, Subscription,
    Window, div, prelude::*, px, relative,
};
use std::cmp::Ordering;
use std::time::Duration;

const ROW_HEIGHT: Pixels = px(24.);
const TWO_LINE_ROW_HEIGHT: Pixels = px(44.);
const BAR_HEIGHT: Pixels = px(14.);
/// The canvas showing between two segments of a bar, so neighbouring phases
/// part by more than their colours.
const SEGMENT_GAP: Pixels = px(2.);
/// The narrowest a segment is drawn, so even a sliver of a phase can be
/// hovered for its time.
const MIN_SEGMENT_WIDTH: Pixels = px(4.);
const CELL_PAD: Pixels = px(6.);
const SWATCH: Pixels = px(10.);
const CARET: Pixels = px(12.);
const NAME_WIDTH: Pixels = px(220.);
const NUMBER_WIDTH: Pixels = px(110.);
const NOTE_WIDTH: Pixels = px(110.);

/// What a table of groups is ordered by.
#[derive(Clone, Copy, PartialEq)]
enum SortBy {
    Name,
    Spawns,
    Time,
}

#[derive(Clone, Copy, PartialEq)]
struct Sort {
    by: SortBy,
    descending: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Sort {
            by: SortBy::Time,
            descending: true,
        }
    }
}

impl Sort {
    /// The sort a click on `by`'s title asks for: the other direction where
    /// the table is sorted by it, and otherwise names from the start of the
    /// alphabet and counts from the largest.
    fn clicked(self, by: SortBy) -> Sort {
        if self.by == by {
            Sort {
                by,
                descending: !self.descending,
            }
        } else {
            Sort {
                by,
                descending: by != SortBy::Name,
            }
        }
    }

    /// Rows equal in what the sort is by go in order of name.
    fn cmp(self, a: &GroupRow, b: &GroupRow) -> Ordering {
        let by = match self.by {
            SortBy::Name => a.name.cmp(&b.name),
            SortBy::Spawns => a.spawns.cmp(&b.spawns),
            SortBy::Time => a.timing.total().cmp(&b.timing.total()),
        };
        let by = if self.descending { by.reverse() } else { by };
        by.then_with(|| a.name.cmp(&b.name))
    }

    fn apply(self, rows: &mut [GroupRow]) {
        rows.sort_by(|a, b| self.cmp(a, b));
    }
}

/// What the panel shows of a loaded log.
struct Shown {
    summary: Summary,
    /// Picks the runner, of the summary's, whose spawns the mnemonic table
    /// sums: one runner's spawns report one set of phases, where several
    /// runners' might mix sets.
    runner_picker: Entity<Dropdown>,
    _runner_picked: Subscription,
    sort: Sort,
    /// Picks the phase the list of spawns longest in a phase ranks by.
    phase_picker: Entity<Dropdown>,
    _phase_picked: Subscription,
}

impl Shown {
    /// The runner the mnemonic table sums the spawns of.
    fn runner(&self, cx: &App) -> &Runner {
        &self.summary.runners[self.runner_picker.read(cx).selected()]
    }

    fn phase(&self, cx: &App) -> Phase {
        Phase::ALL[self.phase_picker.read(cx).selected()]
    }
}

/// The spawn time of an execution log by phase: summed per runner and per
/// mnemonic, and the spawns that spent longest in one phase.
pub struct SpawnTimePanel {
    focus: FocusHandle,
    model: Entity<ExecLogModel>,
    /// `None` until the log is loaded, the model's placeholder showing instead.
    shown: Option<Shown>,
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
        let Source::Loaded(summary) = self.model.read(cx).source() else {
            return;
        };
        let mut summary = summary.clone();
        let sort = Sort::default();
        for runner in &mut summary.runners {
            sort.apply(&mut runner.mnemonics);
        }
        let names: Vec<SharedString> = summary
            .runners
            .iter()
            .map(|runner| runner.sum.name.clone())
            .collect();
        let runner_picker = cx.new(|cx| Dropdown::new("mnemonic-runner", names, 0, cx));
        let runner_picked = cx.subscribe(&runner_picker, |_, _, _: &DropdownEvent, cx| {
            cx.notify();
        });
        let phase = summary.longest_phase();
        let phase_picker =
            cx.new(|cx| Dropdown::new("top-phase", Phase::ALL.map(phase_name), phase.index(), cx));
        let phase_picked = cx.subscribe(&phase_picker, |_, _, _: &DropdownEvent, cx| {
            cx.notify();
        });
        self.shown = Some(Shown {
            summary,
            runner_picker,
            _runner_picked: runner_picked,
            sort,
            phase_picker,
            _phase_picked: phase_picked,
        });
    }

    fn sort_mnemonics(&mut self, by: SortBy, cx: &mut Context<Self>) {
        if let Some(shown) = &mut self.shown {
            shown.sort = shown.sort.clicked(by);
            for runner in &mut shown.summary.runners {
                shown.sort.apply(&mut runner.mnemonics);
            }
            cx.notify();
        }
    }

    fn body(&self, shown: &Shown, palette: &Palette, cx: &mut Context<Self>) -> Div {
        let summary = &shown.summary;
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
                let runners: Vec<GroupRow> =
                    summary.runners.iter().map(|r| r.sum.clone()).collect();
                let phase = shown.phase(cx);
                content
                    .child(section(
                        "By runner",
                        None,
                        group_table("runner", plain_header("Runner", palette), &runners, palette),
                        palette,
                    ))
                    .child(section(
                        "By mnemonic",
                        Some(labelled("Runner", shown.runner_picker.clone())),
                        group_table(
                            "mnemonic",
                            sorting_header(shown.sort, palette, cx),
                            &shown.runner(cx).mnemonics,
                            palette,
                        ),
                        palette,
                    ))
                    .child(section(
                        "Longest in a phase",
                        Some(labelled("Phase", shown.phase_picker.clone())),
                        top_table(phase, &summary.top[phase.index()], palette),
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

/// `control` behind a label naming what it picks.
fn labelled(label: &'static str, control: impl IntoElement) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(label)
        .child(control)
        .into_any_element()
}

/// A titled part of the panel, with the controls acting on it beside the title.
fn section(
    title: &'static str,
    controls: Option<AnyElement>,
    content: impl IntoElement,
    palette: &Palette,
) -> Div {
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
                .child(div().text_base().child(title))
                .children(controls),
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

/// The titles of [`GROUP_COLUMNS`] after the first, which names what the
/// table groups by.
const GROUP_TITLES: [&str; GROUP_COLUMNS.len() - 1] = ["Spawns", "Spawn time", "By phase", ""];

fn header_row(cells: impl IntoIterator<Item = impl IntoElement>, palette: &Palette) -> Div {
    div()
        .flex()
        .flex_row()
        .w_full()
        .h(ROW_HEIGHT)
        .text_color(palette.colors.text_muted)
        .children(cells)
}

/// The header of a table no title of which sorts it.
fn plain_header(name_title: &'static str, palette: &Palette) -> Div {
    let titles = std::iter::once(name_title).chain(GROUP_TITLES);
    header_row(
        GROUP_COLUMNS
            .into_iter()
            .zip(titles)
            .map(|(column, title)| cell(column, title)),
        palette,
    )
}

/// The header of a table of mnemonics, whose titles but the bar's and the
/// note's sort it.
fn sorting_header(sort: Sort, palette: &Palette, cx: &mut Context<SpawnTimePanel>) -> Div {
    let [name, spawns, time, bar, note] = GROUP_COLUMNS;
    let [spawns_title, time_title, bar_title, note_title] = GROUP_TITLES;
    let sorting = |column: Column, title: &'static str, by: SortBy| {
        let caret = (sort.by == by).then(|| {
            let icon = if sort.descending {
                "caret-down"
            } else {
                "caret-up"
            };
            bviz_ui_assets::icon(icon)
                .size(CARET)
                .flex_shrink_0()
                .text_color(palette.colors.text_muted)
        });
        let content = div().flex().items_center().gap_1();
        // The caret stands on the side away from the title's alignment, so
        // the title holds still as the sort moves between columns.
        let content = if column.numeric {
            content.children(caret).child(title)
        } else {
            content.child(title).children(caret)
        };
        cell(column, content)
            .id(title)
            .cursor_pointer()
            .hover(|style| style.bg(palette.colors.element_hover))
            .on_click(cx.listener(move |panel, _, _, cx| panel.sort_mnemonics(by, cx)))
            .into_any_element()
    };
    header_row(
        [
            sorting(name, "Mnemonic", SortBy::Name),
            sorting(spawns, spawns_title, SortBy::Spawns),
            sorting(time, time_title, SortBy::Time),
            cell(bar, bar_title).into_any_element(),
            cell(note, note_title).into_any_element(),
        ],
        palette,
    )
}

/// One row per group of `rows` under `header`, each bar as long against the
/// longest as its spawn time is.
fn group_table(id: &'static str, header: Div, rows: &[GroupRow], palette: &Palette) -> Div {
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
    div().flex().flex_col().child(header).children(body)
}

/// The columns of the list of spawns longest in a phase: its time in the
/// phase, its mnemonic, its runner, and what the spawn made.
const TOP_COLUMNS: [Column; 4] = [
    number(Width::Fixed(NUMBER_WIDTH)),
    text(Width::Fixed(NAME_WIDTH)),
    text(Width::Fixed(NAME_WIDTH)),
    text(Width::Rest),
];

/// A line of a cell, cut short where the cell runs out of room for it.
fn line(text: SharedString) -> Div {
    div().min_w_0().text_ellipsis().child(text)
}

/// `spawns`, longest in `phase` first, each named by its target and output.
fn top_table(phase: Phase, spawns: &[TopSpawn], palette: &Palette) -> Div {
    if spawns.is_empty() {
        return div()
            .text_color(palette.colors.text_muted)
            .child(format!("No spawn spent time in {}.", phase_name(phase)));
    }
    let [time, mnemonic, runner, made] = TOP_COLUMNS;
    let titles = ["Time in phase", "Mnemonic", "Runner", "Target and output"];
    let header = header_row(
        TOP_COLUMNS
            .into_iter()
            .zip(titles)
            .map(|(column, title)| cell(column, title)),
        palette,
    );
    let body = spawns.iter().enumerate().map(|(index, spawn)| {
        let (first, second) = match (&spawn.target_label, &spawn.primary_output) {
            (Some(label), output) => (label.clone(), output.clone()),
            (None, Some(output)) => (output.clone(), None),
            (None, None) => ("no target or output".into(), None),
        };
        row(
            ("top", index),
            [
                cell(time, duration(spawn.time)),
                cell(mnemonic, line(spawn.mnemonic.clone())),
                cell(runner, line(spawn.runner.clone())),
                cell(
                    made,
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(line(first))
                        .children(second.map(|output| {
                            line(output).text_xs().text_color(palette.colors.text_muted)
                        })),
                ),
            ],
        )
        .h(TWO_LINE_ROW_HEIGHT)
    });
    div().flex().flex_col().child(header).children(body)
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
fn phase_bar(timing: &PhaseTiming, longest: Duration, palette: &Palette) -> Div {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(named: &[(&str, usize)]) -> Vec<GroupRow> {
        named
            .iter()
            .map(|&(name, spawns)| GroupRow {
                name: name.to_owned().into(),
                spawns,
                timing: PhaseTiming::default(),
            })
            .collect()
    }

    fn names(rows: &[GroupRow]) -> Vec<&str> {
        rows.iter().map(|row| row.name.as_ref()).collect()
    }

    #[test]
    fn a_click_on_a_new_title_sorts_names_up_and_counts_down() {
        let by_name = Sort::default().clicked(SortBy::Name);
        assert!(!by_name.descending);
        assert!(by_name.clicked(SortBy::Spawns).descending);
    }

    #[test]
    fn a_click_on_the_sorting_title_turns_the_order_round() {
        let by_spawns = Sort::default().clicked(SortBy::Spawns);
        assert!(!by_spawns.clicked(SortBy::Spawns).descending);
        assert!(!Sort::default().clicked(SortBy::Time).descending);
    }

    #[test]
    fn rows_take_the_order_of_their_sort() {
        let mut table = rows(&[("CppLink", 2), ("CppCompile", 9), ("Genrule", 4)]);
        Sort::default().clicked(SortBy::Name).apply(&mut table);
        assert_eq!(names(&table), ["CppCompile", "CppLink", "Genrule"]);
        Sort::default().clicked(SortBy::Spawns).apply(&mut table);
        assert_eq!(names(&table), ["CppCompile", "Genrule", "CppLink"]);
    }

    #[test]
    fn ties_go_in_order_of_name_either_way() {
        let mut table = rows(&[("b", 1), ("c", 2), ("a", 1)]);
        let ascending = Sort::default()
            .clicked(SortBy::Spawns)
            .clicked(SortBy::Spawns);
        ascending.apply(&mut table);
        assert_eq!(names(&table), ["a", "b", "c"]);
        ascending.clicked(SortBy::Spawns).apply(&mut table);
        assert_eq!(names(&table), ["c", "a", "b"]);
    }
}
