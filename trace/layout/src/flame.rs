use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bviz_trace_profile::{LaneClass, Profile, Slice, SliceDetail, Span32, Str, WorkClass};
use rustc_hash::{FxHashMap, FxHashSet};

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
    work_class: WorkClass,
}

/// Maps a [`Str`] handle to the one canonical handle carrying the same text —
/// the lowest-offset handle in the profile with it. A parallel parse assembles
/// a profile by concatenating its chunks' interned pools without cross-chunk
/// dedup (see [`Interner::push_pool`](bviz_trace_profile::Interner::push_pool)),
/// so equal strings can be carried by several handles, and frames must merge by
/// text. The lowest offset is a property of the profile, not of visit order, so
/// shards that scan the lanes in any partition agree on every canonical handle.
/// `memo` holds only the aliased handles — nearly every handle is its own
/// canonical, so it stays far smaller than the distinct-string count — and the
/// per-span path is one read-only small-key lookup, shared across merge workers
/// with no locking.
struct Canon {
    memo: FxHashMap<Str, Str>,
    /// Lexicographic rank of each scanned canonical handle: dense `u32`s
    /// assigned by one byte-wise sort of the scanned distinct texts. Canonical
    /// handles biject with texts, so rank order equals text order and every
    /// later ordering comparison is integer-only. Ranks are dense per scan and
    /// never compare across builds.
    rank: FxHashMap<Str, u32>,
}

impl Canon {
    /// The canonical handle for `s`: aliases resolve through the memo, and a
    /// handle with no entry is its own canonical. An unscanned handle silently
    /// resolves to itself — the coverage invariant of [`scan`] is what keeps
    /// text-dedup sound.
    fn canon(&self, s: Str) -> Str {
        self.memo.get(&s).copied().unwrap_or(s)
    }

    /// The lexicographic rank of a canonical handle's text.
    fn rank_of(&self, s: Str) -> u32 {
        match self.rank.get(&s) {
            Some(&r) => r,
            None => panic!("unscanned label handle: the scan must cover every lane a build walks"),
        }
    }
}

/// The lower-offset of two handles carrying equal text.
fn min_handle(a: Str, b: Str) -> Str {
    if a.range().start <= b.range().start {
        a
    } else {
        b
    }
}

/// A map from a mnemonic-bearing slice's name to its mnemonic, over the whole
/// profile. The "action processing" slices carry both the action description as
/// their name and the mnemonic in their args, so this recovers the mnemonic for
/// any other slice that names the same action — notably critical-path
/// components, whose name is `action '<desc>'` and which carry no mnemonic of
/// their own. `by_name` is keyed by the name's canonical [`Str`] handle, so the
/// per-span lookup hashes a small `Copy` key rather than the full string;
/// `by_text` holds the same entries keyed by name text, for the critical-path
/// components whose stripped `action '…'` inner has no handle of its own. Values
/// are canonical mnemonic handles.
struct MnemonicIndex<'p> {
    by_name: FxHashMap<Str, Str>,
    by_text: FxHashMap<&'p str, Str>,
}

/// One worker's share of the name scan.
#[derive(Default)]
struct ScanShard<'p> {
    /// Text → its lowest-offset handle among this shard's lanes.
    by_text: FxHashMap<&'p str, Str>,
    /// Every distinct raw name/mnemonic handle this shard saw.
    handles: FxHashSet<Str>,
    /// Mnemonic-bearing slice name text → its smallest mnemonic text. A name
    /// carrying several mnemonics resolves to the smallest, so the index is
    /// independent of visit order.
    mnemonics: FxHashMap<&'p str, &'p str>,
}

impl<'p> ScanShard<'p> {
    fn note(&mut self, profile: &'p Profile, s: Str) {
        if self.handles.insert(s) {
            self.by_text
                .entry(profile.str(s))
                .and_modify(|cur| *cur = min_handle(*cur, s))
                .or_insert(s);
        }
    }
}

/// Scan slice names and mnemonics into the canonical map and the mnemonic
/// index, sharding the lanes across `workers`. Names are noted — entered into
/// the canonical map and the rank table — only on the lanes `note_lane` admits
/// (by thread index), and the scan must admit every lane whose slices a build
/// subsequently walks: an unseen handle degrades [`Canon::canon`] to identity,
/// breaking text-dedup, and has no rank. Mnemonic handles and the mnemonic
/// index cover every lane regardless — hashing only the mnemonic-bearing
/// slices, a small fraction of the profile — because a mnemonic-less slice's
/// label can resolve to a mnemonic carried on a lane outside the scan, and
/// that label needs a canonical handle and a rank like any other.
fn scan<'p>(
    profile: &'p Profile,
    workers: usize,
    note_lane: impl Fn(usize) -> bool + Sync,
) -> (Canon, MnemonicIndex<'p>) {
    let threads = profile.threads();
    let mut shards = run_sharded(workers, threads.len(), |shard: &mut ScanShard, i| {
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

    // Seed the merged maps by moving the first shard's — a single-worker scan
    // merges for free — and size them for the rest up front, so the merge never
    // regrows (a regrow re-hashes every string key).
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
            by_text
                .entry(text)
                .and_modify(|cur| *cur = min_handle(*cur, handle))
                .or_insert(handle);
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

    // Memoize only the aliases: a handle some text elected as canonical needs
    // no entry, and skipping it skips the text hash of the memo insert, leaving
    // per-alias string work only for the handful of parse-chunk duplicates.
    let mut canonical: FxHashSet<Str> =
        FxHashSet::with_capacity_and_hasher(by_text.len(), Default::default());
    canonical.extend(by_text.values().copied());
    let mut memo = FxHashMap::default();
    for shard in &shards {
        for &h in &shard.handles {
            if !canonical.contains(&h) {
                memo.insert(h, by_text[profile.str(h)]);
            }
        }
    }

    // Rank the distinct texts by one sort under `&str`'s byte-wise `Ord`, so
    // comparing two ranks is exactly comparing the texts.
    let ordered = sort_pairs_by_text(by_text.iter().map(|(&t, &h)| (t, h)).collect(), workers);
    let rank: FxHashMap<Str, u32> = ordered
        .iter()
        .enumerate()
        .map(|(i, &(_, h))| (h, i as u32))
        .collect();

    let idx_by_text: FxHashMap<&str, Str> =
        mnemonics.iter().map(|(&n, &m)| (n, by_text[m])).collect();
    // A name seen only on unadmitted lanes has no noted handle and needs no
    // handle-keyed entry: only a walked — hence noted — slice's name is ever
    // looked up by handle, while text-keyed lookups cover every name.
    let by_name = idx_by_text
        .iter()
        .filter_map(|(&n, &m)| by_text.get(n).map(|&h| (h, m)))
        .collect();
    (
        Canon { memo, rank },
        MnemonicIndex {
            by_name,
            by_text: idx_by_text,
        },
    )
}

/// Worker count for the aggregation passes: the parallelism granted to the
/// process, queried at runtime.
pub(crate) fn worker_count() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
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

/// Sort `(text, handle)` pairs into byte-wise text order, sharding across
/// `workers` by sampled pivots: each worker classifies a chunk of the input
/// into pivot-bounded buckets, each bucket sorts on its own worker, and the
/// buckets concatenate in pivot order. The texts must be distinct, so the
/// result is the one total order whatever the sampled partition. Small inputs
/// sort serially — the partition overhead only pays off at scale.
fn sort_pairs_by_text<'p>(mut pairs: Vec<(&'p str, Str)>, workers: usize) -> Vec<(&'p str, Str)> {
    /// Input size below which the serial sort wins over partitioning.
    const PARALLEL_MIN: usize = 1 << 13;
    if workers <= 1 || pairs.len() < PARALLEL_MIN {
        pairs.sort_unstable_by_key(|&(t, _)| t);
        return pairs;
    }
    // Evenly spaced cuts of a sorted sparse sample balance the buckets even
    // when the keys cluster under shared prefixes.
    const OVERSAMPLE: usize = 32;
    let step = (pairs.len() / (workers * OVERSAMPLE)).max(1);
    let mut sample: Vec<&str> = pairs.iter().step_by(step).map(|&(t, _)| t).collect();
    sample.sort_unstable();
    let pivots: Vec<&str> = (1..workers)
        .map(|i| sample[i * sample.len() / workers])
        .collect();

    // Bucket `b` holds the texts with exactly `b` pivots at or below them.
    let chunk = pairs.len().div_ceil(workers);
    let mut buckets: Vec<Vec<(&str, Str)>> = vec![Vec::new(); workers];
    std::thread::scope(|scope| {
        let handles: Vec<_> = pairs
            .chunks(chunk)
            .map(|part| {
                let pivots = &pivots;
                scope.spawn(move || {
                    let mut local: Vec<Vec<(&str, Str)>> = vec![Vec::new(); workers];
                    for &(t, h) in part {
                        local[pivots.partition_point(|&p| p <= t)].push((t, h));
                    }
                    local
                })
            })
            .collect();
        for handle in handles {
            for (b, mut part) in handle.join().unwrap().into_iter().enumerate() {
                buckets[b].append(&mut part);
            }
        }
    });
    std::thread::scope(|scope| {
        for bucket in &mut buckets {
            scope.spawn(move || bucket.sort_unstable_by_key(|&(t, _)| t));
        }
    });
    buckets.into_iter().flatten().collect()
}

/// The inner of an `action '…'` name, `None` for any other shape. A
/// critical-path component is named `action '<desc>'` where `<desc>` is the
/// action processing slice's name elsewhere; stripping the wrapper lets the
/// name index recover its mnemonic.
fn action_inner(name: &str) -> Option<&str> {
    name.strip_prefix("action '")?.strip_suffix('\'')
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
    pub work_class: WorkClass,
    pub count: u32,
    pub depth: u32,
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

/// Horizontal view of the flame graph: a window into its normalized width
/// `[0, 1]` at a zoom factor. Unlike `TimeView` there is no time axis — `zoom`
/// `1.0` fits the whole graph to the content column, and `left` is the fraction
/// of the full width at the window's left edge. Holding the position as a
/// fraction makes a window resize inherently invertible: the same fraction stays
/// in view at any width, so growing then shrinking the window is a no-op.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlameView {
    pub left: f64,
    pub zoom: f64,
}

impl FlameView {
    /// The whole graph fitted to the content column.
    pub const FIT: FlameView = FlameView {
        left: 0.0,
        zoom: 1.0,
    };

    /// Full laid-out width in pixels at this zoom for a content column `width_px`
    /// wide. Lay the tree out at this width so sub-pixel culling tracks the zoom.
    pub fn content_px(&self, width_px: f32) -> f32 {
        (width_px as f64 * self.zoom) as f32
    }

    /// Pixel offset of the window's left edge into the laid-out width — the
    /// horizontal scroll position.
    pub fn scroll_px(&self, width_px: f32) -> f32 {
        (self.left * width_px as f64 * self.zoom) as f32
    }

    /// Laid-out width in pixels, floored at one so a collapsed column can't make
    /// the fraction conversions divide by zero.
    fn span_px(&self, width_px: f32) -> f64 {
        (self.zoom * width_px as f64).max(1.0)
    }

    /// Graph fraction at a content-relative pixel.
    pub fn frac_of(&self, x_px: f32, width_px: f32) -> f64 {
        self.left + x_px as f64 / self.span_px(width_px)
    }

    /// Zoom by `factor` (>1 zooms in) holding the graph fraction under the
    /// content-relative pixel `x_px` fixed; zoom never drops below the fit.
    pub fn zoom_at(&mut self, x_px: f32, width_px: f32, factor: f64) {
        let anchor = self.frac_of(x_px, width_px);
        self.zoom = (self.zoom * factor).max(1.0);
        self.left = anchor - x_px as f64 / self.span_px(width_px);
        self.clamp();
    }

    /// Pan by a pixel delta; a positive `dx_px` moves content rightward.
    pub fn pan_px(&mut self, dx_px: f32, width_px: f32) {
        self.left -= dx_px as f64 / self.span_px(width_px);
        self.clamp();
    }

    /// Set the scroll position from a pixel offset into the laid-out width.
    pub fn set_scroll_px(&mut self, offset_px: f32, width_px: f32) {
        self.left = offset_px as f64 / self.span_px(width_px);
        self.clamp();
    }

    /// Bound zoom to the fit and the window within the graph.
    fn clamp(&mut self) {
        self.zoom = self.zoom.max(1.0);
        let max_left = (1.0 - 1.0 / self.zoom).max(0.0);
        self.left = self.left.clamp(0.0, max_left);
    }
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
    pub work_class: WorkClass,
    pub count: u32,
}

/// A node still under construction. Children are kept in insertion order in
/// `children` and located by their `(label, work_class)` merge key through
/// `index`, so merging a span into an existing child is O(1): labels are
/// per-mnemonic, so a single frame can gather thousands of distinct children,
/// where a linear scan per insert would be quadratic. Labels are interned
/// profile handles, so the key is a `Copy` `(Str, WorkClass)` — small and
/// fixed-size, which FxHash hashes in a few cycles where SipHash would dominate
/// the per-span lookup. Sorting for [`flatten`] reorders `children` without
/// updating `index`, so no merge may follow a sort.
pub(crate) struct Builder {
    name: Str,
    /// Lexicographic rank of `name`'s text (see [`FlameNode::rank`]); the
    /// root's is unused — a root is never ordered against a sibling.
    rank: u32,
    work_class: WorkClass,
    total_us: i64,
    count: u32,
    children: Vec<Builder>,
    index: FxHashMap<(Str, WorkClass), usize>,
}

impl Builder {
    pub(crate) fn new_root() -> Self {
        Builder::new(ROOT_LABEL, 0, WorkClass::Meta)
    }

    fn new(name: Str, rank: u32, work_class: WorkClass) -> Self {
        Builder {
            name,
            rank,
            work_class,
            total_us: 0,
            count: 0,
            children: Vec::new(),
            index: FxHashMap::default(),
        }
    }

    /// Find the child matching this segment, or append a fresh one built with
    /// `rank()`. The work class travels with the label; the first occurrence
    /// wins. Ranks are lazy so a merge into an existing child costs no lookup.
    fn child(&mut self, seg: Segment, rank: impl FnOnce() -> u32) -> &mut Builder {
        let key = (seg.label, seg.work_class);
        let idx = *self.index.entry(key).or_insert_with(|| {
            let i = self.children.len();
            self.children
                .push(Builder::new(seg.label, rank(), seg.work_class));
            i
        });
        &mut self.children[idx]
    }
}

/// Merges spans into the builder tree. Every frame label is a canonical
/// interned handle of the source profile — a span's mnemonic and name already
/// live in its text — so merge keys compare and hash as plain integers with no
/// per-span string work. Read-only once built, so merge workers share one.
pub(crate) struct Flamer<'p> {
    profile: &'p Profile,
    idx: MnemonicIndex<'p>,
    canon: Canon,
}

impl<'p> Flamer<'p> {
    /// A flamer whose name scan covers every lane, for a build that may walk
    /// any of them.
    pub(crate) fn new(profile: &'p Profile, workers: usize) -> Self {
        Flamer::scoped(profile, workers, |_| true)
    }

    /// A flamer whose name scan covers only the lanes `note_lane` admits —
    /// which must include every lane the build walks (see [`scan`]). The scan
    /// is the dominant cost of a single-scope build, so a build that walks few
    /// lanes scans only those.
    pub(crate) fn scoped(
        profile: &'p Profile,
        workers: usize,
        note_lane: impl Fn(usize) -> bool + Sync,
    ) -> Self {
        let (canon, idx) = scan(profile, workers, note_lane);
        Flamer {
            profile,
            idx,
            canon,
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
            Some(m) => self.canon.canon(m),
            None => {
                let name = self.canon.canon(slice.name);
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
            work_class: slice.work_class,
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
        let node = parent.child(seg, || self.canon.rank_of(seg.label));
        node.total_us += lane_slices[span_idx].dur_us;
        node.count += 1;
        // Reborrow `node` per child: `child()` borrows `parent` mutably, so the
        // recursion can't hold a `&mut node` across calls; locate it by key each
        // time, which the index makes O(1).
        let key = (seg.label, seg.work_class);
        for ci in 0..forest.children[span_idx].len() {
            let c = forest.children[span_idx][ci];
            let node = {
                let i = parent.index[&key];
                &mut parent.children[i]
            };
            self.merge_span(node, lane_slices, lane_details, forest, c);
        }
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
    let mut roots = Vec::new();
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
    let workers = worker_count();
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

/// The tree for `class` in a class-keyed list, inserted at its slot ([`LaneClass`]
/// declaration order) when absent.
fn class_slot(trees: &mut Vec<(LaneClass, Builder)>, class: LaneClass) -> &mut Builder {
    match trees.iter().position(|(c, _)| *c == class) {
        Some(i) => &mut trees[i].1,
        None => {
            let at = trees.partition_point(|(c, _)| (*c as usize) < class as usize);
            trees.insert(at, (class, Builder::new_root()));
            &mut trees[at].1
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
pub(crate) fn class_trees<'p>(
    profile: &'p Profile,
    flamer: &Flamer<'p>,
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
        let root = class_slot(trees, class);
        let slices = profile.slices(thread);
        let details = profile.details(thread);
        let forest = containment_forest(slices);
        for ri in 0..forest.roots.len() {
            let r = forest.roots[ri];
            root.total_us += slices[r].dur_us;
            flamer.merge_span(root, slices, details, &forest, r);
        }
    });

    let mut buckets: Vec<(LaneClass, Vec<Builder>)> = Vec::new();
    for shard in shards {
        for (class, tree) in shard {
            match buckets.iter_mut().find(|(c, _)| *c == class) {
                Some((_, parts)) => parts.push(tree),
                None => {
                    let at = buckets.partition_point(|(c, _)| (*c as usize) < class as usize);
                    buckets.insert(at, (class, vec![tree]));
                }
            }
        }
    }
    // The classes' partial trees merge independently of one another.
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
        match dst.index.entry((child.name, child.work_class)) {
            std::collections::hash_map::Entry::Occupied(e) => {
                let i = *e.get();
                fold_into(&mut dst.children[i], child);
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(dst.children.len());
                dst.children.push(child);
            }
        }
    }
}

/// Merge a copy of `src`'s subtree into `dst`, leaving `src` intact — for
/// folding a tree that must also seal into a graph of its own.
pub(crate) fn fold_ref(dst: &mut Builder, src: &Builder) {
    dst.total_us += src.total_us;
    dst.count += src.count;
    for child in &src.children {
        let seg = Segment {
            label: child.name,
            work_class: child.work_class,
        };
        let d = dst.child(seg, || child.rank);
        fold_ref(d, child);
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

/// Where an action-processing span lives: its lane (a thread index) and its index
/// within that lane's slices, keyed by the span name (the action description).
pub(crate) struct ActionLoc {
    lane: usize,
    slice: usize,
}

/// Index every mnemonic-bearing span by its name — the action description — so
/// a critical-path component resolves to the worker span it copies. Hashes only
/// the mnemonic-bearing slices, a small fraction of the profile; the first lane
/// hosting a name wins, and lanes are visited in profile order, so the index is
/// deterministic.
pub(crate) fn action_locations(profile: &Profile) -> FxHashMap<&str, ActionLoc> {
    let threads = profile.threads();
    let mut actions: FxHashMap<&str, ActionLoc> = FxHashMap::default();
    for (lane, thread) in threads.iter().enumerate() {
        let details = profile.details(thread);
        for (slice, s) in profile.slices(thread).iter().enumerate() {
            if details[slice].mnemonic.get().is_some() {
                actions
                    .entry(profile.str(s.name))
                    .or_insert(ActionLoc { lane, slice });
            }
        }
    }
    actions
}

/// The lanes a critical-path build walks: every critical-path lane plus the
/// lane of every action a component resolves to. Selecting them needs only the
/// component names and the action index — both cheap — so the expensive name
/// scan can be restricted to this set.
fn critical_lanes(profile: &Profile, actions: &FxHashMap<&str, ActionLoc>) -> FxHashSet<usize> {
    let threads = profile.threads();
    let mut lanes = FxHashSet::default();
    for (i, thread) in threads.iter().enumerate() {
        if thread.lane_class != LaneClass::CriticalPath {
            continue;
        }
        lanes.insert(i);
        for cp in profile.slices(thread) {
            let name = profile.str(cp.name);
            let loc = action_inner(name)
                .and_then(|inner| actions.get(inner))
                .or_else(|| actions.get(name));
            if let Some(loc) = loc {
                lanes.insert(loc.lane);
            }
        }
    }
    lanes
}

/// Graft the critical path's components into `root`. Each component on a
/// `LaneClass::CriticalPath` lane names an action (`action '<desc>'`); when that
/// action's worker span is found in `actions` (an [`action_locations`] index),
/// its whole subtree is merged under the root so the critical action shows its
/// internal breakdown. Components with no matching action (e.g. "Writing file
/// …") merge as a bare leaf. `flamer`'s scan must cover the lanes
/// [`critical_lanes`] selects from the same index.
pub(crate) fn build_critical_path(
    profile: &Profile,
    flamer: &Flamer,
    actions: &FxHashMap<&str, ActionLoc>,
    root: &mut Builder,
) {
    let threads = profile.threads();
    // Components cluster on few worker lanes; build each lane's forest once.
    let mut forests: HashMap<usize, Forest> = HashMap::new();
    for thread in threads {
        if thread.lane_class != LaneClass::CriticalPath {
            continue;
        }
        for (cp, cp_detail) in profile.slices(thread).iter().zip(profile.details(thread)) {
            let name = profile.str(cp.name);
            let loc = action_inner(name)
                .and_then(|inner| actions.get(inner))
                .or_else(|| actions.get(name));
            match loc {
                Some(&ActionLoc { lane, slice }) => {
                    let lane_slices = profile.slices(&threads[lane]);
                    let lane_details = profile.details(&threads[lane]);
                    let forest = forests
                        .entry(lane)
                        .or_insert_with(|| containment_forest(lane_slices));
                    root.total_us += lane_slices[slice].dur_us;
                    flamer.merge_span(root, lane_slices, lane_details, forest, slice);
                }
                None => {
                    root.total_us += cp.dur_us;
                    let seg = flamer.span_label(cp, cp_detail);
                    let node = root.child(seg, || flamer.canon.rank_of(seg.label));
                    node.total_us += cp.dur_us;
                    node.count += 1;
                }
            }
        }
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
        work_class: builder.work_class,
        count: builder.count,
        depth,
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
    fn sort_tree(builder: &mut Builder) {
        builder
            .children
            // Labels are canonical, so same-text children differ only by work
            // class — the last tie-break makes the order total and hence
            // independent of merge order.
            .sort_unstable_by_key(|c| (std::cmp::Reverse(c.total_us), c.rank, c.work_class));
        for c in &mut builder.children {
            sort_tree(c);
        }
    }
    sort_tree(root);

    let mut nodes = vec![freeze(root, 0)];
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
    nodes
}

/// Lay the graph into a content rectangle of the given width. The root spans the
/// full width; each node's children are placed left-to-right with width
/// proportional to `width_us`, so the right-hand remainder is the parent's self
/// time (zero when children fill the parent). Dividing by `width_us` keeps an
/// overflowing child inside its parent. Depth maps to y via `row_height`. Boxes
/// narrower than `min_px` are omitted but still consume their horizontal share so
/// siblings stay placed. Boxes come back in paint order, parents before children.
pub fn lay_out_flame(
    graph: &FlameGraph,
    width: f32,
    metrics: &FlameMetrics,
    min_px: f32,
) -> Vec<FlameBox> {
    let mut out = Vec::new();
    place(graph, graph.root(), 0.0, width, metrics, min_px, &mut out);
    out
}

fn place(
    graph: &FlameGraph,
    node: &FlameNode,
    x: f32,
    w: f32,
    metrics: &FlameMetrics,
    min_px: f32,
    out: &mut Vec<FlameBox>,
) {
    if w >= min_px {
        out.push(box_of(node, x, w, metrics));
    }
    if node.width_us == 0 {
        return;
    }
    let mut cursor = x;
    for child in graph.children(node) {
        let cw = w * child.width_us as f32 / node.width_us as f32;
        place(graph, child, cursor, cw, metrics, min_px, out);
        cursor += cw;
    }
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
        work_class: node.work_class,
        count: node.count,
    }
}

/// The box at `(target_depth, x_px)` in a layout of `graph` into `width` pixels,
/// found by descending the tree the same way [`lay_out_flame`] places boxes —
/// children left-to-right proportional to `width_us` in their stored order — so
/// the result agrees with the corresponding `lay_out_flame` box without building
/// the full box vector. Returns `None` when `x_px` is outside `[0, width)`, no
/// node sits at that depth under it, or the matching box is narrower than
/// `min_px` (and so `lay_out_flame` would omit it).
pub fn flame_box_at(
    graph: &FlameGraph,
    x_px: f32,
    target_depth: u32,
    width: f32,
    metrics: &FlameMetrics,
    min_px: f32,
) -> Option<FlameBox> {
    if x_px < 0.0 || x_px >= width {
        return None;
    }
    let mut node = graph.root();
    let mut x = 0.0_f32;
    let mut w = width;
    loop {
        if node.depth == target_depth {
            if w < min_px {
                return None;
            }
            return Some(box_of(node, x, w, metrics));
        }
        if node.width_us == 0 {
            return None;
        }
        // Descend into the child whose proportional x-extent contains x_px,
        // accumulating its origin and width exactly as `place` does.
        let mut cursor = x;
        let mut next = None;
        for child in graph.children(node) {
            let cw = w * child.width_us as f32 / node.width_us as f32;
            if x_px >= cursor && x_px < cursor + cw {
                next = Some((child, cursor, cw));
                break;
            }
            cursor += cw;
        }
        let (child, cx, cw) = next?;
        node = child;
        x = cx;
        w = cw;
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
        _assert_thread_safe::<crate::StatRow>();
    }

    #[test]
    fn parallel_text_sort_matches_serial() {
        // Enough pairs to cross the parallel threshold, with heavy shared
        // prefixes, case splits, and non-ASCII segments, so the sampled pivot
        // partition is exercised on clustered keys.
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
        for workers in [2, 3, 8, 16] {
            assert_eq!(
                sort_pairs_by_text(pairs.clone(), workers),
                serial,
                "x{workers}"
            );
        }
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
    fn equal_text_merges_across_distinct_handles() {
        // A profile assembled from parallel parse chunks carries equal strings
        // under distinct handles (the chunk pools concatenate without
        // cross-chunk dedup), so frames must merge by text, not raw handle.
        let mut p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![action("Rustc", 0, 100), action("Rustc", 200, 50)],
        )]);
        Arc::get_mut(&mut p).unwrap().duplicate_str_for_test("Rustc");
        // The fixture really carries two handles for the one text.
        let slices = p.slices(&p.threads()[0]);
        assert_ne!(slices[0].name, slices[1].name);
        assert_eq!(p.str(slices[0].name), p.str(slices[1].name));
        let g = build_flame_graph(&p, Scope::All);
        assert_eq!(g.children(g.root()).len(), 1);
        let rustc = child(&g, g.root(), "Rustc");
        assert_eq!(rustc.total_us, 150);
        assert_eq!(rustc.count, 2);
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
                        n.work_class,
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
    fn tied_same_text_children_order_by_work_class() {
        // Two same-named, same-duration top-level slices of different work
        // classes tie on (total, text); the work-class tie-break keeps the
        // flattened order independent of which lane merges first.
        let build = |cats: [&str; 2]| {
            let p = profile(vec![
                track(1, LaneClass::Execution, vec![slice("x", cats[0], 0, 50)]),
                track(2, LaneClass::Execution, vec![slice("x", cats[1], 0, 50)]),
            ]);
            build_flame_graph(&p, Scope::All)
        };
        let ga = build(["local action execution", "thread wait"]);
        let gb = build(["thread wait", "local action execution"]);
        let resolved = |g: &FlameGraph| -> Vec<(String, WorkClass, i64)> {
            g.nodes()
                .iter()
                .map(|n| (g.str(n.name).to_owned(), n.work_class, n.total_us))
                .collect()
        };
        assert_eq!(resolved(&ga), resolved(&gb));
        assert_eq!(
            resolved(&ga)[1..],
            [
                ("x".to_owned(), WorkClass::Cpu, 50),
                ("x".to_owned(), WorkClass::Wait, 50)
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
        let boxes = lay_out_flame(&g, 300.0, &METRICS, 0.0);
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
    fn sub_min_boxes_are_dropped_without_shifting_siblings() {
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![action("Wide", 0, 990), action("Thin", 990, 10)],
        )]);
        let g = build_flame_graph(&p, Scope::All);
        // 1000us over 100px: Thin is 1px wide, below a 5px floor.
        let boxes = lay_out_flame(&g, 100.0, &METRICS, 5.0);
        assert!(boxes.iter().any(|b| g.str(b.name) == "Wide"));
        assert!(!boxes.iter().any(|b| g.str(b.name) == "Thin"));
        let wide = boxes.iter().find(|b| g.str(b.name) == "Wide").unwrap();
        // Wide keeps its proportional slot; Thin's omission does not move it.
        assert_eq!(wide.x, 0.0);
        assert!((wide.w - 99.0).abs() < 1e-3);
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

    #[test]
    fn flame_box_at_agrees_with_lay_out_flame() {
        // A multi-level time-contained tree so the descent is exercised across
        // depths: two disjoint actions, each containing a sub-span.
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![
                action("CppCompile", 0, 100),
                slice("Action.execute", "general information", 0, 90),
                action("Rustc", 100, 80),
                slice("subprocess.run", "subprocess run", 100, 60),
            ],
        )]);
        let g = build_flame_graph(&p, Scope::All);
        let width = 600.0;
        let min_px = 1.0;
        let boxes = lay_out_flame(&g, width, &METRICS, min_px);
        // For every laid-out box, a probe at its center maps back to it.
        for b in &boxes {
            let probe_x = b.x + b.w / 2.0;
            let hit = flame_box_at(&g, probe_x, b.depth, width, &METRICS, min_px);
            assert_eq!(hit.as_ref(), Some(b), "probe at depth {} x {probe_x}", b.depth);
        }
        // Outside the content width yields nothing.
        assert_eq!(flame_box_at(&g, -1.0, 0, width, &METRICS, min_px), None);
        assert_eq!(flame_box_at(&g, width, 0, width, &METRICS, min_px), None);
        // A depth past the tree under a real x yields nothing.
        let deep = g.nodes().iter().map(|n| n.depth).max().unwrap() + 1;
        assert_eq!(flame_box_at(&g, 1.0, deep, width, &METRICS, min_px), None);
    }

    #[test]
    fn flame_box_at_omits_sub_min_boxes() {
        // 1000us over 100px: Thin is 1px wide, below a 5px floor — both
        // lay_out_flame and the descent omit it at Thin's depth and x.
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![action("Wide", 0, 990), action("Thin", 990, 10)],
        )]);
        let g = build_flame_graph(&p, Scope::All);
        let width = 100.0;
        let min_px = 5.0;
        // Thin sits at depth 1, x in [99, 100); probe its center.
        assert_eq!(flame_box_at(&g, 99.5, 1, width, &METRICS, min_px), None);
        // Its Wide sibling at the same depth is found.
        let wide = lay_out_flame(&g, width, &METRICS, min_px)
            .into_iter()
            .find(|b| g.str(b.name) == "Wide")
            .unwrap();
        assert_eq!(flame_box_at(&g, 1.0, 1, width, &METRICS, min_px), Some(wide));
    }

    #[test]
    fn flame_view_fit_spans_the_content_width() {
        let v = FlameView::FIT;
        assert_eq!(v.content_px(800.0), 800.0);
        assert_eq!(v.scroll_px(800.0), 0.0);
    }

    #[test]
    fn flame_view_zoom_holds_the_fraction_under_the_cursor() {
        let mut v = FlameView::FIT;
        // The fraction under x=600 of an 800px column is 0.75; it stays put.
        let before = v.frac_of(600.0, 800.0);
        v.zoom_at(600.0, 800.0, 4.0);
        let after = v.frac_of(600.0, 800.0);
        assert!((before - after).abs() < 1e-12);
        assert_eq!(v.zoom, 4.0);
        assert_eq!(v.content_px(800.0), 3200.0);
    }

    #[test]
    fn flame_view_zoom_never_drops_below_fit() {
        let mut v = FlameView::FIT;
        v.zoom_at(400.0, 800.0, 0.25);
        assert_eq!(v.zoom, 1.0);
        assert_eq!(v.left, 0.0);
    }

    #[test]
    fn flame_view_position_is_width_independent() {
        // A fraction-based position keeps the same fraction in view across a
        // width change, so a window resize is a no-op.
        let mut v = FlameView::FIT;
        v.zoom_at(600.0, 800.0, 3.0);
        let left = v.left;
        // The visible window is the same fraction whatever the column width.
        assert!((v.frac_of(0.0, 800.0) - left).abs() < 1e-12);
        assert!((v.frac_of(0.0, 1280.0) - left).abs() < 1e-12);
        assert!((v.frac_of(800.0, 800.0) - v.frac_of(1280.0, 1280.0)).abs() < 1e-12);
    }

    #[test]
    fn flame_view_pan_clamps_within_the_graph() {
        let mut v = FlameView::FIT;
        v.zoom_at(0.0, 800.0, 2.0);
        // Pan far past the right edge; left stops at 1 - 1/zoom = 0.5.
        v.pan_px(-10_000.0, 800.0);
        assert!((v.left - 0.5).abs() < 1e-12);
        // And cannot pan before the start.
        v.pan_px(10_000.0, 800.0);
        assert_eq!(v.left, 0.0);
    }

    #[test]
    fn flame_view_scroll_offset_round_trips() {
        let mut v = FlameView::FIT;
        v.zoom_at(400.0, 800.0, 4.0);
        let offset = v.scroll_px(800.0);
        let mut w = FlameView { left: 0.0, zoom: 4.0 };
        w.set_scroll_px(offset, 800.0);
        assert!((w.left - v.left).abs() < 1e-12);
    }
}
