use crate::{AnyPanel, Panel, ViewKind, Workspace};
use gpui::{
    App, AppContext, Context, Entity, EntityId, Global, SharedString, Subscription, WeakEntity,
    Window,
};
use std::rc::Rc;

/// Identity of a document the session holds open. Two readings of one file are
/// two documents, each with views of its own.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DocumentId(u64);

/// Builds a panel showing one view of a document. Takes no window, so a view can
/// be minted wherever the reader asks for it.
type Mint = Rc<dyn Fn(&mut App) -> AnyPanel>;

/// One view a document can show, and how to build a panel showing it.
pub struct ViewSpec {
    kind: ViewKind,
    mint: Mint,
}

impl ViewSpec {
    /// The view panels of type `T` show, `mint` building one over whatever the
    /// document holds its content in.
    pub fn new<T: Panel>(mint: impl Fn(&mut App) -> Entity<T> + 'static) -> Self {
        Self {
            kind: T::kind().view,
            mint: Rc::new(move |cx| AnyPanel::new(mint(cx))),
        }
    }
}

/// What a document's view has on screen. A pane draws only the panel it shows,
/// so a view can have a panel that no frame draws.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewState {
    /// No panel shows this view.
    Closed,
    /// A pane holds a panel showing this view, behind the panel that pane shows.
    Open,
    /// The pane holding this view's panel shows it.
    Visible,
}

/// A document of the session, read out of the registry for a reader choosing
/// among what is open.
pub struct OpenDocument {
    pub id: DocumentId,
    pub label: SharedString,
    pub views: Vec<OpenView>,
}

pub struct OpenView {
    pub kind: ViewKind,
    pub state: ViewState,
}

/// The documents a session holds open and the views each can show, so a view
/// that closed with its panel can be shown again over the document it was
/// showing. One registry serves the whole application: a document outlives the
/// panels showing it, and its views may be asked for from any window.
pub struct Documents {
    next: u64,
    documents: Vec<Registered>,
    /// The workspace framing each open window, weakly held so one whose window
    /// has closed drops out rather than being re-shown into.
    windows: Vec<WeakEntity<Workspace>>,
}

struct Registered {
    id: DocumentId,
    label: SharedString,
    views: Vec<View>,
}

struct View {
    kind: ViewKind,
    mint: Mint,
    live: Option<Live>,
}

/// The panel showing a view, named by its identity alone: a pane is a panel's
/// only owner, so a handle held here would keep a closed view alive for the life
/// of the session. The subscription clears this record when that panel is
/// dropped, and is the one writer that clears it, so no close path can leave the
/// registry claiming a view that no pane holds.
struct Live {
    panel: EntityId,
    _released: Subscription,
}

struct GlobalDocuments(Entity<Documents>);

impl Global for GlobalDocuments {}

impl Documents {
    /// The session's registry, opened on first use.
    pub fn global(cx: &mut App) -> Entity<Self> {
        if cx.try_global::<GlobalDocuments>().is_none() {
            let documents = cx.new(|_| Self::new());
            cx.set_global(GlobalDocuments(documents));
        }
        cx.global::<GlobalDocuments>().0.clone()
    }

    fn new() -> Self {
        Self {
            next: 0,
            documents: Vec::new(),
            windows: Vec::new(),
        }
    }

    /// Registers a document labelled `label` showing `views`, and mints a panel
    /// for each in the order given. The caller arranges those panels in a
    /// window; the registry keeps only what it takes to mint them again.
    pub fn open<const N: usize>(
        &mut self,
        label: impl Into<SharedString>,
        views: [ViewSpec; N],
        cx: &mut Context<Self>,
    ) -> (DocumentId, [AnyPanel; N]) {
        let panels = views.each_ref().map(|view| (view.mint)(cx));
        let id = self.register(label.into(), Vec::from(views));
        for panel in &panels {
            self.watch(id, panel.view_kind(), panel, cx);
        }
        cx.notify();
        (id, panels)
    }

    /// Shows `view` of `document` in `workspace`: the panel already showing it,
    /// moved here out of whatever window holds it, or a fresh panel where none
    /// is open. A reopened view comes back as its document stands, not as its
    /// last panel had been scrolled or zoomed.
    pub fn show(
        &mut self,
        document: DocumentId,
        view: ViewKind,
        workspace: &Entity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(found) = self.view(document, view) else {
            return;
        };
        let live = found.live.as_ref().map(|live| live.panel);
        let mint = found.mint.clone();
        match live {
            Some(panel) => self.reveal(panel, workspace, window, cx),
            None => {
                let panel = mint(cx);
                self.watch(document, view, &panel, cx);
                workspace.update(cx, |workspace, cx| {
                    workspace.add_panel(panel, window, cx);
                });
                cx.notify();
            }
        }
    }

    /// Which panel shows `view` of `document`, if one does.
    pub fn panel(&self, document: DocumentId, view: ViewKind) -> Option<EntityId> {
        self.view(document, view)?
            .live
            .as_ref()
            .map(|live| live.panel)
    }

    /// Every document the session holds, in the order they opened, each with
    /// what every view it can show has on screen.
    pub fn documents(&self, cx: &App) -> Vec<OpenDocument> {
        let workspaces = self.workspaces();
        let mut open = Vec::with_capacity(self.documents.len());
        for document in &self.documents {
            let mut views = Vec::with_capacity(document.views.len());
            for view in &document.views {
                views.push(OpenView {
                    kind: view.kind,
                    state: state(view, &workspaces, cx),
                });
            }
            open.push(OpenDocument {
                id: document.id,
                label: document.label.clone(),
                views,
            });
        }
        open
    }

    /// Takes `workspace` as a place a view can be shown, for as long as the
    /// window it frames is open.
    pub(crate) fn watch_workspace(&mut self, workspace: WeakEntity<Workspace>) {
        self.windows.retain(|window| window.upgrade().is_some());
        self.windows.push(workspace);
    }

    /// Brings `panel` forward where `into` holds it, and otherwise moves it
    /// there out of the window that does: a panel is rendered in one window at a
    /// time, and Wayland offers no way to raise the window it sits in.
    fn reveal(
        &self,
        panel: EntityId,
        into: &Entity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if into.update(cx, |workspace, cx| workspace.reveal(panel, window, cx)) {
            return;
        }
        for elsewhere in self.workspaces() {
            if elsewhere == *into {
                continue;
            }
            let handle = elsewhere.read(cx).window();
            let taken = handle.update(cx, |_, window, cx| {
                elsewhere.update(cx, |workspace, cx| workspace.take_panel(panel, window, cx))
            });
            if let Ok(Some(taken)) = taken {
                into.update(cx, |workspace, cx| {
                    workspace.add_panel(taken, window, cx);
                });
                return;
            }
        }
    }

    /// Records `panel` as the one showing `view` of `document`, watched so that
    /// its release closes the record.
    fn watch(
        &mut self,
        document: DocumentId,
        view: ViewKind,
        panel: &AnyPanel,
        cx: &mut Context<Self>,
    ) {
        let documents = cx.weak_entity();
        let released = panel.on_release(cx, move |cx| {
            let _ = documents.update(cx, |documents, cx| {
                documents.closed(document, view);
                cx.notify();
            });
        });
        self.opened(document, view, panel.entity_id(), released);
    }

    /// Records `panel` as the one showing `view` of `document`, until
    /// `released` reports it dropped.
    fn opened(
        &mut self,
        document: DocumentId,
        view: ViewKind,
        panel: EntityId,
        released: Subscription,
    ) {
        if let Some(found) = self.view_mut(document, view) {
            found.live = Some(Live {
                panel,
                _released: released,
            });
        }
    }

    /// Forgets the panel that was showing `view` of `document`.
    fn closed(&mut self, document: DocumentId, view: ViewKind) {
        if let Some(found) = self.view_mut(document, view) {
            found.live = None;
        }
    }

    fn register(&mut self, label: SharedString, views: Vec<ViewSpec>) -> DocumentId {
        let id = DocumentId(self.next);
        self.next += 1;
        self.documents.push(Registered {
            id,
            label,
            views: views
                .into_iter()
                .map(|spec| View {
                    kind: spec.kind,
                    mint: spec.mint,
                    live: None,
                })
                .collect(),
        });
        id
    }

    fn view(&self, document: DocumentId, view: ViewKind) -> Option<&View> {
        self.documents
            .iter()
            .find(|held| held.id == document)?
            .views
            .iter()
            .find(|held| held.kind == view)
    }

    fn view_mut(&mut self, document: DocumentId, view: ViewKind) -> Option<&mut View> {
        self.documents
            .iter_mut()
            .find(|held| held.id == document)?
            .views
            .iter_mut()
            .find(|held| held.kind == view)
    }

    fn workspaces(&self) -> Vec<Entity<Workspace>> {
        let mut open = Vec::with_capacity(self.windows.len());
        open.extend(self.windows.iter().filter_map(WeakEntity::upgrade));
        open
    }
}

/// Where `view`'s panel stands in the windows `workspaces` frame.
fn state(view: &View, workspaces: &[Entity<Workspace>], cx: &App) -> ViewState {
    let Some(live) = &view.live else {
        return ViewState::Closed;
    };
    let shown = workspaces
        .iter()
        .find_map(|workspace| workspace.read(cx).shows(live.panel, cx));
    match shown {
        Some(true) => ViewState::Visible,
        _ => ViewState::Open,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMELINE: ViewKind = ViewKind::new("timeline");
    const FLAME: ViewKind = ViewKind::new("flame");

    /// A view registered for its kind alone. Minting a panel takes a running
    /// application, which the bookkeeping exercised here stands apart from.
    fn spec(kind: ViewKind) -> ViewSpec {
        ViewSpec {
            kind,
            mint: Rc::new(move |_| panic!("{} mints no panel here", kind.as_str())),
        }
    }

    /// A registry holding a document per label, each showing a timeline and a
    /// flame graph.
    fn registry(labels: [&str; 2]) -> (Documents, [DocumentId; 2]) {
        let mut documents = Documents::new();
        let ids =
            labels.map(|label| documents.register(label.into(), vec![spec(TIMELINE), spec(FLAME)]));
        (documents, ids)
    }

    /// The panel a pane holds, named by a number so a fresh one is told from the
    /// one it replaces.
    fn panel(id: u64) -> EntityId {
        EntityId::from(id)
    }

    /// Records `panel` as showing `view` of `document`, watched by a
    /// subscription over nothing in place of the release observer a running
    /// application installs.
    fn opened(documents: &mut Documents, document: DocumentId, view: ViewKind, panel: EntityId) {
        documents.opened(document, view, panel, Subscription::new(|| {}));
    }

    #[test]
    fn a_view_no_panel_shows_is_closed() {
        let (documents, [profile, _]) = registry(["a.profile.gz", "b.profile.gz"]);
        assert_eq!(documents.panel(profile, TIMELINE), None);
    }

    #[test]
    fn a_view_is_open_at_the_panel_showing_it() {
        let (mut documents, [profile, _]) = registry(["a.profile.gz", "b.profile.gz"]);
        opened(&mut documents, profile, TIMELINE, panel(7));
        assert_eq!(documents.panel(profile, TIMELINE), Some(panel(7)));
        assert_eq!(documents.panel(profile, FLAME), None);
    }

    /// The release of the panel showing a view is what closes it, and it closes
    /// that view alone.
    #[test]
    fn a_released_panel_closes_the_view_it_showed() {
        let (mut documents, [profile, _]) = registry(["a.profile.gz", "b.profile.gz"]);
        opened(&mut documents, profile, TIMELINE, panel(1));
        opened(&mut documents, profile, FLAME, panel(2));
        documents.closed(profile, FLAME);
        assert_eq!(documents.panel(profile, FLAME), None);
        assert_eq!(documents.panel(profile, TIMELINE), Some(panel(1)));
    }

    /// Showing a closed view mints a panel of its own, which the record then
    /// names in place of the one that closed.
    #[test]
    fn a_reopened_view_is_open_at_a_fresh_panel() {
        let (mut documents, [profile, _]) = registry(["a.profile.gz", "b.profile.gz"]);
        opened(&mut documents, profile, FLAME, panel(1));
        documents.closed(profile, FLAME);
        opened(&mut documents, profile, FLAME, panel(2));
        assert_eq!(documents.panel(profile, FLAME), Some(panel(2)));
    }

    /// Two documents show views of the same kinds, so a record kept per kind
    /// alone would have either one answering for the other.
    #[test]
    fn documents_showing_the_same_views_keep_records_of_their_own() {
        let (mut documents, [first, second]) = registry(["a.profile.gz", "b.profile.gz"]);
        opened(&mut documents, first, TIMELINE, panel(1));
        opened(&mut documents, second, TIMELINE, panel(2));
        documents.closed(first, TIMELINE);
        assert_eq!(documents.panel(first, TIMELINE), None);
        assert_eq!(documents.panel(second, TIMELINE), Some(panel(2)));
    }

    #[test]
    fn every_document_registers_under_an_identity_of_its_own() {
        let (_, [first, second]) = registry(["a.profile.gz", "b.profile.gz"]);
        assert_ne!(first, second);
    }

    /// A document answers for the views it registered and no others, so asking
    /// it for a view another viewer shows names nothing and records nothing.
    #[test]
    fn a_view_a_document_does_not_show_is_not_its_own() {
        let (mut documents, [profile, _]) = registry(["a.profile.gz", "b.profile.gz"]);
        let stats = ViewKind::new("stats");
        opened(&mut documents, profile, stats, panel(1));
        assert_eq!(documents.panel(profile, stats), None);
    }
}
