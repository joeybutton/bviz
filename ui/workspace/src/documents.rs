use crate::panel::numbered;
use crate::{AnyPanel, Panel, PanelGroup, ViewKind, Workspace};
use gpui::{
    App, AppContext, Context, Entity, EntityId, Global, SharedString, Subscription, WeakEntity,
    Window,
};
use std::num::NonZeroUsize;
use std::rc::Rc;

/// Identity of a document the session holds open. Two readings of one file are
/// two documents, each with views of its own.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DocumentId(u64);

impl DocumentId {
    /// An identity for a test that stands in for the registry minting them.
    #[cfg(test)]
    pub(crate) const fn for_test(id: u64) -> Self {
        Self(id)
    }
}

/// Builds a panel showing one view of a document. Takes no window, so a view can
/// be minted wherever the reader asks for it.
type Mint = Rc<dyn Fn(Option<NonZeroUsize>, &mut App) -> AnyPanel>;

/// One view a document can show, and how to build a panel showing it.
pub struct ViewSpec {
    kind: ViewKind,
    label: &'static str,
    mint: Mint,
}

impl ViewSpec {
    /// The view panels of type `T` show, `mint` building one over whatever the
    /// document holds its content in.
    pub fn new<T: Panel>(mint: impl Fn(&mut App) -> Entity<T> + 'static) -> Self {
        let kind = T::kind();
        Self {
            kind: kind.view,
            label: kind.label,
            mint: Rc::new(move |ordinal, cx| AnyPanel::new(mint(cx), ordinal)),
        }
    }

    /// The registry's record of this view, which no panel shows yet.
    fn registered(&self) -> View {
        View {
            kind: self.kind,
            label: self.label,
            mint: self.mint.clone(),
            live: None,
        }
    }
}

/// What a document's view has on screen. A pane draws only the panel it shows,
/// so a view can have a panel that no frame draws.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewState {
    /// No panel shows this view.
    Closed,
    /// A pane holds a panel showing this view, but no frame draws it: the pane
    /// shows another of its tabs, or the window draws another pane in place of
    /// the one holding it.
    Open,
    /// The pane holding this view's panel shows it.
    Visible,
}

/// A document of the session, read out of the registry for a reader choosing
/// among what is open.
pub struct OpenDocument {
    pub id: DocumentId,
    /// How the document reads, number included.
    pub label: SharedString,
    pub views: Vec<OpenView>,
}

pub struct OpenView {
    pub kind: ViewKind,
    /// How this view reads where it is listed among the ones its document can
    /// show.
    pub label: &'static str,
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
    /// The number the window gave this document.
    ordinal: Option<NonZeroUsize>,
    /// The views the document can show, arranged as the viewer opening it asked
    /// for them to be.
    views: PanelGroup<View>,
}

impl Registered {
    fn name(&self) -> SharedString {
        numbered(self.label.clone(), self.ordinal)
    }

    fn view(&self, kind: ViewKind) -> Option<&View> {
        self.views.panels().find(|held| held.kind == kind)
    }

    fn view_mut(&mut self, kind: ViewKind) -> Option<&mut View> {
        self.views.panels_mut().find(|held| held.kind == kind)
    }
}

struct View {
    kind: ViewKind,
    label: &'static str,
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

    /// Registers a document labelled `label` and numbered `ordinal` showing
    /// `views`, and mints a panel for each where the group places it. The
    /// registry keeps what it takes to mint them again, and the arrangement
    /// they were asked for.
    pub(crate) fn open(
        &mut self,
        label: SharedString,
        ordinal: Option<NonZeroUsize>,
        views: PanelGroup<ViewSpec>,
        cx: &mut Context<Self>,
    ) -> PanelGroup {
        let views = views.map(&mut ViewSpec::registered);
        let panels = views.map(&mut |view: &View| (view.mint)(ordinal, cx));
        let id = self.register(label, ordinal, views);
        for panel in panels.panels() {
            self.watch(id, panel.view_kind(), panel, cx);
        }
        cx.notify();
        panels
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
        let Some(held) = self.registered(document) else {
            return;
        };
        let Some(found) = held.view(view) else {
            return;
        };
        let ordinal = held.ordinal;
        let live = found.live.as_ref().map(|live| live.panel);
        let mint = found.mint.clone();
        match live {
            Some(panel) => self.reveal(panel, workspace, window, cx),
            None => {
                let panel = mint(ordinal, cx);
                self.watch(document, view, &panel, cx);
                workspace.update(cx, |workspace, cx| {
                    workspace.add_panel(panel, window, cx);
                });
                cx.notify();
            }
        }
    }

    /// Which panel shows `view` of `document`, if one does.
    #[cfg(test)]
    fn panel(&self, document: DocumentId, view: ViewKind) -> Option<EntityId> {
        self.registered(document)?
            .view(view)?
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
            open.push(OpenDocument {
                id: document.id,
                label: document.name(),
                views: document
                    .views
                    .panels()
                    .map(|view| OpenView {
                        kind: view.kind,
                        label: view.label,
                        state: state(view, &workspaces, cx),
                    })
                    .collect(),
            });
        }
        open
    }

    /// The arrangement the session's documents described when they opened, over
    /// what `panel` makes of the panel showing each view: every document's own
    /// group [compacted](PanelGroup::compact) to the views `panel` makes
    /// something of, stacked in the order the documents opened. `None` where it
    /// makes nothing of any view of any document.
    pub(crate) fn opening_arrangement<T>(
        &self,
        panel: &mut impl FnMut(EntityId) -> Option<T>,
    ) -> Option<PanelGroup<T>> {
        let mut stacked = Vec::with_capacity(self.documents.len());
        for document in &self.documents {
            let mut shown = |view: &View| view.live.as_ref().and_then(|live| panel(live.panel));
            stacked.extend(document.views.map(&mut shown).compact());
        }
        let mut stacked = stacked.into_iter();
        let first = stacked.next()?;
        Some(PanelGroup::stack(first, stacked.collect()))
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

    fn register(
        &mut self,
        label: SharedString,
        ordinal: Option<NonZeroUsize>,
        views: PanelGroup<View>,
    ) -> DocumentId {
        let id = DocumentId(self.next);
        self.next += 1;
        self.documents.push(Registered {
            id,
            label,
            ordinal,
            views,
        });
        id
    }

    fn registered(&self, document: DocumentId) -> Option<&Registered> {
        self.documents.iter().find(|held| held.id == document)
    }

    fn registered_mut(&mut self, document: DocumentId) -> Option<&mut Registered> {
        self.documents.iter_mut().find(|held| held.id == document)
    }

    fn view_mut(&mut self, document: DocumentId, view: ViewKind) -> Option<&mut View> {
        self.registered_mut(document)?.view_mut(view)
    }

    /// The workspaces framing the session's open windows, in the order their
    /// windows opened.
    pub(crate) fn workspaces(&self) -> Vec<Entity<Workspace>> {
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

    use crate::layout::shape;
    use crate::testing::{SHARE_ROUNDING, letter};
    use crate::{Layout, Weight};
    use gpui::Axis;
    use std::collections::HashMap;

    const TIMELINE: ViewKind = ViewKind::new("timeline");
    const FLAME: ViewKind = ViewKind::new("flame");
    const STATS: ViewKind = ViewKind::new("stats");

    /// A view registered for its kind alone. Minting a panel takes a running
    /// application, which the bookkeeping exercised here stands apart from.
    fn view(kind: ViewKind) -> View {
        ViewSpec {
            kind,
            label: kind.as_str(),
            mint: Rc::new(move |_, _| panic!("{} mints no panel here", kind.as_str())),
        }
        .registered()
    }

    /// A timeline over a pane of a flame graph and a stats table tabbed behind
    /// it, which is how a profile's viewer arranges its views.
    fn profile_views() -> PanelGroup<View> {
        PanelGroup::split(
            Axis::Vertical,
            0.5,
            PanelGroup::one(view(TIMELINE)),
            PanelGroup::tabs(view(FLAME), vec![view(STATS)]),
        )
    }

    /// A registry holding a document per label, each showing a timeline and a
    /// flame graph.
    fn registry(labels: [&str; 2]) -> (Documents, [DocumentId; 2]) {
        let mut documents = Documents::new();
        let ids = labels.map(|label| {
            documents.register(
                label.into(),
                None,
                PanelGroup::tabs(view(TIMELINE), vec![view(FLAME)]),
            )
        });
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

    /// A document given a number is listed with it, and one given none under its
    /// bare label.
    #[test]
    fn a_document_is_listed_under_its_number() {
        let mut documents = Documents::new();
        for (label, ordinal) in [
            ("build.profile.gz", NonZeroUsize::new(1)),
            ("build.bep", None),
            ("build.profile.gz", NonZeroUsize::new(2)),
        ] {
            documents.register(label.into(), ordinal, PanelGroup::one(view(TIMELINE)));
        }
        let names: Vec<SharedString> = documents.documents.iter().map(Registered::name).collect();
        assert_eq!(
            names,
            ["build.profile.gz (1)", "build.bep", "build.profile.gz (2)"]
        );
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
        let graph = ViewKind::new("graph");
        opened(&mut documents, profile, graph, panel(1));
        assert_eq!(documents.panel(profile, graph), None);
    }

    /// A registry holding a document per label, each showing a profile's views
    /// with a panel open for every one of them. The panels are named by letter
    /// in the order the documents registered and their groups lay their views
    /// out.
    fn shown(labels: &[&str]) -> (Documents, Vec<(EntityId, char)>) {
        let mut documents = Documents::new();
        let mut panels = Vec::new();
        for label in labels {
            let id = documents.register((*label).into(), None, profile_views());
            for kind in [TIMELINE, FLAME, STATS] {
                let named = (panel(panels.len() as u64), letter(panels.len()));
                opened(&mut documents, id, kind, named.0);
                panels.push(named);
            }
        }
        (documents, panels)
    }

    /// The arrangement `documents` describes for a window holding the panels
    /// `here` names, over the letters naming those panels. Each panel is taken
    /// as it is laid out, exactly as a window's rebuild takes it, so a registry
    /// naming one panel from two places is handed it once.
    fn rebuilt(
        documents: &Documents,
        panels: &[(EntityId, char)],
        here: &str,
    ) -> Option<PanelGroup<char>> {
        let mut held: HashMap<EntityId, char> = panels
            .iter()
            .filter(|(_, letter)| here.contains(*letter))
            .copied()
            .collect();
        documents.opening_arrangement(&mut |panel| held.remove(&panel))
    }

    /// That arrangement written out, each pane as the panels it holds in tab
    /// order. A panel is laid out in one place: a window holds it in one.
    fn rebuilt_shape(documents: &Documents, panels: &[(EntityId, char)], here: &str) -> String {
        let group = rebuilt(documents, panels, here).expect("an arrangement of panels");
        let written =
            shape(&group.arrange(&mut |first, rest| {
                std::iter::once(first).chain(rest).collect::<String>()
            }));
        let mut laid_out: Vec<char> = written.chars().filter(char::is_ascii_alphabetic).collect();
        let places = laid_out.len();
        laid_out.sort_unstable();
        laid_out.dedup();
        assert_eq!(laid_out.len(), places, "{written} lays a panel out twice");
        written
    }

    /// A window holding every panel of its document is laid out exactly as that
    /// document's viewer asked for.
    #[test]
    fn a_window_holding_every_view_is_laid_out_as_its_document_asked() {
        let (documents, panels) = shown(&["build.profile.gz"]);
        assert_eq!(rebuilt_shape(&documents, &panels, "abc"), "(a/bc)");
    }

    /// A view whose panel closed takes no place in the arrangement, and the
    /// panes left divide what they were laid out in.
    #[test]
    fn a_closed_view_takes_no_place_in_the_arrangement() {
        let (mut documents, panels) = shown(&["build.profile.gz"]);
        documents.closed(documents.documents[0].id, FLAME);
        assert_eq!(rebuilt_shape(&documents, &panels, "abc"), "(a/c)");
        documents.closed(documents.documents[0].id, TIMELINE);
        assert_eq!(rebuilt_shape(&documents, &panels, "abc"), "c");
    }

    /// A view another window holds goes on being open there: putting this window
    /// back leaves that panel where it is and lays out the rest.
    #[test]
    fn a_view_another_window_holds_stays_there() {
        let (documents, panels) = shown(&["build.profile.gz"]);
        assert_eq!(rebuilt_shape(&documents, &panels, "ac"), "(a/c)");
        assert_eq!(
            documents.panel(documents.documents[0].id, FLAME),
            Some(panels[1].0)
        );
    }

    /// A document none of whose panels the window holds is laid out nowhere in
    /// it, and a window holding none at all has no arrangement to be put back
    /// to.
    #[test]
    fn a_document_the_window_holds_nothing_of_is_left_out() {
        let (documents, panels) = shown(&["a.profile.gz", "b.profile.gz"]);
        assert_eq!(rebuilt_shape(&documents, &panels, "abcdef"), "(a/bc/d/ef)");
        assert_eq!(rebuilt_shape(&documents, &panels, "def"), "(d/ef)");
        assert_eq!(rebuilt_shape(&documents, &panels, "b"), "b");
        assert!(rebuilt(&documents, &panels, "").is_none());
    }

    /// Putting a window back settles it: the arrangement described for the
    /// panels it holds once it has been put back is the one it was put into.
    #[test]
    fn putting_a_window_back_twice_lays_it_out_the_same_way() {
        let (documents, panels) = shown(&["a.profile.gz", "b.profile.gz"]);
        let once = rebuilt_shape(&documents, &panels, "abcdef");
        assert_eq!(rebuilt_shape(&documents, &panels, "abcdef"), once);
    }

    /// A document whose group asks twice for a view of one kind keeps the one
    /// record that kind is looked up by, so its second panel is one no document
    /// lays out and the window holding it has to keep a place for it itself.
    #[test]
    fn a_kind_a_document_asks_for_twice_is_laid_out_once() {
        let mut documents = Documents::new();
        let id = documents.register(
            "build.profile.gz".into(),
            None,
            PanelGroup::tabs(view(TIMELINE), vec![view(TIMELINE)]),
        );
        opened(&mut documents, id, TIMELINE, panel(0));
        opened(&mut documents, id, TIMELINE, panel(1));
        assert_eq!(
            rebuilt_shape(&documents, &[(panel(0), 'a'), (panel(1), 'b')], "ab"),
            "b"
        );
    }

    /// A registry holding a document per weight, each showing a timeline with a
    /// panel open for it. The panels are named by letter in the order the
    /// documents registered.
    fn weighed(weights: &[Weight]) -> (Documents, Vec<(EntityId, char)>) {
        let mut documents = Documents::new();
        let mut panels = Vec::with_capacity(weights.len());
        for (at, weight) in weights.iter().enumerate() {
            let id = documents.register(
                format!("d{at}").into(),
                None,
                PanelGroup::one(view(TIMELINE)).with_weight(*weight),
            );
            let named = (panel(at as u64), letter(at));
            opened(&mut documents, id, TIMELINE, named.0);
            panels.push(named);
        }
        (documents, panels)
    }

    /// Asserts the documents of `weights`, in a window holding the panels
    /// `here` names, are laid out at `expected` shares of it, top to bottom.
    fn assert_rebuilt_shares(weights: &[Weight], here: &str, expected: &[f32]) {
        let (documents, panels) = weighed(weights);
        let group = rebuilt(&documents, &panels, here).expect("an arrangement of panels");
        let Layout::Split(split) = group.arrange(&mut |first, _| first) else {
            panic!("documents stacked in a window are parted by a split");
        };
        assert_eq!(split.axis(), Axis::Vertical);
        let shares: Vec<f32> = split.sides().into_iter().map(|side| side.share).collect();
        assert_eq!(shares.len(), expected.len(), "a share per document stacked");
        for (share, want) in shares.into_iter().zip(expected) {
            assert!(
                (share - want).abs() < SHARE_ROUNDING,
                "{weights:?} lays a document out at {share} of the window, not {want}"
            );
        }
    }

    /// Documents are stacked in the order they opened, each taking the share of
    /// the window its own group asked to weigh.
    #[test]
    fn stacked_documents_take_the_shares_their_weights_ask_for() {
        assert_rebuilt_shares(&[Weight::CHART, Weight::LOG], "ab", &[2.0 / 3.0, 1.0 / 3.0]);
    }

    /// A document the window holds no panel of takes no place in the stack, and
    /// what it would have weighed is not divided out: the documents left hold
    /// the proportions they asked for against each other.
    #[test]
    fn a_document_the_window_holds_nothing_of_leaves_the_rest_their_proportions() {
        assert_rebuilt_shares(
            &[Weight::CHART, Weight::LOG, Weight::CHART],
            "ac",
            &[0.5, 0.5],
        );
    }
}
