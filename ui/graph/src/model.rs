use crate::GraphView;
use crate::build::{self, Input, build_graph};
use crate::emphasis;
use bviz_graph_layout::graph::Graph;
use bviz_graph_layout::{Id, LaidOut, Names};
use bviz_ui_components::load_in_background;
use gpui::{Context, Task};
use std::collections::HashSet;
use std::sync::Arc;

/// Caps the nodes the startup view opens with.
const MAX_VISIBLE_NODES: usize = 400;

/// Caps the edges the startup view opens with.
const MAX_VISIBLE_EDGES: usize = 600;

/// A graph that finished loading, in the form every later relayout starts from.
pub(crate) struct Loaded {
    laid_out: LaidOut,
    /// The built (pre-quotient) graph, cached so toggling a group re-lays-out
    /// without re-reading or re-parsing the proto.
    base: Arc<Graph>,
    /// The base with its transitively-redundant edges removed, cached so the
    /// reduced view's edges are a stable projection of one fixed skeleton.
    reduced_base: Arc<Graph>,
    /// The interned node-id table the base graph keys on, threaded into each
    /// relayout so the quotient pass can mint group ids.
    names: Names,
}

impl Loaded {
    pub(crate) fn laid_out(&self) -> &LaidOut {
        &self.laid_out
    }
}

/// The loaded graph, a load in progress, or a message explaining why none is
/// shown.
pub(crate) enum Source {
    Loading,
    Loaded(Loaded),
    Message(String),
}

/// The graph being viewed: the layout on screen and the expansion the next
/// relayout starts from, every layout produced off the UI thread.
pub(crate) struct Model {
    source: Source,
    /// Container and group ids currently expanded; every other container folds
    /// its subtree away so the layout only processes the visible view.
    expanded: HashSet<Id>,
    /// Multi-child container ids of the base graph; only these carry an
    /// expand/collapse handle (a leaf can't open, a singleton auto-expands), so a
    /// click or caret can tell an expandable box from a plain one.
    expandable: HashSet<Id>,
    /// Whether a relayout is running, so the view shows a progress indicator
    /// while the prior layout stays interactive.
    relaying: bool,
    /// Holds the background load/relayout task so it isn't cancelled on drop.
    _task: Task<()>,
}

impl Model {
    /// Reads `inputs` and lays the graph out on the background executor, so
    /// window appearance and interactivity are never blocked; `reduced` selects
    /// whether that first layout draws the transitive reduction.
    pub(crate) fn loading(inputs: Vec<Input>, reduced: bool, cx: &mut Context<GraphView>) -> Self {
        let task = load_in_background(
            cx,
            move || {
                let (graph, reduced_base, mut names) = build_graph(&inputs)?;
                let expanded = bviz_graph_layout::lod::default_expansion(
                    &graph,
                    MAX_VISIBLE_NODES,
                    MAX_VISIBLE_EDGES,
                );
                let laid_out =
                    build::relayout(&graph, &reduced_base, &expanded, &mut names, reduced);
                Ok::<_, String>((graph, reduced_base, names, expanded, laid_out))
            },
            |view, result, _| {
                view.model.source = match result {
                    Ok((graph, reduced_base, names, expanded, laid_out)) => {
                        view.model.expandable =
                            bviz_graph_layout::lod::expandable_containers(&graph);
                        view.model.expanded = expanded;
                        Source::Loaded(Loaded {
                            laid_out,
                            base: Arc::new(graph),
                            reduced_base: Arc::new(reduced_base),
                            names,
                        })
                    }
                    Err(err) => Source::Message(err),
                };
            },
        );
        Self {
            source: Source::Loading,
            expanded: HashSet::new(),
            expandable: HashSet::new(),
            relaying: false,
            _task: task,
        }
    }

    pub(crate) fn source(&self) -> &Source {
        &self.source
    }

    /// The layout being drawn, or `None` while loading or after a failure.
    pub(crate) fn laid_out(&self) -> Option<&LaidOut> {
        match &self.source {
            Source::Loaded(loaded) => Some(&loaded.laid_out),
            _ => None,
        }
    }

    pub(crate) fn expandable(&self) -> &HashSet<Id> {
        &self.expandable
    }

    pub(crate) fn relaying(&self) -> bool {
        self.relaying
    }

    pub(crate) fn status(&self) -> String {
        match &self.source {
            Source::Loaded(Loaded { laid_out: g, .. }) => format!(
                "{} boxes · {} connectors · {}×{}",
                g.boxes.len(),
                g.connectors.len(),
                g.width.round() as i64,
                g.height.round() as i64,
            ),
            Source::Loading => "Loading…".into(),
            Source::Message(_) => "No graph".into(),
        }
    }
}

/// Flips whether `id` is expanded and lays the graph out at the new expansion.
pub(crate) fn toggle_expansion(view: &mut GraphView, id: Id, cx: &mut Context<GraphView>) {
    if !view.model.expanded.remove(&id) {
        view.model.expanded.insert(id);
    }
    relayout(view, cx);
}

/// Lays the cached base graph out at the current expansion off the UI thread,
/// swaps in the result, and asks the framing to settle onto it.
pub(crate) fn relayout(view: &mut GraphView, cx: &mut Context<GraphView>) {
    let Source::Loaded(loaded) = &view.model.source else {
        return;
    };
    let base = Arc::clone(&loaded.base);
    let reduced_base = Arc::clone(&loaded.reduced_base);
    // The relayout mints group ids against the interned-name table, so it
    // extends a clone and hands it back for the view to adopt.
    let mut names = loaded.names.clone();
    let expanded = view.model.expanded.clone();
    let reduced = view.reduced;
    view.model.relaying = true;
    view.model._task = load_in_background(
        cx,
        move || {
            let laid_out = build::relayout(&base, &reduced_base, &expanded, &mut names, reduced);
            Loaded { laid_out, base, reduced_base, names }
        },
        |view, loaded, cx| {
            view.model.source = Source::Loaded(loaded);
            view.framing.request_recenter();
            view.model.relaying = false;
            emphasis::begin_fade(view, cx);
        },
    );
}
