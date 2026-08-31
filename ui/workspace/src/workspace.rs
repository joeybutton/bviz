use crate::{AnyPanel, PanelEvent};
use bviz_ui_shell::Chrome;
use gpui::{App, Context, FocusHandle, IntoElement, Render, Subscription, Window};

/// A window's root view: the application chrome, and the panel it frames.
pub struct Workspace {
    /// Focus for the chrome, so the shell's window actions (quit, close, tab)
    /// dispatch here; the ancestor of the panel's own focus.
    window_focus: FocusHandle,
    panel: AnyPanel,
    _panel_events: Subscription,
}

impl Workspace {
    pub fn new(panel: AnyPanel, window: &mut Window, cx: &mut Context<Self>) -> Self {
        set_window_title(&panel, window, cx);
        let panel_events = panel.subscribe(window, cx);
        panel.set_active(true, window, cx);
        Self {
            window_focus: cx.focus_handle(),
            panel,
            _panel_events: panel_events,
        }
    }

    pub fn active_panel(&self) -> &AnyPanel {
        &self.panel
    }

    pub(crate) fn on_panel_event(
        &mut self,
        event: &PanelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            PanelEvent::TitleChanged => {
                set_window_title(&self.panel, window, cx);
                cx.notify();
            }
            PanelEvent::StatusChanged => cx.notify(),
            PanelEvent::Close => window.remove_window(),
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let chrome = Chrome::new(&self.window_focus)
            .title(self.panel.title(cx))
            .child(self.panel.view());
        match self.panel.status(cx) {
            Some(status) => chrome.status(status),
            None => chrome,
        }
    }
}

/// Retitles the OS window, which under server-side decorations is what the
/// compositor's own title bar shows.
fn set_window_title(panel: &AnyPanel, window: &mut Window, cx: &App) {
    window.set_window_title(&panel.title(cx));
}
