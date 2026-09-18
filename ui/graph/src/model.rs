use crate::build::build_graph;
use crate::derive::derive_levels;
use bviz_graph_layout::graph::Graph;
use bviz_graph_layout::lod::{DEFAULT_MAX_EDGES, DEFAULT_MAX_NODES, Overflow};
use bviz_graph_layout::{Capabilities, Facets, Id, MapLayout, Names};
use bviz_ui_components::{Pending, load_in_background};
use bviz_ui_graph_args::Input;
use gpui::{Context, EventEmitter, Task};
use std::collections::HashSet;
use std::sync::Arc;

/// The graph a document holds, as it was read: whole, unfiltered, and shared by
/// every panel drawing it. Cheap to clone into a panel's background derivation,
/// which restricts it to what that panel's filter draws.
#[derive(Clone)]
pub(crate) struct Shared {
    /// The built (pre-quotient) graph, cached so toggling a group re-lays-out
    /// without re-reading or re-parsing the proto.
    pub(crate) base: Arc<Graph>,
    /// The base with its transitively-redundant edges removed, cached so the
    /// reduced view's edges are a stable projection of one fixed skeleton.
    pub(crate) reduced_base: Arc<Graph>,
    /// The placement every view of the whole graph is drawn in.
    pub(crate) map: Arc<MapLayout>,
    /// The interned node-id table the base graph keys on, which each layout
    /// extends with the group ids its quotient pass mints.
    pub(crate) names: Names,
    /// The facets of the base graph's nodes, keyed by the same ids, which a
    /// panel's filter selects on.
    pub(crate) facets: Arc<Facets>,
    /// The multi-child containers of the base graph, which a layered view offers
    /// to open.
    pub(crate) containers: Arc<HashSet<Id>>,
    /// Which filter axes the inputs this graph was read from support, by which
    /// the toolbar tells a control the inputs cannot answer from one they can.
    pub(crate) capabilities: Capabilities,
}

/// The view a panel opens a freshly read graph at.
#[derive(Clone)]
pub(crate) struct Opening {
    /// Container ids to expand, sized so the first drawing stays legible.
    pub(crate) expanded: HashSet<Id>,
    /// The size of that view where it is past the caps that bound it, absent
    /// where it fits.
    pub(crate) over_budget: Option<Overflow>,
}

/// A graph that finished loading, in the form every panel's derivation starts
/// from.
pub(crate) struct Loaded {
    pub(crate) shared: Shared,
    pub(crate) opening: Opening,
}

/// Whether a graph's layered view starts out drawing the transitive reduction of
/// its edges rather than every edge. The opening expansion is sized against the
/// edges that view draws, so the two are read from one place.
pub(crate) const OPENS_REDUCED: bool = true;

/// Reads `inputs`, builds the graph and derives the map pyramid.
pub(crate) fn build(inputs: Vec<Input>) -> Result<Loaded, String> {
    let (graph, reduced_base, names, facets, capabilities) = build_graph(&inputs)?;
    // A graph no expansion of which fits still has to open at something, and the
    // unexpanded view is the smallest drawing of it there is.
    let (expanded, over_budget) = match bviz_graph_layout::lod::default_expansion(
        &graph,
        OPENS_REDUCED.then_some(&reduced_base),
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
        shared: Shared {
            base,
            reduced_base,
            map: Arc::new(map),
            names,
            facets: Arc::new(facets),
            containers,
            capabilities,
        },
        opening: Opening {
            expanded,
            over_budget,
        },
    })
}

pub(crate) type Source = bviz_ui_components::Source<Loaded>;

/// The read has landed, loaded or failed. Carried as an event because what it
/// calls for turns on the layout mode and filter a view has selected, which the
/// graph does not hold.
pub(crate) struct SourceReplaced;

/// A graph read for viewing, held apart from the panels drawing it so a view of
/// it can close and be shown again without the graph being read or laid out
/// afresh.
pub(crate) struct GraphModel {
    source: Source,
    /// The read in flight, held so that it is not cancelled.
    _work: Task<()>,
}

impl EventEmitter<SourceReplaced> for GraphModel {}

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
                        derive_levels(Arc::clone(&loaded.shared.map), cx);
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

    pub(crate) fn loaded(&self) -> Option<&Loaded> {
        match &self.source {
            Source::Loaded(loaded) => Some(loaded),
            _ => None,
        }
    }
}
