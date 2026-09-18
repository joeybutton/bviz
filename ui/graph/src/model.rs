use crate::MARGIN;
use crate::build::{self, build_graph};
use crate::mode::LayoutMode;
use bviz_graph_layout::graph::Graph;
use bviz_graph_layout::lod::{DEFAULT_MAX_EDGES, DEFAULT_MAX_NODES, Overflow};
use bviz_graph_layout::{Facets, Id, LaidOut, MapLayout, Names};
use bviz_ui_components::{Pending, load_in_background};
use bviz_ui_graph_args::Input;
use gpui::{Context, EventEmitter, Task};
use std::collections::HashSet;
use std::sync::Arc;

/// The layered drawing on screen, and how many relayouts had landed when it was
/// made, by which it is told from the drawing that replaces it.
struct Layered {
    view: Arc<LaidOut>,
    relayouts: u64,
}

/// A graph that finished loading, in the form every later relayout starts from.
pub(crate) struct Loaded {
    map: Arc<MapLayout>,
    /// The layered layout of the expanded view; absent until that mode is first
    /// selected, and replaced by each relayout.
    layered: Option<Layered>,
    /// The built (pre-quotient) graph, cached so toggling a group re-lays-out
    /// without re-reading or re-parsing the proto.
    base: Arc<Graph>,
    /// The base with its transitively-redundant edges removed, cached so the
    /// reduced view's edges are a stable projection of one fixed skeleton.
    reduced_base: Arc<Graph>,
    /// The interned node-id table the base graph keys on, threaded into each
    /// relayout so the quotient pass can mint group ids.
    names: Names,
    /// The facets of the base graph's nodes, keyed by the same ids, which a
    /// filter of the view selects on.
    #[expect(
        dead_code,
        reason = "held with the graph it describes; no view selects on a facet"
    )]
    facets: Arc<Facets>,
    /// Container and group ids expanded in the layered layout last requested;
    /// every other container folds its subtree away so the layered layout only
    /// processes the visible view.
    expanded: HashSet<Id>,
    /// The size of the view the graph opened with where it was past the caps
    /// that bound it, absent where it fit.
    #[expect(
        dead_code,
        reason = "recorded with the graph it measures; no view reports the opening size"
    )]
    over_budget: Option<Overflow>,
    /// The set a layered layout's [`Expandable`] is built from.
    containers: Arc<HashSet<Id>>,
    /// Whether the layered layout last requested draws the transitive reduction
    /// of the edges rather than every edge.
    reduced: bool,
    /// Whether a layered layout is running, so a view can show a progress
    /// indicator while the prior layout stays interactive.
    relaying: bool,
}

/// Derives every view of `map` off the UI thread, coarsest first.
///
/// A map derives a view the first time a camera calls for one, which is what
/// keeps the wait before the first frame to the single view that frame draws.
/// The reader reaches the rest by zooming, and a view derived under that gesture
/// would hold the frame it was made for; deriving them in the background instead
/// leaves the zoom nothing to wait on. A view already derived costs nothing to
/// ask for again, so this races the camera harmlessly.
fn derive_levels(map: Arc<MapLayout>, cx: &mut Context<GraphModel>) {
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
/// view is redone exactly when the view moves under it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewKey {
    /// A layered drawing, told from the one the next relayout puts in its place
    /// by the count of relayouts that had landed when it was made.
    Layered(u64),
    /// A map level, which a map derives once and never replaces.
    Map(usize),
}

/// A share in the layout a mode draws, cheap to clone into a `'static` paint
/// closure. A map resolves to a view only once the camera is known, so the level
/// a frame draws is picked at the same scale it paints at.
#[derive(Clone)]
pub(crate) enum Drawn {
    Layered { view: Arc<LaidOut>, relayouts: u64 },
    Map(Arc<MapLayout>),
}

impl Drawn {
    /// The view drawn at `zoom` screen pixels per layout unit.
    pub(crate) fn view(&self, zoom: f32) -> &LaidOut {
        match self {
            Self::Layered { view, .. } => view,
            Self::Map(map) => map.level(map.level_for_scale(zoom)),
        }
    }

    /// Identifies the view [`view`](Self::view) returns at `zoom`.
    pub(crate) fn view_key(&self, zoom: f32) -> ViewKey {
        match self {
            Self::Layered { relayouts, .. } => ViewKey::Layered(*relayouts),
            Self::Map(map) => ViewKey::Map(map.level_for_scale(zoom)),
        }
    }

    /// Which of a map's levels [`view`](Self::view) draws at `zoom`; `None` for a
    /// layered layout, which has no pyramid.
    pub(crate) fn level(&self, zoom: f32) -> Option<Level> {
        match self {
            Self::Layered { .. } => None,
            Self::Map(map) => Some(Level {
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
            Self::Map(map) => (map.width(), map.height()),
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
    /// The multi-child containers of the base graph, plus every collapsed group
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

/// Reads `inputs`, builds the graph and derives the map pyramid.
pub(crate) fn build(inputs: Vec<Input>) -> Result<Loaded, String> {
    let (graph, reduced_base, names, facets) = build_graph(&inputs)?;
    // A graph no expansion of which fits still has to open at something, and the
    // unexpanded view is the smallest drawing of it there is.
    let (expanded, over_budget) = match bviz_graph_layout::lod::default_expansion(
        &graph,
        DEFAULT_MAX_NODES,
        DEFAULT_MAX_EDGES,
    ) {
        Ok(expanded) => (expanded, None),
        Err(overflow) => (HashSet::new(), Some(overflow)),
    };
    let containers = Arc::new(bviz_graph_layout::lod::expandable_containers(&graph));
    // The map derives each of its views against these, so it holds a share in
    // them rather than a copy.
    let (base, reduced_base) = (Arc::new(graph), Arc::new(reduced_base));
    let map = bviz_graph_layout::map_layout(Arc::clone(&base), Arc::clone(&reduced_base));
    Ok(Loaded {
        map: Arc::new(map),
        layered: None,
        base,
        reduced_base,
        names,
        facets: Arc::new(facets),
        expanded,
        over_budget,
        containers,
        reduced: true,
        relaying: false,
    })
}

pub(crate) type Source = bviz_ui_components::Source<Loaded>;

/// The read has landed, loaded or failed. Carried as an event because what it
/// calls for turns on the layout mode a view has selected, which the graph does
/// not hold.
pub(crate) struct SourceReplaced;

/// A layered layout has landed. Carried as an event because what it calls for is
/// view-local — easing the drawing on screen onto the new one, settling the
/// camera onto what the reader was looking at — and owed once per landing.
pub(crate) struct LayoutLanded {
    /// The drawing this one takes the place of; absent for the first layered
    /// layout of a graph.
    pub(crate) replaced: Option<Arc<LaidOut>>,
    pub(crate) current: Arc<LaidOut>,
}

/// A graph read for viewing, held apart from the panels drawing it so a view of
/// it can close and be shown again without the graph being read or laid out
/// afresh. Every layout it holds is produced off the UI thread.
pub(crate) struct GraphModel {
    source: Source,
    /// The read or relayout in flight, held so that it is not cancelled.
    _work: Task<()>,
}

impl EventEmitter<SourceReplaced> for GraphModel {}
impl EventEmitter<LayoutLanded> for GraphModel {}

impl GraphModel {
    /// The graph `load` is reading, viewable once the read finishes and failed
    /// where it does not.
    pub(crate) fn reading(load: Pending<Loaded>, cx: &mut Context<Self>) -> Self {
        Self::new(load_in_background(
            cx,
            move || load.join(),
            |model, result, cx| {
                model.source = match result {
                    Ok(loaded) => {
                        derive_levels(Arc::clone(&loaded.map), cx);
                        Source::Loaded(loaded)
                    }
                    Err(err) => Source::Failed(err),
                };
                cx.emit(SourceReplaced);
            },
        ))
    }

    fn new(work: Task<()>) -> Self {
        Self {
            source: Source::Loading,
            _work: work,
        }
    }

    pub(crate) fn source(&self) -> &Source {
        &self.source
    }

    fn loaded(&self) -> Option<&Loaded> {
        match &self.source {
            Source::Loaded(loaded) => Some(loaded),
            _ => None,
        }
    }

    /// The layout `mode` draws, or `None` while the graph or that mode's layout
    /// is still being computed.
    pub(crate) fn drawn(&self, mode: LayoutMode) -> Option<Drawn> {
        let loaded = self.loaded()?;
        match mode {
            LayoutMode::Map => Some(Drawn::Map(Arc::clone(&loaded.map))),
            LayoutMode::Layered => loaded.layered.as_ref().map(|l| Drawn::Layered {
                view: Arc::clone(&l.view),
                relayouts: l.relayouts,
            }),
        }
    }

    /// The boxes `mode`'s layout lets the user open or close.
    pub(crate) fn expandable(&self, mode: LayoutMode) -> Expandable {
        match (mode, self.loaded()) {
            (LayoutMode::Layered, Some(loaded)) => {
                Expandable::Containers(Arc::clone(&loaded.containers))
            }
            _ => Expandable::Nothing,
        }
    }

    pub(crate) fn relaying(&self) -> bool {
        self.loaded().is_some_and(|loaded| loaded.relaying)
    }

    pub(crate) fn reduced(&self) -> bool {
        self.loaded().is_some_and(|loaded| loaded.reduced)
    }

    pub(crate) fn status(&self, mode: LayoutMode, zoom: f32) -> String {
        self.source.status("No graph", |_| match self.drawn(mode) {
            None => "Laying out…".into(),
            Some(drawn) => {
                let g = drawn.view(zoom);
                let level = drawn
                    .level(zoom)
                    .map(|l| format!("level {}/{} · ", l.index + 1, l.count))
                    .unwrap_or_default();
                format!(
                    "{level}{} boxes · {} connectors · {}×{}",
                    g.boxes.len(),
                    g.connectors.len(),
                    g.width.round() as i64,
                    g.height.round() as i64,
                )
            }
        })
    }

    /// Lays `mode` out where it has no layout yet and none is already running.
    pub(crate) fn ensure_layout(&mut self, mode: LayoutMode, cx: &mut Context<Self>) {
        if self.drawn(mode).is_none() && !self.relaying() {
            self.relayout(cx);
        }
    }

    /// Flips whether `id` is expanded and lays the graph out at the new expansion.
    pub(crate) fn toggle_expansion(&mut self, id: Id, cx: &mut Context<Self>) {
        let Source::Loaded(loaded) = &mut self.source else {
            return;
        };
        if !loaded.expanded.remove(&id) {
            loaded.expanded.insert(id);
        }
        self.relayout(cx);
    }

    /// Flips whether the layered layout keeps every edge or only the transitive
    /// reduction, and lays the graph out over the edge set that leaves.
    pub(crate) fn toggle_reduce(&mut self, cx: &mut Context<Self>) {
        let Source::Loaded(loaded) = &mut self.source else {
            return;
        };
        loaded.reduced = !loaded.reduced;
        self.relayout(cx);
    }

    /// Lays the cached base graph out at the current expansion off the UI thread
    /// and adopts the result as the layered layout, announcing it with
    /// [`LayoutLanded`] so the views showing it can ease onto it.
    fn relayout(&mut self, cx: &mut Context<Self>) {
        let Source::Loaded(loaded) = &mut self.source else {
            return;
        };
        let base = Arc::clone(&loaded.base);
        let reduced_base = Arc::clone(&loaded.reduced_base);
        // The relayout mints group ids against the interned-name table, so it
        // extends a clone and hands it back to be adopted.
        let mut names = loaded.names.clone();
        let previous = loaded.layered.as_ref().map(|l| Arc::clone(&l.view));
        let expanded = loaded.expanded.clone();
        let reduced = loaded.reduced;
        loaded.relaying = true;
        cx.notify();
        self._work = load_in_background(
            cx,
            move || {
                let laid_out = build::relayout(
                    &base,
                    &reduced_base,
                    &expanded,
                    &mut names,
                    reduced,
                    previous.as_deref().map(|previous| &previous.anchors),
                );
                (laid_out, names)
            },
            |model, (laid_out, names), cx| {
                let Source::Loaded(loaded) = &mut model.source else {
                    return;
                };
                let current = Arc::new(laid_out);
                loaded.names = names;
                let relayouts = loaded.layered.as_ref().map_or(0, |l| l.relayouts) + 1;
                let replaced = loaded
                    .layered
                    .replace(Layered {
                        view: Arc::clone(&current),
                        relayouts,
                    })
                    .map(|l| l.view);
                loaded.relaying = false;
                cx.emit(LayoutLanded { replaced, current });
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_graph_layout::{EdgeKind, Kind};

    /// A graph whose read has not finished, for exercising what a view reads
    /// off it before one has.
    fn reading() -> GraphModel {
        GraphModel::new(Task::ready(()))
    }

    /// A two-node graph with one dependency between them, prepared the way
    /// [`build`] prepares a graph read from a proto.
    fn loaded() -> GraphModel {
        let mut names = Names::default();
        let (from, to) = (names.intern("//a"), names.intern("//b"));
        let mut base = Graph::new();
        base.add_node(from, "a", Kind::Target, None);
        base.add_node(to, "b", Kind::Target, None);
        base.add_edge(from, to, EdgeKind::Query);
        let reduced_base = bviz_graph_layout::reduce::transitive_reduction(&base);
        let (base, reduced_base) = (Arc::new(base), Arc::new(reduced_base));
        let map = bviz_graph_layout::map_layout(Arc::clone(&base), Arc::clone(&reduced_base));
        let containers = Arc::new(bviz_graph_layout::lod::expandable_containers(&base));
        let mut model = reading();
        model.source = Source::Loaded(Loaded {
            map: Arc::new(map),
            layered: None,
            base,
            reduced_base,
            names,
            facets: Arc::new(Facets::default()),
            expanded: HashSet::new(),
            over_budget: None,
            containers,
            reduced: true,
            relaying: false,
        });
        model
    }

    /// The status line is drawn from the graph alone, so it reads the same
    /// whether or not a panel is showing one.
    #[test]
    fn a_graph_still_being_read_says_so() {
        assert_eq!(reading().status(LayoutMode::Map, 1.0), "Loading…");
    }

    #[test]
    fn a_graph_that_could_not_be_read_names_what_is_absent() {
        let mut model = reading();
        model.source = Source::Failed("aquery.pb: no such file".into());
        assert_eq!(model.status(LayoutMode::Map, 1.0), "No graph");
    }

    /// Two leaves with nothing to open stand at the one map level, so the whole
    /// graph is described whatever the zoom. The extent trailing the counts is
    /// the layout engine's to decide.
    #[test]
    fn a_loaded_graph_describes_the_map_it_draws() {
        let status = loaded().status(LayoutMode::Map, 1.0);
        assert!(
            status.starts_with("level 1/1 · 2 boxes · 1 connectors · "),
            "{status}"
        );
    }

    /// The map is derived as the graph is read and the layered layout only on
    /// demand, so a graph holds one and not the other until a view asks for
    /// the layered mode. Which of the two a view has selected does not move
    /// either.
    #[test]
    fn a_loaded_graph_holds_the_map_before_it_holds_a_layered_layout() {
        let model = loaded();
        assert!(model.drawn(LayoutMode::Map).is_some());
        assert!(model.drawn(LayoutMode::Layered).is_none());
        assert_eq!(model.status(LayoutMode::Layered, 1.0), "Laying out…");
    }

    /// A map is opened by zooming rather than by clicking a box, so it offers
    /// nothing to open; the layered layout offers the containers of the graph
    /// read.
    #[test]
    fn only_the_layered_layout_offers_boxes_to_open() {
        let model = loaded();
        assert!(matches!(
            model.expandable(LayoutMode::Map),
            Expandable::Nothing
        ));
        assert!(matches!(
            model.expandable(LayoutMode::Layered),
            Expandable::Containers(_)
        ));
    }

    /// Nothing is expandable until there is a graph to expand, whichever mode a
    /// view has selected.
    #[test]
    fn a_graph_still_being_read_offers_nothing_to_open() {
        for mode in LayoutMode::ALL {
            assert!(matches!(reading().expandable(mode), Expandable::Nothing));
        }
    }
}
