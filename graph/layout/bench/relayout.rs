//! Times the layout one expand/collapse toggle costs.
//!
//! A toggle re-derives the whole visible view — folding every unexpanded
//! container away, merging interchangeable leaves, transitively reducing what
//! is left — and lays it out again, so the wait a toggle imposes is that
//! layout. The same view is laid out twice: freely, deriving a configuration of
//! layers and slots from the graph alone, and held to the configuration of the
//! drawing the toggle replaces, which is what keeps the boxes both drawings
//! share reading where they did. Reporting both prices the holding.
//!
//! The container opened is, of those the starting view draws collapsed, the one
//! whose expansion unfolds the most nodes, so the layout timed is about the
//! heaviest a single toggle of that view can ask for.
//!
//! Usage: `bench_relayout [--query FILE] [--cquery FILE] [--aquery FILE]
//! [--max-nodes N] [--max-edges N]`, at least one input, each a Bazel
//! `--output=streamed_proto` dump of that query. The caps bound the starting
//! view the toggle happens inside, so raising them times the toggle in a larger
//! view, up to the whole graph; the iteration count comes from
//! `BVIZ_BENCH_ITERS`.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use bviz_graph_layout::graph::Graph;
use bviz_graph_layout::{layout, lod, quotient, reduce, streamed_proto, Id, Names};

/// The node cap `--max-nodes` defaults to: the cap the application's first
/// layered view opens with.
const DEFAULT_MAX_NODES: usize = 400;

/// The edge cap `--max-edges` defaults to: the cap the application's first
/// layered view opens with.
const DEFAULT_MAX_EDGES: usize = 600;

const USAGE: &str = "usage: bench_relayout [--query FILE] [--cquery FILE] [--aquery FILE] \
                     [--max-nodes N] [--max-edges N] (at least one input); \
                     iterations from BVIZ_BENCH_ITERS";

fn min_secs(iters: usize, mut f: impl FnMut()) -> f64 {
    (0..iters)
        .map(|_| {
            let t = std::time::Instant::now();
            f();
            t.elapsed().as_secs_f64()
        })
        .fold(f64::INFINITY, f64::min)
}

struct Args {
    query: Option<PathBuf>,
    cquery: Option<PathBuf>,
    aquery: Option<PathBuf>,
    max_nodes: usize,
    max_edges: usize,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            query: None,
            cquery: None,
            aquery: None,
            max_nodes: DEFAULT_MAX_NODES,
            max_edges: DEFAULT_MAX_EDGES,
        }
    }
}

impl Args {
    fn from_env() -> Self {
        let mut args = Self::default();
        let mut argv = std::env::args().skip(1);
        while let Some(flag) = argv.next() {
            let Some(value) = argv.next() else { usage() };
            match flag.as_str() {
                "--query" => args.query = Some(PathBuf::from(&value)),
                "--cquery" => args.cquery = Some(PathBuf::from(&value)),
                "--aquery" => args.aquery = Some(PathBuf::from(&value)),
                "--max-nodes" => args.max_nodes = count(&value),
                "--max-edges" => args.max_edges = count(&value),
                _ => usage(),
            }
        }
        if args.query.is_none() && args.cquery.is_none() && args.aquery.is_none() {
            usage();
        }
        args
    }

    /// Reads every given dump and merges them into one joint graph, as the
    /// application does before any level-of-detail folding.
    fn build_joint(&self) -> (Graph, Names) {
        let aquery = self
            .aquery
            .as_deref()
            .map(|p| decode(p, streamed_proto::read_action_graph_containers));
        let cquery = self.cquery.as_deref().map(|p| decode(p, streamed_proto::read_cquery_results));
        let query = self.query.as_deref().map(|p| decode(p, streamed_proto::read_query_targets));
        bviz_graph_layout::build_joint(
            aquery.as_deref(),
            cquery.as_deref(),
            query.as_deref(),
            false,
        )
    }
}

fn usage() -> ! {
    eprintln!("{USAGE}");
    std::process::exit(2)
}

fn count(value: &str) -> usize {
    value.parse().unwrap_or_else(|_| usage())
}

fn decode<T, E: std::fmt::Display>(
    path: &Path,
    parse: impl Fn(&[u8]) -> Result<Vec<T>, E>,
) -> Vec<T> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    parse(&bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The graph one expansion of `base` draws: only the descendants of expanded
/// containers, interchangeable leaves merged, and of the edges that projection
/// leaves only those the transitive reduction keeps. `reduced_base` is `base`
/// already reduced, so a collapsed box draws the reduction of its hidden
/// subtree's edges.
fn view_at(base: &Graph, reduced_base: &Graph, expanded: &HashSet<Id>, names: &mut Names) -> Graph {
    let visible = lod::collapse(base, expanded);
    let (mut view, remap) = quotient::quotient(&visible, expanded, names);
    let skeleton = lod::collapse(reduced_base, expanded);
    let box_of = |id: Id| remap.get(&id).copied().unwrap_or(id);
    let kept: HashSet<(Id, Id)> = skeleton
        .edges
        .iter()
        .map(|e| (box_of(e.from), box_of(e.to)))
        .filter(|(a, b)| a != b)
        .collect();
    view.edges.retain(|e| kept.contains(&(e.from, e.to)));
    reduce::transitive_reduction(&view)
}

fn children_by_parent(graph: &Graph) -> HashMap<Id, Vec<Id>> {
    let mut children: HashMap<Id, Vec<Id>> = HashMap::new();
    for node in &graph.nodes {
        if let Some(parent) = node.parent {
            children.entry(parent).or_default().push(node.id);
        }
    }
    children
}

/// How many nodes the view gains when `node` becomes visible: itself plus the
/// chain of single-child descendants below it, each of which auto-expands.
fn opened_chain(children: &HashMap<Id, Vec<Id>>, memo: &mut HashMap<Id, usize>, node: Id) -> usize {
    let mut chain = Vec::new();
    let mut n = node;
    let mut below = loop {
        if let Some(&known) = memo.get(&n) {
            break known;
        }
        match children.get(&n).map(Vec::as_slice) {
            Some([only]) => {
                chain.push(n);
                n = *only;
            }
            _ => {
                memo.insert(n, 1);
                break 1;
            }
        }
    };
    for &n in chain.iter().rev() {
        below += 1;
        memo.insert(n, below);
    }
    below
}

/// Of the containers the view at `expanded` draws collapsed, the one whose
/// expansion unfolds the most nodes, and how many that is — counted before
/// interchangeable leaves merge, so the boxes the view gains are fewer. Ties go
/// to the lower id, so a graph always picks the same container.
fn densest_collapsed_container(base: &Graph, expanded: &HashSet<Id>) -> Option<(Id, usize)> {
    let drawn: HashSet<Id> = lod::collapse(base, expanded).nodes.iter().map(|n| n.id).collect();
    let children = children_by_parent(base);
    let mut chains: HashMap<Id, usize> = HashMap::new();
    lod::expandable_containers(base)
        .into_iter()
        .filter(|id| drawn.contains(id) && !expanded.contains(id))
        .map(|id| {
            let revealed =
                children[&id].iter().map(|&c| opened_chain(&children, &mut chains, c)).sum();
            (id, revealed)
        })
        .max_by_key(|&(id, revealed)| (revealed, Reverse(id)))
}

fn main() {
    let args = Args::from_env();
    let iters: usize =
        std::env::var("BVIZ_BENCH_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(5);

    let (base, mut names) = args.build_joint();
    let reduced_base = reduce::transitive_reduction(&base);

    let opening = lod::default_expansion(&base, args.max_nodes, args.max_edges);
    let start_view = view_at(&base, &reduced_base, &opening, &mut names);
    let start = layout::layout(&start_view);

    let (container, revealed) = densest_collapsed_container(&base, &opening)
        .expect("the starting view draws a container it can open");
    let mut opened = opening.clone();
    opened.insert(container);
    let toggled = view_at(&base, &reduced_base, &opened, &mut names);

    let free = min_secs(iters, || {
        std::hint::black_box(layout::layout(&toggled));
    });
    let pinned = min_secs(iters, || {
        std::hint::black_box(layout::layout_pinned(&toggled, &start.anchors));
    });

    eprintln!(
        "[bench] base: {} nodes / {} edges; under caps {}/{} the starting view is \
         {} boxes / {} edges, {} of them anchored by its drawing",
        base.nodes.len(),
        base.edges.len(),
        args.max_nodes,
        args.max_edges,
        start_view.nodes.len(),
        start_view.edges.len(),
        start.anchors.len(),
    );
    eprintln!(
        "[bench] opening {} unfolds {revealed} nodes: the view laid out grows to \
         {} boxes / {} edges",
        names.resolve(container),
        toggled.nodes.len(),
        toggled.edges.len(),
    );
    eprintln!("[bench] layout free: {free:.3}s  pinned: {pinned:.3}s  (min of {iters})");
}
