//! The application shell: window bootstrap, client-side decorations, lifecycle,
//! and the global interactivity (quit/close/tab) shared by every app window.

mod chrome;
pub use chrome::Chrome;

use gpui::{
    App, Application, Bounds, KeyBinding, Pixels, Render, Size, TitlebarOptions, WindowBounds,
    WindowDecorations, WindowOptions, actions, px,
};

actions!(ui_shell, [Quit, CloseWindow, Tab, TabPrev]);

/// The smallest the window may be resized to, so its chrome and content can't be
/// crushed to nothing. Smaller than every app's opening size.
const MIN_WINDOW_SIZE: Size<Pixels> = Size {
    width: px(480.0),
    height: px(360.0),
};

/// The application builder with the bundled asset source installed.
pub fn application() -> Application {
    gpui_platform::application().with_assets(bviz_ui_assets::Assets)
}

/// Register fonts, the themes, global key bindings, and quit-on-last-window-close.
/// Call once at startup before opening windows.
pub fn setup(cx: &mut App) {
    bviz_ui_assets::load_fonts(cx);
    bviz_ui_theme::init(cx).expect("the built-in themes load");
    cx.bind_keys([
        KeyBinding::new("secondary-q", Quit, None),
        KeyBinding::new("secondary-w", CloseWindow, None),
        KeyBinding::new("tab", Tab, None),
        KeyBinding::new("shift-tab", TabPrev, None),
    ]);
    bviz_ui_components::register_keys(cx);
    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
}

/// Run an app: install the asset source, run `setup`, and open a single decorated
/// floating window of `window_size` whose root view comes from `build_root`. Wrap
/// the view's body in [`Chrome`] to get the decorations.
pub fn run<V, F>(app_id: impl Into<String>, window_size: Size<Pixels>, build_root: F)
where
    V: 'static + Render,
    F: 'static + FnOnce(&mut gpui::Window, &mut App) -> gpui::Entity<V>,
{
    let app_id = app_id.into();
    application().run(move |cx: &mut App| {
        setup(cx);
        // A floating window of the requested size, centered on the active display.
        // The size is absolute rather than a fraction of the resolution, so it
        // works even on Wayland, where the display list is still empty at startup.
        let bounds = Bounds::centered(None, window_size, cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(app_id.clone().into()),
                    ..Default::default()
                }),
                app_id: Some(app_id.clone()),
                window_min_size: Some(MIN_WINDOW_SIZE),
                // Prefer the compositor's decorations; Chrome draws our own when
                // only client-side is supported (e.g. GNOME).
                window_decorations: Some(WindowDecorations::Server),
                ..Default::default()
            },
            |window, cx| {
                window
                    .observe_window_appearance(|window, _cx| window.refresh())
                    .detach();
                build_root(window, cx)
            },
        )
        .unwrap();
        cx.activate(true);
    });
}
