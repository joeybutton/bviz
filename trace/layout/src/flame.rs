use bviz_trace_profile::{Profile, Slice};

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
fn merge_thread(root: &mut Builder, slices: &[Slice]) {
    // Stack of (frame name, occurrence end-time) for each open ancestor, indexed
    // by depth; the builder for an open frame is reached by name from the root.
    let mut stack: Vec<(String, i64)> = Vec::new();
    for s in slices {
        while stack.last().is_some_and(|&(_, end)| end <= s.start_us) {
            stack.pop();
        }
        let parent = stack
            .iter()
            .fold(&mut *root, |node, (name, _)| node.child(name, ""));
        let node = parent.child(&s.name, &s.category);
        node.total_us += s.dur_us;
        node.count += 1;
        stack.push((s.name.clone(), s.start_us + s.dur_us));
    }
}

/// Build the aggregated flame graph for a whole profile: a synthetic root whose
/// children are the merged top-level frames of every thread. The root's
/// `total_us` sums all top-level slice durations and its `count` is the number
/// of top-level occurrences across all threads.
pub fn build_flame_graph(profile: &Profile) -> FlameNode {
    let mut root = Builder::new("All".to_owned(), String::new());
    for thread in &profile.threads {
        merge_thread(&mut root, &thread.slices);
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
        let root = build_flame_graph(&p);
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
    fn distinct_top_level_frames_stay_separate() {
        let p = profile(vec![track(
            1,
            vec![slice("a", 0, 100, 0), slice("b", 100, 40, 0)],
        )]);
        let root = build_flame_graph(&p);
        assert_eq!(root.children.len(), 2);
        // Widest first: a (100) before b (40).
        assert_eq!(root.children[0].name, "a");
        assert_eq!(root.children[1].name, "b");
        assert_eq!(root.total_us, 140);
        assert_eq!(root.count, 2);
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
        let root = build_flame_graph(&p);
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
        let root = build_flame_graph(&p);
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
        let ra = build_flame_graph(&profile(vec![track(1, a_slices)]));
        let rb = build_flame_graph(&profile(vec![track(1, b_slices)]));
        assert_eq!(ra, rb);
    }

    #[test]
    fn layout_widths_are_proportional_and_bounded() {
        // root total 150: a=100, b=50 -> widths 200, 100 of a 300px span.
        let p = profile(vec![track(
            1,
            vec![slice("a", 0, 100, 0), slice("b", 100, 50, 0)],
        )]);
        let root = build_flame_graph(&p);
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
        let root = build_flame_graph(&p);
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
        let root = build_flame_graph(&p);
        // root depth 0, a depth 1, b depth 2 -> 3 rows.
        assert_eq!(flame_height(&root, &METRICS), 30.0);
    }
}
