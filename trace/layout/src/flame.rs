use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bviz_trace_profile::{
    LaneClass, Profile, Slice, SliceDetail, Span32, SpanRole, Str, ThreadTrack, Workload,
};
use rayon::slice::ParallelSliceMut;
use rustc_hash::{FxHashMap, FxHashSet};

mod critical_path;

pub(crate) use critical_path::{action_locations, build_critical_path};
use critical_path::{action_inner, critical_lanes};

/// Which thread lanes an aggregation draws from, by the role their lane carries.
/// Under Skymeld analysis and execution share one phase marker, so the role
/// comes from the lane class, not the build phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Only the synthetic critical-path lane (tid 0).
    CriticalPath,
    /// Repository-rule fetching — the loading-phase work.
    Loading,
    /// Configured-target/aspect analysis.
    Analysis,
    /// Action execution and its support threads.
    Execution,
    /// Every real lane. Excludes the critical-path lane, whose slices are
    /// redirected copies of worker time; counting both would double-count.
    All,
}

impl Scope {
    /// Whether a thread of this lane class is included in the aggregation.
    pub fn includes(self, lane: LaneClass) -> bool {
        match self {
            Scope::CriticalPath => lane == LaneClass::CriticalPath,
            Scope::Loading => lane == LaneClass::RepoFetch,
            Scope::Analysis => lane == LaneClass::Analysis,
            Scope::Execution => lane == LaneClass::Execution,
            Scope::All => lane != LaneClass::CriticalPath,
        }
    }
}

/// One key segment of a slice's aggregation path: a frame label — a [`Str`]
/// handle into the source profile's text — and the work class it carries (for
/// coloring and class-keyed merging).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Segment {
    label: Str,
    role: SpanRole,
}

/// Lexicographic rank of each distinct scanned label handle: dense `u32`s
/// assigned by one byte-wise sort of the scanned distinct texts. The profile
/// interns each distinct string to one handle, so the handles biject with
/// texts and rank order equals byte-wise text order, making every later
/// ordering comparison integer-only. Ranks are dense per scan and never
/// compare across builds.
struct Ranks {
    rank: FxHashMap<Str, u32>,
}

impl Ranks {
    /// The lexicographic rank of a label handle's text. An unscanned handle
    /// panics — the coverage invariant of [`scan`] is what keeps ranking sound.
    fn rank_of(&self, s: Str) -> u32 {
        match self.rank.get(&s) {
            Some(&r) => r,
            None => panic!("unscanned label handle: the scan must cover every lane a build walks"),
        }
    }
}

/// A map from a mnemonic-bearing slice's name to its mnemonic, over the whole
/// profile. The "action processing" slices carry both the action description as
/// their name and the mnemonic in their args, so this recovers the mnemonic for
/// any other slice that names the same action — notably critical-path
/// components, whose name is `action '<desc>'` and which carry no mnemonic of
/// their own. `by_name` is keyed by the name's [`Str`] handle, so the per-span
/// lookup hashes a small `Copy` key rather than the full string; `by_text`
/// holds the same entries keyed by name text, for the critical-path components
/// whose stripped `action '…'` inner has no handle of its own. Values are
/// mnemonic handles.
struct MnemonicIndex {
    by_name: FxHashMap<Str, Str>,
    by_text: FxHashMap<Box<str>, Str>,
}

/// One worker's share of the name scan.
#[derive(Default)]
struct ScanShard<'p> {
    /// Text → its handle. Each distinct text has one handle profile-wide.
    by_text: FxHashMap<&'p str, Str>,
    /// Distinct name/mnemonic handles seen, to skip the text hash of an
    /// already-noted handle.
    handles: FxHashSet<Str>,
    /// Mnemonic-bearing slice name text → its smallest mnemonic text. A name
    /// carrying several mnemonics resolves to the smallest, so the index is
    /// independent of visit order.
    mnemonics: FxHashMap<&'p str, &'p str>,
}

impl<'p> ScanShard<'p> {
    fn note(&mut self, profile: &'p Profile, s: Str) {
        if self.handles.insert(s) {
            self.by_text.entry(profile.str(s)).or_insert(s);
        }
    }
}

/// Scan slice names and mnemonics into the rank table and the mnemonic index,
/// sharding the lanes across `workers`. Names are noted — entered into the rank
/// table — only on the lanes `note_lane` admits (by thread index), and the scan
/// must admit every lane whose slices a build subsequently walks: an unnoted
/// handle has no rank, and [`Ranks::rank_of`] panics on it. Mnemonic handles
/// and the mnemonic index cover every lane regardless — hashing only the
/// mnemonic-bearing slices, a small fraction of the profile — because a
/// mnemonic-less slice's label can resolve to a mnemonic carried on a lane
/// outside the scan, and that label needs a rank like any other.
fn scan(
    profile: &Profile,
    workers: usize,
    note_lane: impl Fn(usize) -> bool + Sync,
) -> (Ranks, MnemonicIndex) {
    let threads = profile.threads();
    // Each distinct name/mnemonic is noted once, keyed by its text, so a shard
    // holds at most the profile's distinct strings; sharing them roughly evenly
    // over the shards, `distinct / shards` sizes each map so the scan reserves
    // once rather than rehashing every string key through a dozen doublings. A
    // shard that steals more than its share rehashes from this already-large
    // base, a couple of doublings at most.
    let shard_count = workers.clamp(1, threads.len().max(1));
    let per_shard = profile.distinct_strings().div_ceil(shard_count);
    let shards = run_sharded(workers, threads.len(), |shard: &mut ScanShard, i| {
        if shard.by_text.capacity() == 0 {
            shard.by_text.reserve(per_shard);
            shard.handles.reserve(per_shard);
            shard.mnemonics.reserve(per_shard);
        }
        let thread = &threads[i];
        let noted = note_lane(i);
        for (s, d) in profile.slices(thread).iter().zip(profile.details(thread)) {
            if noted {
                shard.note(profile, s.name);
            }
            if let Some(m) = d.mnemonic.get() {
                shard.note(profile, m);
                let mn = profile.str(m);
                shard
                    .mnemonics
                    .entry(profile.str(s.name))
                    .and_modify(|cur| {
                        if mn < *cur {
                            *cur = mn;
                        }
                    })
                    .or_insert(mn);
            }
        }
    });

    let (by_text, mnemonics) = merge_shards(shards);
    (
        build_ranks(&by_text),
        build_index(&by_text, &mnemonics),
    )
}

/// Merge the per-worker scan shards into one text→handle map and one
/// name→smallest-mnemonic map. Seeds from the first shard by move — a
/// single-worker scan merges for free — and reserves for the rest up front, so
/// the merge never regrows (a regrow re-hashes every string key).
fn merge_shards<'p>(
    mut shards: Vec<ScanShard<'p>>,
) -> (FxHashMap<&'p str, Str>, FxHashMap<&'p str, &'p str>) {
    let (mut by_text, mut mnemonics) = {
        let first = &mut shards[0];
        (
            std::mem::take(&mut first.by_text),
            std::mem::take(&mut first.mnemonics),
        )
    };
    by_text.reserve(shards.iter().skip(1).map(|s| s.by_text.len()).sum());
    mnemonics.reserve(shards.iter().skip(1).map(|s| s.mnemonics.len()).sum());
    for shard in shards.iter().skip(1) {
        for (&text, &handle) in &shard.by_text {
            by_text.entry(text).or_insert(handle);
        }
        for (&name, &mn) in &shard.mnemonics {
            mnemonics
                .entry(name)
                .and_modify(|cur| {
                    if mn < *cur {
                        *cur = mn;
                    }
                })
                .or_insert(mn);
        }
    }
    (by_text, mnemonics)
}

/// Rank the distinct scanned texts by one sort under `&str`'s byte-wise `Ord`,
/// so comparing two ranks is exactly comparing the texts.
fn build_ranks<'p>(by_text: &FxHashMap<&'p str, Str>) -> Ranks {
    let mut ordered: Vec<(&'p str, Str)> = by_text.iter().map(|(&t, &h)| (t, h)).collect();
    sort_pairs_by_text(&mut ordered);
    let rank = ordered
        .iter()
        .enumerate()
        .map(|(i, &(_, h))| (h, i as u32))
        .collect();
    Ranks { rank }
}

/// Build the mnemonic index from the merged maps: a text-keyed table over every
/// scanned name, and a handle-keyed table restricted to names with a noted
/// handle. A name seen only on unadmitted lanes has no noted handle and needs
/// no handle-keyed entry: only a walked — hence noted — slice's name is ever
/// looked up by handle, while text-keyed lookups cover every name.
fn build_index(
    by_text: &FxHashMap<&str, Str>,
    mnemonics: &FxHashMap<&str, &str>,
) -> MnemonicIndex {
    let name_to_mnemonic: Vec<(&str, Str)> =
        mnemonics.iter().map(|(&n, &m)| (n, by_text[m])).collect();
    let by_name = name_to_mnemonic
        .iter()
        .filter_map(|&(n, m)| by_text.get(n).map(|&h| (h, m)))
        .collect();
    let by_text_idx = name_to_mnemonic
        .into_iter()
        .map(|(n, m)| (Box::from(n), m))
        .collect();
    MnemonicIndex {
        by_name,
        by_text: by_text_idx,
    }
}

/// Run `task` over the indexes `0..n`, each worker folding into its own `S`;
/// returns the per-worker shards. Workers steal indexes one at a time through a
/// shared cursor, since per-index work is heavily skewed (a few big evaluator
/// lanes). A single worker runs inline.
fn run_sharded<S: Default + Send>(
    workers: usize,
    n: usize,
    task: impl Fn(&mut S, usize) + Sync,
) -> Vec<S> {
    let workers = workers.clamp(1, n.max(1));
    let next = AtomicUsize::new(0);
    let run_one = || {
        let mut shard = S::default();
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            if i >= n {
                break;
            }
            task(&mut shard, i);
        }
        shard
    };
    if workers == 1 {
        return vec![run_one()];
    }
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers).map(|_| scope.spawn(&run_one)).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    })
}

/// Sort `(text, handle)` pairs into byte-wise text order. The texts are
/// distinct, so the result is the one total order. Small inputs sort serially —
/// the parallel split only pays off at scale.
fn sort_pairs_by_text(pairs: &mut [(&str, Str)]) {
    /// Input size below which the serial sort wins over the parallel split.
    const PARALLEL_MIN: usize = 1 << 13;
    if pairs.len() < PARALLEL_MIN {
        pairs.sort_unstable_by_key(|&(t, _)| t);
    } else {
        pairs.par_sort_unstable_by_key(|&(t, _)| t);
    }
}

/// One node of the aggregated flame graph: a frame merged across every span that
/// shares this label at the same place in the time-containment tree. `total_us`
/// is the summed duration of the spans merged here (each counted once, at its own
/// node); `self_us` is that minus the children's totals (`max(0, …)`); `count` is
/// how many spans merged at this node. `width_us` is the layout width
/// `max(total_us, Σ children.width_us)` — a time-contained child may overflow its
/// parent's end (Bazel's catch-all frames), so laying out by `width_us` keeps
/// children inside their parent. `children` is a contiguous range into the
/// owning [`FlameGraph`]'s node array, sorted widest-first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlameNode {
    pub name: Str,
    /// Lexicographic rank of `name`'s text among the distinct names scanned
    /// for this graph's build — rank order equals byte-wise text order — so
    /// name ordering compares integers without resolving text. Ranks are dense
    /// per build and only order names within one graph, never across graphs.
    pub rank: u32,
    pub total_us: i64,
    pub self_us: i64,
    pub width_us: i64,
    pub role: SpanRole,
    pub count: u32,
    pub depth: u32,
    /// The deepest node depth in this node's subtree (itself included). Layout
    /// keeps, among sub-pixel siblings sharing a pixel column, the one reaching
    /// deepest, so the depth drawn at a column is stable as the zoom shifts which
    /// siblings fall in it.
    pub subtree_max_depth: u32,
    pub children: Span32,
}

/// An aggregated flame graph as a flat arena: node 0 is the synthetic root,
/// every other node is reached through a parent's `children` range, and all node
/// names are [`Str`] handles into the text of the [`Profile`] the graph holds —
/// every label is already a profile string, so the graph carries no text of its
/// own. Holding the profile by `Arc` keeps the graph free of lifetime
/// parameters, so it is built off the UI thread and handed back across threads.
#[derive(Debug, Clone, PartialEq)]
pub struct FlameGraph {
    profile: Arc<Profile>,
    nodes: Vec<FlameNode>,
    /// Deepest node depth, found once at build so [`flame_height`] need not scan
    /// every node on each paint.
    max_depth: u32,
}

/// The synthetic root's name. Its label ("All") is the one frame name not in the
/// profile, so it is encoded as the offset-0 handle: the profile's interner
/// reserves text offset 0 (the `OptStr` absence encoding), so no profile string
/// ever carries it and the sentinel cannot collide with a real name.
const ROOT_LABEL: Str = Str::new(0, 0);

impl FlameGraph {
    /// The synthetic root frame (node 0), under which every lane's forest merges.
    pub fn root(&self) -> &FlameNode {
        &self.nodes[0]
    }

    pub fn nodes(&self) -> &[FlameNode] {
        &self.nodes
    }

    /// The direct children of a node, in widest-first order.
    pub fn children(&self, node: &FlameNode) -> &[FlameNode] {
        &self.nodes[node.children.range()]
    }

    /// Resolves a node name (or a [`StatRow`](crate::StatRow) key) to its text,
    /// through the profile the graph was built from.
    pub fn str(&self, s: Str) -> &str {
        if s == ROOT_LABEL {
            return "All";
        }
        self.profile.str(s)
    }
}

/// Vertical sizing of the flame graph, in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlameMetrics {
    pub row_height: f32,
}

/// A flame-graph node laid out for painting. The field copies identify the node
/// for hit-testing and tooltips; `name` is resolved with the producing
/// [`FlameGraph`]'s [`str`](FlameGraph::str).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlameBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub depth: u32,
    pub name: Str,
    pub total_us: i64,
    pub self_us: i64,
    pub role: SpanRole,
    pub count: u32,
}

/// Child count above which a node's child index switches from a linear scan of
/// the child list to a hashmap. Most nodes gather only a handful of children and
/// stay under it, paying no map allocation; the rare wide frames (a mnemonic
/// gathering thousands of children) promote and keep O(1) lookup. Set just above
/// the width where a scan per insert starts to lose to a hash.
const CHILD_INDEX_LINEAR_MAX: usize = 16;

/// A node's `(label, role)` → child-index accelerator. Below
/// [`CHILD_INDEX_LINEAR_MAX`] children it is [`Linear`](ChildIndex::Linear) — the
/// lookup scans `children`, and the node holds no map — so most nodes carry no
/// hash table at all. A frame that gathers more promotes to
/// [`Map`](ChildIndex::Map) for O(1) lookup.
enum ChildIndex {
    /// Scan `children` for the `(name, role)` key.
    Linear,
    /// `(name, role)` → index into `children`.
    Map(FxHashMap<(Str, SpanRole), u32>),
}

/// A node still under construction. Children are kept in insertion order in
/// `children` and located by their `(label, role)` merge key through `index`:
/// labels are per-mnemonic, so a single frame can gather thousands of distinct
/// children — past [`CHILD_INDEX_LINEAR_MAX`] the index promotes from a linear
/// scan to a hashmap so wide frames keep O(1) lookup instead of a quadratic scan.
/// Labels are interned profile handles, so the key is a `Copy` `(Str, SpanRole)`
/// — small and fixed-size, which FxHash hashes in a few cycles where SipHash
/// would dominate the per-span lookup. Sorting for [`flatten`] reorders
/// `children` without updating `index`, so no merge may follow a sort.
pub(crate) struct Builder {
    name: Str,
    /// Lexicographic rank of `name`'s text (see [`FlameNode::rank`]); the
    /// root's is unused — a root is never ordered against a sibling.
    rank: u32,
    role: SpanRole,
    total_us: i64,
    count: u32,
    children: Vec<Builder>,
    index: ChildIndex,
}

impl Builder {
    pub(crate) fn new_root() -> Self {
        Builder::new(
            ROOT_LABEL,
            0,
            SpanRole { workload: Workload::Other, payload: false },
        )
    }

    fn new(name: Str, rank: u32, role: SpanRole) -> Self {
        Builder {
            name,
            rank,
            role,
            total_us: 0,
            count: 0,
            children: Vec::new(),
            index: ChildIndex::Linear,
        }
    }

    /// The index of the child matching `key`, or `None` if none is present.
    fn find_child(&self, key: (Str, SpanRole)) -> Option<usize> {
        match &self.index {
            ChildIndex::Linear => self
                .children
                .iter()
                .position(|c| (c.name, c.role) == key),
            ChildIndex::Map(m) => m.get(&key).map(|&i| i as usize),
        }
    }

    /// Record the child just pushed at `idx` under `key`. When still linear,
    /// crossing [`CHILD_INDEX_LINEAR_MAX`] promotes to a map built from every
    /// current child (the just-pushed one included); below that the linear scan
    /// keeps finding it, so nothing is recorded.
    fn index_child(&mut self, idx: usize, key: (Str, SpanRole)) {
        match &mut self.index {
            ChildIndex::Map(m) => {
                m.insert(key, idx as u32);
            }
            ChildIndex::Linear => {
                if self.children.len() > CHILD_INDEX_LINEAR_MAX {
                    let mut m = FxHashMap::default();
                    m.reserve(self.children.len());
                    for (i, c) in self.children.iter().enumerate() {
                        m.insert((c.name, c.role), i as u32);
                    }
                    self.index = ChildIndex::Map(m);
                }
            }
        }
    }

    /// Find the child matching this segment, or append a fresh one built with
    /// `rank()`. The span role travels with the label; the first occurrence
    /// wins. Ranks are lazy so a merge into an existing child costs no lookup.
    fn child(&mut self, seg: Segment, rank: impl FnOnce() -> u32) -> &mut Builder {
        let key = (seg.label, seg.role);
        let idx = match self.find_child(key) {
            Some(i) => i,
            None => {
                let i = self.children.len();
                self.children
                    .push(Builder::new(seg.label, rank(), seg.role));
                self.index_child(i, key);
                i
            }
        };
        &mut self.children[idx]
    }
}

impl Default for Builder {
    fn default() -> Self {
        Builder::new_root()
    }
}

/// Merges spans into the builder tree. Every frame label is an interned handle
/// of the source profile — a span's mnemonic and name already live in its text.
/// The profile interns each distinct string to one handle, so equal text ⇒
/// equal handle: frames key by handle directly, and merge keys compare and hash
/// as plain integers with no per-span string work. Read-only once built, so
/// merge workers share one.
pub(crate) struct Flamer {
    profile: Arc<Profile>,
    idx: MnemonicIndex,
    ranks: Ranks,
}

impl Flamer {
    /// A flamer whose name scan covers every lane, for a build that may walk
    /// any of them.
    pub(crate) fn new(profile: &Arc<Profile>, workers: usize) -> Self {
        Flamer::scoped(profile, workers, |_| true)
    }

    /// A flamer whose name scan covers only the lanes `note_lane` admits —
    /// which must include every lane the build walks (see [`scan`]). The scan
    /// is the dominant cost of a single-scope build, so a build that walks few
    /// lanes scans only those.
    pub(crate) fn scoped(
        profile: &Arc<Profile>,
        workers: usize,
        note_lane: impl Fn(usize) -> bool + Sync,
    ) -> Self {
        let (ranks, idx) = scan(profile, workers, note_lane);
        Flamer {
            profile: Arc::clone(profile),
            idx,
            ranks,
        }
    }

    /// The label a span carries: its action mnemonic — from its own args if
    /// present, else recovered from the name index by the slice's name or, for
    /// a critical-path component, by its unwrapped `action '…'` description —
    /// so all Rustc collapse together; else its own name (the informative label
    /// for non-action slices — "checkOutputs", "subprocess.run", a repo
    /// target, …). The tree shape comes from time-containment, not from this
    /// label; the label only decides which spans at the same place merge into
    /// one frame. Only the critical-path unwrapping consults the text-keyed
    /// index, so the hot per-span path never hashes a full string.
    fn span_label(&self, slice: &Slice, detail: &SliceDetail) -> Segment {
        let label = match detail.mnemonic.get() {
            Some(m) => m,
            None => {
                let name = slice.name;
                match self.idx.by_name.get(&name) {
                    Some(&m) => m,
                    None => action_inner(self.profile.str(slice.name))
                        .and_then(|inner| self.idx.by_text.get(inner).copied())
                        .unwrap_or(name),
                }
            }
        };
        Segment {
            label,
            role: slice.role,
        }
    }

    /// Merge span `span_idx` of `lane_slices` and its time-children into `parent`:
    /// the span's duration lands once at its own labelled node, then each child
    /// recurses under that node. So a node merging N spans sums their durations,
    /// and a child's duration is counted at the child, not again at the parent.
    /// `lane_details` is the lane's parallel detail slice.
    fn merge_span(
        &self,
        parent: &mut Builder,
        lane_slices: &[Slice],
        lane_details: &[SliceDetail],
        forest: &Forest,
        span_idx: usize,
    ) {
        let seg = self.span_label(&lane_slices[span_idx], &lane_details[span_idx]);
        let node = parent.child(seg, || self.ranks.rank_of(seg.label));
        node.total_us += lane_slices[span_idx].dur_us;
        node.count += 1;
        // Reborrow `node` per child: `child()` borrows `parent` mutably, so the
        // recursion can't hold a `&mut node` across calls; locate it by key each
        // time, which the index makes O(1).
        let key = (seg.label, seg.role);
        for ci in 0..forest.children[span_idx].len() {
            let c = forest.children[span_idx][ci];
            let node = {
                let i = parent.find_child(key).expect("child was just inserted");
                &mut parent.children[i]
            };
            self.merge_span(node, lane_slices, lane_details, forest, c);
        }
    }
}

/// Merge one lane's containment forest into `root`: each top-level span's
/// duration adds to the root total, then merges in as a frame.
fn merge_lane(root: &mut Builder, profile: &Profile, flamer: &Flamer, thread: &ThreadTrack) {
    let slices = profile.slices(thread);
    let details = profile.details(thread);
    let forest = containment_forest(slices);
    for ri in 0..forest.roots.len() {
        let r = forest.roots[ri];
        root.total_us += slices[r].dur_us;
        flamer.merge_span(root, slices, details, &forest, r);
    }
}

/// End time of a slice.
fn end_us(slice: &Slice) -> i64 {
    slice.start_us + slice.dur_us
}

/// The time-containment forest of a lane's slices. `children[i]` lists the
/// slices directly nested in slice `i`; `roots` lists the top-level slices (no
/// container). Slices must be in lane order — start ascending, ties
/// longest-first, the order [`Profile::slices`] guarantees — so a depth-stack
/// walk visits every container before its contents: a slice's parent is the
/// nearest ENCLOSING span, so pop while the stack top doesn't enclose the slice
/// (`top.end < slice.end`). Popping on end-before-start alone would wrongly nest
/// spans that merely overlap — one that starts inside another but ends after it
/// is a sibling, not a child — which otherwise builds a runaway staircase.
struct Forest {
    children: Vec<Vec<usize>>,
    roots: Vec<usize>,
}

fn containment_forest(slices: &[Slice]) -> Forest {
    debug_assert!(
        slices.windows(2).all(|w| {
            w[0].start_us < w[1].start_us
                || (w[0].start_us == w[1].start_us && w[0].dur_us >= w[1].dur_us)
        }),
        "slices must be in lane order: start ascending, ties longest-first"
    );
    let mut children = vec![Vec::new(); slices.len()];
    // Each slice is a root or nested under one parent, never both, so the roots
    // number at most the lane's slices; a flat lane reaches that bound.
    let mut roots = Vec::with_capacity(slices.len());
    let mut stack: Vec<usize> = Vec::new();
    for i in 0..slices.len() {
        let slice_end = end_us(&slices[i]);
        while stack
            .last()
            .is_some_and(|&top| end_us(&slices[top]) < slice_end)
        {
            stack.pop();
        }
        match stack.last() {
            Some(&parent) => children[parent].push(i),
            None => roots.push(i),
        }
        stack.push(i);
    }
    Forest { children, roots }
}

/// Build the aggregated flame graph for a profile by time-containment plus
/// semantic merge: within each in-scope lane every span nests under the span that
/// time-contains it, and identically labelled spans at the same place merge
/// into one frame whose duration is the sum of the merged spans' own durations.
/// A parent span's Bazel duration already includes its children's, so counting
/// each span once at its own node avoids the double-count of summing a span and
/// its sub-spans as siblings.
///
/// For [`Scope::CriticalPath`] the critical-path lane holds only bare components
/// with no sub-spans, so each component is associated with its originating action
/// (by name) and that action's whole worker-lane subtree is grafted under the
/// root, giving the critical action its internal breakdown.
///
/// The returned [`FlameGraph`] keeps a clone of `profile`, whose interned text
/// its node names index, so it is `Send` with no lifetime parameters.
///
/// The name scan — the dominant cost — covers only the lanes the scope's build
/// walks: [`Scope::All`] scans the whole profile, a class scope its own lanes,
/// and the critical path the lanes [`critical_lanes`] selects.
///
/// [`FlameScopes`](crate::FlameScopes) derives every scope from one aggregation
/// pass; this composes the same pieces for a single scope.
pub fn build_flame_graph(profile: &Arc<Profile>, scope: Scope) -> FlameGraph {
    let workers = bviz_trace_profile::worker_count();
    let mut root = if scope == Scope::CriticalPath {
        // Locate the actions before the scan: the index hashes only the
        // mnemonic-bearing slices, so it is cheap, and it bounds the lanes the
        // graft walks — and hence the lanes the scan must cover.
        let actions = action_locations(profile);
        let lanes = critical_lanes(profile, &actions);
        let flamer = Flamer::scoped(profile, workers, |i| lanes.contains(&i));
        let mut root = Builder::new_root();
        build_critical_path(profile, &flamer, &actions, &mut root);
        root
    } else {
        let threads = profile.threads();
        let flamer = match scope {
            Scope::All => Flamer::new(profile, workers),
            _ => Flamer::scoped(profile, workers, |i| scope.includes(threads[i].lane_class)),
        };
        fold_all(
            class_trees(profile, &flamer, |class| scope.includes(class), workers)
                .into_iter()
                .map(|(_, tree)| tree)
                .collect(),
        )
    };
    seal(&mut root, profile)
}

/// A whole-profile name scan (rank table + mnemonic index) held so a
/// track-selection flame graph can be rebuilt for any lane subset without
/// rescanning. The scan covers every lane, so it ranks and labels any
/// selection the caller later chooses.
pub struct TrackFlamer {
    flamer: Flamer,
}

impl TrackFlamer {
    pub fn new(profile: &Arc<Profile>) -> TrackFlamer {
        TrackFlamer {
            flamer: Flamer::new(profile, bviz_trace_profile::worker_count()),
        }
    }

    pub(crate) fn build(&self, selected: impl Fn(usize) -> bool + Sync) -> FlameGraph {
        build_flame_graph_for_tracks(&self.flamer, selected)
    }
}

/// Build the aggregated flame graph over exactly the thread tracks `selected`
/// admits (by index into [`Profile::threads`]), by the same time-containment
/// plus semantic merge as [`build_flame_graph`]: within each selected lane every
/// span nests under the span that time-contains it, and identically labelled
/// spans at the same place merge across the selected lanes into one frame.
///
/// `flamer`'s name scan must cover every selected lane, so the rank-coverage
/// invariant [`scan`] documents holds; a whole-profile scan (see [`TrackFlamer`])
/// satisfies any selection. A critical-path lane holds redirected copies of
/// worker time, so selecting it alongside the worker lanes it copies sums both;
/// membership is the caller's to choose.
pub(crate) fn build_flame_graph_for_tracks(
    flamer: &Flamer,
    selected: impl Fn(usize) -> bool + Sync,
) -> FlameGraph {
    let workers = bviz_trace_profile::worker_count();
    let threads = flamer.profile.threads();
    // Each worker folds its stolen lanes into one root; the roots then fold
    // together. Lane order within a worker can't change the sealed graph, since
    // the flatten sort is total.
    let shards = run_sharded(workers, threads.len(), |root: &mut Builder, i| {
        if !selected(i) {
            return;
        }
        let thread = &threads[i];
        merge_lane(root, &flamer.profile, flamer, thread);
    });
    let mut root = fold_all(shards);
    seal(&mut root, &flamer.profile)
}

/// A mutable reference to `class`'s value in a class-keyed list, inserting
/// `make()` at its slot ([`LaneClass`] declaration order) when absent.
fn class_entry<T>(
    entries: &mut Vec<(LaneClass, T)>,
    class: LaneClass,
    make: impl FnOnce() -> T,
) -> &mut T {
    match entries.iter().position(|(c, _)| *c == class) {
        Some(i) => &mut entries[i].1,
        None => {
            let at = entries.partition_point(|(c, _)| (*c as usize) < class as usize);
            entries.insert(at, (class, make()));
            &mut entries[at].1
        }
    }
}

/// One aggregation tree per lane class over the lanes `include` admits, keyed in
/// [`LaneClass`] declaration order; classes with no admitted lane get no entry.
/// Each lane's containment forest merges into its class's tree, so any scope —
/// a single class or a union — derives from these without re-walking a lane.
/// The critical-path lane is never walked: its slices are redirected copies of
/// worker time, aggregated only by the [`build_critical_path`] graft.
///
/// The lanes shard across `workers` into per-worker partial trees; which lanes
/// a worker steals cannot change the result, because the partial-tree fold sums
/// and the flatten sort is total.
pub(crate) fn class_trees(
    profile: &Profile,
    flamer: &Flamer,
    include: impl Fn(LaneClass) -> bool + Sync,
    workers: usize,
) -> Vec<(LaneClass, Builder)> {
    let threads = profile.threads();
    let shards: Vec<Vec<(LaneClass, Builder)>> = run_sharded(workers, threads.len(), |trees, i| {
        let thread = &threads[i];
        let class = thread.lane_class;
        if class == LaneClass::CriticalPath || !include(class) {
            return;
        }
        let root = class_entry(trees, class, Builder::new_root);
        merge_lane(root, profile, flamer, thread);
    });

    fold_buckets(bucket_by_class(shards), workers)
}

/// Regroup the per-worker shards' partial trees by lane class, keyed in
/// [`LaneClass`] declaration order.
fn bucket_by_class(shards: Vec<Vec<(LaneClass, Builder)>>) -> Vec<(LaneClass, Vec<Builder>)> {
    let mut buckets: Vec<(LaneClass, Vec<Builder>)> = Vec::new();
    for shard in shards {
        for (class, tree) in shard {
            class_entry(&mut buckets, class, Vec::new).push(tree);
        }
    }
    buckets
}

/// Fold each class's partial trees into one tree per class. The classes merge
/// independently of one another, so with more than one class and worker they
/// fold on a thread apiece.
fn fold_buckets(
    buckets: Vec<(LaneClass, Vec<Builder>)>,
    workers: usize,
) -> Vec<(LaneClass, Builder)> {
    if buckets.len() > 1 && workers > 1 {
        std::thread::scope(|scope| {
            let handles: Vec<_> = buckets
                .into_iter()
                .map(|(class, parts)| scope.spawn(move || (class, fold_all(parts))))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        })
    } else {
        buckets
            .into_iter()
            .map(|(class, parts)| (class, fold_all(parts)))
            .collect()
    }
}

/// Merge `src`'s subtree into `dst`, matching children by the `(label, work
/// class)` merge key and summing totals and counts; a subtree `dst` has no
/// match for is moved over wholesale.
pub(crate) fn fold_into(dst: &mut Builder, src: Builder) {
    dst.total_us += src.total_us;
    dst.count += src.count;
    for child in src.children {
        let key = (child.name, child.role);
        match dst.find_child(key) {
            Some(i) => fold_into(&mut dst.children[i], child),
            None => {
                let i = dst.children.len();
                dst.children.push(child);
                dst.index_child(i, key);
            }
        }
    }
}

/// Fold a set of trees into one, in the given order; empty input yields a bare
/// root.
pub(crate) fn fold_all(trees: Vec<Builder>) -> Builder {
    let mut iter = trees.into_iter();
    let mut acc = iter.next().unwrap_or_else(Builder::new_root);
    for tree in iter {
        fold_into(&mut acc, tree);
    }
    acc
}

/// Sort and freeze a finished builder tree into a [`FlameGraph`] over `profile`.
pub(crate) fn seal(root: &mut Builder, profile: &Arc<Profile>) -> FlameGraph {
    let nodes = flatten(root);
    let max_depth = nodes.iter().map(|n| n.depth).max().unwrap_or(0);
    FlameGraph {
        profile: Arc::clone(profile),
        nodes,
        max_depth,
    }
}

/// The layout width of a builder subtree: the larger of its own merged total and
/// the sum of its children's widths, so a time-overflowing child stays inside the
/// width allotted to its parent.
fn width_us(builder: &Builder) -> i64 {
    let children_width: i64 = builder.children.iter().map(width_us).sum();
    builder.total_us.max(children_width)
}

/// Freeze a builder into a flame node at `depth`, leaving its `children` range
/// blank for the caller to fill once the children are laid down. Self time is the
/// own total minus the children's totals, floored at zero since an overflowing
/// child can exceed the parent's total.
fn freeze(builder: &Builder, depth: u32) -> FlameNode {
    let children_total: i64 = builder.children.iter().map(|c| c.total_us).sum();
    let children_width: i64 = builder.children.iter().map(width_us).sum();
    FlameNode {
        name: builder.name,
        rank: builder.rank,
        total_us: builder.total_us,
        self_us: (builder.total_us - children_total).max(0),
        width_us: builder.total_us.max(children_width),
        role: builder.role,
        count: builder.count,
        depth,
        // Filled by a bottom-up pass once the whole tree is flattened.
        subtree_max_depth: depth,
        children: Span32::default(),
    }
}

/// Flatten the builder forest into a flat node array with node 0 the root. A
/// breadth-first walk lays each node's direct children down as one contiguous
/// block, recorded as the parent's `children` range; the children are sorted
/// widest-first, breaking ties by name — via the precomputed rank, whose order
/// equals text order — so the layout is independent of the non-deterministic
/// input order. Sorts the tree in place, leaving its merge indexes stale.
fn flatten(root: &mut Builder) -> Vec<FlameNode> {
    // Returns the node count of the subtree (itself plus its descendants), so
    // the flat array reserves exactly once instead of doubling through millions
    // of nodes.
    fn sort_tree(builder: &mut Builder) -> usize {
        builder
            .children
            // Same-text children carry the same handle, so they differ only by
            // span role — the role tie-break makes the order total and hence
            // independent of merge order.
            .sort_unstable_by_key(|c| (std::cmp::Reverse(c.total_us), c.rank, c.role));
        let mut count = 1;
        for c in &mut builder.children {
            count += sort_tree(c);
        }
        count
    }
    let node_count = sort_tree(root);

    let mut nodes = Vec::with_capacity(node_count);
    nodes.push(freeze(root, 0));
    let mut queue: std::collections::VecDeque<(&Builder, usize, u32)> =
        std::collections::VecDeque::new();
    queue.push_back((root, 0, 0));
    while let Some((builder, idx, depth)) = queue.pop_front() {
        let off = nodes.len() as u32;
        for c in &builder.children {
            let child_idx = nodes.len();
            nodes.push(freeze(c, depth + 1));
            queue.push_back((c, child_idx, depth + 1));
        }
        nodes[idx].children = Span32::new(off, nodes.len() as u32 - off);
    }
    set_subtree_max_depth(&mut nodes);
    nodes
}

/// Fills each node's `subtree_max_depth` with the deepest `depth` reached in its
/// subtree (itself or any descendant). A child block always follows its parent,
/// so a reverse pass sees every child's value finalized before its parent's.
fn set_subtree_max_depth(nodes: &mut [FlameNode]) {
    for i in (0..nodes.len()).rev() {
        let mut m = nodes[i].depth;
        for c in nodes[i].children.range() {
            m = m.max(nodes[c].subtree_max_depth);
        }
        nodes[i].subtree_max_depth = m;
    }
}

/// Lay the visible window of the graph into pixels, mirroring the flame chart's
/// [`visible_slices`](crate::visible_slices). The graph occupies `content_w`
/// pixels at the current zoom; `scroll_x` is the content pixel at the left edge
/// of a `view_w`-wide viewport. Each node's children are placed left-to-right
/// with width proportional to `width_us` (the right-hand remainder being the
/// parent's self time), so dividing by `width_us` keeps an overflowing child
/// inside its parent. Depth maps to y via `row_height`.
///
/// Returned box positions are **viewport-relative** (x `0` is the left edge),
/// and only boxes overlapping the viewport are laid out — an off-screen subtree
/// is pruned — so the layout is local to what is on screen, not the whole graph.
///
/// A box narrower than `min_px` is widened to a one-pixel marker so it stays
/// visible. Where several sub-pixel siblings fall in one pixel column, only the
/// one reaching the **deepest** is kept (its subtree drawn, the rest dropped):
/// keeping the leftmost instead would let its shallow subtree hide a deeper
/// sibling's, so the depth drawn at a column would jump as the zoom shifted
/// which sibling was leftmost. Keeping the deepest makes a column's depth stable.
/// A box wide enough to render on its own is always kept. Coordinates are carried
/// in `f64` so the proportional division holds precision at deep zoom, and each
/// box is clamped to a viewport-sized margin so no huge value reaches its `f32`
/// fields. Boxes come back in paint order, parents before children.
pub fn lay_out_flame(graph: &FlameGraph, content_w: f64, placement: FlamePlacement) -> Vec<FlameBox> {
    let mut out = Vec::new();
    place(graph, graph.root(), 0.0, content_w, placement, &mut out);
    out
}

/// Horizontal scroll offset, viewport width, row sizing, and minimum box width
/// — the parameters that place the flame graph's visible window into pixels.
#[derive(Debug, Clone, Copy)]
pub struct FlamePlacement<'a> {
    pub scroll_x: f64,
    pub view_w: f32,
    pub metrics: &'a FlameMetrics,
    pub min_px: f32,
}

fn place(
    graph: &FlameGraph,
    node: &FlameNode,
    cx: f64,
    cw: f64,
    placement: FlamePlacement,
    out: &mut Vec<FlameBox>,
) {
    let FlamePlacement { scroll_x, view_w, metrics, min_px } = placement;
    let min = min_px as f64;
    // The node's left edge and widened right edge in viewport pixels.
    let vx = cx - scroll_x;
    let right = vx + cw.max(min);
    // Prune a subtree that lies entirely off either edge; a sub-pixel node's
    // widened edge is what must reach the viewport, so `right` uses `min`.
    if right <= 0.0 || vx >= view_w as f64 {
        return;
    }
    // Clamp to a viewport-sized margin so a wide box spanning far off-screen
    // never stores a huge coordinate; a visible marker's `vx` is already small.
    let px = vx.max(-(view_w as f64));
    let pr = right.min(2.0 * view_w as f64);
    out.push(box_of(node, px as f32, (pr - px) as f32, metrics));
    if node.width_us == 0 {
        return;
    }
    // Place children left-to-right. A child wide enough to render recurses on its
    // own; a run of sub-pixel children sharing one pixel column collapses to a
    // single kept sibling (see `deepest_in_column`).
    let children = graph.children(node);
    let child_w = |c: &FlameNode| cw * c.width_us as f64 / node.width_us as f64;
    let mut cursor = cx;
    let mut i = 0;
    while i < children.len() {
        let ccw = child_w(&children[i]);
        if ccw >= min {
            place(graph, &children[i], cursor, ccw, placement, out);
            cursor += ccw;
            i += 1;
            continue;
        }
        let (best, best_cx, best_cw, run_cursor, next) =
            deepest_in_column(children, i, ccw, cursor, placement, &child_w);
        place(graph, best, best_cx, best_cw, placement, out);
        cursor = run_cursor;
        i = next;
    }
}

/// Within a run of consecutive sub-pixel siblings sharing one pixel column, keep
/// the one whose subtree reaches deepest — see `lay_out_flame` for why the
/// deepest, not the leftmost, must win. `start`/`start_cw` are the first sibling
/// and its width; `cursor` is its left edge. Returns the chosen child with its
/// left edge and width, the cursor advanced past the whole run, and the index of
/// the first sibling beyond it.
fn deepest_in_column<'a>(
    children: &'a [FlameNode],
    start: usize,
    start_cw: f64,
    cursor: f64,
    placement: FlamePlacement,
    child_w: &impl Fn(&FlameNode) -> f64,
) -> (&'a FlameNode, f64, f64, f64, usize) {
    let scroll_x = placement.scroll_x;
    let min = placement.min_px as f64;
    let col = (cursor - scroll_x).floor() as i32;
    let (mut best, mut best_cx, mut best_cw) = (&children[start], cursor, start_cw);
    let mut run_cursor = cursor + start_cw;
    let mut j = start + 1;
    while j < children.len() {
        let cjw = child_w(&children[j]);
        if cjw >= min || ((run_cursor - scroll_x).floor() as i32) != col {
            break;
        }
        if children[j].subtree_max_depth > best.subtree_max_depth {
            (best, best_cx, best_cw) = (&children[j], run_cursor, cjw);
        }
        run_cursor += cjw;
        j += 1;
    }
    (best, best_cx, best_cw, run_cursor, j)
}

fn box_of(node: &FlameNode, x: f32, w: f32, metrics: &FlameMetrics) -> FlameBox {
    FlameBox {
        x,
        y: node.depth as f32 * metrics.row_height,
        w,
        h: metrics.row_height,
        depth: node.depth,
        name: node.name,
        total_us: node.total_us,
        self_us: node.self_us,
        role: node.role,
        count: node.count,
    }
}

/// Total painted height: one row per depth level the graph reaches.
pub fn flame_height(graph: &FlameGraph, metrics: &FlameMetrics) -> f32 {
    (graph.max_depth + 1) as f32 * metrics.row_height
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_profile::{action, action_named, profile, slice, track};

    fn child<'a>(graph: &'a FlameGraph, node: &FlameNode, name: &str) -> &'a FlameNode {
        graph
            .children(node)
            .iter()
            .find(|c| graph.str(c.name) == name)
            .unwrap_or_else(|| panic!("no child {name}"))
    }

    const METRICS: FlameMetrics = FlameMetrics { row_height: 10.0 };

    // The graph is built off the UI thread and handed back, so it must cross
    // threads with no borrow of the source profile.
    fn _assert_thread_safe<T: Send + Sync + 'static>() {}
    #[test]
    fn flame_types_are_send_sync_and_lifetime_free() {
        _assert_thread_safe::<FlameGraph>();
        _assert_thread_safe::<FlameNode>();
        _assert_thread_safe::<FlameBox>();
        _assert_thread_safe::<TrackFlamer>();
        _assert_thread_safe::<crate::StatRow>();
    }

    #[test]
    fn parallel_text_sort_matches_serial() {
        // Enough pairs to cross the parallel threshold, with heavy shared
        // prefixes, case splits, and non-ASCII segments, so the parallel sort is
        // exercised on clustered keys.
        let texts: Vec<String> = (0..20_000)
            .map(|i| format!("prefix/{}/x{:05}", ["a", "A", "é", ""][i % 4], i))
            .collect();
        let pairs: Vec<(&str, Str)> = texts
            .iter()
            .enumerate()
            .map(|(i, t)| (t.as_str(), Str::new(i as u32 + 1, t.len() as u32)))
            .collect();
        let serial = {
            let mut p = pairs.clone();
            p.sort_unstable_by_key(|&(t, _)| t);
            p
        };
        let mut parallel = pairs;
        sort_pairs_by_text(&mut parallel);
        assert_eq!(parallel, serial);
    }

    #[test]
    fn conflicting_mnemonics_resolve_to_the_smallest_text() {
        // Two lanes carry the same action name under different mnemonics; a
        // third slice shares the name but has no mnemonic of its own, so its
        // label comes from the index — which must pick the byte-wise smallest
        // mnemonic regardless of lane visit order.
        let p = profile(vec![
            track(
                1,
                LaneClass::Execution,
                vec![action_named("Compiling foo", "Rustc", 0, 100)],
            ),
            track(
                2,
                LaneClass::Execution,
                vec![action_named("Compiling foo", "Javac", 200, 50)],
            ),
            track(
                3,
                LaneClass::Execution,
                vec![slice("Compiling foo", "action processing", 400, 30)],
            ),
        ]);
        let g = build_flame_graph(&p, Scope::All);
        // A slice's own mnemonic always wins; only the mnemonic-less slice
        // resolves through the index and joins the smallest-text frame.
        assert_eq!(child(&g, g.root(), "Rustc").total_us, 100);
        let javac = child(&g, g.root(), "Javac");
        assert_eq!(javac.total_us, 80);
        assert_eq!(javac.count, 2);
        assert!(
            g.children(g.root())
                .iter()
                .all(|c| g.str(c.name) != "Compiling foo")
        );
    }

    #[test]
    fn nested_spans_form_a_call_stack_without_double_counting() {
        // A lane with Rustc(0..100) containing Action.execute(0..90) containing
        // subprocess.run(0..80): the tree nests by time, each span's duration
        // counted once at its own node. Root total is the action's wall time
        // (100), NOT the inflated 270 that summing siblings would give.
        let p = profile(vec![track(
            8,
            LaneClass::Execution,
            vec![
                action("Rustc", 0, 100),
                slice("Action.execute", "general information", 0, 90),
                slice("subprocess.run", "subprocess run", 0, 80),
            ],
        )]);
        let g = build_flame_graph(&p, Scope::All);
        assert_eq!(g.root().total_us, 100);
        let rustc = child(&g, g.root(), "Rustc");
        assert_eq!(rustc.total_us, 100);
        assert_eq!(rustc.self_us, 10);
        let exec = child(&g, rustc, "Action.execute");
        assert_eq!(exec.total_us, 90);
        let sub = child(&g, exec, "subprocess.run");
        assert_eq!(sub.total_us, 80);
        assert_eq!(sub.self_us, 80);
    }

    #[test]
    fn identical_spans_merge_across_lanes() {
        // Two Rustc actions on two execution lanes, each containing its own
        // Action.execute, merge into one Rustc frame whose Action.execute child
        // sums both.
        let lane = |start| {
            vec![
                action("Rustc", start, 100),
                slice("Action.execute", "general information", start, 80),
            ]
        };
        let p = profile(vec![
            track(8, LaneClass::Execution, lane(0)),
            track(9, LaneClass::Execution, lane(0)),
        ]);
        let g = build_flame_graph(&p, Scope::All);
        assert_eq!(g.children(g.root()).len(), 1);
        let rustc = child(&g, g.root(), "Rustc");
        assert_eq!(rustc.total_us, 200);
        assert_eq!(rustc.count, 2);
        let exec = child(&g, rustc, "Action.execute");
        assert_eq!(exec.total_us, 160);
        assert_eq!(exec.count, 2);
    }

    #[test]
    fn mnemonicless_slice_keeps_its_name() {
        // A top-level slice with no mnemonic keeps its own informative name, not a
        // coarse work-class bucket — so distinct operations stay distinct.
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![
                action("CppCompile", 0, 100),
                slice("checkOutputs", "general information", 100, 30),
            ],
        )]);
        let g = build_flame_graph(&p, Scope::All);
        assert_eq!(child(&g, g.root(), "CppCompile").total_us, 100);
        assert_eq!(child(&g, g.root(), "checkOutputs").total_us, 30);
    }

    #[test]
    fn scope_selects_lanes_by_role() {
        let p = profile(vec![
            track(7, LaneClass::Analysis, vec![action("CppCompile", 0, 20)]),
            track(8, LaneClass::Execution, vec![action("Rustc", 0, 40)]),
            track(9, LaneClass::RepoFetch, vec![action("Fetch", 0, 5)]),
        ]);
        // Analysis: only the analysis lane.
        let analysis = build_flame_graph(&p, Scope::Analysis);
        assert_eq!(analysis.root().total_us, 20);
        assert!(analysis
            .children(analysis.root())
            .iter()
            .all(|c| analysis.str(c.name) == "CppCompile"));
        // Execution: only the execution lane.
        let exec = build_flame_graph(&p, Scope::Execution);
        assert_eq!(exec.root().total_us, 40);
        assert!(exec
            .children(exec.root())
            .iter()
            .all(|c| exec.str(c.name) == "Rustc"));
        // Loading: only the repo-fetch lane.
        let loading = build_flame_graph(&p, Scope::Loading);
        assert_eq!(loading.root().total_us, 5);
        // All: every real lane.
        let all = build_flame_graph(&p, Scope::All);
        assert_eq!(all.root().total_us, 65);
    }

    #[test]
    fn critical_path_grafts_the_originating_action_subtree() {
        // A critical-path component names an action but carries no sub-spans; its
        // worker action (mnemonic Rustc) contains a subprocess.run child. Under
        // CriticalPath the worker's whole subtree grafts under the root, so the
        // critical action shows its sub-detail rather than a bare leaf.
        let worker = vec![
            action_named("Compiling foo", "Rustc", 0, 100),
            slice("subprocess.run", "subprocess run", 0, 80),
        ];
        let cp = slice("action 'Compiling foo'", "critical path component", 500, 50);
        let p = profile(vec![
            track(8, LaneClass::Execution, worker),
            track(0, LaneClass::CriticalPath, vec![cp]),
        ]);
        let g = build_flame_graph(&p, Scope::CriticalPath);
        assert_eq!(g.children(g.root()).len(), 1);
        let rustc = child(&g, g.root(), "Rustc");
        // Grafted from the worker lane: total is the worker action's 100, not the
        // critical component's 50, and the sub-span is present.
        assert_eq!(rustc.total_us, 100);
        assert_eq!(child(&g, rustc, "subprocess.run").total_us, 80);
    }

    #[test]
    fn critical_path_reuses_a_lane_forest_across_components() {
        // Two critical components match actions on the same worker lane, so the
        // second graft runs against the lane's cached forest; both actions must
        // still graft with their own subtrees.
        let worker = vec![
            action_named("Compiling foo", "Rustc", 0, 100),
            slice("subprocess.run", "subprocess run", 0, 80),
            action_named("Linking foo", "Link", 200, 60),
            slice("subprocess.run", "subprocess run", 200, 40),
        ];
        let p = profile(vec![
            track(8, LaneClass::Execution, worker),
            track(
                0,
                LaneClass::CriticalPath,
                vec![
                    slice("action 'Compiling foo'", "critical path component", 500, 50),
                    slice("action 'Linking foo'", "critical path component", 560, 30),
                ],
            ),
        ]);
        let g = build_flame_graph(&p, Scope::CriticalPath);
        assert_eq!(g.children(g.root()).len(), 2);
        let rustc = child(&g, g.root(), "Rustc");
        assert_eq!(rustc.total_us, 100);
        assert_eq!(child(&g, rustc, "subprocess.run").total_us, 80);
        let link = child(&g, g.root(), "Link");
        assert_eq!(link.total_us, 60);
        assert_eq!(child(&g, link, "subprocess.run").total_us, 40);
    }

    #[test]
    fn critical_path_unmatched_component_is_a_leaf() {
        // A critical component with no matching action merges as a bare leaf.
        let cp = slice("Writing file out", "critical path component", 0, 40);
        let p = profile(vec![track(0, LaneClass::CriticalPath, vec![cp])]);
        let g = build_flame_graph(&p, Scope::CriticalPath);
        assert_eq!(g.root().total_us, 40);
        assert_eq!(child(&g, g.root(), "Writing file out").total_us, 40);
    }

    #[test]
    fn overflowing_span_is_a_sibling_not_a_child() {
        // A span that starts inside another but ends AFTER it isn't enclosed, so
        // it is a sibling, not a child. Nesting it (popping only on
        // end-before-start) is what built the runaway false-depth staircase.
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![
                action("Outer", 0, 50),
                slice("inner", "general information", 10, 100),
            ],
        )]);
        let g = build_flame_graph(&p, Scope::All);
        let outer = child(&g, g.root(), "Outer");
        assert_eq!(outer.total_us, 50);
        assert!(
            g.children(outer).is_empty(),
            "inner must not nest under Outer"
        );
        assert_eq!(child(&g, g.root(), "inner").total_us, 100);
    }

    #[test]
    fn build_is_independent_of_slice_order() {
        // Disjoint top-level spans, so order only affects the emitted event
        // stream; the parser sorts each lane into lane order and the merge is
        // commutative.
        let ordered = vec![
            action("CppCompile", 0, 100),
            action("Rustc", 100, 40),
            action("CppCompile", 200, 50),
            slice("wait", "thread wait", 300, 90),
        ];
        let mut reversed = ordered.clone();
        reversed.reverse();
        let ga = build_flame_graph(&profile(vec![track(1, LaneClass::Execution, ordered)]), Scope::All);
        let gb = build_flame_graph(&profile(vec![track(1, LaneClass::Execution, reversed)]), Scope::All);
        // Compare every node field with the name resolved: the graphs hold two
        // independently interned profiles, so equal names need not share a
        // handle across them, but the flattened structure must be identical.
        let resolved = |g: &FlameGraph| -> Vec<_> {
            g.nodes()
                .iter()
                .map(|n| {
                    (
                        g.str(n.name).to_owned(),
                        n.total_us,
                        n.self_us,
                        n.width_us,
                        n.role,
                        n.count,
                        n.depth,
                        n.children,
                    )
                })
                .collect()
        };
        assert_eq!(resolved(&ga), resolved(&gb));
    }

    #[test]
    fn tied_same_text_children_order_by_role() {
        // Two same-named, same-duration top-level slices of different span roles
        // tie on (total, text); the role tie-break keeps the flattened order
        // independent of which lane merges first.
        let build = |cats: [&str; 2]| {
            let p = profile(vec![
                track(1, LaneClass::Execution, vec![slice("x", cats[0], 0, 50)]),
                track(2, LaneClass::Execution, vec![slice("x", cats[1], 0, 50)]),
            ]);
            build_flame_graph(&p, Scope::All)
        };
        let ga = build(["local action execution", "thread wait"]);
        let gb = build(["thread wait", "local action execution"]);
        let resolved = |g: &FlameGraph| -> Vec<(String, SpanRole, i64)> {
            g.nodes()
                .iter()
                .map(|n| (g.str(n.name).to_owned(), n.role, n.total_us))
                .collect()
        };
        assert_eq!(resolved(&ga), resolved(&gb));
        // Workload order (Subprocess before Scheduling) makes the tie-break total.
        assert_eq!(
            resolved(&ga)[1..],
            [
                (
                    "x".to_owned(),
                    SpanRole { workload: Workload::Subprocess, payload: true },
                    50
                ),
                (
                    "x".to_owned(),
                    SpanRole { workload: Workload::Scheduling, payload: false },
                    50
                )
            ]
        );
    }

    #[test]
    fn layout_widths_are_proportional_and_bounded() {
        // root width 150: CppCompile=100, Rustc=50 -> widths 200, 100 of 300px.
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![action("CppCompile", 0, 100), action("Rustc", 100, 50)],
        )]);
        let g = build_flame_graph(&p, Scope::All);
        let boxes = lay_out_flame(
            &g,
            300.0,
            FlamePlacement { scroll_x: 0.0, view_w: 300.0, metrics: &METRICS, min_px: 0.0 },
        );
        let root_box = &boxes[0];
        assert_eq!(root_box.w, 300.0);
        assert_eq!(root_box.y, 0.0);
        let a = boxes.iter().find(|b| g.str(b.name) == "CppCompile").unwrap();
        let b = boxes.iter().find(|b| g.str(b.name) == "Rustc").unwrap();
        assert_eq!(a.w, 200.0);
        assert_eq!(b.w, 100.0);
        assert_eq!(a.x, 0.0);
        assert_eq!(b.x, 200.0);
        assert_eq!(a.y, METRICS.row_height);
        // Children fit within the parent.
        assert!(a.w + b.w <= root_box.w + f32::EPSILON);
    }

    #[test]
    fn sub_pixel_boxes_widen_to_markers_and_coalesce() {
        // Over 100px: "wide" fills most of the row; "aaa" and "bbb" are each
        // sub-pixel and land in the same trailing column.
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![
                action("wide", 0, 500),
                action("aaa", 500, 1),
                action("bbb", 501, 1),
            ],
        )]);
        let g = build_flame_graph(&p, Scope::All);
        // Fully visible: 100px wide graph, no scroll, 100px viewport.
        let boxes = lay_out_flame(
            &g,
            100.0,
            FlamePlacement { scroll_x: 0.0, view_w: 100.0, metrics: &METRICS, min_px: 1.0 },
        );
        let wide = boxes.iter().find(|b| g.str(b.name) == "wide").unwrap();
        // Wide keeps its proportional slot at the left.
        assert_eq!(wide.x, 0.0);
        assert!(wide.w > 1.0);
        // The first sub-pixel box widens to a one-pixel marker rather than
        // vanishing; the second coalesces into its column and is dropped.
        let aaa = boxes.iter().find(|b| g.str(b.name) == "aaa").unwrap();
        assert_eq!(aaa.w, 1.0);
        assert!(!boxes.iter().any(|b| g.str(b.name) == "bbb"));
    }

    #[test]
    fn deepest_sub_pixel_sibling_wins_its_column() {
        // Two sub-pixel siblings in one column: "shallow" is a leaf and sorts
        // first (wider), "deep" is narrower but nests "deepchild". The column
        // must show "deep"'s spike, not the leftmost "shallow"'s, so its depth
        // stays put as the zoom shifts which sibling leads.
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![
                slice("pad", "general information", 0, 990),
                slice("shallow", "general information", 990, 6),
                slice("deep", "general information", 996, 4),
                slice("deepchild", "general information", 996, 1),
            ],
        )]);
        let g = build_flame_graph(&p, Scope::All);
        let boxes = lay_out_flame(
            &g,
            100.0,
            FlamePlacement { scroll_x: 0.0, view_w: 100.0, metrics: &METRICS, min_px: 1.0 },
        );
        assert!(boxes.iter().any(|b| g.str(b.name) == "pad"));
        assert!(boxes.iter().any(|b| g.str(b.name) == "deep"));
        assert!(boxes.iter().any(|b| g.str(b.name) == "deepchild"));
        // The shallower leftmost sibling is dropped in favour of the deeper one.
        assert!(!boxes.iter().any(|b| g.str(b.name) == "shallow"));
    }

    #[test]
    fn lay_out_is_viewport_relative_and_culls_off_screen() {
        // A wide graph scrolled so only its right half shows.
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![action("left", 0, 100), action("right", 100, 100)],
        )]);
        let g = build_flame_graph(&p, Scope::All);
        // 1000px content, scrolled 600px into a 400px viewport: the window covers
        // content [600, 1000) — the right half. "left" (content [0, 500)) is fully
        // off-screen; "right" (content [500, 1000)) shows from its middle.
        let boxes = lay_out_flame(
            &g,
            1000.0,
            FlamePlacement { scroll_x: 600.0, view_w: 400.0, metrics: &METRICS, min_px: 1.0 },
        );
        assert!(!boxes.iter().any(|b| g.str(b.name) == "left"));
        let right = boxes.iter().find(|b| g.str(b.name) == "right").unwrap();
        // Its content x is 500; viewport-relative that is 500 - 600 = -100 (still
        // within the -view_w margin, so unclamped).
        assert_eq!(right.x, -100.0);
        // The root spans the whole content from far left, clamped to the margin.
        assert_eq!(boxes[0].x, -400.0);
    }

    #[test]
    fn height_counts_one_row_per_depth() {
        // Rustc(0..100) contains Action.execute(0..90): root depth 0, Rustc depth
        // 1, Action.execute depth 2 -> 3 rows.
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![
                action("Rustc", 0, 100),
                slice("Action.execute", "general information", 0, 90),
            ],
        )]);
        let g = build_flame_graph(&p, Scope::All);
        assert_eq!(flame_height(&g, &METRICS), 30.0);
    }
}
