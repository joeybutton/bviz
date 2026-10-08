//! A scatter plot of a graph's actions, configured targets or targets by two
//! of: how much of the build each pulls in, how much of it depends on it, and
//! how long its actions took.

mod axis;
mod hue;
mod paint;
mod pick;
mod placed;
mod plot;

use crate::Palette;
use crate::colour;
use crate::drag::{DOUBLE_CLICK, past_slop};
use crate::hue::{Hue, Swatch, legend_strip};
use crate::model::{FilterChanged, GraphModel, Loaded, MetricsLanded, SourceReplaced};
use crate::paint::UNMARKED_FADE;
use crate::point::{
    configuration_readings, identify, mnemonic, point_label, primary_output, unit_names,
};
use crate::timing::{NodeTimes, Untimed};
use crate::toolbar::{busy, group};
use crate::unit_picker::UnitPicker;
use axis::{Axes, Axis, Clocks};
use bviz_bazel_flags::TRANSITIONS;
use bviz_graph_layout::metrics::{Measurement, Metrics, Scope, Sizing, Unit};
use bviz_graph_layout::select::Filter;
use bviz_graph_layout::{Capabilities, Input, InputSet, Kind, Support};
use bviz_invocation::Revision;
use bviz_ui_components::gesture::{self, Clicks, Entry, GestureRow, Gestures, Held, Motion, Place};
use bviz_ui_components::{
    Button, DragEnd, Dropdown, DropdownEvent, KeyScope, ListScroll, Nesting, SCROLLBAR_THICKNESS,
    TextTooltip, Toolbar, TooltipColors, ZOOM_KEY_STEP, ZoomIn, ZoomOut, cached_layer,
    centered_spinner, list_scrollbar, load_in_background, menu_chrome, paint_tooltip,
    paint_tooltip_box, tooltip_extent, track_drag,
};
use bviz_ui_components::{KeyStyle, key_text_in};
use bviz_ui_workspace::{Panel, PanelEvent, PanelKind, Role, ViewKind};
use gpui::{
    App, Bounds, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, Hsla,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PinchEvent, Pixels,
    Point, Rems, Render, Rgba, ScrollStrategy, ScrollWheelEvent, SharedString, Size, Stateful,
    Subscription, Task, WeakEntity, Window, canvas, div, point, prelude::*, px, rems, uniform_list,
};
use hue::{ColourBy, Colouring};
use menu_chrome::{ROW_INSET, SHEET_BORDER};
use pick::{Extents, PickList, PickRow};
use placed::Placed;
use plot::{MIN_RADIUS, Mark, Picked, Plotted, Spot, Zoom};
use std::rc::Rc;
use std::sync::Arc;

/// The key context scoping the scatter plot's own bindings, so a key pressed
/// here reaches this panel and no other.
pub(crate) const KEY_CONTEXT: KeyScope = KeyScope::new("GraphScatter", "Scatter");

/// The identifier the scatter plot's key context carries while a list of the
/// points under a click is up, which the list's own bindings require.
pub(crate) const PICKING: KeyScope =
    KeyScope::within(&KEY_CONTEXT, Nesting::State, "picking", "Pick list");

gpui::actions!(
    bviz_graph,
    [
        /// Show whole plot
        ShowWholePlot,
        /// Aim at previous point
        AimUp,
        /// Aim at next point
        AimDown,
        /// Show point in graph
        RevealAimed,
        /// Close pick list
        ClosePickList,
    ]
);

/// What the panel reads while the graph is being measured.
const MEASURING: &str = "Computing metrics…";

/// The points of a graph, placed by two of their measures.
pub struct ScatterPanel {
    focus: FocusHandle,
    /// Measures the graph the first time this panel is shown.
    model: Entity<GraphModel>,
    /// Whether the workspace is showing the panel, which a graph landing while
    /// it is shown is measured for at once.
    shown: bool,
    /// What the plot measures, from the time the graph is read.
    choice: Option<Choice>,
    /// The measured points as drawn, from the time they land.
    plot: Option<Plot>,
    /// The part of each axis the plot drawn shows, which outlasts a replot in
    /// the same unit, since a filter moves no mark, and is the whole of each
    /// axis for the first plot of a unit.
    zoom: Zoom,
    /// The layer drawing the marks; notified through
    /// [`Self::invalidate_canvas`].
    canvas: Entity<ScatterCanvas>,
    /// Where the plot was last laid out, which a pointer is resolved against.
    bounds: Option<Bounds<Pixels>>,
    /// The press over the plot, until the button comes up again.
    press: Option<Press>,
    /// How far the list of the points under a click has scrolled.
    pick_scroll: ListScroll,
    /// Whether the last click revealed a row of the list of points, whose
    /// double click's second press then lands on the plot the list stood over.
    row_revealed: bool,
    /// Closes the list of the points under a click as focus leaves the panel;
    /// made the first time the panel is shown, where the window is in hand.
    closes_on_blur: Option<Subscription>,
    /// Plots the points through the graph's filter, absent while nothing is
    /// being plotted, so that one plotting runs at a time.
    plotting: Option<Task<()>>,
    /// Redraws as the graph is read and measured.
    _changed: Subscription,
    _source_replaced: Subscription,
    _metrics_landed: Subscription,
    _filter_changed: Subscription,
}

impl ScatterPanel {
    pub(crate) fn new(model: Entity<GraphModel>, cx: &mut Context<Self>) -> Self {
        let changed = cx.observe(&model, |view, _, cx| {
            view.replot(cx);
            cx.notify();
        });
        let source_replaced = cx.subscribe(&model, |view, _, _: &SourceReplaced, cx| {
            view.plot = None;
            view.plotting = None;
            view.choice = Choice::of(&view.model, cx);
            if view.shown {
                view.measure(cx);
            }
        });
        let metrics_landed = cx.subscribe(&model, |view, _, landed: &MetricsLanded, cx| {
            let (scope, reference) = view.scopes(cx).unzip();
            if [scope, reference].contains(&Some(landed.key)) {
                view.replot(cx);
            }
        });
        let filter_changed = cx.subscribe(&model, |view, _, _: &FilterChanged, cx| {
            view.close_pick(cx);
            view.replot(cx);
        });
        let parent = cx.entity().downgrade();
        Self {
            focus: cx.focus_handle(),
            choice: Choice::of(&model, cx),
            model,
            shown: false,
            plot: None,
            zoom: Zoom::WHOLE,
            canvas: cx.new(|_| ScatterCanvas { parent }),
            bounds: None,
            press: None,
            pick_scroll: ListScroll::new(),
            row_revealed: false,
            closes_on_blur: None,
            plotting: None,
            _changed: changed,
            _source_replaced: source_replaced,
            _metrics_landed: metrics_landed,
            _filter_changed: filter_changed,
        }
    }

    /// The scope the plot measures in, and the scope of its unit over every
    /// input loaded that it is scaled to, once the graph is read.
    fn scopes(&self, cx: &App) -> Option<(Scope, Scope)> {
        self.choice.as_ref().map(|choice| choice.scopes(cx))
    }

    /// Has the graph measured in the chosen scope and the one it is scaled to,
    /// and brings the plot up to date where it already is.
    fn measure(&mut self, cx: &mut Context<Self>) {
        self.close_pick(cx);
        if let Some((scope, reference)) = self.scopes(cx) {
            self.model
                .update(cx, |model, cx| model.want_metrics(&[scope, reference], cx));
        }
        self.replot(cx);
    }

    /// Measures the graph's points in the unit chosen, over the inputs last
    /// enabled for it.
    fn choose_unit(&mut self, cx: &mut Context<Self>) {
        if let Some(choice) = &self.choice {
            choice.name_colourings(cx);
        }
        self.measure(cx);
        cx.notify();
    }

    /// Colours the marks `by` what the picker offers.
    fn choose_colour(&mut self, by: ColourBy, cx: &mut Context<Self>) {
        let Some(choice) = &mut self.choice else {
            return;
        };
        if choice.colour == by {
            return;
        }
        choice.colour = by;
        self.close_pick(cx);
        self.replot(cx);
        cx.notify();
    }

    /// Places the points by the axes `choose` makes of those chosen.
    fn choose_axes(&mut self, choose: impl FnOnce(Axes) -> Axes, cx: &mut Context<Self>) {
        let Some(choice) = &mut self.choice else {
            return;
        };
        let axes = choose(choice.axes);
        if choice.axes == axes {
            return;
        }
        choice.axes = axes;
        choice.show_axes(cx);
        self.close_pick(cx);
        self.replot(cx);
        cx.notify();
    }

    /// Offers the choices reading action time only where the unit plotted is
    /// `timed`, taking down a list of points drawn by one it falls back from.
    fn offer_times(&mut self, timed: bool, cx: &mut Context<Self>) {
        let Some(choice) = &mut self.choice else {
            return;
        };
        if choice.offer_times(timed, cx) {
            self.close_pick(cx);
            cx.notify();
        }
    }

    /// Enables `input` where it is disabled and disables it where it is
    /// enabled, unless that leaves no input describing the unit plotted.
    fn toggle_input(&mut self, input: Input, cx: &mut Context<Self>) {
        let Some(choice) = &mut self.choice else {
            return;
        };
        let inputs = choice.picker.chosen(cx).inputs();
        let inputs = match inputs.contains(input) {
            true => inputs.without(input),
            false => Some(inputs.with(input)),
        };
        if !inputs.is_some_and(|inputs| choice.picker.draw_over(inputs, cx)) {
            return;
        }
        self.measure(cx);
        cx.notify();
    }

    /// Plots the measured points through the filter the graph's other views
    /// draw through, where the plot drawn is not of the chosen scope, axes and
    /// colouring, not drawn through that filter, or not of the action times
    /// the graph holds, once the choices reading action time are offered as
    /// the unit's measure has any. A hidden panel waits until it is shown, and
    /// a plotting running replots once it lands. The filter is evaluated over
    /// every node of the graph, so this runs off the UI thread, and the plot
    /// drawn stays until the new one lands.
    fn replot(&mut self, cx: &mut Context<Self>) {
        if !self.shown || self.plotting.is_some() {
            return;
        }
        let Some((_, reference)) = self.scopes(cx) else {
            return;
        };
        let model = self.model.read(cx);
        let timed = model
            .loaded()
            .zip(model.metrics(reference))
            .map(|(loaded, reference)| any_timed(loaded.times(), reference));
        if let Some(timed) = timed {
            self.offer_times(timed, cx);
        }
        let Some(((scope, reference), choice)) = self.scopes(cx).zip(self.choice.as_ref()) else {
            return;
        };
        let (axes, colour) = (choice.axes, choice.colour);
        let model = self.model.read(cx);
        let Some(loaded) = model.loaded() else {
            return;
        };
        let timed = loaded.timed();
        let (Some(metrics), Some(reference)) = (model.metrics(scope), model.metrics(reference))
        else {
            return;
        };
        let filter = model.filter().cloned().unwrap_or_default();
        if self.plot.as_ref().is_some_and(|plot| {
            plot.filter == filter
                && plot.scope == scope
                && plot.plotted.axes == axes
                && plot.colour == colour
                && plot.timed == timed
        }) {
            return;
        }
        let clocks = Clocks::of(loaded.times(), &reference.points);
        let (metrics, reference) = (Arc::clone(metrics), Arc::clone(reference));
        let shared = loaded.shared.clone();
        self.plotting = Some(load_in_background(
            cx,
            move || {
                let verdicts = filter.evaluate(&shared.base);
                let plots = |point: &Metrics| plotted_as(scope.unit(), point.kind);
                let colouring = Colouring::of(colour, scope.unit(), &reference, &shared.facets);
                let hue = |point: &Metrics| colouring.hue(point, &shared.facets, &clocks);
                let verdict = |id| verdicts.get(id);
                let time = |id| clocks.time(id);
                let plotted = Plotted::of(&metrics, &reference, axes, time, plots, verdict, hue);
                let points = metrics
                    .points
                    .iter()
                    .filter(|point| plots(point) && axes.place(point, time(point.id)).is_some());
                let legend = colouring.legend(points, &shared.facets, &clocks);
                (plotted, legend, filter)
            },
            move |view, (plotted, legend, filter), cx| {
                view.plotting = None;
                if view.plot.as_ref().is_none_or(|plot| {
                    plot.scope.unit() != scope.unit()
                        || plot.plotted.axes != axes
                        || (plot.timed != timed && axes.reads_time())
                }) {
                    view.zoom = Zoom::WHOLE;
                }
                view.plot = Some(Plot {
                    plotted: Arc::new(plotted),
                    legend,
                    pointer: Pointer::Away,
                    filter,
                    scope,
                    colour,
                    timed,
                });
                view.invalidate_canvas(cx);
                view.replot(cx);
            },
        ));
    }

    /// Repaints the marks along with the layer over them; every change to what
    /// the marks show routes through here.
    fn invalidate_canvas(&self, cx: &mut Context<Self>) {
        self.canvas.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    /// Where the plot places values, where it is drawn.
    fn area(&self) -> Option<plot::DataArea> {
        paint::data_area(&self.plot.as_ref()?.plotted, self.zoom, self.bounds?)
    }

    /// The mark a pointer at `at` picks, where the plot is drawn.
    fn pick(&self, at: Point<Pixels>) -> Option<Picked> {
        let marks = &self.plot.as_ref()?.plotted.marks;
        self.area()?.pick(marks, at)
    }

    /// Shows the plot at `zoom`, and names what the pointer at `at` is over
    /// there unless a press is in progress, whose release names it.
    fn zoom_to(&mut self, zoom: Zoom, at: Point<Pixels>, cx: &mut Context<Self>) {
        if zoom == self.zoom {
            return;
        }
        self.zoom = zoom;
        self.close_pick(cx);
        self.invalidate_canvas(cx);
        if self.press.is_none() {
            self.hover_at(at, cx);
        }
    }

    fn on_scroll_wheel(&mut self, e: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let delta = gesture::scroll_pixels(e);
        GESTURES.dispatch(gesture::Pointer::scroll(e), |intent| {
            let by = match intent {
                ScatterIntent::Zoom => {
                    self.zoom_about(e.position, gesture::wheel_zoom_factor(e), cx);
                    return true;
                }
                ScatterIntent::PanAcross => point(delta.x + delta.y, px(0.)),
                ScatterIntent::Scroll => delta,
                _ => return false,
            };
            if let Some(area) = self.area() {
                self.zoom_to(area.panned(by), e.position, cx);
            }
            true
        });
    }

    fn on_pinch(&mut self, e: &PinchEvent, cx: &mut Context<Self>) {
        GESTURES.dispatch(gesture::Pointer::pinch(e), |intent| match intent {
            ScatterIntent::Zoom => {
                self.zoom_about(e.position, gesture::pinch_factor(e), cx);
                true
            }
            _ => false,
        });
    }

    /// Zooms `factor` times closer about the window-space point `at`.
    fn zoom_about(&mut self, at: Point<Pixels>, factor: f32, cx: &mut Context<Self>) {
        if let Some(area) = self.area() {
            self.zoom_to(area.zoomed(at, factor), at, cx);
        }
    }

    /// Zooms `factor` times closer about the middle of the plot, naming what
    /// the pointer is over there.
    fn zoom_centred(&mut self, factor: f32, window: &Window, cx: &mut Context<Self>) {
        if let Some(area) = self.area() {
            let zoom = area.zoomed(area.frame().center(), factor);
            self.zoom_to(zoom, window.mouse_position(), cx);
        }
    }

    /// Acts on a press as [`GESTURES`] has it. A double click shows the whole
    /// of each axis only clear of every mark: a first click on a mark shows it
    /// in the graph, which the second click then lands on. A press beside an
    /// open list of points closes it and goes on as a press on the plot. The
    /// second press of a double click whose first put the list up or revealed
    /// one of its rows goes no further.
    fn on_mouse_down(&mut self, e: &MouseDownEvent, cx: &mut Context<Self>) {
        let row_revealed = std::mem::take(&mut self.row_revealed);
        if e.click_count >= DOUBLE_CLICK && (row_revealed || self.picking()) {
            return;
        }
        self.close_pick(cx);
        GESTURES.dispatch(gesture::Pointer::press(e), |intent| match intent {
            ScatterIntent::ShowWholePlot if self.pick(e.position).is_none() => {
                self.press = None;
                self.zoom_to(Zoom::WHOLE, e.position, cx);
                true
            }
            ScatterIntent::Pan => {
                self.press = Some(Press {
                    from: e.position,
                    last: e.position,
                    dragged: false,
                    click: gesture::Pointer::completing(e),
                });
                // The frame this asks for installs the handlers carrying the
                // drag.
                cx.notify();
                true
            }
            _ => false,
        });
    }

    /// Follows the pointer to the mark it picks, repainting only where that
    /// changes what the tooltip reads. An open list of points holds the
    /// pointer, which names nothing else while it is up.
    fn hover_at(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let picked = self.pick(at);
        let model = self.model.read(cx);
        let Some(plot) = &mut self.plot else {
            return;
        };
        let named = match &plot.pointer {
            Pointer::Picking(_) => return,
            Pointer::Away => None,
            Pointer::Hover(hover) => Some((hover.mark, hover.others)),
        };
        if named == picked.as_ref().map(|picked| (picked.mark, picked.others())) {
            return;
        }
        let unit = plot.scope.unit();
        let hover = picked.and_then(|picked| {
            let mark = &plot.plotted.marks[picked.mark];
            let lines = describe(mark, unit, &plot.plotted, picked.others(), model.loaded()?);
            Some(Hover {
                mark: picked.mark,
                others: picked.others(),
                lines,
            })
        });
        plot.pointer = hover.map_or(Pointer::Away, Pointer::Hover);
        cx.notify();
    }

    /// Carries the zoom along with a press dragged past a click's reach, which
    /// puts away the tooltip until the release.
    fn drag_to(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(press) = &mut self.press else {
            return;
        };
        let starts = !press.dragged && past_slop(press.from, at);
        press.dragged |= starts;
        if !press.dragged {
            return;
        }
        let by = at - press.last;
        press.last = at;
        if starts {
            self.clear_hover(cx);
        }
        if let Some(area) = self.area() {
            self.zoom_to(area.panned(by), at, cx);
        }
    }

    /// Ends the press, which where it was a click acts as [`GESTURES`] has
    /// it. Taking the press makes a second call for the same release a no-op.
    fn end_press(&mut self, at: Point<Pixels>, end: DragEnd, cx: &mut Context<Self>) {
        let Some(press) = self.press.take() else {
            return;
        };
        cx.notify();
        self.hover_at(at, cx);
        if press.dragged || !matches!(end, DragEnd::Released) {
            return;
        }
        GESTURES.dispatch(press.click, |intent| match intent {
            ScatterIntent::Reveal => {
                self.reveal_at(at, cx);
                true
            }
            _ => false,
        });
    }

    /// Shows the graph's drawing of the point under `at`, or lists the points
    /// there where it lands on several.
    fn reveal_at(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let (Some(picked), Some(area)) = (self.pick(at), self.area()) else {
            return;
        };
        match picked.covered() {
            &[mark] => self.reveal(mark, cx),
            _ => self.open_pick(area.spot(at), picked, cx),
        }
    }

    /// Shows the graph's drawing of the point the plotted mark `mark` stands
    /// for.
    fn reveal(&mut self, mark: usize, cx: &mut Context<Self>) {
        let Some(plot) = &self.plot else {
            return;
        };
        let id = plot.plotted.marks[mark].target.id;
        self.model.update(cx, |model, cx| model.reveal(id, cx));
    }

    /// Drops the tooltip, which names the mark under a pointer that has left.
    fn clear_hover(&mut self, cx: &mut Context<Self>) {
        if let Some(plot) = &mut self.plot
            && let Pointer::Hover(_) = plot.pointer
        {
            plot.pointer = Pointer::Away;
            cx.notify();
        }
    }

    /// Whether a list of the points under a click is up.
    fn picking(&self) -> bool {
        self.plot
            .as_ref()
            .is_some_and(|plot| matches!(plot.pointer, Pointer::Picking(_)))
    }

    /// Puts up the list of every point `picked` covers under a click at `spot`.
    fn open_pick(&mut self, spot: Spot, picked: Picked, cx: &mut Context<Self>) {
        let model = self.model.read(cx);
        let (Some(plot), Some(loaded)) = (&mut self.plot, model.loaded()) else {
            return;
        };
        let (unit, covered) = (plot.scope.unit(), picked.covered());
        let mut named = Vec::with_capacity(covered.len());
        let mut readings = Vec::with_capacity(covered.len());
        for &mark in covered {
            let (mnemonic, label, read) =
                pick_reading(&plot.plotted.marks[mark].target, unit, loaded);
            named.push((mark, mnemonic, label));
            readings.push(read);
        }
        let keys: Vec<(Option<&str>, &str)> = named
            .iter()
            .map(|(_, mnemonic, label)| (*mnemonic, label.as_str()))
            .collect();
        let hints = crate::hints::hints(&keys, readings);
        let mut rows: Vec<PickRow> = named
            .iter()
            .zip(hints)
            .map(|((mark, mnemonic, label), hint)| {
                let drawn = &plot.plotted.marks[*mark];
                let point = &drawn.target;
                let (package, name) = pick::split_label(label);
                PickRow {
                    mark: *mark,
                    mnemonic: mnemonic.map(|mnemonic| mnemonic.to_owned().into()),
                    package: package.to_owned().into(),
                    name: name.to_owned().into(),
                    hint: hint.map(SharedString::from),
                    measures: plot.plotted.axes.values(point, drawn.time).into(),
                    x: drawn.x,
                    y: drawn.y,
                    tooltip: describe(drawn, unit, &plot.plotted, 0, loaded),
                }
            })
            .collect();
        pick::order(&mut rows);
        plot.pointer = Pointer::Picking(PickList {
            spot,
            rows: rows.into(),
            aim: None,
            extents: None,
        });
        self.pick_scroll.to_top();
        cx.notify();
    }

    /// Takes down the list of the points under a click, where one is up.
    fn close_pick(&mut self, cx: &mut Context<Self>) {
        if let Some(plot) = &mut self.plot
            && let Pointer::Picking(_) = plot.pointer
        {
            plot.pointer = Pointer::Away;
            cx.notify();
        }
    }

    /// Aims at the list's row `row`, which rings its mark and describes its
    /// point beside the list.
    fn aim_at(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(Pointer::Picking(pick)) = self.plot.as_mut().map(|plot| &mut plot.pointer) else {
            return;
        };
        if pick.aim == Some(row) || row >= pick.rows.len() {
            return;
        }
        pick.aim = Some(row);
        cx.notify();
    }

    /// Moves the aim `by` rows through the list, and brings the row it lands
    /// on into view.
    fn step_aim(&mut self, by: isize, cx: &mut Context<Self>) {
        let Some(Pointer::Picking(pick)) = self.plot.as_ref().map(|plot| &plot.pointer) else {
            return;
        };
        if let Some(row) = pick::stepped(pick.aim, by, pick.rows.len()) {
            self.aim_at(row, cx);
            let scroll = self.pick_scroll.uniform_handle();
            scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
    }

    /// Shows the graph's drawing of the point on the list's row `row`, and
    /// takes the list down.
    fn reveal_row(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(Pointer::Picking(pick)) = self.plot.as_ref().map(|plot| &plot.pointer) else {
            return;
        };
        let Some(mark) = pick.rows.get(row).map(|listed| listed.mark) else {
            return;
        };
        self.close_pick(cx);
        self.reveal(mark, cx);
        self.row_revealed = true;
    }

    /// Shows the graph's drawing of the point on the row aimed at.
    fn reveal_aimed(&mut self, cx: &mut Context<Self>) {
        let Some(Pointer::Picking(pick)) = self.plot.as_ref().map(|plot| &plot.pointer) else {
            return;
        };
        if let Some(row) = pick.aim {
            self.reveal_row(row, cx);
        }
    }
}

/// The choice of what the plot measures, which the strip offers.
struct Choice {
    /// The inputs the graph was read from.
    loaded: InputSet,
    picker: UnitPicker,
    axes: Axes,
    /// Whether any point of the unit plotted has an action time, which the
    /// choices reading it are offered by.
    timed: bool,
    across_picker: Entity<Dropdown>,
    _across_picked: Subscription,
    up_picker: Entity<Dropdown>,
    _up_picked: Subscription,
    colour: ColourBy,
    colour_picker: Entity<Dropdown>,
    _colour_picked: Subscription,
}

impl Choice {
    /// The finest unit the graph `model` has read is measured in, over every
    /// input describing it; absent until the graph is read.
    fn of(model: &Entity<GraphModel>, cx: &mut Context<ScatterPanel>) -> Option<Self> {
        let loaded = model.read(cx).loaded()?;
        let inputs = loaded.shared.inputs;
        // Until the unit is measured, whether any of its points is timed is
        // taken from whether any node of the graph is.
        let timed = loaded.times().is_ok();
        let picker = UnitPicker::new("scatter-unit", inputs, None, ScatterPanel::choose_unit, cx)?;
        let axes = Axes::OPENING;
        let names = Axis::ALL.map(Axis::name);
        let across_picker =
            cx.new(|cx| Dropdown::new("scatter-across", names, axes.x().index(), cx));
        let across_picked = cx.subscribe(&across_picker, |view, _, event, cx| {
            let DropdownEvent::Changed(index) = event;
            if let Some(&axis) = Axis::ALL.get(*index) {
                view.choose_axes(|axes| axes.across(axis), cx);
            }
        });
        let up_picker = cx.new(|cx| Dropdown::new("scatter-up", names, axes.y().index(), cx));
        let up_picked = cx.subscribe(&up_picker, |view, _, event, cx| {
            let DropdownEvent::Changed(index) = event;
            if let Some(&axis) = Axis::ALL.get(*index) {
                view.choose_axes(|axes| axes.up(axis), cx);
            }
        });
        let unit = picker.chosen(cx).unit();
        let names = ColourBy::ALL.map(|by| by.label(unit));
        let colour_picker = cx.new(|cx| Dropdown::new("scatter-colour", names, 0, cx));
        let colour_picked = cx.subscribe(&colour_picker, |view, _, event, cx| {
            let DropdownEvent::Changed(index) = event;
            if let Some(&by) = ColourBy::ALL.get(*index) {
                view.choose_colour(by, cx);
            }
        });
        let mut choice = Self {
            loaded: inputs,
            picker,
            axes,
            across_picker,
            _across_picked: across_picked,
            up_picker,
            _up_picked: up_picked,
            timed,
            colour: ColourBy::ALL[0],
            colour_picker,
            _colour_picked: colour_picked,
        };
        choice.offer_times(timed, cx);
        Some(choice)
    }

    /// Names each colouring the picker offers as it reads for the unit
    /// plotted.
    fn name_colourings(&self, cx: &mut Context<ScatterPanel>) {
        let unit = self.picker.chosen(cx).unit();
        self.colour_picker.update(cx, |picker, cx| {
            for by in ColourBy::ALL {
                picker.set_label(by.index(), by.label(unit), cx);
            }
        });
    }

    /// Shows on each axis's picker the measure chosen for it.
    fn show_axes(&self, cx: &mut Context<ScatterPanel>) {
        let axes = self.axes;
        let pickers = [(&self.across_picker, axes.x()), (&self.up_picker, axes.y())];
        for (picker, axis) in pickers {
            picker.update(cx, |picker, cx| picker.set_selected(axis.index(), cx));
        }
    }

    /// Offers the choices reading action time only where the unit plotted is
    /// `timed`, falling back from any of them chosen where it is not, and
    /// shows what is chosen as chosen. Whether a choice fell back.
    fn offer_times(&mut self, timed: bool, cx: &mut Context<ScatterPanel>) -> bool {
        self.timed = timed;
        let axes_fall = !timed && self.axes.reads_time();
        if axes_fall {
            self.axes = self.axes.untimed();
        }
        let colour_falls = !timed && self.colour == ColourBy::Runner;
        if colour_falls {
            self.colour = ColourBy::Identity;
        }
        for picker in [&self.across_picker, &self.up_picker] {
            picker.update(cx, |picker, cx| {
                for offered in Axis::ALL {
                    let available = timed || !offered.reads_time();
                    picker.set_available(offered.index(), available, cx);
                }
            });
        }
        self.show_axes(cx);
        let colour = self.colour;
        self.colour_picker.update(cx, |picker, cx| {
            for offered in ColourBy::ALL {
                let available = timed || offered != ColourBy::Runner;
                picker.set_available(offered.index(), available, cx);
            }
            picker.set_selected(colour.index(), cx);
        });
        axes_fall || colour_falls
    }

    /// The scope the plot measures in, and that scope's unit over every loaded
    /// input describing it, whose measure the plot is scaled to.
    fn scopes(&self, cx: &App) -> (Scope, Scope) {
        let scope = self.picker.chosen(cx);
        (scope, scope.widened(self.loaded))
    }
}

/// How the toggle for one loaded input stands against the unit plotted.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Toggle {
    On,
    Off,
    /// On, and the only input on describing the unit, which a measure needs.
    Lone,
    /// The input describes nothing of the unit plotted.
    Ignored,
}

impl Toggle {
    /// How the toggle for the loaded input `input` stands against a plot
    /// measuring in `scope`.
    fn of(scope: Scope, input: Input) -> Self {
        let unit = scope.unit();
        let inputs = scope.inputs();
        if !unit.inputs().contains(input) {
            Toggle::Ignored
        } else if !inputs.contains(input) {
            Toggle::Off
        } else if inputs.without(input).is_none() {
            Toggle::Lone
        } else {
            Toggle::On
        }
    }
}

fn input_name(input: Input) -> &'static str {
    match input {
        Input::Query => "query",
        Input::Cquery => "cquery",
        Input::Aquery => "aquery",
    }
}

/// The measured points as drawn, and what among them the pointer is on.
struct Plot {
    plotted: Arc<Plotted>,
    /// Each hue the plotted points are drawn in, and what it stands for.
    legend: Vec<(SharedString, Hue)>,
    pointer: Pointer,
    /// What `plotted` was drawn through.
    filter: Filter,
    /// What `plotted` measures.
    scope: Scope,
    /// What the marks of `plotted` are coloured by.
    colour: ColourBy,
    /// Which of its session's timings `plotted` was drawn with.
    timed: Revision,
}

/// What the panel does for a gesture in [`GESTURES`].
#[derive(Clone, Copy)]
enum ScatterIntent {
    ShowWholePlot,
    Pan,
    Reveal,
    RevealListed,
    Zoom,
    PanAcross,
    Scroll,
    Describe,
}

const POINT: Place = Place::On("a point");

const SHOW_WHOLE: Entry<ScatterIntent> = Entry::new(
    gesture::Input::new(Motion::Press(gesture::Button::Left, Clicks::Double)),
    Place::Empty,
    "Show whole plot",
    ScatterIntent::ShowWholePlot,
);

const REVEAL: Entry<ScatterIntent> = Entry::new(
    gesture::Input::click(gesture::Button::Left),
    POINT,
    "Show in graph or list points",
    ScatterIntent::Reveal,
);

const GESTURES: Gestures<ScatterIntent> = Gestures::new(&[
    SHOW_WHOLE,
    Entry::new(
        gesture::Input::drag(gesture::Button::Left),
        Place::Anywhere,
        "Pan",
        ScatterIntent::Pan,
    ),
    REVEAL,
    Entry::new(
        gesture::Input::click(gesture::Button::Left),
        Place::On("a listed point"),
        "Show point in graph",
        ScatterIntent::RevealListed,
    ),
    Entry::new(
        gesture::Input::SCROLL.holding(Held::SECONDARY),
        Place::Anywhere,
        "Zoom about pointer",
        ScatterIntent::Zoom,
    ),
    Entry::new(
        gesture::Input::PINCH,
        Place::Anywhere,
        "Zoom about pointer",
        ScatterIntent::Zoom,
    ),
    Entry::new(
        gesture::Input::SCROLL.holding(Held::SHIFT),
        Place::Anywhere,
        "Pan across",
        ScatterIntent::PanAcross,
    ),
    Entry::new(
        gesture::Input::SCROLL,
        Place::Anywhere,
        "Pan",
        ScatterIntent::Scroll,
    ),
    Entry::new(
        gesture::Input::HOVER,
        POINT,
        "Describe point",
        ScatterIntent::Describe,
    ),
]);

/// A press over the plot, which is a click until it is dragged past
/// [`CLICK_SLOP`](crate::drag::CLICK_SLOP) and pans the plot from then on.
struct Press {
    from: Point<Pixels>,
    /// Where the pointer was at the last step of the pan.
    last: Point<Pixels>,
    dragged: bool,
    /// The click a release short of [`CLICK_SLOP`] makes of the press.
    click: gesture::Pointer,
}

/// What among a plot's marks the pointer is on.
enum Pointer {
    Away,
    Hover(Hover),
    /// A click landing on several marks put up the list of their points,
    /// which holds the pointer until it closes.
    Picking(PickList),
}

/// A mark under the pointer, by its index among the plotted marks, and what its
/// tooltip reads.
struct Hover {
    mark: usize,
    /// How many other marks are under the pointer with it.
    others: usize,
    lines: Vec<String>,
}

/// Whether a measured point of `kind` is plotted as a point of `unit`: a
/// source file is a point of a measure of actions, but no action.
fn plotted_as(unit: Unit, kind: Kind) -> bool {
    unit != Unit::Action || kind == Kind::Action
}

/// How many points of `unit` the plot draws, of how many it plots.
fn drawn_status(plotted: &Plotted, unit: Unit) -> String {
    let many = unit_names(unit).many;
    let drawn = match (plotted.marks.len(), plotted.plotted) {
        (drawn, all) if drawn == all => format!("{all} {many}"),
        (drawn, all) => format!("{drawn} of {all} {many}"),
    };
    match plotted.estimated {
        true => format!("{drawn}, counts estimated"),
        false => drawn,
    }
}

/// What the tooltip over `mark`, a point of `unit` drawn in `plotted`, reads,
/// where a pick naming it covers `others` points besides: what the point is,
/// then its measures.
fn describe(
    mark: &Mark,
    unit: Unit,
    plotted: &Plotted,
    others: usize,
    loaded: &Loaded,
) -> Vec<String> {
    let (point, time) = (&mark.target, mark.time);
    let mut lines = Vec::with_capacity(TOOLTIP_LINES);
    identify(point.id, point.kind, unit, &loaded.shared, &mut lines);
    lines.push(plotted.axes.readings(point, time));
    lines.extend(plotted.axes.unplaced(point, time));
    if let Hue::Ran(ran) = mark.hue {
        lines.push(format!("runner: {}", ran.label()));
    }
    lines.push(format!(
        "fan-in {} · fan-out {}",
        point.fan_in, point.fan_out
    ));
    if plotted.sizing == Sizing::Actions {
        let names = unit_names(Unit::Action);
        let actions = match point.actions {
            1 => names.one,
            _ => names.many,
        };
        lines.push(format!("{} {actions}", point.actions));
    }
    if others > 0 {
        let click = REVEAL.gesture().input.text(KeyStyle::NATIVE);
        lines.push(format!("+{others} more here · {click} to list"));
    }
    lines
}

/// What a row of the list of points under a click reads for `point`, a point
/// of `unit` — for an action its mnemonic, and the label of its target — and
/// each reading that could tell it apart from points whose rows read alike,
/// most compact first: for an action, the name of the file it writes, the
/// readings of its configuration, that file's path in its configuration's
/// output tree, and the whole path; for a configured target, the readings of
/// its configuration.
fn pick_reading<'a>(
    point: &Metrics,
    unit: Unit,
    loaded: &'a Loaded,
) -> (Option<&'a str>, String, Vec<Option<String>>) {
    let shared = &loaded.shared;
    let label = point_label(point.id, shared);
    let [short, whole] = configuration_readings(point.id, shared);
    match unit {
        Unit::Action => {
            let output = primary_output(point.id, shared);
            let file = output.map(|output| output.rsplit('/').next().unwrap_or(output));
            let readings = vec![
                file.map(str::to_owned),
                short,
                whole,
                output.map(|output| pick::in_output_tree(output).to_owned()),
                output.map(str::to_owned),
            ];
            (Some(mnemonic(point.id, shared)), label, readings)
        }
        Unit::ConfiguredTarget => (None, label, vec![short, whole]),
        Unit::Target => (None, label, Vec::new()),
    }
}

/// The most lines a tooltip reads.
const TOOLTIP_LINES: usize = 10;

impl EventEmitter<PanelEvent> for ScatterPanel {}

impl Focusable for ScatterPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Panel for ScatterPanel {
    fn kind() -> PanelKind {
        PanelKind {
            view: ViewKind::new("graph-scatter"),
            key_context: KEY_CONTEXT,
            role: Role::Canvas,
        }
    }

    fn gestures() -> Vec<GestureRow> {
        GESTURES.rows().collect()
    }

    fn status(&self, cx: &App) -> Option<SharedString> {
        let model = self.model.read(cx);
        let status = model.source().status("No graph", |_| match &self.plot {
            None => MEASURING.into(),
            Some(plot) => drawn_status(&plot.plotted, plot.scope.unit()),
        });
        Some(status.into())
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.shown = active;
        match active {
            true => {
                if self.closes_on_blur.is_none() {
                    let blurred =
                        cx.on_focus_out(&self.focus, window, |view, _, _, cx| view.close_pick(cx));
                    self.closes_on_blur = Some(blurred);
                }
                self.focus.focus(window, cx);
                self.measure(cx);
            }
            false => {
                self.clear_hover(cx);
                self.close_pick(cx);
            }
        }
    }
}

impl Render for ScatterPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::from_theme(&bviz_ui_theme::theme(window, cx));
        let key_context = match self.picking() {
            true => format!("{} {PICKING}", Self::kind().key_context),
            false => Self::kind().key_context.to_string(),
        };
        let root = div()
            .key_context(key_context.as_str())
            .on_action(cx.listener(|view, _: &ShowWholePlot, window, cx| {
                view.zoom_to(Zoom::WHOLE, window.mouse_position(), cx);
            }))
            .on_action(cx.listener(|view, _: &ZoomIn, window, cx| {
                view.zoom_centred(ZOOM_KEY_STEP, window, cx);
            }))
            .on_action(cx.listener(|view, _: &ZoomOut, window, cx| {
                view.zoom_centred(ZOOM_KEY_STEP.recip(), window, cx);
            }))
            .on_action(cx.listener(|view, _: &AimUp, _, cx| view.step_aim(-1, cx)))
            .on_action(cx.listener(|view, _: &AimDown, _, cx| view.step_aim(1, cx)))
            .on_action(cx.listener(|view, _: &RevealAimed, _, cx| view.reveal_aimed(cx)))
            .on_action(cx.listener(|view, _: &ClosePickList, _, cx| view.close_pick(cx)))
            .track_focus(&self.focus)
            .flex_1()
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden();
        let model = self.model.read(cx);
        if let Some(placeholder) = model.source().placeholder(&palette.colors) {
            return root.child(placeholder);
        }
        let scope = self.choice.as_ref().map(|choice| choice.picker.chosen(cx));
        let root = root.children(self.choice.as_ref().zip(scope).map(|(choice, scope)| {
            let measuring = self
                .plot
                .as_ref()
                .is_some_and(|plot| plot.scope != scope || plot.plotted.axes != choice.axes);
            let times = model
                .loaded()
                .map_or(Err(Untimed::NoProfile), Loaded::times);
            strip(choice, scope, times, measuring, &palette, cx)
        }));
        let capabilities = model.loaded().map(|loaded| &loaded.shared.capabilities);
        let dropped = scope.zip(capabilities);
        let dropped = dropped.and_then(|(scope, capabilities)| dropped_edges(scope, capabilities));
        let root = root.children(dropped.map(|dropped| notice(dropped, &palette)));
        let reveals = model.reveals();
        let row = menu_chrome::row_height(window);
        let extents = match self
            .plot
            .as_mut()
            .map(|plot| (plot.scope.unit(), plot.plotted.axes, &mut plot.pointer))
        {
            Some((unit, axes, Pointer::Picking(pick))) => Some(
                *pick
                    .extents
                    .get_or_insert_with(|| pick_extents(&pick.rows, unit, axes, window)),
            ),
            _ => None,
        };
        let Some(plot) = &self.plot else {
            return root.child(centered_spinner(MEASURING, &palette.colors));
        };
        let list = match (&plot.pointer, extents) {
            (Pointer::Picking(pick), Some(extents)) => {
                let style = PickRowStyle {
                    row,
                    line: window.line_height(),
                    reveals,
                };
                let list = pick_list(plot, pick, &self.pick_scroll, style, palette, cx);
                let at = PickAt {
                    plotted: Arc::clone(&plot.plotted),
                    zoom: self.zoom,
                    spot: pick.spot,
                };
                Some(pick_layer(
                    list,
                    pick,
                    at,
                    extents,
                    row,
                    &self.pick_scroll,
                    palette,
                ))
            }
            _ => None,
        };
        let hovering = matches!(plot.pointer, Pointer::Hover(_));
        let show_whole = key_text_in(&ShowWholePlot, &self.focus, window);
        let legend = legend(plot, self.zoom, show_whole.as_deref(), &palette);
        root.child(legend).child(
            div()
                .id("scatter-plot")
                .relative()
                .flex_1()
                .min_h(px(0.))
                .child(cached_layer(self.canvas.clone()))
                .when(hovering && reveals, |plot| plot.cursor_pointer())
                .child(hover_layer(cx.entity(), palette))
                .children(list)
                .on_mouse_move(cx.listener(|view, e: &MouseMoveEvent, _, cx| {
                    if view.press.is_none() {
                        GESTURES.dispatch(gesture::Pointer::HOVER, |intent| match intent {
                            ScatterIntent::Describe => {
                                view.hover_at(e.position, cx);
                                true
                            }
                            _ => false,
                        });
                    }
                }))
                .on_scroll_wheel(cx.listener(|view, e: &ScrollWheelEvent, _, cx| {
                    view.on_scroll_wheel(e, cx);
                }))
                .on_pinch(cx.listener(|view, e: &PinchEvent, _, cx| view.on_pinch(e, cx)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|view, e: &MouseDownEvent, _, cx| view.on_mouse_down(e, cx)),
                )
                // The window-global handlers are installed only by a frame
                // painted with the press in progress, which a quick click
                // finishes before.
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|view, e: &MouseUpEvent, _, cx| {
                        view.end_press(e.position, DragEnd::Released, cx);
                    }),
                )
                .on_hover(cx.listener(|view, hovered: &bool, _, cx| {
                    if !hovered {
                        view.clear_hover(cx);
                    }
                })),
        )
    }
}

/// The layer over the marks that rings the one hovered or aimed at on a list
/// of points, and describes the one hovered, painted apart from the marks so
/// that neither repaints any of them. Its layout records where the plot lies
/// for the pointer handlers; its paint carries a press past the plot's edge.
fn hover_layer(view: Entity<ScatterPanel>, palette: Palette) -> impl IntoElement {
    let prepaint = {
        let view = view.clone();
        move |bounds: Bounds<Pixels>, _: &mut Window, cx: &mut App| {
            view.update(cx, |view, _| view.bounds = Some(bounds));
        }
    };
    let paint = move |bounds: Bounds<Pixels>, (), window: &mut Window, cx: &mut App| {
        let panel = view.read(cx);
        let (zoom, pressed) = (panel.zoom, panel.press.is_some());
        let described = panel.plot.as_ref().and_then(|plot| {
            let (mark, lines) = match &plot.pointer {
                Pointer::Away => return None,
                Pointer::Hover(hover) => (hover.mark, Some(hover.lines.clone())),
                Pointer::Picking(pick) => (pick.rows[pick.aim?].mark, None),
            };
            Some((Arc::clone(&plot.plotted), mark, lines))
        });
        if pressed {
            track_drag(
                window,
                &view,
                |view, at, _, cx| view.drag_to(at, cx),
                |view, at, end, _, cx| view.end_press(at, end, cx),
            );
        }
        let Some((plotted, mark, lines)) = described else {
            return;
        };
        let Some(area) = paint::data_area(&plotted, zoom, bounds) else {
            return;
        };
        let center = paint::paint_ring(&plotted.marks[mark], &area, &palette, window);
        if let Some(lines) = lines {
            let tooltip = TooltipColors::from_colors(&palette.colors);
            paint_tooltip_box(&lines, center, bounds, tooltip, window, cx);
        }
    };
    canvas(prepaint, paint).absolute().inset_0().size_full()
}

/// Where a list of points opens: beside where `spot` is drawn on `plotted` at
/// `zoom`.
struct PickAt {
    plotted: Arc<Plotted>,
    zoom: Zoom,
    spot: Spot,
}

impl PickAt {
    /// Where the spot is drawn in a plot filling `bounds`, or the plot's corner
    /// where it is too small to draw the axes.
    fn within(&self, bounds: Bounds<Pixels>) -> Point<Pixels> {
        paint::data_area(&self.plotted, self.zoom, bounds)
            .map_or(bounds.origin, |area| area.at_spot(self.spot))
    }
}

/// `list`, the list of the points of `pick`, standing where `at` places it in
/// whatever bounds the plot is drawn, with the tooltip of the row aimed at
/// beside it.
fn pick_layer(
    list: impl IntoElement,
    pick: &PickList,
    at: PickAt,
    extents: Extents,
    row: Pixels,
    scroll: &ListScroll,
    palette: Palette,
) -> Placed {
    let rows = pick.rows.len();
    let at = Rc::new(at);
    let frame = {
        let at = Rc::clone(&at);
        move |bounds| pick::frame(at.within(bounds), rows, row, extents, bounds)
    };
    let placed = Placed::new(list, frame);
    let Some(aimed) = pick.aim else {
        return placed;
    };
    let lines = pick.rows[aimed].tooltip.clone();
    let scroll = scroll.handle().clone();
    placed.over(move |list, bounds, window, cx| {
        let level = pick::row_top(scroll.bounds(), scroll.offset().y, row, aimed);
        let at = at.within(bounds);
        let place = |extent| pick::flank(at, list, extent, level, bounds);
        let tooltip = TooltipColors::from_colors(&palette.colors);
        paint_tooltip(&lines, place, tooltip, window, cx);
    })
}

/// Room between the parts a row of the list of points under a click reads.
const PICK_GAP: Pixels = px(8.);

/// The least room the package of a row of the list of points is shown in, cut
/// short from its start; with less, the row leaves the package out.
const PACKAGE_MIN_WIDTH: Rems = rems(2.5);

/// What the header of a list of `rows` points of `unit` reads over the points.
fn pick_heading(rows: usize, unit: Unit) -> String {
    format!("{rows} {} here", unit_names(unit).many)
}

/// How large the list of points listing `rows` of `unit` placed by `axes` and
/// the tooltips of its rows stand, under the window's inherited text style.
fn pick_extents(rows: &[PickRow], unit: Unit, axes: Axes, window: &mut Window) -> Extents {
    let tooltip = rows.iter().fold(Size::default(), |largest, row| {
        largest.max(&tooltip_extent(&row.tooltip, window))
    });
    Extents {
        list_width: pick_width(rows, unit, axes, window),
        tooltip,
    }
}

/// How wide the list of points listing `rows` of `unit` placed by `axes` stands
/// to read each row and its header whole, under the window's inherited text
/// style.
fn pick_width(rows: &[PickRow], unit: Unit, axes: Axes, window: &mut Window) -> Pixels {
    let mut measure =
        |text: &SharedString| menu_chrome::widest(window, std::iter::once(text.clone()));
    let heading =
        measure(&pick_heading(rows.len(), unit).into()) + PICK_GAP + measure(&axes.names().into());
    let widest = rows.iter().fold(heading, |widest, row| {
        let label = measure(&row.package) + measure(&row.name);
        let parts = [
            row.mnemonic.as_ref(),
            row.hint.as_ref(),
            Some(&row.measures),
        ];
        let rest = parts
            .into_iter()
            .flatten()
            .map(|part| PICK_GAP + measure(part));
        let width =
            px(SWATCH_DIAMETER) + PICK_GAP + label + rest.fold(px(0.), |sum, part| sum + part);
        widest.max(width)
    });
    widest + ROW_INSET * 2. + px(SCROLLBAR_THICKNESS) + SHEET_BORDER * 2.
}

/// How tall each row of a list of points and a line of its text stand, and
/// whether a click on a row can reveal it.
#[derive(Clone, Copy)]
struct PickRowStyle {
    row: Pixels,
    line: Pixels,
    reveals: bool,
}

/// The list of the points under a click, `pick`, filling the rect it is given,
/// its rows drawing only those in view.
fn pick_list(
    plot: &Plot,
    pick: &PickList,
    scroll: &ListScroll,
    style: PickRowStyle,
    palette: Palette,
    cx: &mut Context<ScatterPanel>,
) -> impl IntoElement + use<> {
    let colors = &palette.colors;
    let unit = plot.scope.unit();
    let rows = Rc::clone(&pick.rows);
    let plotted = Arc::clone(&plot.plotted);
    let aimed = pick.aim;
    let view = cx.entity();
    let listed = uniform_list("scatter-pick-rows", rows.len(), move |range, _, _| {
        range
            .map(|index| {
                let aimed = aimed == Some(index);
                pick_row(index, &rows[index], &plotted, aimed, style, &view, &palette)
            })
            .collect::<Vec<_>>()
    })
    .flex_1()
    .w_full()
    .track_scroll(scroll.uniform_handle());
    let header = menu_chrome::header(colors)
        .flex_none()
        .h(style.row)
        .justify_between()
        .gap(PICK_GAP)
        .text_color(colors.text_muted)
        .child(pick_heading(pick.rows.len(), unit))
        .child(
            div()
                .mr(px(SCROLLBAR_THICKNESS))
                .child(plot.plotted.axes.names()),
        );
    let sheet = div()
        .size_full()
        .occlude()
        .flex()
        .flex_col()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|view, e: &MouseDownEvent, window, cx| {
                view.focus.focus(window, cx);
                view.pick_scroll.press(e.position);
                cx.stop_propagation();
                cx.notify();
            }),
        )
        .child(header)
        .child(
            div()
                .relative()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(listed)
                .child(list_scrollbar(
                    cx.entity(),
                    |view: &ScatterPanel| &view.pick_scroll,
                    |view: &mut ScatterPanel| &mut view.pick_scroll,
                    palette.scroll,
                )),
        );
    menu_chrome::surface(sheet, colors)
}

/// A line of `text` in a row of the list of points, cut short where the row
/// runs out of room for it.
fn pick_part(text: SharedString) -> Div {
    div()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .child(text)
}

/// The label of the row `listed`, its package and name abutting on a `line`
/// tall, the package read in `muted`.
///
/// Laid out right to left and wrapping, the package goes onto a second, clipped
/// line when less than its least room is left beside the name, and otherwise
/// grows into what the name leaves. Its flex basis is that least room, all the
/// label would ask of the row, so the label stands over an unseen copy of
/// itself asking for its whole width.
fn pick_label(listed: &PickRow, muted: Hsla, line: Pixels) -> Div {
    let package = pick_part(listed.package.clone())
        .flex_basis(PACKAGE_MIN_WIDTH)
        .flex_grow_1()
        .text_ellipsis_start()
        // An elided line falls short of its box by up to a glyph; aligned
        // right, that slack stands before the ellipsis, not before the name.
        .text_right()
        .text_color(muted);
    let name = pick_part(listed.name.clone()).text_ellipsis();
    let whole = div()
        .flex()
        .min_w_0()
        .h(px(0.))
        .overflow_hidden()
        .whitespace_nowrap()
        .child(div().min_w(PACKAGE_MIN_WIDTH).child(listed.package.clone()))
        .child(listed.name.clone());
    let label = div()
        .absolute()
        .inset_0()
        .flex()
        .flex_row_reverse()
        .flex_wrap()
        .justify_start()
        .content_start()
        .overflow_hidden()
        .child(name)
        .child(package);
    div()
        .relative()
        .flex()
        .min_w_0()
        .mr_auto()
        .h(line)
        .child(whole)
        .child(label)
}

/// The row `index` of a list of points, reading `listed`: the colour its mark
/// is drawn in, what its point is, what tells it apart from the rows reading
/// alike, and its measures.
fn pick_row(
    index: usize,
    listed: &PickRow,
    plotted: &Plotted,
    aimed: bool,
    style: PickRowStyle,
    view: &Entity<ScatterPanel>,
    palette: &Palette,
) -> Stateful<Div> {
    let colors = &palette.colors;
    let mark = &plotted.marks[listed.mark];
    let fade = if mark.faded { UNMARKED_FADE } else { 1.0 };
    let swatch = Rgba {
        a: SWATCH_ALPHA * fade,
        ..mark.hue.color(palette)
    };
    let (aim, reveal) = (view.clone(), view.clone());
    menu_chrome::clear_of_scrollbar(menu_chrome::aimed_row(
        ("scatter-pick", index).into(),
        colors,
        aimed,
    ))
    .w_full()
    .h(style.row)
    .gap(PICK_GAP)
    .when(!style.reveals, |row| row.cursor_default())
    .on_mouse_move(move |_, _, cx| aim.update(cx, |view, cx| view.aim_at(index, cx)))
    .on_click(move |e, _, cx| {
        reveal.update(cx, |view, cx| {
            GESTURES.dispatch(gesture::Pointer::click(e), |intent| match intent {
                ScatterIntent::RevealListed => {
                    view.reveal_row(index, cx);
                    true
                }
                _ => false,
            });
        });
    })
    .child(
        div()
            .flex_none()
            .size(px(SWATCH_DIAMETER))
            .rounded_full()
            .bg(swatch),
    )
    .children(
        listed
            .mnemonic
            .clone()
            .map(|mnemonic| div().flex_none().whitespace_nowrap().child(mnemonic)),
    )
    .child(pick_label(listed, colors.text_muted, style.line))
    .children(listed.hint.clone().map(|hint| {
        pick_part(hint)
            .text_ellipsis_start()
            .text_color(colors.text_muted)
    }))
    .child(
        div()
            .flex_none()
            .whitespace_nowrap()
            .text_color(colors.text_muted)
            .child(listed.measures.clone()),
    )
}

/// Opacity of a legend swatch, firmer than a mark's so that a lone swatch still
/// shows the hue a cluster of marks builds up to.
const SWATCH_ALPHA: f32 = 0.7;

/// Diameter of a legend swatch: twice the smallest mark's, so its hue reads.
const SWATCH_DIAMETER: f32 = 4.0 * MIN_RADIUS;

/// The strip choosing what the plot measures, now `scope`, which reads
/// `measuring` while a new choice is measured over the plot drawn for the last.
fn strip(
    choice: &Choice,
    scope: Scope,
    times: Result<&NodeTimes, Untimed>,
    measuring: bool,
    palette: &Palette,
    cx: &Context<ScatterPanel>,
) -> Toolbar {
    let unit = group("points", palette).child(choice.picker.dropdown());
    let tip = TooltipColors::from_colors(&palette.colors);
    let timed_picker = |id: &'static str, picker: &Entity<Dropdown>, read: &str| {
        let note = times_note(times, choice.timed, scope.unit(), read);
        div()
            .id(id)
            .tooltip(TextTooltip::builder(note, tip))
            .child(picker.clone())
    };
    let time_axis = "place points by their action time";
    let across = group("across", palette).child(timed_picker(
        "scatter-across-why",
        &choice.across_picker,
        time_axis,
    ));
    let up =
        group("up", palette).child(timed_picker("scatter-up-why", &choice.up_picker, time_axis));
    let colour = group("colour", palette).child(timed_picker(
        "scatter-colour-why",
        &choice.colour_picker,
        "colour points by where their actions ran",
    ));
    let toggles: Vec<_> = choice
        .loaded
        .inputs()
        .map(|input| input_toggle(input, scope, palette, cx))
        .collect();
    let inputs = group("inputs", palette).children(toggles);
    Toolbar::new()
        .child(unit)
        .child(across)
        .child(up)
        .child(colour)
        .child(inputs)
        .when(measuring, |bar| {
            bar.trailing(busy("scatter-measuring", "measuring…", palette))
        })
}

/// The band beneath the strip saying `text` of what the measure leaves out,
/// wrapped to the panel's width.
fn notice(text: String, palette: &Palette) -> Div {
    div()
        .flex_none()
        .px_2()
        .py_1()
        .bg(palette.colors.surface)
        .border_b_1()
        .border_color(palette.border)
        .text_sm()
        .text_color(palette.colors.warning)
        .child(text)
}

/// What a measure in `scope` of configured targets over cquery says of the
/// cquery edges it leaves out: those a cquery run without the configuration
/// of each dependency links only to the dependency's target, which no
/// configured target holds.
fn dropped_edges(scope: Scope, capabilities: &Capabilities) -> Option<String> {
    if scope.unit() != Unit::ConfiguredTarget || !scope.inputs().contains(Input::Cquery) {
        return None;
    }
    let dropped = match capabilities.support(bviz_graph_layout::Axis::ExecConfiguration) {
        Support::Available => return None,
        Support::Partial { .. } => "some cquery edges left out",
        Support::Unavailable { .. } => "cquery edges left out",
    };
    Some(format!("{dropped} — run the cquery with {TRANSITIONS}"))
}

/// Turns the loaded `input`'s share in the measure in `scope` on and off, and
/// says why where it cannot.
fn input_toggle(
    input: Input,
    scope: Scope,
    palette: &Palette,
    cx: &Context<ScatterPanel>,
) -> Stateful<Div> {
    let name = input_name(input);
    let toggle = Toggle::of(scope, input);
    let many = unit_names(scope.unit()).many;
    let why = match toggle {
        Toggle::On | Toggle::Off => None,
        Toggle::Lone => Some(format!(
            "{name} is the only input on describing {many}, and a plot needs one"
        )),
        Toggle::Ignored => Some(format!("{name} describes no {many}")),
    };
    let control = Button::new(
        ("scatter-input", input as usize),
        cx.listener(move |view, _: &ClickEvent, _, cx| view.toggle_input(input, cx)),
    )
    .label(name)
    .selected(matches!(toggle, Toggle::On | Toggle::Lone))
    .disabled(matches!(toggle, Toggle::Lone | Toggle::Ignored))
    .compact();
    let tip = TooltipColors::from_colors(&palette.colors);
    div()
        .id(("scatter-input-why", input as usize))
        .child(control)
        .when_some(why, |el, why| {
            el.tooltip(TextTooltip::builder(why.into(), tip))
        })
}

/// What the legend says of the points of `plot` left unplotted, where any are.
fn unplaced(plot: &Plot) -> Option<String> {
    let unplaced = plot.plotted.unplaced;
    if unplaced == 0 {
        return None;
    }
    let names = unit_names(plot.scope.unit());
    let many = match unplaced {
        1 => names.one,
        _ => names.many,
    };
    let axes = plot.plotted.axes;
    let lacked: Vec<&str> = [axes.x(), axes.y()]
        .into_iter()
        .filter(|axis| axis.reads_time())
        .map(Axis::name)
        .collect();
    let lacked = lacked.join(" or ");
    Some(format!("{unplaced} {many} without {lacked} left out"))
}

/// Whether `times` places action time on any point `reference` measures.
fn any_timed(times: Result<&NodeTimes, Untimed>, reference: &Measurement) -> bool {
    times.is_ok_and(|times| {
        let mut points = reference.points.iter();
        points.any(|point| times.get(point.id).is_some())
    })
}

/// What a picker offering to `read` action time says of it: as the graph's
/// colour picker does, but where the graph places time on none of the points
/// of `unit`, that it places none there.
fn times_note(
    times: Result<&NodeTimes, Untimed>,
    timed: bool,
    unit: Unit,
    read: &str,
) -> SharedString {
    match times {
        Ok(times) if !timed => {
            let many = unit_names(unit).many;
            format!("{}, none of them on {many}", times.coverage().line()).into()
        }
        _ => colour::note(times, read),
    }
}

/// How the legend names what a mark's size stands for.
fn sizing_name(sizing: Sizing) -> &'static str {
    match sizing {
        Sizing::Actions => "mark size: actions",
        Sizing::FanIn => "mark size: direct dependents",
    }
}

/// The strip naming what each colour a mark is drawn in stands for, what a
/// mark's size stands for, and how to leave a zoom, which the keys
/// `show_whole` also do.
fn legend(plot: &Plot, zoom: Zoom, show_whole: Option<&str>, palette: &Palette) -> Div {
    let swatch = Swatch {
        alpha: SWATCH_ALPHA,
        diameter: px(SWATCH_DIAMETER),
    };
    let zoomed = (zoom != Zoom::WHOLE).then(|| {
        let gesture = SHOW_WHOLE.gesture().text(KeyStyle::NATIVE);
        match show_whole {
            Some(keys) => format!("{gesture} or press {keys} to show the whole plot"),
            None => format!("{gesture} to show the whole plot"),
        }
    });
    let hints = zoomed
        .into_iter()
        .map(SharedString::from)
        .chain(unplaced(plot).map(SharedString::from))
        .chain([sizing_name(plot.plotted.sizing).into()]);
    legend_strip(&plot.legend, swatch, hints, palette)
}

/// The layer the points are drawn in, cached across frames that leave the
/// plot and zoom unchanged.
struct ScatterCanvas {
    /// The panel whose plot and zoom this draws.
    parent: WeakEntity<ScatterPanel>,
}

impl Render for ScatterCanvas {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::from_theme(&bviz_ui_theme::theme(window, cx));
        let drawn = self.parent.upgrade().and_then(|parent| {
            let panel = parent.read(cx);
            Some((Arc::clone(&panel.plot.as_ref()?.plotted), panel.zoom))
        });
        canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, (), window, cx| {
                if let Some((plotted, zoom)) = drawn {
                    paint::paint_plot(&plotted, zoom, &palette, bounds, window, cx);
                }
            },
        )
        .size_full()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_gesture_is_reachable() {
        assert_eq!(GESTURES.unreachable(), []);
    }

    #[test]
    fn every_gesture_fits_the_keys_help() {
        assert_eq!(GESTURES.overlong(), []);
    }
}
