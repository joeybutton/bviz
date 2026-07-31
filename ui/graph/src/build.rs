use bviz_graph_layout::graph::Graph;
use bviz_graph_layout::{layout, quotient, streamed_proto, Id, LaidOut, Names};
use std::collections::HashSet;
use std::path::PathBuf;

/// Which Bazel query produced a `--output=streamed_proto` file, selecting the
/// reader and builder used to turn it into a graph.
#[derive(Clone, Copy)]
pub(crate) enum InputKind {
    Aquery,
    Cquery,
    Query,
}

/// A graph input: a streamed_proto file and the query kind that produced it.
#[derive(Clone)]
pub(crate) struct Input {
    pub(crate) kind: InputKind,
    pub(crate) path: PathBuf,
}

/// Reads every input's streamed_proto file and merges them into one joint graph,
/// before any group quotienting. Cached so toggling a group needn't re-read or
/// re-parse.
pub(crate) fn build_graph(inputs: &[Input]) -> Result<(Graph, Graph, Names), String> {
    let mut aq = None;
    let mut cq = None;
    let mut q = None;
    for input in inputs {
        let path = input.path.display();
        let bytes = std::fs::read(&input.path).map_err(|e| format!("{path}: {e}"))?;
        match input.kind {
            InputKind::Aquery => {
                aq = Some(
                    streamed_proto::read_action_graph_containers(&bytes)
                        .map_err(|e| format!("{path}: {e}"))?,
                );
            }
            InputKind::Cquery => {
                cq = Some(
                    streamed_proto::read_cquery_results(&bytes)
                        .map_err(|e| format!("{path}: {e}"))?,
                );
            }
            InputKind::Query => {
                q = Some(
                    streamed_proto::read_query_targets(&bytes)
                        .map_err(|e| format!("{path}: {e}"))?,
                );
            }
        }
    }
    let (base, names) =
        bviz_graph_layout::build_joint(aq.as_deref(), cq.as_deref(), q.as_deref(), false);
    // Transitively reduce the whole base once: a collapsed box then draws the
    // reduction of its hidden subtree's edges, so its edge count is stable under
    // expansion rather than re-derived (differently) from each view.
    let reduced = bviz_graph_layout::reduce::transitive_reduction(&base);
    Ok((base, reduced, names))
}

/// Builds the display graph with the given ids expanded and lays it out: fold
/// every unexpanded container away, merge interchangeable leaves, then lay out
/// only the resulting visible view. When `reduced`, the view is transitively
/// reduced in two steps. First the base's reduction projects onto the view's
/// boxes — the full base and the already-reduced base collapse to the same
/// boxes, so a collapsed box stands in for its hidden subtree's reduced edges,
/// keeping its edge count stable under expansion. Then the projected box graph
/// is reduced again, because merging a subtree into one box creates box-level
/// shortcuts that no longer hold at node granularity (a box A->B edge implied by
/// a drawn A->M->B path through other visible boxes); the second pass drops them.
/// `full_edges` keeps the unreduced connectivity for hover.
pub(crate) fn relayout(
    base: &Graph,
    reduced_base: &Graph,
    expanded: &HashSet<Id>,
    names: &mut Names,
    reduced: bool,
) -> LaidOut {
    let visible = bviz_graph_layout::lod::collapse(base, expanded);
    let (mut view, remap) = quotient::quotient(&visible, expanded, names);
    let full_edges: Vec<(Id, Id)> =
        view.edges.iter().map(|e| (e.from, e.to)).collect::<HashSet<_>>().into_iter().collect();
    if reduced {
        // Project the reduced base's edges onto the same boxes (a collapsed group's
        // members map through `remap`, everything else keeps its id), and keep only
        // those of the view's edges.
        let skeleton = bviz_graph_layout::lod::collapse(reduced_base, expanded);
        let box_of = |id: Id| remap.get(&id).copied().unwrap_or(id);
        let kept: HashSet<(Id, Id)> = skeleton
            .edges
            .iter()
            .map(|e| (box_of(e.from), box_of(e.to)))
            .filter(|(a, b)| a != b)
            .collect();
        view.edges.retain(|e| kept.contains(&(e.from, e.to)));
        view = bviz_graph_layout::reduce::transitive_reduction(&view);
    }
    let mut laid = layout::layout(&view);
    laid.full_edges = full_edges;
    laid
}
