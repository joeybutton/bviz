//! What exists in a build only because of each of its actions, configured
//! targets or targets: the dominator tree of the graph's points of one unit,
//! drawn as an icicle.

mod ancestry;
mod cell_menu;
mod icicle;
mod paint;
mod roots;
mod selection;
mod transition;

pub(crate) use roots::NOTICE_SEPARATOR;

use crate::Palette;
use crate::chips::{self, ChipBar, Holder};
use crate::history::History;
use crate::hue::{Colouring, Hue};
use crate::model::{
    DominatorsLanded, FilterChanged, GraphModel, Shared, SourceReplaced, TreeKey, VerdictsLanded,
};
use crate::point::{
    KindNames, built_for_tool, configuration, identify, unit_names, written_target_label,
};
use crate::unit_picker::{CyclePoints, UnitPicker};
use ancestry::{Spot, Stands};
use bviz_graph_layout::Id;
use bviz_graph_layout::dominators::{Chosen, Dominators, Elsewhere, Roots, Standing, Targets};
use bviz_graph_layout::metrics::{ByUnit, Count, Scope, Unit};
use bviz_graph_layout::select::Verdicts;
use bviz_graph_layout::select::labels::carried_label;
use bviz_graph_layout::target_set::PatternError;
use bviz_ui_components::gesture::{
    Button, Clicks, Entry, GestureRow, Gestures, Input, Motion, Place, Pointer,
};
use bviz_ui_components::{
    CardAnchor, HoverCard, KeyScope, Legend, LegendEntry, LegendState, Menu, MenuEvent, MenuItem,
    OneRow, Priority, QueryField, QueryFieldEvent, ShowLegend, Swatch, ToolbarGroup, cached_layer,
    centered_notice, centered_spinner, defer_hover_card, legend_gestures, load_in_background,
    over_window,
};
use bviz_ui_workspace::{Panel, PanelEvent, PanelKind, Role, ViewKind};
use cell_menu::{Caveat, Offer, PointFacts};
use gpui::{
    Action, App, Bounds, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, NavigationDirection, Pixels, Point,
    Render, Rgba, SharedString, Subscription, Task, WeakEntity, Window, canvas, div, prelude::*,
    px,
};
use icicle::{Cell, Holds, Parent, Run};
use paint::{MIN_CELL_WIDTH, Scene, point_name, run_name, shared_name};
use roots::{Found, Within};
use selection::{LaidOut, Step};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;
use transition::Transition;

/// The key context scoping the panel's own bindings, so a key pressed here
/// reaches this panel and no other.
pub(crate) const KEY_CONTEXT: KeyScope = KeyScope::new("GraphExclusive", "Exclusive deps");

pub(crate) use roots::KEY_CONTEXT as ROOTS_KEY_CONTEXT;

/// What the panel does for a gesture in [`GESTURES`].
#[derive(Clone, Copy)]
enum ExclusiveIntent {
    ShowWholeBuild,
    FocusCell,
    FocusAncestor,
    ListHidden,
    OpenMenu,
    GoBack,
    GoForward,
    SelectCell,
}

const CELL: Place = Place::On("a cell");

const FOCUS_CELL: Entry<ExclusiveIntent> = Entry::new(
    Input::new(Motion::Press(Button::Left, Clicks::Single)),
    CELL,
    "Focus cell or go up one level",
    ExclusiveIntent::FocusCell,
);

const FOCUS_ANCESTOR: Entry<ExclusiveIntent> = Entry::new(
    Input::new(Motion::Press(Button::Left, Clicks::Single)),
    Place::On("an ancestor row"),
    "Focus ancestor",
    ExclusiveIntent::FocusAncestor,
);

const LIST_HIDDEN: Entry<ExclusiveIntent> = Entry::new(
    Input::new(Motion::Press(Button::Left, Clicks::Single)),
    Place::On("the hidden ancestors row"),
    "List hidden ancestors",
    ExclusiveIntent::ListHidden,
);

const OPEN_MENU: Entry<ExclusiveIntent> = Entry::new(
    Input::press(Button::Right),
    CELL,
    "Open cell menu",
    ExclusiveIntent::OpenMenu,
);

const GESTURES: Gestures<ExclusiveIntent> = Gestures::new(&[
    Entry::new(
        Input::new(Motion::Press(Button::Left, Clicks::Double)),
        Place::Empty,
        "Show whole build",
        ExclusiveIntent::ShowWholeBuild,
    ),
    FOCUS_CELL,
    FOCUS_ANCESTOR,
    LIST_HIDDEN,
    OPEN_MENU,
    Entry::new(
        Input::press(Button::Back),
        Place::Anywhere,
        "Go back",
        ExclusiveIntent::GoBack,
    ),
    Entry::new(
        Input::press(Button::Forward),
        Place::Anywhere,
        "Go forward",
        ExclusiveIntent::GoForward,
    ),
    Entry::new(
        Input::HOVER,
        CELL,
        "Select and describe cell",
        ExclusiveIntent::SelectCell,
    ),
]);

gpui::actions!(
    bviz_graph,
    [
        /// Show whole build
        ShowWholeBuild,
        /// Go up one level
        FocusUp,
        /// Leave roots field
        LeaveRoots,
        /// Focus roots field
        FocusRoots,
        /// Select previous cell
        SelectPrevious,
        /// Select next cell
        SelectNext,
        /// Select parent cell
        SelectParent,
        /// Select heaviest child cell
        SelectChild,
        /// Clear selection
        Deselect,
        /// Focus cell
        FocusSelected,
        /// Show cell in graph
        ShowSelected,
        /// Go back
        FocusBack,
        /// Go forward
        FocusForward,
    ]
);

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
    legend: Entity<LegendState>,
    /// Where the cells were last laid out, which a pointer is resolved
    /// against.
    frame: Option<Frame>,
    /// The verdicts of the document's filter the cells are faded by, from the
    /// time they first land.
    fade: Option<Arc<Verdicts>>,
    /// The roots chosen last, from the time the first are chosen.
    rooting: Option<Rooting>,
    /// Chooses the roots afresh, absent while none are being chosen; replaced
    /// by a later choice, which it would only hold up.
    choosing: Option<Choosing>,
    /// The field naming the roots as a target set.
    roots_field: Entity<QueryField>,
    /// The document's query bar, as this panel stands it.
    chips: Entity<ChipBar>,
    /// The target set that last failed, which is not resolved again while
    /// the field holds it.
    failed: Option<Failed>,
    /// The trees of the graph read coloured so far that the graph still
    /// keeps.
    coloured: HashMap<TreeKey, Arc<Coloured>>,
    /// Colours a tree landed, absent while none is being coloured.
    drawing: Option<Task<()>>,
    /// The menu a right press put up over a cell, while it stands.
    menu: Option<CellMenu>,
    /// The card describing what is selected.
    card: HoverCard<Stands>,
    /// Redraws as the graph is read.
    _changed: Subscription,
    _source_replaced: Subscription,
    _dominators_landed: Subscription,
    _filter_changed: Subscription,
    _verdicts_landed: Subscription,
    _roots_typed: Subscription,
}

impl ExclusivePanel {
    /// The selection of a tree at rest, which the hover layer rings.
    fn at_rest_selection(&self) -> Option<&Selection> {
        let drawn = self.drawn.as_ref()?;
        let at_rest = drawn.transition.is_none();
        drawn.selection.as_ref().filter(|_| at_rest)
    }

    /// What the card describing what is selected reads and stands beside,
    /// while it is shown.
    fn card(&self) -> Option<(Vec<String>, CardAnchor)> {
        let Selection { spot, by, lines } = self.at_rest_selection()?;
        let anchor = match (by, self.card.pointer()) {
            // The menu stands in for the description.
            (Picked::Menu, _) => return None,
            _ if !self.card.shows(&spot.stands()) => return None,
            (Picked::Pointer, Some(at)) => CardAnchor::Pointer(at),
            (Picked::Pointer, None) | (Picked::Keys, _) => CardAnchor::Item(spot.rect()),
        };
        Some((lines.clone(), anchor))
    }

    pub(crate) fn new(model: Entity<GraphModel>, cx: &mut Context<Self>) -> Self {
        let changed = cx.observe(&model, |_, _, cx| cx.notify());
        let source_replaced = cx.subscribe(&model, |view, _, _: &SourceReplaced, cx| {
            view.drawn = None;
            view.restate_shown(cx);
            view.fade = None;
            view.rooting = None;
            view.choosing = None;
            view.failed = None;
            view.coloured.clear();
            view.drawing = None;
            view.picker = unit_picker(&view.model, &view.focus_handle, cx);
            view.compute(cx);
        });
        let dominators_landed = cx.subscribe(&model, |view, _, landed: &DominatorsLanded, cx| {
            if view.key(cx).as_ref() == Some(&landed.key) {
                view.redraw(cx);
            } else if view.target_key().as_ref() == Some(&landed.key) {
                view.reselect(cx);
            }
        });
        let filter_changed = cx.subscribe(&model, |view, _, _: &FilterChanged, cx| view.refade(cx));
        let verdicts_landed =
            cx.subscribe(&model, |view, _, _: &VerdictsLanded, cx| view.refade(cx));
        let roots_field = cx.new(|cx| {
            QueryField::new(
                "exclusive-roots",
                roots::PLACEHOLDER,
                roots::FIELD_WIDTH,
                cx,
            )
            .narrowing_to(roots::FIELD_LEAST)
        });
        let roots_typed = cx.subscribe(&roots_field, |view, _, event, cx| match event {
            QueryFieldEvent::Edited | QueryFieldEvent::CursorMoved => {}
            QueryFieldEvent::Settled => view.reroot(cx),
        });
        let parent = cx.entity().downgrade();
        let focus_handle = cx.focus_handle();
        let legend = cx.new(|cx| LegendState::new(focus_handle.clone(), cx));
        let picker = unit_picker(&model, &focus_handle, cx);
        let chips = cx.new(|cx| {
            ChipBar::new(
                model.clone(),
                Holder::Exclusive,
                focus_handle.clone(),
                None,
                cx,
            )
        });
        Self {
            focus_handle,
            legend,
            picker,
            model,
            shown: false,
            drawn: None,
            canvas: cx.new(|_| IcicleCanvas { parent }),
            frame: None,
            fade: None,
            rooting: None,
            choosing: None,
            roots_field,
            chips,
            failed: None,
            coloured: HashMap::new(),
            drawing: None,
            menu: None,
            card: HoverCard::new(cx, |view| &mut view.card, Self::carded),
            _changed: changed,
            _source_replaced: source_replaced,
            _dominators_landed: dominators_landed,
            _filter_changed: filter_changed,
            _verdicts_landed: verdicts_landed,
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

    /// Fades the cells by the verdicts of the document's filter as it stands,
    /// where the panel is shown, asking for them where they have yet to land.
    fn refade(&mut self, cx: &mut Context<Self>) {
        if !self.shown {
            return;
        }
        let Some(verdicts) = self.model.update(cx, |model, cx| model.verdicts(cx)) else {
            return;
        };
        if self
            .fade
            .as_ref()
            .is_some_and(|fade| Arc::ptr_eq(fade, &verdicts))
        {
            return;
        }
        self.fade = Some(verdicts);
        self.restate_shown(cx);
        self.invalidate_canvas(cx);
    }

    /// Tells the query bar how many points of the tree drawn the document's
    /// filter leaves unfaded, where the filter narrows the graph.
    fn restate_shown(&self, cx: &mut Context<Self>) {
        let narrows = self.model.read(cx).filter().narrows();
        let shown = self.drawn.as_ref().filter(|_| narrows).map(|drawn| {
            let tree = &drawn.coloured.tree;
            let shown = match &self.fade {
                Some(fade) => (0..tree.total())
                    .filter(|&node| !paint::fades(fade.get(tree.id(node))))
                    .count(),
                None => tree.total() as usize,
            };
            chips::Shown {
                shown,
                of: tree.total() as usize,
                many: unit_names(tree.scope().unit()).many,
            }
        });
        self.chips.update(cx, |bar, cx| bar.set_shown(shown, cx));
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
                self.reselect(cx);
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
                history: History::new(None),
                transition: None,
                selection: None,
                pressed_clear: false,
            });
            if self.shown {
                self.want(key, cx);
            }
            self.restate_shown(cx);
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

    /// The rows over the focus and the cells drawn from it at rest, where
    /// they are laid out.
    fn laid_out(&self) -> Option<LaidOut> {
        let (drawn, frame) = (self.drawn.as_ref()?, self.frame?);
        let tree = &*drawn.coloured.tree;
        let rows = ancestry::rows(tree, drawn.top());
        let (above, under) = ancestry::lay_out(&rows, frame.bounds, frame.row);
        let cells = icicle::lay_out(tree, drawn.top(), under, frame.row, MIN_CELL_WIDTH);
        Some(LaidOut { above, cells })
    }

    /// What a pointer at `at` is over, where the icicle is drawn and at rest.
    fn spot_at(&self, at: Point<Pixels>) -> Option<Spot> {
        if self.drawn.as_ref()?.transition.is_some() {
            return None;
        }
        let laid = self.laid_out()?;
        laid.spots().find(|spot| spot.rect().contains(&at))
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
    /// none, as the step after the focus drawn.
    fn refocus(&mut self, focus: Option<Focus>, cx: &mut Context<Self>) {
        self.travel(|history| history.push(focus), cx);
    }

    /// Draws the icicle from the focus it was drawn from before the one
    /// drawn, where there was one.
    fn go_back(&mut self, cx: &mut Context<Self>) {
        self.travel(|history| _ = history.back(), cx);
    }

    /// Draws the icicle from the focus gone back from last, where there is
    /// one.
    fn go_forward(&mut self, cx: &mut Context<Self>) {
        self.travel(|history| _ = history.forward(), cx);
    }

    /// Takes the foci drawn a `step`, moving the icicle from the focus drawn
    /// to the one that makes current, and selects afresh. A move under way
    /// ends at once on the focus it was making for, and the next starts from
    /// there, so a key held or pressed again mid-move goes a step further
    /// each time.
    fn travel(&mut self, step: impl FnOnce(&mut History<Option<Focus>>), cx: &mut Context<Self>) {
        let Some(drawn) = &mut self.drawn else {
            return;
        };
        let from = drawn.top();
        step(&mut drawn.history);
        if drawn.top() == from {
            return;
        }
        let tree = &*drawn.coloured.tree;
        drawn.transition = Transition::between(tree, from, drawn.top(), Instant::now());
        self.invalidate_canvas(cx);
        self.reselect(cx);
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
            self.reselect(cx);
        }
    }

    /// Selects and describes afresh: where the keys selected, what they
    /// selected wherever it is still drawn and the cell on top where it is
    /// not, and otherwise what is under the pointer.
    fn reselect(&mut self, cx: &mut Context<Self>) {
        let Some(drawn) = &mut self.drawn else {
            return;
        };
        let (selected, pointer) = (drawn.selection.take(), self.card.pointer());
        match selected {
            Some(Selection {
                spot,
                by: Picked::Keys,
                ..
            }) => {
                let laid = self.laid_out();
                let kept = laid.and_then(|laid| laid.find(&spot).or(laid.top()));
                match kept {
                    Some(kept) => self.select(kept, Picked::Keys, cx),
                    None => cx.notify(),
                }
            }
            _ => match pointer {
                Some(at) => self.hover_at(at, cx),
                None => cx.notify(),
            },
        }
    }

    /// Selects `spot` as `by` picked it, describing it unless it is what is
    /// selected already.
    fn select(&mut self, spot: Spot, by: Picked, cx: &mut Context<Self>) {
        let as_targets = self
            .target_key()
            .and_then(|key| self.model.read(cx).dominators(&key).cloned());
        let Some(drawn) = &mut self.drawn else {
            return;
        };
        let focus = drawn.focused();
        match &mut drawn.selection {
            Some(selection) if selection.spot == spot => selection.by = by,
            selection => {
                let coloured = &drawn.coloured;
                let lines = match spot {
                    Spot::Cell(cell) => {
                        describe(coloured, as_targets.as_deref(), cell.holds, focus)
                    }
                    Spot::Hidden { count, .. } => describe_hidden(coloured, count, focus),
                };
                *selection = Some(Selection { spot, by, lines });
            }
        }
        cx.notify();
    }

    /// Selects nothing.
    fn deselect(&mut self, cx: &mut Context<Self>) {
        if let Some(drawn) = &mut self.drawn
            && drawn.selection.take().is_some()
        {
            cx.notify();
        }
    }

    /// Takes the selection a `step` through what is drawn, or selects the
    /// cell on top where nothing is selected.
    fn step(&mut self, step: Step, cx: &mut Context<Self>) {
        let (Some(drawn), Some(laid)) = (&self.drawn, self.laid_out()) else {
            return;
        };
        let selected = drawn.selection.as_ref();
        let selected = selected.and_then(|selection| laid.find(&selection.spot));
        let to = match selected {
            Some(from) => laid.stepped(&from, step, &*drawn.coloured.tree),
            None => laid.top(),
        };
        if let Some(to) = to {
            self.card.pick();
            self.select(to, Picked::Keys, cx);
        }
    }

    /// Does what a click on what is selected does.
    fn focus_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self
            .drawn
            .as_ref()
            .and_then(|drawn| drawn.selection.as_ref());
        match selected.map(|selection| selection.spot) {
            Some(Spot::Cell(cell)) => self.click_cell(cell.holds, cx),
            Some(spot @ Spot::Hidden { .. }) => {
                let at = spot.rect().bottom_left();
                self.list_hidden(spot, at, window, cx);
            }
            None => {}
        }
    }

    /// Shows the graph's drawing of the point selected, where a point is.
    fn show_selected(&mut self, cx: &mut Context<Self>) {
        let Some(drawn) = &self.drawn else {
            return;
        };
        if let Some(Selection {
            spot:
                Spot::Cell(Cell {
                    holds: Holds::Node(node),
                    ..
                }),
            ..
        }) = drawn.selection
        {
            let id = drawn.coloured.tree.id(node);
            self.model.update(cx, |model, cx| model.reveal(id, cx));
        }
    }

    /// Whether `tree` is the tree drawn, which a gesture begun over another
    /// names none of the nodes of.
    fn draws(&self, tree: &Arc<Dominators>) -> bool {
        (self.drawn.as_ref()).is_some_and(|drawn| Arc::ptr_eq(&drawn.coloured.tree, tree))
    }

    /// Draws the icicle from what the focus stands under.
    fn focus_up(&mut self, cx: &mut Context<Self>) {
        let Some(drawn) = &self.drawn else {
            return;
        };
        if let Some(on) = drawn.focused() {
            let up = on.holds().parent(&*drawn.coloured.tree);
            self.focus_on(up.and_then(Focused::of), cx);
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

    /// Acts on a press as [`GESTURES`] has it. A double click returns to the
    /// whole build only where its first press also landed clear of every cell;
    /// a later press of a click begun on a cell does nothing, the cell under it
    /// being whatever the first press moved there.
    fn on_mouse_down(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let spot = self.spot_at(e.position);
        let above = spot.is_some_and(|spot| {
            let laid = self.laid_out();
            laid.is_some_and(|laid| laid.is_above(&spot))
        });
        let Some(drawn) = &mut self.drawn else {
            return;
        };
        let pressed_clear = std::mem::take(&mut drawn.pressed_clear);
        // What is under a press while the icicle moves is not what the reader
        // aimed at.
        if drawn.transition.is_some() {
            return;
        }
        drawn.pressed_clear = spot.is_none() && e.click_count == 1;
        GESTURES.dispatch(Pointer::press(e), |intent| match (intent, spot) {
            (ExclusiveIntent::ShowWholeBuild, None) if pressed_clear => {
                self.focus_on(None, cx);
                true
            }
            (ExclusiveIntent::FocusCell, Some(Spot::Cell(cell))) if !above => {
                self.click_cell(cell.holds, cx);
                true
            }
            (ExclusiveIntent::FocusAncestor, Some(Spot::Cell(cell))) if above => {
                self.click_cell(cell.holds, cx);
                true
            }
            (ExclusiveIntent::ListHidden, Some(spot @ Spot::Hidden { .. })) => {
                self.list_hidden(spot, e.position, window, cx);
                true
            }
            _ => false,
        });
    }

    /// Selects what is under a right press and puts up the menu of what can
    /// be done with it, opening where the press landed.
    fn open_menu(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match self.spot_at(e.position) {
            Some(spot @ Spot::Cell(cell)) => {
                self.select(spot, Picked::Menu, cx);
                let items = self.menu_items(cell.holds, window, cx);
                self.put_up(items, e.position, window, cx);
            }
            Some(spot @ Spot::Hidden { .. }) => self.list_hidden(spot, e.position, window, cx),
            None => {}
        }
    }

    /// Selects the row of hidden ancestors `spot` and puts up the menu
    /// listing them at `at`, choosing one focusing it.
    fn list_hidden(
        &mut self,
        spot: Spot,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drawn) = &self.drawn else {
            return;
        };
        let (tree, shared, by) = (
            &drawn.coloured.tree,
            &drawn.coloured.shared,
            drawn.coloured.by(),
        );
        let panel = cx.weak_entity();
        let hidden = ancestry::hidden(&**tree, drawn.top());
        let nodes: Vec<u32> = hidden.iter().filter_map(|holds| holds.node()).collect();
        let hints = paint::hints_among(tree, shared, &nodes, &[]);
        let items: Vec<MenuItem> = hidden
            .into_iter()
            .filter_map(|holds| {
                let name = Focused::of(holds)?.name(tree, shared, by);
                let hint = holds.node().and_then(|node| hints.get(&node));
                let name = match hint {
                    Some(hint) => format!("{name}{}{hint}", paint::PART_SEPARATOR),
                    None => name,
                };
                let (panel, tree) = (panel.clone(), Arc::clone(tree));
                Some(MenuItem::new(name, move |_: &mut Window, cx: &mut App| {
                    let done =
                        panel.update(cx, |view, cx| view.take(&Deed::Click(holds), &tree, cx));
                    done.ok();
                }))
            })
            .collect();
        self.select(spot, Picked::Menu, cx);
        self.put_up(items, at, window, cx);
    }

    /// Puts up the menu of `items` at `at` over what is selected, which stays
    /// selected until it closes; nothing where there are no items.
    fn put_up(
        &mut self,
        items: Vec<MenuItem>,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if items.is_empty() {
            return;
        }
        let menu = cx.new(|cx| Menu::new(items, at, window, cx));
        let dismissed = cx.subscribe_in(&menu, window, |view, _, event: &MenuEvent, window, cx| {
            let MenuEvent::Dismissed { holding_focus } = event;
            view.menu = None;
            if *holding_focus {
                view.focus_handle.focus(window, cx);
            }
            view.reselect(cx);
        });
        self.menu = Some(CellMenu {
            menu,
            _dismissed: dismissed,
        });
        cx.notify();
    }

    /// What the menu of the cell holding `holds` offers.
    fn offers(&self, holds: Holds, cx: &App) -> Vec<Offer> {
        let Some(drawn) = &self.drawn else {
            return Vec::new();
        };
        let (tree, shared) = (&drawn.coloured.tree, &drawn.coloured.shared);
        let targets = drawn.coloured.targets.as_ref();
        let facts = |node: u32| {
            let id = tree.id(node);
            PointFacts {
                labelled: carried_label(&shared.names, &shared.facets, id).is_some(),
                copies: targets.map_or(1, |targets| targets.copies(node)),
                for_tool: built_for_tool(id, shared),
            }
        };
        let reveals = self.model.read(cx).reveals();
        cell_menu::offered(holds, drawn.top(), tree.scope().unit(), reveals, facts)
    }

    /// The rows of the menu of the cell holding `holds`.
    fn menu_items(&self, holds: Holds, window: &Window, cx: &Context<Self>) -> Vec<MenuItem> {
        let Some(drawn) = &self.drawn else {
            return Vec::new();
        };
        let (tree, shared) = (&drawn.coloured.tree, &drawn.coloured.shared);
        let offers = self.offers(holds, cx);
        let written = |node: u32| written_target_label(tree.id(node), shared);
        let panel = cx.weak_entity();
        let deed = |deed: Deed| {
            let (panel, tree) = (panel.clone(), Arc::clone(tree));
            move |_: &mut Window, cx: &mut App| {
                let done = panel.update(cx, |view, cx| view.take(&deed, &tree, cx));
                done.ok();
            }
        };
        // A row an action also does reads that action's label, the action
        // working on the selected cell, which the cell under the menu becomes.
        let keys_in = Some(&self.focus_handle);
        let action =
            |action: &dyn Action, chosen| MenuItem::action(action, keys_in, window, cx, chosen);
        let items = offers.into_iter().filter_map(|offer| match offer {
            Offer::Focus => Some(action(&FocusSelected, deed(Deed::Click(holds)))),
            Offer::Up => Some(action(&FocusUp, deed(Deed::Click(holds)))),
            Offer::Show { node, can } => {
                let item = action(&ShowSelected, deed(Deed::Reveal(node)));
                Some(if can { item } else { item.disabled() })
            }
            Offer::Root { node, caveat } => {
                let label = written(node)?;
                let explained = caveat.map(Caveat::explain);
                let item = MenuItem::new("Use as root", deed(Deed::Root(label.into())));
                Some(match explained {
                    Some(explained) => item.tooltip(explained),
                    None => item,
                })
            }
            Offer::CopyLabel { node } => {
                let label = written(node)?;
                Some(MenuItem::new("Copy label", move |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(label.clone()));
                }))
            }
        });
        items.collect()
    }

    /// Carries out `deed`, chosen from the menu of a cell of `tree`, where
    /// that is still the tree drawn.
    fn take(&mut self, deed: &Deed, tree: &Arc<Dominators>, cx: &mut Context<Self>) {
        if !self.draws(tree) {
            return;
        }
        match deed {
            Deed::Click(holds) => self.click_cell(*holds, cx),
            &Deed::Reveal(node) => {
                let id = tree.id(node);
                self.model.update(cx, |model, cx| model.reveal(id, cx));
            }
            Deed::Root(label) => {
                let label = label.clone();
                self.roots_field
                    .update(cx, |field, cx| field.set_text(label, cx));
                self.reroot(cx);
            }
        }
    }

    /// Selects the cell the pointer at `at` is over; a pointer over none lets
    /// go of a cell it selected.
    fn hover_at(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let spot = self.spot_at(at);
        self.card
            .hover(at, spot.as_ref().map(Spot::stands).as_ref());
        let Some(drawn) = &mut self.drawn else {
            return;
        };
        match spot {
            Some(spot) => self.select(spot, Picked::Pointer, cx),
            None if drawn.picked_by_pointer() => self.deselect(cx),
            None => {}
        }
    }

    /// Lets go of the cell the pointer selected, which has left.
    fn clear_hover(&mut self, cx: &mut Context<Self>) {
        if self.card.leave() {
            cx.notify();
        }
        if self.drawn.as_ref().is_some_and(Drawn::picked_by_pointer) {
            self.deselect(cx);
        }
    }

    /// What the card describes: what is selected, unless its menu stands in
    /// for the card.
    fn carded(&self, _: &App) -> Option<Stands> {
        let selection = self.drawn.as_ref()?.selection.as_ref()?;
        (selection.by != Picked::Menu).then_some(selection.spot.stands())
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
    focus: &FocusHandle,
    cx: &mut Context<ExclusivePanel>,
) -> Option<UnitPicker> {
    let inputs = model.read(cx).loaded()?.shared.inputs;
    let opening = Some(Unit::ConfiguredTarget);
    UnitPicker::new(
        "exclusive-unit",
        "Cell unit",
        inputs,
        opening,
        focus,
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
    /// The foci the icicle has been drawn from, the current one drawn; none
    /// for the whole build.
    history: History<Option<Focus>>,
    /// The move to the focus drawn from the one drawn before, while it is
    /// under way.
    transition: Option<Transition>,
    selection: Option<Selection>,
    /// Whether the last press landed clear of every cell, starting a click.
    pressed_clear: bool,
}

impl Drawn {
    fn picked_by_pointer(&self) -> bool {
        matches!(
            self.selection,
            Some(Selection {
                by: Picked::Pointer,
                ..
            })
        )
    }

    fn focus(&self) -> Option<&Focus> {
        self.history.current().as_ref()
    }

    fn focused(&self) -> Option<Focused> {
        self.focus().map(|focus| focus.on)
    }

    /// What the cell on top holds.
    fn top(&self) -> Holds {
        self.focused().map_or(Holds::Build, Focused::holds)
    }

    /// Whether a cell the icicle draws from its top can stand for a root
    /// the build also reaches as a dependency.
    fn draws_reached(&self) -> bool {
        match self.focus() {
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

    /// The points of `tree` this stands over.
    fn weight(self, tree: &Dominators) -> u32 {
        self.holds().weight(tree)
    }

    /// How a tooltip names this in `tree`, whose roots
    /// were chosen `by`.
    fn name(self, tree: &Dominators, shared: &Shared, by: RootedBy) -> String {
        match self {
            Self::Shared => shared_name(tree, by),
            Self::Node(node) => point_name(tree.id(node), tree.scope().unit(), shared),
            Self::Run(run) => run_name(tree, shared, by, run),
        }
    }
}

/// Where the icicle is drawn from, with what is read of the tree for it
/// taken once as the focus is set.
#[derive(Clone, PartialEq)]
struct Focus {
    on: Focused,
    /// Whether a cell drawn from `on` can stand for a root the build also
    /// reaches as a dependency: `on` itself, or where it is a run of roots,
    /// one it gathers.
    reached: bool,
}

impl Focus {
    fn on(tree: &Dominators, on: Focused) -> Self {
        let reached = match on {
            Focused::Node(node) => paint::reached(tree, node),
            Focused::Run(run) if run.parent() == Parent::Build => {
                run.siblings().any(|root| paint::reached(tree, root))
            }
            Focused::Shared | Focused::Run(_) => false,
        };
        Self { on, reached }
    }
}

/// What is ringed and described in the tooltip, described once as it is
/// selected, so a pointer moving within it only moves the tooltip.
struct Selection {
    spot: Spot,
    by: Picked,
    lines: Vec<String>,
}

/// What selected a cell.
#[derive(Clone, Copy, PartialEq)]
enum Picked {
    /// The pointer, where it rests.
    Pointer,
    Keys,
    /// A right press, whose menu stands over the cell until it closes, the
    /// pointer leaving the cell for the menu letting go of nothing.
    Menu,
}

/// A menu put up over a cell, let go of once it closes.
struct CellMenu {
    menu: Entity<Menu>,
    _dismissed: Subscription,
}

/// What a row of a cell's menu does to the panel.
enum Deed {
    /// What a click on the cell holding this does.
    Click(Holds),
    /// Shows the graph's drawing of the node.
    Reveal(u32),
    /// Roots the tree at the target set naming this label.
    Root(SharedString),
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
    let exclusive = |weight: u32| exclusive(weight, &names);
    let shares = |weight: u32, within: Option<Focused>| {
        let of_build = format!("{} of build", share(weight, tree.total()));
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
    // A share of the focus is told only of what stands under it.
    let under = focus.filter(|focus| ancestry::ancestors(tree, holds).contains(&focus.holds()));
    let mut lines = Vec::new();
    match holds {
        Holds::Build => lines.extend([WHOLE_BUILD.to_owned(), names.counted(tree.total())]),
        Holds::Shared => {
            let (count, weight) = (tree.shared_tops().len() as u32, tree.shared_weight());
            lines.push(shared_name(tree, by));
            lines.push(gathered_as_shared(count, by.word(), &names));
            lines.push(exclusive(weight));
            lines.push(shares(weight, under));
        }
        Holds::Node(node) => {
            identify(tree.id(node), tree.kind(node), unit, shared, &mut lines);
            let weight = tree.weight(node);
            lines.push(exclusive(weight));
            if let Some(targets) = &coloured.targets {
                let held = targets.held(node);
                lines.extend(targets_held(held.targets(), held.only_here()));
            }
            lines.push(shares(weight, under));
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
                false => format!("{} · too narrow", names.counted(count)),
            });
            lines.push(exclusive(weight));
            lines.push(shares(weight, under));
        }
    }
    lines
}

/// What the tooltip over the row gathering the `count` ancestors of `focus`
/// left out in the tree of `coloured` reads.
fn describe_hidden(coloured: &Coloured, count: u32, focus: Option<Focused>) -> Vec<String> {
    let (tree, shared, by) = (&*coloured.tree, &coloured.shared, coloured.by());
    let ancestors = if count == 1 { "ancestor" } else { "ancestors" };
    let hidden = format!("{count} hidden {ancestors}");
    vec![match focus {
        Some(focus) => format!("{hidden} of {}", focus.name(tree, shared, by)),
        None => hidden,
    }]
}

/// What a tooltip says of the `targets` distinct targets a configured
/// target's subtree holds, `only_here` of which are configured nowhere else;
/// absent where it holds none.
fn targets_held(targets: u32, only_here: u32) -> Option<String> {
    let elsewhere = match (targets, only_here) {
        (0, _) => return None,
        (1, 0) => "configured elsewhere too".to_owned(),
        (1, _) => "configured only here".to_owned(),
        (_, 0) => "each configured elsewhere too".to_owned(),
        _ if only_here == targets => "all configured only here".to_owned(),
        _ => format!("{only_here} configured only here"),
    };
    let covered = unit_names(Unit::Target).counted(targets);
    Some(format!("{covered} · {elsewhere}"))
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
    let held = exclusive(targets.weight(node), &names);
    Some(format!("as target: {place} · {held}"))
}

/// How a tooltip counts the `weight` points, which `names` names, that exist
/// only because of what it describes.
fn exclusive(weight: u32, names: &KindNames) -> String {
    format!("{} exclusive", names.counted(weight))
}

/// How a tooltip names `parent` of `tree` as the immediate dominator of the
/// point it describes.
fn dominated_by(tree: &Dominators, parent: u32, shared: &Shared) -> String {
    let dominator = point_name(tree.id(parent), tree.scope().unit(), shared);
    format!("dominator {dominator}")
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
                None => lines.push(format!("{word} · {}", by.why(in_cycle))),
                Some(elsewhere) => {
                    lines.push(format!("{word} here · dependency elsewhere:"));
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
        lines.push(format!("{LISTED}+{more} more"));
    }
    if let Some(elsewhere) = elsewhere {
        lines.push("dependency elsewhere:".to_owned());
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
        Some(configuration) => format!("{configuration} copy"),
        None => "another copy".to_owned(),
    };
    let reached = format!("{LISTED}{dependent} → {copy}");
    let more = other_copies(elsewhere.copies.saturating_sub(1));
    std::iter::once(reached).chain(more).collect()
}

/// What a tooltip says of the `more` copies of a root's target, beside the
/// one it names, that the build reaches through a dependency.
fn other_copies(more: u32) -> Option<String> {
    match more {
        0 => None,
        1 => Some(format!("{LISTED}+1 copy depended on")),
        _ => Some(format!("{LISTED}+{more} copies depended on")),
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
            (RootedBy::Tops, false) => "no dependents",
            (RootedBy::Tops, true) => "no dependents outside its cycle",
            (RootedBy::Targets, _) => "in the target set",
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
    format!("{shared} · shared by 2+ {word} {sharing}")
}

/// How a reader says that `how_many` roots besides a root, called `word`
/// `named`, reach it.
fn reached_from(how_many: impl std::fmt::Display, word: &str, named: &str) -> String {
    format!("{word} · reached from {how_many} other {word} {named}")
}

/// The most roots a tooltip names as reaching a top.
const NAMED_ROOTS: usize = 3;

/// How a cell names the whole build.
const WHOLE_BUILD: &str = "the whole build";

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
            key_context: KEY_CONTEXT,
            role: Role::Canvas,
        }
    }

    fn description() -> Option<&'static str> {
        Some("Build's dominator tree")
    }

    fn gestures() -> Vec<GestureRow> {
        (GESTURES.rows())
            .chain(chips::gestures())
            .chain(legend_gestures())
            .collect()
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
        match active {
            true => {
                self.focus_handle.focus(window, cx);
                self.compute(cx);
            }
            false => self.clear_hover(cx),
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
            .on_action(cx.listener(|view, _: &SelectPrevious, _, cx| {
                view.step(Step::Previous, cx);
            }))
            .on_action(cx.listener(|view, _: &SelectNext, _, cx| view.step(Step::Next, cx)))
            .on_action(cx.listener(|view, _: &SelectParent, _, cx| view.step(Step::Parent, cx)))
            .on_action(cx.listener(|view, _: &SelectChild, _, cx| view.step(Step::Child, cx)))
            .on_action(cx.listener(|view, _: &Deselect, _, cx| view.deselect(cx)))
            .on_action(cx.listener(|view, _: &FocusSelected, window, cx| {
                view.focus_selected(window, cx);
            }))
            .on_action(cx.listener(|view, _: &ShowSelected, _, cx| view.show_selected(cx)))
            .on_action(cx.listener(|view, _: &FocusBack, _, cx| view.go_back(cx)))
            .on_action(cx.listener(|view, _: &FocusForward, _, cx| view.go_forward(cx)))
            .on_action(cx.listener(|view, _: &ShowLegend, window, cx| {
                view.legend.update(cx, |legend, cx| legend.show(window, cx));
            }))
            .on_action(cx.listener(|view, _: &CyclePoints, _, cx| {
                if let Some(picker) = &view.picker {
                    picker.cycle(cx);
                }
            }))
            .on_action(cx.listener(|view, _: &chips::Focus, window, cx| {
                view.chips
                    .update(cx, |bar, cx| chips::focus(bar, window, cx));
            }))
            .on_action(cx.listener(|view, _: &chips::Apply, _, cx| {
                view.chips.update(cx, chips::apply_staged);
            }))
            .on_action(cx.listener(|view, _: &chips::CopyStack, _, cx| {
                view.chips.update(cx, chips::copy_stack);
            }))
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Back),
                cx.listener(|view, e: &MouseDownEvent, _, cx| {
                    GESTURES.dispatch(Pointer::press(e), |intent| match intent {
                        ExclusiveIntent::GoBack => {
                            view.go_back(cx);
                            true
                        }
                        _ => false,
                    });
                }),
            )
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Forward),
                cx.listener(|view, e: &MouseDownEvent, _, cx| {
                    GESTURES.dispatch(Pointer::press(e), |intent| match intent {
                        ExclusiveIntent::GoForward => {
                            view.go_forward(cx);
                            true
                        }
                        _ => false,
                    });
                }),
            )
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
        let root = root.when(self.picker.is_some(), |root| {
            root.on_action(cx.listener(|view, _: &FocusRoots, window, cx| {
                view.roots_field.read(cx).focus_handle(cx).focus(window, cx);
            }))
        });
        let root = root.children(self.picker.as_ref().map(|picker| {
            let computing = self.choosing.is_some()
                || drawn.is_some_and(|drawn| Some(&drawn.coloured.key) != key.as_ref());
            let travels = drawn.map_or_else(Travels::default, |drawn| Travels {
                back: drawn.history.has_back(),
                up: drawn.focused().is_some(),
            });
            let scoping = Scoping {
                roots: &self.roots_field,
                computing,
                travels,
            };
            let legend = legend(&self.legend, drawn, &palette);
            strip(
                picker,
                scoping,
                legend,
                &self.focus_handle,
                &palette,
                window,
                cx,
            )
        }));
        let chips = &self.chips;
        let root = root.child(OneRow::new("exclusive-query-bar").filling(|| chips.clone()));
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
        root.child(
            div()
                .id("exclusive-icicle")
                .relative()
                .flex_1()
                .min_h(px(0.))
                .child(cached_layer(self.canvas.clone()))
                .child(hover_layer(cx.entity(), palette))
                .when(drawn.picked_by_pointer(), |icicle| icicle.cursor_pointer())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|view, e: &MouseDownEvent, window, cx| {
                        view.on_mouse_down(e, window, cx);
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|view, e: &MouseDownEvent, window, cx| {
                        GESTURES.dispatch(Pointer::press(e), |intent| match intent {
                            ExclusiveIntent::OpenMenu => {
                                view.open_menu(e, window, cx);
                                true
                            }
                            _ => false,
                        });
                    }),
                )
                .on_mouse_move(cx.listener(|view, e: &MouseMoveEvent, _, cx| {
                    GESTURES.dispatch(Pointer::HOVER, |intent| match intent {
                        ExclusiveIntent::SelectCell => {
                            view.hover_at(e.position, cx);
                            true
                        }
                        _ => false,
                    });
                }))
                .on_hover(cx.listener(|view, hovered: &bool, _, cx| {
                    if !hovered {
                        view.clear_hover(cx);
                    }
                })),
        )
        .children(
            self.menu
                .as_ref()
                .map(|open| over_window(&open.menu, window)),
        )
    }
}

/// How long the points picker holds out against the panel narrowing.
const POINTS: Priority = 2;

/// What the strip holds past its picker.
struct Scoping<'a> {
    /// The field choosing the roots of the tree.
    roots: &'a Entity<QueryField>,
    /// Whether the roots of a new choice are chosen and its tree computed over
    /// the one drawn for the last.
    computing: bool,
    travels: Travels,
}

/// Where the focus of the tree drawn can be taken from where it stands.
#[derive(Clone, Copy, Default)]
struct Travels {
    /// Back to the focus drawn before it.
    back: bool,
    /// Up to what it stands under.
    up: bool,
}

/// How long the back and up buttons hold out against the panel narrowing.
const TRAVEL: Priority = 1;

/// The buttons taking the focus back or up as `travels` allows, their keys
/// named where `focus` stands, leaving for the overflow menu as a row each.
fn travel(
    travels: Travels,
    focus: &FocusHandle,
    window: &Window,
    cx: &Context<ExclusivePanel>,
) -> ToolbarGroup {
    let back = bviz_ui_components::Button::new(
        "exclusive-back",
        cx.listener(|view, _, _, cx| view.go_back(cx)),
    )
    .ghost()
    .icon(BACK_ICON)
    .keys_for(&FocusBack, focus)
    .disabled(!travels.back);
    let up = bviz_ui_components::Button::new(
        "exclusive-up",
        cx.listener(|view, _, _, cx| view.focus_up(cx)),
    )
    .ghost()
    .icon(UP_ICON)
    .keys_for(&FocusUp, focus)
    .disabled(!travels.up);
    let row = |action: &dyn Action,
               can: bool,
               does: fn(&mut ExclusivePanel, &mut Context<ExclusivePanel>)| {
        let panel = cx.weak_entity();
        let item = MenuItem::action(action, Some(focus), window, cx, move |_, cx: &mut App| {
            panel.update(cx, does).ok();
        });
        if can { item } else { item.disabled() }
    };
    let back_row = row(&FocusBack, travels.back, ExclusivePanel::go_back);
    let up_row = row(&FocusUp, travels.up, ExclusivePanel::focus_up);
    ToolbarGroup::new(TRAVEL, div().flex().items_center().child(back).child(up)).overflow_rows(
        back_row,
        [up_row],
        false,
    )
}

const BACK_ICON: &str = "arrow-left";
const UP_ICON: &str = "arrow-up";

/// The strip choosing the unit drawn and the roots, its picker's keys named
/// where `focus` stands, ending in the toggle opening the tree's `legend`.
fn strip(
    picker: &UnitPicker,
    scoping: Scoping,
    legend: Legend,
    focus: &FocusHandle,
    palette: &Palette,
    window: &Window,
    cx: &Context<ExclusivePanel>,
) -> OneRow {
    let points = picker.group(POINTS, focus, window, cx);
    OneRow::new("exclusive-controls")
        .group(travel(scoping.travels, focus, window, cx))
        .group(points)
        .filling(|| roots::field(scoping.roots, palette, cx))
        .when(scoping.computing, |row| {
            row.busy("exclusive-computing", "computing…")
        })
        .trailing(legend.toolbar_group())
}

/// The legend of `drawn`, open as `state` says: what each hue its cells wear
/// stands for, then the key to the notch; keying nothing where it names
/// nothing the unit picked does not, so that no key outlives the cells it
/// named.
fn legend(state: &Entity<LegendState>, drawn: Option<&Drawn>, palette: &Palette) -> Legend {
    let legend = Legend::new("exclusive-legend", state);
    let Some(drawn) = drawn.filter(|drawn| drawn.coloured.tree.total() > 0) else {
        return legend;
    };
    let coloured = &drawn.coloured;
    // A legend of one hue names nothing the unit picked does not.
    let hues = match coloured.legend.len() {
        0 | 1 => &[][..],
        _ => &coloured.legend[..],
    };
    let hues = hues
        .iter()
        .map(|(name, hue)| LegendEntry::new(Swatch::Fill(hue.color(palette).into()), name.clone()));
    // The key's cells wear a hue the cells drawn wear.
    let key_fill = match coloured.legend.first() {
        Some((_, hue)) => hue.color(palette),
        None => palette.data.neutral.into(),
    };
    let unit = coloured.tree.scope().unit();
    let key = drawn
        .draws_reached()
        .then(|| reached_key(unit, coloured.by(), key_fill, palette));
    legend.entries(hues.chain(key.into_iter().flatten()))
}

/// The legend's key to the notch, telling the roots of `unit`, chosen `by`,
/// that the build also reaches as a dependency apart from the others, each
/// beside a cell filled `fill` drawn as the icicle draws it.
fn reached_key(unit: Unit, by: RootedBy, fill: Rgba, palette: &Palette) -> [LegendEntry; 2] {
    let ink = paint::notch_ink(fill, palette.background);
    let cell = |notched: bool| {
        Swatch::Painted(Rc::new(move |cell, window, _| {
            window.paint_quad(gpui::fill(cell, fill));
            if notched {
                paint::paint_notch(cell, ink, window);
            }
        }))
    };
    let roots = format!("{} {}", by.word(), unit_names(unit).many);
    let reached = format!("{roots} {}", by.also_a_dependency());
    [
        LegendEntry::new(cell(false), roots),
        LegendEntry::new(cell(true), reached),
    ]
}

/// The layer over the cells that rings and describes the one selected,
/// painted apart from the cells so that neither repaints any of them, and
/// only while they are at rest. Its layout records where the cells lie for
/// the pointer handlers and the keys, and lets go of a cell the pointer
/// selected where they are laid out elsewhere.
fn hover_layer(view: Entity<ExclusivePanel>, palette: Palette) -> impl IntoElement {
    let prepaint = {
        let view = view.clone();
        move |bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App| {
            let frame = Frame {
                bounds,
                row: paint::row_height(window),
            };
            view.update(cx, |view, _| {
                if view.frame == Some(frame) {
                    return;
                }
                view.frame = Some(frame);
                let laid = view.laid_out();
                if let Some(drawn) = &mut view.drawn {
                    let moved = drawn.selection.take().and_then(|selection| {
                        let spot = laid.as_ref()?.find(&selection.spot)?;
                        (selection.by == Picked::Keys).then_some(Selection { spot, ..selection })
                    });
                    drawn.selection = moved;
                }
            });
            if let Some((lines, anchor)) = view.read(cx).card() {
                let tooltip = palette.colors.tooltip();
                defer_hover_card(lines, anchor, bounds, tooltip, window, cx);
            }
        }
    };
    let paint = move |_: Bounds<Pixels>, (), window: &mut Window, cx: &mut App| {
        if let Some(Selection { spot, .. }) = view.read(cx).at_rest_selection() {
            paint::paint_ring(spot.rect(), &palette, window);
        }
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
                verdicts: panel.fade.clone(),
                tree: Arc::clone(&drawn.coloured.tree),
                by: drawn.coloured.by(),
                top: drawn.top(),
                transition: drawn.transition.clone(),
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
    fn every_gesture_is_reachable() {
        assert_eq!(GESTURES.unreachable(), []);
    }

    #[test]
    fn every_gesture_fits_the_keys_help() {
        assert_eq!(GESTURES.overlong(), []);
    }

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
        let one = "  · +1 copy depended on";
        assert_eq!(other_copies(1).unwrap(), one);
        let several = "  · +2 copies depended on";
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
            "requested · reached from 3 other requested configured targets"
        );
        assert_eq!(targets.why(true), "in the target set");
        assert_eq!(tops.why(false), "no dependents");
    }

    #[test]
    fn the_targets_a_subtree_holds_read_by_where_else_they_are_configured() {
        let read = |targets, only_here| targets_held(targets, only_here);
        assert_eq!(read(0, 0), None);
        let read = |targets, only_here| read(targets, only_here).unwrap();
        assert_eq!(read(1, 0), "1 target · configured elsewhere too");
        assert_eq!(read(1, 1), "1 target · configured only here");
        let each_elsewhere = "3 targets · each configured elsewhere too";
        assert_eq!(read(3, 0), each_elsewhere);
        assert_eq!(read(3, 3), "3 targets · all configured only here");
        let some_here = "3 targets · 1 configured only here";
        assert_eq!(read(3, 1), some_here);
    }

    #[test]
    fn the_tops_gathered_as_shared_are_counted_and_said_to_be_depended_on_by_several() {
        let names = unit_names(Unit::ConfiguredTarget);
        assert_eq!(
            gathered_as_shared(1, TOP_LEVEL, &names),
            "1 configured target · shared by 2+ top-level configured targets"
        );
        assert_eq!(
            gathered_as_shared(7, REQUESTED, &names),
            "7 configured targets · shared by 2+ requested configured targets"
        );
    }
}
