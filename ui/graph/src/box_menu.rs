//! The menu of what can be done with a box of the graph, which a right press
//! on the box puts up, and a key for the selected box.

use crate::camera::{Rect, to_screen};
use crate::menu::over_window;
use crate::relation::Relation;
use crate::{GraphPanel, pins, why};
use bviz_graph_layout::Id;
use bviz_graph_layout::select::Visibility;
use bviz_ui_components::{Menu, MenuEvent, MenuItem, SubmenuItem};
use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, Pixels, Point, SharedString, Subscription,
    WeakEntity, Window, point,
};
use std::collections::HashSet;

/// One row of a box's menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Offer {
    /// Choose which of its relations a selected box marks.
    Highlight,
    AlwaysHide,
    AskWhy,
    /// Narrow the question open, which is about another box, to the paths from
    /// this one.
    AskFrom,
}

impl Offer {
    fn label(self) -> SharedString {
        match self {
            Offer::Highlight => "Highlight".into(),
            Offer::AlwaysHide => pins::offer(Visibility::Hidden).into(),
            Offer::AskWhy => "Show why box is in build".into(),
            Offer::AskFrom => "Show only paths from box".into(),
        }
    }
}

/// What the menu of a box offers, `asking_another` saying whether the question
/// open is about another box.
fn offered(asking_another: bool) -> Vec<Offer> {
    let mut offers = vec![Offer::Highlight, Offer::AlwaysHide, Offer::AskWhy];
    if asking_another {
        offers.push(Offer::AskFrom);
    }
    offers
}

/// A menu put up over a box, let go of once it closes.
pub(crate) struct BoxMenu {
    /// The box the rows act on.
    node: Id,
    menu: Entity<Menu>,
    _dismissed: Subscription,
}

/// Selects `node` and puts up the menu of what can be done with it at `at`.
pub(crate) fn open(
    view: &mut GraphPanel,
    node: Id,
    at: Point<Pixels>,
    window: &mut Window,
    cx: &mut Context<GraphPanel>,
) {
    view.selection.select(node);
    view.invalidate_canvas(cx);
    // The card stands beside the pointer, where the menu opens.
    view.box_card.forget(cx);
    let items = items(view, node, cx);
    let menu = cx.new(|cx| Menu::new(items, at, window, cx));
    let dismissed = cx.subscribe_in(&menu, window, |view, _, event: &MenuEvent, window, cx| {
        let MenuEvent::Dismissed { holding_focus } = event;
        view.box_menu = None;
        if *holding_focus {
            view.focus_handle.focus(window, cx);
        }
        cx.notify();
    });
    view.box_menu = Some(BoxMenu {
        node,
        menu,
        _dismissed: dismissed,
    });
}

/// Takes the menu down where `gone` holds the box it stands over.
pub(crate) fn forget(view: &GraphPanel, gone: &HashSet<Id>, cx: &mut App) {
    if let Some(open) = &view.box_menu
        && gone.contains(&open.node)
    {
        open.menu.update(cx, |menu, cx| menu.dismiss(cx));
    }
}

/// Takes the menu down, its box belonging to a graph no longer drawn.
pub(crate) fn close(view: &GraphPanel, cx: &mut App) {
    if let Some(open) = &view.box_menu {
        open.menu.update(cx, |menu, cx| menu.dismiss(cx));
    }
}

/// Puts up the menu of the selected box, where one is selected, at the foot
/// of the box.
pub(crate) fn open_selected(
    view: &mut GraphPanel,
    window: &mut Window,
    cx: &mut Context<GraphPanel>,
) {
    if let Some(node) = view.selection.node() {
        let at = foot(view, node);
        open(view, node, at, window, cx);
    }
}

/// The window point at the foot of `node` as the canvas draws it, held inside
/// the canvas; the canvas's middle where the drawing shows no box for `node`.
fn foot(view: &GraphPanel, node: Id) -> Point<Pixels> {
    let canvas = view.framing.viewport();
    let drawn = view.drawn().and_then(|drawn| {
        let shown = drawn.view(view.view_zoom(&drawn)?)?;
        shown.boxes.iter().find(|b| b.id == node).map(Rect::of)
    });
    let Some(rect) = drawn else {
        return canvas.center();
    };
    let foot = to_screen(rect.x, rect.y + rect.h, canvas, view.camera());
    point(
        foot.x.max(canvas.left()).min(canvas.right()),
        foot.y.max(canvas.top()).min(canvas.bottom()),
    )
}

/// The rows of the menu of `node`.
fn items(view: &GraphPanel, node: Id, cx: &Context<GraphPanel>) -> Vec<MenuItem> {
    let panel = cx.weak_entity();
    offered(why::asking_about_another(view, node))
        .into_iter()
        .map(|offer| match offer {
            Offer::Highlight => {
                MenuItem::submenu(offer.label(), highlights(&panel, view.selection.relation()))
            }
            Offer::AlwaysHide => MenuItem::new(
                offer.label(),
                deed(&panel, move |view, cx| {
                    pins::set(view, node, Visibility::Hidden, cx)
                }),
            ),
            Offer::AskWhy => MenuItem::new(
                offer.label(),
                deed(&panel, move |view, cx| why::ask_why(view, node, cx)),
            ),
            Offer::AskFrom => MenuItem::new(
                offer.label(),
                deed(&panel, move |view, cx| why::ask_from(view, node, cx)),
            ),
        })
        .collect()
}

/// The rows choosing what a selected box marks, `standing` checked.
fn highlights(panel: &WeakEntity<GraphPanel>, standing: Relation) -> Vec<SubmenuItem> {
    Relation::ALL
        .into_iter()
        .map(|relation| {
            SubmenuItem::new(
                relation.label(),
                deed(panel, move |view, cx| view.set_relation(relation, cx)),
            )
            .checked(relation == standing)
        })
        .collect()
}

/// What choosing a row doing `act` to the panel runs.
fn deed(
    panel: &WeakEntity<GraphPanel>,
    act: impl Fn(&mut GraphPanel, &mut Context<GraphPanel>) + 'static,
) -> impl Fn(&mut Window, &mut App) + 'static {
    let panel = panel.clone();
    move |_, cx| {
        panel.update(cx, |view, cx| act(view, cx)).ok();
    }
}

/// The menu standing over a box, while one does.
pub(crate) fn menu(view: &GraphPanel, window: &Window) -> Option<impl IntoElement> {
    let open = view.box_menu.as_ref()?;
    Some(over_window(&open.menu, window))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_box_is_offered_its_highlight_its_pin_and_why_it_is_in_the_build() {
        assert_eq!(
            offered(false),
            [Offer::Highlight, Offer::AlwaysHide, Offer::AskWhy]
        );
    }

    /// Narrowing to the paths from a box narrows a question about another, so
    /// it is offered only while one is open.
    #[test]
    fn the_paths_from_a_box_are_offered_while_another_is_asked_about() {
        assert_eq!(offered(true).last(), Some(&Offer::AskFrom));
    }
}
