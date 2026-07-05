use std::collections::HashMap;

use bviz_trace_profile::{Interner, LaneClass, Profile, Slice, SliceDetail, Span32, Str, WorkClass};

/// Which label a span carries in the flame graph. The tree shape comes from
/// time-containment within a lane; the mode only chooses how each span is named,
/// so identical-named spans merge into one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupMode {
    /// Action mnemonic (recovered from the name index for slices that name an
    /// action) or, for everything else, the slice name itself.
    Mnemonic,
    /// The slice's Bazel category.
    Category,
    /// The slice's work class.
    WorkClass,
}

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

/// One key segment of a slice's aggregation path: a frame label interned into
/// the flame's own text pool, and the work class it carries (for coloring and
/// class-keyed merging).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Segment {
    label: Str,
    work_class: WorkClass,
}

/// A map from a mnemonic-bearing slice's name to its mnemonic, over the whole
/// profile, by interned profile handle. The "action processing" slices carry
/// both the action description as their name and the mnemonic in their args, so
/// this recovers the mnemonic for any other slice that names the same action —
/// notably critical-path components, whose name is `action '<desc>'` and which
/// carry no mnemonic of their own.
pub(crate) fn mnemonic_index(profile: &Profile) -> HashMap<&str, &str> {
    let mut idx = HashMap::new();
    for thread in profile.threads() {
        for (s, d) in profile.slices(thread).iter().zip(profile.details(thread)) {
            if let Some(m) = d.mnemonic.get() {
                idx.insert(profile.str(s.name), profile.str(m));
            }
        }
    }
    idx
}

/// The inner of an `action '…'` name, else the name unchanged. A critical-path
/// component is named `action '<desc>'` where `<desc>` is the action processing
/// slice's name elsewhere; stripping the wrapper lets the name index recover its
/// mnemonic.
fn action_inner(name: &str) -> &str {
    name.strip_prefix("action '")
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(name)
}

/// The slice's action mnemonic, taken from its own args if present, else
/// recovered from the name index by the slice's name or — for a critical-path
/// component — by its unwrapped action description. `None` when the slice names
/// no known action.
fn resolve_mnemonic<'a>(
    profile: &'a Profile,
    slice: &Slice,
    detail: &SliceDetail,
    idx: &HashMap<&'a str, &'a str>,
) -> Option<&'a str> {
    if let Some(m) = detail.mnemonic.get() {
        return Some(profile.str(m));
    }
    let name = profile.str(slice.name);
    idx.get(name)
        .copied()
        .or_else(|| idx.get(action_inner(name)).copied())
}

/// The work-class label for a slice (no parens; the class itself, not a bucket).
fn work_class_label(class: WorkClass) -> &'static str {
    match class {
        WorkClass::Cpu => "cpu",
        WorkClass::Remote => "remote",
        WorkClass::Io => "io",
        WorkClass::Wait => "wait",
        WorkClass::Gc => "gc",
        WorkClass::Meta => "meta",
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
    pub total_us: i64,
    pub self_us: i64,
    pub width_us: i64,
    pub work_class: WorkClass,
    pub count: u32,
    pub depth: u32,
    pub children: Span32,
}

/// An aggregated flame graph as a flat, self-contained arena: node 0 is the
/// synthetic root, every other node is reached through a parent's `children`
/// range, and all node names are interned into `text` (copied out of the source
/// profile, so the graph borrows nothing). The flat layout makes the whole graph
/// a couple of allocations to drop and lets it cross threads — it is built off
/// the UI thread and handed back with no lifetime parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct FlameGraph {
    text: String,
    nodes: Vec<FlameNode>,
    /// Deepest node depth, found once at build so [`flame_height`] need not scan
    /// every node on each paint.
    max_depth: u32,
}

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

    /// Resolves a node name (or a [`StatRow`](crate::StatRow) key) to its text.
    pub fn str(&self, s: Str) -> &str {
        &self.text[s.range()]
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
/// for hit-testing and tooltips; `name` is a [`Str`] into the producing
/// [`FlameGraph`]'s text pool, resolved with [`FlameGraph::str`].
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
/// `index`, so merging a span into an existing child is O(1): under a fine
/// grouping (mnemonic) a single frame can gather thousands of distinct children,
/// where a linear scan per insert would be quadratic. Labels are already interned
/// into the flame's text pool, so the key is a `Copy` `(Str, WorkClass)`.
struct Builder {
    name: Str,
    work_class: WorkClass,
    total_us: i64,
    count: u32,
    children: Vec<Builder>,
    index: HashMap<(Str, WorkClass), usize>,
}

impl Builder {
    fn new(name: Str, work_class: WorkClass) -> Self {
        Builder {
            name,
            work_class,
            total_us: 0,
            count: 0,
            children: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// Find the child matching this segment, or append a fresh one. The work
    /// class travels with the label; the first occurrence wins.
    fn child(&mut self, seg: Segment) -> &mut Builder {
        let key = (seg.label, seg.work_class);
        let idx = *self.index.entry(key).or_insert_with(|| {
            let i = self.children.len();
            self.children.push(Builder::new(seg.label, seg.work_class));
            i
        });
        &mut self.children[idx]
    }
}

/// Assembles the flame's text pool and node array. Holds the [`Interner`] that
/// every label is interned through, so the produced [`Str`] handles index the
/// graph's own text — making the graph self-contained and `Send`.
struct Flamer<'p> {
    profile: &'p Profile,
    idx: HashMap<&'p str, &'p str>,
    mode: GroupMode,
    interner: Interner,
}

impl<'p> Flamer<'p> {
    fn new(profile: &'p Profile, mode: GroupMode) -> Self {
        Flamer {
            profile,
            idx: mnemonic_index(profile),
            mode,
            interner: Interner::new(),
        }
    }

    /// The finest detail label for a slice: its resolved action mnemonic if it
    /// names an action (so all Rustc collapse together), else its own name (the
    /// informative label for non-action slices — "checkOutputs", "subprocess.run",
    /// a repo target, …).
    fn detail(&mut self, slice: &Slice, detail: &SliceDetail) -> Segment {
        let label = match resolve_mnemonic(self.profile, slice, detail, &self.idx) {
            Some(m) => self.interner.intern(m),
            None => self.interner.intern(self.profile.str(slice.name)),
        };
        Segment {
            label,
            work_class: slice.work_class,
        }
    }

    /// The label a span carries under the grouping mode, using the name index to
    /// recover action mnemonics. The tree shape comes from time-containment, not
    /// from this label; the mode only names each span so identical-named spans
    /// merge into one frame.
    fn span_label(&mut self, slice: &Slice, detail: &SliceDetail) -> Segment {
        match self.mode {
            GroupMode::Mnemonic => self.detail(slice, detail),
            GroupMode::Category => Segment {
                label: self.interner.intern(self.profile.str(detail.category)),
                work_class: slice.work_class,
            },
            GroupMode::WorkClass => Segment {
                label: self.interner.intern(work_class_label(slice.work_class)),
                work_class: slice.work_class,
            },
        }
    }

    /// Merge span `span_idx` of `lane_slices` and its time-children into `parent`:
    /// the span's duration lands once at its own labelled node, then each child
    /// recurses under that node. So a node merging N spans sums their durations,
    /// and a child's duration is counted at the child, not again at the parent.
    /// `lane_details` is the lane's parallel detail slice.
    fn merge_span(
        &mut self,
        parent: &mut Builder,
        lane_slices: &[Slice],
        lane_details: &[SliceDetail],
        forest: &Forest,
        span_idx: usize,
    ) {
        let seg = self.span_label(&lane_slices[span_idx], &lane_details[span_idx]);
        let node = parent.child(seg);
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
/// time-contains it, and identical-`mode`-labelled spans at the same place merge
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
/// The returned [`FlameGraph`] owns its text pool and flat node array, copying
/// every name out of `profile`, so it borrows nothing and is `Send`.
pub fn build_flame_graph(profile: &Profile, mode: GroupMode, scope: Scope) -> FlameGraph {
    let mut flamer = Flamer::new(profile, mode);
    let root_name = flamer.interner.intern("All");
    let mut root = Builder::new(root_name, WorkClass::Meta);

    if scope == Scope::CriticalPath {
        build_critical_path(profile, &mut flamer, &mut root);
    } else {
        for thread in profile.threads() {
            if !scope.includes(thread.lane_class) {
                continue;
            }
            let slices = profile.slices(thread);
            let details = profile.details(thread);
            let forest = containment_forest(slices);
            for ri in 0..forest.roots.len() {
                let r = forest.roots[ri];
                root.total_us += slices[r].dur_us;
                flamer.merge_span(&mut root, slices, details, &forest, r);
            }
        }
    }

    let text = flamer.interner.into_text();
    let nodes = flatten(root, &text);
    let max_depth = nodes.iter().map(|n| n.depth).max().unwrap_or(0);
    FlameGraph {
        text,
        nodes,
        max_depth,
    }
}

/// Where an action-processing span lives: its lane (a thread index) and its index
/// within that lane's slices, keyed by the span name (the action description).
struct ActionLoc {
    lane: usize,
    slice: usize,
}

/// Graft the critical path's components into `root`. Each component on a
/// `LaneClass::CriticalPath` lane names an action (`action '<desc>'`); when that
/// action's worker span is found by name, its whole subtree is merged under the
/// root so the critical action shows its internal breakdown. Components with no
/// matching action (e.g. "Writing file …") merge as a bare leaf.
fn build_critical_path(profile: &Profile, flamer: &mut Flamer, root: &mut Builder) {
    let threads = profile.threads();
    let mut actions: HashMap<&str, ActionLoc> = HashMap::new();
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

    for thread in threads {
        if thread.lane_class != LaneClass::CriticalPath {
            continue;
        }
        for (cp, cp_detail) in profile.slices(thread).iter().zip(profile.details(thread)) {
            let name = profile.str(cp.name);
            let inner = action_inner(name);
            let loc = actions.get(inner).or_else(|| actions.get(name));
            match loc {
                Some(&ActionLoc { lane, slice }) => {
                    let lane_slices = profile.slices(&threads[lane]);
                    let lane_details = profile.details(&threads[lane]);
                    let forest = containment_forest(lane_slices);
                    root.total_us += lane_slices[slice].dur_us;
                    flamer.merge_span(root, lane_slices, lane_details, &forest, slice);
                }
                None => {
                    root.total_us += cp.dur_us;
                    let seg = flamer.span_label(cp, cp_detail);
                    let node = root.child(seg);
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
/// widest-first, breaking ties by name handle so the layout is independent of the
/// non-deterministic input order. The walk interns no text — labels are already
/// handles into `text`, the flame's pool — and consumes the temporary builder
/// tree.
fn flatten(mut root: Builder, text: &str) -> Vec<FlameNode> {
    fn sort_tree(builder: &mut Builder, text: &str) {
        builder.children.sort_by(|a, b| {
            b.total_us
                .cmp(&a.total_us)
                .then_with(|| text[a.name.range()].cmp(&text[b.name.range()]))
        });
        for c in &mut builder.children {
            sort_tree(c, text);
        }
    }
    sort_tree(&mut root, text);

    let mut nodes = vec![freeze(&root, 0)];
    let mut queue: std::collections::VecDeque<(&Builder, usize, u32)> =
        std::collections::VecDeque::new();
    queue.push_back((&root, 0, 0));
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
        let g = build_flame_graph(&p, GroupMode::Mnemonic, Scope::All);
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
        let g = build_flame_graph(&p, GroupMode::Mnemonic, Scope::All);
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
        let g = build_flame_graph(&p, GroupMode::Mnemonic, Scope::All);
        assert_eq!(child(&g, g.root(), "CppCompile").total_us, 100);
        assert_eq!(child(&g, g.root(), "checkOutputs").total_us, 30);
    }

    #[test]
    fn work_class_mode_labels_each_span_by_class() {
        // Three disjoint top-level spans labelled by their work class.
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![
                action("CppCompile", 0, 100),
                action("Rustc", 100, 50),
                slice("fetch", "remote action execution", 200, 30),
            ],
        )]);
        let g = build_flame_graph(&p, GroupMode::WorkClass, Scope::All);
        // Two CPU actions; one remote.
        assert_eq!(child(&g, g.root(), "cpu").total_us, 150);
        assert_eq!(child(&g, g.root(), "remote").total_us, 30);
    }

    #[test]
    fn scope_selects_lanes_by_role() {
        let p = profile(vec![
            track(7, LaneClass::Analysis, vec![action("CppCompile", 0, 20)]),
            track(8, LaneClass::Execution, vec![action("Rustc", 0, 40)]),
            track(9, LaneClass::RepoFetch, vec![action("Fetch", 0, 5)]),
        ]);
        // Analysis: only the analysis lane.
        let analysis = build_flame_graph(&p, GroupMode::Mnemonic, Scope::Analysis);
        assert_eq!(analysis.root().total_us, 20);
        assert!(analysis
            .children(analysis.root())
            .iter()
            .all(|c| analysis.str(c.name) == "CppCompile"));
        // Execution: only the execution lane.
        let exec = build_flame_graph(&p, GroupMode::Mnemonic, Scope::Execution);
        assert_eq!(exec.root().total_us, 40);
        assert!(exec
            .children(exec.root())
            .iter()
            .all(|c| exec.str(c.name) == "Rustc"));
        // Loading: only the repo-fetch lane.
        let loading = build_flame_graph(&p, GroupMode::Mnemonic, Scope::Loading);
        assert_eq!(loading.root().total_us, 5);
        // All: every real lane.
        let all = build_flame_graph(&p, GroupMode::Mnemonic, Scope::All);
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
        let g = build_flame_graph(&p, GroupMode::Mnemonic, Scope::CriticalPath);
        assert_eq!(g.children(g.root()).len(), 1);
        let rustc = child(&g, g.root(), "Rustc");
        // Grafted from the worker lane: total is the worker action's 100, not the
        // critical component's 50, and the sub-span is present.
        assert_eq!(rustc.total_us, 100);
        assert_eq!(child(&g, rustc, "subprocess.run").total_us, 80);
    }

    #[test]
    fn critical_path_unmatched_component_is_a_leaf() {
        // A critical component with no matching action merges as a bare leaf.
        let cp = slice("Writing file out", "critical path component", 0, 40);
        let p = profile(vec![track(0, LaneClass::CriticalPath, vec![cp])]);
        let g = build_flame_graph(&p, GroupMode::Mnemonic, Scope::CriticalPath);
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
        let g = build_flame_graph(&p, GroupMode::Mnemonic, Scope::All);
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
        let ga = build_flame_graph(
            &profile(vec![track(1, LaneClass::Execution, ordered)]),
            GroupMode::Mnemonic,
            Scope::All,
        );
        let gb = build_flame_graph(
            &profile(vec![track(1, LaneClass::Execution, reversed)]),
            GroupMode::Mnemonic,
            Scope::All,
        );
        // The two graphs intern the same labels in the same merge order, so they
        // are byte-for-byte identical.
        assert_eq!(ga, gb);
    }

    #[test]
    fn layout_widths_are_proportional_and_bounded() {
        // root width 150: CppCompile=100, Rustc=50 -> widths 200, 100 of 300px.
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![action("CppCompile", 0, 100), action("Rustc", 100, 50)],
        )]);
        let g = build_flame_graph(&p, GroupMode::Mnemonic, Scope::All);
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
        let g = build_flame_graph(&p, GroupMode::Mnemonic, Scope::All);
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
        let g = build_flame_graph(&p, GroupMode::Mnemonic, Scope::All);
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
        let g = build_flame_graph(&p, GroupMode::Mnemonic, Scope::All);
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
        let g = build_flame_graph(&p, GroupMode::Mnemonic, Scope::All);
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
