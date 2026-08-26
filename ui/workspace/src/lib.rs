//! The window's root: a [`Workspace`] framing an arrangement of [`Pane`]s in the
//! application chrome, the panels ([`AnyPanel`]) each pane holds and shows one
//! of, and [`run`] for an app whose window frames them.

mod layout;
mod pane;
mod panel;
mod workspace;

pub use layout::{Layout, Panes, Removal, Split, SplitId};
pub use pane::{Pane, PaneEvent};
pub use panel::{AnyPanel, Panel, PanelEvent, PanelKind, document_label};
pub use workspace::{Workspace, register_keys};

use gpui::{App, AppContext, Pixels, Size, Window};

/// Runs an app whose one window frames the panel `build_panel` makes.
pub fn run<F>(
    app_id: impl Into<String>,
    window_size: Size<Pixels>,
    build_panel: F,
    panel_keys: &'static [fn(&mut App)],
) where
    F: 'static + FnOnce(&mut Window, &mut App) -> AnyPanel,
{
    run_with(app_id, window_size, panel_keys, |window, cx| {
        (build_panel(window, cx), Vec::new())
    });
}

/// Runs an app whose one window frames a panel per document: `first` on show
/// and each of `rest` tabbed behind it, every one of them built by `open`.
pub fn run_panels<T, F>(
    app_id: impl Into<String>,
    window_size: Size<Pixels>,
    first: T,
    rest: Vec<T>,
    open: F,
    panel_keys: &'static [fn(&mut App)],
) where
    T: 'static,
    F: 'static + Fn(T, &mut Window, &mut App) -> AnyPanel,
{
    run_with(app_id, window_size, panel_keys, move |window, cx| {
        let panel = open(first, window, cx);
        let mut others = Vec::with_capacity(rest.len());
        for document in rest {
            others.push(open(document, window, cx));
        }
        (panel, others)
    });
}

/// The application shell's own setup, this crate's key bindings, one call per
/// entry in `panel_keys` to register the bindings of one panel kind the app can
/// show, and the window the panels `build_panels` makes are framed in. The
/// registrations run before the first window opens.
fn run_with<F>(
    app_id: impl Into<String>,
    window_size: Size<Pixels>,
    panel_keys: &'static [fn(&mut App)],
    build_panels: F,
) where
    F: 'static + FnOnce(&mut Window, &mut App) -> (AnyPanel, Vec<AnyPanel>),
{
    let app_id = app_id.into();
    bviz_ui_shell::application().run(move |cx: &mut App| {
        bviz_ui_shell::setup(cx);
        register_keys(cx);
        for register in panel_keys {
            register(cx);
        }
        bviz_ui_shell::open_window(&app_id, window_size, cx, |window, cx| {
            let (panel, others) = build_panels(window, cx);
            cx.new(|cx| Workspace::with_panels(app_id.clone(), panel, others, window, cx))
        })
        .unwrap();
        cx.activate(true);
    });
}
