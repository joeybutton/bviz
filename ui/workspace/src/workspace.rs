use crate::documents_menu::DocumentsMenu;
use crate::pane::Handover;
use crate::{
    AnyPanel, DividerId, Document, Documents, Layout, Pane, PaneEvent, PanelGroup, Removal, Side,
    Split, SplitSide,
};
use bviz_ui_components::{Divider, DragEnd, overlay_canvas, track_drag};
use bviz_ui_shell::Chrome;
use bviz_ui_theme::ThemeColors;
use gpui::{
    Action, AnyElement, AnyWindowHandle, App, Axis, Bounds, Canvas, Div, ElementId, Entity,
    EntityId, FocusHandle, IntoElement, KeyBinding, Pixels, Point, Render, SharedString,
    Subscription, Window, actions, div, prelude::*, px,
};
use std::collections::HashMap;

actions!(
    bviz_workspace,
    [
        NextTab,
        PreviousTab,
        ClosePanel,
        DetachPanel,
        ToggleDocuments,
        FocusLeft,
        FocusRight,
        FocusUp,
        FocusDown,
        MovePanelLeft,
        MovePanelRight,
        MovePanelUp,
        MovePanelDown,
        SplitLeft,
        SplitRight,
        SplitUp,
        SplitDown
    ]
);

/// The key context the workspace's own bindings are scoped to, so they reach a
/// window this crate frames and no other.
const KEY_CONTEXT: &str = "Workspace";

/// The hairline drawn between two sides of a split.
const DIVIDER_LINE: Pixels = px(1.);

/// The band the divider is grabbed by, wide enough to aim a press at where the
/// line alone would be a pixel wide.
const DIVIDER_GRIP: Pixels = px(7.);

pub(crate) fn register_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-tab", NextTab, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-shift-w", ClosePanel, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-shift-n", DetachPanel, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-shift-d", ToggleDocuments, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-k left", FocusLeft, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-k right", FocusRight, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-k up", FocusUp, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-k down", FocusDown, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-k shift-left", MovePanelLeft, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-k shift-right", MovePanelRight, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-k shift-up", MovePanelUp, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-k shift-down", MovePanelDown, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-k alt-left", SplitLeft, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-k alt-right", SplitRight, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-k alt-up", SplitUp, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-k alt-down", SplitDown, Some(KEY_CONTEXT)),
    ]);
}

/// Where a panel is re-homed by [`Workspace::move_panel`]. A pane is named by
/// its own handle, which holds wherever the arrangement lays that pane out: the
/// move takes the panel out first, and the splits around the pane it leaves can
/// collapse, and the destination still names the pane it named.
pub(crate) enum Destination {
    /// Among that pane's tabs, at `index` of them or at the end where it holds
    /// fewer.
    Tab { pane: Entity<Pane>, index: usize },
    /// A pane of its own on that side of that pane.
    Beside { pane: Entity<Pane>, side: Side },
    /// A pane of its own spanning that side of the window.
    WindowEdge(Side),
    /// A window of its own, framed by a workspace of the same application at
    /// this window's size.
    NewWindow,
}

/// Which pane lies on one side of another, as far as the last frame says.
#[derive(Debug, PartialEq)]
pub(crate) enum Towards<P = Entity<Pane>> {
    /// The pane laid out nearest that way.
    Found(P),
    /// Nothing but the window's own edge is laid out beyond that edge of the
    /// pane the direction starts from.
    Edge,
    /// Where the panes stand is not known.
    Unknown,
}

/// What a workspace keeps for each pane it frames.
struct Framed {
    _events: Subscription,
    /// Where the last frame laid the pane out, which is `None` until a frame
    /// has. Read between frames, to tell which pane a direction points at.
    bounds: Option<Bounds<Pixels>>,
}

/// A window's root view: the application chrome, and the arrangement of panes it
/// frames. One pane is the active one, which the panel actions act on and whose
/// panel titles the window.
pub struct Workspace {
    /// The application, named ahead of the active panel in the window title
    /// so a task list tells a bviz window from any other.
    app_name: SharedString,
    /// Focus for the chrome, so the shell's window actions (quit, close, tab)
    /// dispatch here; the ancestor of every panel's own focus.
    window_focus: FocusHandle,
    /// The window this workspace is the root of, through which a panel of
    /// another window is taken out of the panes here.
    window: AnyWindowHandle,
    layout: Layout,
    active: Entity<Pane>,
    /// The status bar's list of the session's documents, through which a view
    /// closed here or shown in another window is brought into this one.
    documents_menu: Entity<DocumentsMenu>,
    /// What is kept for every pane of `layout`, retired as its pane leaves.
    panes: HashMap<EntityId, Framed>,
    /// The divider being dragged, while one is.
    divider_drag: Option<DividerId>,
}

impl Workspace {
    /// Builds a workspace framing the arrangement `group` describes, showing and
    /// focusing its leading panel.
    pub(crate) fn new(
        app_name: impl Into<SharedString>,
        group: PanelGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut panes = Vec::new();
        let layout = group.arrange(&mut |panel, tabbed| {
            let pane = cx.new(|cx| Pane::new(panel, window, cx));
            pane.update(cx, |pane, cx| {
                for tab in tabbed {
                    pane.add(tab, window, cx);
                }
                pane.activate(0, window, cx);
            });
            panes.push(pane.clone());
            pane
        });
        let active = layout.first_pane().clone();
        let this = cx.weak_entity();
        let documents_menu = cx.new(|cx| DocumentsMenu::new(this.clone(), window, cx));
        let mut workspace = Self {
            app_name: app_name.into(),
            window_focus: cx.focus_handle(),
            window: window.window_handle(),
            layout,
            active,
            documents_menu,
            panes: HashMap::new(),
            divider_drag: None,
        };
        for pane in panes {
            workspace.watch(&pane, window, cx);
        }
        Documents::global(cx).update(cx, |documents, _| documents.watch_workspace(this));
        // Every pane took focus as it was built, so the last one built holds it;
        // the window opens on the leading panel instead.
        let panel = workspace.active_panel(cx);
        window.focus(&panel.focus_handle(cx), cx);
        set_window_title(&workspace.app_name, &panel, window, cx);
        workspace
    }

    /// Builds a workspace framing a region of the window per document: `first`
    /// across the top and each of `others` under it in the order given, every
    /// region laid out as its document's group describes.
    pub(crate) fn with_documents(
        app_name: impl Into<SharedString>,
        first: Document,
        others: Vec<Document>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new(app_name, Document::stack(first, others, cx), window, cx)
    }

    pub fn active_pane(&self) -> &Entity<Pane> {
        &self.active
    }

    pub(crate) fn window(&self) -> AnyWindowHandle {
        self.window
    }

    /// Whether the pane holding `panel` shows it; `None` where no pane here
    /// holds it at all.
    pub(crate) fn shows(&self, panel: EntityId, cx: &App) -> Option<bool> {
        let (pane, index) = self.holder(panel, cx)?;
        Some(pane.read(cx).showing() == index)
    }

    /// Brings `panel` forward in the pane holding it and makes that the active
    /// pane, reporting whether this workspace holds it.
    pub(crate) fn reveal(
        &mut self,
        panel: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((pane, index)) = self.holder(panel, cx) else {
            return false;
        };
        pane.update(cx, |pane, cx| pane.activate(index, window, cx));
        self.activate_pane(pane, window, cx);
        true
    }

    /// Takes `panel` out of the pane holding it, for a caller that re-homes it
    /// in another window. `None` where no pane here holds it.
    pub(crate) fn take_panel(
        &mut self,
        panel: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyPanel> {
        let (pane, _) = self.holder(panel, cx)?;
        self.vacate(&pane, panel, window, cx)
    }

    /// Takes `panel` out of the pane holding it and re-homes it at `to`, which
    /// becomes the active pane showing it. The panel itself is the one that
    /// moves — what it has scrolled to, zoomed in on and selected moves with it.
    /// A destination asking for where the panel already stands ([`re_homes`])
    /// leaves the arrangement and the panel as they are.
    ///
    /// [`re_homes`]: Workspace::re_homes
    pub(crate) fn move_panel(
        &mut self,
        panel: EntityId,
        to: Destination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((from, _)) = self.holder(panel, cx) else {
            return;
        };
        if !self.re_homes(&to, &from, cx) {
            return;
        }
        let Some(panel) = self.vacate(&from, panel, window, cx) else {
            return;
        };
        let landed = match to {
            Destination::Tab { pane, index } => {
                pane.update(cx, |pane, cx| pane.insert(index, panel, window, cx));
                pane
            }
            Destination::Beside { pane, side } => {
                let landed = self.pane_for(panel, window, cx);
                self.layout.insert_beside(&pane, side, landed.clone());
                landed
            }
            Destination::WindowEdge(side) => {
                let landed = self.pane_for(panel, window, cx);
                self.layout.insert_at_edge(side, landed.clone());
                landed
            }
            // A window the move leaves framing no pane is taken away only once
            // this update unwinds, by which time the window opened here stands
            // in its place, so moving the last panel out of the last window
            // does not quit the application.
            Destination::NewWindow => {
                self.open_window_for(panel, window, cx);
                return;
            }
        };
        self.activate_pane(landed, window, cx);
    }

    /// Whether moving a panel to `to` would lay it out anywhere other than where
    /// the pane `from` holding it already stands. A destination has to name a
    /// pane this arrangement lays out; and where `from` holds that one panel
    /// alone it is already the panel's own pane, so a destination asking for a
    /// pane of its own where that one stands — `from` itself, or an edge of the
    /// window `from` already spans — is refused: the move would take that pane
    /// out and build an equivalent one at an even share of its split, putting
    /// the dividers around it back where a drag had moved them from.
    fn re_homes(&self, to: &Destination, from: &Entity<Pane>, cx: &App) -> bool {
        let alone = from.read(cx).tab_count() == 1;
        match to {
            Destination::Tab { pane, .. } | Destination::Beside { pane, .. } => {
                self.layout.contains(pane) && !(alone && pane == from)
            }
            Destination::WindowEdge(side) => !(alone && self.layout.spans_edge(from, *side)),
            Destination::NewWindow => true,
        }
    }

    /// Takes `panel` out of `from` for a caller re-homing it, and takes `from`
    /// out of the arrangement where that panel was the only one it held.
    fn vacate(
        &mut self,
        from: &Entity<Pane>,
        panel: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyPanel> {
        match from.update(cx, |pane, cx| pane.remove(panel, window, cx))? {
            Handover::Kept(panel) => Some(panel),
            Handover::Emptied(panel) => {
                self.close_pane(from, window, cx);
                Some(panel)
            }
        }
    }

    /// A pane of the arrangement showing `panel` alone.
    fn pane_for(
        &mut self,
        panel: AnyPanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Pane> {
        let pane = cx.new(|cx| Pane::new(panel, window, cx));
        self.watch(&pane, window, cx);
        pane
    }

    fn open_window_for(&mut self, panel: AnyPanel, window: &mut Window, cx: &mut Context<Self>) {
        let app_name = self.app_name.clone();
        let size = window.viewport_size();
        bviz_ui_shell::open_window(&self.app_name, size, cx, move |window, cx| {
            cx.new(|cx| Self::new(app_name, PanelGroup::one(panel), window, cx))
        })
        .expect("a window opens for the panel moved out of this one");
    }

    /// The pane laid out nearest the active one on `side` of it, made active.
    fn focus_towards(&mut self, side: Side, window: &mut Window, cx: &mut Context<Self>) {
        if let Towards::Found(pane) = self.pane_in_direction(&self.active, side) {
            self.activate_pane(pane, window, cx);
        }
    }

    /// Moves the panel on show into the pane on `side` of the one showing it, as
    /// its last tab — or against that edge of the window, where no pane lies
    /// that way.
    fn move_towards(&mut self, side: Side, window: &mut Window, cx: &mut Context<Self>) {
        let to = match self.pane_in_direction(&self.active, side) {
            Towards::Found(pane) => {
                let index = pane.read(cx).tab_count();
                Destination::Tab { pane, index }
            }
            Towards::Edge => Destination::WindowEdge(side),
            Towards::Unknown => return,
        };
        let panel = self.active_panel(cx).entity_id();
        self.move_panel(panel, to, window, cx);
    }

    /// Takes the panel on show into a pane of its own on `side` of the pane
    /// showing it.
    fn split_towards(&mut self, side: Side, window: &mut Window, cx: &mut Context<Self>) {
        let panel = self.active_panel(cx).entity_id();
        let pane = self.active.clone();
        self.move_panel(panel, Destination::Beside { pane, side }, window, cx);
    }

    /// The pane `side` of `from` a move or a focus in that direction picks.
    pub(crate) fn pane_in_direction(&self, from: &Entity<Pane>, side: Side) -> Towards {
        in_direction(&self.layout, from, side, |pane| self.pane_bounds(pane))
    }

    fn pane_bounds(&self, pane: &Entity<Pane>) -> Option<Bounds<Pixels>> {
        self.panes.get(&pane.entity_id())?.bounds
    }

    /// Records where the frame being laid out put `pane`.
    fn laid_out(&mut self, pane: EntityId, bounds: Bounds<Pixels>) {
        if let Some(framed) = self.panes.get_mut(&pane) {
            framed.bounds = Some(bounds);
        }
    }

    /// The pane holding `panel` and the tab it holds it at.
    fn holder(&self, panel: EntityId, cx: &App) -> Option<(Entity<Pane>, usize)> {
        self.layout
            .panes()
            .find_map(|pane| pane.read(cx).index_of(panel).map(|at| (pane.clone(), at)))
    }

    pub fn active_panel(&self, cx: &App) -> AnyPanel {
        self.active.read(cx).active_panel().clone()
    }

    /// Adds `panel` to the active pane and shows it.
    pub(crate) fn add_panel(
        &mut self,
        panel: AnyPanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.active
            .update(cx, |pane, cx| pane.add(panel, window, cx));
    }

    /// Takes the panel the active pane shows into a window of its own, and
    /// focuses it there. A pane left with no panel leaves the arrangement, and a
    /// window left framing no pane closes.
    pub fn detach_active_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = self.active_panel(cx).entity_id();
        self.move_panel(panel, Destination::NewWindow, window, cx);
    }

    fn watch(&mut self, pane: &Entity<Pane>, window: &Window, cx: &mut Context<Self>) {
        let events = cx.subscribe_in(
            pane,
            window,
            |workspace, pane, event, window, cx| match event {
                PaneEvent::Changed => {
                    if *pane == workspace.active {
                        set_window_title(
                            &workspace.app_name,
                            &workspace.active_panel(cx),
                            window,
                            cx,
                        );
                    }
                    cx.notify();
                }
                PaneEvent::Closed => workspace.close_pane(pane, window, cx),
                PaneEvent::Detach => {
                    workspace.activate_pane(pane.clone(), window, cx);
                    workspace.detach_active_panel(window, cx);
                }
            },
        );
        self.panes.insert(
            pane.entity_id(),
            Framed {
                _events: events,
                bounds: None,
            },
        );
    }

    fn close_pane(&mut self, pane: &Entity<Pane>, window: &mut Window, cx: &mut Context<Self>) {
        let removal = self.layout.remove(pane);
        if removal != Removal::NotFound {
            // A pane out of the arrangement is no longer this workspace's to
            // answer for: an emptied one goes on holding the panel it gave up,
            // so its events would otherwise reach here as well as the workspace
            // the panel moved to.
            self.panes.remove(&pane.entity_id());
        }
        match removal {
            Removal::NotFound => {}
            // The window has nothing left to frame.
            Removal::Emptied => window.remove_window(),
            Removal::Removed => {
                // A close can take away the boundary a drag is holding, and the
                // overlay that carries the drag goes with it.
                self.divider_drag = None;
                if *pane == self.active
                    && let Some(next) = self.layout.panes().next().cloned()
                {
                    self.activate_pane(next, window, cx);
                }
                cx.notify();
            }
        }
    }

    fn activate_pane(&mut self, pane: Entity<Pane>, window: &mut Window, cx: &mut Context<Self>) {
        self.active = pane;
        let panel = self.active_panel(cx);
        window.focus(&panel.focus_handle(cx), cx);
        set_window_title(&self.app_name, &panel, window, cx);
        cx.notify();
    }

    fn move_divider(&mut self, divider: DividerId, share: f32, cx: &mut Context<Self>) {
        if self.layout.move_divider(divider, share) {
            cx.notify();
        }
    }

    /// The arrangement laid out: panes side by side or one above the other, each
    /// side of a split at the share of it the split gives that side.
    fn arrangement(
        &self,
        layout: &Layout,
        colors: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match layout {
            Layout::Pane(pane) => self.pane_element(pane, cx),
            Layout::Split(split) => self.split_element(split, colors, cx),
        }
    }

    fn pane_element(&self, pane: &Entity<Pane>, cx: &mut Context<Self>) -> AnyElement {
        let handle = pane.clone();
        div()
            .id(ElementId::NamedInteger(
                "workspace-pane".into(),
                pane.entity_id().as_u64(),
            ))
            // A panel is laid out as the window's own column lays one out, so
            // that a pane frames it exactly as a window holding it alone does.
            .flex()
            .flex_col()
            .size_full()
            // A press anywhere in a pane aims the panel actions at it. Taken in
            // the capture phase, so a panel that makes something of the press
            // itself does not keep it from reaching here.
            .capture_any_mouse_down(cx.listener(move |workspace, _, window, cx| {
                if workspace.active != handle {
                    workspace.activate_pane(handle.clone(), window, cx);
                }
            }))
            .child(pane.clone())
            .child(measure(pane, cx))
            .into_any_element()
    }

    fn split_element(
        &self,
        split: &Split,
        colors: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let axis = split.axis();
        let mut container = match axis {
            Axis::Horizontal => div().flex_row(),
            Axis::Vertical => div().flex_col(),
        }
        .flex()
        .size_full();
        let mut boundaries = 0;
        let mut dragged = None;
        for SplitSide {
            divider: against,
            layout,
            share,
        } in split.sides()
        {
            if let Some(id) = against {
                let dragging = self.divider_drag == Some(id);
                container = container.child(divider(id, axis, dragging, colors, cx));
                if dragging {
                    dragged = Some((id, boundaries));
                }
                boundaries += 1;
            }
            let held = self.arrangement(layout, colors, cx);
            container = container.child(side(held, share, axis));
        }
        container
            .when_some(dragged, |container, (id, boundary)| {
                container.child(divider_drag(id, axis, boundary, boundaries, cx))
            })
            .into_any_element()
    }
}

/// A canvas over the pane, which draws nothing and tells the workspace where the
/// frame laid that pane out.
fn measure(pane: &Entity<Pane>, cx: &mut Context<Workspace>) -> Canvas<()> {
    let workspace = cx.entity();
    let pane = pane.entity_id();
    overlay_canvas(move |bounds, _, cx| {
        workspace.update(cx, |workspace, _| workspace.laid_out(pane, bounds));
    })
}

/// Which pane `side` of `from` a direction points at, out of the panes `layout`
/// lays out, `bounds` giving where the last frame laid each of them out. Any
/// pane without bounds leaves the answer [`Towards::Unknown`]: an arrangement
/// only part of which has been laid out cannot say what is beside what, and the
/// pane a frame has yet to measure may be the very one on that side.
fn in_direction<P: Clone + PartialEq>(
    layout: &Layout<P>,
    from: &P,
    side: Side,
    bounds: impl Fn(&P) -> Option<Bounds<Pixels>>,
) -> Towards<P> {
    let mut others = Vec::with_capacity(layout.panes().count());
    for pane in layout.panes().filter(|pane| *pane != from) {
        let Some(laid_out) = bounds(pane) else {
            return Towards::Unknown;
        };
        others.push((pane.clone(), laid_out));
    }
    // Nothing is laid out beyond any edge of the only pane of an arrangement,
    // and that answer needs no bounds — which it must not, a window framing its
    // one pane by itself, with no measuring canvas over it.
    if others.is_empty() {
        return Towards::Edge;
    }
    let Some(here) = bounds(from) else {
        return Towards::Unknown;
    };
    match nearest(here, side, others) {
        Some(pane) => Towards::Found(pane),
        None => Towards::Edge,
    }
}

/// Of the panes laid out on `side` of `from`, the one a direction points at:
/// those whose span across that side's axis overlaps `from`'s come first, then
/// the nearest along that axis, then the nearest across it, and then whichever
/// was offered first.
fn nearest<T>(
    from: Bounds<Pixels>,
    side: Side,
    candidates: impl IntoIterator<Item = (T, Bounds<Pixels>)>,
) -> Option<T> {
    candidates
        .into_iter()
        .filter_map(|(pane, bounds)| Some((pane, reach(from, side, bounds)?)))
        .min_by(|(_, reached), (_, against)| {
            reached
                .0
                .cmp(&against.0)
                .then(reached.1.total_cmp(&against.1))
                .then(reached.2.total_cmp(&against.2))
        })
        .map(|(pane, _)| pane)
}

/// How far `other` is from `from` in the direction of `side`: whether their
/// spans across that side's axis are apart, how far along that axis the two are
/// parted, and how far apart their middles are across it. `None` where `other`
/// is not laid out beyond `from`'s edge on that side at all.
fn reach(from: Bounds<Pixels>, side: Side, other: Bounds<Pixels>) -> Option<(bool, f32, f32)> {
    let (from_start, from_end) = span(from, side.axis());
    let (other_start, other_end) = span(other, side.axis());
    let parted = match side.leads() {
        true => from_start - other_end,
        false => other_start - from_end,
    };
    if parted < 0.0 {
        return None;
    }
    let (from_near, from_far) = span(from, side.axis().invert());
    let (other_near, other_far) = span(other, side.axis().invert());
    let apart = other_far <= from_near || other_near >= from_far;
    let across = ((other_near + other_far) - (from_near + from_far)).abs() / 2.0;
    Some((apart, parted, across))
}

/// Where `bounds` starts and ends along `axis`.
fn span(bounds: Bounds<Pixels>, axis: Axis) -> (f32, f32) {
    match axis {
        Axis::Horizontal => (f32::from(bounds.left()), f32::from(bounds.right())),
        Axis::Vertical => (f32::from(bounds.top()), f32::from(bounds.bottom())),
    }
}

/// The grab band at one boundary between a split's sides, [`DIVIDER_GRIP`]
/// across.
fn divider(
    id: DividerId,
    axis: Axis,
    dragging: bool,
    colors: &ThemeColors,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let line = if dragging {
        colors.accent
    } else {
        colors.border
    };
    Divider::new(
        ElementId::NamedInteger("split-divider".into(), id.as_u64()),
        axis,
        cx.listener(move |workspace, _, _, cx| {
            workspace.divider_drag = Some(id);
            cx.notify();
        }),
    )
    .band(DIVIDER_GRIP)
    .line(DIVIDER_LINE, line)
}

/// One side of a split, taking `share` of the room the split's dividers leave
/// its sides, and clipping whatever it holds to the room it is given.
fn side(content: AnyElement, share: f32, axis: Axis) -> Div {
    let side = div()
        .flex_basis(px(0.))
        .flex_grow(share)
        .overflow_hidden()
        .child(content);
    match axis {
        Axis::Horizontal => side.h_full().min_w(px(0.)),
        Axis::Vertical => side.w_full().min_h(px(0.)),
    }
}

/// A canvas over the split, rendered while one of its dividers is being dragged,
/// which draws nothing and re-installs the window-global handlers that carry the
/// drag past the divider and past the window's own edge.
fn divider_drag(
    id: DividerId,
    axis: Axis,
    boundary: usize,
    boundaries: usize,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let workspace = cx.entity();
    overlay_canvas(move |bounds, window, _| {
        track_drag(
            window,
            &workspace,
            move |workspace, position, cx| {
                if let Some(share) = grabbed_share(bounds, position, axis, boundary, boundaries) {
                    workspace.move_divider(id, share, cx);
                }
            },
            move |workspace, _, _: DragEnd, cx| {
                workspace.divider_drag = None;
                cx.notify();
            },
        );
    })
}

/// The share of `bounds` the divider at `boundary` leaves the sides ahead of it
/// when dragged to `position`, measured over the room the sides share — what is
/// left of the split once each of its `boundaries` bands has taken its own
/// extent, and what the bands ahead of this one push it past. `None` for a split
/// with no room to divide.
fn grabbed_share(
    bounds: Bounds<Pixels>,
    position: Point<Pixels>,
    axis: Axis,
    boundary: usize,
    boundaries: usize,
) -> Option<f32> {
    let (along, extent) = match axis {
        Axis::Horizontal => (position.x - bounds.origin.x, bounds.size.width),
        Axis::Vertical => (position.y - bounds.origin.y, bounds.size.height),
    };
    let grip = f32::from(DIVIDER_GRIP);
    let room = f32::from(extent) - grip * boundaries as f32;
    if room <= 0.0 {
        return None;
    }
    Some((f32::from(along) - grip * (boundary as f32 + 0.5)) / room)
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = bviz_ui_theme::theme(window, cx).colors;
        let panel = self.active_panel(cx);
        let body = match &self.layout {
            // A lone pane is framed by the window itself: there is no room to
            // divide, and with no other pane to pick between, no direction out
            // of it turns on where a frame laid it out.
            Layout::Pane(pane) => pane.clone().into_any_element(),
            layout => self.arrangement(layout, &colors, cx),
        };
        let chrome = Chrome::new(&self.window_focus)
            .title(window_title(&self.app_name, &panel, cx))
            .menu(self.documents_menu.clone())
            .child(body);
        let chrome = match panel.status(cx) {
            Some(status) => chrome.status(status),
            None => chrome,
        };
        let body = div()
            .key_context(KEY_CONTEXT)
            .size_full()
            .on_action(cx.listener(|workspace, _: &NextTab, window, cx| {
                workspace
                    .active
                    .update(cx, |pane, cx| pane.activate_next(window, cx));
            }))
            .on_action(cx.listener(|workspace, _: &PreviousTab, window, cx| {
                workspace
                    .active
                    .update(cx, |pane, cx| pane.activate_previous(window, cx));
            }))
            .on_action(cx.listener(|workspace, _: &ClosePanel, window, cx| {
                workspace
                    .active
                    .update(cx, |pane, cx| pane.close_active(window, cx));
            }))
            .on_action(cx.listener(|workspace, _: &DetachPanel, window, cx| {
                workspace.detach_active_panel(window, cx);
            }))
            // Not a listener: `toggle` reads this workspace.
            .on_action({
                let menu = self.documents_menu.clone();
                move |_: &ToggleDocuments, window, cx| {
                    menu.update(cx, |menu, cx| menu.toggle(window, cx));
                }
            });
        let body = towards::<FocusLeft, MovePanelLeft, SplitLeft>(body, Side::Left, cx);
        let body = towards::<FocusRight, MovePanelRight, SplitRight>(body, Side::Right, cx);
        let body = towards::<FocusUp, MovePanelUp, SplitUp>(body, Side::Top, cx);
        let body = towards::<FocusDown, MovePanelDown, SplitDown>(body, Side::Bottom, cx);
        body.child(chrome)
    }
}

/// Handlers for the three actions pointing at `side`.
fn towards<Focus: Action, Move: Action, SplitOff: Action>(
    body: Div,
    side: Side,
    cx: &mut Context<Workspace>,
) -> Div {
    body.on_action(cx.listener(move |workspace, _: &Focus, window, cx| {
        workspace.focus_towards(side, window, cx);
    }))
    .on_action(cx.listener(move |workspace, _: &Move, window, cx| {
        workspace.move_towards(side, window, cx);
    }))
    .on_action(cx.listener(move |workspace, _: &SplitOff, window, cx| {
        workspace.split_towards(side, window, cx);
    }))
}

/// What names the window: the application, then the document on show.
fn window_title(app_name: &str, panel: &AnyPanel, cx: &App) -> String {
    format!("{app_name} \u{2014} {}", panel.display_title(cx))
}

fn set_window_title(app_name: &str, panel: &AnyPanel, window: &mut Window, cx: &App) {
    window.set_window_title(&window_title(app_name, panel, cx));
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size};

    const EXTENT: f32 = 407.0;
    const GRIP: f32 = 7.0;

    /// A split of `EXTENT` along `axis`, offset from the window's own origin so
    /// a share measured from the wrong origin is told from a right one.
    fn split_bounds(axis: Axis) -> Bounds<Pixels> {
        let origin = point(px(30.), px(70.));
        let extent = match axis {
            Axis::Horizontal => size(px(EXTENT), px(100.)),
            Axis::Vertical => size(px(100.), px(EXTENT)),
        };
        Bounds::new(origin, extent)
    }

    /// The share a divider at `boundary`, one of `boundaries` in the split,
    /// leaves the sides ahead of it when grabbed `along` from the split's start.
    fn share(along: f32, axis: Axis, boundary: usize, boundaries: usize) -> Option<f32> {
        let bounds = split_bounds(axis);
        let position = match axis {
            Axis::Horizontal => point(bounds.origin.x + px(along), bounds.origin.y + px(50.)),
            Axis::Vertical => point(bounds.origin.x + px(50.), bounds.origin.y + px(along)),
        };
        grabbed_share(bounds, position, axis, boundary, boundaries)
    }

    /// The share is of the room the sides share, which is what a side is laid
    /// out at: a divider dragged to the middle of that room halves it, and one
    /// dragged to either end leaves the whole of it to one side of itself.
    #[test]
    fn a_divider_leaves_the_share_it_is_dragged_to() {
        for axis in [Axis::Horizontal, Axis::Vertical] {
            assert_eq!(share(GRIP / 2.0, axis, 0, 1), Some(0.0));
            assert_eq!(share(EXTENT / 2.0, axis, 0, 1), Some(0.5));
            assert_eq!(share(EXTENT - GRIP / 2.0, axis, 0, 1), Some(1.0));
        }
    }

    /// Every band takes its extent out of the room the sides divide, and the
    /// bands ahead of a divider push it that much further along the split, so a
    /// later divider reads its share from the same room measured from the same
    /// place.
    #[test]
    fn a_later_divider_reads_its_share_over_the_room_every_band_leaves() {
        let boundaries = 3;
        let room = EXTENT - GRIP * boundaries as f32;
        for boundary in 0..boundaries {
            let start = GRIP * (boundary as f32 + 0.5);
            let axis = Axis::Horizontal;
            assert_eq!(share(start, axis, boundary, boundaries), Some(0.0));
            assert_eq!(share(start + room, axis, boundary, boundaries), Some(1.0));
            assert_eq!(
                share(start + room / 2.0, axis, boundary, boundaries),
                Some(0.5)
            );
        }
    }

    /// A drag runs on past the split it belongs to, and reports where the cursor
    /// went; the share it asks for follows it out and is bounded on the way to
    /// the layout rather than here.
    #[test]
    fn a_divider_dragged_past_the_split_asks_for_a_share_past_its_end() {
        assert!(share(-100.0, Axis::Horizontal, 0, 1).is_some_and(|share| share < 0.0));
        assert!(share(EXTENT + 100.0, Axis::Horizontal, 0, 1).is_some_and(|share| share > 1.0));
    }

    /// A split with no more room than its own dividers has no room to divide.
    #[test]
    fn a_split_with_no_room_for_any_side_has_no_share_to_give() {
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(GRIP), px(100.)));
        let position = point(px(3.), px(50.));
        assert_eq!(
            grabbed_share(bounds, position, Axis::Horizontal, 0, 1),
            None
        );
    }

    /// `width` by `height` at (`x`, `y`) of the window.
    fn at(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    }

    /// A pane down the left of the window, two halves of the middle one above
    /// the other, and a pane down the right, each parted from its neighbours by
    /// a divider's band.
    fn quartered() -> [(&'static str, Bounds<Pixels>); 4] {
        [
            ("left", at(0.0, 0.0, 400.0, 600.0)),
            ("upper middle", at(410.0, 0.0, 590.0, 295.0)),
            ("lower middle", at(410.0, 305.0, 590.0, 295.0)),
            ("right", at(1010.0, 0.0, 190.0, 600.0)),
        ]
    }

    /// The arrangement `quartered` lays out: the left pane, the two middle ones
    /// one above the other, and the right pane.
    fn quartered_layout() -> Layout<&'static str> {
        let middle = Layout::split_at(
            Axis::Vertical,
            0.5,
            Layout::Pane("upper middle"),
            Layout::Pane("lower middle"),
        );
        Layout::divided(
            Axis::Horizontal,
            (Layout::Pane("left"), 1.0),
            vec![(middle, 1.0), (Layout::Pane("right"), 1.0)],
        )
    }

    /// Where `quartered` lays `pane` out.
    fn laid_out(pane: &&str) -> Option<Bounds<Pixels>> {
        quartered()
            .into_iter()
            .find(|(name, _)| name == pane)
            .map(|(_, bounds)| bounds)
    }

    /// The pane `side` of the one named `from` in `quartered`.
    fn pane_towards(from: &'static str, side: Side) -> Towards<&'static str> {
        in_direction(&quartered_layout(), &from, side, laid_out)
    }

    /// A direction picks the pane laid out beyond that edge of the one it starts
    /// from, and the nearest of them along the direction it points.
    #[test]
    fn a_direction_picks_the_pane_nearest_it_on_that_side() {
        assert_eq!(
            pane_towards("upper middle", Side::Left),
            Towards::Found("left")
        );
        assert_eq!(
            pane_towards("upper middle", Side::Right),
            Towards::Found("right")
        );
        assert_eq!(
            pane_towards("upper middle", Side::Bottom),
            Towards::Found("lower middle")
        );
        assert_eq!(
            pane_towards("lower middle", Side::Top),
            Towards::Found("upper middle")
        );
        assert_eq!(
            pane_towards("right", Side::Left),
            Towards::Found("upper middle")
        );
    }

    /// A pane laid out across an edge rather than beyond it is not on that side
    /// at all, so a direction pointing out of the window reaches the window's
    /// own edge.
    #[test]
    fn a_direction_with_no_pane_beyond_that_edge_reaches_the_windows_own() {
        assert_eq!(pane_towards("left", Side::Left), Towards::Edge);
        assert_eq!(pane_towards("right", Side::Right), Towards::Edge);
        assert_eq!(pane_towards("left", Side::Top), Towards::Edge);
        assert_eq!(pane_towards("upper middle", Side::Top), Towards::Edge);
    }

    /// Every direction out of the one pane of a window reaches the window's own
    /// edge because nothing else is laid out at all, which is an answer the
    /// arrangement gives whether or not a frame has measured that pane.
    #[test]
    fn every_direction_out_of_a_lone_pane_reaches_the_windows_edge() {
        let lone = Layout::Pane("only");
        for side in [Side::Left, Side::Right, Side::Top, Side::Bottom] {
            assert_eq!(in_direction(&lone, &"only", side, |_| None), Towards::Edge);
        }
    }

    /// A pane a frame has yet to lay out could be the very one on that side, so
    /// a direction out of an arrangement holding one answers neither a pane nor
    /// the window's edge — whether it is the pane the direction starts from that
    /// is unmeasured or any other.
    #[test]
    fn a_direction_out_of_an_unmeasured_arrangement_is_unknown() {
        let without = |unmeasured: &'static str, from: &'static str, side| {
            in_direction(&quartered_layout(), &from, side, |pane| match *pane {
                name if name == unmeasured => None,
                _ => laid_out(pane),
            })
        };
        assert_eq!(without("right", "left", Side::Right), Towards::Unknown);
        assert_eq!(without("left", "left", Side::Right), Towards::Unknown);
    }

    /// Panes equally far along the direction and equally far across it are
    /// picked between by the order they are offered, and a direction offers them
    /// in the order the arrangement lays them out.
    #[test]
    fn a_direction_settles_a_tie_by_the_order_the_panes_are_laid_out() {
        let [left, upper, lower, _] = quartered();
        assert_eq!(
            nearest(left.1, Side::Right, [upper, lower]),
            Some("upper middle")
        );
        assert_eq!(
            nearest(left.1, Side::Right, [lower, upper]),
            Some("lower middle")
        );
        assert_eq!(
            pane_towards("left", Side::Right),
            Towards::Found("upper middle")
        );
    }

    /// A pane whose span across the direction overlaps the one the direction
    /// starts from is picked over one laid out nearer that misses it, so a
    /// direction lands on a pane that is really beside the pane it came from.
    #[test]
    fn a_direction_picks_an_overlapping_pane_over_a_nearer_one_it_misses() {
        let from = at(0.0, 300.0, 200.0, 200.0);
        let missing = ("above", at(210.0, 0.0, 100.0, 100.0));
        let beside = ("beside", at(400.0, 350.0, 100.0, 100.0));
        assert_eq!(
            nearest(from, Side::Right, [missing, beside]),
            Some("beside")
        );
    }

    /// Panes equally far along the direction and both beside the one it starts
    /// from are picked between by how far apart their middles are across it, so
    /// a direction lands on the pane it most nearly points at.
    #[test]
    fn a_direction_settles_an_equal_reach_by_the_nearer_middle() {
        let from = at(0.0, 200.0, 100.0, 200.0);
        let level = ("level", at(200.0, 250.0, 100.0, 100.0));
        let offset = ("offset", at(200.0, 350.0, 100.0, 100.0));
        assert_eq!(nearest(from, Side::Right, [offset, level]), Some("level"));
    }

    /// A pane whose far edge is exactly level with the near edge of the one the
    /// direction starts from is beside it and is picked; one reaching past that
    /// edge lies over it rather than beyond it, and is not.
    #[test]
    fn a_direction_picks_a_pane_that_reaches_level_with_its_own_edge() {
        let from = at(100.0, 0.0, 100.0, 100.0);
        let level = ("level", at(0.0, 0.0, 100.0, 100.0));
        let over = ("over", at(0.0, 0.0, 101.0, 100.0));
        assert_eq!(nearest(from, Side::Left, [level]), Some("level"));
        assert_eq!(nearest(from, Side::Left, [over]), None);
    }
}
