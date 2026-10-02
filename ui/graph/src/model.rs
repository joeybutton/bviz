use crate::GraphPanel;
use crate::build::{build_graph, loaded_inputs};
use crate::derive::derive_levels;
use crate::status::Statuses;
use bviz_graph_layout::graph::Graph;
use bviz_graph_layout::lod::{DEFAULT_MAX_EDGES, DEFAULT_MAX_NODES, Overflow};
use bviz_graph_layout::metrics::{Measurement, Scope, measure};
use bviz_graph_layout::select::Filter;
use bviz_graph_layout::select::labels::LabelIndex;
use bviz_graph_layout::{Capabilities, Facets, Id, InputSet, Joint, MapLayout, Names, Provenance};
use bviz_invocation::{Configurations, Revision, Session};
use bviz_ui_components::{Pending, load_in_background};
use bviz_ui_graph_args::GraphSource;
use gpui::{Context, Entity, EntityId, EventEmitter, Subscription, Task};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
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
    /// The inputs that built each node of the base graph.
    pub(crate) provenance: Arc<Provenance>,
    /// The inputs the graph was read from.
    pub(crate) inputs: InputSet,
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
    measures: HashMap<Scope, Measure>,
    /// What the build of the graph's invocation reports of its nodes.
    statuses: Statuses,
    /// Which of its session's outcomes `statuses` says that of: those of a
    /// session no build has reported to, until a report is taken up.
    taken_up: Revision,
}

impl Loaded {
    pub(crate) fn new(shared: Shared, opening: Opening) -> Self {
        Self {
            shared,
            opening,
            measures: HashMap::new(),
            statuses: Statuses::default(),
            taken_up: Revision::default(),
        }
    }

    pub(crate) fn statuses(&self) -> &Statuses {
        &self.statuses
    }

    /// Takes up what the build of `session` reports of the graph's nodes, and
    /// says whether that changed the status of any. Outcomes already taken up
    /// are not read again.
    fn report(&mut self, session: &Session) -> bool {
        let revision = session.outcomes_revision();
        if self.taken_up == revision {
            return false;
        }
        self.taken_up = revision;
        let (base, names) = (&self.shared.base, &self.shared.names);
        let statuses = Statuses::resolve(session.outcomes(), base, names);
        let changed = self.statuses != statuses;
        self.statuses = statuses;
        changed
    }
}

/// The mnemonic `facets` gives each configuration, by its checksum. A
/// configuration the graph's inputs state no mnemonic for is left out.
fn configurations(facets: &Facets) -> Configurations {
    let strings = facets.strings();
    let configs = facets.configs();
    let mut described = Configurations::with_capacity(configs.size_hint().0);
    for (config, entry) in configs {
        if let Some(mnemonic) = entry.mnemonic {
            described.insert(
                strings.resolve(config.sym()).to_owned(),
                strings.resolve(mnemonic).to_owned(),
            );
        }
    }
    described
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

/// Reads `source` and builds the graph, and starts placing its map.
pub(crate) fn build(source: GraphSource) -> Result<Read, String> {
    let inputs = loaded_inputs(&source);
    let (
        Joint {
            graph,
            names,
            facets,
            capabilities,
            provenance,
        },
        reduced_base,
    ) = build_graph(&source.inputs())?;
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
                provenance: Arc::new(provenance),
                inputs,
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

/// A view of the graph named a node for the graph panel it defers to to show.
pub(crate) struct RevealRequested {
    pub(crate) panel: EntityId,
    pub(crate) node: Id,
}

/// The graph panel the graph's other views defer to.
struct Deferred {
    panel: EntityId,
    /// The filter the panel draws through.
    filter: Filter,
    /// Forgets the panel once it is released.
    _released: Subscription,
}

/// The loaded graph's metrics in `scope` have landed.
pub(crate) struct MetricsLanded {
    pub(crate) scope: Scope,
}

/// The graph's other views may have another [`GraphModel::filter`] to draw
/// through.
pub(crate) struct FilterChanged;

/// A graph's metrics in one scope, a pass over the whole graph that is only
/// taken once a view drawing them asks for it.
enum Measure {
    Measuring {
        /// Dropping it gives the measuring up: a pass not yet started never
        /// runs, and one running lands nothing.
        _work: Task<()>,
    },
    Measured(Arc<Measurement>),
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
    /// The graph panel opened, shown or filtered last, for as long as it is
    /// open.
    deferred: Option<Deferred>,
    joined: Option<Joined>,
}

/// The invocation a graph is opened as part of.
struct Joined {
    /// Held for as long as the graph is: a session nothing holds is released,
    /// and tells its observers nothing further.
    session: Entity<Session>,
    /// Takes up what the build reports whenever the session changes.
    _changed: Subscription,
}

impl EventEmitter<SourceReplaced> for GraphModel {}
impl EventEmitter<MapPlaced> for GraphModel {}
impl EventEmitter<RevealRequested> for GraphModel {}
impl EventEmitter<MetricsLanded> for GraphModel {}
impl EventEmitter<FilterChanged> for GraphModel {}

impl GraphModel {
    /// The graph `load` is reading, viewable once the read finishes and failed
    /// where it does not.
    pub(crate) fn reading(
        load: Pending<Read>,
        session: Option<Entity<Session>>,
        cx: &mut Context<Self>,
    ) -> Self {
        // The build's outcomes can land on either side of the graph, so they
        // are taken up as the graph lands and again whenever the session
        // changes.
        let joined = session.map(|session| Joined {
            _changed: cx.observe(&session, |model, _, cx| model.take_up(cx)),
            session,
        });
        let work = load_in_background(
            cx,
            move || {
                let read = load.join()?;
                let described = configurations(&read.loaded.shared.facets);
                Ok((read, described))
            },
            move |model, result: Result<(Read, Configurations), String>, cx| {
                model.source = match result {
                    Ok((read, described)) => {
                        if let Some(joined) = &model.joined {
                            joined.session.update(cx, |session, cx| {
                                session.add_configurations(described);
                                cx.notify();
                            });
                        }
                        model._placing = Self::placing(read.placing, read.landing, cx);
                        Source::Loaded(read.loaded)
                    }
                    Err(err) => Source::Failed(err),
                };
                model.take_up(cx);
                cx.emit(SourceReplaced);
            },
        );
        Self {
            joined,
            ..Self::new(work)
        }
    }

    /// Takes up what the session's build reports of the loaded graph.
    fn take_up(&mut self, cx: &mut Context<Self>) {
        if let (Some(joined), Source::Loaded(loaded)) = (&self.joined, &mut self.source)
            && loaded.report(joined.session.read(cx))
        {
            cx.notify();
        }
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
            deferred: None,
            joined: None,
        }
    }

    /// Takes `panel`, drawing through `filter`, as the graph panel the graph's
    /// other views defer to.
    pub(crate) fn defer_to(
        &mut self,
        panel: &Entity<GraphPanel>,
        filter: Filter,
        cx: &mut Context<Self>,
    ) {
        let id = panel.entity_id();
        match &mut self.deferred {
            Some(deferred) if deferred.panel == id => {
                if deferred.filter == filter {
                    return;
                }
                deferred.filter = filter;
            }
            _ => {
                let released = cx.observe_release(panel, |model, _, cx| {
                    model.deferred = None;
                    cx.emit(FilterChanged);
                    cx.notify();
                });
                self.deferred = Some(Deferred {
                    panel: id,
                    filter,
                    _released: released,
                });
                cx.notify();
            }
        }
        cx.emit(FilterChanged);
    }

    /// Whether a graph panel is open to show a node another view names.
    pub(crate) fn reveals(&self) -> bool {
        self.deferred.is_some()
    }

    /// The filter the graph's other views draw their share of it through: the
    /// one the graph panel they defer to draws through, absent where no graph
    /// panel is open.
    pub(crate) fn filter(&self) -> Option<&Filter> {
        self.deferred.as_ref().map(|deferred| &deferred.filter)
    }

    /// Has the loaded graph measured off the UI thread in each scope of
    /// `wanted` not yet asked for, emitting [`MetricsLanded`] as each lands, and
    /// gives up measuring any other scope, whose pass would only hold the
    /// workers from those wanted. Metrics already measured are kept. Does
    /// nothing while the graph is still being read.
    pub(crate) fn want_metrics(&mut self, wanted: &[Scope], cx: &mut Context<Self>) {
        let Source::Loaded(loaded) = &mut self.source else {
            return;
        };
        forget_unwanted(&mut loaded.measures, wanted);
        for &scope in wanted {
            let Entry::Vacant(unasked) = loaded.measures.entry(scope) else {
                continue;
            };
            let shared = loaded.shared.clone();
            let work = load_in_background(
                cx,
                move || {
                    let (base, facets) = (&shared.base, &shared.facets);
                    let names = &shared.names;
                    Arc::new(measure(base, names, facets, &shared.provenance, scope))
                },
                move |model, metrics, cx| {
                    if let Source::Loaded(loaded) = &mut model.source {
                        loaded.measures.insert(scope, Measure::Measured(metrics));
                        cx.emit(MetricsLanded { scope });
                    }
                },
            );
            unasked.insert(Measure::Measuring { _work: work });
        }
    }

    /// The loaded graph's metrics in `scope`, once measured.
    pub(crate) fn metrics(&self, scope: Scope) -> Option<&Arc<Measurement>> {
        match self.loaded()?.measures.get(&scope)? {
            Measure::Measured(metrics) => Some(metrics),
            Measure::Measuring { .. } => None,
        }
    }

    /// Asks the graph panel the graph's other views defer to to show the node
    /// `id`, where one is open.
    pub(crate) fn reveal(&mut self, id: Id, cx: &mut Context<Self>) {
        if let Some(deferred) = &self.deferred {
            cx.emit(RevealRequested {
                panel: deferred.panel,
                node: id,
            });
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

/// Drops each measuring of `measures` whose scope `wanted` leaves out.
fn forget_unwanted(measures: &mut HashMap<Scope, Measure>, wanted: &[Scope]) {
    measures
        .retain(|scope, measure| matches!(measure, Measure::Measured(_)) || wanted.contains(scope));
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_graph_layout::facets::Tri;
    use bviz_graph_layout::graph::Node;
    use bviz_graph_layout::metrics::{Sizing, Unit};
    use bviz_graph_layout::{Input, Kind};
    use bviz_invocation::{BuildOutcome, ConfiguredTarget, Outcomes};

    #[test]
    fn a_graph_describes_its_configurations_by_checksum() {
        let mut facets = Facets::default();
        facets.intern_config("55817e2200e2", Some("k8-fastbuild"), Tri::No);
        facets.intern_config("0c3d9a7714be", Some("k8-opt-exec"), Tri::Yes);
        facets.intern_config("91e24984663c", None, Tri::No);

        let described = configurations(&facets);
        let name = |checksum| described.name(checksum);
        assert_eq!(name("55817e2200e2").as_deref(), Some("k8-fastbuild"));
        assert_eq!(name("0c3d9a7714be").as_deref(), Some("k8-opt-exec"));
        assert_eq!(name("91e24984663c"), None);
    }

    /// The graph of the one target `//app:bin`, as a read lands it.
    fn loaded() -> Loaded {
        let mut names = Names::default();
        let mut base = Graph::new();
        let bin = names.intern("target://app:bin");
        base.add_node(Node::new(bin, "bin", Kind::Target, None));
        let reduced_base = bviz_graph_layout::reduce::transitive_reduction(&base);
        let containers = Arc::new(bviz_graph_layout::lod::expandable_containers(&base));
        let (base, reduced_base) = (Arc::new(base), Arc::new(reduced_base));
        let facets = Arc::new(Facets::default());
        let labels = Arc::new(LabelIndex::of(&base, &names, &facets));
        Loaded::new(
            Shared {
                base,
                reduced_base,
                map: Placement::pending().0,
                names: Arc::new(names),
                facets,
                labels,
                containers,
                capabilities: bviz_graph_layout::capability::CapabilitiesBuilder::default().seal(),
                provenance: Arc::default(),
                inputs: Input::Query.into(),
            },
            Opening {
                expanded: HashSet::new(),
                over_budget: None,
            },
        )
    }

    fn reporting(outcome: BuildOutcome) -> Outcomes {
        let mut outcomes = Outcomes::default();
        outcomes.report_build(ConfiguredTarget::new("//app:bin".to_owned(), None), outcome);
        outcomes
    }

    /// A session changes for more than its outcomes, and outcomes can be
    /// reported that say what the last ones did.
    #[test]
    fn a_graph_takes_up_the_outcomes_its_session_holds_once_per_report_of_them() {
        let mut loaded = loaded();
        let mut session = Session::default();
        assert!(!loaded.report(&session));
        assert_eq!(loaded.statuses, Statuses::default());

        session.report_outcomes(reporting(BuildOutcome::NotBuilt));
        assert!(loaded.report(&session));
        assert_ne!(loaded.statuses, Statuses::default());
        assert!(!loaded.report(&session));

        session.add_configurations(Configurations::default());
        assert!(!loaded.report(&session));

        session.report_outcomes(reporting(BuildOutcome::NotBuilt));
        assert!(!loaded.report(&session));
        session.report_outcomes(reporting(BuildOutcome::Built));
        assert!(loaded.report(&session));
    }

    /// Outcomes reported before the graph is read are the first it takes up.
    #[test]
    fn a_graph_takes_up_outcomes_reported_before_it_landed() {
        let mut session = Session::default();
        session.report_outcomes(reporting(BuildOutcome::Built));
        let mut loaded = loaded();
        assert!(loaded.report(&session));
        assert_ne!(loaded.statuses, Statuses::default());
    }

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

    /// A scope no longer wanted stops being measured, but what was measured of
    /// it stays for when it is wanted again.
    #[test]
    fn an_unwanted_measuring_is_dropped_and_a_landed_measure_kept() {
        let scope = |input: Input| Scope::new(Unit::Target, input.into()).unwrap();
        let (landed, dropped, kept) = (
            scope(Input::Query),
            scope(Input::Cquery),
            scope(Input::Aquery),
        );
        let measuring = || Measure::Measuring {
            _work: Task::ready(()),
        };
        let measured = Measure::Measured(Arc::new(Measurement {
            scope: landed,
            points: Vec::new(),
            sizing: Sizing::FanIn,
        }));
        let mut measures = HashMap::from([
            (landed, measured),
            (dropped, measuring()),
            (kept, measuring()),
        ]);

        forget_unwanted(&mut measures, &[kept]);

        assert!(matches!(measures.get(&landed), Some(Measure::Measured(_))));
        assert!(!measures.contains_key(&dropped));
        assert!(matches!(
            measures.get(&kept),
            Some(Measure::Measuring { .. })
        ));
    }
}
