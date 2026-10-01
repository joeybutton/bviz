//! What exists in a build only because of each of its actions, configured
//! targets or targets: the dominator tree of the graph's points of one unit,
//! drawn as an icicle.

mod icicle;
mod paint;

use crate::Palette;
use crate::drag::DOUBLE_CLICK;
use crate::model::{DominatorsLanded, GraphModel, Shared, SourceReplaced};
use crate::point::{identify, unit_names};
use crate::toolbar::{busy, group};
use crate::unit_picker::UnitPicker;
use bviz_graph_layout::Id;
use bviz_graph_layout::dominators::Dominators;
use bviz_graph_layout::metrics::Scope;
use bviz_ui_components::{
    Toolbar, TooltipColors, cached_layer, centered_notice, centered_spinner, paint_tooltip_box,
};
use bviz_ui_workspace::{Panel, PanelEvent, PanelKind, ViewKind};
use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle,
    Focusable, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Point, Render,
    SharedString, Subscription, WeakEntity, Window, canvas, div, prelude::*, px, relative,
};
use icicle::{Cell, Holds};
use paint::{MIN_CELL_WIDTH, Scene, point_name};
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
    /// Redraws as the graph is read.
    _changed: Subscription,
    _source_replaced: Subscription,
    _dominators_landed: Subscription,
}

impl DominatorsPanel {
    pub(crate) fn new(model: Entity<GraphModel>, cx: &mut Context<Self>) -> Self {
        let changed = cx.observe(&model, |_, _, cx| cx.notify());
        let source_replaced = cx.subscribe(&model, |view, _, _: &SourceReplaced, cx| {
            view.drawn = None;
            view.picker = unit_picker(&view.model, cx);
            view.compute(cx);
        });
        let dominators_landed = cx.subscribe(&model, |view, _, landed: &DominatorsLanded, cx| {
            if view.scope(cx) == Some(landed.scope) {
                view.redraw(cx);
            }
        });
        let parent = cx.entity().downgrade();
        Self {
            focus_handle: cx.focus_handle(),
            picker: unit_picker(&model, cx),
            model,
            shown: false,
            drawn: None,
            canvas: cx.new(|_| IcicleCanvas { parent }),
            frame: None,
            _changed: changed,
            _source_replaced: source_replaced,
            _dominators_landed: dominators_landed,
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
    }

    /// Draws the unit chosen.
    fn choose(&mut self, cx: &mut Context<Self>) {
        self.compute(cx);
        cx.notify();
    }

    /// Draws the tree of the chosen scope from the whole build, where it has
    /// landed and is not the one drawn. The tree drawn stays until then.
    fn redraw(&mut self, cx: &mut Context<Self>) {
        let Some(scope) = self.scope(cx) else {
            return;
        };
        let model = self.model.read(cx);
        let (Some(tree), Some(loaded)) = (model.dominators(scope), model.loaded()) else {
            return;
        };
        if self
            .drawn
            .as_ref()
            .is_some_and(|drawn| Arc::ptr_eq(&drawn.tree, tree))
        {
            return;
        }
        self.drawn = Some(Drawn {
            tree: Arc::clone(tree),
            shared: loaded.shared.clone(),
            focus: None,
            hover: None,
            press: None,
        });
        self.invalidate_canvas(cx);
    }

    /// The cell a pointer at `at` is over, where the cells are drawn.
    fn cell_at(&self, at: Point<Pixels>) -> Option<Cell> {
        let (drawn, frame) = (self.drawn.as_ref()?, self.frame?);
        let focus = drawn.focus_node();
        let cells = icicle::lay_out(&*drawn.tree, focus, frame.bounds, frame.row, MIN_CELL_WIDTH);
        cells.into_iter().find(|cell| cell.rect.contains(&at))
    }

    /// Draws the icicle from `node`, or from the whole build where there is
    /// none, and describes the cell that brings under the pointer.
    fn focus_on(&mut self, node: Option<u32>, cx: &mut Context<Self>) {
        let Some(drawn) = &self.drawn else {
            return;
        };
        if drawn.focus_node() != node {
            let focus = node.map(|node| Focus::on(&drawn.tree, node));
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
            .is_some_and(|drawn| Arc::ptr_eq(&drawn.tree, tree))
        {
            self.focus_on(node, cx);
        }
    }

    /// Draws the icicle from the dominator of the focus.
    fn focus_up(&mut self, cx: &mut Context<Self>) {
        let Some(drawn) = &self.drawn else {
            return;
        };
        if let Some(focus) = drawn.focus_node() {
            let up = drawn.tree.parent(focus);
            self.focus_on(up, cx);
        }
    }

    /// Focuses what a click on the cell holding `holds` asks for: the point it
    /// holds, the dominator of the focus where it holds the focus, and the
    /// dominator of the siblings it gathers.
    fn click_cell(&mut self, holds: Holds, cx: &mut Context<Self>) {
        let Some(drawn) = &self.drawn else {
            return;
        };
        match holds {
            Holds::Build => {}
            Holds::Node(node) if drawn.focus_node() == Some(node) => self.focus_up(cx),
            Holds::Node(node) => self.focus_on(Some(node), cx),
            Holds::Rest { first, .. } => {
                let dominator = drawn.tree.parent(first);
                self.focus_on(dominator, cx);
            }
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
                        id: drawn.tree.id(node),
                        replaced: drawn.focus.clone(),
                    }),
                    Some(Holds::Build | Holds::Rest { .. }) => None,
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
        let focus = drawn.focus_node();
        match (&mut drawn.hover, cell) {
            (Some(hover), Some(cell)) if hover.cell == cell => hover.at = at,
            (None, None) => return,
            (hover, cell) => {
                *hover = cell.map(|cell| Hover {
                    cell,
                    at,
                    lines: describe(&drawn.tree, cell.holds, focus, &drawn.shared),
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

/// The picker among the units the graph `model` has read, the finest chosen;
/// absent until the graph is read.
fn unit_picker(
    model: &Entity<GraphModel>,
    cx: &mut Context<DominatorsPanel>,
) -> Option<UnitPicker> {
    let inputs = model.read(cx).loaded()?.shared.inputs;
    UnitPicker::new("dominators-unit", inputs, DominatorsPanel::choose, cx)
}

/// A tree drawn, and how the reader has it drawn, which lasts only as long as
/// the tree is drawn.
struct Drawn {
    tree: Arc<Dominators>,
    /// The graph whose points `tree` holds.
    shared: Shared,
    focus: Option<Focus>,
    hover: Option<Hover>,
    /// Where the first press of the last click landed.
    press: Option<Press>,
}

impl Drawn {
    fn focus_node(&self) -> Option<u32> {
        self.focus.as_ref().map(|focus| focus.node)
    }
}

/// Where the icicle is drawn from, with the path above it that the breadcrumb
/// names taken once as the focus is set.
#[derive(Clone)]
struct Focus {
    node: u32,
    /// Top first, those nearest `node` at most [`CRUMBS`] of them.
    above: Vec<u32>,
    /// Whether more dominators stand above those of `above`.
    elided: bool,
}

/// The most dominators above the focus the breadcrumb names.
const CRUMBS: usize = 4;

impl Focus {
    fn on(tree: &Dominators, node: u32) -> Self {
        let mut above: Vec<u32> = Vec::with_capacity(CRUMBS + 1);
        let up = std::iter::successors(tree.parent(node), |&above| tree.parent(above));
        above.extend(up.take(CRUMBS + 1));
        let elided = above.len() > CRUMBS;
        above.truncate(CRUMBS);
        above.reverse();
        Self {
            node,
            above,
            elided,
        }
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
fn describe(tree: &Dominators, holds: Holds, focus: Option<u32>, shared: &Shared) -> Vec<String> {
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
    let shares = |weight: u32, within: Option<u32>| {
        let of_build = format!("{} of the build", share(weight, tree.total()));
        match within {
            Some(within) => {
                let name = point_name(tree.id(within), unit, shared);
                format!(
                    "{of_build} · {} of {name}",
                    share(weight, tree.weight(within))
                )
            }
            None => of_build,
        }
    };
    let mut lines = Vec::with_capacity(TOOLTIP_LINES);
    match holds {
        Holds::Build => lines.extend([WHOLE_BUILD.to_owned(), points(tree.total())]),
        Holds::Node(node) => {
            identify(tree.id(node), tree.kind(node), unit, shared, &mut lines);
            let weight = tree.weight(node);
            lines.push(stand_on(weight, "this"));
            let is_focus = focus == Some(node);
            lines.push(shares(weight, focus.filter(|_| !is_focus)));
            let dominator = match tree.parent(node) {
                Some(parent) => point_name(tree.id(parent), unit, shared),
                None => WHOLE_BUILD.to_owned(),
            };
            lines.push(format!("immediately dominated by {dominator}"));
        }
        Holds::Rest { count, weight, .. } => {
            lines.push(format!("{} too narrow to draw apart", points(count)));
            lines.push(stand_on(weight, if count == 1 { "it" } else { "them" }));
            lines.push(shares(weight, focus));
        }
    }
    lines
}

/// What a click on the cell holding `holds` drawn from `focus` does, and a
/// double click where a graph panel `reveals` the point; absent where a click
/// does nothing.
fn gesture(holds: Holds, focus: Option<u32>, reveals: bool) -> Option<String> {
    let Holds::Node(node) = holds else {
        return None;
    };
    let click = if focus == Some(node) { FOCUS_UP } else { FOCUS };
    Some(match reveals {
        true => format!("{click} · {REVEAL}"),
        false => click.to_owned(),
    })
}

/// How a cell names the whole build.
const WHOLE_BUILD: &str = "the whole build";

/// What a click on a cell does, there and on the cell of the focus.
const FOCUS: &str = "click to focus";
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
                let many = unit_names(drawn.tree.scope().unit()).many;
                format!("{} {many}", drawn.tree.total())
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
            let computing = drawn.is_some_and(|drawn| drawn.tree.scope() != picker.chosen(cx));
            let crumbs = drawn.and_then(|drawn| breadcrumb(drawn, &palette, cx));
            strip(picker, crumbs, computing, &palette)
        }));
        let Some(drawn) = drawn else {
            return root.child(centered_spinner(COMPUTING, &palette.colors));
        };
        if drawn.tree.total() == 0 {
            let many = unit_names(drawn.tree.scope().unit()).many;
            let empty = format!("The graph holds no {many}.");
            return root.child(centered_notice(empty, &palette.colors));
        }
        root.child(
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

/// The path from the whole build down to the focus of `drawn`, each step but
/// the focus a click away; absent at the whole build. Where the path outgrows
/// its room the dominators above the focus give way first, and the focus is
/// cut short only once they are gone.
fn breadcrumb(drawn: &Drawn, palette: &Palette, cx: &Context<DominatorsPanel>) -> Option<Div> {
    let focus = drawn.focus.as_ref()?;
    let unit = drawn.tree.scope().unit();
    let name = |node: u32| point_name(drawn.tree.id(node), unit, &drawn.shared);
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
        let tree = Arc::clone(&drawn.tree);
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
    let last = after(crumb(name(focus.node)).into_any_element())
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
        let gesture = gesture(cell.holds, drawn.focus_node(), reveals);
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
            let drawn = parent.read(cx).drawn.as_ref()?;
            Some(Scene {
                tree: Arc::clone(&drawn.tree),
                focus: drawn.focus_node(),
                shared: drawn.shared.clone(),
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
