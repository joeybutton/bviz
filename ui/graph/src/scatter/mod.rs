//! A scatter plot of a graph's actions, configured targets or targets by how
//! much of the build each pulls in against how much of it depends on it.

mod hue;
mod paint;
mod plot;

use crate::Palette;
use crate::drag::CLICK_SLOP;
use crate::model::{FilterChanged, GraphModel, Loaded, MetricsLanded, SourceReplaced};
use crate::{PIXELS_PER_NOTCH, ZOOM_PER_NOTCH};
use bviz_graph_layout::facets::Tri;
use bviz_graph_layout::metrics::{Metrics, Scope, Sizing, Unit};
use bviz_graph_layout::select::Filter;
use bviz_graph_layout::select::labels::carried_label;
use bviz_graph_layout::{Id, Input, InputSet, Kind};
use bviz_ui_components::{
    Button, DragEnd, Dropdown, DropdownEvent, TextTooltip, Toolbar, TooltipColors, cached_layer,
    centered_spinner, load_in_background, spinner, track_drag,
};
use bviz_ui_workspace::{Panel, PanelEvent, PanelKind, ViewKind};
use gpui::{
    App, Bounds, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render,
    Rgba, ScrollDelta, ScrollWheelEvent, SharedString, Stateful, Subscription, Task, WeakEntity,
    Window, canvas, div, prelude::*, px,
};
use hue::{Colouring, Hue};
use plot::{MIN_RADIUS, Picked, Plotted, Zoom};
use std::sync::Arc;

/// The key context scoping the scatter plot's own bindings, so a key pressed
/// here reaches this panel and no other.
pub(crate) const KEY_CONTEXT: &str = "GraphScatter";

gpui::actions!(bviz_graph, [ShowWholePlot]);

/// What the panel reads while the graph is being measured.
const MEASURING: &str = "Computing metrics…";

/// The points of a graph, placed by their closure and blast radius.
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
        let changed = cx.observe(&model, |_, _, cx| cx.notify());
        let source_replaced = cx.subscribe(&model, |view, _, _: &SourceReplaced, cx| {
            view.plot = None;
            view.plotting = None;
            view.choice = Choice::of(&view.model, cx);
            if view.shown {
                view.measure(cx);
            }
        });
        let metrics_landed = cx.subscribe(&model, |view, _, landed: &MetricsLanded, cx| {
            let (scope, reference) = view.scopes().unzip();
            if [scope, reference].contains(&Some(landed.scope)) {
                view.replot(cx);
            }
        });
        let filter_changed = cx.subscribe(&model, |view, _, _: &FilterChanged, cx| {
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
            plotting: None,
            _changed: changed,
            _source_replaced: source_replaced,
            _metrics_landed: metrics_landed,
            _filter_changed: filter_changed,
        }
    }

    /// The scope the plot measures in, and the scope of its unit over every
    /// input loaded that it is scaled to, once the graph is read.
    fn scopes(&self) -> Option<(Scope, Scope)> {
        self.choice
            .as_ref()
            .map(|choice| (choice.scope, choice.reference()))
    }

    /// Has the graph measured in the chosen scope and the one it is scaled to,
    /// and brings the plot up to date where it already is.
    fn measure(&mut self, cx: &mut Context<Self>) {
        if let Some((scope, reference)) = self.scopes() {
            self.model
                .update(cx, |model, cx| model.want_metrics(&[scope, reference], cx));
        }
        self.replot(cx);
    }

    /// Measures the graph's points in `unit` over the inputs last enabled for
    /// it.
    fn choose_unit(&mut self, unit: Unit, cx: &mut Context<Self>) {
        let Some(choice) = &mut self.choice else {
            return;
        };
        choice.choose(unit);
        self.measure(cx);
        cx.notify();
    }

    /// Enables `input` where it is disabled and disables it where it is
    /// enabled, unless that leaves no input describing the unit plotted.
    fn toggle_input(&mut self, input: Input, cx: &mut Context<Self>) {
        let Some(choice) = &mut self.choice else {
            return;
        };
        let inputs = choice.scope.inputs();
        let inputs = match inputs.contains(input) {
            true => inputs.without(input),
            false => Some(inputs.with(input)),
        };
        let Some(scope) = inputs.and_then(|inputs| Scope::new(choice.scope.unit(), inputs)) else {
            return;
        };
        choice.scope = scope;
        self.measure(cx);
        cx.notify();
    }

    /// Plots the measured points through the filter the graph's other views
    /// draw through, where the plot drawn is not of the chosen scope or not
    /// drawn through that filter. A hidden panel waits until it is shown, and a
    /// plotting running replots once it lands. The filter is evaluated over
    /// every node of the graph, so this runs off the UI thread, and the plot
    /// drawn stays until the new one lands.
    fn replot(&mut self, cx: &mut Context<Self>) {
        if !self.shown || self.plotting.is_some() {
            return;
        }
        let Some((scope, reference)) = self.scopes() else {
            return;
        };
        let model = self.model.read(cx);
        let Some(loaded) = model.loaded() else {
            return;
        };
        let (Some(metrics), Some(reference)) = (model.metrics(scope), model.metrics(reference))
        else {
            return;
        };
        let filter = model.filter().cloned().unwrap_or_default();
        if self
            .plot
            .as_ref()
            .is_some_and(|plot| plot.filter == filter && plot.scope == scope)
        {
            return;
        }
        let (metrics, reference) = (Arc::clone(metrics), Arc::clone(reference));
        let shared = loaded.shared.clone();
        self.plotting = Some(load_in_background(
            cx,
            move || {
                let verdicts = filter.evaluate(&shared.base, &shared.names, &shared.facets);
                let plots = |point: &Metrics| plotted_as(scope.unit(), point.kind);
                let colouring = Colouring::of(scope.unit(), &reference, &shared.facets);
                let hue = |point: &Metrics| colouring.hue(point, &shared.facets);
                let plotted = Plotted::of(&metrics, &reference, plots, |id| verdicts.get(id), hue);
                let points = metrics.points.iter().filter(|point| plots(point));
                let legend = colouring.legend(points, &shared.facets);
                (plotted, legend, filter)
            },
            move |view, (plotted, legend, filter), cx| {
                view.plotting = None;
                if view
                    .plot
                    .as_ref()
                    .is_none_or(|plot| plot.scope.unit() != scope.unit())
                {
                    view.zoom = Zoom::WHOLE;
                }
                view.plot = Some(Plot {
                    plotted: Arc::new(plotted),
                    legend,
                    hover: None,
                    filter,
                    scope,
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

    /// Where the plot places counts, where it is drawn.
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
        self.invalidate_canvas(cx);
        if self.press.is_none() {
            self.hover_at(at, cx);
        }
    }

    /// Zooms about the pointer by as many notches as the wheel turned.
    fn on_scroll_wheel(&mut self, e: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let notches = match e.delta {
            ScrollDelta::Lines(lines) => lines.y,
            ScrollDelta::Pixels(pixels) => f32::from(pixels.y) / PIXELS_PER_NOTCH,
        };
        if let Some(area) = self.area() {
            let zoom = area.zoomed(e.position, ZOOM_PER_NOTCH.powf(notches));
            self.zoom_to(zoom, e.position, cx);
        }
    }

    /// Starts a press, or shows the whole of each axis again on a double
    /// click clear of every mark: a first click on a mark shows it in the
    /// graph, which the second click then lands on.
    fn on_mouse_down(&mut self, e: &MouseDownEvent, cx: &mut Context<Self>) {
        if e.click_count == DOUBLE_CLICK && self.pick(e.position).is_none() {
            self.press = None;
            self.zoom_to(Zoom::WHOLE, e.position, cx);
            return;
        }
        self.press = Some(Press {
            from: e.position,
            last: e.position,
            dragged: false,
        });
        // The frame this asks for installs the handlers carrying the drag.
        cx.notify();
    }

    /// Follows the pointer to the mark it picks, repainting only where that
    /// changes which mark the tooltip names.
    fn hover_at(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let picked = self.pick(at);
        let model = self.model.read(cx);
        let Some(plot) = &mut self.plot else {
            return;
        };
        if plot.hover.as_ref().map(|hover| hover.mark) == picked.map(|picked| picked.mark) {
            return;
        }
        let (unit, sizing) = (plot.scope.unit(), plot.plotted.sizing);
        plot.hover = picked.and_then(|picked| {
            let point = &plot.plotted.marks[picked.mark].target;
            let lines = describe(point, unit, sizing, picked.coincident, model.loaded()?);
            Some(Hover {
                mark: picked.mark,
                lines,
            })
        });
        cx.notify();
    }

    /// Carries the zoom along with a press dragged past a click's reach, which
    /// puts away the tooltip until the release.
    fn drag_to(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(press) = &mut self.press else {
            return;
        };
        let travelled = at - press.from;
        let starts =
            !press.dragged && f32::from(travelled.x).hypot(f32::from(travelled.y)) > CLICK_SLOP;
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

    /// Ends the press, which shows the graph's drawing of the target under the
    /// pointer where it was a click. Taking the press makes a second call for
    /// the same release a no-op.
    fn end_press(&mut self, at: Point<Pixels>, end: DragEnd, cx: &mut Context<Self>) {
        let Some(press) = self.press.take() else {
            return;
        };
        cx.notify();
        self.hover_at(at, cx);
        if press.dragged || !matches!(end, DragEnd::Released) {
            return;
        }
        let Some((plot, picked)) = self.plot.as_ref().zip(self.pick(at)) else {
            return;
        };
        let id = plot.plotted.marks[picked.mark].target.id;
        self.model.update(cx, |model, cx| model.reveal(id, cx));
    }

    /// Drops the tooltip, which names the mark under a pointer that has left.
    fn clear_hover(&mut self, cx: &mut Context<Self>) {
        if let Some(plot) = &mut self.plot
            && plot.hover.take().is_some()
        {
            cx.notify();
        }
    }
}

/// What the plot measures, and the picker choosing its unit among those the
/// graph's inputs describe. Each unit keeps the inputs enabled for it while
/// another is plotted.
struct Choice {
    scope: Scope,
    /// The scope of each unit offered but the one plotted.
    others: Vec<Scope>,
    /// The inputs the graph was read from.
    loaded: InputSet,
    /// Finest first, in the picker's order.
    units: Vec<Unit>,
    picker: Entity<Dropdown>,
    _picked: Subscription,
}

impl Choice {
    /// The finest unit the graph `model` has read is measured in, over every
    /// input describing it; absent until the graph is read.
    fn of(model: &Entity<GraphModel>, cx: &mut Context<ScatterPanel>) -> Option<Self> {
        let inputs = model.read(cx).loaded()?.shared.inputs;
        let mut scopes = Unit::ALL
            .into_iter()
            .filter_map(|unit| Scope::new(unit, inputs));
        let scope = scopes.next()?;
        let others: Vec<Scope> = scopes.collect();
        let units: Vec<Unit> = std::iter::once(scope)
            .chain(others.iter().copied())
            .map(Scope::unit)
            .collect();
        let names = units.iter().map(|&unit| unit_names(unit).many);
        let picker = cx.new(|cx| Dropdown::new("scatter-unit", names, 0, cx));
        let picked = cx.subscribe(&picker, |view, _, event, cx| {
            let DropdownEvent::Changed(index) = event;
            let unit = view
                .choice
                .as_ref()
                .and_then(|c| c.units.get(*index).copied());
            if let Some(unit) = unit {
                view.choose_unit(unit, cx);
            }
        });
        Some(Self {
            scope,
            others,
            loaded: inputs,
            units,
            picker,
            _picked: picked,
        })
    }

    /// The scope's unit over every loaded input describing it, whose measure
    /// the plot is scaled to.
    fn reference(&self) -> Scope {
        self.scope.widened(self.loaded)
    }

    /// Plots `unit`, one of those offered, in the scope it was left in.
    fn choose(&mut self, unit: Unit) {
        if let Some(other) = self.others.iter_mut().find(|other| other.unit() == unit) {
            std::mem::swap(&mut self.scope, other);
        }
    }

    /// How the toggle for the loaded input `input` stands.
    fn toggle(&self, input: Input) -> Toggle {
        let unit = self.scope.unit();
        let inputs = self.scope.inputs();
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

fn input_name(input: Input) -> &'static str {
    match input {
        Input::Query => "query",
        Input::Cquery => "cquery",
        Input::Aquery => "aquery",
    }
}

/// The `click_count` of the press completing a double click.
const DOUBLE_CLICK: usize = 2;

/// The measured points as drawn, and the mark among them under the pointer.
struct Plot {
    plotted: Arc<Plotted>,
    /// Each hue the plotted points are drawn in, and what it stands for.
    legend: Vec<(SharedString, Hue)>,
    hover: Option<Hover>,
    /// What `plotted` was drawn through.
    filter: Filter,
    /// What `plotted` measures.
    scope: Scope,
}

/// A press over the plot, which is a click until it is dragged past
/// [`CLICK_SLOP`] and pans the plot from then on.
struct Press {
    from: Point<Pixels>,
    /// Where the pointer was at the last step of the pan.
    last: Point<Pixels>,
    dragged: bool,
}

/// A mark under the pointer, by its index among the plotted marks, and what its
/// tooltip reads.
struct Hover {
    mark: usize,
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

/// How a reader names one point of `unit`, and several.
fn unit_names(unit: Unit) -> KindNames {
    let (one, many) = match unit {
        Unit::Action => ("action", "actions"),
        Unit::ConfiguredTarget => ("configured target", "configured targets"),
        Unit::Target => ("target", "targets"),
    };
    KindNames { one, many }
}

/// What the tooltip over `point`, a point of `unit` sized by `sizing`, reads,
/// where `coincident` other points sit at its place in the plot: what the point
/// is, then its counts.
fn describe(
    point: &Metrics,
    unit: Unit,
    sizing: Sizing,
    coincident: usize,
    loaded: &Loaded,
) -> Vec<String> {
    let shared = &loaded.shared;
    let (names, facets) = (&shared.names, &shared.facets);
    let label = match carried_label(names, facets, point.id) {
        Some(label) => shared.labels.written(label).into_owned(),
        None => names.resolve(point.id).to_owned(),
    };
    let mut lines = Vec::with_capacity(TOOLTIP_LINES);
    match unit {
        Unit::Action => {
            let mnemonic = facets.mnemonic(point.id);
            let mnemonic = mnemonic.map_or("action", |m| facets.strings().resolve(m));
            lines.extend([mnemonic.to_owned(), format!("of {label}")]);
            lines.extend(configuration(point.id, loaded));
            let aspects = facets.aspects().values(point.id);
            if !aspects.is_empty() {
                let aspects = aspects.iter().map(|&a| facets.strings().resolve(a));
                lines.push(format!("via {}", aspects.collect::<Vec<_>>().join(", ")));
            }
            // The base graph holds each node at the position of its id.
            let output = facets
                .primary_output(point.id)
                .and_then(|output| shared.base.nodes.get(output.0 as usize));
            lines.extend(output.map(|node| format!("writes {}", node.label)));
        }
        Unit::ConfiguredTarget => {
            lines.push(label);
            lines.extend(configuration(point.id, loaded));
        }
        Unit::Target => {
            let repo = match facets.repo(point.id) {
                Some(repo) => match shared.labels.repos().written(repo) {
                    "" => MAIN_REPO,
                    written => written,
                },
                None => UNKNOWN_REPO,
            };
            lines.extend([label, format!("{} in {repo}", kind_names(point.kind).one)]);
        }
    }
    lines.extend([
        format!(
            "closure {} · blast radius {}",
            point.closure, point.blast_radius
        ),
        format!("fan-in {} · fan-out {}", point.fan_in, point.fan_out),
    ]);
    if sizing == Sizing::Actions {
        let names = unit_names(Unit::Action);
        let actions = match point.actions {
            1 => names.one,
            _ => names.many,
        };
        lines.push(format!("{} {actions}", point.actions));
    }
    if coincident > 0 {
        lines.push(format!("+{coincident} more here"));
    }
    lines
}

/// The line naming the configuration the node `id` is built in, by its
/// mnemonic where it has one, and marked where it builds a tool the build runs.
fn configuration(id: Id, loaded: &Loaded) -> Option<String> {
    let facets = &loaded.shared.facets;
    let config = facets.config(id)?;
    let entry = facets.config_entry(config);
    let name = entry
        .and_then(|entry| entry.mnemonic)
        .unwrap_or(config.sym());
    let name = facets.strings().resolve(name);
    Some(match entry.map(|entry| entry.is_tool) {
        Some(Tri::Yes) => format!("in {name} (exec)"),
        _ => format!("in {name}"),
    })
}

/// The most lines a tooltip reads.
const TOOLTIP_LINES: usize = 8;

const MAIN_REPO: &str = "the main repository";
const UNKNOWN_REPO: &str = "an unnamed repository";

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
            label: "Scatter",
            key_context: KEY_CONTEXT,
        }
    }

    fn title(&self, _cx: &App) -> SharedString {
        Self::kind().label.into()
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
                self.focus.focus(window, cx);
                self.measure(cx);
            }
            false => self.clear_hover(cx),
        }
    }
}

impl Render for ScatterPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::from_theme(&bviz_ui_theme::theme(window, cx));
        let root = div()
            .key_context(Self::kind().key_context)
            .on_action(cx.listener(|view, _: &ShowWholePlot, window, cx| {
                view.zoom_to(Zoom::WHOLE, window.mouse_position(), cx);
            }))
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
        let root = root.children(self.choice.as_ref().map(|choice| {
            let measuring = self
                .plot
                .as_ref()
                .is_some_and(|plot| plot.scope != choice.scope);
            strip(choice, measuring, &palette, cx)
        }));
        match &self.plot {
            None => root.child(centered_spinner(MEASURING, &palette.colors)),
            Some(plot) => root.child(legend(plot, self.zoom, &palette)).child(
                div()
                    .id("scatter-plot")
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .child(cached_layer(self.canvas.clone()))
                    .when(plot.hover.is_some() && model.reveals(), |plot| {
                        plot.cursor_pointer()
                    })
                    .child(hover_layer(cx.entity(), palette))
                    .on_mouse_move(cx.listener(|view, e: &MouseMoveEvent, _, cx| {
                        if view.press.is_none() {
                            view.hover_at(e.position, cx);
                        }
                    }))
                    .on_scroll_wheel(cx.listener(|view, e: &ScrollWheelEvent, _, cx| {
                        view.on_scroll_wheel(e, cx);
                    }))
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
            ),
        }
    }
}

/// The layer over the marks that rings the hovered one and names it, painted
/// apart from them so that a hover repaints none of the marks. Its layout
/// records where the plot lies for the pointer handlers, and its paint carries
/// a press past the plot's edge.
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
        let hovered = panel.plot.as_ref().and_then(|plot| {
            let hover = plot.hover.as_ref()?;
            Some((Arc::clone(&plot.plotted), hover.mark, hover.lines.clone()))
        });
        if pressed {
            track_drag(
                window,
                &view,
                |view, at, _, cx| view.drag_to(at, cx),
                |view, at, end, _, cx| view.end_press(at, end, cx),
            );
        }
        let Some((plotted, mark, lines)) = hovered else {
            return;
        };
        let Some(area) = paint::data_area(&plotted, zoom, bounds) else {
            return;
        };
        let mark = &plotted.marks[mark];
        paint::paint_hover(mark, &lines, &area, &palette, bounds, window, cx);
    };
    canvas(prepaint, paint).absolute().inset_0().size_full()
}

/// How a reader names one target of a kind, and several.
struct KindNames {
    one: &'static str,
    many: &'static str,
}

fn kind_names(kind: Kind) -> KindNames {
    let (one, many) = match kind {
        Kind::Action => ("action", "actions"),
        Kind::ConfiguredTarget => ("configured target", "configured targets"),
        Kind::Source => ("source file", "source files"),
        Kind::Artifact => ("generated file", "generated files"),
        Kind::TargetGroup => (
            "package or environment group",
            "package and environment groups",
        ),
        _ => ("rule target", "rule targets"),
    };
    KindNames { one, many }
}

/// Opacity of a legend swatch, firmer than a mark's so that a lone swatch still
/// shows the hue a cluster of marks builds up to.
const SWATCH_ALPHA: f32 = 0.7;

/// Diameter of a legend swatch: twice the smallest mark's, so its hue reads.
const SWATCH_DIAMETER: f32 = 4.0 * MIN_RADIUS;

/// What the legend reads while the plot shows part of an axis.
const ZOOMED: &str = "double-click empty space or press 0 to show all";

/// The strip choosing what the plot measures, which reads `measuring` while a
/// new choice is measured over the plot drawn for the last.
fn strip(
    choice: &Choice,
    measuring: bool,
    palette: &Palette,
    cx: &Context<ScatterPanel>,
) -> Toolbar {
    let group = |name: &'static str| {
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .text_color(palette.label)
            .child(name)
    };
    let unit = group("points").child(choice.picker.clone());
    let toggles: Vec<_> = choice
        .loaded
        .inputs()
        .map(|input| input_toggle(input, choice, palette, cx))
        .collect();
    let inputs = group("inputs").children(toggles);
    Toolbar::new()
        .child(unit)
        .child(inputs)
        .when(measuring, |bar| {
            bar.trailing(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .text_color(palette.label)
                    .child(spinner("scatter-measuring", px(14.), palette.label))
                    .child("measuring…"),
            )
        })
}

/// Turns the loaded `input`'s share in the measure on and off, and says why
/// where it cannot.
fn input_toggle(
    input: Input,
    choice: &Choice,
    palette: &Palette,
    cx: &Context<ScatterPanel>,
) -> Stateful<Div> {
    let name = input_name(input);
    let toggle = choice.toggle(input);
    let many = unit_names(choice.scope.unit()).many;
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

/// How the legend names what a mark's size stands for.
fn sizing_name(sizing: Sizing) -> &'static str {
    match sizing {
        Sizing::Actions => "mark size: actions",
        Sizing::FanIn => "mark size: direct dependents",
    }
}

/// The strip naming what each colour a mark is drawn in stands for, what a
/// mark's size stands for, and how to leave a zoom. The names wrap onto as many
/// rows as they take, and one too long for a row is cut short; the hints wrap
/// beneath the names where both do not fit.
fn legend(plot: &Plot, zoom: Zoom, palette: &Palette) -> Div {
    let entry = |(name, hue): &(SharedString, Hue)| {
        let color = Rgba {
            a: SWATCH_ALPHA,
            ..hue.color(palette)
        };
        div()
            .flex()
            .items_center()
            .gap_1()
            .min_w_0()
            .child(
                div()
                    .flex_none()
                    .size(px(SWATCH_DIAMETER))
                    .rounded_full()
                    .bg(color),
            )
            .child(
                div()
                    .min_w_0()
                    .max_w(px(LEGEND_NAME_WIDTH))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(name.clone()),
            )
    };
    div()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_start()
        .gap_x_4()
        .px_3()
        .py_1()
        .text_sm()
        .text_color(palette.label)
        .border_b_1()
        .border_color(palette.border)
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_x_4()
                .flex_1()
                .min_w(px(LEGEND_NAMES_MIN_WIDTH))
                .children(plot.legend.iter().map(entry)),
        )
        .child(
            div()
                .ml_auto()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap_x_4()
                .text_color(palette.colors.text_muted)
                .when(zoom != Zoom::WHOLE, |hints| hints.child(ZOOMED))
                .child(sizing_name(plot.plotted.sizing)),
        )
}

/// The widest a legend entry's name is drawn before it is cut short, which a
/// mnemonic or kind is well within.
const LEGEND_NAME_WIDTH: f32 = 220.0;

/// The narrowest the legend's names are squeezed to beside its hints, which
/// move to a row of their own where the two do not fit side by side.
const LEGEND_NAMES_MIN_WIDTH: f32 = 160.0;

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
