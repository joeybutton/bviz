use bviz_trace_profile::{phase_intervals, LaneClass, Profile, Slice, WorkClass};

/// How a slice's contribution is split into a path of frames when aggregating
/// the flame graph. Multi-segment modes nest the second key under the first
/// (root→leaf).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupMode {
    /// Mnemonic-or-bucket only.
    Mnemonic,
    /// Phase → mnemonic-or-bucket.
    PhaseMnemonic,
    /// Work class only.
    WorkClass,
    /// Work class → mnemonic-or-bucket.
    ClassMnemonic,
    /// Mnemonic-or-bucket → target.
    Target,
}

/// Which thread lanes an aggregation draws from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Only the synthetic critical-path lane (tid 0).
    CriticalPath,
    /// Worker lanes — the actions actually executed.
    Execution,
    /// The main thread and skyframe evaluators — analysis-phase work.
    Analysis,
    /// Every real lane. Excludes the critical-path lane, whose slices are
    /// redirected copies of worker time; counting both would double-count.
    All,
}

impl Scope {
    /// Whether a thread of this lane class is included in the aggregation.
    pub fn includes(self, lane: LaneClass) -> bool {
        match self {
            Scope::CriticalPath => lane == LaneClass::CriticalPath,
            Scope::Execution => lane == LaneClass::Worker,
            Scope::Analysis => matches!(lane, LaneClass::Main | LaneClass::SkyframeEvaluator),
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

/// The mnemonic-or-bucket label for a slice: its action mnemonic if present,
/// else a parenthesized work-class bucket so non-action frames (phases,
/// evaluator, locks) collapse by class rather than scattering by name.
fn mnemonic_or_bucket(slice: &Slice) -> Segment {
    let label = match &slice.args.mnemonic {
        Some(m) => m.clone(),
        None => match slice.work_class {
            WorkClass::Cpu => "(cpu)",
            WorkClass::Remote => "(remote)",
            WorkClass::Io => "(io)",
            WorkClass::Wait => "(wait)",
            WorkClass::Gc => "(gc)",
            WorkClass::Meta => "(meta)",
        }
        .to_owned(),
    };
    Segment {
        label,
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

/// One node of the aggregated flame graph: a frame merged across every place it
/// appears in the trace by semantic-path identity. `total_us` is inclusive (own
/// time plus all descendants'); `self_us` is the own time
/// (`total_us − Σ children.total_us`); `count` is how many slice occurrences
/// merged at this node. Children are merged by `(label, work_class)` and sorted
/// widest-first.
#[derive(Debug, Clone, PartialEq)]
pub struct FlameNode {
    pub name: String,
    pub total_us: i64,
    pub self_us: i64,
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
    /// independent of the non-deterministic input order, then freeze to a node,
    /// computing self time as the own total minus the children's totals.
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
        let children_us: i64 = children.iter().map(|c| c.total_us).sum();
        FlameNode {
            name: self.name,
            total_us: self.total_us,
            self_us: self.total_us - children_us,
            work_class: self.work_class,
            count: self.count,
            depth,
            children,
        }
    }
}

/// The semantic-path segments a slice contributes under `mode`. `phase_of`
/// resolves the slice's start to a phase name (only consulted by phase modes).
fn segments(slice: &Slice, mode: GroupMode, phase_of: impl Fn(i64) -> String) -> Vec<Segment> {
    let class = Segment {
        label: work_class_label(slice.work_class).to_owned(),
        work_class: slice.work_class,
    };
    match mode {
        GroupMode::Mnemonic => vec![mnemonic_or_bucket(slice)],
        GroupMode::PhaseMnemonic => vec![
            Segment {
                label: phase_of(slice.start_us),
                work_class: WorkClass::Meta,
            },
            mnemonic_or_bucket(slice),
        ],
        GroupMode::WorkClass => vec![class],
        GroupMode::ClassMnemonic => vec![class, mnemonic_or_bucket(slice)],
        GroupMode::Target => vec![
            mnemonic_or_bucket(slice),
            Segment {
                label: slice
                    .args
                    .target
                    .clone()
                    .unwrap_or_else(|| "(no target)".to_owned()),
                work_class: slice.work_class,
            },
        ],
    }
}

/// The leaf segment of a slice's `mode` path — the label a flat per-leaf table
/// groups it under, with the work class to color the row. Phase modes never
/// reach this (the table groups by the action leaf), so phase resolution is
/// unneeded here.
pub(crate) fn leaf_label(slice: &Slice, mode: GroupMode) -> (String, WorkClass) {
    let seg = match mode {
        GroupMode::WorkClass => Segment {
            label: work_class_label(slice.work_class).to_owned(),
            work_class: slice.work_class,
        },
        GroupMode::Target => Segment {
            label: slice
                .args
                .target
                .clone()
                .unwrap_or_else(|| "(no target)".to_owned()),
            work_class: slice.work_class,
        },
        GroupMode::Mnemonic | GroupMode::PhaseMnemonic | GroupMode::ClassMnemonic => {
            mnemonic_or_bucket(slice)
        }
    };
    (seg.label, seg.work_class)
}

/// Build the aggregated flame graph for a profile by semantic-stack identity:
/// each in-scope slice contributes its duration along a path of `mode` key
/// segments, and identical paths merge into one frame. The synthetic root "All"
/// accumulates the duration of every in-scope slice, so children always sum to
/// at most their parent and self time is non-negative by construction.
pub fn build_flame_graph(profile: &Profile, mode: GroupMode, scope: Scope) -> FlameNode {
    let intervals = match mode {
        GroupMode::PhaseMnemonic => phase_intervals(profile),
        _ => Vec::new(),
    };
    let phase_of = |start_us: i64| -> String {
        intervals
            .iter()
            .find(|&&(_, s, e)| start_us >= s && start_us < e)
            .map_or_else(|| "(no phase)".to_owned(), |(name, _, _)| name.clone())
    };

    let mut root = Builder::new("All".to_owned(), WorkClass::Meta);
    for thread in &profile.threads {
        if !scope.includes(thread.lane_class) {
            continue;
        }
        for s in &thread.slices {
            root.total_us += s.dur_us;
            let mut node = &mut root;
            let segs = segments(s, mode, phase_of);
            for seg in &segs {
                node = node.child(seg);
                node.total_us += s.dur_us;
            }
            node.count += 1;
        }
    }
    root.finish(0)
}

/// Lay the tree into a content rectangle of the given width. The root spans the
/// full width; each node's children are placed left-to-right with width
/// proportional to `total_us`, so the right-hand remainder is the parent's self
/// time (zero when children fill the parent). Depth maps to y via `row_height`.
/// Boxes narrower than `min_px` are omitted but still consume their horizontal
/// share so siblings stay placed. Boxes come back in paint order, parents before
/// children.
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
    if node.total_us == 0 {
        return;
    }
    let mut cursor = x;
    for child in &node.children {
        let cw = w * child.total_us as f32 / node.total_us as f32;
        place(child, cursor, cw, metrics, min_px, out);
        cursor += cw;
    }
}

/// The box at `(target_depth, x_px)` in a layout of `root` into `width` pixels,
/// found by descending the tree the same way [`lay_out_flame`] places boxes —
/// children left-to-right proportional to `total_us` in their stored order — so
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
        if node.total_us == 0 {
            return None;
        }
        // Descend into the child whose proportional x-extent contains x_px,
        // accumulating its origin and width exactly as `place` does.
        let mut cursor = x;
        let mut next = None;
        for child in &node.children {
            let cw = w * child.total_us as f32 / node.total_us as f32;
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
    use bviz_trace_profile::{Metadata, PhaseMarker, SliceArgs, ThreadTrack, TimeSpan};

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
    fn rustc_merges_into_one_node_regardless_of_position() {
        // Identical "Rustc" actions scattered across threads, depths, and
        // overlapping times all collapse into a single frame under the root —
        // the headline fix over time-containment, which split them apart.
        let p = profile(vec![
            track(
                7,
                LaneClass::Worker,
                vec![action("Rustc", 0, 100), action("Rustc", 50, 80)],
            ),
            track(8, LaneClass::Worker, vec![action("Rustc", 0, 60)]),
        ]);
        let root = build_flame_graph(&p, GroupMode::Mnemonic, Scope::All);
        assert_eq!(root.name, "All");
        assert_eq!(root.children.len(), 1);
        let rustc = &root.children[0];
        assert_eq!(rustc.name, "Rustc");
        assert_eq!(rustc.total_us, 240);
        assert_eq!(rustc.count, 3);
        assert_eq!(rustc.work_class, WorkClass::Cpu);
        assert_eq!(root.total_us, 240);
    }

    #[test]
    fn mnemonicless_slice_buckets_by_work_class() {
        // A slice with no mnemonic buckets under its work class, not its name.
        let p = profile(vec![track(
            1,
            LaneClass::Worker,
            vec![
                action("CppCompile", 0, 100),
                slice("borrowing a worker", "borrowing a worker", 100, 30, 0),
            ],
        )]);
        let root = build_flame_graph(&p, GroupMode::Mnemonic, Scope::All);
        assert_eq!(child(&root, "CppCompile").total_us, 100);
        let wait = child(&root, "(wait)");
        assert_eq!(wait.total_us, 30);
        assert_eq!(wait.work_class, WorkClass::Wait);
    }

    #[test]
    fn phase_mnemonic_nests_mnemonic_under_its_phase() {
        let mut p = profile(vec![track(
            1,
            LaneClass::Worker,
            vec![action("CppCompile", 10, 50), action("Rustc", 120, 40)],
        )]);
        p.phase_markers = vec![
            PhaseMarker {
                name: "analyze".to_owned(),
                start_us: 0,
            },
            PhaseMarker {
                name: "execute".to_owned(),
                start_us: 100,
            },
        ];
        let root = build_flame_graph(&p, GroupMode::PhaseMnemonic, Scope::All);
        let analyze = child(&root, "analyze");
        assert_eq!(analyze.work_class, WorkClass::Meta);
        assert_eq!(child(analyze, "CppCompile").total_us, 50);
        let execute = child(&root, "execute");
        assert_eq!(child(execute, "Rustc").total_us, 40);
    }

    #[test]
    fn work_class_mode_sums_per_class_and_class_mnemonic_nests() {
        let p = profile(vec![track(
            1,
            LaneClass::Worker,
            vec![
                action("CppCompile", 0, 100),
                action("Rustc", 100, 50),
                slice("remote", "remote action execution", 200, 30, 0),
            ],
        )]);
        let by_class = build_flame_graph(&p, GroupMode::WorkClass, Scope::All);
        assert_eq!(child(&by_class, "cpu").total_us, 150);
        assert_eq!(child(&by_class, "remote").total_us, 30);
        let nested = build_flame_graph(&p, GroupMode::ClassMnemonic, Scope::All);
        let cpu = child(&nested, "cpu");
        assert_eq!(cpu.total_us, 150);
        assert_eq!(child(cpu, "CppCompile").total_us, 100);
        assert_eq!(child(cpu, "Rustc").total_us, 50);
    }

    #[test]
    fn scope_selects_lanes() {
        let p = profile(vec![
            track(0, LaneClass::CriticalPath, vec![action("Rustc", 0, 10)]),
            track(7, LaneClass::Main, vec![action("CppCompile", 0, 20)]),
            track(8, LaneClass::Worker, vec![action("Rustc", 0, 40)]),
        ]);
        // CriticalPath: only the tid:0 lane.
        let crit = build_flame_graph(&p, GroupMode::Mnemonic, Scope::CriticalPath);
        assert_eq!(crit.total_us, 10);
        // Execution: only workers, excluding Main.
        let exec = build_flame_graph(&p, GroupMode::Mnemonic, Scope::Execution);
        assert_eq!(exec.total_us, 40);
        assert!(exec.children.iter().all(|c| c.name == "Rustc"));
        // All: every real lane but the critical path (10 omitted).
        let all = build_flame_graph(&p, GroupMode::Mnemonic, Scope::All);
        assert_eq!(all.total_us, 60);
    }

    #[test]
    fn self_us_is_total_minus_children_and_children_fit() {
        // ClassMnemonic: cpu (150) holds CppCompile (100) + Rustc (50), so the
        // cpu node's self time is zero and the root's self time is its own.
        let p = profile(vec![track(
            1,
            LaneClass::Worker,
            vec![action("CppCompile", 0, 100), action("Rustc", 100, 50)],
        )]);
        let root = build_flame_graph(&p, GroupMode::ClassMnemonic, Scope::All);
        let cpu = child(&root, "cpu");
        let children_sum: i64 = cpu.children.iter().map(|c| c.total_us).sum();
        assert_eq!(cpu.self_us, cpu.total_us - children_sum);
        assert_eq!(cpu.self_us, 0);
        assert!(children_sum <= cpu.total_us);
        assert_eq!(root.self_us, root.total_us - cpu.total_us);
        assert_eq!(root.self_us, 0);
    }

    #[test]
    fn build_is_independent_of_slice_order() {
        let ordered = vec![
            action("CppCompile", 0, 100),
            action("Rustc", 100, 40),
            action("CppCompile", 200, 50),
            slice("wait", "thread wait", 300, 90, 0),
        ];
        let mut reversed = ordered.clone();
        reversed.reverse();
        let ra = build_flame_graph(
            &profile(vec![track(1, LaneClass::Worker, ordered)]),
            GroupMode::ClassMnemonic,
            Scope::All,
        );
        let rb = build_flame_graph(
            &profile(vec![track(1, LaneClass::Worker, reversed)]),
            GroupMode::ClassMnemonic,
            Scope::All,
        );
        assert_eq!(ra, rb);
    }

    #[test]
    fn layout_widths_are_proportional_and_bounded() {
        // root total 150: CppCompile=100, Rustc=50 -> widths 200, 100 of 300px.
        let p = profile(vec![track(
            1,
            LaneClass::Worker,
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
            LaneClass::Worker,
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
    fn children_never_exceed_their_parent() {
        // Semantic-path aggregation can't produce a child wider than its parent:
        // each slice adds the same duration to the leaf and every ancestor, so a
        // node's total is at least the sum of its children's totals.
        let p = profile(vec![track(
            1,
            LaneClass::Worker,
            vec![action("CppCompile", 0, 100), action("Rustc", 100, 50)],
        )]);
        let root = build_flame_graph(&p, GroupMode::ClassMnemonic, Scope::All);
        let cpu = child(&root, "cpu");
        let children_sum: i64 = cpu.children.iter().map(|c| c.total_us).sum();
        assert!(children_sum <= cpu.total_us);
        let boxes = lay_out_flame(&root, 300.0, &METRICS, 0.0);
        let cpu_box = boxes.iter().find(|b| b.name == "cpu").unwrap();
        for c in &cpu.children {
            let cb = boxes.iter().find(|b| b.name == c.name).unwrap();
            assert!(cb.w <= cpu_box.w + f32::EPSILON);
            assert!(cb.x >= cpu_box.x - f32::EPSILON);
            assert!(cb.x + cb.w <= cpu_box.x + cpu_box.w + 1e-3);
        }
    }

    #[test]
    fn height_counts_one_row_per_depth() {
        let p = profile(vec![track(
            1,
            LaneClass::Worker,
            vec![action("CppCompile", 0, 100)],
        )]);
        // ClassMnemonic: root depth 0, cpu depth 1, CppCompile depth 2 -> 3 rows.
        let root = build_flame_graph(&p, GroupMode::ClassMnemonic, Scope::All);
        assert_eq!(flame_height(&root, &METRICS), 30.0);
    }

    #[test]
    fn flame_box_at_agrees_with_lay_out_flame() {
        // A two-level tree (ClassMnemonic) so the descent is exercised across
        // depths and several classes.
        let p = profile(vec![track(
            1,
            LaneClass::Worker,
            vec![
                action("CppCompile", 0, 100),
                action("Rustc", 100, 40),
                action("CppCompile", 200, 50),
                slice("remote", "remote action execution", 300, 90, 0),
                slice("wait", "thread wait", 400, 30, 0),
            ],
        )]);
        let root = build_flame_graph(&p, GroupMode::ClassMnemonic, Scope::All);
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
            LaneClass::Worker,
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
