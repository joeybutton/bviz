//! What one panel draws: the document's graph as that panel's filter leaves it.
//!
//! A filter says which of the graph a reader is looking at, so it belongs to the
//! panel rather than to the document behind it, and two panels over one graph
//! draw different slices of it. What the document holds — the base graph, its
//! reduction, the facets, the names and the map's placement — is derived once,
//! unfiltered, and each panel restricts that for itself.
//!
//! A restriction keeps every surviving node's id, so the expansion, the
//! selection and the layout's pinning anchors go on naming what they named and
//! carry across a filter change.

use crate::mode::LayoutMode;
use crate::model::{Loaded, OPENS_REDUCED, Shared, Source};
use crate::{GraphPanel, MARGIN, build};
use bviz_graph_layout::graph::Graph;
use bviz_graph_layout::lod::{DEFAULT_MAX_EDGES, DEFAULT_MAX_NODES, Overflow};
use bviz_graph_layout::select::{Filter, Verdict};
use bviz_graph_layout::{Id, LaidOut, MapLayout, Names};
use bviz_ui_components::load_in_background;
use gpui::{Context, Task};
use std::collections::HashSet;
use std::sync::Arc;

/// Derives every view of `map` off the UI thread, coarsest first.
///
/// A map derives a view the first time a camera calls for one, which is what
/// keeps the wait before the first frame to the single view that frame draws.
/// The reader reaches the rest by zooming, and a view derived under that gesture
/// would hold the frame it was made for; deriving them in the background instead
/// leaves the zoom nothing to wait on. A view already derived costs nothing to
/// ask for again, so this races the camera harmlessly.
pub(crate) fn derive_levels<V: 'static>(map: Arc<MapLayout>, cx: &mut Context<V>) {
    cx.background_executor()
        .spawn(async move {
            for level in 0..map.level_count() {
                map.level(level);
            }
        })
        .detach();
}

/// Where a map's zoom stands in its pyramid.
#[derive(Clone, Copy)]
pub(crate) struct Level {
    pub(crate) index: usize,
    pub(crate) count: usize,
}

/// Identifies the view a [`Drawn`] resolves to, by which work derived from that
/// view is redone exactly when the view moves under it. A derivation replaces
/// both the layered drawing and the map, so the generation that produced them
/// tells either from what took its place.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewKey {
    Layered(u64),
    Map { derivation: u64, level: usize },
}

/// A share in the layout a mode draws, cheap to clone into a `'static` paint
/// closure. A map resolves to a view only once the camera is known, so the level
/// a frame draws is picked at the same scale it paints at.
#[derive(Clone)]
pub(crate) enum Drawn {
    Layered {
        view: Arc<LaidOut>,
        derivation: u64,
    },
    Map {
        map: Arc<MapLayout>,
        derivation: u64,
    },
}

impl Drawn {
    /// The view drawn at `zoom` screen pixels per layout unit.
    pub(crate) fn view(&self, zoom: f32) -> &LaidOut {
        match self {
            Self::Layered { view, .. } => view,
            Self::Map { map, .. } => map.level(map.level_for_scale(zoom)),
        }
    }

    /// Identifies the view [`view`](Self::view) returns at `zoom`.
    pub(crate) fn view_key(&self, zoom: f32) -> ViewKey {
        match self {
            Self::Layered { derivation, .. } => ViewKey::Layered(*derivation),
            Self::Map { map, derivation } => ViewKey::Map {
                derivation: *derivation,
                level: map.level_for_scale(zoom),
            },
        }
    }

    /// Which of a map's levels [`view`](Self::view) draws at `zoom`; `None` for a
    /// layered layout, which has no pyramid.
    pub(crate) fn level(&self, zoom: f32) -> Option<Level> {
        match self {
            Self::Layered { .. } => None,
            Self::Map { map, .. } => Some(Level {
                index: map.level_for_scale(zoom),
                count: map.level_count(),
            }),
        }
    }

    /// The extent the camera pans and zooms over: the graph's own size plus
    /// [`MARGIN`] on every side. Every map level spans the same extent, so this
    /// holds still as the zoom changes level.
    pub(crate) fn content(&self) -> (f32, f32) {
        let (width, height) = match self {
            Self::Layered { view, .. } => (view.width, view.height),
            Self::Map { map, .. } => (map.width(), map.height()),
        };
        (
            width as f32 + 2. * MARGIN as f32,
            height as f32 + 2. * MARGIN as f32,
        )
    }
}

/// The boxes a mode's layout lets the user open or close, cheap to clone into a
/// `'static` paint closure. A box outside it carries no disclosure caret and
/// ignores a click.
#[derive(Clone)]
pub(crate) enum Expandable {
    /// No box at all: a map draws whichever level its camera calls for, so what
    /// a container holds is reached by zooming in on it.
    Nothing,
    /// The multi-child containers of the drawn graph, plus every collapsed group
    /// — a leaf holds nothing to open, and a singleton container opens itself.
    Containers(Arc<HashSet<Id>>),
}

impl Expandable {
    pub(crate) fn holds(&self, b: &bviz_graph_layout::Box) -> bool {
        match self {
            Self::Nothing => false,
            Self::Containers(ids) => ids.contains(&b.id) || b.group,
        }
    }
}

/// The graph one filter leaves of the document's, with everything derived from
/// it. A derivation replaces it whole, so a frame reads one filter's graph
/// throughout.
#[derive(Clone)]
struct Slice {
    /// The filter this was derived from, which a panel drawing through another
    /// must restrict the document's graph afresh to leave behind.
    filter: Filter,
    base: Arc<Graph>,
    /// The restriction with its transitively-redundant edges removed, which the
    /// collapsed views are projected through.
    reduced: Arc<Graph>,
    /// The restriction placed where the document's map places it, so filtering
    /// leaves holes rather than moving what survives.
    map: Arc<MapLayout>,
    containers: Arc<HashSet<Id>>,
    /// The nodes the filter draws as no part of what it selects, which the
    /// drawing fades rather than leaving out.
    faded: Arc<HashSet<Id>>,
    /// Extended by each layered layout over this graph, whose quotient pass
    /// mints group ids against it.
    names: Names,
}

impl Slice {
    /// The whole graph, which is what `filter` leaves where it hides nothing; it
    /// is already placed and reduced, so such a filter costs nothing to draw
    /// through.
    fn whole(shared: &Shared, filter: Filter) -> Self {
        Slice {
            filter,
            base: Arc::clone(&shared.base),
            reduced: Arc::clone(&shared.reduced_base),
            map: Arc::clone(&shared.map),
            containers: Arc::clone(&shared.containers),
            faded: Arc::new(HashSet::new()),
            names: shared.names.clone(),
        }
    }
}

/// What re-evaluating a filter left of the graph a panel was drawing.
struct Restricted {
    dropped: HashSet<Id>,
    expanded: HashSet<Id>,
    over_budget: Option<Overflow>,
}

/// One derivation's result.
struct Landed {
    slice: Slice,
    /// Absent where the derivation drew on the restriction the panel already
    /// held, which leaves the expansion and the selection as they were.
    restricted: Option<Restricted>,
    /// The layered drawing, absent where this derivation laid none out — a
    /// panel drawing a map that holds no layered drawing to keep current.
    layered: Option<Arc<LaidOut>>,
}

/// What the derivation in flight will produce.
#[derive(Clone, Copy)]
struct InFlight {
    /// Whether it lays a layered drawing out, which is settled when the
    /// derivation is requested and so says nothing about the mode the panel has
    /// selected since.
    lays_out: bool,
}

/// A layered drawing that has landed, and the one it takes the place of, which
/// the view eases onto it.
pub(crate) struct LayoutLanded {
    pub(crate) replaced: Option<Arc<LaidOut>>,
    pub(crate) current: Arc<LaidOut>,
}

/// What adopting a derivation's result leaves the panel to follow up on.
struct Adopted {
    /// Absent where the derivation left the restriction the panel held
    /// standing.
    restriction: Option<Restriction>,
    /// Whether the camera is to be moved onto what the derivation left drawn.
    frame: bool,
    /// The layered drawing to ease the one it replaces onto, where this
    /// derivation laid one out.
    landing: Option<LayoutLanded>,
}

/// What a derivation that evaluated the filter afresh leaves for the panel to
/// answer for.
struct Restriction {
    /// The nodes the filter took out of the graph, which nothing keyed on a
    /// node — an expansion, a selection — can go on naming.
    dropped: HashSet<Id>,
    /// The graph that leaves, placed; no camera has called for a level of it.
    map: Arc<MapLayout>,
}

/// What a panel draws the document's graph as, and how far the derivation of it
/// has got.
pub(crate) struct Derived {
    filter: Filter,
    /// Container and group ids expanded in the layered layout last requested;
    /// every other container folds its subtree away so the layered layout only
    /// processes the visible view.
    expanded: HashSet<Id>,
    /// Whether the layered layout last requested draws the transitive reduction
    /// of the edges rather than every edge.
    reduced: bool,
    /// Absent until the document's read lands.
    slice: Option<Slice>,
    layered: Option<Arc<LaidOut>>,
    /// The size of the view the panel last restricted to where it is past the
    /// caps that bound it, absent where it fits.
    over_budget: Option<Overflow>,
    /// What the derivation running will produce, absent while none is. A
    /// request cancels the one it replaces, so at most one runs at a time and
    /// its result is the panel's.
    in_flight: Option<InFlight>,
    /// Counts the derivations adopted, by which the drawing on screen is told
    /// from the one it replaced.
    landed: u64,
    /// Whether the filter awaiting a restriction was set by a gesture naming
    /// what to look at, which is answered by moving the camera onto what that
    /// restriction leaves drawn.
    frame_on_landing: bool,
    /// The derivation in flight, held so that it is not cancelled.
    _work: Task<()>,
}

impl Derived {
    /// A panel's view of a graph that has not been read yet.
    pub(crate) fn new() -> Self {
        Derived {
            filter: Filter::default(),
            expanded: HashSet::new(),
            reduced: OPENS_REDUCED,
            slice: None,
            layered: None,
            over_budget: None,
            in_flight: None,
            landed: 0,
            frame_on_landing: false,
            _work: Task::ready(()),
        }
    }

    /// Draws `loaded`'s graph whole, at the expansion its read sized, through no
    /// filter.
    ///
    /// A filter selects within one graph — its rules over that graph's
    /// repositories, classes and labels, its pins over that graph's node ids —
    /// so it does not carry to the graph that takes its place. Every drawing of
    /// the graph left behind goes with it, and so does the derivation that was
    /// producing one.
    fn opened_on(&mut self, loaded: &Loaded) {
        self.filter = Filter::default();
        self.expanded = loaded.opening.expanded.clone();
        self.over_budget = loaded.opening.over_budget;
        self.slice = Some(Slice::whole(&loaded.shared, Filter::default()));
        self.layered = None;
        self.in_flight = None;
        self._work = Task::ready(());
    }

    /// The layout `mode` draws, or `None` while the graph or that mode's layout
    /// is still being derived.
    pub(crate) fn drawn(&self, mode: LayoutMode) -> Option<Drawn> {
        let slice = self.slice.as_ref()?;
        match mode {
            LayoutMode::Map => Some(Drawn::Map {
                map: Arc::clone(&slice.map),
                derivation: self.landed,
            }),
            LayoutMode::Layered => self.layered.as_ref().map(|view| Drawn::Layered {
                view: Arc::clone(view),
                derivation: self.landed,
            }),
        }
    }

    /// The boxes `mode`'s layout lets the user open or close.
    pub(crate) fn expandable(&self, mode: LayoutMode) -> Expandable {
        match (mode, &self.slice) {
            (LayoutMode::Layered, Some(slice)) => {
                Expandable::Containers(Arc::clone(&slice.containers))
            }
            _ => Expandable::Nothing,
        }
    }

    pub(crate) fn reduced(&self) -> bool {
        self.reduced
    }

    /// The filter this panel draws its share of the document's graph through,
    /// which is where the toolbar's controls read their state from.
    pub(crate) fn filter(&self) -> &Filter {
        &self.filter
    }

    /// The boxes the filter draws as no part of what it selects.
    pub(crate) fn faded(&self) -> Arc<HashSet<Id>> {
        self.slice.as_ref().map_or_else(
            || Arc::new(HashSet::new()),
            |slice| Arc::clone(&slice.faded),
        )
    }

    /// Whether a derivation is running, so a view can show a progress indicator
    /// while the drawing it will replace stays on screen and interactive.
    pub(crate) fn deriving(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Whether the derivation in flight will produce what `mode` draws, by which
    /// a mode selected while one runs waits on it only where it will answer for
    /// that mode.
    fn deriving_for(&self, mode: LayoutMode) -> bool {
        self.in_flight.is_some_and(|in_flight| match mode {
            LayoutMode::Map => true,
            LayoutMode::Layered => in_flight.lays_out,
        })
    }

    /// Whether a derivation has to restrict the document's graph afresh rather
    /// than lay out over the restriction the panel holds, which it must wherever
    /// that restriction was derived from another filter — as it is for every
    /// request made before a filter change has landed.
    fn restricting(&self) -> bool {
        self.slice
            .as_ref()
            .is_some_and(|slice| slice.filter != self.filter)
    }

    pub(crate) fn over_budget(&self) -> Option<Overflow> {
        self.over_budget
    }

    /// Takes `landed` as what this panel draws.
    fn adopt(&mut self, landed: Landed) -> Adopted {
        self.in_flight = None;
        self.landed += 1;
        let restriction = landed.restricted.map(|restricted| {
            self.expanded = restricted.expanded;
            self.over_budget = restricted.over_budget;
            Restriction {
                dropped: restricted.dropped,
                map: Arc::clone(&landed.slice.map),
            }
        });
        let frame = std::mem::take(&mut self.frame_on_landing) && restriction.is_some();
        self.slice = Some(landed.slice);
        let replaced = std::mem::replace(&mut self.layered, landed.layered.clone());
        Adopted {
            restriction,
            frame,
            landing: landed
                .layered
                .map(|current| LayoutLanded { replaced, current }),
        }
    }
}

/// The status line describing what a panel draws: what the read left where it
/// left no graph, and the size of the drawing `drawn` otherwise, followed by
/// `over_budget` where the panel's view is past the caps on an opening view.
pub(crate) fn status(
    source: &Source,
    drawn: Option<Drawn>,
    zoom: f32,
    over_budget: Option<Overflow>,
) -> String {
    source.status("No graph", |_| match drawn {
        None => "Laying out…".into(),
        Some(drawn) => {
            let g = drawn.view(zoom);
            let level = drawn
                .level(zoom)
                .map(|l| format!("level {}/{} · ", l.index + 1, l.count))
                .unwrap_or_default();
            format!(
                "{level}{} boxes · {} connectors · {}×{}{}",
                g.boxes.len(),
                g.connectors.len(),
                g.width.round() as i64,
                g.height.round() as i64,
                over_budget.map(past_the_caps).unwrap_or_default(),
            )
        }
    })
}

/// What a view no fold brings within the caps is past them by, so that a graph
/// drawn fully collapsed says why it is.
fn past_the_caps(over: Overflow) -> String {
    let past: Vec<String> = [
        (over.nodes, DEFAULT_MAX_NODES, "boxes"),
        (over.edges, DEFAULT_MAX_EDGES, "connectors"),
    ]
    .into_iter()
    .filter(|&(reached, cap, _)| reached > cap)
    .map(|(reached, cap, what)| format!("{reached} {what} over {cap}"))
    .collect();
    format!(" · fully folded and still {}", past.join(" and "))
}

/// Adopts the graph the document has finished reading, opening at the expansion
/// the read sized, and lays out what the selected mode draws.
pub(crate) fn opened(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let Some(loaded) = view.model.read(cx).loaded() else {
        return;
    };
    view.derived.opened_on(loaded);
    crate::search::reset(view, cx);
    ensure_layout(view, cx);
}

/// Derives what `mode` draws where the panel has no drawing of it yet and no
/// derivation that will produce one is already running.
pub(crate) fn ensure_layout(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    if view.derived.drawn(view.mode).is_none() && !view.derived.deriving_for(view.mode) {
        request(view, cx);
    }
}

/// Flips whether `id` is expanded and lays the graph out at the new expansion.
pub(crate) fn toggle_expansion(view: &mut GraphPanel, id: Id, cx: &mut Context<GraphPanel>) {
    if !view.derived.expanded.remove(&id) {
        view.derived.expanded.insert(id);
    }
    request(view, cx);
}

/// Flips whether the layered layout keeps every edge or only the transitive
/// reduction, and lays the graph out over the edge set that leaves.
pub(crate) fn toggle_reduce(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    view.derived.reduced = !view.derived.reduced;
    request(view, cx);
}

/// Draws the graph through `filter` from here on.
///
/// The view keeps what it can of where the reader had it: the expansion as far
/// as the surviving nodes and the budget allow, the camera, the edge set and —
/// the panel's own to keep — the selection. The drawing on screen stays until
/// the one this asks for lands.
pub(crate) fn set_filter(view: &mut GraphPanel, filter: Filter, cx: &mut Context<GraphPanel>) {
    apply_filter(view, filter, false, cx);
}

/// Draws the graph through `filter` from here on and moves the camera onto what
/// that leaves drawn, for a gesture that named the part of the graph to look at.
/// What survives stays where the document places it, so on a graph whose extent
/// the rest of it still spans the named part need not be anywhere near the view.
pub(crate) fn set_filter_framed(
    view: &mut GraphPanel,
    filter: Filter,
    cx: &mut Context<GraphPanel>,
) {
    apply_filter(view, filter, true, cx);
}

fn apply_filter(view: &mut GraphPanel, filter: Filter, frame: bool, cx: &mut Context<GraphPanel>) {
    if view.derived.filter == filter {
        return;
    }
    view.derived.filter = filter;
    view.derived.frame_on_landing = frame;
    request(view, cx);
}

/// Derives what the panel draws off the UI thread and adopts the result.
fn request(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let Some(shared) = view.model.read(cx).loaded().map(|l| l.shared.clone()) else {
        return;
    };
    let Some(slice) = view.derived.slice.clone() else {
        return;
    };
    let restricting = view.derived.restricting();
    // A derivation that leaves the restriction standing was asked for to
    // redraw. One that restricts afresh lays a drawing out for the panel showing
    // one, and for the panel holding one, which would otherwise go on drawing
    // over a graph the filter has moved out from under it; a map needs none.
    let lays_out =
        !restricting || view.mode == LayoutMode::Layered || view.derived.layered.is_some();
    let derived = &mut view.derived;
    derived.in_flight = Some(InFlight { lays_out });
    let (filter, reduced) = (derived.filter.clone(), derived.reduced);
    let expanded = derived.expanded.clone();
    let held = derived.layered.clone();
    cx.notify();
    derived._work = load_in_background(
        cx,
        move || {
            let (mut slice, restricted) = match restricting {
                true => {
                    let (slice, restricted) = restrict(&shared, &filter, &expanded, reduced);
                    (slice, Some(restricted))
                }
                false => (slice, None),
            };
            let expanded = match &restricted {
                Some(restricted) => &restricted.expanded,
                None => &expanded,
            };
            let layered = lays_out.then(|| {
                Arc::new(build::relayout(
                    &slice.base,
                    &slice.reduced,
                    expanded,
                    &mut slice.names,
                    reduced,
                    held.as_deref().map(|held| &held.anchors),
                ))
            });
            Landed {
                slice,
                restricted,
                layered,
            }
        },
        |view, landed, cx| {
            let Adopted {
                restriction,
                frame,
                landing,
            } = view.derived.adopt(landed);
            if let Some(restriction) = restriction {
                view.selection.forget(&restriction.dropped);
                derive_levels(restriction.map, cx);
            }
            if frame {
                crate::framing::frame_drawing(view);
            }
            if let Some(landing) = landing {
                view.settle_onto(&landing, cx);
            }
        },
    );
}

/// The graph `filter` leaves of `shared`'s, with `expanded` held to what
/// survives it and to the caps that bound a legible drawing.
///
/// A filter that hides nothing leaves the shared graph itself, which is already
/// placed, reduced and sized.
fn restrict(
    shared: &Shared,
    filter: &Filter,
    expanded: &HashSet<Id>,
    reduced: bool,
) -> (Slice, Restricted) {
    let verdicts = filter.evaluate(&shared.base, &shared.names, &shared.facets);
    // Every node lands in `keep` or in `dropped`, and `faded` is a part of what
    // `keep` holds, so the node count bounds all three.
    let nodes = shared.base.nodes.len();
    let mut keep: HashSet<Id> = HashSet::with_capacity(nodes);
    let mut dropped: HashSet<Id> = HashSet::with_capacity(nodes);
    let mut faded: HashSet<Id> = HashSet::with_capacity(nodes);
    for node in &shared.base.nodes {
        match verdicts.get(node.id) {
            Verdict::Hidden => {
                dropped.insert(node.id);
            }
            verdict => {
                keep.insert(node.id);
                if verdict == Verdict::Faded {
                    faded.insert(node.id);
                }
            }
        }
    }
    let faded = Arc::new(faded);
    let slice = match dropped.is_empty() {
        true => Slice {
            faded,
            ..Slice::whole(shared, filter.clone())
        },
        false => {
            let base = bviz_graph_layout::filter::filter(&shared.base, &shared.reduced_base, &keep);
            let reduction = bviz_graph_layout::reduce::transitive_reduction(&base);
            let containers = Arc::new(bviz_graph_layout::lod::expandable_containers(&base));
            let (base, reduction) = (Arc::new(base), Arc::new(reduction));
            Slice {
                filter: filter.clone(),
                map: Arc::new(
                    shared
                        .map
                        .restricted(Arc::clone(&base), Arc::clone(&reduction)),
                ),
                base,
                reduced: reduction,
                containers,
                faded,
                names: shared.names.clone(),
            }
        }
    };
    let restricted = keeping(&slice, expanded, dropped, reduced);
    (slice, restricted)
}

/// What `slice` leaves of an expansion built up over the graph it restricts:
/// those of `expanded` it still holds boxes for, as far as the caps that bound a
/// legible view allow.
///
/// `dropped` names the nodes the filter took out. An expanded id beside those
/// and beside `slice`'s own containers is a group the quotient pass minted,
/// which the next layout mints afresh, so it is left alone.
fn keeping(
    slice: &Slice,
    expanded: &HashSet<Id>,
    dropped: HashSet<Id>,
    reduced: bool,
) -> Restricted {
    let mut surviving: HashSet<Id> = HashSet::with_capacity(expanded.len());
    surviving.extend(expanded.iter().copied().filter(|id| !dropped.contains(id)));
    match bviz_graph_layout::lod::fitting_expansion(
        &slice.base,
        reduced.then_some(slice.reduced.as_ref()),
        &surviving,
        DEFAULT_MAX_NODES,
        DEFAULT_MAX_EDGES,
    ) {
        Ok(expanded) => Restricted {
            dropped,
            expanded,
            over_budget: None,
        },
        // No expansion of this graph fits, and the unexpanded view is the
        // smallest drawing of it there is.
        Err(overflow) => Restricted {
            dropped,
            expanded: HashSet::new(),
            over_budget: Some(overflow),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Opening;
    use crate::selection::Selection;
    use bviz_graph_layout::graph::Node;
    use bviz_graph_layout::select::labels::LabelIndex;
    use bviz_graph_layout::select::{Predicate, Rule, Values, Visibility};
    use bviz_graph_layout::{EdgeKind, Facets, Kind};

    /// Two packages, each holding a target with two actions under it, the
    /// actions wired in a chain, prepared the way a read prepares a document's
    /// graph.
    fn document() -> Loaded {
        let mut names = Names::default();
        let mut base = Graph::new();
        let mut actions: Vec<Id> = Vec::new();
        for package in ["a", "b"] {
            let package_id = names.intern(&format!("//{package}"));
            base.add_node(Node::new(package_id, package, Kind::Package, None));
            let target = names.intern(&format!("//{package}:t"));
            base.add_node(Node::new(target, "t", Kind::Target, Some(package_id)));
            for action in ["compile", "link"] {
                let id = names.intern(&format!("//{package}:t#{action}"));
                base.add_node(Node::new(id, action, Kind::Action, Some(target)));
                actions.push(id);
            }
        }
        for pair in actions.windows(2) {
            base.add_edge(pair[0], pair[1], EdgeKind::Aquery);
        }
        let reduced_base = bviz_graph_layout::reduce::transitive_reduction(&base);
        let containers = Arc::new(bviz_graph_layout::lod::expandable_containers(&base));
        let (base, reduced_base) = (Arc::new(base), Arc::new(reduced_base));
        let facets = Arc::new(Facets::default());
        let labels = Arc::new(LabelIndex::of(&base, &names, &facets));
        let map = bviz_graph_layout::map_layout(Arc::clone(&base), Arc::clone(&reduced_base));
        Loaded {
            shared: Shared {
                base,
                reduced_base,
                map: Arc::new(map),
                names,
                facets,
                labels,
                containers,
                capabilities: bviz_graph_layout::capability::CapabilitiesBuilder::default().seal(),
            },
            opening: Opening {
                expanded: HashSet::new(),
                over_budget: None,
            },
        }
    }

    fn id(loaded: &Loaded, name: &str) -> Id {
        loaded.shared.names.get(name).expect("an interned node")
    }

    fn kind_of(loaded: &Loaded, id: Id) -> Kind {
        loaded
            .shared
            .base
            .nodes
            .iter()
            .find(|node| node.id == id)
            .expect("a node of the document's graph")
            .kind
    }

    /// A filter that draws everything but the nodes of `kind`.
    fn hiding(kind: Kind) -> Filter {
        Filter {
            rules: vec![Rule {
                predicate: Predicate {
                    kinds: Values::new([kind]),
                    ..Predicate::default()
                },
                visibility: Visibility::Hidden,
            }],
            ..Filter::default()
        }
    }

    fn finest(map: &MapLayout) -> &LaidOut {
        map.level(map.level_count() - 1)
    }

    /// The panel's own view of a graph opened with no filter set.
    fn opened(loaded: &Loaded) -> Derived {
        let mut derived = Derived::new();
        derived.opened_on(loaded);
        derived
    }

    /// A filter drawing every node leaves the document's own graph, placement
    /// and all, so a panel that sets no filter draws exactly what the document
    /// derived and pays nothing for the filter.
    #[test]
    fn a_filter_that_hides_nothing_draws_the_document_graph_itself() {
        let loaded = document();
        let expanded: HashSet<Id> = [id(&loaded, "//a:t")].into_iter().collect();

        let (slice, restricted) = restrict(&loaded.shared, &Filter::default(), &expanded, true);

        assert!(Arc::ptr_eq(&slice.base, &loaded.shared.base));
        assert!(Arc::ptr_eq(&slice.reduced, &loaded.shared.reduced_base));
        assert!(Arc::ptr_eq(&slice.map, &loaded.shared.map));
        assert!(restricted.dropped.is_empty());
        assert_eq!(restricted.expanded, expanded);
        assert_eq!(restricted.over_budget, None);
    }

    #[test]
    fn a_filter_hiding_a_kind_leaves_no_box_for_those_nodes() {
        let loaded = document();

        let (slice, restricted) =
            restrict(&loaded.shared, &hiding(Kind::Action), &HashSet::new(), true);

        assert!(
            restricted
                .dropped
                .iter()
                .all(|&id| kind_of(&loaded, id) == Kind::Action)
        );
        assert!(!restricted.dropped.is_empty());
        assert!(
            slice
                .base
                .nodes
                .iter()
                .all(|node| node.kind != Kind::Action)
        );
        assert!(
            finest(&slice.map)
                .boxes
                .iter()
                .all(|b| kind_of(&loaded, b.id) != Kind::Action),
        );
        assert!(
            finest(&loaded.shared.map)
                .boxes
                .iter()
                .any(|b| kind_of(&loaded, b.id) == Kind::Action),
            "the document's own drawing holds none of the boxes the filter hides",
        );
    }

    /// A restriction places what survives where the whole graph places it, so a
    /// filter change leaves the view looking at the same place rather than at a
    /// graph redrawn under it.
    #[test]
    fn a_restricted_map_draws_a_survivor_where_the_document_draws_it() {
        let loaded = document();
        let (slice, _) = restrict(&loaded.shared, &hiding(Kind::Action), &HashSet::new(), true);

        let whole = finest(&loaded.shared.map);
        for b in &finest(&slice.map).boxes {
            let at = whole
                .boxes
                .iter()
                .find(|whole| whole.id == b.id)
                .expect("a surviving box the document draws");
            assert_eq!((b.x, b.y), (at.x, at.y));
        }
    }

    /// Ids outlive a restriction, so what the reader had opened and picked out
    /// goes on naming the same boxes; only what the filter took out gives way.
    #[test]
    fn an_expansion_and_a_selection_outlive_a_filter_change() {
        let loaded = document();
        let (kept, hidden) = (id(&loaded, "//a:t"), id(&loaded, "//b:t"));
        let expanded: HashSet<Id> = [kept, hidden].into_iter().collect();
        let filter = Filter {
            pins: [(hidden, Visibility::Hidden)].into_iter().collect(),
            ..Filter::default()
        };

        let (_, restricted) = restrict(&loaded.shared, &filter, &expanded, true);

        assert_eq!(restricted.expanded, [kept].into_iter().collect());

        let mut selection = Selection::none();
        selection.toggle(kept);
        selection.forget(&restricted.dropped);
        assert_eq!(selection.node(), Some(kept));
        selection.toggle(hidden);
        selection.forget(&restricted.dropped);
        assert_eq!(selection.node(), None);
    }

    /// A derivation laying no layered drawing out settles nothing for a panel
    /// that switches to the layered mode while it runs, which must therefore
    /// ask for a drawing of its own rather than wait on that derivation.
    #[test]
    fn a_derivation_laying_out_no_drawing_answers_for_the_map_alone() {
        let loaded = document();
        let mut derived = opened(&loaded);
        derived.in_flight = Some(InFlight { lays_out: false });

        assert!(derived.deriving_for(LayoutMode::Map));
        assert!(!derived.deriving_for(LayoutMode::Layered));

        derived.adopt(Landed {
            slice: Slice::whole(&loaded.shared, Filter::default()),
            restricted: None,
            layered: None,
        });

        assert!(derived.drawn(LayoutMode::Layered).is_none());
        assert!(!derived.deriving_for(LayoutMode::Layered));
    }

    /// A filter set while a derivation runs is owed a restriction that nothing
    /// else records, so every request made before it lands — an expansion, a
    /// change of edge set — restricts afresh rather than laying out the graph
    /// the superseded filter left.
    #[test]
    fn a_request_made_before_a_filter_change_lands_restricts_afresh() {
        let loaded = document();
        let mut derived = opened(&loaded);
        assert!(!derived.restricting());

        derived.filter = hiding(Kind::Action);
        assert!(derived.restricting());

        let (slice, restricted) = restrict(
            &loaded.shared,
            &derived.filter,
            &derived.expanded,
            derived.reduced,
        );
        derived.adopt(Landed {
            slice,
            restricted: Some(restricted),
            layered: None,
        });

        assert!(!derived.restricting());
    }

    /// The controls read the filter and the drawing is derived from it, so a
    /// filter the panel held over a graph that has been replaced would have the
    /// two describing different graphs — and a drawing already in hand answers
    /// the request for one, so nothing would re-derive it.
    #[test]
    fn a_document_opened_into_a_panel_is_drawn_through_no_filter() {
        let loaded = document();
        let mut derived = opened(&loaded);
        derived.filter = hiding(Kind::Action);
        derived.layered = Some(drawing(10.0));

        derived.opened_on(&document());

        assert_eq!(derived.filter(), &Filter::default());
        assert!(
            !derived.restricting(),
            "the drawing was derived from another filter than the controls read"
        );
        assert!(derived.drawn(LayoutMode::Layered).is_none());
    }

    /// A drawing whose extent says which derivation produced it.
    fn drawing(width: f64) -> Arc<LaidOut> {
        Arc::new(LaidOut {
            width,
            ..LaidOut::default()
        })
    }

    fn landed(loaded: &Loaded, width: f64) -> Landed {
        Landed {
            slice: Slice::whole(&loaded.shared, Filter::default()),
            restricted: None,
            layered: Some(drawing(width)),
        }
    }

    /// Each drawing adopted is told from the one it replaced, so work derived
    /// from a drawing — a camera's fit, a morph's anchors — is redone exactly
    /// when a derivation lands.
    #[test]
    fn each_derivation_adopted_identifies_the_drawing_it_leaves() {
        let loaded = document();
        let mut derived = opened(&loaded);
        let first = derived.drawn(LayoutMode::Map).expect("a map").view_key(1.0);

        derived.adopt(landed(&loaded, 10.0));

        let second = derived.drawn(LayoutMode::Map).expect("a map").view_key(1.0);
        assert!(first != second);
    }

    /// The drawing on screen is the panel's until a derivation lands, so a
    /// filter change never blanks the view.
    #[test]
    fn a_running_derivation_leaves_the_drawing_it_will_replace_on_screen() {
        let loaded = document();
        let mut derived = opened(&loaded);
        derived.adopt(landed(&loaded, 10.0));

        derived.in_flight = Some(InFlight { lays_out: true });

        assert!(derived.deriving());
        let drawn = derived
            .drawn(LayoutMode::Layered)
            .expect("a layered drawing");
        assert_eq!(drawn.view(1.0).width, 10.0);
    }

    /// The status line is drawn from the read and the panel's own drawing, so it
    /// reads the same whether or not a panel is showing one.
    #[test]
    fn a_graph_still_being_read_says_so() {
        assert_eq!(status(&Source::Loading, None, 1.0, None), "Loading…");
    }

    #[test]
    fn a_graph_that_could_not_be_read_names_what_is_absent() {
        let failed = Source::Failed("aquery.pb: no such file".into());
        assert_eq!(status(&failed, None, 1.0, None), "No graph");
    }

    #[test]
    fn a_drawn_graph_describes_what_it_draws() {
        let loaded = document();
        let derived = opened(&loaded);
        let drawn = derived.drawn(LayoutMode::Map).expect("a map");
        let level = drawn.level(1.0).expect("a map draws one of its levels");
        let boxes = drawn.view(1.0).boxes.len();
        let status = status(&Source::Loaded(document()), Some(drawn), 1.0, None);
        assert!(
            status.starts_with(&format!(
                "level {}/{} · {boxes} boxes · ",
                level.index + 1,
                level.count
            )),
            "{status}",
        );
    }

    /// A graph too large to draw within the caps opens fully collapsed, which
    /// looks like a graph that needed no folding, so the status line says which
    /// count the view is past its cap on.
    #[test]
    fn a_view_no_fold_brings_within_the_caps_says_what_it_is_past() {
        let loaded = document();
        let derived = opened(&loaded);
        let drawn = derived.drawn(LayoutMode::Map).expect("a map");
        let over = Overflow {
            nodes: DEFAULT_MAX_NODES,
            edges: DEFAULT_MAX_EDGES + 5,
        };

        let status = status(&Source::Loaded(document()), Some(drawn), 1.0, Some(over));

        assert!(
            status.ends_with(&format!(
                " · fully folded and still {} connectors over {DEFAULT_MAX_EDGES}",
                over.edges
            )),
            "{status}",
        );
    }

    /// The map is derived as the graph is read and the layered layout only on
    /// demand, so a panel holds one and not the other until it asks for the
    /// layered mode.
    #[test]
    fn a_panel_holds_the_map_before_it_holds_a_layered_layout() {
        let loaded = document();
        let derived = opened(&loaded);
        assert!(derived.drawn(LayoutMode::Map).is_some());
        assert!(derived.drawn(LayoutMode::Layered).is_none());
        assert_eq!(
            status(&Source::Loaded(document()), None, 1.0, None),
            "Laying out…"
        );
    }

    /// A map is opened by zooming rather than by clicking a box, so it offers
    /// nothing to open; the layered layout offers the containers of the graph
    /// the panel draws.
    #[test]
    fn only_the_layered_layout_offers_boxes_to_open() {
        let loaded = document();
        let derived = opened(&loaded);
        assert!(matches!(
            derived.expandable(LayoutMode::Map),
            Expandable::Nothing
        ));
        assert!(matches!(
            derived.expandable(LayoutMode::Layered),
            Expandable::Containers(_)
        ));
    }

    /// Nothing is expandable until there is a graph to expand, whichever mode a
    /// panel has selected.
    #[test]
    fn a_graph_still_being_read_offers_nothing_to_open() {
        for mode in LayoutMode::ALL {
            assert!(matches!(
                Derived::new().expandable(mode),
                Expandable::Nothing
            ));
        }
    }
}
