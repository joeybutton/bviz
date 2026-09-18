//! The window's root: a [`Workspace`] framing an arrangement of [`Pane`]s in the
//! application chrome, the panels ([`AnyPanel`]) each pane holds and shows one
//! of, the registry of what every window's panels show views of and the menu
//! listing it, and [`run_panels`] for an app whose window frames them.

mod documents;
mod documents_menu;
mod drop;
mod group;
mod layout;
mod pane;
mod panel;
mod workspace;

pub use documents::ViewSpec;
pub(crate) use documents::{DocumentId, Documents, OpenDocument, ViewState};
pub use group::{PanelGroup, Weight};
pub use layout::{AtEdge, DividerId, Layout, Panes, Removal, Side, Split, SplitSide};
pub use pane::{Pane, PaneEvent};
pub use panel::{AnyPanel, Document, Panel, PanelEvent, PanelKind, ViewKind, document_label};
pub use workspace::Workspace;

use workspace::register_keys;

use gpui::{App, AppContext, Pixels, Size, Window};

/// Runs an app whose one window frames the [`Document`]s `open` builds from
/// `first` and then `rest`, stacked top to bottom in that order. Each of
/// `panel_keys` registers the bindings of one panel kind the app can show, and
/// runs before the first window opens. Each runs exactly once per process:
/// `cx.bind_keys` refreshes every window, which drops what each has cached of
/// its last frame.
pub fn run_panels<T, F>(
    app_id: impl Into<String>,
    window_size: Size<Pixels>,
    first: T,
    rest: Vec<T>,
    open: F,
    panel_keys: &'static [fn(&mut App)],
) where
    T: 'static,
    F: 'static + Fn(T, &mut Window, &mut App) -> Document,
{
    let app_id = app_id.into();
    bviz_ui_shell::application().run(move |cx: &mut App| {
        bviz_ui_shell::setup(cx);
        register_keys(cx);
        for register in panel_keys {
            register(cx);
        }
        let (first, rest) = (first, rest);
        bviz_ui_shell::open_window(&app_id, window_size, cx, |window, cx| {
            let first = open(first, window, cx);
            let mut others = Vec::with_capacity(rest.len());
            for document in rest {
                others.push(open(document, window, cx));
            }
            cx.new(|cx| Workspace::with_documents(app_id.clone(), first, others, window, cx))
        })
        .unwrap();
        cx.activate(true);
    });
}
