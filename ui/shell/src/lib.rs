//! The application shell: window bootstrap, client-side decorations, lifecycle,
//! and the global interactivity (quit/close/tab) shared by every app window.

mod chrome;
pub use chrome::Chrome;

use gpui::{
    App, Application, Bounds, Entity, KeyBinding, Pixels, Render, Result, Size, TitlebarOptions,
    Window, WindowBounds, WindowDecorations, WindowHandle, WindowOptions, actions, px,
};

actions!(bviz_window, [Quit, CloseWindow, Tab, TabPrev]);

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

/// Open a decorated floating window of `window_size` whose root view comes from
/// `build_root`. Wrap the view's body in [`Chrome`] to get the decorations.
/// Callable any number of times once [`setup`] has run.
pub fn open_window<V, F>(
    app_id: &str,
    window_size: Size<Pixels>,
    cx: &mut App,
    build_root: F,
) -> Result<WindowHandle<V>>
where
    V: 'static + Render,
    F: FnOnce(&mut Window, &mut App) -> Entity<V>,
{
    // Centered on the active display, at an absolute size rather than a fraction
    // of the resolution, so it works even on Wayland, where the display list is
    // still empty at startup.
    let bounds = Bounds::centered(None, window_size, cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(app_id.to_string().into()),
                ..Default::default()
            }),
            app_id: Some(app_id.to_string()),
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
}
