use crate::{Documents, Pane, PanelGroup, ViewSpec};
use gpui::{
    AnyView, App, Context, Entity, EntityId, EventEmitter, FocusHandle, Focusable, Render,
    SharedString, Subscription, Window,
};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::iter;
use std::num::NonZeroUsize;
use std::path::Path;
use std::rc::Rc;

/// How a path reads as a document's label: its file name, or the whole path
/// when it has none.
pub fn document_label(path: &Path) -> SharedString {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
        .into()
}

/// Which of a document's views a panel shows — its timeline, its flame graph,
/// its stats table. Distinct across panel types, so a view of an open document
/// can be named whether or not a panel showing it exists.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ViewKind(&'static str);

impl ViewKind {
    pub const fn new(name: &'static str) -> Self {
        Self(name)
    }

    pub fn as_str(self) -> &'static str {
        self.0
    }
}

/// Stable identity of a panel type.
#[derive(Clone, Copy)]
pub struct PanelKind {
    pub view: ViewKind,
    /// The name the view goes by among the views its document can show.
    pub label: &'static str,
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
pub struct AnyPanel {
    panel: Rc<dyn PanelOps>,
    /// Fixed for the panel's lifetime, so a panel keeps its number when it moves
    /// to another pane or another window.
    ordinal: Option<NonZeroUsize>,
}

impl AnyPanel {
    pub(crate) fn new<T: Panel>(panel: Entity<T>) -> Self {
        Self {
            panel: Rc::new(panel),
            ordinal: None,
        }
    }

    pub fn view_kind(&self) -> ViewKind {
        self.panel.view_kind()
    }

    /// The title the panel gives itself.
    pub fn plain_title(&self, cx: &App) -> SharedString {
        self.panel.title(cx)
    }

    /// The number following the plain title, if the panel's document has one.
    pub fn title_suffix(&self) -> Option<SharedString> {
        self.ordinal.map(ordinal_suffix)
    }

    /// The plain title followed by its suffix, for a place that shows the title
    /// as one string.
    pub fn display_title(&self, cx: &App) -> SharedString {
        match self.title_suffix() {
            Some(suffix) => format!("{}{suffix}", self.plain_title(cx)).into(),
            None => self.plain_title(cx),
        }
    }

    pub fn status(&self, cx: &App) -> Option<SharedString> {
        self.panel.status(cx)
    }

    pub fn icon(&self, cx: &App) -> Option<&'static str> {
        self.panel.icon(cx)
    }

    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.panel.focus_handle(cx)
    }

    pub fn set_active(&self, active: bool, window: &mut Window, cx: &mut App) {
        self.panel.set_active(active, window, cx)
    }

    pub fn entity_id(&self) -> EntityId {
        self.panel.view().entity_id()
    }

    /// Calls `released` once this panel is dropped, which is the last pane
    /// holding it letting go: a pane is a panel's only owner.
    pub fn on_release(&self, cx: &App, released: impl FnOnce(&mut App) + 'static) -> Subscription {
        self.panel.on_release(cx, Box::new(released))
    }

    /// Marks the panel for a repaint of its own, which its pane owes it after
    /// frames that left it out.
    pub fn notify(&self, cx: &mut App) {
        self.panel.notify(cx)
    }

    pub fn view(&self) -> AnyView {
        self.panel.view()
    }

    /// Forward this panel's [`PanelEvent`]s to the pane `cx` belongs to.
    pub fn subscribe(&self, window: &Window, cx: &mut Context<Pane>) -> Subscription {
        self.panel.subscribe(window, cx)
    }
}

/// A document as a viewer opens it into a window. Its label tells it apart from
/// the other documents the window opens with: every panel of a document whose
/// label another shares is numbered, counting in the order the window stacks the
/// documents.
pub struct Document {
    label: SharedString,
    views: PanelGroup<ViewSpec>,
}

impl Document {
    pub fn new(label: SharedString, views: PanelGroup<ViewSpec>) -> Self {
        Self { label, views }
    }

    /// The panels of `first` and `others`, each document registered with the
    /// session's [`Documents`], stacked as [`PanelGroup::stack`] stacks them, and
    /// numbered.
    pub(crate) fn stack(first: Self, others: Vec<Self>, cx: &mut App) -> PanelGroup {
        let labels: Vec<SharedString> = iter::once(&first)
            .chain(&others)
            .map(|document| document.label.clone())
            .collect();
        let ordinals = shared_label_ordinals(&labels);
        Documents::global(cx).update(cx, |documents, cx| {
            let mut open = |document: Self, ordinal| {
                let mut panels = documents.open(document.label, document.views, cx);
                for panel in panels.panels_mut() {
                    panel.ordinal = ordinal;
                }
                panels
            };
            let first = open(first, ordinals[0]);
            let others = others
                .into_iter()
                .zip(&ordinals[1..])
                .map(|(document, &ordinal)| open(document, ordinal))
                .collect();
            PanelGroup::stack(first, others)
        })
    }
}

/// The ordinal of each of `labels` within the labels equal to it, or `None` for
/// a label no other one equals.
fn shared_label_ordinals(labels: &[SharedString]) -> Vec<Option<NonZeroUsize>> {
    let mut ordinals = vec![None; labels.len()];
    // Each label's first index, and how many of it have been seen.
    let mut groups: HashMap<&SharedString, (usize, NonZeroUsize)> =
        HashMap::with_capacity(labels.len());
    for (index, label) in labels.iter().enumerate() {
        match groups.entry(label) {
            Entry::Vacant(group) => {
                group.insert((index, NonZeroUsize::MIN));
            }
            Entry::Occupied(mut group) => {
                let (first, seen) = group.get_mut();
                ordinals[*first] = Some(NonZeroUsize::MIN);
                *seen = seen.saturating_add(1);
                ordinals[index] = Some(*seen);
            }
        }
    }
    ordinals
}

fn ordinal_suffix(ordinal: NonZeroUsize) -> SharedString {
    format!(" ({ordinal})").into()
}

/// [`Panel`] with its type parameter discharged, so a workspace can hold panels
/// of differing kinds. Implemented once, for the entity holding the panel.
trait PanelOps {
    fn view_kind(&self) -> ViewKind;
    fn title(&self, cx: &App) -> SharedString;
    fn status(&self, cx: &App) -> Option<SharedString>;
    fn icon(&self, cx: &App) -> Option<&'static str>;
    fn focus_handle(&self, cx: &App) -> FocusHandle;
    fn set_active(&self, active: bool, window: &mut Window, cx: &mut App);
    fn notify(&self, cx: &mut App);
    fn on_release(&self, cx: &App, released: Box<dyn FnOnce(&mut App)>) -> Subscription;
    fn view(&self) -> AnyView;
    fn subscribe(&self, window: &Window, cx: &mut Context<Pane>) -> Subscription;
}

impl<T: Panel> PanelOps for Entity<T> {
    fn view_kind(&self) -> ViewKind {
        T::kind().view
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

    fn on_release(&self, cx: &App, released: Box<dyn FnOnce(&mut App)>) -> Subscription {
        cx.observe_release(self, move |_, cx| released(cx))
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

    fn ordinals(labels: &[&'static str]) -> Vec<Option<usize>> {
        let labels: Vec<SharedString> = labels.iter().copied().map(SharedString::from).collect();
        shared_label_ordinals(&labels)
            .into_iter()
            .map(|ordinal| ordinal.map(NonZeroUsize::get))
            .collect()
    }

    #[test]
    fn distinct_labels_are_not_numbered() {
        assert_eq!(ordinals(&[]), []);
        assert_eq!(ordinals(&["a.bep", "b.bep"]), [None, None]);
    }

    #[test]
    fn a_shared_label_numbers_every_document_bearing_it_from_one() {
        assert_eq!(ordinals(&["x", "x"]), [Some(1), Some(2)]);
        assert_eq!(
            ordinals(&["x", "y", "x", "x"]),
            [Some(1), None, Some(2), Some(3)]
        );
    }

    #[test]
    fn each_shared_label_counts_on_its_own() {
        assert_eq!(
            ordinals(&["x", "y", "y", "x", "z"]),
            [Some(1), Some(1), Some(2), Some(2), None]
        );
    }

    #[test]
    fn an_ordinal_reads_as_a_parenthesized_suffix() {
        assert_eq!(ordinal_suffix(NonZeroUsize::MIN), " (1)");
        assert_eq!(
            NonZeroUsize::new(12).map(ordinal_suffix),
            Some(" (12)".into())
        );
    }
}
