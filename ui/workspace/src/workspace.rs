use crate::{AnyPanel, Layout, Pane, PaneEvent, Removal, Split, SplitId};
use bviz_ui_components::{DragEnd, overlay_canvas, track_drag};
use bviz_ui_shell::Chrome;
use bviz_ui_theme::ThemeColors;
use gpui::{
    AnyElement, App, Axis, Bounds, Div, ElementId, Entity, EntityId, FocusHandle, IntoElement,
    KeyBinding, MouseButton, Pixels, Point, Render, Subscription, Window, actions, div, prelude::*,
    px,
};
use std::collections::HashMap;

actions!(
    ui_workspace,
    [
        NextTab,
        PreviousTab,
        ClosePanel,
        SplitHorizontal,
        SplitVertical
    ]
);

/// The key context the workspace's own bindings are scoped to, so they reach a
/// window this crate frames and no other.
const KEY_CONTEXT: &str = "Workspace";

/// The hairline drawn between a split's two sides.
const DIVIDER_LINE: Pixels = px(1.);

/// The band the divider is grabbed by, wide enough to aim a press at where the
/// line alone would be a pixel wide.
const DIVIDER_GRIP: Pixels = px(7.);

/// Register the key bindings the workspace's panel and pane actions answer to.
/// Call once at startup, before any window opens: binding keys refreshes every
/// window, which drops what each has cached of its last frame.
pub fn register_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-tab", NextTab, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-shift-w", ClosePanel, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-shift-h", SplitHorizontal, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-shift-v", SplitVertical, Some(KEY_CONTEXT)),
    ]);
}

/// A window's root view: the application chrome, and the arrangement of panes it
/// frames. One pane is the active one, which the panel actions act on and whose
/// panel titles the window.
pub struct Workspace {
    /// Focus for the chrome, so the shell's window actions (quit, close, tab)
    /// dispatch here; the ancestor of every panel's own focus.
    window_focus: FocusHandle,
    layout: Layout,
    active: Entity<Pane>,
    /// The events of every pane of `layout`, each retired as its pane leaves.
    pane_events: HashMap<EntityId, Subscription>,
    /// The split whose divider is being dragged, while one is.
    divider_drag: Option<SplitId>,
}

impl Workspace {
    pub fn new(panel: AnyPanel, window: &mut Window, cx: &mut Context<Self>) -> Self {
        set_window_title(&panel, window, cx);
        let pane = cx.new(|cx| Pane::new(panel, window, cx));
        let mut workspace = Self {
            window_focus: cx.focus_handle(),
            layout: Layout::Pane(pane.clone()),
            active: pane.clone(),
            pane_events: HashMap::new(),
            divider_drag: None,
        };
        workspace.watch(&pane, window, cx);
        workspace
    }

    /// Builds a workspace whose one pane holds `panel` and `others` as tabs, in
    /// that order, showing `panel`.
    pub fn with_panels(
        panel: AnyPanel,
        others: Vec<AnyPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut workspace = Self::new(panel, window, cx);
        for other in others {
            workspace.add_panel(other, window, cx);
        }
        let pane = workspace.active.clone();
        pane.update(cx, |pane, cx| pane.activate(0, window, cx));
        workspace
    }

    pub fn active_pane(&self) -> &Entity<Pane> {
        &self.active
    }

    pub fn active_panel(&self, cx: &App) -> AnyPanel {
        self.active.read(cx).active_panel().clone()
    }

    /// Adds `panel` to the active pane and shows it.
    pub fn add_panel(&mut self, panel: AnyPanel, window: &mut Window, cx: &mut Context<Self>) {
        self.active
            .update(cx, |pane, cx| pane.add(panel, window, cx));
    }

    /// Puts `panel` in a pane of its own beside the active pane, along `axis`,
    /// and makes that the active pane.
    pub fn split(
        &mut self,
        axis: Axis,
        panel: AnyPanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pane = cx.new(|cx| Pane::new(panel, window, cx));
        self.layout.split(&self.active.clone(), axis, pane.clone());
        self.watch(&pane, window, cx);
        self.activate_pane(pane, window, cx);
    }

    /// Takes the panel the active pane shows into a pane of its own beside it.
    /// A pane showing its only panel has nothing to split off.
    fn split_active(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        let taken = self
            .active
            .update(cx, |pane, cx| pane.take_active(window, cx));
        if let Some(panel) = taken {
            self.split(axis, panel, window, cx);
        }
    }

    fn watch(&mut self, pane: &Entity<Pane>, window: &Window, cx: &mut Context<Self>) {
        let events = cx.subscribe_in(
            pane,
            window,
            |workspace, pane, event, window, cx| match event {
                PaneEvent::Changed => {
                    if *pane == workspace.active {
                        set_window_title(&workspace.active_panel(cx), window, cx);
                    }
                    cx.notify();
                }
                PaneEvent::Closed => workspace.close_pane(pane, window, cx),
            },
        );
        self.pane_events.insert(pane.entity_id(), events);
    }

    fn close_pane(&mut self, pane: &Entity<Pane>, window: &mut Window, cx: &mut Context<Self>) {
        match self.layout.remove(pane) {
            Removal::NotFound => {}
            // The window has nothing left to frame.
            Removal::Emptied => window.remove_window(),
            Removal::Removed => {
                self.pane_events.remove(&pane.entity_id());
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
        set_window_title(&panel, window, cx);
        cx.notify();
    }

    fn move_divider(&mut self, id: SplitId, ratio: f32, cx: &mut Context<Self>) {
        if self.layout.move_divider(id, ratio) {
            cx.notify();
        }
    }

    /// The arrangement laid out: panes side by side or one above the other, each
    /// split's two sides sized by its own ratio.
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
            .into_any_element()
    }

    fn split_element(
        &self,
        split: &Split,
        colors: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (axis, id, ratio) = (split.axis(), split.id(), split.ratio());
        let dragging = self.divider_drag == Some(id);
        let first = self.arrangement(split.first(), colors, cx);
        let second = self.arrangement(split.second(), colors, cx);
        let container = match axis {
            Axis::Horizontal => div().flex_row(),
            Axis::Vertical => div().flex_col(),
        };
        container
            .flex()
            .size_full()
            .child(side(first, ratio, axis))
            .child(divider(id, axis, dragging, colors, cx))
            .child(side(second, 1.0 - ratio, axis))
            .when(dragging, |container| {
                container.child(divider_drag(id, axis, cx))
            })
            .into_any_element()
    }
}

/// The grip between a split's two sides: a hairline down the middle of a band
/// wide enough to aim a press at.
fn divider(
    id: SplitId,
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
    let grip = div()
        .id(ElementId::NamedInteger("split-divider".into(), id.as_u64()))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |workspace, _, _, cx| {
                workspace.divider_drag = Some(id);
                cx.notify();
            }),
        );
    match axis {
        Axis::Horizontal => grip
            .w(DIVIDER_GRIP)
            .h_full()
            .cursor_col_resize()
            .child(div().w(DIVIDER_LINE).h_full().bg(line)),
        Axis::Vertical => grip
            .h(DIVIDER_GRIP)
            .w_full()
            .cursor_row_resize()
            .child(div().h(DIVIDER_LINE).w_full().bg(line)),
    }
}

/// One side of a split, taking `share` of the room the divider leaves it and its
/// opposite, and clipping whatever it holds to the room it is given.
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

/// A canvas over the split, rendered while its divider is being dragged, which
/// re-installs the window-global handlers that carry the drag past the divider
/// and past the window's own edge. It draws nothing and takes no press, so the
/// panes under it go on hit-testing as they were.
fn divider_drag(id: SplitId, axis: Axis, cx: &mut Context<Workspace>) -> impl IntoElement {
    let workspace = cx.entity();
    overlay_canvas(move |bounds, window, _| {
        track_drag(
            window,
            &workspace,
            move |workspace, position, cx| {
                if let Some(ratio) = grabbed_share(bounds, position, axis) {
                    workspace.move_divider(id, ratio, cx);
                }
            },
            move |workspace, _, _: DragEnd, cx| {
                workspace.divider_drag = None;
                cx.notify();
            },
        );
    })
}

/// The share of `bounds` a divider dragged to `position` leaves its first side,
/// measured over the room the two sides share; `None` for a split with no room
/// to divide.
fn grabbed_share(bounds: Bounds<Pixels>, position: Point<Pixels>, axis: Axis) -> Option<f32> {
    let (along, extent) = match axis {
        Axis::Horizontal => (position.x - bounds.origin.x, bounds.size.width),
        Axis::Vertical => (position.y - bounds.origin.y, bounds.size.height),
    };
    let room = f32::from(extent) - f32::from(DIVIDER_GRIP);
    if room <= 0.0 {
        return None;
    }
    Some((f32::from(along) - f32::from(DIVIDER_GRIP) / 2.0) / room)
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = bviz_ui_theme::theme(window, cx).colors;
        let panel = self.active_panel(cx);
        let body = match &self.layout {
            // A lone pane is framed by the window itself; there is no room to
            // divide and no other pane to pick out.
            Layout::Pane(pane) => pane.clone().into_any_element(),
            layout => self.arrangement(layout, &colors, cx),
        };
        let chrome = Chrome::new(&self.window_focus)
            .title(panel.title(cx))
            .child(body);
        let chrome = match panel.status(cx) {
            Some(status) => chrome.status(status),
            None => chrome,
        };
        div()
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
            .on_action(cx.listener(|workspace, _: &SplitHorizontal, window, cx| {
                workspace.split_active(Axis::Horizontal, window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &SplitVertical, window, cx| {
                workspace.split_active(Axis::Vertical, window, cx);
            }))
            .child(chrome)
    }
}

/// Retitles the OS window, which under server-side decorations is what the
/// compositor's own title bar shows.
fn set_window_title(panel: &AnyPanel, window: &mut Window, cx: &App) {
    window.set_window_title(&panel.title(cx));
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

    /// The share a divider grabbed `along` from the start of the split leaves
    /// its first side.
    fn share(along: f32, axis: Axis) -> Option<f32> {
        let bounds = split_bounds(axis);
        let position = match axis {
            Axis::Horizontal => point(bounds.origin.x + px(along), bounds.origin.y + px(50.)),
            Axis::Vertical => point(bounds.origin.x + px(50.), bounds.origin.y + px(along)),
        };
        grabbed_share(bounds, position, axis)
    }

    /// The share is of the room the two sides share, which is what a side is
    /// laid out at: a divider dragged to the middle of that room halves it, and
    /// one dragged to either end leaves the whole of it to one side.
    #[test]
    fn a_divider_leaves_the_share_it_is_dragged_to() {
        for axis in [Axis::Horizontal, Axis::Vertical] {
            assert_eq!(share(GRIP / 2.0, axis), Some(0.0));
            assert_eq!(share(EXTENT / 2.0, axis), Some(0.5));
            assert_eq!(share(EXTENT - GRIP / 2.0, axis), Some(1.0));
        }
    }

    /// A drag runs on past the split it belongs to, and reports where the cursor
    /// went; the share it asks for follows it out and is bounded on the way to
    /// the layout rather than here.
    #[test]
    fn a_divider_dragged_past_the_split_asks_for_a_share_past_its_end() {
        assert!(share(-100.0, Axis::Horizontal).is_some_and(|share| share < 0.0));
        assert!(share(EXTENT + 100.0, Axis::Horizontal).is_some_and(|share| share > 1.0));
    }

    /// A split with no more room than its own divider has no room to divide.
    #[test]
    fn a_split_with_no_room_for_either_side_has_no_share_to_give() {
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(GRIP), px(100.)));
        let position = point(px(3.), px(50.));
        assert_eq!(grabbed_share(bounds, position, Axis::Horizontal), None);
    }
}
