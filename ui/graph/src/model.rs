use crate::GraphPanel;
use crate::build::{build_graph, loaded_inputs};
use crate::derive::derive_levels;
use crate::described::DescribedActions;
use crate::playback::NodeStates;
use crate::point::ConfigNames;
use crate::report::Report;
use crate::status::{Holding, Landings, Nodes, Statuses};
use crate::timecard::Timecard;
use crate::timing::{NodeTimes, Untimed, landings};
use bviz_graph_layout::dominators::{Dominators, Roots, ScopeGraph};
use bviz_graph_layout::graph::Graph;
use bviz_graph_layout::lod::{DEFAULT_MAX_EDGES, DEFAULT_MAX_NODES, Overflow};
use bviz_graph_layout::metrics::{Measurement, Scope, measure};
use bviz_graph_layout::select::Filter;
use bviz_graph_layout::select::labels::LabelIndex;
use bviz_graph_layout::target_set::TargetIndex;
use bviz_graph_layout::{Capabilities, Facets, Id, InputSet, Joint, MapLayout, Names, Provenance};
use bviz_invocation::{Configurations, EpochMicros, Folded, Known, Revision, Session};
use bviz_ui_components::{Pending, load_in_background};
use bviz_ui_graph_args::GraphSource;
use bviz_ui_playback::{Clock, DocumentClock};
use gpui::{App, Context, Entity, EntityId, EventEmitter, Subscription, Task};
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::marker::PhantomData;
use std::sync::{Arc, LazyLock, OnceLock};

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
    /// How the configurations the facets name are named apart.
    pub(crate) config_names: Arc<ConfigNames>,
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
    measures: Passes<Scope, Measurement>,
    /// How the graph's nodes hold each other, which every report on them is
    /// landed and rolled up through.
    holding: Holding,
    /// The playback clock's time the outcomes are shown at, as last told.
    at: Option<EpochMicros>,
    /// The dated outcomes of the session's report of them at the revision
    /// it pairs, folded as far as they were last shown.
    folded: (Revision, Folded),
    /// What the build of the graph's invocation reports of its nodes.
    statuses: Statuses,
    /// What the build of the graph's invocation reported, as the card over
    /// the graph lists it.
    reported: Report,
    /// Which of its session's outcomes `statuses` and `reported` are read
    /// from: those of a session no build has reported to, until a report is
    /// taken up.
    taken_up: OutcomesShown,
    /// How long the actions the invocation's profile recorded took, placed on
    /// the graph's nodes.
    times: Result<NodeTimes, Untimed>,
    /// Where the actions the invocation's profile recorded spent their time,
    /// as the card over the graph lists it.
    timecard: Timecard,
    timed: Revision,
    dominators: Passes<TreeKey, Dominators>,
    scope_graphs: HashMap<Scope, Prepared<ScopeGraph>>,
    /// The graph's targets by repository, package and name.
    targets: Prepared<TargetIndex>,
    /// Where the execution attempts the invocation's profile recorded stood
    /// at each instant, placed on the graph's nodes; `None` where none lands.
    states: Option<NodeStates>,
    attempted: Revision,
}

impl Loaded {
    pub(crate) fn new(shared: Shared, opening: Opening) -> Self {
        let targets = Prepared::new(&shared, |shared| {
            let labels = Arc::clone(&shared.labels);
            TargetIndex::of(labels, &shared.base, &shared.facets, &shared.capabilities)
        });
        Self {
            targets,
            holding: Holding::of(&shared.base, &shared.names),
            at: None,
            folded: Default::default(),
            shared,
            opening,
            measures: Passes::keeping_all(),
            statuses: Statuses::default(),
            reported: Report::default(),
            taken_up: OutcomesShown::default(),
            times: Err(Untimed::NoProfile),
            timecard: Timecard::default(),
            timed: Revision::default(),
            dominators: Passes::keeping_recent(RECENT_TREES),
            scope_graphs: HashMap::new(),
            states: None,
            attempted: Revision::default(),
        }
    }

    pub(crate) fn statuses(&self) -> &Statuses {
        &self.statuses
    }

    pub(crate) fn reported(&self) -> &Report {
        &self.reported
    }

    /// Which of its session's outcomes the [statuses](Self::statuses) say
    /// that of, so that a reader keeping it can tell whether they are still
    /// those it read.
    pub(crate) fn taken_up(&self) -> OutcomesShown {
        self.taken_up
    }

    /// Whether the outcomes shown are those known at the playback clock's
    /// time, and so change as it moves.
    pub(crate) fn follows_clock(&self) -> bool {
        self.taken_up.known.is_some()
    }

    /// Which of its session's timings the [times](Self::times) and the
    /// [timecard](Self::timecard) are read from, as
    /// [`taken_up`](Self::taken_up) is for outcomes.
    pub(crate) fn timed(&self) -> Revision {
        self.timed
    }

    pub(crate) fn times(&self) -> Result<&NodeTimes, Untimed> {
        self.times.as_ref().map_err(|untimed| *untimed)
    }

    pub(crate) fn timecard(&self) -> &Timecard {
        &self.timecard
    }

    /// Which of its session's attempts the [states](Self::states) are read
    /// from, as [`taken_up`](Self::taken_up) is for outcomes.
    pub(crate) fn attempted(&self) -> Revision {
        self.attempted
    }

    pub(crate) fn states(&self) -> Option<&NodeStates> {
        self.states.as_ref()
    }

    /// Takes up what the build and the profile of `session` report of the
    /// graph's nodes, and says whether that changed anything a view of the
    /// graph shows. Each is read again only once it is reported anew.
    fn report(&mut self, session: &Session) -> bool {
        let outcomes = self.take_up_outcomes(session);
        let timings = self.take_up_timings(session);
        let attempts = self.take_up_attempts(session);
        outcomes || timings || attempts
    }

    /// Takes the playback clock's time to be `now`, `None` where it has none.
    fn follow(&mut self, now: Option<EpochMicros>) {
        self.at = now;
    }

    /// Takes up the outcomes of `session` known at `now`, and says whether
    /// that changed anything a view of the graph shows.
    fn take_up_at(&mut self, session: &Session, now: Option<EpochMicros>) -> bool {
        self.follow(now);
        self.take_up_outcomes(session)
    }

    fn take_up_attempts(&mut self, session: &Session) -> bool {
        let revision = session.attempts_revision();
        if self.attempted == revision {
            return false;
        }
        self.attempted = revision;
        let attempts = session.attempts();
        let Shared {
            base,
            names,
            facets,
            ..
        } = &self.shared;
        let described = DescribedActions::new(base, names, facets, attempts.described());
        let resolved = attempts.resolve(|action| described.output(action));
        let attempts = attempts.resolved(&resolved);
        let landings = Landings::new(base, names, attempts.held());
        let states = NodeStates::resolve(&landings, base, &attempts);
        let changed = self.states != states;
        self.states = states;
        changed
    }

    fn take_up_timings(&mut self, session: &Session) -> bool {
        let revision = session.timings_revision();
        if self.timed == revision {
            return false;
        }
        self.timed = revision;
        let timings = session.timings();
        let base = &self.shared.base;
        let landings = landings(base, &self.shared.names, timings);
        let times = NodeTimes::resolve(&landings, base, timings);
        let timecard = Timecard::of(timings, &landings);
        let changed = self.times != times || self.timecard != timecard;
        self.times = times;
        self.timecard = timecard;
        changed
    }

    /// Takes up the outcomes of `session`, as known at the clock's time
    /// where the build dated them. Moving on to later outcomes folds in only
    /// those reported since; moving back to earlier ones folds them afresh.
    fn take_up_outcomes(&mut self, session: &Session) -> bool {
        let dated = session.dated_outcomes();
        let revision = session.outcomes_revision();
        let shown = OutcomesShown {
            revision,
            known: dated.zip(self.at).map(|(dated, now)| dated.known_at(now)),
        };
        if self.taken_up == shown {
            return false;
        }
        self.taken_up = shown;
        let outcomes = match dated.zip(shown.known) {
            Some((dated, known)) if known != dated.all_known() => {
                let (of, folded) = &mut self.folded;
                if *of != revision {
                    (*of, *folded) = (revision, Folded::default());
                }
                dated.fold_to(folded, known);
                folded.outcomes()
            }
            _ => session.outcomes(),
        };
        let (statuses, reported) = match outcomes.is_empty() {
            true => Default::default(),
            false => {
                let nodes = Nodes::new(&self.holding, &self.shared.names, outcomes);
                (Statuses::resolve(&nodes), Report::of(&nodes))
            }
        };
        let changed = self.statuses != statuses || self.reported != reported;
        self.statuses = statuses;
        self.reported = reported;
        changed
    }

    /// The points of `scope` and the dependencies between them.
    fn scope_graph(&mut self, scope: Scope) -> Prepared<ScopeGraph> {
        let prepare = move |shared: &Shared| {
            let (base, names, facets) = (&shared.base, &shared.names, &shared.facets);
            ScopeGraph::new(base, names, facets, &shared.provenance, scope)
        };
        (self.scope_graphs.entry(scope))
            .or_insert_with(|| Prepared::new(&self.shared, prepare))
            .clone()
    }
}

/// Which of a session's outcomes a graph shows: those of one report of them,
/// and of a dated report those known at the playback clock's time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct OutcomesShown {
    revision: Revision,
    /// `None` where the report is not dated or the clock has no time.
    known: Option<Known>,
}

#[cfg(test)]
impl OutcomesShown {
    /// The outcomes of the report at `revision`, shown whole.
    pub(crate) fn undated(revision: Revision) -> Self {
        Self {
            revision,
            known: None,
        }
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

/// How many dominator trees no view wants are kept for a view coming back to
/// them, beside those wanted.
const RECENT_TREES: usize = 4;

/// What a reader takes from a loaded graph, prepared the first time it is
/// read and shared by every reader after.
pub(crate) struct Prepared<T>(Arc<Preparing<T>>);

struct Preparing<T> {
    shared: Shared,
    prepared: LazyLock<T, Box<dyn FnOnce() -> T + Send>>,
}

impl<T> Prepared<T> {
    /// What `prepare` takes from `shared`.
    fn new(shared: &Shared, prepare: impl FnOnce(&Shared) -> T + Send + 'static) -> Self {
        let from = shared.clone();
        Self(Arc::new(Preparing {
            shared: shared.clone(),
            prepared: LazyLock::new(Box::new(move || prepare(&from))),
        }))
    }

    /// What was prepared, prepared here where no reader has prepared it yet,
    /// and waited for where another is preparing it; so read only off the UI
    /// thread.
    pub(crate) fn get(&self) -> &T {
        &self.0.prepared
    }

    /// The graph it was taken from.
    pub(crate) fn shared(&self) -> &Shared {
        &self.0.shared
    }
}

impl<T> Clone for Prepared<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

/// What a graph keeps each of its dominator trees by: all that a tree is a
/// function of.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct TreeKey {
    pub(crate) scope: Scope,
    pub(crate) roots: Roots,
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
    let config_names = Arc::new(ConfigNames::of(&facets));
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
                config_names,
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

/// The loaded graph's pass yielding a `T` for `key` has landed.
pub(crate) struct Landed<K, T> {
    pub(crate) key: K,
    _pass: PhantomData<fn() -> T>,
}

pub(crate) type MetricsLanded = Landed<Scope, Measurement>;
pub(crate) type DominatorsLanded = Landed<TreeKey, Dominators>;

/// The graph's other views may have another [`GraphModel::filter`] to draw
/// through.
pub(crate) struct FilterChanged;

/// A pass over the whole of a graph, which is only taken once a view drawing
/// what it yields asks for it.
enum Pass<T> {
    Running {
        /// Dropping it gives the pass up: one not yet started never runs, and
        /// one running lands nothing.
        _work: Task<()>,
    },
    Done {
        yielded: Arc<T>,
        /// When it was last wanted or landed, on its [`Passes`]' clock.
        used: u64,
    },
}

/// A graph's passes of one kind, by what each passes over. Goes with the
/// graph they pass over, so a pass still running when the graph is replaced
/// never lands.
struct Passes<K, T> {
    by_key: HashMap<K, Pass<T>>,
    /// How many passes done that are no longer wanted are kept, those used
    /// last.
    kept: usize,
    /// Counts each time the passes are wanted or one lands.
    clock: u64,
}

impl<K: Clone + Eq + Hash, T> Passes<K, T> {
    fn keeping_all() -> Self {
        Self::keeping_recent(usize::MAX)
    }

    fn keeping_recent(kept: usize) -> Self {
        Self {
            by_key: HashMap::new(),
            kept,
            clock: 0,
        }
    }

    fn done(&self, key: &K) -> Option<&Arc<T>> {
        match self.by_key.get(key)? {
            Pass::Done { yielded, .. } => Some(yielded),
            Pass::Running { .. } => None,
        }
    }

    fn land(&mut self, key: K, yielded: Arc<T>) {
        self.clock += 1;
        let used = self.clock;
        self.by_key.insert(key, Pass::Done { yielded, used });
    }

    /// Drops each pass running for a key `wanted` leaves out, and of those
    /// done for such a key, all but the [`Self::kept`] used last.
    fn forget_unwanted(&mut self, wanted: &[K]) {
        self.clock += 1;
        for key in wanted {
            if let Some(Pass::Done { used, .. }) = self.by_key.get_mut(key) {
                *used = self.clock;
            }
        }
        let mut idle: Vec<(u64, K)> = Vec::with_capacity(self.by_key.len());
        self.by_key.retain(|key, pass| match pass {
            _ if wanted.contains(key) => true,
            Pass::Running { .. } => false,
            Pass::Done { used, .. } => {
                idle.push((*used, key.clone()));
                true
            }
        });
        idle.sort_unstable_by_key(|&(used, _)| std::cmp::Reverse(used));
        for (_, key) in idle.into_iter().skip(self.kept) {
            self.by_key.remove(&key);
        }
    }
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
    /// The playback clock of the document the graph is opened in, where that
    /// document plays back.
    clock: Option<DocumentClock>,
}

/// The invocation a graph is opened as part of.
struct Joined {
    /// Held for as long as the graph is: a session nothing holds is released,
    /// and tells its observers nothing further.
    session: Entity<Session>,
    /// Takes up what the build reports whenever the session changes.
    _changed: Subscription,
    /// Takes up the outcomes known at the time a gesture moves the clock to.
    _clock_moved: Option<Subscription>,
}

impl EventEmitter<SourceReplaced> for GraphModel {}
impl EventEmitter<MapPlaced> for GraphModel {}
impl EventEmitter<RevealRequested> for GraphModel {}
impl<K: 'static, T: 'static> EventEmitter<Landed<K, T>> for GraphModel {}
impl EventEmitter<FilterChanged> for GraphModel {}

impl GraphModel {
    /// The graph `load` is reading, viewable once the read finishes and failed
    /// where it does not.
    pub(crate) fn reading(
        load: Pending<Read>,
        session: Option<Entity<Session>>,
        clock: Option<DocumentClock>,
        cx: &mut Context<Self>,
    ) -> Self {
        // The build's outcomes can land on either side of the graph, so they
        // are taken up as the graph lands and again whenever the session
        // changes.
        let joined = session.map(|session| Joined {
            _changed: cx.observe(&session, |model, _, cx| model.take_up(cx)),
            _clock_moved: (clock.as_ref())
                .map(|clock| cx.observe(clock.clock(), |model, _, cx| model.take_up_at_clock(cx))),
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
            clock,
            ..Self::new(work)
        }
    }

    /// Takes up what the session's build reports of the loaded graph, its
    /// outcomes as known at the playback clock's time.
    fn take_up(&mut self, cx: &mut Context<Self>) {
        let now = self.clock().and_then(|clock| clock.read(cx).now());
        if let (Some(joined), Source::Loaded(loaded)) = (&self.joined, &mut self.source) {
            loaded.follow(now);
            if loaded.report(joined.session.read(cx)) {
                cx.notify();
            }
        }
    }

    /// Takes up the outcomes the session's build had reported by the
    /// playback clock's time, which moves on without telling anyone while
    /// the clock plays.
    pub(crate) fn take_up_at_clock(&mut self, cx: &mut Context<Self>) {
        let now = self.clock().and_then(|clock| clock.read(cx).now());
        if let (Some(joined), Source::Loaded(loaded)) = (&self.joined, &mut self.source)
            && loaded.take_up_at(joined.session.read(cx), now)
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
            clock: None,
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
    /// `wanted`, as [`Self::want`] does.
    pub(crate) fn want_metrics(&mut self, wanted: &[Scope], cx: &mut Context<Self>) {
        let pass = |loaded: &mut Loaded, &scope: &Scope| {
            let shared = loaded.shared.clone();
            move || {
                let (base, names, facets) = (&shared.base, &shared.names, &shared.facets);
                measure(base, names, facets, &shared.provenance, scope)
            }
        };
        self.want(wanted, |loaded| &mut loaded.measures, pass, cx);
    }

    /// The loaded graph's metrics in `scope`, once measured.
    pub(crate) fn metrics(&self, scope: Scope) -> Option<&Arc<Measurement>> {
        self.loaded()?.measures.done(&scope)
    }

    /// Has the loaded graph's dominator tree computed off the UI thread for
    /// each key of `wanted`, as [`Self::want`] does.
    pub(crate) fn want_dominators(&mut self, wanted: &[TreeKey], cx: &mut Context<Self>) {
        let pass = |loaded: &mut Loaded, key: &TreeKey| {
            let (prepared, roots) = (loaded.scope_graph(key.scope), key.roots.clone());
            move || {
                let shared = prepared.shared();
                (prepared.get()).dominators(&shared.names, &shared.facets, &roots)
            }
        };
        self.want(wanted, |loaded| &mut loaded.dominators, pass, cx);
    }

    /// The loaded graph's dominator tree for `key`, once computed.
    pub(crate) fn dominators(&self, key: &TreeKey) -> Option<&Arc<Dominators>> {
        self.loaded()?.dominators.done(key)
    }

    /// The points of `scope` of the loaded graph and the dependencies between
    /// them, which the graph's dominator trees in `scope` share; absent while
    /// the graph is still being read.
    pub(crate) fn scope_graph(&mut self, scope: Scope) -> Option<Prepared<ScopeGraph>> {
        match &mut self.source {
            Source::Loaded(loaded) => Some(loaded.scope_graph(scope)),
            _ => None,
        }
    }

    /// The targets of the loaded graph, indexed for resolving a target set;
    /// absent while the graph is still being read.
    pub(crate) fn targets(&self) -> Option<Prepared<TargetIndex>> {
        Some(self.loaded()?.targets.clone())
    }

    /// Runs the work `pass` makes of the loaded graph off the UI thread for
    /// each key of `wanted` not yet asked for, keeping what it yields among
    /// the passes `passes` picks out and emitting [`Landed`] as each lands,
    /// and gives up any other key's pass still running, which would only hold
    /// the workers from those wanted; of what other keys' passes yielded, the
    /// passes keep as many as they keep. Does nothing while the graph is still
    /// being read.
    fn want<K, T, W>(
        &mut self,
        wanted: &[K],
        passes: fn(&mut Loaded) -> &mut Passes<K, T>,
        pass: impl Fn(&mut Loaded, &K) -> W,
        cx: &mut Context<Self>,
    ) where
        K: Clone + Eq + Hash + 'static,
        T: Send + Sync + 'static,
        W: FnOnce() -> T + Send + 'static,
    {
        let Source::Loaded(loaded) = &mut self.source else {
            return;
        };
        passes(loaded).forget_unwanted(wanted);
        for key in wanted {
            if passes(loaded).by_key.contains_key(key) {
                continue;
            }
            let landing = key.clone();
            let work = load_in_background(cx, pass(loaded, key), move |model, done, cx| {
                if let Source::Loaded(loaded) = &mut model.source {
                    passes(loaded).land(landing.clone(), Arc::new(done));
                    cx.emit(Landed::<K, T> {
                        key: landing,
                        _pass: PhantomData,
                    });
                }
            });
            (passes(loaded).by_key).insert(key.clone(), Pass::Running { _work: work });
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

    pub(crate) fn clock(&self) -> Option<&Entity<Clock>> {
        self.clock.as_ref().map(DocumentClock::clock)
    }

    pub(crate) fn source(&self) -> &Source {
        &self.source
    }

    /// The configurations known of the invocation the graph is opened as part
    /// of; `None` for a graph opened as part of none.
    pub(crate) fn configurations<'a>(&self, cx: &'a App) -> Option<&'a Configurations> {
        let joined = self.joined.as_ref()?;
        Some(joined.session.read(cx).configurations())
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
    use bviz_graph_layout::facets::Tri;
    use bviz_graph_layout::graph::Node;
    use bviz_graph_layout::metrics::{Sizing, Unit};
    use bviz_graph_layout::{Input, Kind};
    use bviz_invocation::{
        AttemptSource, Attempts, BuildOutcome, ConfiguredTarget, Outcomes, Profiled, Runner,
        Spawns, Timings,
    };
    use std::time::Duration;

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

    /// The graph of the one target `//app:bin` and the action writing it, as
    /// a read lands it.
    fn loaded() -> Loaded {
        let mut names = Names::default();
        let mut base = Graph::new();
        let bin = names.intern("target://app:bin");
        base.add_node(Node::new(bin, "bin", Kind::Target, None));
        let link = names.intern("action:bazel-out/bin/app/bin");
        base.add_node(Node::new(link, "link", Kind::Action, Some(bin)));
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
                facets: Arc::clone(&facets),
                labels,
                containers,
                capabilities: bviz_graph_layout::capability::CapabilitiesBuilder::default().seal(),
                provenance: Arc::default(),
                inputs: Input::Query.into(),
                config_names: Arc::new(ConfigNames::of(&facets)),
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

    const SANDBOXED_WALL: Duration = Duration::from_millis(7);

    fn timing(primary_output: &str) -> Timings {
        let mut timings = Timings::default();
        timings.record(Profiled {
            primary_output: Some(primary_output),
            owner: None,
            wall: SANDBOXED_WALL,
            spawns: Spawns::on(Runner::Sandboxed, SANDBOXED_WALL),
        });
        timings
    }

    /// A report of timings is taken up apart from one of outcomes, so that
    /// either landing leaves what the other placed as it was.
    #[test]
    fn a_graph_takes_up_timings_and_outcomes_each_once_per_report_of_them() {
        let mut loaded = loaded();
        let mut session = Session::default();
        assert_eq!(loaded.times, Err(Untimed::NoProfile));

        session.report_timings(timing("bazel-out/bin/app/bin"));
        assert!(loaded.report(&session));
        assert!(loaded.times.is_ok());
        assert!(!loaded.report(&session));

        session.report_outcomes(reporting(BuildOutcome::NotBuilt));
        let timed = loaded.timed;
        assert!(loaded.report(&session));
        assert_eq!(loaded.timed, timed);
        assert!(loaded.times.is_ok());

        let taken_up = loaded.taken_up;
        session.report_timings(timing("bazel-out/bin/app/other"));
        assert!(loaded.report(&session));
        assert_eq!(loaded.taken_up, taken_up);
        assert_eq!(loaded.times, Err(Untimed::Unplaced));
        assert_ne!(loaded.statuses, Statuses::default());
    }

    /// Attempts are taken up apart from timings, and once per report of them.
    #[test]
    fn a_graph_takes_up_the_attempts_its_session_holds_once_per_report_of_them() {
        let mut loaded = loaded();
        let mut session = Session::default();
        assert!(!loaded.report(&session));
        assert!(loaded.states().is_none());

        let mut attempts = Attempts::default();
        let attempt = crate::playback::tests::attempt(0, 10, &[]);
        attempts.record(Some("bazel-out/bin/app/bin"), None, attempt);
        session.report_attempts(AttemptSource::Profile, attempts);
        assert!(loaded.report(&session));
        assert!(loaded.states().is_some());
        assert!(!loaded.report(&session));

        session.report_timings(timing("bazel-out/bin/app/bin"));
        assert!(loaded.report(&session));
        assert!(loaded.states().is_some());

        session.report_attempts(AttemptSource::Profile, Attempts::default());
        assert!(loaded.report(&session));
        assert!(loaded.states().is_none());
    }

    /// A build that dated its outcomes is shown as far as the time a graph
    /// is given has come, and whole where it is given none.
    #[test]
    fn a_graph_shows_the_dated_outcomes_known_at_the_time_it_is_given() {
        use bviz_invocation::{DatedOutcomes, Report};
        let at = |millis: i64| EpochMicros::from_millis(1_781_439_245_000 + millis);
        let built = ConfiguredTarget::new("//app:bin".to_owned(), None);
        let mut session = Session::default();
        session.report_dated_outcomes(DatedOutcomes::new([(
            at(10),
            Report::Build(built, BuildOutcome::Built),
        )]));
        let mut loaded = loaded();
        assert!(!loaded.take_up_at(&session, Some(at(9))));
        assert_eq!(loaded.statuses, Statuses::default());
        assert!(loaded.follows_clock());

        assert!(loaded.take_up_at(&session, Some(at(10))));
        assert_ne!(loaded.statuses, Statuses::default());
        assert!(!loaded.take_up_at(&session, Some(at(20))));

        assert!(!loaded.take_up_at(&session, None), "the whole build again");
        assert!(!loaded.follows_clock());
        assert_ne!(loaded.statuses, Statuses::default());
        assert!(loaded.take_up_at(&session, Some(at(0))));
        assert_eq!(loaded.statuses, Statuses::default());
        assert!(loaded.take_up_at(&session, Some(at(10))));
        assert_ne!(loaded.statuses, Statuses::default());
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

    fn running() -> Pass<Measurement> {
        Pass::Running {
            _work: Task::ready(()),
        }
    }

    fn measured(scope: Scope) -> Arc<Measurement> {
        Arc::new(Measurement {
            scope,
            points: Vec::new(),
            sizing: Sizing::FanIn,
        })
    }

    /// A scope no longer wanted stops being passed over, but what a pass
    /// yielded in it stays for when it is wanted again.
    #[test]
    fn an_unwanted_pass_is_dropped_and_a_landed_one_kept() {
        let scope = |input: Input| Scope::new(Unit::Target, input.into()).unwrap();
        let (landed, dropped, kept) = (
            scope(Input::Query),
            scope(Input::Cquery),
            scope(Input::Aquery),
        );
        let mut passes = Passes::keeping_all();
        passes.land(landed, measured(landed));
        passes.by_key.insert(dropped, running());
        passes.by_key.insert(kept, running());

        passes.forget_unwanted(&[kept]);

        assert!(passes.done(&landed).is_some());
        assert!(!passes.by_key.contains_key(&dropped));
        assert!(matches!(
            passes.by_key.get(&kept),
            Some(Pass::Running { .. })
        ));
    }

    /// Of the passes done that are no longer wanted, those wanted or landed
    /// last are kept, as many as the passes keep, and the rest dropped.
    #[test]
    fn of_the_passes_no_longer_wanted_only_the_last_used_are_kept() {
        let mut passes: Passes<u32, Measurement> = Passes::keeping_recent(2);
        let scope = Scope::new(Unit::Target, Input::Query.into()).unwrap();
        for key in 0..4 {
            passes.land(key, measured(scope));
        }
        passes.forget_unwanted(&[0]);
        passes.forget_unwanted(&[3]);

        let mut kept: Vec<u32> = passes.by_key.keys().copied().collect();
        kept.sort_unstable();
        assert_eq!(kept, [0, 2, 3]);
    }
}
