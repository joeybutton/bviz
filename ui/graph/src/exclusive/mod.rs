//! What exists in a build only because of each of its actions, configured
//! targets or targets: the dominator tree of the graph's points of one unit,
//! drawn as an icicle.

mod icicle;
mod paint;
mod roots;
mod transition;

use crate::Palette;
use crate::drag::DOUBLE_CLICK;
use crate::hue::{Colouring, Hue, Swatch, legend_strip};
use crate::model::{DominatorsLanded, FilterChanged, GraphModel, Shared, SourceReplaced, TreeKey};
use crate::point::{KindNames, configuration, identify, unit_names};
use crate::toolbar::{busy, group};
use crate::unit_picker::UnitPicker;
use bviz_graph_layout::Id;
use bviz_graph_layout::dominators::{Chosen, Dominators, Elsewhere, Roots, Standing, Targets};
use bviz_graph_layout::metrics::{ByUnit, Count, Scope, Unit};
use bviz_graph_layout::select::{Filter, Verdicts};
use bviz_graph_layout::target_set::PatternError;
use bviz_ui_components::{
    QueryField, QueryFieldEvent, Toolbar, TooltipColors, cached_layer, centered_notice,
    centered_spinner, load_in_background, paint_tooltip_box,
};
use bviz_ui_workspace::{Panel, PanelEvent, PanelKind, ViewKind};
use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle,
    Focusable, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Point, Render,
    Rgba, SharedString, Subscription, Task, WeakEntity, Window, canvas, div, prelude::*, px,
    relative,
};
use icicle::{Cell, Holds, Parent, Run};
use paint::{
    HINT_SEPARATOR, MIN_CELL_WIDTH, Scene, cell_name, hints_among, point_name, run_name,
    shared_name,
};
use roots::{Found, Within};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use transition::Transition;

/// The key context scoping the panel's own bindings, so a key pressed here
/// reaches this panel and no other.
pub(crate) const KEY_CONTEXT: &str = "GraphExclusive";

pub(crate) use roots::KEY_CONTEXT as ROOTS_KEY_CONTEXT;

gpui::actions!(bviz_graph, [ShowWholeBuild, FocusUp, LeaveRoots]);

/// What the panel reads while the tree is being computed.
const COMPUTING: &str = "Computing exclusive deps…";

/// The dominator tree of a graph's points, drawn as an icicle.
pub struct ExclusivePanel {
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
    /// The roots chosen last, from the time the first are chosen.
    rooting: Option<Rooting>,
    /// Chooses the roots afresh, absent while none are being chosen; replaced
    /// by a later choice, which it would only hold up.
    choosing: Option<Choosing>,
    /// The field naming the roots as a target set.
    roots_field: Entity<QueryField>,
    /// The target set that last failed, which is not resolved again while
    /// the field holds it.
    failed: Option<Failed>,
    /// The trees of the graph read coloured so far that the graph still
    /// keeps.
    coloured: HashMap<TreeKey, Arc<Coloured>>,
    /// Colours a tree landed, absent while none is being coloured.
    drawing: Option<Task<()>>,
    /// Redraws as the graph is read.
    _changed: Subscription,
    _source_replaced: Subscription,
    _dominators_landed: Subscription,
    _filter_changed: Subscription,
    _roots_typed: Subscription,
}

impl ExclusivePanel {
    pub(crate) fn new(model: Entity<GraphModel>, cx: &mut Context<Self>) -> Self {
        let changed = cx.observe(&model, |_, _, cx| cx.notify());
        let source_replaced = cx.subscribe(&model, |view, _, _: &SourceReplaced, cx| {
            view.drawn = None;
            view.fade = None;
            view.fading = None;
            view.rooting = None;
            view.choosing = None;
            view.failed = None;
            view.coloured.clear();
            view.drawing = None;
            view.picker = unit_picker(&view.model, cx);
            view.compute(cx);
        });
        let dominators_landed = cx.subscribe(&model, |view, _, landed: &DominatorsLanded, cx| {
            if view.key(cx).as_ref() == Some(&landed.key) {
                view.redraw(cx);
            } else if view.target_key().as_ref() == Some(&landed.key) {
                view.rehover(cx);
            }
        });
        let filter_changed = cx.subscribe(&model, |view, _, _: &FilterChanged, cx| view.refade(cx));
        let roots_field = cx.new(|cx| {
            QueryField::new(
                "exclusive-roots",
                roots::PLACEHOLDER,
                roots::FIELD_WIDTH,
                cx,
            )
        });
        let roots_typed = cx.subscribe(&roots_field, |view, _, event, cx| match event {
            QueryFieldEvent::Edited => {}
            QueryFieldEvent::Settled => view.reroot(cx),
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
            fade: None,
            fading: None,
            rooting: None,
            choosing: None,
            roots_field,
            failed: None,
            coloured: HashMap::new(),
            drawing: None,
            _changed: changed,
            _source_replaced: source_replaced,
            _dominators_landed: dominators_landed,
            _filter_changed: filter_changed,
            _roots_typed: roots_typed,
        }
    }

    /// The scope whose tree is drawn, once the graph is read.
    fn scope(&self, cx: &App) -> Option<Scope> {
        self.picker.as_ref().map(|picker| picker.chosen(cx))
    }

    /// The tree drawn for the scope chosen, once its roots are chosen.
    fn key(&self, cx: &App) -> Option<TreeKey> {
        self.rooting(cx).map(Rooting::key)
    }

    /// The roots chosen last, where they were chosen in the scope chosen.
    fn rooting(&self, cx: &App) -> Option<&Rooting> {
        let scope = self.scope(cx)?;
        (self.rooting.as_ref()).filter(|rooting| rooting.scope() == scope)
    }

    /// The tree of targets a tooltip places the configured targets drawn in;
    /// absent where the tree drawn is of another unit.
    fn target_key(&self) -> Option<TreeKey> {
        self.drawn.as_ref()?.as_targets.clone()
    }

    /// Chooses the roots in the chosen scope and has the tree from them
    /// computed, where the panel is shown, drawing it where it already is.
    fn compute(&mut self, cx: &mut Context<Self>) {
        if !self.shown {
            return;
        }
        self.reroot(cx);
        self.grow(cx);
        self.refade(cx);
    }

    /// Has the tree of the chosen scope from the roots chosen in it computed,
    /// and draws it where it already is.
    fn grow(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.key(cx) else {
            return;
        };
        self.want(key, cx);
        self.redraw(cx);
    }

    /// The target set the roots field holds; none where it is empty.
    fn target_set(&self, cx: &App) -> Option<SharedString> {
        let text = self.roots_field.read(cx).text(cx);
        (!text.trim().is_empty()).then_some(text)
    }

    /// How the target set the roots field holds failed in the scope chosen,
    /// where it is the set that last failed.
    fn failure(&self, cx: &App) -> Option<&Failed> {
        let (scope, text) = (self.scope(cx)?, self.target_set(cx)?);
        (self.failed.as_ref()).filter(|failed| failed.scope == scope && failed.text == text)
    }

    /// Chooses the roots of the chosen scope by the target set the roots
    /// field holds, or as the top-level points with the field empty, where
    /// neither the roots in place nor those being chosen are chosen so. A
    /// target set that failed gives up the choice under way and leaves in
    /// place the roots an earlier set named in the scope, and otherwise
    /// stands for the top-level points.
    fn reroot(&mut self, cx: &mut Context<Self>) {
        let Some(scope) = self.scope(cx) else {
            return;
        };
        let failed = self.failure(cx).is_some();
        let set = self.target_set(cx).filter(|_| !failed);
        let stands = self.rooting(cx).is_some_and(|rooting| match rooting {
            Rooting::Targets { .. } if failed => true,
            rooting => rooting.set() == set.as_ref(),
        });
        if stands {
            self.choosing = None;
            return;
        }
        match set {
            None => {
                self.choosing = None;
                self.root(Rooting::Tops { scope }, cx);
            }
            Some(text) => {
                let choosing = self.choosing.as_ref();
                if !choosing.is_some_and(|choosing| choosing.is(scope, &text)) {
                    self.root_by_targets(scope, text, cx);
                }
            }
        }
    }

    /// Chooses the roots of `scope` the target set `text` names.
    fn root_by_targets(&mut self, scope: Scope, text: SharedString, cx: &mut Context<Self>) {
        let prepared = self.model.update(cx, |model, _| {
            Some((model.targets()?, Within::of(scope, model)?))
        });
        let Some((targets, within)) = prepared else {
            return;
        };
        let (set, landing) = (text.clone(), text.clone());
        let work = load_in_background(
            cx,
            move || roots::find(&set, &targets, &within),
            move |view, found, cx| view.found(scope, landing, found, cx),
        );
        self.choosing = Some(Choosing {
            scope,
            text,
            _work: work,
        });
    }

    /// Roots the tree of `scope` at what the target set `text` came to. A
    /// set that fails is kept as the one that failed, and the roots are
    /// chosen as [`Self::reroot`] chooses them under a failed set.
    fn found(
        &mut self,
        scope: Scope,
        text: SharedString,
        found: Result<Found, PatternError>,
        cx: &mut Context<Self>,
    ) {
        self.choosing = None;
        match found {
            Ok(Found {
                roots,
                named,
                notices,
            }) => {
                let rooting = Rooting::Targets {
                    scope,
                    text,
                    roots,
                    named,
                    notices,
                };
                self.root(rooting, cx);
            }
            Err(error) => {
                self.failed = Some(Failed { scope, text, error });
                self.reroot(cx);
            }
        }
    }

    /// Takes `rooting` for the roots chosen and has the tree from them
    /// computed.
    fn root(&mut self, rooting: Rooting, cx: &mut Context<Self>) {
        self.rooting = Some(rooting);
        self.grow(cx);
        cx.notify();
    }

    /// Has the tree of `key`, the one chosen, computed, and once that is the
    /// tree drawn, the tree of targets a tooltip places its points in, so
    /// that the second holds no worker from the first.
    fn want(&mut self, key: TreeKey, cx: &mut Context<Self>) {
        let landed = self
            .drawn
            .as_ref()
            .is_some_and(|drawn| drawn.coloured.key == key);
        let as_targets = self.target_key().filter(|_| landed);
        let wanted: Vec<TreeKey> = std::iter::once(key).chain(as_targets).collect();
        self.model
            .update(cx, |model, cx| model.want_dominators(&wanted, cx));
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

    /// Draws the tree of the chosen scope from the roots chosen in it once it
    /// has landed and been coloured, where it is not the one drawn. Colouring is a
    /// pass over the whole tree, so it runs off the UI thread, one at a time,
    /// and a colouring running draws again once it lands. The tree drawn stays
    /// until then.
    fn redraw(&mut self, cx: &mut Context<Self>) {
        let (Some(key), Some(rooting)) = (self.key(cx), self.rooting(cx)) else {
            return;
        };
        let as_targets = rooting.as_targets();
        if let Some(drawn) = &mut self.drawn
            && drawn.coloured.key == key
        {
            // Two target sets can name the roots of one tree and different
            // targets.
            if drawn.as_targets != as_targets {
                drawn.as_targets = as_targets;
                self.want(key, cx);
                self.rehover(cx);
            }
            return;
        }
        let model = self.model.read(cx);
        self.coloured
            .retain(|key, _| model.dominators(key).is_some());
        if let Some(coloured) = self.coloured.get(&key) {
            self.drawn = Some(Drawn {
                coloured: Arc::clone(coloured),
                as_targets,
                focus: None,
                transition: None,
                pointer: None,
                hover: None,
                press: None,
            });
            if self.shown {
                self.want(key, cx);
            }
            self.invalidate_canvas(cx);
            return;
        }
        if self.drawing.is_some() {
            return;
        }
        let model = self.model.read(cx);
        let (Some(tree), Some(loaded)) = (model.dominators(&key), model.loaded()) else {
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
                let targets =
                    (unit == Unit::ConfiguredTarget).then(|| Targets::of(&tree, &shared.facets));
                let reached = tree.roots().any(|root| paint::reached(&tree, root));
                Coloured {
                    key,
                    tree,
                    shared,
                    colouring: Arc::new(colouring),
                    legend,
                    targets,
                    reached,
                }
            },
            |view, coloured, cx| {
                view.drawing = None;
                view.coloured
                    .insert(coloured.key.clone(), Arc::new(coloured));
                view.redraw(cx);
            },
        ));
    }

    /// The cell a pointer at `at` is over, where the cells are drawn and at
    /// rest.
    fn cell_at(&self, at: Point<Pixels>) -> Option<Cell> {
        let (drawn, frame) = (self.drawn.as_ref()?, self.frame?);
        if drawn.transition.is_some() {
            return None;
        }
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
    /// none, and describes the cell that brings under the pointer. A move
    /// under way ends at once on the focus it was making for, and the move to
    /// `focus` starts from there, so a key held or pressed again mid-move
    /// goes a step further each time.
    fn refocus(&mut self, focus: Option<Focus>, cx: &mut Context<Self>) {
        let Some(drawn) = &mut self.drawn else {
            return;
        };
        let from = drawn.top();
        drawn.focus = focus;
        let tree = &*drawn.coloured.tree;
        drawn.transition = Transition::between(tree, from, drawn.top(), Instant::now());
        self.invalidate_canvas(cx);
        self.rehover(cx);
    }

    /// Draws the icicle at rest once the last frame of `transition` is
    /// painted, where that is still the move under way.
    fn settle(&mut self, transition: &Transition, cx: &mut Context<Self>) {
        let Some(drawn) = &mut self.drawn else {
            return;
        };
        if drawn.transition.as_ref() == Some(transition) {
            drawn.transition = None;
            self.invalidate_canvas(cx);
            self.rehover(cx);
        }
    }

    /// Describes the cell under the pointer afresh.
    fn rehover(&mut self, cx: &mut Context<Self>) {
        let Some(drawn) = &mut self.drawn else {
            return;
        };
        drawn.hover = None;
        if let Some(at) = drawn.pointer {
            self.hover_at(at, cx);
        }
    }

    /// Focuses `on` of `tree` as [`Self::focus_on`] does, where `tree` is
    /// still the tree drawn.
    fn focus_on_in(&mut self, tree: &Arc<Dominators>, on: Option<Focused>, cx: &mut Context<Self>) {
        if self
            .drawn
            .as_ref()
            .is_some_and(|drawn| Arc::ptr_eq(&drawn.coloured.tree, tree))
        {
            self.focus_on(on, cx);
        }
    }

    /// Draws the icicle from what the focus stands under.
    fn focus_up(&mut self, cx: &mut Context<Self>) {
        let Some(drawn) = &self.drawn else {
            return;
        };
        if let Some(on) = drawn.focused() {
            let up = Crumb::of(on.parent(&drawn.coloured.tree));
            self.focus_on(up.map(Focused::from), cx);
        }
    }

    /// Focuses what a click on the cell holding `holds` asks for: what it
    /// holds, or what that stands under where it is the cell on top.
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
            // The cell under a press while the icicle moves is not the one
            // the reader aimed at.
            1 if drawn.transition.is_some() => drawn.press = None,
            1 => {
                drawn.press = match cell.map(|cell| cell.holds) {
                    None => Some(Press::Clear),
                    Some(Holds::Node(node)) => Some(Press::Point {
                        id: drawn.coloured.tree.id(node),
                        replaced: drawn.focus.clone(),
                    }),
                    Some(Holds::Build | Holds::Shared | Holds::Rest(_)) => None,
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
        let as_targets = self
            .target_key()
            .and_then(|key| self.model.read(cx).dominators(&key).cloned());
        let Some(drawn) = &mut self.drawn else {
            return;
        };
        drawn.pointer = Some(at);
        let focus = drawn.focused();
        match (&mut drawn.hover, cell) {
            (Some(hover), Some(cell)) if hover.cell == cell => hover.at = at,
            (None, None) => return,
            (hover, cell) => {
                *hover = cell.map(|cell| Hover {
                    cell,
                    at,
                    lines: describe(&drawn.coloured, as_targets.as_deref(), cell.holds, focus),
                });
            }
        }
        cx.notify();
    }

    /// Drops the tooltip, which names the cell under a pointer that has left.
    fn clear_hover(&mut self, cx: &mut Context<Self>) {
        if let Some(drawn) = &mut self.drawn {
            drawn.pointer = None;
            if drawn.hover.take().is_some() {
                cx.notify();
            }
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
fn unit_picker(model: &Entity<GraphModel>, cx: &mut Context<ExclusivePanel>) -> Option<UnitPicker> {
    let inputs = model.read(cx).loaded()?.shared.inputs;
    let opening = Some(Unit::ConfiguredTarget);
    UnitPicker::new(
        "exclusive-unit",
        inputs,
        opening,
        ExclusivePanel::choose,
        cx,
    )
}

/// The roots a tree of one scope is drawn from, with what chose them.
enum Rooting {
    /// Every top-level point of the scope.
    Tops { scope: Scope },
    /// The points of the scope a target set names.
    Targets {
        scope: Scope,
        /// The set, as typed.
        text: SharedString,
        roots: Chosen,
        /// The targets the set names.
        named: Chosen,
        /// What the roots leave unsaid of the set.
        notices: Vec<String>,
    },
}

impl Rooting {
    fn scope(&self) -> Scope {
        match *self {
            Rooting::Tops { scope } | Rooting::Targets { scope, .. } => scope,
        }
    }

    /// The target set that chose the roots; none where they are the
    /// top-level points.
    fn set(&self) -> Option<&SharedString> {
        match self {
            Rooting::Tops { .. } => None,
            Rooting::Targets { text, .. } => Some(text),
        }
    }

    fn roots(&self) -> Roots {
        match self {
            Rooting::Tops { .. } => Roots::Tops,
            Rooting::Targets { roots, .. } => Roots::Chosen(roots.clone()),
        }
    }

    /// The tree drawn from the roots.
    fn key(&self) -> TreeKey {
        TreeKey {
            scope: self.scope(),
            roots: self.roots(),
        }
    }

    /// The tree of targets a tooltip places the scope's points in, rooted as
    /// what chose the roots roots a tree of targets; absent where the points
    /// are no configured targets.
    fn as_targets(&self) -> Option<TreeKey> {
        let ByUnit::ConfiguredTarget { targets: scope } = self.scope().by_unit() else {
            return None;
        };
        let roots = match self {
            Rooting::Tops { .. } => Roots::Tops,
            Rooting::Targets { named, .. } => Roots::Chosen(named.clone()),
        };
        Some(TreeKey { scope, roots })
    }

    fn notices(&self) -> &[String] {
        match self {
            Rooting::Tops { .. } => &[],
            Rooting::Targets { notices, .. } => notices,
        }
    }
}

/// A choice of the roots of `scope` by the target set `text`, under way.
struct Choosing {
    scope: Scope,
    text: SharedString,
    /// Dropping it gives the choice up.
    _work: Task<()>,
}

impl Choosing {
    fn is(&self, scope: Scope, text: &SharedString) -> bool {
        self.scope == scope && self.text == *text
    }
}

/// The target set `text`, which names no roots in `scope`.
struct Failed {
    scope: Scope,
    text: SharedString,
    error: PatternError,
}

/// How the roots of a tree were chosen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RootedBy {
    /// As every top-level point.
    Tops,
    /// As the points a target set names.
    Targets,
}

impl RootedBy {
    fn of(roots: &Roots) -> Self {
        match roots {
            Roots::Tops => RootedBy::Tops,
            Roots::Chosen(_) => RootedBy::Targets,
        }
    }
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
    key: TreeKey,
    tree: Arc<Dominators>,
    /// The graph whose points `tree` holds.
    shared: Shared,
    colouring: Arc<Colouring>,
    legend: Vec<(SharedString, Hue)>,
    /// The targets each node's subtree holds, where its points are configured
    /// targets.
    targets: Option<Targets>,
    /// Whether the build also reaches a root of `tree` as a dependency.
    reached: bool,
}

impl Coloured {
    fn by(&self) -> RootedBy {
        RootedBy::of(&self.key.roots)
    }
}

/// A tree drawn, and how the reader has it drawn, which lasts only as long as
/// the tree is drawn.
struct Drawn {
    coloured: Arc<Coloured>,
    /// The [`Rooting::as_targets`] of the roots the tree is drawn from.
    as_targets: Option<TreeKey>,
    focus: Option<Focus>,
    /// The move to `focus` from the focus before it, while it is under way.
    transition: Option<Transition>,
    /// Where the pointer rests over the icicle, while it does.
    pointer: Option<Point<Pixels>>,
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

    /// Whether a cell the icicle draws from its top can stand for a root
    /// the build also reaches as a dependency.
    fn draws_reached(&self) -> bool {
        match &self.focus {
            None => self.coloured.reached,
            Some(focus) => focus.reached,
        }
    }
}

/// What the icicle is drawn from short of the whole build.
#[derive(Clone, Copy, PartialEq)]
enum Focused {
    /// The shared tops.
    Shared,
    Node(u32),
    Run(Run),
}

impl Focused {
    /// What focusing the cell holding `holds` draws from; none for the whole
    /// build.
    fn of(holds: Holds) -> Option<Self> {
        match holds {
            Holds::Build => None,
            Holds::Shared => Some(Self::Shared),
            Holds::Node(node) => Some(Self::Node(node)),
            Holds::Rest(run) => Some(Self::Run(run)),
        }
    }

    fn holds(self) -> Holds {
        match self {
            Self::Shared => Holds::Shared,
            Self::Node(node) => Holds::Node(node),
            Self::Run(run) => Holds::Rest(run),
        }
    }

    /// The node this is, where it is one.
    fn node(self) -> Option<u32> {
        match self {
            Self::Node(node) => Some(node),
            Self::Shared | Self::Run(_) => None,
        }
    }

    /// What this stands under in `tree`.
    fn parent(self, tree: &Dominators) -> Parent {
        match self {
            Self::Shared => Crumb::Shared.parent(tree),
            Self::Node(node) => Crumb::Node(node).parent(tree),
            Self::Run(run) => run.parent(),
        }
    }

    /// The points of `tree` this stands over.
    fn weight(self, tree: &Dominators) -> u32 {
        match self {
            Self::Shared => tree.shared_weight(),
            Self::Node(node) => tree.weight(node),
            Self::Run(run) => run.weight(),
        }
    }

    /// How a tooltip or the breadcrumb names this in `tree`, whose roots
    /// were chosen `by`.
    fn name(self, tree: &Dominators, shared: &Shared, by: RootedBy) -> String {
        match self {
            Self::Shared => shared_name(tree, by),
            Self::Node(node) => point_name(tree.id(node), tree.scope().unit(), shared),
            Self::Run(run) => run_name(tree, shared, by, run),
        }
    }
}

/// What a focus stands under short of the whole build, which the breadcrumb
/// names a step for.
#[derive(Clone, Copy, PartialEq)]
enum Crumb {
    /// The shared tops.
    Shared,
    Node(u32),
}

impl Crumb {
    /// The step for `parent`; none for the whole build.
    fn of(parent: Parent) -> Option<Self> {
        match parent {
            Parent::Build => None,
            Parent::Shared => Some(Self::Shared),
            Parent::Node(node) => Some(Self::Node(node)),
        }
    }

    /// What this stands under in `tree`.
    fn parent(self, tree: &Dominators) -> Parent {
        match self {
            Self::Shared => Parent::Build,
            Self::Node(node) => icicle::Tree::parent(tree, node),
        }
    }
}

impl From<Crumb> for Focused {
    fn from(crumb: Crumb) -> Self {
        match crumb {
            Crumb::Shared => Self::Shared,
            Crumb::Node(node) => Self::Node(node),
        }
    }
}

/// Where the icicle is drawn from, with what is read of the tree for it
/// taken once as the focus is set.
#[derive(Clone)]
struct Focus {
    on: Focused,
    /// Top first, those nearest `on` at most [`CRUMBS`] of them.
    above: Vec<Crumb>,
    /// Whether more steps stand above those of `above`.
    elided: bool,
    /// Whether a cell drawn from `on` can stand for a root the build also
    /// reaches as a dependency: `on` itself, or where it is a run of roots,
    /// one it gathers.
    reached: bool,
}

/// The most steps above the focus the breadcrumb names.
const CRUMBS: usize = 4;

impl Focus {
    fn on(tree: &Dominators, on: Focused) -> Self {
        let mut above: Vec<Crumb> = Vec::with_capacity(CRUMBS + 1);
        let nearest = Crumb::of(on.parent(tree));
        let up = std::iter::successors(nearest, |above| Crumb::of(above.parent(tree)));
        above.extend(up.take(CRUMBS + 1));
        let elided = above.len() > CRUMBS;
        above.truncate(CRUMBS);
        above.reverse();
        let reached = match on {
            Focused::Node(node) => paint::reached(tree, node),
            Focused::Run(run) if run.parent() == Parent::Build => {
                run.siblings().any(|root| paint::reached(tree, root))
            }
            Focused::Shared | Focused::Run(_) => false,
        };
        Self {
            on,
            above,
            elided,
            reached,
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

/// What the tooltip over the cell holding `holds` of the tree of `coloured`
/// drawn from `focus` reads: what it stands for, how much of the build exists
/// only because of it, and where its target stands in `as_targets`, the tree
/// of targets rooted by the choice that rooted the tree drawn.
fn describe(
    coloured: &Coloured,
    as_targets: Option<&Dominators>,
    holds: Holds,
    focus: Option<Focused>,
) -> Vec<String> {
    let (tree, shared, by) = (&*coloured.tree, &coloured.shared, coloured.by());
    let unit = tree.scope().unit();
    let names = unit_names(unit);
    let stand_on = |weight: u32, on: &str| exist_only_because(weight, &names, on);
    let shares = |weight: u32, within: Option<Focused>| {
        let of_build = format!("{} of the build", share(weight, tree.total()));
        match within {
            Some(within) => {
                let name = match within {
                    Focused::Shared => what_is_shared(by.word(), names.many),
                    Focused::Node(_) | Focused::Run(_) => within.name(tree, shared, by),
                };
                format!(
                    "{of_build} · {} of {name}",
                    share(weight, within.weight(tree))
                )
            }
            None => of_build,
        }
    };
    let on_top = Focused::of(holds) == focus;
    let mut lines = Vec::new();
    match holds {
        Holds::Build => lines.extend([WHOLE_BUILD.to_owned(), names.counted(tree.total())]),
        Holds::Shared => {
            let (count, weight) = (tree.shared_tops().len() as u32, tree.shared_weight());
            lines.push(shared_name(tree, by));
            lines.push(gathered_as_shared(count, by.word(), &names));
            lines.push(stand_on(weight, if count == 1 { "it" } else { "them" }));
            lines.push(shares(weight, focus.filter(|_| !on_top)));
        }
        Holds::Node(node) => {
            identify(tree.id(node), tree.kind(node), unit, shared, &mut lines);
            let weight = tree.weight(node);
            let copies = coloured
                .targets
                .as_ref()
                .map(|targets| targets.copies(node));
            let this = if copies.is_some_and(|copies| copies > 1) {
                "this copy"
            } else {
                "this"
            };
            lines.push(stand_on(weight, this));
            if let Some(targets) = &coloured.targets {
                let held = targets.held(node);
                lines.extend(targets_held(held.targets(), held.only_here()));
            }
            lines.push(shares(weight, focus.filter(|_| !on_top)));
            match tree.parent(node) {
                Some(parent) => lines.push(dominated_by(tree, parent, shared)),
                None => standing(tree, node, by, shared, &mut lines),
            }
            let id = tree.id(node);
            lines.extend(as_targets.and_then(|targets| as_target(targets, id, by, shared)));
        }
        Holds::Rest(run) => {
            let (count, weight) = (run.count(), run.weight());
            lines.push(match on_top {
                true => run_name(tree, shared, by, run),
                false => format!("{} too narrow to draw apart", names.counted(count)),
            });
            lines.push(stand_on(weight, if count == 1 { "it" } else { "them" }));
            lines.push(shares(weight, focus.filter(|_| !on_top)));
        }
    }
    lines
}

/// What a tooltip says of the `targets` distinct targets a configured
/// target's subtree holds, `only_here` of which are configured nowhere else;
/// absent where it holds none.
fn targets_held(targets: u32, only_here: u32) -> Option<String> {
    let elsewhere = match (targets, only_here) {
        (0, _) => return None,
        (1, 0) => "also configured elsewhere".to_owned(),
        (1, _) => "configured only here".to_owned(),
        (_, 0) => "each also configured elsewhere".to_owned(),
        _ if only_here == targets => "all configured only here".to_owned(),
        _ => format!("{only_here} of them configured only here"),
    };
    let covered = unit_names(Unit::Target).counted(targets);
    Some(format!("covering {covered}, {elsewhere}"))
}

/// Where the target of the configured target `id` stands in `targets`, the
/// tree of targets whose roots were chosen `by`, and how many targets exist
/// only because of it; absent where it names no target of the tree.
fn as_target(targets: &Dominators, id: Id, by: RootedBy, shared: &Shared) -> Option<String> {
    let node = targets.index_of(shared.facets.label(id)?)?;
    let names = unit_names(Unit::Target);
    let word = by.word();
    let place = match (targets.parent(node), targets.standing(node)) {
        (Some(parent), _) => dominated_by(targets, parent, shared),
        (None, Standing::Root { .. } | Standing::ReachedRoot(_)) => word.to_owned(),
        (None, Standing::Below(count)) => shared_by(count, word, names.of(count.value())),
    };
    let held = exist_only_because(targets.weight(node), &names, "it");
    Some(format!("as a target: {place}; {held}"))
}

/// How a tooltip says that `weight` points, which `names` names, exist only
/// because of `on`.
fn exist_only_because(weight: u32, names: &KindNames, on: &str) -> String {
    let exist = if weight == 1 { "exists" } else { "exist" };
    format!("{} {exist} only because of {on}", names.counted(weight))
}

/// How a tooltip names `parent` of `tree` as the immediate dominator of the
/// point it describes.
fn dominated_by(tree: &Dominators, parent: u32, shared: &Shared) -> String {
    let dominator = point_name(tree.id(parent), tree.scope().unit(), shared);
    format!("immediately dominated by {dominator}")
}

/// Pushes onto `lines` what a tooltip says of `node`, a top of `tree` whose
/// roots were chosen `by`: that it is a root, and how many roots reach it or
/// share it, listing the first few.
fn standing(tree: &Dominators, node: u32, by: RootedBy, shared: &Shared, lines: &mut Vec<String>) {
    let unit = tree.scope().unit();
    let word = by.word();
    let elsewhere = tree
        .elsewhere(node)
        .map(|elsewhere| reached_elsewhere(elsewhere, unit, shared));
    let (count, said): (_, fn(Count, &str, &str) -> String) = match tree.standing(node) {
        Standing::Root { in_cycle } => {
            return match elsewhere {
                None => lines.push(format!("{word}: {}", by.why(in_cycle))),
                Some(elsewhere) => {
                    lines.push(format!("{word} here, and a dependency elsewhere:"));
                    lines.extend(elsewhere);
                }
            };
        }
        Standing::ReachedRoot(count) => (count, reached_from),
        Standing::Below(count) => (count, shared_by),
    };
    let named = tree.roots_above(node, NAMED_ROOTS);
    let colon = if named.is_empty() { "" } else { ":" };
    let named_as = unit_names(unit).of(count.value());
    lines.push(format!("{}{colon}", said(count, word, named_as)));
    let listed = named
        .iter()
        .map(|&above| point_name(tree.id(above), unit, shared));
    lines.extend(listed.map(|name| format!("{LISTED}{name}")));
    let more = count.less(named.len() as u32);
    if !named.is_empty() && more.value() > 0 {
        lines.push(format!("{LISTED}and {more} more"));
    }
    if let Some(elsewhere) = elsewhere {
        lines.push("and a dependency elsewhere:".to_owned());
        lines.extend(elsewhere);
    }
}

/// What a tooltip lists of a root of `unit` another copy of whose target the
/// build reaches through a dependency, as `elsewhere` tells: a dependent
/// reaching that copy and the configuration of the copy, and how many more
/// copies are reached so.
fn reached_elsewhere(elsewhere: Elsewhere, unit: Unit, shared: &Shared) -> Vec<String> {
    let dependent = point_name(elsewhere.dependent, unit, shared);
    let copy = match configuration(elsewhere.copy, shared) {
        Some(configuration) => format!("its copy in {configuration}"),
        None => "another copy of it".to_owned(),
    };
    let reached = format!("{LISTED}{dependent} reaches {copy}");
    let more = other_copies(elsewhere.copies.saturating_sub(1));
    std::iter::once(reached).chain(more).collect()
}

/// What a tooltip says of the `more` copies of a root's target, beside the
/// one it names, that the build reaches through a dependency.
fn other_copies(more: u32) -> Option<String> {
    match more {
        0 => None,
        1 => Some(format!("{LISTED}1 other copy is a dependency too")),
        _ => Some(format!("{LISTED}{more} other copies are dependencies too")),
    }
}

/// Leads each line of a list within a tooltip.
const LISTED: &str = "  · ";

/// What a reader calls a point nothing depends on but the other members of
/// a cycle it belongs to.
const TOP_LEVEL: &str = "top-level";

/// What a reader calls a point a target set names.
const REQUESTED: &str = "requested";

impl RootedBy {
    /// What a reader calls the roots so chosen.
    fn word(self) -> &'static str {
        match self {
            RootedBy::Tops => TOP_LEVEL,
            RootedBy::Targets => REQUESTED,
        }
    }

    /// What the legend says of the roots so chosen that the build also
    /// reaches through a dependency. Nothing depends on a top-level point,
    /// so there it is another copy of its target that something depends on.
    fn also_a_dependency(self) -> &'static str {
        match self {
            RootedBy::Tops => "whose target is also a dependency",
            RootedBy::Targets => "also reached as a dependency",
        }
    }

    /// Why a root so chosen is one, where nothing reaches it from outside its
    /// cycle, which it is `in_cycle` with.
    fn why(self, in_cycle: bool) -> &'static str {
        match (self, in_cycle) {
            (RootedBy::Tops, false) => "nothing depends on it",
            (RootedBy::Tops, true) => "nothing outside its cycle depends on it",
            (RootedBy::Targets, _) => "named by the target set",
        }
    }
}

/// How a reader says that `how_many` roots, called `word` `named`, share a
/// point.
fn shared_by(how_many: impl std::fmt::Display, word: &str, named: &str) -> String {
    format!("shared by {how_many} {word} {named}")
}

/// How a reader counts the roots sharing the shared tops.
const SEVERAL: &str = "several";

/// How a reader names the shared tops as what a share or a run is taken of,
/// the roots being called `word` `named`.
fn what_is_shared(word: &str, named: &str) -> String {
    format!("what {SEVERAL} {word} {named} share")
}

/// What a tooltip says of the `count` shared tops, which `names` names, the
/// roots being called `word`.
fn gathered_as_shared(count: u32, word: &str, names: &KindNames) -> String {
    let (shared, sharing) = (names.counted(count), names.many);
    format!("{shared} that two or more {word} {sharing} depend on")
}

/// How a reader says that `how_many` roots besides a root, called `word`
/// `named`, reach it.
fn reached_from(how_many: impl std::fmt::Display, word: &str, named: &str) -> String {
    format!("{word}, and reached from {how_many} other {word} {named}")
}

/// The most roots a tooltip names as reaching a top.
const NAMED_ROOTS: usize = 3;

/// What a click on the cell holding `holds` under `top` does, and a double
/// click where it holds a point and a graph panel `reveals` it; absent where a
/// click does nothing.
fn gesture(holds: Holds, top: Holds, reveals: bool) -> Option<String> {
    let click = match holds {
        Holds::Build => return None,
        _ if holds == top => FOCUS_UP,
        Holds::Node(_) => FOCUS,
        Holds::Shared | Holds::Rest(_) => OPEN,
    };
    Some(match (holds, reveals) {
        (Holds::Node(_), true) => format!("{click} · {REVEAL}"),
        _ => click.to_owned(),
    })
}

/// How a cell names the whole build.
const WHOLE_BUILD: &str = "the whole build";

/// What a click on a cell does: on a point, on a cell gathering several, and
/// on the cell on top.
const FOCUS: &str = "click to focus";
const OPEN: &str = "click to open";
const FOCUS_UP: &str = "click or Backspace to go up a level";
const REVEAL: &str = "double-click to show in the graph";

/// What share of `whole` `part` is, as a percentage.
fn share(part: u32, whole: u32) -> String {
    format!("{:.1}%", f64::from(part) * PERCENT / f64::from(whole))
}

const PERCENT: f64 = 100.0;

/// What the status says of a tree of `total` points of `unit` from `roots`
/// roots, leaving out the `outside` points of its scope none of them reaches.
fn tally(unit: Unit, total: u32, roots: u32, outside: u32) -> String {
    let held = unit_names(unit).counted(total);
    let rooted = format!(
        "{held} · {roots} {}",
        if roots == 1 { "root" } else { "roots" }
    );
    match outside {
        0 => rooted,
        _ => format!("{rooted} · {outside} outside"),
    }
}

impl EventEmitter<PanelEvent> for ExclusivePanel {}

impl Focusable for ExclusivePanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for ExclusivePanel {
    fn kind() -> PanelKind {
        PanelKind {
            view: ViewKind::new("graph-exclusive"),
            label: "Exclusive deps",
            key_context: KEY_CONTEXT,
        }
    }

    fn title(&self, _cx: &App) -> SharedString {
        Self::kind().label.into()
    }

    fn description() -> Option<&'static str> {
        Some("What exists only because of each target (the build's dominator tree)")
    }

    fn status(&self, cx: &App) -> Option<SharedString> {
        let model = self.model.read(cx);
        let status = model.source().status("No graph", |_| match &self.drawn {
            None => COMPUTING.into(),
            Some(drawn) => {
                let tree = &drawn.coloured.tree;
                tally(
                    tree.scope().unit(),
                    tree.total(),
                    tree.roots().len() as u32,
                    tree.outside(),
                )
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

impl Render for ExclusivePanel {
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
        let key = self.key(cx);
        let root = root.children(self.picker.as_ref().map(|picker| {
            let computing = self.choosing.is_some()
                || drawn.is_some_and(|drawn| Some(&drawn.coloured.key) != key.as_ref());
            let field = roots::field(&self.roots_field, &palette, cx);
            let crumbs = drawn.and_then(|drawn| breadcrumb(drawn, &palette, cx));
            strip(picker, field, crumbs, computing, &palette)
        }));
        let rooting = self.rooting(cx);
        let report = match self.failure(cx) {
            Some(failed) => {
                let by = rooting.map(|rooting| RootedBy::of(&rooting.roots()));
                Some(roots::failure_line(&failed.error, by, &palette))
            }
            None => rooting
                .map(Rooting::notices)
                .filter(|notices| !notices.is_empty())
                .map(|notices| roots::notices_line(notices, &palette)),
        };
        let root = root.children(report);
        let Some(drawn) = drawn else {
            return root.child(centered_spinner(COMPUTING, &palette.colors));
        };
        let tree = &drawn.coloured.tree;
        if tree.total() == 0 {
            let many = unit_names(tree.scope().unit()).many;
            let empty = match drawn.coloured.by() {
                RootedBy::Tops => format!("The graph holds no {many}."),
                RootedBy::Targets => format!("The target set names no {many} of the graph."),
            };
            return root.child(centered_notice(empty, &palette.colors));
        }
        let swatch = Swatch {
            alpha: 1.0,
            diameter: LEGEND_SWATCH,
        };
        // A legend of one hue names nothing the unit picked does not.
        let hues = match drawn.coloured.legend.len() {
            0 | 1 => &[][..],
            _ => &drawn.coloured.legend[..],
        };
        let unit = drawn.coloured.tree.scope().unit();
        // The key's cells wear a hue the cells drawn wear.
        let key_fill = match drawn.coloured.legend.first() {
            Some((_, hue)) => hue.color(&palette),
            None => palette.data.neutral.into(),
        };
        let key = drawn
            .draws_reached()
            .then(|| reached_key(unit, drawn.coloured.by(), key_fill, &palette));
        let legend = (!hues.is_empty() || key.is_some())
            .then(|| legend_strip(hues, swatch, key.into_iter().flatten(), &palette));
        root.children(legend).child(
            div()
                .id("exclusive-icicle")
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

/// The strip choosing the unit drawn and the roots `field`, followed by
/// `crumbs`, which reads `computing` while the roots of a new choice are
/// chosen and its tree is computed over the one drawn for the last.
fn strip(
    picker: &UnitPicker,
    field: impl IntoElement,
    crumbs: Option<Div>,
    computing: bool,
    palette: &Palette,
) -> Toolbar {
    Toolbar::new()
        .child(group("points", palette).child(picker.dropdown()))
        .child(field)
        .when_some(crumbs, |bar, crumbs| bar.child(crumbs))
        .when(computing, |bar| {
            bar.trailing(busy("exclusive-computing", "computing…", palette))
        })
}

/// The legend's key to the notch, telling the roots of `unit`, chosen `by`,
/// that the build also reaches as a dependency apart from the others, each
/// beside a cell filled `fill` drawn as the icicle draws it.
fn reached_key(unit: Unit, by: RootedBy, fill: Rgba, palette: &Palette) -> [Div; 2] {
    let ink = paint::notch_ink(fill, palette.background);
    let entry = |reached: bool, name: String| {
        let notch = reached.then(|| {
            canvas(
                |_, _, _| {},
                move |cell, _, window, _| {
                    paint::paint_notch(cell, ink, window);
                },
            )
            .size_full()
        });
        let cell = div()
            .flex_none()
            .w(KEY_CELL_WIDTH)
            .h(paint::NOTCH * 2.)
            .bg(fill)
            .children(notch);
        div().flex().items_center().gap_1().child(cell).child(name)
    };
    let roots = format!("{} {}", by.word(), unit_names(unit).many);
    let reached = format!("{roots} {}", by.also_a_dependency());
    [entry(false, roots), entry(true, reached)]
}

/// How wide a cell beside a key's entry is drawn.
const KEY_CELL_WIDTH: Pixels = px(16.);

/// How wide a legend swatch is drawn.
const LEGEND_SWATCH: Pixels = px(8.);

/// The path from the whole build down to the focus of `drawn`, each step but
/// the focus a click away; absent at the whole build. Where the path outgrows
/// its room the dominators above the focus give way first, and the focus is
/// cut short only once they are gone. A name reading alike with another
/// crumb's is followed by what tells it apart.
fn breadcrumb(drawn: &Drawn, palette: &Palette, cx: &Context<ExclusivePanel>) -> Option<Div> {
    let focus = drawn.focus.as_ref()?;
    let (tree, shared) = (&drawn.coloured.tree, &drawn.coloured.shared);
    let by = drawn.coloured.by();
    // A row whose steps shrink to fit it.
    let row = || div().flex().items_center().min_w_0().overflow_hidden();
    // A name and the hint telling it apart give way in proportion to their
    // lengths, so neither is cut away before the other.
    let part = |text: String| {
        div()
            .min_w_0()
            .overflow_hidden()
            .text_ellipsis()
            .child(text)
    };
    let crumb = |name: String, hint: Option<String>| {
        let hint = hint.map(|hint| part(format!("{HINT_SEPARATOR}{hint}")));
        div()
            .flex()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .child(part(name))
            .children(hint)
    };
    let after = |step: AnyElement| row().gap_1().pl_1().child(separator()).child(step);
    let link = |index: usize, name: String, hint: Option<String>, to: Option<Focused>| {
        // A click landing once another tree is drawn names none of its nodes.
        let tree = Arc::clone(&drawn.coloured.tree);
        crumb(name, hint)
            .id(("exclusive-crumb", index))
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
    let above_focus = focus.above.iter().map(|&above| Focused::from(above));
    let steps = above_focus.chain([focus.on]);
    let crumbs: Vec<u32> = steps.filter_map(Focused::node).collect();
    let hints = hints_among(tree, shared, &crumbs, &[]);
    let hint = |step: Focused| step.node().and_then(|node| hints.get(&node).cloned());
    for (index, &step) in focus.above.iter().enumerate() {
        let step = Focused::from(step);
        let step = link(
            index + 1,
            step.name(tree, shared, by),
            hint(step),
            Some(step),
        );
        above.push(after(step.into_any_element()));
    }
    let focused = cell_name(tree, shared, by, focus.on.holds());
    let last = after(crumb(focused, hint(focus.on)).into_any_element())
        .flex_shrink_0()
        .max_w(relative(1.));
    Some(
        row()
            .flex_1()
            .min_w(CRUMBS_MIN_WIDTH)
            .text_sm()
            .text_color(palette.label)
            .child(link(0, WHOLE_BUILD_CRUMB.to_owned(), None, None).flex_shrink_0())
            .child(row().child(row().children(above)).child(last)),
    )
}

/// The least width the breadcrumb is laid out at: with less left beside the
/// strip's controls, it takes a row of the strip to itself.
const CRUMBS_MIN_WIDTH: Pixels = px(320.);

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
fn hover_layer(view: Entity<ExclusivePanel>, palette: Palette) -> impl IntoElement {
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
    parent: WeakEntity<ExclusivePanel>,
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
                by: drawn.coloured.by(),
                top: drawn.top(),
                transition: drawn.transition.clone(),
                above: drawn.focus.as_ref().map_or_else(Vec::new, |focus| {
                    let above = focus.above.iter().map(|&above| Focused::from(above));
                    above.filter_map(Focused::node).collect()
                }),
                shared: drawn.coloured.shared.clone(),
                palette,
            })
        });
        let parent = self.parent.clone();
        canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, (), window, cx| {
                let Some(scene) = scene else {
                    return;
                };
                let now = Instant::now();
                paint::paint_icicle(&scene, bounds, now, window, cx);
                match scene.transition {
                    Some(transition) if transition.ended(now) => {
                        window.on_next_frame(move |_, cx| {
                            let settled =
                                parent.update(cx, |panel, cx| panel.settle(&transition, cx));
                            settled.ok();
                        });
                    }
                    Some(_) => window.request_animation_frame(),
                    None => {}
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
    fn the_status_counts_the_points_outside_only_where_there_are_any() {
        let unit = Unit::ConfiguredTarget;
        assert_eq!(tally(unit, 9, 1, 0), "9 configured targets · 1 root");
        let outside = "1 configured target · 2 roots · 7 outside";
        assert_eq!(tally(unit, 1, 2, 7), outside);
    }

    #[test]
    fn the_other_copies_reached_are_counted_only_where_there_are_any() {
        assert_eq!(other_copies(0), None);
        let one = "  · 1 other copy is a dependency too";
        assert_eq!(other_copies(1).unwrap(), one);
        let several = "  · 2 other copies are dependencies too";
        assert_eq!(other_copies(2).unwrap(), several);
    }

    #[test]
    fn roots_read_requested_where_a_target_set_chose_them() {
        let (tops, targets) = (RootedBy::Tops, RootedBy::Targets);
        let many = "configured targets";
        assert_eq!(
            shared_by(2, tops.word(), many),
            "shared by 2 top-level configured targets"
        );
        assert_eq!(
            shared_by(2, targets.word(), many),
            "shared by 2 requested configured targets"
        );
        assert_eq!(
            reached_from(3, targets.word(), many),
            "requested, and reached from 3 other requested configured targets"
        );
        assert_eq!(targets.why(true), "named by the target set");
        assert_eq!(tops.why(false), "nothing depends on it");
    }

    #[test]
    fn the_targets_a_subtree_holds_read_by_where_else_they_are_configured() {
        let read = |targets, only_here| targets_held(targets, only_here);
        assert_eq!(read(0, 0), None);
        let read = |targets, only_here| read(targets, only_here).unwrap();
        assert_eq!(read(1, 0), "covering 1 target, also configured elsewhere");
        assert_eq!(read(1, 1), "covering 1 target, configured only here");
        let each_elsewhere = "covering 3 targets, each also configured elsewhere";
        assert_eq!(read(3, 0), each_elsewhere);
        assert_eq!(read(3, 3), "covering 3 targets, all configured only here");
        let some_here = "covering 3 targets, 1 of them configured only here";
        assert_eq!(read(3, 1), some_here);
    }

    #[test]
    fn the_tops_gathered_as_shared_are_counted_and_said_to_be_depended_on_by_several() {
        let names = unit_names(Unit::ConfiguredTarget);
        assert_eq!(
            gathered_as_shared(1, TOP_LEVEL, &names),
            "1 configured target that two or more top-level configured targets depend on"
        );
        assert_eq!(
            gathered_as_shared(7, REQUESTED, &names),
            "7 configured targets that two or more requested configured targets depend on"
        );
    }
}
