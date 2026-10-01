//! What exists in a build only because of each of its actions, configured
//! targets or targets: the dominator tree of the graph's points of one unit,
//! drawn as an icicle.

mod icicle;
mod paint;

use crate::Palette;
use crate::model::{DominatorsLanded, GraphModel, Shared, SourceReplaced};
use crate::point::{identify, unit_names};
use crate::toolbar::{busy, group};
use crate::unit_picker::UnitPicker;
use bviz_graph_layout::dominators::Dominators;
use bviz_graph_layout::metrics::Scope;
use bviz_ui_components::{
    Toolbar, TooltipColors, cached_layer, centered_notice, centered_spinner, paint_tooltip_box,
};
use bviz_ui_workspace::{Panel, PanelEvent, PanelKind, ViewKind};
use gpui::{
    App, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    MouseMoveEvent, Pixels, Point, Render, SharedString, Subscription, WeakEntity, Window, canvas,
    div, prelude::*, px,
};
use icicle::{Cell, Holds};
use paint::{MIN_CELL_WIDTH, Scene, point_name};
use std::sync::Arc;

/// The key context scoping the dominators panel's own bindings, so a key
/// pressed here reaches this panel and no other.
pub(crate) const KEY_CONTEXT: &str = "GraphDominators";

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

    /// Draws the tree of the chosen scope where it has landed and is not the
    /// one drawn. The tree drawn stays until then.
    fn redraw(&mut self, cx: &mut Context<Self>) {
        let Some(scope) = self.scope(cx) else {
            return;
        };
        let Some(tree) = self.model.read(cx).dominators(scope).cloned() else {
            return;
        };
        if self
            .drawn
            .as_ref()
            .is_some_and(|drawn| Arc::ptr_eq(&drawn.tree, &tree))
        {
            return;
        }
        self.drawn = Some(Drawn { tree, hover: None });
        self.invalidate_canvas(cx);
    }

    /// The cell a pointer at `at` is over, where the cells are drawn.
    fn cell_at(&self, at: Point<Pixels>) -> Option<Cell> {
        let (drawn, frame) = (self.drawn.as_ref()?, self.frame?);
        let cells = icicle::lay_out(&*drawn.tree, frame.bounds, frame.row, MIN_CELL_WIDTH);
        cells.into_iter().find(|cell| cell.rect.contains(&at))
    }

    /// Follows the pointer to the cell it is over, describing the cell afresh
    /// only where the pointer has moved onto another.
    fn hover_at(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let cell = self.cell_at(at);
        let model = self.model.read(cx);
        let (Some(drawn), Some(loaded)) = (&mut self.drawn, model.loaded()) else {
            return;
        };
        match (&mut drawn.hover, cell) {
            (Some(hover), Some(cell)) if hover.cell == cell => hover.at = at,
            (None, None) => return,
            (hover, cell) => {
                *hover = cell.map(|cell| Hover {
                    cell,
                    at,
                    lines: describe(&drawn.tree, cell.holds, &loaded.shared),
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

/// The tree drawn, and the cell of it the pointer is over.
struct Drawn {
    tree: Arc<Dominators>,
    hover: Option<Hover>,
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
const TOOLTIP_LINES: usize = 8;

/// What the tooltip over the cell holding `holds` of `tree` reads: what it
/// stands for, and how much of the build exists only because of it.
fn describe(tree: &Dominators, holds: Holds, shared: &Shared) -> Vec<String> {
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
    let of_build = |weight: u32| format!("{} of the build", share(weight, tree.total()));
    let mut lines = Vec::with_capacity(TOOLTIP_LINES);
    match holds {
        Holds::Build => lines.extend([WHOLE_BUILD.to_owned(), points(tree.total())]),
        Holds::Node(node) => {
            identify(tree.id(node), tree.kind(node), unit, shared, &mut lines);
            let weight = tree.weight(node);
            lines.push(stand_on(weight, "this"));
            lines.push(of_build(weight));
            let dominator = match tree.parent(node) {
                Some(parent) => point_name(tree.id(parent), unit, shared),
                None => WHOLE_BUILD.to_owned(),
            };
            lines.push(format!("immediately dominated by {dominator}"));
        }
        Holds::Rest { count, weight, .. } => {
            lines.push(format!("{} too narrow to draw apart", points(count)));
            lines.push(stand_on(weight, if count == 1 { "it" } else { "them" }));
            lines.push(of_build(weight));
        }
    }
    lines
}

/// How a cell names the whole build.
const WHOLE_BUILD: &str = "the whole build";

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
        let drawn_scope = self.drawn.as_ref().map(|drawn| drawn.tree.scope());
        let root = root.children(self.picker.as_ref().map(|picker| {
            let computing = drawn_scope.is_some_and(|drawn| drawn != picker.chosen(cx));
            strip(picker, computing, &palette)
        }));
        let Some(drawn) = &self.drawn else {
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

/// The strip choosing the unit drawn, which reads `computing` while the tree of
/// a new choice is computed over the one drawn for the last.
fn strip(picker: &UnitPicker, computing: bool, palette: &Palette) -> Toolbar {
    Toolbar::new()
        .child(group("points", palette).child(picker.dropdown()))
        .when(computing, |bar| {
            bar.trailing(busy("dominators-computing", "computing…", palette))
        })
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
        let Some(Hover { cell, at, lines }) = view
            .read(cx)
            .drawn
            .as_ref()
            .and_then(|drawn| drawn.hover.as_ref())
        else {
            return;
        };
        let (cell, at, lines) = (*cell, *at, lines.clone());
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
            let tree = Arc::clone(&panel.drawn.as_ref()?.tree);
            let shared = panel.model.read(cx).loaded()?.shared.clone();
            Some(Scene {
                tree,
                shared,
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
