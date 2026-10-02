//! Transitive reduction of a graph's edges: drops a connection whose endpoints a
//! longer path already links, leaving the minimal edge set with the same
//! reachability.
//!
//! Edges are reduced on their `(from, to)` pair, ignoring provenance, so a pair
//! is kept or dropped as a whole and a kept pair keeps all its edges.
//!
//! An acyclic graph reduces in one pass over an integer-indexed reachability
//! bitset, the only feasible way on the whole base graph. Folding a subtree into
//! one box leaves graphs that are not acyclic — two packages each depending on
//! the other through different members become a pair of boxes pointing both
//! ways — so a cyclic graph is reduced by condensing each cycle to a node,
//! reducing the acyclic graph that leaves, and keeping every edge inside a
//! cycle. Reachability survives, and no edge of a cycle is redundant anyway,
//! since each of its nodes already reaches every other.
//!
//! The same reachability rows count how many nodes each node reaches, where
//! they fit; past that the counts are estimated.

use std::cmp::Reverse;
use std::ops::Range;

use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::graph::{Edge, Graph};
use crate::intern::Id;

/// Returns `graph` with the same nodes but only the edges of a transitive
/// reduction.
pub fn transitive_reduction(graph: &Graph) -> Graph {
    reduced(graph.clone())
}

/// [`transitive_reduction`] of a graph the caller is done with, which spares the
/// copy of its nodes.
pub fn reduced(graph: Graph) -> Graph {
    match dag_reduction(graph) {
        Ok(reduced) => reduced,
        Err(cyclic) => condensed_reduction(cyclic),
    }
}

/// Transitive reduction of a DAG: the redundant pairs are those whose target is
/// reachable from another of the source's successors. A cycle leaves no
/// topological order, and hands `graph` back for the caller to fall back with.
fn dag_reduction(graph: Graph) -> Result<Graph, Graph> {
    let (index, n) = DenseIds::new(graph.nodes.iter().map(|node| node.id));
    let dense = |e: &Edge| Some((index.get(e.from)?, index.get(e.to)?));
    let pairs = || {
        graph
            .edges
            .iter()
            .filter(|e| e.from != e.to)
            .filter_map(dense)
    };
    let mut out_degree: Vec<usize> = vec![0; n as usize];
    for (u, _) in pairs() {
        out_degree[u as usize] += 1;
    }
    let mut succ: Vec<Vec<u32>> = out_degree.into_iter().map(Vec::with_capacity).collect();
    let mut indeg: Vec<u32> = vec![0; n as usize];
    for (u, v) in pairs() {
        succ[u as usize].push(v);
        indeg[v as usize] += 1;
    }

    let Some(order) = topo_order(&succ, indeg) else {
        return Err(graph);
    };
    let reduction = Reduction::new(&order, &succ);

    let mut out = graph;
    out.edges.retain(|e| {
        e.from == e.to
            || match dense(e) {
                Some((u, v)) => reduction.keeps(u, v),
                None => true,
            }
    });
    Ok(out)
}

/// Dense indices for distinct node ids, numbered from zero in the order they
/// are first named.
pub(crate) enum DenseIds {
    /// Indexed by id.
    Table(Vec<Option<u32>>),
    Map(FxHashMap<Id, u32>),
}

/// The most slots per node an id-indexed [`DenseIds::Table`] may take. A small
/// view of a large graph names ids far apart, and a table reaching its largest
/// would cost more to allocate than the view takes to process.
const TABLE_SLOTS_PER_NODE: usize = 32;

impl DenseIds {
    /// Numbers the distinct `ids`, returning the index and how many there are.
    pub(crate) fn new(ids: impl ExactSizeIterator<Item = Id> + Clone) -> (Self, u32) {
        let named = ids.len();
        let span = ids.clone().map(|id| id.0 as usize + 1).max().unwrap_or(0);
        let mut count = 0;
        let mut next = || {
            count += 1;
            count - 1
        };
        let index = if span <= TABLE_SLOTS_PER_NODE * named {
            let mut table = vec![None; span];
            for id in ids {
                table[id.0 as usize].get_or_insert_with(&mut next);
            }
            Self::Table(table)
        } else {
            let mut map = FxHashMap::with_capacity_and_hasher(named, Default::default());
            for id in ids {
                map.entry(id).or_insert_with(&mut next);
            }
            Self::Map(map)
        };
        (index, count)
    }

    pub(crate) fn get(&self, id: Id) -> Option<u32> {
        match self {
            Self::Table(table) => table.get(id.0 as usize).copied().flatten(),
            Self::Map(map) => map.get(&id).copied(),
        }
    }
}

/// Kahn's algorithm over the successor adjacency; `None` when a leftover node
/// betrays a cycle. Consumes `indeg`, decrementing it as nodes are emitted.
fn topo_order(succ: &[Vec<u32>], mut indeg: Vec<u32>) -> Option<Vec<u32>> {
    let n = succ.len();
    let mut order: Vec<u32> = Vec::with_capacity(n);
    let mut ready: Vec<u32> = (0..n as u32).filter(|&i| indeg[i as usize] == 0).collect();
    while let Some(u) = ready.pop() {
        order.push(u);
        for &w in &succ[u as usize] {
            indeg[w as usize] -= 1;
            if indeg[w as usize] == 0 {
                ready.push(w);
            }
        }
    }
    (order.len() == n).then_some(order)
}

/// Which edges of a DAG over dense indices its transitive reduction keeps: an
/// edge is dropped when another of its source's successors reaches its target.
struct Reduction {
    /// By node, the distinct successors whose edge the reduction keeps, in
    /// descending [`Reduction::bit`] order.
    kept: Vec<Vec<u32>>,
    /// By node, its [`Reach::bit`].
    bit: Vec<u32>,
}

impl Reduction {
    /// Reduces the DAG `succ` holds, `order` being a topological order of it.
    fn new(order: &[u32], succ: &[Vec<u32>]) -> Self {
        let reach = Reach::new(order, succ);
        let mut kept: Vec<Vec<u32>> = vec![Vec::new(); succ.len()];
        reach.fill(
            succ,
            |_, _, keep| keep.to_vec(),
            |u, keep| kept[u as usize] = keep,
        );
        Self {
            kept,
            bit: reach.bit,
        }
    }

    /// Whether the reduction keeps the edge `u -> v`, one of the DAG's.
    fn keeps(&self, u: u32, v: u32) -> bool {
        self.kept[u as usize]
            .binary_search_by_key(&Reverse(self.bit[v as usize]), |&w| {
                Reverse(self.bit[w as usize])
            })
            .is_ok()
    }
}

/// A DAG's nodes numbered for its reachability rows.
struct Reach {
    levels: Levels,
    /// By node, its height: the longest path from it to a sink.
    height: Vec<u32>,
    /// By node, its bit in the reachability rows. Bits ascend with height, so a
    /// node reaches only lower bits.
    bit: Vec<u32>,
    /// By bit, the node holding it.
    by_bit: Vec<u32>,
}

impl Reach {
    /// Numbers the DAG `succ` holds, `order` being a topological order of it.
    fn new(order: &[u32], succ: &[Vec<u32>]) -> Self {
        let height = node_heights(order, succ);
        let levels = Levels::new(&height);
        let (bit, by_bit) = levels.number(&height);
        Self {
            levels,
            height,
            bit,
            by_bit,
        }
    }

    /// Fills one reachability row per node that is not a sink, handing `finish`
    /// each node's filled row — the words of it holding bits, the first of them
    /// being the row's word `lo` — with the successors a transitive reduction
    /// keeps of it, and `take` the node with what `finish` made of them. A sink
    /// is handed to neither.
    ///
    /// Every node of one height reaches only lower heights, so a height's rows
    /// are filled together from rows already settled, in parallel where there
    /// are enough of them. The rows are dropped once every height is filled.
    fn fill<T: Send>(
        &self,
        succ: &[Vec<u32>],
        finish: impl Fn(usize, &[u64], &[u32]) -> T + Sync,
        mut take: impl FnMut(u32, T) + Send,
    ) {
        let levels = &self.levels;
        let mut rows = Rows::new(levels, &self.height, &self.bit);
        let mut fill_heights = || {
            for h in 1..levels.count() {
                let nodes = &self.by_bit[levels.bits(h)];
                rows.fill_height(levels, h, nodes, succ, &self.bit, &finish, &mut take);
            }
        };
        // Entering the pool queues behind whatever it is running, which a
        // caller with nothing to split need not wait for.
        if (1..levels.count()).any(|h| splits(levels.bits(h).len())) {
            bviz_parallel::workers().install(fill_heights);
        } else {
            fill_heights();
        }
    }
}

/// Each node's height in the DAG `succ` holds: the longest path from it to a
/// sink. `order` is a topological order of the DAG.
fn node_heights(order: &[u32], succ: &[Vec<u32>]) -> Vec<u32> {
    let mut height = vec![0u32; succ.len()];
    for &u in order.iter().rev() {
        height[u as usize] = succ[u as usize]
            .iter()
            .map(|&w| height[w as usize] + 1)
            .max()
            .unwrap_or(0);
    }
    height
}

/// Where the nodes of each height sit among the bits of the reachability rows,
/// and their rows among the words.
struct Levels {
    /// The nodes of height `h` hold the bits `first[h]..first[h + 1]`.
    first: Vec<usize>,
    /// The rows of height `h` lie together at the words
    /// `rows_at[h]..rows_at[h + 1]`.
    rows_at: Vec<usize>,
}

impl Levels {
    /// Lays out the heights `height` gives by node.
    fn new(height: &[u32]) -> Self {
        let heights = height.iter().max().map_or(0, |&h| h as usize + 1);
        let mut first = vec![0usize; heights + 1];
        for &h in height {
            first[h as usize + 1] += 1;
        }
        for h in 0..heights {
            first[h + 1] += first[h];
        }
        let mut levels = Self {
            first,
            rows_at: vec![0; heights + 1],
        };
        for h in 0..heights {
            levels.rows_at[h + 1] = levels.rows_at[h] + levels.bits(h).len() * levels.width(h);
        }
        levels
    }

    fn count(&self) -> usize {
        self.first.len() - 1
    }

    fn bits(&self, h: usize) -> Range<usize> {
        self.first[h]..self.first[h + 1]
    }

    fn words(&self, h: usize) -> Range<usize> {
        self.rows_at[h]..self.rows_at[h + 1]
    }

    /// How many words the rows of every height take together.
    fn word_count(&self) -> usize {
        self.rows_at[self.count()]
    }

    /// How many words a row of height `h` spans: those of the bits below the
    /// height, so a sink's row is empty.
    fn width(&self, h: usize) -> usize {
        self.first[h].div_ceil(64)
    }

    /// Where the row of the node of height `h` holding bit `b` begins.
    fn row_start(&self, h: usize, b: u32) -> usize {
        self.rows_at[h] + (b as usize - self.first[h]) * self.width(h)
    }

    /// Gives each node of `height` a bit within its height, returning the bit
    /// by node and the node by bit.
    fn number(&self, height: &[u32]) -> (Vec<u32>, Vec<u32>) {
        let n = height.len();
        let mut bit: Vec<u32> = vec![0; n];
        let mut by_bit: Vec<u32> = vec![0; n];
        let mut next = self.first.clone();
        for (u, &h) in height.iter().enumerate() {
            bit[u] = next[h as usize] as u32;
            by_bit[next[h as usize]] = u as u32;
            next[h as usize] += 1;
        }
        (bit, by_bit)
    }
}

/// Whether [`Settled::fill_level`] fills a height of `rows` rows in more than
/// one task.
fn splits(rows: usize) -> bool {
    rows >= 2 * ROWS_PER_TASK
}

/// The reachability rows of [`Reach::fill`], by node.
struct Rows {
    reach: Vec<u64>,
    /// Where each node's row begins in `reach`.
    start: Vec<usize>,
    /// The words `lo..hi` each row has bits in, the only ones a union reads.
    span: Vec<(u32, u32)>,
}

impl Rows {
    /// Empty rows where `levels` places the nodes `height` and `bit` give.
    fn new(levels: &Levels, height: &[u32], bit: &[u32]) -> Self {
        let start = height
            .iter()
            .zip(bit)
            .map(|(&h, &b)| levels.row_start(h as usize, b))
            .collect();
        Self {
            reach: vec![0u64; levels.word_count()],
            start,
            span: vec![(0, 0); height.len()],
        }
    }

    /// Fills the rows of `nodes`, the nodes of height `h`, from those of the
    /// heights below, as [`Reach::fill`] does.
    #[allow(clippy::too_many_arguments)]
    fn fill_height<T: Send>(
        &mut self,
        levels: &Levels,
        h: usize,
        nodes: &[u32],
        succ: &[Vec<u32>],
        bit: &[u32],
        finish: &(impl Fn(usize, &[u64], &[u32]) -> T + Sync),
        take: &mut impl FnMut(u32, T),
    ) {
        let words = levels.words(h);
        let (settled, level) = self.reach.split_at_mut(words.start);
        let settled = Settled {
            reach: settled,
            start: &self.start,
            span: &self.span,
        };
        let filled = settled.fill_level(
            &mut level[..words.len()],
            levels.width(h),
            nodes,
            succ,
            bit,
            finish,
        );
        for (&u, (span, finished)) in nodes.iter().zip(filled) {
            self.span[u as usize] = span;
            take(u, finished);
        }
    }
}

/// The rows of the heights below the one being filled, which it reads.
struct Settled<'a> {
    reach: &'a [u64],
    start: &'a [usize],
    span: &'a [(u32, u32)],
}

impl Settled<'_> {
    /// Fills `level`, the rows of `nodes` at `width` words each, returning
    /// each node's [`Settled::fill_node`].
    fn fill_level<T: Send>(
        &self,
        level: &mut [u64],
        width: usize,
        nodes: &[u32],
        succ: &[Vec<u32>],
        bit: &[u32],
        finish: &(impl Fn(usize, &[u64], &[u32]) -> T + Sync),
    ) -> Vec<((u32, u32), T)> {
        if splits(nodes.len()) {
            level
                .par_chunks_mut(width)
                .zip(nodes.par_iter())
                .with_min_len(ROWS_PER_TASK)
                .map_init(Vec::new, |highest_first, (row, &u)| {
                    self.fill_node(highest_first, row, &succ[u as usize], bit, finish)
                })
                .collect()
        } else {
            let mut highest_first = Vec::new();
            level
                .chunks_mut(width)
                .zip(nodes)
                .map(|(row, &u)| {
                    self.fill_node(&mut highest_first, row, &succ[u as usize], bit, finish)
                })
                .collect()
        }
    }

    /// [`Settled::fill`] with a node's distinct `successors`, returning the
    /// words `row` has bits in and what `finish` makes of the filled row and the
    /// successors it keeps. `highest_first` is scratch.
    fn fill_node<T>(
        &self,
        highest_first: &mut Vec<u32>,
        row: &mut [u64],
        successors: &[u32],
        bit: &[u32],
        finish: &impl Fn(usize, &[u64], &[u32]) -> T,
    ) -> ((u32, u32), T) {
        highest_first.clear();
        highest_first.extend_from_slice(successors);
        highest_first.sort_unstable_by_key(|&w| Reverse(bit[w as usize]));
        highest_first.dedup();
        let (lo, hi) = self.fill(row, highest_first, bit);
        let words = &row[lo as usize..hi as usize];
        ((lo, hi), finish(lo as usize, words, highest_first))
    }

    /// Fills `row` with what a node reaches through `successors`, given highest
    /// bit first, leaves in `successors` only those whose edge the reduction
    /// keeps, and returns the words `row` has bits in. A successor another one
    /// reaches sits lower, so it is found already set, which marks its edge
    /// redundant and leaves its row unread, being inside the union already.
    fn fill(&self, row: &mut [u64], successors: &mut Vec<u32>, bit: &[u32]) -> (u32, u32) {
        let (mut lo, mut hi) = (u32::MAX, 0);
        successors.retain(|&w| {
            let b = bit[w as usize] as usize;
            let (word, mask) = (b / 64, 1u64 << (b % 64));
            if row[word] & mask != 0 {
                return false;
            }
            let (w_lo, w_hi) = self.span[w as usize];
            let w_row = &self.reach[self.start[w as usize]..];
            for k in w_lo as usize..w_hi as usize {
                row[k] |= w_row[k];
            }
            row[word] |= mask;
            lo = lo.min(w_lo).min(word as u32);
            hi = hi.max(w_hi).max(word as u32 + 1);
            true
        });
        (lo, hi)
    }
}

/// The fewest rows one parallel task of [`Settled::fill_level`] fills, so a
/// height of few nodes is filled on one thread.
const ROWS_PER_TASK: usize = 64;

/// Cycle-tolerant reduction: drop a pair only while its endpoints stay linked
/// through the pairs kept so far, visiting pairs in a fixed order for a
/// deterministic result.
fn condensed_reduction(graph: Graph) -> Graph {
    let mut index: FxHashMap<Id, u32> =
        FxHashMap::with_capacity_and_hasher(graph.nodes.len(), Default::default());
    for node in &graph.nodes {
        let next = index.len() as u32;
        index.entry(node.id).or_insert(next);
    }
    let n = index.len();
    let pairs: FxHashSet<(u32, u32)> = graph
        .edges
        .iter()
        .filter(|e| e.from != e.to)
        .filter_map(|e| Some((*index.get(&e.from)?, *index.get(&e.to)?)))
        .collect();
    let mut succ: Vec<Vec<u32>> = vec![Vec::new(); n];
    for &(u, v) in &pairs {
        succ[u as usize].push(v);
    }

    let condensation = Condensation::new(&succ);
    let comp = &condensation.of;
    let reduction = Reduction::new(&condensation.order, &condensation.succ);

    let mut within: FxHashMap<u32, Vec<(u32, u32)>> = FxHashMap::default();
    // One edge into a cycle reaches all of it, and one out of a cycle leaves
    // from all of it, so a pair of cycles the reduction keeps joined needs only
    // the one edge between them; the lowest is taken so a graph always keeps the
    // same one.
    let mut between: FxHashMap<(u32, u32), (u32, u32)> = FxHashMap::default();
    for &(u, v) in &pairs {
        let (a, b) = (comp[u as usize], comp[v as usize]);
        match a == b {
            true => within.entry(a).or_default().push((u, v)),
            false => {
                let lowest = between.entry((a, b)).or_insert((u, v));
                *lowest = (*lowest).min((u, v));
            }
        }
    }
    let mut held: FxHashSet<(u32, u32)> = FxHashSet::default();
    for cycle in within.values() {
        held.extend(thin(cycle));
    }
    held.extend(
        between
            .iter()
            .filter(|&(&(a, b), _)| reduction.keeps(a, b))
            .map(|(_, &edge)| edge),
    );

    let mut out = graph;
    out.edges.retain(|e| {
        e.from == e.to
            || match (index.get(&e.from), index.get(&e.to)) {
                (Some(&u), Some(&v)) => held.contains(&(u, v)),
                _ => true,
            }
    });
    out
}

/// The edges of one cycle that hold it together. Each is dropped in turn where
/// the rest still carry its source to its target, leaving every node of the
/// cycle reaching every other over fewer edges than it began with. A cycle has
/// no unique reduction, so which edges survive follows the order they are tried
/// in; that order is by index, so one graph always reduces the same way.
///
/// Every path between two nodes of a cycle stays inside it, so the search needs
/// none of the graph around it and costs the cycle's own size rather than the
/// whole view's.
fn thin(cycle: &[(u32, u32)]) -> FxHashSet<(u32, u32)> {
    let mut order = cycle.to_vec();
    order.sort_unstable();
    let thinned = ThinnedCycle::new(&order);
    order
        .into_iter()
        .zip(thinned.kept)
        .filter_map(|(edge, kept)| kept.then_some(edge))
        .collect()
}

/// One cycle's edges over its own nodes, numbered densely so a search over them
/// indexes arrays, with which of the edges [`thin`] keeps.
struct ThinnedCycle {
    /// By node, the edges leaving it, as indices into the edge list.
    leaving: Vec<Vec<u32>>,
    /// By edge, the node it enters.
    target: Vec<u32>,
    /// By edge, whether it is still part of the cycle.
    kept: Vec<bool>,
    /// By node, the search that last reached it, so a search starts clean
    /// without clearing the table.
    reached_by: Vec<u32>,
    search: u32,
    /// The nodes a search has reached and not yet walked on from.
    walk: Vec<u32>,
}

impl ThinnedCycle {
    /// Thins `edges`, trying each in the order given.
    fn new(edges: &[(u32, u32)]) -> Self {
        let mut node: FxHashMap<u32, u32> =
            FxHashMap::with_capacity_and_hasher(edges.len(), Default::default());
        let mut dense = |id: u32| {
            let next = node.len() as u32;
            *node.entry(id).or_insert(next)
        };
        let ends: Vec<(u32, u32)> = edges.iter().map(|&(u, v)| (dense(u), dense(v))).collect();
        let mut leaving: Vec<Vec<u32>> = vec![Vec::new(); node.len()];
        for (edge, &(u, _)) in ends.iter().enumerate() {
            leaving[u as usize].push(edge as u32);
        }
        let mut cycle = ThinnedCycle {
            leaving,
            target: ends.iter().map(|&(_, v)| v).collect(),
            kept: vec![true; ends.len()],
            reached_by: vec![0; node.len()],
            search: 0,
            walk: Vec::with_capacity(node.len()),
        };
        for (edge, &(u, v)) in ends.iter().enumerate() {
            cycle.kept[edge] = false;
            cycle.kept[edge] = !cycle.reaches(u, v);
        }
        cycle
    }

    /// Whether `dst` is reachable from `src` over the edges still kept.
    fn reaches(&mut self, src: u32, dst: u32) -> bool {
        self.search += 1;
        self.walk.clear();
        self.walk.push(src);
        while let Some(u) = self.walk.pop() {
            for &edge in &self.leaving[u as usize] {
                if !self.kept[edge as usize] {
                    continue;
                }
                let w = self.target[edge as usize];
                if w == dst {
                    return true;
                }
                if self.reached_by[w as usize] != self.search {
                    self.reached_by[w as usize] = self.search;
                    self.walk.push(w);
                }
            }
        }
        false
    }
}

/// A count of nodes, found exactly or estimated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Count {
    Exact(u32),
    Estimated(u32),
}

impl Count {
    pub fn value(self) -> u32 {
        match self {
            Count::Exact(n) | Count::Estimated(n) => n,
        }
    }

    pub fn is_estimated(self) -> bool {
        matches!(self, Count::Estimated(_))
    }

    /// The count with `n` of what it counts left out, and zero where `n`
    /// exceeds it.
    pub fn less(self, n: u32) -> Self {
        match self {
            Count::Exact(m) => Count::Exact(m.saturating_sub(n)),
            Count::Estimated(m) => Count::Estimated(m.saturating_sub(n)),
        }
    }
}

impl std::fmt::Display for Count {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Count::Exact(n) => write!(f, "{n}"),
            Count::Estimated(n) => write!(f, "≈{n}"),
        }
    }
}

/// The most words the reachability rows of an exact count may take before the
/// counts are estimated instead. On action graphs about this size the two take
/// alike, and past it the rows cost more time than the estimate, and memory
/// growing with the square of the nodes where the estimate's grows with them.
const EXACT_REACH_WORDS: usize = 1 << 22;

/// How many other nodes each node of the graph `succ` holds over the indices
/// `0..succ.len()` reaches, of those `counts` takes by index. An estimate
/// samples a counted node by the key `key` gives its index, so a node keyed
/// alike in two graphs is sampled alike in both. Each node of a cycle reaches
/// every other node of it and all that any of them reaches.
pub fn reach_counts(
    succ: &[Vec<u32>],
    counts: impl Fn(usize) -> bool,
    key: impl Fn(usize) -> u64,
) -> Vec<Count> {
    counts_within(succ, counts, key, EXACT_REACH_WORDS)
}

/// [`reach_counts`], exact where the rows take at most `exact_words` words.
fn counts_within(
    succ: &[Vec<u32>],
    counts: impl Fn(usize) -> bool,
    key: impl Fn(usize) -> u64,
    exact_words: usize,
) -> Vec<Count> {
    let condensation = Condensation::new(succ);
    let reach = Reach::new(&condensation.order, &condensation.succ);
    let whole: Vec<Count> = if reach.levels.word_count() <= exact_words {
        exact_counts(&condensation, &reach, &counts)
    } else {
        let Reach { height, .. } = reach;
        crate::sketch::sketched(&condensation, &height, |i| counts(i).then(|| key(i)))
    };
    condensation
        .of
        .iter()
        .enumerate()
        .map(|(i, &c)| whole[c as usize].less(u32::from(counts(i))))
        .collect()
}

/// By component of `condensation`, how many counted nodes it holds and reaches
/// together, found through the reachability rows `reach` numbers, of the
/// nodes `counts` takes by index.
fn exact_counts(
    condensation: &Condensation,
    reach: &Reach,
    counts: impl Fn(usize) -> bool,
) -> Vec<Count> {
    let mut weight = vec![0u32; condensation.succ.len()];
    for (i, &c) in condensation.of.iter().enumerate() {
        weight[c as usize] += u32::from(counts(i));
    }
    let weights = Weights::new(&weight, &reach.bit);
    let mut reached = vec![0u32; weight.len()];
    reach.fill(
        &condensation.succ,
        |lo, words, _| weights.covered(lo, words),
        |c, count| reached[c as usize] = count,
    );
    reached
        .into_iter()
        .zip(weight)
        .map(|(reached, own)| Count::Exact(reached + own))
        .collect()
}

/// How many counted nodes each bit of a condensation's reachability rows
/// stands for: one, but for a cycle or an uncounted node.
struct Weights {
    /// By word, the bits standing for other than one node.
    mask: Vec<u64>,
    /// By bit, how many counted nodes past one it stands for.
    more: Vec<i32>,
}

impl Weights {
    /// The components of the counted-node weights `weight` gives, each holding
    /// the bit `bit` gives it.
    fn new(weight: &[u32], bit: &[u32]) -> Self {
        let mut mask = vec![0u64; weight.len().div_ceil(64)];
        let mut more = vec![0i32; weight.len()];
        for (&weight, &b) in weight.iter().zip(bit).filter(|&(&weight, _)| weight != 1) {
            mask[b as usize / 64] |= 1 << (b % 64);
            more[b as usize] = weight as i32 - 1;
        }
        Self { mask, more }
    }

    /// How many counted nodes a reachability row covers, `words` being the
    /// words of it from its word `lo` on.
    fn covered(&self, lo: usize, words: &[u64]) -> u32 {
        let mut count: i64 = 0;
        for (k, &word) in (lo..).zip(words) {
            count += i64::from(word.count_ones());
            let mut weighted = word & self.mask[k];
            while weighted != 0 {
                count += i64::from(self.more[k * 64 + weighted.trailing_zeros() as usize]);
                weighted &= weighted - 1;
            }
        }
        count as u32
    }
}

/// The acyclic graph a graph's strongly connected components leave: each cycle
/// of the original is one index here. A cyclic graph admits no topological
/// order, so a question settled by a single walk over a DAG has to be asked of
/// this instead — sound for any answer every node of a cycle shares, such as
/// what it reaches.
pub(crate) struct Condensation {
    /// The component each index of the original graph belongs to.
    pub(crate) of: Vec<u32>,
    /// Successors of each component, deduplicated.
    pub(crate) succ: Vec<Vec<u32>>,
    /// The components with every one ahead of its successors, so a walk needing
    /// its successors settled first takes this in reverse.
    pub(crate) order: Vec<u32>,
}

impl Condensation {
    /// Condenses the graph `succ` holds over the indices `0..succ.len()`.
    pub(crate) fn new(succ: &[Vec<u32>]) -> Self {
        let mut pred: Vec<Vec<u32>> = vec![Vec::new(); succ.len()];
        for (u, forward) in succ.iter().enumerate() {
            for &w in forward {
                pred[w as usize].push(u as u32);
            }
        }
        let of = components(succ, &pred);
        let count = of.iter().copied().max().map_or(0, |c| c as usize + 1);
        // The indices grouped by component, in ascending order within each,
        // and by component where its group ends.
        let mut end = vec![0usize; count];
        for &c in &of {
            end[c as usize] += 1;
        }
        let mut filled = 0;
        for group in &mut end {
            filled += std::mem::replace(group, filled);
        }
        let mut members = vec![0u32; of.len()];
        for (u, &c) in of.iter().enumerate() {
            members[end[c as usize]] = u as u32;
            end[c as usize] += 1;
        }

        /// Stands for a component no edge has been found into.
        const UNENTERED: u32 = u32::MAX;

        // By component, the last component an edge into it was found from: all
        // the edges out of one component are read together, so a second edge
        // between two components finds the first one's mark still standing.
        let mut entered_from = vec![UNENTERED; count];
        let mut condensed: Vec<Vec<u32>> = vec![Vec::new(); count];
        let mut indeg: Vec<u32> = vec![0; count];
        let mut start = 0;
        for (a, &end) in end.iter().enumerate() {
            let a = a as u32;
            for &u in &members[start..end] {
                for &v in &succ[u as usize] {
                    let b = of[v as usize];
                    if a != b && std::mem::replace(&mut entered_from[b as usize], a) != a {
                        condensed[a as usize].push(b);
                        indeg[b as usize] += 1;
                    }
                }
            }
            start = end;
        }
        let order = topo_order(&condensed, indeg).expect("a condensation holds no cycle");
        Self {
            of,
            succ: condensed,
            order,
        }
    }
}

/// The strongly connected component each index of the graph `succ` belongs to
/// — two indices share one exactly when each reaches the other — numbered
/// densely from zero. `pred` holds the same graph's edges reversed.
pub(crate) fn components(succ: &[Vec<u32>], pred: &[Vec<u32>]) -> Vec<u32> {
    let n = succ.len();
    // The order the search finishes with each index, which is the order the
    // second walk has to reach them in, reversed.
    let mut seen = vec![false; n];
    let mut finished: Vec<u32> = Vec::with_capacity(n);
    let mut walk: Vec<(u32, usize)> = Vec::new();
    for start in 0..n as u32 {
        if seen[start as usize] {
            continue;
        }
        seen[start as usize] = true;
        walk.push((start, 0));
        while let Some((u, next)) = walk.pop() {
            match succ[u as usize].get(next) {
                Some(&w) => {
                    walk.push((u, next + 1));
                    if !seen[w as usize] {
                        seen[w as usize] = true;
                        walk.push((w, 0));
                    }
                }
                None => finished.push(u),
            }
        }
    }

    /// Stands in the component table for an index no component has claimed.
    const UNCLAIMED: u32 = u32::MAX;

    let mut comp = vec![UNCLAIMED; n];
    let mut next = 0;
    for &root in finished.iter().rev() {
        if comp[root as usize] != UNCLAIMED {
            continue;
        }
        comp[root as usize] = next;
        let mut walk = vec![root];
        while let Some(u) = walk.pop() {
            for &w in &pred[u as usize] {
                if comp[w as usize] == UNCLAIMED {
                    comp[w as usize] = next;
                    walk.push(w);
                }
            }
        }
        next += 1;
    }
    comp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{EdgeKind, Kind, Node};
    use crate::intern::Names;
    use fastrand::Rng;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn a_count_less_more_than_it_holds_is_zero() {
        assert_eq!(Count::Exact(3).less(2), Count::Exact(1));
        assert_eq!(Count::Exact(2).less(3), Count::Exact(0));
        assert_eq!(Count::Estimated(2).less(3), Count::Estimated(0));
    }

    fn build(edges: &[(&str, &str)]) -> (Graph, Names) {
        let mut g = Graph::new();
        let mut n = Names::default();
        let mut seen = std::collections::HashSet::new();
        for &(a, b) in edges {
            for id in [a, b] {
                if seen.insert(id) {
                    g.add_node(Node::new(n.intern(id), id, Kind::Action, None));
                }
            }
        }
        for &(a, b) in edges {
            g.add_edge(n.intern(a), n.intern(b), EdgeKind::Aquery);
        }
        (g, n)
    }

    fn kept(g: &Graph, n: &Names) -> std::collections::HashSet<(String, String)> {
        g.edges
            .iter()
            .map(|e| (n.resolve(e.from).to_string(), n.resolve(e.to).to_string()))
            .collect()
    }

    #[test]
    fn drops_an_edge_a_longer_path_implies() {
        // a->b->c plus a redundant a->c.
        let (g, n) = build(&[("a", "b"), ("b", "c"), ("a", "c")]);
        let r = transitive_reduction(&g);
        let e = kept(&r, &n);
        assert!(e.contains(&("a".into(), "b".into())));
        assert!(e.contains(&("b".into(), "c".into())));
        assert!(!e.contains(&("a".into(), "c".into())), "a->c is redundant");
        // All nodes survive.
        assert_eq!(r.nodes.len(), 3);
    }

    #[test]
    fn keeps_a_direct_edge_with_no_alternate_path() {
        let (g, n) = build(&[("a", "b"), ("a", "c")]);
        let r = transitive_reduction(&g);
        assert_eq!(kept(&r, &n).len(), 2);
    }

    #[test]
    fn a_cycle_keeps_every_edge_so_reachability_survives() {
        // a->b->c->a: none is redundant (its endpoints only connect through it).
        let (g, n) = build(&[("a", "b"), ("b", "c"), ("c", "a")]);
        assert_eq!(kept(&transitive_reduction(&g), &n).len(), 3);
    }

    #[test]
    fn reduction_never_strands_a_reachable_node() {
        // a fans into a 2-cycle {b,c}; removing both a->b and a->c would strand
        // them, so at least one survives.
        let (g, n) = build(&[("a", "b"), ("a", "c"), ("b", "c"), ("c", "b")]);
        let r = transitive_reduction(&g);
        let e = kept(&r, &n);
        assert!(e.contains(&("a".into(), "b".into())) || e.contains(&("a".into(), "c".into())));
    }

    #[test]
    fn diamond_keeps_both_branches_drops_the_shortcut() {
        // a->b->d and a->c->d: both branches stay (neither implies the other),
        // only the direct a->d is redundant.
        let (g, n) = build(&[("a", "b"), ("a", "c"), ("b", "d"), ("c", "d"), ("a", "d")]);
        let e = kept(&transitive_reduction(&g), &n);
        assert_eq!(e.len(), 4);
        assert!(!e.contains(&("a".into(), "d".into())));
    }

    #[test]
    fn a_chain_drops_every_skip_edge() {
        // a->b->c->d with all forward skips; only the three chain steps remain.
        let (g, n) = build(&[
            ("a", "b"),
            ("b", "c"),
            ("c", "d"),
            ("a", "c"),
            ("a", "d"),
            ("b", "d"),
        ]);
        let e = kept(&transitive_reduction(&g), &n);
        assert_eq!(
            e,
            [("a", "b"), ("b", "c"), ("c", "d")]
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        );
    }

    /// A path `n0 -> n1 -> ... -> n{len-1}` plus, from `n0`, a redundant skip to
    /// every later node, named so the reachability bitset must span several
    /// 64-bit words.
    fn long_chain_with_skips(len: usize) -> Vec<(String, String)> {
        let name = |i: usize| format!("n{i:03}");
        let mut edges: Vec<(String, String)> =
            (0..len - 1).map(|i| (name(i), name(i + 1))).collect();
        edges.extend((2..len).map(|j| (name(0), name(j))));
        edges
    }

    #[test]
    fn the_bitset_path_spans_multiple_words() {
        // 100 nodes > 64, so reach[] uses two words and the v/64 indexing matters.
        let len = 100;
        let owned = long_chain_with_skips(len);
        let refs: Vec<(&str, &str)> = owned
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let (g, n) = build(&refs);
        // The fast path must apply (the graph is acyclic).
        assert!(dag_reduction(g.clone()).is_ok());
        let e = kept(&transitive_reduction(&g), &n);
        let chain: HashSet<(String, String)> = (0..len - 1)
            .map(|i| (format!("n{i:03}"), format!("n{:03}", i + 1)))
            .collect();
        assert_eq!(e, chain);
    }

    /// Every pair one graph links the other must link too, which is what a
    /// reduction is allowed to trade edges for.
    fn reaches_alike(before: &Graph, after: &Graph, n: &Names) {
        let out = |g: &Graph| {
            let mut m: HashMap<Id, Vec<Id>> = HashMap::new();
            for e in &g.edges {
                m.entry(e.from).or_default().push(e.to);
            }
            m
        };
        let (was, now) = (out(before), out(after));
        let seen_from = |m: &HashMap<Id, Vec<Id>>, src: Id| {
            let mut seen: HashSet<Id> = HashSet::new();
            let mut walk = vec![src];
            while let Some(u) = walk.pop() {
                for &w in m.get(&u).map(Vec::as_slice).unwrap_or(&[]) {
                    if seen.insert(w) {
                        walk.push(w);
                    }
                }
            }
            seen
        };
        for node in &before.nodes {
            let (a, b) = (seen_from(&was, node.id), seen_from(&now, node.id));
            for target in a {
                assert!(
                    b.contains(&target),
                    "{} no longer reaches {}",
                    n.resolve(node.id),
                    n.resolve(target),
                );
            }
        }
    }

    /// A cycle keeps its nodes reaching one another, but the edges that do more
    /// than that are still dropped: a chord across a cycle is implied by the way
    /// round.
    #[test]
    fn a_cycle_drops_a_chord_it_already_implies() {
        let (g, n) = build(&[
            ("a", "b"),
            ("b", "c"),
            ("c", "d"),
            ("d", "a"),
            // Implied by b -> c -> d.
            ("b", "d"),
        ]);
        let r = transitive_reduction(&g);
        let e = kept(&r, &n);
        assert!(!e.contains(&("b".into(), "d".into())), "the chord survived");
        assert_eq!(e.len(), 4, "the cycle itself lost an edge: {e:?}");
        reaches_alike(&g, &r, &n);
    }

    /// An edge into a cycle that another edge into the same cycle already
    /// implies is dropped: reaching any node of a cycle reaches all of them.
    #[test]
    fn one_edge_into_a_cycle_stands_for_the_rest() {
        let (g, n) = build(&[
            ("a", "b"),
            ("b", "c"),
            ("c", "a"),
            ("d", "a"),
            // d already reaches b through a.
            ("d", "b"),
        ]);
        let r = transitive_reduction(&g);
        let e = kept(&r, &n);
        assert_eq!(
            e.iter().filter(|(from, _)| from == "d").count(),
            1,
            "d keeps more than one way into the cycle: {e:?}",
        );
        reaches_alike(&g, &r, &n);
    }

    /// Two cycles joined by several edges keep one of them; the rest are what
    /// that one already implies.
    #[test]
    fn cycles_joined_many_ways_keep_one_way() {
        let (g, n) = build(&[
            ("a1", "a2"),
            ("a2", "a1"),
            ("b1", "b2"),
            ("b2", "b1"),
            ("a1", "b1"),
            ("a1", "b2"),
            ("a2", "b1"),
            ("a2", "b2"),
        ]);
        let r = transitive_reduction(&g);
        let e = kept(&r, &n);
        let crossing = e
            .iter()
            .filter(|(from, to)| from.starts_with('a') && to.starts_with('b'))
            .count();
        assert_eq!(crossing, 1, "more than one crossing edge kept: {e:?}");
        reaches_alike(&g, &r, &n);
    }

    /// A graph a fold leaves cyclic still reduces in one pass over the acyclic
    /// graph its cycles condense to, so its cost follows its size rather than
    /// the product of its edges and its nodes.
    #[test]
    fn a_graph_of_many_cycles_reduces() {
        // Twenty cycles of five, chained so the condensation is a long path,
        // and every chain step doubled so the reduction has something to drop.
        let mut edges: Vec<(String, String)> = Vec::new();
        for c in 0..20 {
            for i in 0..5 {
                edges.push((format!("c{c}n{i}"), format!("c{c}n{}", (i + 1) % 5)));
            }
            if c > 0 {
                edges.push((format!("c{}n0", c - 1), format!("c{c}n0")));
                edges.push((format!("c{}n1", c - 1), format!("c{c}n1")));
            }
        }
        let refs: Vec<(&str, &str)> = edges
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let (g, n) = build(&refs);
        let r = transitive_reduction(&g);
        reaches_alike(&g, &r, &n);
        // Each pair of neighbouring cycles is joined two ways and needs one.
        assert_eq!(kept(&r, &n).len(), 20 * 5 + 19);
    }

    /// The edges of `g` between its nodes whose target no other successor of
    /// their source reaches: the definition the bitset reduction must agree
    /// with. `g`'s edges must ascend in node order, so a node's reach is settled
    /// from the last node down.
    fn naive_reduction(g: &Graph) -> HashSet<(Id, Id)> {
        let nodes: HashSet<Id> = g.nodes.iter().map(|node| node.id).collect();
        let mut succ: HashMap<Id, HashSet<Id>> = HashMap::new();
        for e in g.edges.iter().filter(|e| e.from != e.to) {
            if nodes.contains(&e.from) && nodes.contains(&e.to) {
                succ.entry(e.from).or_default().insert(e.to);
            }
        }
        let mut reach: HashMap<Id, HashSet<Id>> = HashMap::new();
        for node in g.nodes.iter().rev() {
            let mut all = HashSet::new();
            for &w in succ.get(&node.id).into_iter().flatten() {
                all.insert(w);
                all.extend(&reach[&w]);
            }
            reach.insert(node.id, all);
        }
        let mut kept = HashSet::new();
        for (&u, targets) in &succ {
            for &v in targets {
                if !targets.iter().any(|&w| w != v && reach[&w].contains(&v)) {
                    kept.insert((u, v));
                }
            }
        }
        kept
    }

    /// `g`'s edges, in order, that a reduction keeping the pairs `kept` leaves:
    /// every copy of a kept pair, every self-loop, and every edge naming an id
    /// that is not one of `g`'s nodes.
    fn edges_keeping(g: &Graph, kept: &HashSet<(Id, Id)>) -> Vec<(Id, Id)> {
        let nodes: HashSet<Id> = g.nodes.iter().map(|node| node.id).collect();
        g.edges
            .iter()
            .map(|e| (e.from, e.to))
            .filter(|&(u, v)| {
                u == v || !nodes.contains(&u) || !nodes.contains(&v) || kept.contains(&(u, v))
            })
            .collect()
    }

    fn pairs(g: &Graph) -> Vec<(Id, Id)> {
        g.edges.iter().map(|e| (e.from, e.to)).collect()
    }

    /// Seeds a randomized test's generator, so the test draws the same graphs
    /// on every run.
    const SEED: u64 = 0x9E37_79B9_7F4A_7C15;

    /// A graph of `nodes` nodes and `absent` further ids that name no node, the
    /// ids of both in that order.
    fn with_absent_ids(nodes: usize, absent: usize) -> (Graph, Names, Vec<Id>) {
        let mut g = Graph::new();
        let mut n = Names::default();
        let mut ids: Vec<Id> = (0..nodes)
            .map(|i| {
                let name = format!("n{i}");
                let id = n.intern(&name);
                g.add_node(Node::new(id, &name, Kind::Action, None));
                id
            })
            .collect();
        ids.extend((0..absent).map(|i| n.intern(&format!("absent{i}"))));
        (g, n, ids)
    }

    /// Random DAGs wide enough that a height's rows are filled by several
    /// parallel tasks and a row spans several words, with repeated edges,
    /// self-loops and edges naming ids that are no node mixed in, reduce to what
    /// the definition keeps, each copy of a kept pair kept.
    #[test]
    fn random_dags_reduce_as_the_definition_does() {
        const ABSENT: usize = 5;
        let mut rng = Rng::with_seed(SEED);
        let mut split = false;
        for (nodes, edges) in [(40, 120), (700, 4_000), (2_000, 6_000), (2_000, 30_000)] {
            let (mut g, _, ids) = with_absent_ids(nodes, ABSENT);
            for _ in 0..edges {
                let (a, b) = (rng.usize(..nodes), rng.usize(..nodes));
                // Edges point from lower to higher index, so the graph is acyclic.
                g.add_edge(ids[a.min(b)], ids[a.max(b)], EdgeKind::Aquery);
            }
            for _ in 0..ABSENT {
                let absent = ids[nodes + rng.usize(..ABSENT)];
                g.add_edge(ids[rng.usize(..nodes)], absent, EdgeKind::Aquery);
                g.add_edge(absent, ids[rng.usize(..nodes)], EdgeKind::Aquery);
            }
            let r = dag_reduction(g.clone())
                .ok()
                .expect("edges ascending by index form no cycle");
            let expected = edges_keeping(&g, &naive_reduction(&g));
            assert_eq!(pairs(&r), expected, "{nodes} nodes, {edges} edges");

            let (index, count) = DenseIds::new(g.nodes.iter().map(|node| node.id));
            let mut succ: Vec<Vec<u32>> = vec![Vec::new(); count as usize];
            for e in &g.edges {
                if let (Some(u), Some(v)) = (index.get(e.from), index.get(e.to))
                    && u != v
                {
                    succ[u as usize].push(v);
                }
            }
            let order: Vec<u32> = (0..count).collect();
            let mut rows = HashMap::new();
            for h in node_heights(&order, &succ) {
                *rows.entry(h).or_insert(0) += 1;
            }
            split |= rows.into_iter().any(|(h, rows)| h > 0 && splits(rows));
        }
        assert!(split, "no height was filled by more than one task");
    }

    /// Random graphs, most of them cyclic, reduce through the cycle-tolerant
    /// path to a graph that reaches what they reach, over their own edges with
    /// every copy of a kept pair, where no kept pair is implied by the rest.
    #[test]
    fn random_graphs_reduce_through_the_fallback_minimally() {
        const ABSENT: usize = 3;
        let mut rng = Rng::with_seed(SEED);
        for round in 0..200 {
            let nodes = 2 + rng.usize(..40);
            let edges = rng.usize(..4 * nodes);
            let (mut g, n, ids) = with_absent_ids(nodes, ABSENT);
            for _ in 0..edges {
                g.add_edge(
                    ids[rng.usize(..nodes)],
                    ids[rng.usize(..nodes)],
                    EdgeKind::Aquery,
                );
            }
            for _ in 0..rng.usize(..ABSENT) {
                g.add_edge(
                    ids[rng.usize(..nodes)],
                    ids[nodes + rng.usize(..ABSENT)],
                    EdgeKind::Aquery,
                );
            }
            let r = condensed_reduction(g.clone());
            let node_ids: HashSet<Id> = g.nodes.iter().map(|node| node.id).collect();
            let kept: HashSet<(Id, Id)> = pairs(&r)
                .into_iter()
                .filter(|&(u, v)| u != v && node_ids.contains(&u) && node_ids.contains(&v))
                .collect();
            assert_eq!(pairs(&r), edges_keeping(&g, &kept), "round {round}");
            reaches_alike(&g, &r, &n);
            for &(u, v) in &kept {
                let mut walk = vec![u];
                let mut seen = HashSet::from([u]);
                while let Some(w) = walk.pop() {
                    for e in r
                        .edges
                        .iter()
                        .filter(|e| e.from == w && (e.from, e.to) != (u, v))
                    {
                        if seen.insert(e.to) {
                            walk.push(e.to);
                        }
                    }
                }
                assert!(
                    !seen.contains(&v),
                    "round {round}: {} -> {} is implied by the rest",
                    n.resolve(u),
                    n.resolve(v),
                );
            }
        }
    }

    /// A small view of a large graph names ids far apart, which a map numbers
    /// in place of a table reaching the largest.
    #[test]
    fn a_view_of_far_apart_ids_reduces() {
        let mut n = Names::default();
        for i in 0..10_000 {
            n.intern(&format!("elsewhere{i}"));
        }
        let mut g = Graph::new();
        for name in ["a", "b", "c"] {
            g.add_node(Node::new(n.intern(name), name, Kind::Action, None));
        }
        for (a, b) in [("a", "b"), ("b", "c"), ("a", "c")] {
            g.add_edge(n.intern(a), n.intern(b), EdgeKind::Aquery);
        }
        let ids = g.nodes.iter().map(|node| node.id);
        assert!(matches!(DenseIds::new(ids).0, DenseIds::Map(_)));
        let r = dag_reduction(g).ok().expect("a chain is acyclic");
        assert_eq!(
            kept(&r, &n),
            [("a", "b"), ("b", "c")]
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        );
    }

    #[test]
    fn a_chain_reaches_everything_below_it() {
        assert_eq!(exact(&[vec![1], vec![2], vec![]], &[true; 3]), [2, 1, 0]);
    }

    #[test]
    fn a_diamond_counts_its_join_once() {
        let succ = [vec![1, 2], vec![3], vec![3], vec![]];
        assert_eq!(exact(&succ, &[true; 4]), [3, 1, 1, 0]);
    }

    /// `0` and `1` form a cycle that reaches `2`; `3` reaches into it.
    #[test]
    fn a_cycle_reaches_its_other_members_and_beyond() {
        let succ = [vec![1], vec![0, 2], vec![], vec![0]];
        assert_eq!(exact(&succ, &[true; 4]), [2, 2, 0, 3]);
    }

    /// An uncounted node is passed through but not counted, whether it stands
    /// alone or in a cycle: `1` is uncounted, and `2` and `3` a cycle of which
    /// `3` is uncounted.
    #[test]
    fn an_uncounted_node_is_passed_through() {
        let succ = [vec![1], vec![2], vec![3], vec![2, 4], vec![]];
        let counted = [true, false, true, false, true];
        assert_eq!(exact(&succ, &counted), [2, 2, 1, 2, 0]);
    }

    /// Random graphs wide enough that a height is filled by several parallel
    /// tasks and a row spans several words, with cycles among their nodes, count
    /// what a walk from each node finds.
    #[test]
    fn random_graphs_count_what_a_walk_reaches() {
        let mut rng = Rng::with_seed(SEED);
        for (nodes, edges) in [(40, 60), (2_000, 3_000), (2_000, 6_000)] {
            let succ = random_graph(&mut rng, nodes, edges);
            let counted: Vec<bool> = (0..nodes).map(|_| rng.usize(..4) != 0).collect();
            assert_eq!(
                exact(&succ, &counted),
                walked(&succ, &counted),
                "{nodes} nodes, {edges} edges"
            );
        }
    }

    /// A graph of `nodes` nodes and `edges` random edges between them, mostly
    /// ascending so that its cycles stay small.
    fn random_graph(rng: &mut Rng, nodes: usize, edges: usize) -> Vec<Vec<u32>> {
        let mut succ: Vec<Vec<u32>> = vec![Vec::new(); nodes];
        for _ in 0..edges {
            let (a, b) = (rng.usize(..nodes), rng.usize(..nodes));
            let (from, to) = match rng.usize(..50) {
                0 => (a.max(b), a.min(b)),
                _ => (a.min(b), a.max(b)),
            };
            succ[from].push(to as u32);
        }
        succ
    }

    /// Estimated counts of random graphs, cycles and uncounted nodes among
    /// them, are exact below the size of a sketch and near what a walk finds
    /// above it: within [`WORST_ERROR`] of it each, and within [`MEAN_ERROR`]
    /// on average.
    #[test]
    fn estimates_come_near_what_a_walk_reaches() {
        /// Four standard errors of a sketch's estimate.
        const WORST_ERROR: f64 = 0.35;
        /// Two standard errors, which the counts of one graph share much of,
        /// sampling nested sets.
        const MEAN_ERROR: f64 = 0.2;

        let mut rng = Rng::with_seed(SEED);
        for (nodes, edges) in [(3_000, 4_500), (3_000, 12_000)] {
            let succ = random_graph(&mut rng, nodes, edges);
            let counted: Vec<bool> = (0..nodes).map(|_| rng.usize(..4) != 0).collect();
            let counts = counts_within(&succ, |i| counted[i], |i| i as u64, 0);
            let mut errors: Vec<f64> = Vec::new();
            for (count, truth) in counts.into_iter().zip(walked(&succ, &counted)) {
                match count {
                    Count::Exact(n) => assert_eq!(n, truth),
                    Count::Estimated(n) => {
                        assert!(truth as usize + 1 >= crate::sketch::RANKS);
                        let error = (f64::from(n) - f64::from(truth)).abs() / f64::from(truth);
                        assert!(error < WORST_ERROR, "{n} estimates {truth}");
                        errors.push(error);
                    }
                }
            }
            if !errors.is_empty() {
                let mean = errors.iter().sum::<f64>() / errors.len() as f64;
                assert!(mean < MEAN_ERROR, "{nodes} nodes, {edges} edges: {mean}");
            }
        }
    }

    /// Many two-node cycles, chained so that each reaches every later one, put
    /// cycle bits in every word of the widest rows.
    #[test]
    fn a_chain_of_many_small_cycles_counts_each_member() {
        const CYCLES: u32 = 300;
        let mut succ: Vec<Vec<u32>> = vec![Vec::new(); 2 * CYCLES as usize];
        for c in 0..CYCLES {
            let (a, b) = (2 * c, 2 * c + 1);
            succ[a as usize].push(b);
            succ[b as usize].push(a);
            if c + 1 < CYCLES {
                succ[b as usize].push(a + 2);
            }
        }
        let counted = vec![true; succ.len()];
        assert_eq!(exact(&succ, &counted), walked(&succ, &counted));
    }

    /// The counts of [`reach_counts`] of `succ`, where `counted` marks the
    /// nodes that count, each of which must be exact.
    fn exact(succ: &[Vec<u32>], counted: &[bool]) -> Vec<u32> {
        reach_counts(succ, |i| counted[i], |i| i as u64)
            .into_iter()
            .map(|count| match count {
                Count::Exact(n) => n,
                Count::Estimated(_) => panic!("an estimate of a small graph"),
            })
            .collect()
    }

    /// How many other nodes of those `counted` marks a walk from each node of
    /// `succ` finds.
    fn walked(succ: &[Vec<u32>], counted: &[bool]) -> Vec<u32> {
        let nodes = succ.len();
        (0..nodes)
            .map(|start| {
                let mut seen = vec![false; nodes];
                seen[start] = true;
                let mut walk = vec![start as u32];
                let mut count = 0;
                while let Some(u) = walk.pop() {
                    for &w in &succ[u as usize] {
                        if !seen[w as usize] {
                            seen[w as usize] = true;
                            count += u32::from(counted[w as usize]);
                            walk.push(w);
                        }
                    }
                }
                count
            })
            .collect()
    }

    #[test]
    fn the_two_reductions_agree_on_a_dag() {
        // On a DAG the reduction is unique, so the fast bitset path and the
        // per-edge fallback must yield the same edges.
        let owned = long_chain_with_skips(70);
        let refs: Vec<(&str, &str)> = owned
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let (g, n) = build(&refs);
        assert_eq!(
            kept(&dag_reduction(g.clone()).ok().unwrap(), &n),
            kept(&condensed_reduction(g), &n),
        );
    }
}
