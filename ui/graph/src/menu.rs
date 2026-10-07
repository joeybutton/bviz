use bviz_ui_components::Menu;
use gpui::{Entity, IntoElement, Window, anchored, deferred, div, point, prelude::*, px};

/// The priority a menu is deferred at, above that of any other deferred
/// element a graph view draws.
const MENU_PRIORITY: usize = 1;

/// `menu` standing over the whole window, drawn above whatever else the
/// window draws.
pub(crate) fn over_window(menu: &Entity<Menu>, window: &Window) -> impl IntoElement {
    let viewport = window.viewport_size();
    let whole = div()
        .w(viewport.width)
        .h(viewport.height)
        .child(menu.clone());
    let origin = point(px(0.), px(0.));
    deferred(anchored().position(origin).child(whole)).with_priority(MENU_PRIORITY)
}
