use crate::Pane;
use gpui::{
    AnyView, App, Context, Entity, EntityId, EventEmitter, FocusHandle, Focusable, Render,
    SharedString, Subscription, Window,
};
use std::path::Path;
use std::rc::Rc;

/// How a path reads as a panel title: its file name, or the whole path when it
/// has none.
pub fn document_label(path: &Path) -> SharedString {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
        .into()
}

/// Stable identity of a panel type.
#[derive(Clone, Copy)]
pub struct PanelKind {
    pub name: &'static str,
    /// The gpui key context this panel's own key bindings are scoped to; unique
    /// across kinds, so such a binding reaches only this panel.
    pub key_context: &'static str,
}

/// A view a [`Workspace`] can host. It renders its own body; the workspace draws
/// the window chrome around it from the title, status and icon asked for here.
pub trait Panel: Render + Focusable + EventEmitter<PanelEvent> {
    fn kind() -> PanelKind
    where
        Self: Sized;

    fn title(&self, cx: &App) -> SharedString;

    fn status(&self, _cx: &App) -> Option<SharedString> {
        None
    }

    fn icon(&self) -> Option<&'static str> {
        None
    }

    /// Called when the workspace starts or stops showing this panel. Focus is
    /// taken here rather than by the panel's constructor, so building a panel
    /// the workspace does not go on to show leaves focus where it was.
    fn set_active(&mut self, _active: bool, _window: &mut Window, _cx: &mut Context<Self>) {}
}

/// What a [`Panel`] tells its workspace about itself.
pub enum PanelEvent {
    TitleChanged,
    StatusChanged,
    Close,
}

/// A panel of any kind, holding it through operations resolved once for its
/// concrete type. Cloning shares the one panel.
#[derive(Clone)]
pub struct AnyPanel(Rc<dyn PanelOps>);

impl AnyPanel {
    pub fn new<T: Panel>(panel: Entity<T>) -> Self {
        Self(Rc::new(panel))
    }

    pub fn kind(&self) -> PanelKind {
        self.0.kind()
    }

    pub fn title(&self, cx: &App) -> SharedString {
        self.0.title(cx)
    }

    pub fn status(&self, cx: &App) -> Option<SharedString> {
        self.0.status(cx)
    }

    pub fn icon(&self, cx: &App) -> Option<&'static str> {
        self.0.icon(cx)
    }

    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.0.focus_handle(cx)
    }

    pub fn set_active(&self, active: bool, window: &mut Window, cx: &mut App) {
        self.0.set_active(active, window, cx)
    }

    pub fn entity_id(&self) -> EntityId {
        self.0.view().entity_id()
    }

    /// Marks the panel for a repaint of its own, which its pane owes it after
    /// frames that left it out.
    pub fn notify(&self, cx: &mut App) {
        self.0.notify(cx)
    }

    pub fn view(&self) -> AnyView {
        self.0.view()
    }

    /// Forward this panel's [`PanelEvent`]s to the pane `cx` belongs to.
    pub fn subscribe(&self, window: &Window, cx: &mut Context<Pane>) -> Subscription {
        self.0.subscribe(window, cx)
    }
}

/// [`Panel`] with its type parameter discharged, so a workspace can hold panels
/// of differing kinds. Implemented once, for the entity holding the panel.
trait PanelOps {
    fn kind(&self) -> PanelKind;
    fn title(&self, cx: &App) -> SharedString;
    fn status(&self, cx: &App) -> Option<SharedString>;
    fn icon(&self, cx: &App) -> Option<&'static str>;
    fn focus_handle(&self, cx: &App) -> FocusHandle;
    fn set_active(&self, active: bool, window: &mut Window, cx: &mut App);
    fn notify(&self, cx: &mut App);
    fn view(&self) -> AnyView;
    fn subscribe(&self, window: &Window, cx: &mut Context<Pane>) -> Subscription;
}

impl<T: Panel> PanelOps for Entity<T> {
    fn kind(&self) -> PanelKind {
        T::kind()
    }

    fn title(&self, cx: &App) -> SharedString {
        self.read(cx).title(cx)
    }

    fn status(&self, cx: &App) -> Option<SharedString> {
        self.read(cx).status(cx)
    }

    fn icon(&self, cx: &App) -> Option<&'static str> {
        self.read(cx).icon()
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.read(cx).focus_handle(cx)
    }

    fn set_active(&self, active: bool, window: &mut Window, cx: &mut App) {
        self.update(cx, |panel, cx| panel.set_active(active, window, cx));
    }

    fn notify(&self, cx: &mut App) {
        self.update(cx, |_, cx| cx.notify());
    }

    fn view(&self) -> AnyView {
        self.clone().into()
    }

    fn subscribe(&self, window: &Window, cx: &mut Context<Pane>) -> Subscription {
        cx.subscribe_in(self, window, |pane, panel, event, window, cx| {
            pane.on_panel_event(panel.entity_id(), event, window, cx);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_is_labelled_by_its_file_name() {
        assert_eq!(
            document_label(Path::new("/tmp/build.profile.gz")),
            "build.profile.gz"
        );
        assert_eq!(document_label(Path::new("aquery.pb")), "aquery.pb");
    }

    /// A path ending in `..` or in the root has no file name, and reads whole.
    #[test]
    fn a_path_with_no_file_name_is_its_own_label() {
        assert_eq!(document_label(Path::new("/tmp/..")), "/tmp/..");
        assert_eq!(document_label(Path::new("/")), "/");
    }
}
