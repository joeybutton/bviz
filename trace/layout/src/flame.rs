use std::collections::HashMap;

use bviz_trace_profile::{LaneClass, Profile, Slice, WorkClass};

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

/// One key segment of a slice's aggregation path: a frame label and the work
/// class it carries (for coloring and class-keyed merging).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Segment {
    label: String,
    work_class: WorkClass,
}

/// A map from a mnemonic-bearing slice's name to its mnemonic, over the whole
/// profile. The "action processing" slices carry both the action description as
/// their name and the mnemonic in their args, so this recovers the mnemonic for
/// any other slice that names the same action — notably critical-path
/// components, whose name is `action '<desc>'` and which carry no mnemonic of
/// their own.
pub(crate) fn mnemonic_index(profile: &Profile) -> HashMap<&str, &str> {
    let mut idx = HashMap::new();
    for thread in &profile.threads {
        for s in &thread.slices {
            if let Some(m) = &s.args.mnemonic {
                idx.insert(s.name.as_str(), m.as_str());
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
fn resolve_mnemonic<'a>(slice: &'a Slice, idx: &HashMap<&'a str, &'a str>) -> Option<&'a str> {
    if let Some(m) = &slice.args.mnemonic {
        return Some(m);
    }
    idx.get(slice.name.as_str())
        .copied()
        .or_else(|| idx.get(action_inner(&slice.name)).copied())
}

/// The finest detail label for a slice: its resolved action mnemonic if it names
/// an action (so all Rustc collapse together), else its own name (the
/// informative label for non-action slices — "checkOutputs", "subprocess.run",
/// a repo target, …).
fn detail(slice: &Slice, idx: &HashMap<&str, &str>) -> Segment {
    let label = resolve_mnemonic(slice, idx)
        .map(str::to_owned)
        .unwrap_or_else(|| slice.name.clone());
    Segment {
        label,
        work_class: slice.work_class,
    }
}

/// The activity-type label for a slice: its Bazel `ProfilerTask` category (the
/// `cat` description — "Fetching repository", "package creation", "general
/// information", "action processing", …).
fn category_seg(slice: &Slice) -> Segment {
    Segment {
        label: slice.category.clone(),
        work_class: slice.work_class,
    }
}

/// The work-class segment for a slice (the class itself, not a parenthesized
/// bucket).
fn class_seg(slice: &Slice) -> Segment {
    Segment {
        label: work_class_label(slice.work_class).to_owned(),
        work_class: slice.work_class,
    }
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
/// children inside their parent. Children are merged by `(label, work_class)` and
/// sorted widest-first.
#[derive(Debug, Clone, PartialEq)]
pub struct FlameNode {
    pub name: String,
    pub total_us: i64,
    pub self_us: i64,
    pub width_us: i64,
    pub work_class: WorkClass,
    pub count: u32,
    pub depth: u32,
    pub children: Vec<FlameNode>,
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

/// A flame-graph node laid out for painting. Field copies of the node identify
/// it for hit-testing and tooltips after the source tree is dropped.
#[derive(Debug, Clone, PartialEq)]
pub struct FlameBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub depth: u32,
    pub name: String,
    pub total_us: i64,
    pub self_us: i64,
    pub work_class: WorkClass,
    pub count: u32,
}

/// A node still under construction: children keyed by `(label, work_class)` for
/// merging by semantic-path identity.
struct Builder {
    name: String,
    work_class: WorkClass,
    total_us: i64,
    count: u32,
    children: Vec<Builder>,
}

impl Builder {
    fn new(name: String, work_class: WorkClass) -> Self {
        Builder {
            name,
            work_class,
            total_us: 0,
            count: 0,
            children: Vec::new(),
        }
    }

    /// Find the child matching this segment, or append a fresh one. The work
    /// class travels with the label; the first occurrence wins.
    fn child(&mut self, seg: &Segment) -> &mut Builder {
        let idx = match self
            .children
            .iter()
            .position(|c| c.name == seg.label && c.work_class == seg.work_class)
        {
            Some(i) => i,
            None => {
                self.children
                    .push(Builder::new(seg.label.clone(), seg.work_class));
                self.children.len() - 1
            }
        };
        &mut self.children[idx]
    }

    /// Sort children widest-first, breaking ties by name so the layout is
    /// independent of the non-deterministic input order, then freeze to a node.
    /// Self time is the own total minus the children's totals, floored at zero
    /// since an overflowing child can exceed the parent's total; layout width is
    /// the larger of the own total and the children's widths so children fit.
    fn finish(mut self, depth: u32) -> FlameNode {
        self.children.sort_by(|a, b| {
            b.total_us
                .cmp(&a.total_us)
                .then_with(|| a.name.cmp(&b.name))
        });
        let children: Vec<FlameNode> = self
            .children
            .into_iter()
            .map(|c| c.finish(depth + 1))
            .collect();
        let children_total: i64 = children.iter().map(|c| c.total_us).sum();
        let children_width: i64 = children.iter().map(|c| c.width_us).sum();
        FlameNode {
            name: self.name,
            total_us: self.total_us,
            self_us: (self.total_us - children_total).max(0),
            width_us: self.total_us.max(children_width),
            work_class: self.work_class,
            count: self.count,
            depth,
            children,
        }
    }
}

/// The label a span carries under `mode`, using the name index to recover action
/// mnemonics. The tree shape comes from time-containment, not from this label;
/// the mode only names each span so identical-named spans merge into one frame.
fn span_label(slice: &Slice, idx: &HashMap<&str, &str>, mode: GroupMode) -> Segment {
    match mode {
        GroupMode::Mnemonic => detail(slice, idx),
        GroupMode::Category => category_seg(slice),
        GroupMode::WorkClass => class_seg(slice),
    }
}

/// The single key a flat per-mode stats row groups a slice under — the mode's
/// primary dimension: detail for `Mnemonic`, the category for `Category`, the
/// work class for `WorkClass`.
pub(crate) fn leaf_label(
    slice: &Slice,
    idx: &HashMap<&str, &str>,
    mode: GroupMode,
) -> (String, WorkClass) {
    let seg = span_label(slice, idx, mode);
    (seg.label, seg.work_class)
}

/// End time of a slice.
fn end_us(slice: &Slice) -> i64 {
    slice.start_us + slice.dur_us
}

/// The time-containment forest of a lane's slices. `order` lists the slice
/// indices sorted by start then longest-duration-first (the parser's nesting
/// order, recomputed so the result is independent of the input vector's order);
/// `children[i]` lists the slices directly nested in slice `i`; `roots` lists the
/// top-level slices (no container). Built with a depth-stack over `order`: for
/// each slice, pop while the stack top ends at or before its start, the remaining
/// top (if any) is its parent, then push it. A slice that starts inside its
/// parent but ends after it stays a child — the overflow case handled by
/// `width_us`.
struct Forest {
    children: Vec<Vec<usize>>,
    roots: Vec<usize>,
}

fn containment_forest(slices: &[Slice]) -> Forest {
    let mut order: Vec<usize> = (0..slices.len()).collect();
    order.sort_by(|&a, &b| {
        slices[a]
            .start_us
            .cmp(&slices[b].start_us)
            .then_with(|| slices[b].dur_us.cmp(&slices[a].dur_us))
    });
    let mut children = vec![Vec::new(); slices.len()];
    let mut roots = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for &i in &order {
        while stack
            .last()
            .is_some_and(|&top| end_us(&slices[top]) <= slices[i].start_us)
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

/// Merge span `span_idx` of `lane_slices` and its time-children into `parent`:
/// the span's duration lands once at its own labelled node, then each child
/// recurses under that node. So a node merging N spans sums their durations, and
/// a child's duration is counted at the child, not again at the parent.
fn merge_span(
    parent: &mut Builder,
    lane_slices: &[Slice],
    forest: &Forest,
    span_idx: usize,
    idx: &HashMap<&str, &str>,
    mode: GroupMode,
) {
    let seg = span_label(&lane_slices[span_idx], idx, mode);
    let node = parent.child(&seg);
    node.total_us += lane_slices[span_idx].dur_us;
    node.count += 1;
    for &c in &forest.children[span_idx] {
        merge_span(node, lane_slices, forest, c, idx, mode);
    }
}

/// Build the aggregated flame graph for a profile by time-containment plus
/// semantic merge: within each in-scope lane every span nests under the span that
/// time-contains it, and identical-`mode`-labelled spans at the same place merge
/// into one frame whose duration is the sum of the merged spans' own durations.
/// A parent span's Bazel duration already includes its children's, so counting
/// each span once at its own node avoids the double-count of summing a span and
/// its sub-spans as siblings.
pub fn build_flame_graph(profile: &Profile, mode: GroupMode, scope: Scope) -> FlameNode {
    let idx = mnemonic_index(profile);
    let mut root = Builder::new("All".to_owned(), WorkClass::Meta);

    for thread in &profile.threads {
        if !scope.includes(thread.lane_class) {
            continue;
        }
        let forest = containment_forest(&thread.slices);
        for &r in &forest.roots {
            root.total_us += thread.slices[r].dur_us;
            merge_span(&mut root, &thread.slices, &forest, r, &idx, mode);
        }
    }
    root.finish(0)
}

/// Lay the tree into a content rectangle of the given width. The root spans the
/// full width; each node's children are placed left-to-right with width
/// proportional to `width_us`, so the right-hand remainder is the parent's self
/// time (zero when children fill the parent). Dividing by `width_us` keeps an
/// overflowing child inside its parent. Depth maps to y via `row_height`. Boxes
/// narrower than `min_px` are omitted but still consume their horizontal share so
/// siblings stay placed. Boxes come back in paint order, parents before children.
pub fn lay_out_flame(
    root: &FlameNode,
    width: f32,
    metrics: &FlameMetrics,
    min_px: f32,
) -> Vec<FlameBox> {
    let mut out = Vec::new();
    place(root, 0.0, width, metrics, min_px, &mut out);
    out
}

fn place(
    node: &FlameNode,
    x: f32,
    w: f32,
    metrics: &FlameMetrics,
    min_px: f32,
    out: &mut Vec<FlameBox>,
) {
    if w >= min_px {
        out.push(FlameBox {
            x,
            y: node.depth as f32 * metrics.row_height,
            w,
            h: metrics.row_height,
            depth: node.depth,
            name: node.name.clone(),
            total_us: node.total_us,
            self_us: node.self_us,
            work_class: node.work_class,
            count: node.count,
        });
    }
    if node.width_us == 0 {
        return;
    }
    let mut cursor = x;
    for child in &node.children {
        let cw = w * child.width_us as f32 / node.width_us as f32;
        place(child, cursor, cw, metrics, min_px, out);
        cursor += cw;
    }
}

/// The box at `(target_depth, x_px)` in a layout of `root` into `width` pixels,
/// found by descending the tree the same way [`lay_out_flame`] places boxes —
/// children left-to-right proportional to `width_us` in their stored order — so
/// the result agrees with the corresponding `lay_out_flame` box without building
/// the full box vector. Returns `None` when `x_px` is outside `[0, width)`, no
/// node sits at that depth under it, or the matching box is narrower than
/// `min_px` (and so `lay_out_flame` would omit it).
pub fn flame_box_at(
    root: &FlameNode,
    x_px: f32,
    target_depth: u32,
    width: f32,
    metrics: &FlameMetrics,
    min_px: f32,
) -> Option<FlameBox> {
    if x_px < 0.0 || x_px >= width {
        return None;
    }
    let mut node = root;
    let mut x = 0.0_f32;
    let mut w = width;
    loop {
        if node.depth == target_depth {
            if w < min_px {
                return None;
            }
            return Some(FlameBox {
                x,
                y: node.depth as f32 * metrics.row_height,
                w,
                h: metrics.row_height,
                depth: node.depth,
                name: node.name.clone(),
                total_us: node.total_us,
                self_us: node.self_us,
                work_class: node.work_class,
                count: node.count,
            });
        }
        if node.width_us == 0 {
            return None;
        }
        // Descend into the child whose proportional x-extent contains x_px,
        // accumulating its origin and width exactly as `place` does.
        let mut cursor = x;
        let mut next = None;
        for child in &node.children {
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

/// Total painted height: one row per depth level the tree reaches.
pub fn flame_height(root: &FlameNode, metrics: &FlameMetrics) -> f32 {
    (max_depth(root) + 1) as f32 * metrics.row_height
}

fn max_depth(node: &FlameNode) -> u32 {
    node.children
        .iter()
        .map(max_depth)
        .max()
        .map_or(node.depth, |d| d.max(node.depth))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_trace_profile::{Metadata, SliceArgs, ThreadTrack, TimeSpan};

    /// A slice whose work class is taken from its `category` via `work_class`.
    fn slice(name: &str, category: &str, start_us: i64, dur_us: i64, depth: u32) -> Slice {
        Slice {
            name: name.to_owned(),
            category: category.to_owned(),
            start_us,
            dur_us,
            depth,
            args: SliceArgs::default(),
            work_class: bviz_trace_profile::work_class(category),
        }
    }

    /// A CPU-class slice carrying an action mnemonic.
    fn action(mnemonic: &str, start_us: i64, dur_us: i64) -> Slice {
        Slice {
            args: SliceArgs {
                mnemonic: Some(mnemonic.to_owned()),
                ..SliceArgs::default()
            },
            ..slice(mnemonic, "local action execution", start_us, dur_us, 0)
        }
    }

    fn track(tid: i64, lane_class: LaneClass, slices: Vec<Slice>) -> ThreadTrack {
        ThreadTrack {
            tid,
            name: format!("t{tid}"),
            sort_index: tid,
            slices,
            max_depth: 0,
            lane_class,
        }
    }

    fn profile(threads: Vec<ThreadTrack>) -> Profile {
        Profile {
            metadata: Metadata::default(),
            threads,
            counters: Vec::new(),
            phase_markers: Vec::new(),
            span: TimeSpan {
                start_us: 0,
                end_us: 1000,
            },
        }
    }

    fn child<'a>(node: &'a FlameNode, name: &str) -> &'a FlameNode {
        node.children
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no child {name}"))
    }

    const METRICS: FlameMetrics = FlameMetrics { row_height: 10.0 };

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
                slice("Action.execute", "general information", 0, 90, 0),
                slice("subprocess.run", "subprocess run", 0, 80, 0),
            ],
        )]);
        let root = build_flame_graph(&p, GroupMode::Mnemonic, Scope::All);
        assert_eq!(root.total_us, 100);
        let rustc = child(&root, "Rustc");
        assert_eq!(rustc.total_us, 100);
        assert_eq!(rustc.self_us, 10);
        let exec = child(rustc, "Action.execute");
        assert_eq!(exec.total_us, 90);
        let sub = child(exec, "subprocess.run");
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
                slice("Action.execute", "general information", start, 80, 0),
            ]
        };
        let p = profile(vec![
            track(8, LaneClass::Execution, lane(0)),
            track(9, LaneClass::Execution, lane(0)),
        ]);
        let root = build_flame_graph(&p, GroupMode::Mnemonic, Scope::All);
        assert_eq!(root.children.len(), 1);
        let rustc = child(&root, "Rustc");
        assert_eq!(rustc.total_us, 200);
        assert_eq!(rustc.count, 2);
        let exec = child(rustc, "Action.execute");
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
                slice("checkOutputs", "general information", 100, 30, 0),
            ],
        )]);
        let root = build_flame_graph(&p, GroupMode::Mnemonic, Scope::All);
        assert_eq!(child(&root, "CppCompile").total_us, 100);
        assert_eq!(child(&root, "checkOutputs").total_us, 30);
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
                slice("fetch", "remote action execution", 200, 30, 0),
            ],
        )]);
        let root = build_flame_graph(&p, GroupMode::WorkClass, Scope::All);
        // Two CPU actions; one remote.
        assert_eq!(child(&root, "cpu").total_us, 150);
        assert_eq!(child(&root, "remote").total_us, 30);
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
        assert_eq!(analysis.total_us, 20);
        assert!(analysis.children.iter().all(|c| c.name == "CppCompile"));
        // Execution: only the execution lane.
        let exec = build_flame_graph(&p, GroupMode::Mnemonic, Scope::Execution);
        assert_eq!(exec.total_us, 40);
        assert!(exec.children.iter().all(|c| c.name == "Rustc"));
        // Loading: only the repo-fetch lane.
        let loading = build_flame_graph(&p, GroupMode::Mnemonic, Scope::Loading);
        assert_eq!(loading.total_us, 5);
        // All: every real lane.
        let all = build_flame_graph(&p, GroupMode::Mnemonic, Scope::All);
        assert_eq!(all.total_us, 65);
    }

    #[test]
    fn overflowing_child_still_fits_in_layout() {
        // A child that starts inside its parent but ends after it (Bazel's
        // catch-all frames): width_us is the max, self_us floors at zero, and the
        // child stays within the parent's box.
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![
                action("Outer", 0, 50),
                slice("inner", "general information", 10, 100, 0),
            ],
        )]);
        let root = build_flame_graph(&p, GroupMode::Mnemonic, Scope::All);
        let outer = child(&root, "Outer");
        let inner = child(outer, "inner");
        assert_eq!(outer.total_us, 50);
        assert_eq!(inner.total_us, 100);
        // Child total exceeds parent total; width and self time stay sane.
        assert_eq!(outer.width_us, 100);
        assert_eq!(outer.self_us, 0);
        assert!(inner.self_us >= 0);
        let boxes = lay_out_flame(&root, 300.0, &METRICS, 0.0);
        let ob = boxes.iter().find(|b| b.name == "Outer").unwrap();
        let ib = boxes.iter().find(|b| b.name == "inner").unwrap();
        assert!(ib.w <= ob.w + f32::EPSILON);
        assert!(ib.x >= ob.x - f32::EPSILON);
        assert!(ib.x + ib.w <= ob.x + ob.w + 1e-3);
    }

    #[test]
    fn build_is_independent_of_slice_order() {
        // Disjoint top-level spans, so order only affects the input vector; the
        // forest sorts internally and the merge is commutative.
        let ordered = vec![
            action("CppCompile", 0, 100),
            action("Rustc", 100, 40),
            action("CppCompile", 200, 50),
            slice("wait", "thread wait", 300, 90, 0),
        ];
        let mut reversed = ordered.clone();
        reversed.reverse();
        let ra = build_flame_graph(
            &profile(vec![track(1, LaneClass::Execution, ordered)]),
            GroupMode::Mnemonic,
            Scope::All,
        );
        let rb = build_flame_graph(
            &profile(vec![track(1, LaneClass::Execution, reversed)]),
            GroupMode::Mnemonic,
            Scope::All,
        );
        assert_eq!(ra, rb);
    }

    #[test]
    fn layout_widths_are_proportional_and_bounded() {
        // root width 150: CppCompile=100, Rustc=50 -> widths 200, 100 of 300px.
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![action("CppCompile", 0, 100), action("Rustc", 100, 50)],
        )]);
        let root = build_flame_graph(&p, GroupMode::Mnemonic, Scope::All);
        let boxes = lay_out_flame(&root, 300.0, &METRICS, 0.0);
        let root_box = &boxes[0];
        assert_eq!(root_box.w, 300.0);
        assert_eq!(root_box.y, 0.0);
        let a = boxes.iter().find(|b| b.name == "CppCompile").unwrap();
        let b = boxes.iter().find(|b| b.name == "Rustc").unwrap();
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
        let root = build_flame_graph(&p, GroupMode::Mnemonic, Scope::All);
        // 1000us over 100px: Thin is 1px wide, below a 5px floor.
        let boxes = lay_out_flame(&root, 100.0, &METRICS, 5.0);
        assert!(boxes.iter().any(|b| b.name == "Wide"));
        assert!(!boxes.iter().any(|b| b.name == "Thin"));
        let wide = boxes.iter().find(|b| b.name == "Wide").unwrap();
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
                slice("Action.execute", "general information", 0, 90, 0),
            ],
        )]);
        let root = build_flame_graph(&p, GroupMode::Mnemonic, Scope::All);
        assert_eq!(flame_height(&root, &METRICS), 30.0);
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
                slice("Action.execute", "general information", 0, 90, 0),
                action("Rustc", 100, 80),
                slice("subprocess.run", "subprocess run", 100, 60, 0),
            ],
        )]);
        let root = build_flame_graph(&p, GroupMode::Mnemonic, Scope::All);
        let width = 600.0;
        let min_px = 1.0;
        let boxes = lay_out_flame(&root, width, &METRICS, min_px);
        // For every laid-out box, a probe at its center maps back to it.
        for b in &boxes {
            let probe_x = b.x + b.w / 2.0;
            let hit = flame_box_at(&root, probe_x, b.depth, width, &METRICS, min_px);
            assert_eq!(hit.as_ref(), Some(b), "probe at depth {} x {probe_x}", b.depth);
        }
        // Outside the content width yields nothing.
        assert_eq!(flame_box_at(&root, -1.0, 0, width, &METRICS, min_px), None);
        assert_eq!(flame_box_at(&root, width, 0, width, &METRICS, min_px), None);
        // A depth past the tree under a real x yields nothing.
        let deep = max_depth(&root) + 1;
        assert_eq!(flame_box_at(&root, 1.0, deep, width, &METRICS, min_px), None);
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
        let root = build_flame_graph(&p, GroupMode::Mnemonic, Scope::All);
        let width = 100.0;
        let min_px = 5.0;
        // Thin sits at depth 1, x in [99, 100); probe its center.
        assert_eq!(flame_box_at(&root, 99.5, 1, width, &METRICS, min_px), None);
        // Its Wide sibling at the same depth is found.
        let wide = lay_out_flame(&root, width, &METRICS, min_px)
            .into_iter()
            .find(|b| b.name == "Wide")
            .unwrap();
        assert_eq!(
            flame_box_at(&root, 1.0, 1, width, &METRICS, min_px),
            Some(wide)
        );
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

