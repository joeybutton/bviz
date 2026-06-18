use bviz_trace_profile::{Profile, Slice};

/// What slice attribute frames are merged by when aggregating the flame graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlameKey {
    /// The slice name (each distinct frame stays separate).
    Name,
    /// The action mnemonic, collapsing all actions of a kind together.
    Mnemonic,
    /// The slice category.
    Category,
}

impl FlameKey {
    /// The merge key for a slice: the attribute selected by `self`. A slice with
    /// no mnemonic (phases, evaluator frames) falls back to its name so it stays
    /// visible under `Mnemonic` keying rather than collapsing into one bucket.
    pub(crate) fn of<'a>(self, slice: &'a Slice) -> &'a str {
        match self {
            FlameKey::Name => &slice.name,
            FlameKey::Mnemonic => slice.args.mnemonic.as_deref().unwrap_or(&slice.name),
            FlameKey::Category => &slice.category,
        }
    }
}

/// One node of the aggregated flame graph: a frame merged across every place it
/// appears in the trace. `total_us` sums the merged occurrences' durations;
/// `count` is how many occurrences merged here. Children are merged by name and
/// sorted widest-first.
#[derive(Debug, Clone, PartialEq)]
pub struct FlameNode {
    pub name: String,
    pub category: String,
    pub total_us: i64,
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

    /// Graph fraction at a content-relative pixel.
    pub fn frac_of(&self, x_px: f32, width_px: f32) -> f64 {
        self.left + x_px as f64 / (self.zoom * width_px as f64)
    }

    /// Zoom by `factor` (>1 zooms in) holding the graph fraction under the
    /// content-relative pixel `x_px` fixed; zoom never drops below the fit.
    pub fn zoom_at(&mut self, x_px: f32, width_px: f32, factor: f64) {
        let anchor = self.frac_of(x_px, width_px);
        self.zoom = (self.zoom * factor).max(1.0);
        self.left = anchor - x_px as f64 / (self.zoom * width_px as f64);
        self.clamp();
    }

    /// Pan by a pixel delta; a positive `dx_px` moves content rightward.
    pub fn pan_px(&mut self, dx_px: f32, width_px: f32) {
        self.left -= dx_px as f64 / (self.zoom * width_px as f64);
        self.clamp();
    }

    /// Set the scroll position from a pixel offset into the laid-out width.
    pub fn set_scroll_px(&mut self, offset_px: f32, width_px: f32) {
        self.left = offset_px as f64 / (self.zoom * width_px as f64).max(1.0);
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
    pub category: String,
    pub total_us: i64,
    pub count: u32,
}

/// A node still under construction: children keyed by frame name for merging.
struct Builder {
    name: String,
    category: String,
    total_us: i64,
    count: u32,
    children: Vec<Builder>,
}

impl Builder {
    fn new(name: String, category: String) -> Self {
        Builder {
            name,
            category,
            total_us: 0,
            count: 0,
            children: Vec::new(),
        }
    }

    /// Find the child with this name, or append a fresh one. Category travels
    /// with the name; the first occurrence wins.
    fn child(&mut self, name: &str, category: &str) -> &mut Builder {
        let idx = match self.children.iter().position(|c| c.name == name) {
            Some(i) => i,
            None => {
                self.children
                    .push(Builder::new(name.to_owned(), category.to_owned()));
                self.children.len() - 1
            }
        };
        &mut self.children[idx]
    }

    /// Sort children widest-first, breaking ties by name so the layout is
    /// independent of the non-deterministic input order, then freeze to a node.
    fn finish(mut self, depth: u32) -> FlameNode {
        self.children.sort_by(|a, b| {
            b.total_us
                .cmp(&a.total_us)
                .then_with(|| a.name.cmp(&b.name))
        });
        let children = self
            .children
            .into_iter()
            .map(|c| c.finish(depth + 1))
            .collect();
        FlameNode {
            name: self.name,
            category: self.category,
            total_us: self.total_us,
            count: self.count,
            depth,
            children,
        }
    }
}

/// Merge a thread's slices into `root` as a call tree. Slices arrive sorted by
/// start then longest-duration-first (the parser's nesting order), so a slice's
/// parent is the deepest still-open ancestor on the stack.
fn merge_thread(root: &mut Builder, slices: &[Slice], key: FlameKey) {
    // Stack of (merge key, occurrence end-time) for each open ancestor, indexed
    // by depth; the builder for an open frame is reached by its key from the root.
    let mut stack: Vec<(String, i64)> = Vec::new();
    for s in slices {
        while stack.last().is_some_and(|&(_, end)| end <= s.start_us) {
            stack.pop();
        }
        let parent = stack
            .iter()
            .fold(&mut *root, |node, (k, _)| node.child(k, ""));
        let k = key.of(s);
        let node = parent.child(k, &s.category);
        node.total_us += s.dur_us;
        node.count += 1;
        stack.push((k.to_owned(), s.start_us + s.dur_us));
    }
}

/// Build the aggregated flame graph for a profile: a synthetic root whose
/// children are the merged top-level frames. `scope` of `Some(i)` aggregates
/// only `profile.threads[i]` (the root is labeled by that thread); `None`
/// aggregates every thread (the root is labeled "All"). The root's `total_us`
/// sums all top-level slice durations and its `count` is the number of
/// top-level occurrences across the included threads.
pub fn build_flame_graph(profile: &Profile, key: FlameKey, scope: Option<usize>) -> FlameNode {
    let root_name = match scope {
        Some(i) => profile.threads[i].name.clone(),
        None => "All".to_owned(),
    };
    let mut root = Builder::new(root_name, String::new());
    let threads = match scope {
        Some(i) => &profile.threads[i..=i],
        None => &profile.threads[..],
    };
    for thread in threads {
        merge_thread(&mut root, &thread.slices, key);
    }
    root.total_us = root.children.iter().map(|c| c.total_us).sum();
    root.count = root.children.iter().map(|c| c.count).sum();
    root.finish(0)
}

/// Lay the tree into a content rectangle of the given width. The root spans the
/// full width; each node's children are placed left-to-right with width
/// proportional to `total_us`, so the right-hand remainder is the parent's self
/// time. Depth maps to y via `row_height`. Boxes narrower than `min_px` are
/// omitted but still consume their horizontal share so siblings stay placed.
/// Boxes come back in paint order, parents before children.
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
            category: node.category.clone(),
            total_us: node.total_us,
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

    fn slice(name: &str, start_us: i64, dur_us: i64, depth: u32) -> Slice {
        Slice {
            name: name.to_owned(),
            category: "cat".to_owned(),
            start_us,
            dur_us,
            depth,
            args: SliceArgs::default(),
        }
    }

    /// A slice in category `cat` carrying an action mnemonic.
    fn action(name: &str, mnemonic: &str, start_us: i64, dur_us: i64, depth: u32) -> Slice {
        Slice {
            args: SliceArgs {
                mnemonic: Some(mnemonic.to_owned()),
                ..SliceArgs::default()
            },
            ..slice(name, start_us, dur_us, depth)
        }
    }

    /// A slice in an explicit category, so category keying can be exercised.
    fn categorized(name: &str, category: &str, start_us: i64, dur_us: i64, depth: u32) -> Slice {
        Slice {
            category: category.to_owned(),
            ..slice(name, start_us, dur_us, depth)
        }
    }

    fn track(tid: i64, slices: Vec<Slice>) -> ThreadTrack {
        ThreadTrack {
            tid,
            name: format!("t{tid}"),
            sort_index: tid,
            slices,
            max_depth: 0,
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
    fn merges_identical_frames_across_threads() {
        let p = profile(vec![
            track(1, vec![slice("a", 0, 100, 0)]),
            track(2, vec![slice("a", 0, 50, 0)]),
        ]);
        let root = build_flame_graph(&p, FlameKey::Name, None);
        assert_eq!(root.name, "All");
        assert_eq!(root.children.len(), 1);
        let a = &root.children[0];
        assert_eq!(a.name, "a");
        assert_eq!(a.total_us, 150);
        assert_eq!(a.count, 2);
        assert_eq!(root.total_us, 150);
        assert_eq!(root.count, 2);
    }

    #[test]
    fn scoping_to_one_thread_excludes_the_others() {
        let p = profile(vec![
            track(1, vec![slice("a", 0, 100, 0)]),
            track(2, vec![slice("b", 0, 50, 0)]),
        ]);
        let root = build_flame_graph(&p, FlameKey::Name, Some(1));
        // Scoped to thread index 1: only its slice "b", root labeled by it.
        assert_eq!(root.name, "t2");
        assert_eq!(root.children.len(), 1);
        assert_eq!(root.children[0].name, "b");
        assert_eq!(root.total_us, 50);
    }

    #[test]
    fn distinct_top_level_frames_stay_separate() {
        let p = profile(vec![track(
            1,
            vec![slice("a", 0, 100, 0), slice("b", 100, 40, 0)],
        )]);
        let root = build_flame_graph(&p, FlameKey::Name, None);
        assert_eq!(root.children.len(), 2);
        // Widest first: a (100) before b (40).
        assert_eq!(root.children[0].name, "a");
        assert_eq!(root.children[1].name, "b");
        assert_eq!(root.total_us, 140);
        assert_eq!(root.count, 2);
    }

    #[test]
    fn mnemonic_keying_merges_actions_of_a_kind() {
        // Two differently-named CppCompile actions and one slice with no
        // mnemonic; by mnemonic the two actions merge and the bare slice keys by
        // its name.
        let p = profile(vec![track(
            1,
            vec![
                action("compile foo.cc", "CppCompile", 0, 100, 0),
                action("compile bar.cc", "CppCompile", 100, 50, 0),
                slice("evaluate", 200, 30, 0),
            ],
        )]);
        let root = build_flame_graph(&p, FlameKey::Mnemonic, None);
        assert_eq!(root.children.len(), 2);
        let compile = child(&root, "CppCompile");
        assert_eq!(compile.total_us, 150);
        assert_eq!(compile.count, 2);
        let evaluate = child(&root, "evaluate");
        assert_eq!(evaluate.total_us, 30);
        assert_eq!(evaluate.count, 1);
    }

    #[test]
    fn category_keying_merges_by_category() {
        // Distinct names in shared categories merge by category.
        let p = profile(vec![track(
            1,
            vec![
                categorized("a", "action", 0, 100, 0),
                categorized("b", "action", 100, 40, 0),
                categorized("c", "skyframe", 200, 60, 0),
            ],
        )]);
        let root = build_flame_graph(&p, FlameKey::Category, None);
        assert_eq!(root.children.len(), 2);
        let action = child(&root, "action");
        assert_eq!(action.total_us, 140);
        assert_eq!(action.count, 2);
        assert_eq!(action.category, "action");
        assert_eq!(child(&root, "skyframe").total_us, 60);
    }

    #[test]
    fn reconstructs_nesting_by_time_containment() {
        // a [0,100) contains b [0,40) and c [40,90); b contains d [0,20).
        let p = profile(vec![track(
            1,
            vec![
                slice("a", 0, 100, 0),
                slice("b", 0, 40, 1),
                slice("d", 0, 20, 2),
                slice("c", 40, 50, 1),
            ],
        )]);
        let root = build_flame_graph(&p, FlameKey::Name, None);
        let a = child(&root, "a");
        assert_eq!(a.depth, 1);
        assert_eq!(a.children.len(), 2);
        let b = child(a, "b");
        let c = child(a, "c");
        assert_eq!(b.depth, 2);
        assert_eq!(c.depth, 2);
        let d = child(b, "d");
        assert_eq!(d.depth, 3);
        assert_eq!(d.total_us, 20);
        assert!(c.children.is_empty());
    }

    #[test]
    fn sibling_occurrences_merge_under_their_parent() {
        // Two calls to a, each calling b once: a.count=2, a's b.count=2.
        let p = profile(vec![track(
            1,
            vec![
                slice("a", 0, 100, 0),
                slice("b", 0, 30, 1),
                slice("a", 100, 100, 0),
                slice("b", 100, 30, 1),
            ],
        )]);
        let root = build_flame_graph(&p, FlameKey::Name, None);
        let a = child(&root, "a");
        assert_eq!(a.count, 2);
        assert_eq!(a.total_us, 200);
        let b = child(a, "b");
        assert_eq!(b.count, 2);
        assert_eq!(b.total_us, 60);
    }

    #[test]
    fn build_is_independent_of_slice_order() {
        let ordered = vec![
            slice("a", 0, 100, 0),
            slice("b", 0, 40, 1),
            slice("c", 40, 50, 1),
            slice("z", 200, 90, 0),
        ];
        let mut shuffled = ordered.clone();
        shuffled.reverse();
        // The parser sorts each thread's slices; mirror that before building so
        // nesting is reconstructed, then check the merged tree matches.
        let mut a_slices = ordered.clone();
        let mut b_slices = shuffled;
        a_slices.sort_by(|x, y| x.start_us.cmp(&y.start_us).then(y.dur_us.cmp(&x.dur_us)));
        b_slices.sort_by(|x, y| x.start_us.cmp(&y.start_us).then(y.dur_us.cmp(&x.dur_us)));
        let ra = build_flame_graph(&profile(vec![track(1, a_slices)]), FlameKey::Name, None);
        let rb = build_flame_graph(&profile(vec![track(1, b_slices)]), FlameKey::Name, None);
        assert_eq!(ra, rb);
    }

    #[test]
    fn layout_widths_are_proportional_and_bounded() {
        // root total 150: a=100, b=50 -> widths 200, 100 of a 300px span.
        let p = profile(vec![track(
            1,
            vec![slice("a", 0, 100, 0), slice("b", 100, 50, 0)],
        )]);
        let root = build_flame_graph(&p, FlameKey::Name, None);
        let boxes = lay_out_flame(&root, 300.0, &METRICS, 0.0);
        let root_box = &boxes[0];
        assert_eq!(root_box.w, 300.0);
        assert_eq!(root_box.y, 0.0);
        let a = boxes.iter().find(|b| b.name == "a").unwrap();
        let b = boxes.iter().find(|b| b.name == "b").unwrap();
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
            vec![slice("wide", 0, 990, 0), slice("thin", 990, 10, 0)],
        )]);
        let root = build_flame_graph(&p, FlameKey::Name, None);
        // 1000us over 100px: thin is 1px wide, below a 5px floor.
        let boxes = lay_out_flame(&root, 100.0, &METRICS, 5.0);
        assert!(boxes.iter().any(|b| b.name == "wide"));
        assert!(!boxes.iter().any(|b| b.name == "thin"));
        let wide = boxes.iter().find(|b| b.name == "wide").unwrap();
        // Wide keeps its proportional slot; thin's omission does not move it.
        assert_eq!(wide.x, 0.0);
        assert!((wide.w - 99.0).abs() < 1e-3);
    }

    #[test]
    fn height_counts_one_row_per_depth() {
        let p = profile(vec![track(
            1,
            vec![slice("a", 0, 100, 0), slice("b", 0, 40, 1)],
        )]);
        let root = build_flame_graph(&p, FlameKey::Name, None);
        // root depth 0, a depth 1, b depth 2 -> 3 rows.
        assert_eq!(flame_height(&root, &METRICS), 30.0);
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
