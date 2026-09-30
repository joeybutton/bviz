use crate::build::build_graph;
use crate::derive::derive_levels;
use bviz_graph_layout::graph::Graph;
use bviz_graph_layout::lod::{DEFAULT_MAX_EDGES, DEFAULT_MAX_NODES, Overflow};
use bviz_graph_layout::metrics::{TargetMetrics, target_metrics};
use bviz_graph_layout::select::labels::LabelIndex;
use bviz_graph_layout::{Capabilities, Facets, Id, MapLayout, Names};
use bviz_ui_components::{Pending, load_in_background};
use bviz_ui_graph_args::Input;
use gpui::{Context, EventEmitter, Task};
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

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
    pub(crate) map: Placement,
    /// The interned node-id table the base graph keys on, of which a panel's
    /// drawing takes a copy to extend with the group ids its quotient pass
    /// mints.
    pub(crate) names: Arc<Names>,
    /// The facets of the base graph's nodes, keyed by the same ids, which a
    /// panel's filter selects on.
    pub(crate) facets: Arc<Facets>,
    /// The canonical labels those nodes belong to, gathered as the graph is read
    /// so that a search matches a pattern over them without walking the graph.
    pub(crate) labels: Arc<LabelIndex>,
    /// The multi-child containers of the base graph, which a layered view offers
    /// to open.
    pub(crate) containers: Arc<HashSet<Id>>,
    /// Which filter axes the inputs this graph was read from support, by which
    /// the toolbar tells a control the inputs cannot answer from one they can.
    pub(crate) capabilities: Capabilities,
}

/// A graph's map, landing after the rest of the read. Every clone shares the
/// one placement, so whatever holds one draws the map as soon as it lands.
#[derive(Clone)]
pub(crate) struct Placement(Arc<OnceLock<Arc<MapLayout>>>);

/// The one handle that lands a [`Placement`]'s map, consumed by landing it, so a
/// placement lands at most once.
pub(crate) struct Landing(Placement);

impl Placement {
    /// A placement whose map has yet to land, and the handle that lands it.
    pub(crate) fn pending() -> (Self, Landing) {
        let placement = Placement(Arc::default());
        (placement.clone(), Landing(placement))
    }

    /// The map, once it has landed.
    pub(crate) fn placed(&self) -> Option<&Arc<MapLayout>> {
        self.0.get()
    }
}

impl Landing {
    pub(crate) fn land(self, map: Arc<MapLayout>) {
        self.0.0.get_or_init(|| map);
    }
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
    /// Goes with the graph it measures, so a measuring still in flight when the
    /// graph is replaced never lands.
    measure: Measure,
}

impl Loaded {
    pub(crate) fn new(shared: Shared, opening: Opening) -> Self {
        Self {
            shared,
            opening,
            measure: Measure::Unasked,
        }
    }
}

/// Whether a graph's layered view starts out drawing the transitive reduction of
/// its edges rather than every edge. The opening expansion is sized against the
/// edges that view draws, so the two are read from one place.
pub(crate) const OPENS_REDUCED: bool = true;

/// A read that has landed but for its map, which is still being placed.
pub(crate) struct Read {
    loaded: Loaded,
    placing: Pending<Arc<MapLayout>>,
    /// Lands what `placing` places in the read's [`Placement`].
    landing: Landing,
}

/// Reads `inputs` and builds the graph, and starts placing its map.
pub(crate) fn build(inputs: Vec<Input>) -> Result<Read, String> {
    let (graph, reduced_base, names, facets, capabilities) = build_graph(&inputs)?;
    // The map derives each of its views against these, so it holds a share in
    // them rather than a copy.
    let (base, reduced_base) = (Arc::new(graph), Arc::new(reduced_base));
    let (map, landing) = Placement::pending();
    let placing = {
        let (base, reduced_base) = (Arc::clone(&base), Arc::clone(&reduced_base));
        Pending::start(move || Ok(Arc::new(bviz_graph_layout::map_layout(base, reduced_base))))
    };
    // A graph no expansion of which fits still has to open at something, and the
    // unexpanded view is the smallest drawing of it there is.
    let (expanded, over_budget) = match bviz_graph_layout::lod::default_expansion(
        &base,
        OPENS_REDUCED.then_some(&reduced_base),
        DEFAULT_MAX_NODES,
        DEFAULT_MAX_EDGES,
    ) {
        Ok(expanded) => (expanded, None),
        Err(overflow) => (HashSet::new(), Some(overflow)),
    };
    let containers = Arc::new(bviz_graph_layout::lod::expandable_containers(&base));
    let labels = Arc::new(LabelIndex::of(&base, &names, &facets));
    Ok(Read {
        loaded: Loaded::new(
            Shared {
                base,
                reduced_base,
                map,
                names: Arc::new(names),
                facets: Arc::new(facets),
                labels,
                containers,
                capabilities,
            },
            Opening {
                expanded,
                over_budget,
            },
        ),
        placing,
        landing,
    })
}

pub(crate) type Source = bviz_ui_components::Source<Loaded>;

/// The read has landed, loaded or failed. Carried as an event because what it
/// calls for turns on the layout mode and filter a view has selected, which the
/// graph does not hold.
pub(crate) struct SourceReplaced;

/// The loaded graph's map has landed in its [`Placement`].
pub(crate) struct MapPlaced;

/// A graph's per-target metrics, a pass over the whole graph that is only
/// taken once a view drawing them asks for it.
enum Measure {
    Unasked,
    Measuring {
        /// Held so that the measuring is not cancelled.
        _work: Task<()>,
    },
    Measured(Arc<[TargetMetrics]>),
}

/// A graph read for viewing, held apart from the panels drawing it so a view of
/// it can close and be shown again without the graph being read or laid out
/// afresh.
pub(crate) struct GraphModel {
    source: Source,
    /// The read in flight, held so that it is not cancelled.
    _work: Task<()>,
    /// The placement of the map in flight once the read has landed, held so
    /// that it is not cancelled.
    _placing: Task<()>,
}

impl EventEmitter<SourceReplaced> for GraphModel {}
impl EventEmitter<MapPlaced> for GraphModel {}

impl GraphModel {
    /// The graph `load` is reading, viewable once the read finishes and failed
    /// where it does not.
    pub(crate) fn reading(load: Pending<Read>, cx: &mut Context<Self>) -> Self {
        Self::new(load_in_background(
            cx,
            move || load.join(),
            |model, result, cx| {
                model.source = match result {
                    Ok(read) => {
                        model._placing = Self::placing(read.placing, read.landing, cx);
                        Source::Loaded(read.loaded)
                    }
                    Err(err) => Source::Failed(err),
                };
                cx.emit(SourceReplaced);
            },
        ))
    }

    /// Waits on `placing` and lands the map it places through `landing`.
    fn placing(
        placing: Pending<Arc<MapLayout>>,
        landing: Landing,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        load_in_background(
            cx,
            move || placing.join(),
            |model, placed, cx| match model.land(placed, landing) {
                Some(map) => {
                    derive_levels(map, cx);
                    cx.emit(MapPlaced);
                }
                None => cx.emit(SourceReplaced),
            },
        )
    }

    /// Lands what placing the loaded graph's map came to: the map, through
    /// `landing`, or a failed read where placing failed. Returns the map where
    /// it landed.
    fn land(
        &mut self,
        placed: Result<Arc<MapLayout>, String>,
        landing: Landing,
    ) -> Option<Arc<MapLayout>> {
        match placed {
            Ok(map) => {
                landing.land(Arc::clone(&map));
                Some(map)
            }
            Err(err) => {
                self.source = Source::Failed(err);
                None
            }
        }
    }

    fn new(work: Task<()>) -> Self {
        Self {
            source: Source::Loading,
            _work: work,
            _placing: Task::ready(()),
        }
    }

    /// Starts measuring the loaded graph's targets off the UI thread, notifying
    /// once they land. Does nothing once started, or while the graph is still
    /// being read.
    pub(crate) fn ensure_metrics(&mut self, cx: &mut Context<Self>) {
        let Source::Loaded(loaded) = &mut self.source else {
            return;
        };
        let Measure::Unasked = loaded.measure else {
            return;
        };
        let (base, facets) = (
            Arc::clone(&loaded.shared.base),
            Arc::clone(&loaded.shared.facets),
        );
        let work = load_in_background(
            cx,
            move || target_metrics(&base, &facets).into(),
            |model, metrics, _| {
                if let Source::Loaded(loaded) = &mut model.source {
                    loaded.measure = Measure::Measured(metrics);
                }
            },
        );
        loaded.measure = Measure::Measuring { _work: work };
    }

    /// The loaded graph's per-target metrics, once measured.
    pub(crate) fn metrics(&self) -> Option<&Arc<[TargetMetrics]>> {
        match &self.loaded()?.measure {
            Measure::Measured(metrics) => Some(metrics),
            _ => None,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Placing runs apart from the read it belongs to, so a panic there fails
    /// the read all the same, and lands no map.
    #[test]
    fn a_placement_that_panics_fails_the_read() {
        let (placement, landing) = Placement::pending();
        let mut model = GraphModel::new(Task::ready(()));
        let placed = Pending::<Arc<MapLayout>>::start(|| panic!("placing the map")).join();

        assert!(model.land(placed, landing).is_none());
        assert!(matches!(model.source(), Source::Failed(_)));
        assert!(placement.placed().is_none());
    }
}
