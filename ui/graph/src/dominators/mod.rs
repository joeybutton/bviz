//! What exists in a build only because of each of its actions, configured
//! targets or targets: the dominator tree of the graph's points of one unit,
//! drawn as an icicle.

mod icicle;
mod paint;

use crate::Palette;
use crate::drag::DOUBLE_CLICK;
use crate::hue::{Colouring, Hue, Swatch, legend_strip};
use crate::model::{DominatorsLanded, FilterChanged, GraphModel, Shared, SourceReplaced};
use crate::point::{identify, unit_names};
use crate::toolbar::{busy, group};
use crate::unit_picker::UnitPicker;
use bviz_graph_layout::Id;
use bviz_graph_layout::dominators::Dominators;
use bviz_graph_layout::metrics::{Scope, Unit};
use bviz_graph_layout::select::{Filter, Verdicts};
use bviz_ui_components::{
    Toolbar, TooltipColors, cached_layer, centered_notice, centered_spinner, load_in_background,
    paint_tooltip_box,
};
use bviz_ui_workspace::{Panel, PanelEvent, PanelKind, ViewKind};
use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle,
    Focusable, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Point, Render,
    SharedString, Subscription, Task, WeakEntity, Window, canvas, div, prelude::*, px, relative,
};
use icicle::{Cell, Holds, Run};
use paint::{MIN_CELL_WIDTH, Scene, cell_name, point_name, run_name};
use std::collections::HashMap;
use std::sync::Arc;

/// The key context scoping the dominators panel's own bindings, so a key
/// pressed here reaches this panel and no other.
pub(crate) const KEY_CONTEXT: &str = "GraphDominators";

gpui::actions!(bviz_graph, [ShowWholeBuild, FocusUp]);

/// What the panel reads while the tree is being computed.
const COMPUTING: &str = "Computing dominators…";

/// The dominator tree of a graph's points, drawn as an icicle.
pub struct DominatorsPanel {
    focus_handle: FocusHandle,
    /// The graph whose tree is drawn, which computes it.
    model: Entity<GraphModel>,
    /// Whether the workspace is showing the panel, which a graph landing while
    /// it is shown has its tree computed for at once.
    shown: bool,
    /// Chooses the unit drawn, from the time the graph is read.
    picker: Option<UnitPicker>,
    /// The tree drawn, from the time the first one lands.
    drawn: Option<Drawn>,
    /// The layer drawing the cells; notified through
    /// [`Self::invalidate_canvas`].
    canvas: Entity<IcicleCanvas>,
    /// Where the cells were last laid out, which a pointer is resolved
    /// against.
    frame: Option<Frame>,
    /// How the filter the graph's other views draw through takes the nodes of
    /// the graph read, from the time it is evaluated; absent while no filter
    /// is drawn through.
    fade: Option<Fade>,
    /// Evaluates the filter, absent while none is being evaluated, so that one
    /// evaluation runs at a time.
    fading: Option<Task<()>>,
    /// The trees of the graph read coloured so far, by scope.
    coloured: HashMap<Scope, Arc<Coloured>>,
    /// Colours a tree landed, absent while none is being coloured.
    drawing: Option<Task<()>>,
    /// Redraws as the graph is read.
    _changed: Subscription,
    _source_replaced: Subscription,
    _dominators_landed: Subscription,
    _filter_changed: Subscription,
}

impl DominatorsPanel {
    pub(crate) fn new(model: Entity<GraphModel>, cx: &mut Context<Self>) -> Self {
        let changed = cx.observe(&model, |_, _, cx| cx.notify());
        let source_replaced = cx.subscribe(&model, |view, _, _: &SourceReplaced, cx| {
            view.drawn = None;
            view.fade = None;
            view.fading = None;
            view.coloured.clear();
            view.drawing = None;
            view.picker = unit_picker(&view.model, cx);
            view.compute(cx);
        });
        let dominators_landed = cx.subscribe(&model, |view, _, landed: &DominatorsLanded, cx| {
            if view.scope(cx) == Some(landed.scope) {
                view.redraw(cx);
            }
        });
        let filter_changed = cx.subscribe(&model, |view, _, _: &FilterChanged, cx| view.refade(cx));
        let parent = cx.entity().downgrade();
        Self {
            focus_handle: cx.focus_handle(),
            picker: unit_picker(&model, cx),
            model,
            shown: false,
            drawn: None,
            canvas: cx.new(|_| IcicleCanvas { parent }),
            frame: None,
            fade: None,
            fading: None,
            coloured: HashMap::new(),
            drawing: None,
            _changed: changed,
            _source_replaced: source_replaced,
            _dominators_landed: dominators_landed,
            _filter_changed: filter_changed,
        }
    }

    /// The scope whose tree is drawn, once the graph is read.
    fn scope(&self, cx: &App) -> Option<Scope> {
        self.picker.as_ref().map(|picker| picker.chosen(cx))
    }

    /// Has the tree of the chosen scope computed, where the panel is shown, and
    /// draws it where it already is.
    fn compute(&mut self, cx: &mut Context<Self>) {
        let Some(scope) = self.scope(cx).filter(|_| self.shown) else {
            return;
        };
        self.model
            .update(cx, |model, cx| model.want_dominators(&[scope], cx));
        self.redraw(cx);
        self.refade(cx);
    }

    /// Evaluates the filter the graph's other views draw through, where the
    /// panel is shown and that is not the filter the cells are faded by. The
    /// filter is evaluated over every node of the graph, so this runs off the
    /// UI thread, and an evaluation running evaluates again once it lands.
    fn refade(&mut self, cx: &mut Context<Self>) {
        if !self.shown || self.fading.is_some() {
            return;
        }
        let model = self.model.read(cx);
        let Some(loaded) = model.loaded() else {
            return;
        };
        let wanted = model.filter().cloned();
        if self.fade.as_ref().map(|fade| &fade.filter) == wanted.as_ref() {
            return;
        }
        let shared = loaded.shared.clone();
        let Some(filter) = wanted else {
            self.fade = None;
            self.invalidate_canvas(cx);
            return;
        };
        self.fading = Some(load_in_background(
            cx,
            move || {
                let verdicts = filter.evaluate(&shared.base, &shared.names, &shared.facets);
                Fade {
                    filter,
                    verdicts: Arc::new(verdicts),
                }
            },
            |view, fade, cx| {
                view.fading = None;
                view.fade = Some(fade);
                view.invalidate_canvas(cx);
                view.refade(cx);
            },
        ));
    }

    /// Draws the unit chosen.
    fn choose(&mut self, cx: &mut Context<Self>) {
        self.compute(cx);
        cx.notify();
    }

    /// Draws the tree of the chosen scope from the whole build once it has
    /// landed and been coloured, where it is not the one drawn. Colouring is a
    /// pass over the whole tree, so it runs off the UI thread, one at a time,
    /// and a colouring running draws again once it lands. The tree drawn stays
    /// until then.
    fn redraw(&mut self, cx: &mut Context<Self>) {
        let Some(scope) = self.scope(cx) else {
            return;
        };
        if self
            .drawn
            .as_ref()
            .is_some_and(|drawn| drawn.coloured.tree.scope() == scope)
        {
            return;
        }
        if let Some(coloured) = self.coloured.get(&scope) {
            self.drawn = Some(Drawn {
                coloured: Arc::clone(coloured),
                focus: None,
                hover: None,
                press: None,
            });
            self.invalidate_canvas(cx);
            return;
        }
        if self.drawing.is_some() {
            return;
        }
        let model = self.model.read(cx);
        let (Some(tree), Some(loaded)) = (model.dominators(scope), model.loaded()) else {
            return;
        };
        let (tree, shared) = (Arc::clone(tree), loaded.shared.clone());
        self.drawing = Some(load_in_background(
            cx,
            move || {
                let unit = tree.scope().unit();
                let points = || (0..tree.total()).map(|node| (tree.id(node), tree.kind(node)));
                let colouring = Colouring::of(unit, points().map(|(id, _)| id), &shared.facets);
                let legend = colouring.legend(points(), &shared.facets);
                Coloured {
                    tree,
                    shared,
                    colouring: Arc::new(colouring),
                    legend,
                }
            },
            |view, coloured, cx| {
                view.drawing = None;
                let scope = coloured.tree.scope();
                view.coloured.insert(scope, Arc::new(coloured));
                view.redraw(cx);
            },
        ));
    }

    /// The cell a pointer at `at` is over, where the cells are drawn.
    fn cell_at(&self, at: Point<Pixels>) -> Option<Cell> {
        let (drawn, frame) = (self.drawn.as_ref()?, self.frame?);
        let cells = icicle::lay_out(
            &*drawn.coloured.tree,
            drawn.top(),
            frame.bounds,
            frame.row,
            MIN_CELL_WIDTH,
        );
        cells.into_iter().find(|cell| cell.rect.contains(&at))
    }

    /// Draws the icicle from `on`, or from the whole build where there is
    /// none, and describes the cell that brings under the pointer.
    fn focus_on(&mut self, on: Option<Focused>, cx: &mut Context<Self>) {
        let Some(drawn) = &self.drawn else {
            return;
        };
        if drawn.focused() != on {
            let focus = on.map(|on| Focus::on(&drawn.coloured.tree, on));
            self.refocus(focus, cx);
        }
    }

    /// Draws the icicle from `focus`, or from the whole build where there is
    /// none, and describes the cell that brings under the pointer.
    fn refocus(&mut self, focus: Option<Focus>, cx: &mut Context<Self>) {
        let Some(drawn) = &mut self.drawn else {
            return;
        };
        drawn.focus = focus;
        let at = drawn.hover.take().map(|hover| hover.at);
        self.invalidate_canvas(cx);
        if let Some(at) = at {
            self.hover_at(at, cx);
        }
    }

    /// Focuses `node` of `tree` as [`Self::focus_on`] does, where `tree` is
    /// still the tree drawn.
    fn focus_on_in(&mut self, tree: &Arc<Dominators>, node: Option<u32>, cx: &mut Context<Self>) {
        if self
            .drawn
            .as_ref()
            .is_some_and(|drawn| Arc::ptr_eq(&drawn.coloured.tree, tree))
        {
            self.focus_on(node.map(Focused::Node), cx);
        }
    }

    /// Draws the icicle from the dominator the focus stands under.
    fn focus_up(&mut self, cx: &mut Context<Self>) {
        let Some(drawn) = &self.drawn else {
            return;
        };
        if let Some(on) = drawn.focused() {
            let up = on.parent(&drawn.coloured.tree);
            self.focus_on(up.map(Focused::Node), cx);
        }
    }

    /// Focuses what a click on the cell holding `holds` asks for: what it
    /// holds, or the dominator above where it is the cell on top.
    fn click_cell(&mut self, holds: Holds, cx: &mut Context<Self>) {
        let Some(drawn) = &self.drawn else {
            return;
        };
        if holds == drawn.top() {
            self.focus_up(cx);
        } else {
            self.focus_on(Focused::of(holds), cx);
        }
    }

    /// Acts on a press: the first of a click on a cell focuses as
    /// [`Self::click_cell`] does, and the second of a double click on the cell
    /// of a point puts back the focus the first replaced and shows the graph's
    /// drawing of the point, or returns to the whole build where the first
    /// landed clear of every cell.
    fn on_mouse_down(&mut self, e: &MouseDownEvent, cx: &mut Context<Self>) {
        let cell = self.cell_at(e.position);
        let Some(drawn) = &mut self.drawn else {
            return;
        };
        match e.click_count {
            1 => {
                drawn.press = match cell.map(|cell| cell.holds) {
                    None => Some(Press::Clear),
                    Some(Holds::Node(node)) => Some(Press::Point {
                        id: drawn.coloured.tree.id(node),
                        replaced: drawn.focus.clone(),
                    }),
                    Some(Holds::Build | Holds::Rest(_)) => None,
                };
                if let Some(cell) = cell {
                    self.click_cell(cell.holds, cx);
                }
            }
            DOUBLE_CLICK => match drawn.press.take() {
                Some(Press::Clear) => self.focus_on(None, cx),
                Some(Press::Point { id, replaced }) => {
                    self.refocus(replaced, cx);
                    self.model.update(cx, |model, cx| model.reveal(id, cx));
                }
                None => {}
            },
            _ => {}
        }
    }

    /// Follows the pointer to the cell it is over, describing the cell afresh
    /// only where the pointer has moved onto another.
    fn hover_at(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let cell = self.cell_at(at);
        let Some(drawn) = &mut self.drawn else {
            return;
        };
        let focus = drawn.focused();
        match (&mut drawn.hover, cell) {
            (Some(hover), Some(cell)) if hover.cell == cell => hover.at = at,
            (None, None) => return,
            (hover, cell) => {
                *hover = cell.map(|cell| Hover {
                    cell,
                    at,
                    lines: describe(
                        &drawn.coloured.tree,
                        cell.holds,
                        focus,
                        &drawn.coloured.shared,
                    ),
                });
            }
        }
        cx.notify();
    }

    /// Drops the tooltip, which names the cell under a pointer that has left.
    fn clear_hover(&mut self, cx: &mut Context<Self>) {
        if let Some(drawn) = &mut self.drawn
            && drawn.hover.take().is_some()
        {
            cx.notify();
        }
    }

    /// Repaints the cells along with the layer over them; every change to what
    /// the cells show routes through here.
    fn invalidate_canvas(&self, cx: &mut Context<Self>) {
        self.canvas.update(cx, |_, cx| cx.notify());
        cx.notify();
    }
}

/// The picker among the units the graph `model` has read; absent until the
/// graph is read. It opens on configured targets: most actions produce
/// something no other action reads, so they stand directly under the whole
/// build, each a sliver of its row, while configured targets mostly stand
/// under one another and leave the row to cells wide enough to name.
fn unit_picker(
    model: &Entity<GraphModel>,
    cx: &mut Context<DominatorsPanel>,
) -> Option<UnitPicker> {
    let inputs = model.read(cx).loaded()?.shared.inputs;
    let opening = Some(Unit::ConfiguredTarget);
    UnitPicker::new(
        "dominators-unit",
        inputs,
        opening,
        DominatorsPanel::choose,
        cx,
    )
}

/// How the cells are faded, kept with the filter it was evaluated from so a
/// filter that changes is told apart from the one the cells are faded by.
struct Fade {
    filter: Filter,
    verdicts: Arc<Verdicts>,
}

/// A tree landed, coloured: the hue of each of its points, and the legend
/// naming the hues, which hold for as long as the graph read does.
struct Coloured {
    tree: Arc<Dominators>,
    /// The graph whose points `tree` holds.
    shared: Shared,
    colouring: Arc<Colouring>,
    legend: Vec<(SharedString, Hue)>,
}

/// A tree drawn, and how the reader has it drawn, which lasts only as long as
/// the tree is drawn.
struct Drawn {
    coloured: Arc<Coloured>,
    focus: Option<Focus>,
    hover: Option<Hover>,
    /// Where the first press of the last click landed.
    press: Option<Press>,
}

impl Drawn {
    fn focused(&self) -> Option<Focused> {
        self.focus.as_ref().map(|focus| focus.on)
    }

    /// What the cell on top holds.
    fn top(&self) -> Holds {
        self.focused().map_or(Holds::Build, Focused::holds)
    }
}

/// What the icicle is drawn from short of the whole build.
#[derive(Clone, Copy, PartialEq)]
enum Focused {
    Node(u32),
    Run(Run),
}

impl Focused {
    /// What focusing the cell holding `holds` draws from; none for the whole
    /// build.
    fn of(holds: Holds) -> Option<Self> {
        match holds {
            Holds::Build => None,
            Holds::Node(node) => Some(Self::Node(node)),
            Holds::Rest(run) => Some(Self::Run(run)),
        }
    }

    fn holds(self) -> Holds {
        match self {
            Self::Node(node) => Holds::Node(node),
            Self::Run(run) => Holds::Rest(run),
        }
    }

    /// The dominator this stands under in `tree`, or none under the whole
    /// build.
    fn parent(self, tree: &Dominators) -> Option<u32> {
        match self {
            Self::Node(node) => tree.parent(node),
            Self::Run(run) => run.parent(),
        }
    }

    /// The points of `tree` this stands over.
    fn weight(self, tree: &Dominators) -> u32 {
        match self {
            Self::Node(node) => tree.weight(node),
            Self::Run(run) => run.weight(),
        }
    }

    /// How a tooltip or the breadcrumb names this in `tree`.
    fn name(self, tree: &Dominators, shared: &Shared) -> String {
        match self {
            Self::Node(node) => point_name(tree.id(node), tree.scope().unit(), shared),
            Self::Run(run) => run_name(tree, shared, run),
        }
    }
}

/// Where the icicle is drawn from, with the path above it that the breadcrumb
/// names taken once as the focus is set.
#[derive(Clone)]
struct Focus {
    on: Focused,
    /// Top first, those nearest `on` at most [`CRUMBS`] of them.
    above: Vec<u32>,
    /// Whether more dominators stand above those of `above`.
    elided: bool,
}

/// The most dominators above the focus the breadcrumb names.
const CRUMBS: usize = 4;

impl Focus {
    fn on(tree: &Dominators, on: Focused) -> Self {
        let mut above: Vec<u32> = Vec::with_capacity(CRUMBS + 1);
        let up = std::iter::successors(on.parent(tree), |&above| tree.parent(above));
        above.extend(up.take(CRUMBS + 1));
        let elided = above.len() > CRUMBS;
        above.truncate(CRUMBS);
        above.reverse();
        Self { on, above, elided }
    }
}

/// Where the first press of a click landed, as far as a second press
/// completing a double click acts on it.
enum Press {
    Clear,
    /// On the cell of the point `id`, while the icicle was drawn from
    /// `replaced`.
    Point {
        id: Id,
        replaced: Option<Focus>,
    },
}

/// The cell the pointer rests on, described once as the pointer reaches it,
/// so a pointer moving within the cell only moves the tooltip.
struct Hover {
    cell: Cell,
    at: Point<Pixels>,
    lines: Vec<String>,
}

/// What the cells' layout turns on besides the tree drawn, so a pointer is
/// resolved against the cells as they were painted.
#[derive(Clone, Copy, PartialEq)]
struct Frame {
    bounds: Bounds<Pixels>,
    row: Pixels,
}

/// The most lines a tooltip reads.
const TOOLTIP_LINES: usize = 9;

/// What the tooltip over the cell holding `holds` of `tree` drawn from `focus`
/// reads: what it stands for, and how much of the build exists only because
/// of it.
fn describe(
    tree: &Dominators,
    holds: Holds,
    focus: Option<Focused>,
    shared: &Shared,
) -> Vec<String> {
    let unit = tree.scope().unit();
    let names = unit_names(unit);
    let points = |count: u32| match count {
        1 => format!("1 {}", names.one),
        _ => format!("{count} {}", names.many),
    };
    let stand_on = |weight: u32, on: &str| {
        let exist = if weight == 1 { "exists" } else { "exist" };
        format!("{} {exist} only because of {on}", points(weight))
    };
    let shares = |weight: u32, within: Option<Focused>| {
        let of_build = format!("{} of the build", share(weight, tree.total()));
        match within {
            Some(within) => {
                let name = within.name(tree, shared);
                format!(
                    "{of_build} · {} of {name}",
                    share(weight, within.weight(tree))
                )
            }
            None => of_build,
        }
    };
    let on_top = Focused::of(holds) == focus;
    let mut lines = Vec::with_capacity(TOOLTIP_LINES);
    match holds {
        Holds::Build => lines.extend([WHOLE_BUILD.to_owned(), points(tree.total())]),
        Holds::Node(node) => {
            identify(tree.id(node), tree.kind(node), unit, shared, &mut lines);
            let weight = tree.weight(node);
            lines.push(stand_on(weight, "this"));
            lines.push(shares(weight, focus.filter(|_| !on_top)));
            let dominator = match tree.parent(node) {
                Some(parent) => point_name(tree.id(parent), unit, shared),
                None => WHOLE_BUILD.to_owned(),
            };
            lines.push(format!("immediately dominated by {dominator}"));
        }
        Holds::Rest(run) => {
            let (count, weight) = (run.count(), run.weight());
            lines.push(match on_top {
                true => run_name(tree, shared, run),
                false => format!("{} too narrow to draw apart", points(count)),
            });
            lines.push(stand_on(weight, if count == 1 { "it" } else { "them" }));
            lines.push(shares(weight, focus.filter(|_| !on_top)));
        }
    }
    lines
}

/// What a click on the cell holding `holds` under `top` does, and a double
/// click where it holds a point and a graph panel `reveals` it; absent where a
/// click does nothing.
fn gesture(holds: Holds, top: Holds, reveals: bool) -> Option<String> {
    let click = match holds {
        Holds::Build => return None,
        _ if holds == top => FOCUS_UP,
        Holds::Node(_) => FOCUS,
        Holds::Rest(_) => OPEN,
    };
    Some(match (holds, reveals) {
        (Holds::Node(_), true) => format!("{click} · {REVEAL}"),
        _ => click.to_owned(),
    })
}

/// How a cell names the whole build.
const WHOLE_BUILD: &str = "the whole build";

/// What a click on a cell does: on a point, on a run, and on the cell on top.
const FOCUS: &str = "click to focus";
const OPEN: &str = "click to open";
const FOCUS_UP: &str = "click or Backspace to go up a level";
const REVEAL: &str = "double-click to show in the graph";

/// What share of `whole` `part` is, as a percentage.
fn share(part: u32, whole: u32) -> String {
    format!("{:.1}%", f64::from(part) * PERCENT / f64::from(whole))
}

const PERCENT: f64 = 100.0;

impl EventEmitter<PanelEvent> for DominatorsPanel {}

impl Focusable for DominatorsPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for DominatorsPanel {
    fn kind() -> PanelKind {
        PanelKind {
            view: ViewKind::new("graph-dominators"),
            label: "Dominators",
            key_context: KEY_CONTEXT,
        }
    }

    fn title(&self, _cx: &App) -> SharedString {
        Self::kind().label.into()
    }

    fn status(&self, cx: &App) -> Option<SharedString> {
        let model = self.model.read(cx);
        let status = model.source().status("No graph", |_| match &self.drawn {
            None => COMPUTING.into(),
            Some(drawn) => {
                let many = unit_names(drawn.coloured.tree.scope().unit()).many;
                format!("{} {many}", drawn.coloured.tree.total())
            }
        });
        Some(status.into())
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.shown = active;
        if active {
            self.focus_handle.focus(window, cx);
            self.compute(cx);
        }
    }
}

impl Render for DominatorsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::from_theme(&bviz_ui_theme::theme(window, cx));
        let root = div()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|view, _: &ShowWholeBuild, _, cx| view.focus_on(None, cx)))
            .on_action(cx.listener(|view, _: &FocusUp, _, cx| view.focus_up(cx)))
            .track_focus(&self.focus_handle)
            .flex_1()
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden();
        let model = self.model.read(cx);
        if let Some(placeholder) = model.source().placeholder(&palette.colors) {
            return root.child(placeholder);
        }
        let drawn = self.drawn.as_ref();
        let root = root.children(self.picker.as_ref().map(|picker| {
            let computing =
                drawn.is_some_and(|drawn| drawn.coloured.tree.scope() != picker.chosen(cx));
            let crumbs = drawn.and_then(|drawn| breadcrumb(drawn, &palette, cx));
            strip(picker, crumbs, computing, &palette)
        }));
        let Some(drawn) = drawn else {
            return root.child(centered_spinner(COMPUTING, &palette.colors));
        };
        if drawn.coloured.tree.total() == 0 {
            let many = unit_names(drawn.coloured.tree.scope().unit()).many;
            let empty = format!("The graph holds no {many}.");
            return root.child(centered_notice(empty, &palette.colors));
        }
        let swatch = Swatch {
            alpha: 1.0,
            diameter: LEGEND_SWATCH,
        };
        // A legend of one hue names nothing the unit picked does not.
        let legend = drawn.coloured.legend.len() > 1;
        let legend = legend.then(|| legend_strip(&drawn.coloured.legend, swatch, [], &palette));
        root.children(legend).child(
            div()
                .id("dominators-icicle")
                .relative()
                .flex_1()
                .min_h(px(0.))
                .child(cached_layer(self.canvas.clone()))
                .child(hover_layer(cx.entity(), palette))
                .when(drawn.hover.is_some(), |icicle| icicle.cursor_pointer())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|view, e: &MouseDownEvent, _, cx| view.on_mouse_down(e, cx)),
                )
                .on_mouse_move(cx.listener(|view, e: &MouseMoveEvent, _, cx| {
                    view.hover_at(e.position, cx);
                }))
                .on_hover(cx.listener(|view, hovered: &bool, _, cx| {
                    if !hovered {
                        view.clear_hover(cx);
                    }
                })),
        )
    }
}

/// The strip choosing the unit drawn, followed by `crumbs`, which reads
/// `computing` while the tree of a new choice is computed over the one drawn
/// for the last.
fn strip(picker: &UnitPicker, crumbs: Option<Div>, computing: bool, palette: &Palette) -> Toolbar {
    Toolbar::new()
        .child(group("points", palette).child(picker.dropdown()))
        .when_some(crumbs, |bar, crumbs| bar.child(crumbs))
        .when(computing, |bar| {
            bar.trailing(busy("dominators-computing", "computing…", palette))
        })
}

/// How wide a legend swatch is drawn.
const LEGEND_SWATCH: Pixels = px(8.);

/// The path from the whole build down to the focus of `drawn`, each step but
/// the focus a click away; absent at the whole build. Where the path outgrows
/// its room the dominators above the focus give way first, and the focus is
/// cut short only once they are gone.
fn breadcrumb(drawn: &Drawn, palette: &Palette, cx: &Context<DominatorsPanel>) -> Option<Div> {
    let focus = drawn.focus.as_ref()?;
    let unit = drawn.coloured.tree.scope().unit();
    let name = |node: u32| point_name(drawn.coloured.tree.id(node), unit, &drawn.coloured.shared);
    // A row whose steps shrink to fit it.
    let row = || div().flex().items_center().min_w_0().overflow_hidden();
    let crumb = |name: String| {
        div()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .child(name)
    };
    let after = |step: AnyElement| row().gap_1().pl_1().child(separator()).child(step);
    let link = |index: usize, name: String, to: Option<u32>| {
        // A click landing once another tree is drawn names none of its nodes.
        let tree = Arc::clone(&drawn.coloured.tree);
        crumb(name)
            .id(("dominators-crumb", index))
            .cursor_pointer()
            .text_color(palette.colors.text_muted)
            .hover(|style| style.text_color(palette.label))
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                view.focus_on_in(&tree, to, cx);
            }))
    };
    let mut above: Vec<Div> = Vec::with_capacity(CRUMBS + 1);
    if focus.elided {
        above.push(after(ELIDED.into_any_element()));
    }
    for (index, &node) in focus.above.iter().enumerate() {
        above.push(after(
            link(index + 1, name(node), Some(node)).into_any_element(),
        ));
    }
    let focused = cell_name(
        &drawn.coloured.tree,
        &drawn.coloured.shared,
        focus.on.holds(),
    );
    let last = after(crumb(focused).into_any_element())
        .flex_shrink_0()
        .max_w(relative(1.));
    Some(
        row()
            .flex_1()
            .text_sm()
            .text_color(palette.label)
            .child(link(0, WHOLE_BUILD_CRUMB.to_owned(), None).flex_shrink_0())
            .child(row().child(row().children(above)).child(last)),
    )
}

/// What the breadcrumb names the whole build.
const WHOLE_BUILD_CRUMB: &str = "Whole build";

/// Stands in the breadcrumb for the dominators it leaves out.
const ELIDED: &str = "…";

fn separator() -> AnyElement {
    "›".into_any_element()
}

/// The layer over the cells that rings and describes the one under the
/// pointer, painted apart from the cells so that neither repaints any of them.
/// Its layout records where the cells lie for the pointer handlers, and lets
/// go of a cell laid out elsewhere.
fn hover_layer(view: Entity<DominatorsPanel>, palette: Palette) -> impl IntoElement {
    let prepaint = {
        let view = view.clone();
        move |bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App| {
            let frame = Frame {
                bounds,
                row: paint::row_height(window),
            };
            view.update(cx, |view, _| {
                if view.frame != Some(frame) {
                    view.frame = Some(frame);
                    if let Some(drawn) = &mut view.drawn {
                        drawn.hover = None;
                    }
                }
            });
        }
    };
    let paint = move |bounds: Bounds<Pixels>, (), window: &mut Window, cx: &mut App| {
        let panel = view.read(cx);
        let Some((drawn, Hover { cell, at, lines })) = panel
            .drawn
            .as_ref()
            .and_then(|drawn| Some((drawn, drawn.hover.as_ref()?)))
        else {
            return;
        };
        // Read as it is painted, since a graph panel opening or closing
        // repaints the tooltip without the pointer moving.
        let reveals = panel.model.read(cx).reveals();
        let gesture = gesture(cell.holds, drawn.top(), reveals);
        let lines: Vec<String> = lines.iter().cloned().chain(gesture).collect();
        let (cell, at) = (*cell, *at);
        paint::paint_ring(&cell, &palette, window);
        let tooltip = TooltipColors::from_colors(&palette.colors);
        paint_tooltip_box(&lines, at, bounds, tooltip, window, cx);
    };
    canvas(prepaint, paint).absolute().inset_0().size_full()
}

/// The layer the cells are drawn in, cached across frames that leave the tree
/// drawn unchanged.
struct IcicleCanvas {
    /// The panel whose tree this draws.
    parent: WeakEntity<DominatorsPanel>,
}

impl Render for IcicleCanvas {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::from_theme(&bviz_ui_theme::theme(window, cx));
        let scene = self.parent.upgrade().and_then(|parent| {
            let panel = parent.read(cx);
            let drawn = panel.drawn.as_ref()?;
            Some(Scene {
                colouring: Arc::clone(&drawn.coloured.colouring),
                verdicts: panel.fade.as_ref().map(|fade| Arc::clone(&fade.verdicts)),
                tree: Arc::clone(&drawn.coloured.tree),
                top: drawn.top(),
                shared: drawn.coloured.shared.clone(),
                palette,
            })
        });
        canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, (), window, cx| {
                if let Some(scene) = scene {
                    paint::paint_icicle(&scene, bounds, window, cx);
                }
            },
        )
        .size_full()
    }
}
