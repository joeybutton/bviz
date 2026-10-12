//! The menu of what can be done with a box of the graph, which a right press
//! on the box puts up, and a key for the selected box.

use crate::camera::{Rect, to_screen};
use crate::relation::Relation;
use crate::{GraphPanel, derive, pins, selection, why};
use bviz_graph_layout::Id;
use bviz_graph_layout::select::Visibility;
use bviz_ui_components::{Menu, MenuEvent, MenuItem, SubmenuItem, over_window};
use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, Pixels, Point, SharedString, Subscription,
    WeakEntity, Window, point,
};

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
    /// The node of the graph the rows act on, which the document holds
    /// selected while the menu stands.
    node: Id,
    menu: Entity<Menu>,
    _dismissed: Subscription,
}

/// Selects the node the box `hit` stands for, as [`derive::member`] names
/// it, and puts up the menu of what can be done with it at `at`.
pub(crate) fn open(
    view: &mut GraphPanel,
    hit: Id,
    at: Point<Pixels>,
    window: &mut Window,
    cx: &mut Context<GraphPanel>,
) {
    let node = derive::member(view, hit);
    selection::select(view, Some(node), cx);
    let gathered = derive::gathered(view, hit);
    put_up(view, node, gathered, at, window, cx);
}

/// Puts up at `at` the menu of what can be done with `node`, or with the
/// nodes `gathered` where the box is a merge group's.
fn put_up(
    view: &mut GraphPanel,
    node: Id,
    gathered: Vec<Id>,
    at: Point<Pixels>,
    window: &mut Window,
    cx: &mut Context<GraphPanel>,
) {
    // The card stands beside the pointer, where the menu opens.
    view.box_card.forget(cx);
    let items = items(view, node, gathered, cx);
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

/// Takes the menu down where the panel no longer acts on its node, as
/// [`selection::acted_on`] names what it acts on.
pub(crate) fn remarked(view: &GraphPanel, cx: &mut App) {
    if let Some(open) = &view.box_menu
        && selection::acted_on(view, cx) != Some(open.node)
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

/// Puts up the menu of the node the panel acts on, where it acts on one, at
/// the foot of the box marked for it.
pub(crate) fn open_selected(
    view: &mut GraphPanel,
    window: &mut Window,
    cx: &mut Context<GraphPanel>,
) {
    let (Some(node), Some(marked)) = (selection::acted_on(view, cx), view.selection.node()) else {
        return;
    };
    let at = foot(view, marked);
    let gathered = derive::gathered(view, marked);
    put_up(view, node, gathered, at, window, cx);
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

/// The rows of the menu of `node`, or of a merge group's box gathering the
/// nodes `gathered`, which stands for all of them: hiding it pins every one,
/// and the questions, each about a single node, wait for the group to be
/// opened.
fn items(
    view: &GraphPanel,
    node: Id,
    gathered: Vec<Id>,
    cx: &Context<GraphPanel>,
) -> Vec<MenuItem> {
    let panel = cx.weak_entity();
    let group = gathered.len();
    let hidden = match gathered.is_empty() {
        true => vec![node],
        false => gathered,
    };
    let single = |row: MenuItem| match group {
        0 => row,
        _ => row.disabled().tooltip(format!(
            "Open the group to ask about one of its {group} nodes"
        )),
    };
    offered(why::asking_about_another(view, node))
        .into_iter()
        .map(|offer| match offer {
            Offer::Highlight => {
                MenuItem::submenu(offer.label(), highlights(&panel, view.selection.relation()))
            }
            Offer::AlwaysHide => {
                let hidden = hidden.clone();
                MenuItem::new(
                    offer.label(),
                    deed(&panel, move |view, cx| pins::hide(view, &hidden, cx)),
                )
            }
            Offer::AskWhy => single(MenuItem::new(
                offer.label(),
                deed(&panel, move |view, cx| why::ask_why(view, node, cx)),
            )),
            Offer::AskFrom => single(MenuItem::new(
                offer.label(),
                deed(&panel, move |view, cx| why::ask_from(view, node, cx)),
            )),
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
