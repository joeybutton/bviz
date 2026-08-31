//! The window's root: a [`Workspace`] framing an arrangement of [`Pane`]s in the
//! application chrome, the panels ([`AnyPanel`]) each pane holds and shows one
//! of, and [`run`] for an app whose window is one of them.

mod layout;
mod pane;
mod panel;
mod workspace;

pub use layout::{Layout, Panes, Removal, Split, SplitId};
pub use pane::{Pane, PaneEvent};
pub use panel::{AnyPanel, Panel, PanelEvent, PanelKind};
pub use workspace::{Workspace, register_keys};

use gpui::{App, AppContext, Pixels, Size, Window};

/// Runs an app whose one window frames the panel `build_panel` makes: the
/// application shell's own setup, this crate's key bindings, and the window.
pub fn run<F>(app_id: impl Into<String>, window_size: Size<Pixels>, build_panel: F)
where
    F: 'static + FnOnce(&mut Window, &mut App) -> AnyPanel,
{
    let app_id = app_id.into();
    bviz_ui_shell::application().run(move |cx: &mut App| {
        bviz_ui_shell::setup(cx);
        register_keys(cx);
        bviz_ui_shell::open_window(&app_id, window_size, cx, |window, cx| {
            let panel = build_panel(window, cx);
            cx.new(|cx| Workspace::new(panel, window, cx))
        })
        .unwrap();
        cx.activate(true);
    });
}
