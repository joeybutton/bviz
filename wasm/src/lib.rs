//! bviz's trace model, compiled to WebAssembly.
//!
//! The GPUI views of the desktop app do not run in a browser, but everything
//! beneath them does: these bindings parse a Bazel `--profile` with
//! `bviz_trace_profile`, place its lanes and cull its slices with
//! `bviz_trace_layout`, aggregate it into a flame graph with the same
//! [`ScopeAggregate`] the app's flame panel shows, and partition its wall time
//! along the critical path with `bviz_critical_path`. A page draws what they
//! return; it reimplements none of the model.
//!
//! Geometry crosses the boundary as packed `Float32Array`s, one record of
//! fixed width per box, so a frame costs one copy rather than one object per
//! slice. Everything else is JSON.

use std::sync::Arc;

use bviz_critical_path::CriticalPath;
use bviz_invocation::Recorded;
use bviz_trace_layout::{
    Cull, FlameBox, FlameBoxContent, FlameDirection, FlameLabels, FlameMetrics, FlameMode,
    FlamePlacement, FlameWeight, LaneLayout, LaneMetrics, ScopeAggregate, TimeSelection,
    TimeView, TrackFlamer, Viewport, Workload, flame_height, lay_out_flame, lay_out_lanes,
    span_between, visible_slices,
};
use bviz_trace_profile::{LaneClass, Profile, SpanRole};
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

/// Floats per box in [`Trace::visible`]: x, y, w, h, color, track, slice.
pub const SLICE_STRIDE: usize = 7;
/// Floats per box in [`Trace::flame_boxes`]: x, y, w, h, color, kind, index.
pub const FLAME_STRIDE: usize = 7;

/// A span's palette slot: its workload's position in [`WORKLOADS`] times two,
/// plus one where it is payload. The page holds the palette, so a theme switch
/// recolors without a call back in.
fn color(role: SpanRole) -> f32 {
    let workload = WORKLOADS.iter().position(|w| *w == role.workload).unwrap_or(7);
    (workload * 2 + usize::from(role.payload)) as f32
}

/// The workloads in palette order, matching `WORKLOAD_NAMES`.
const WORKLOADS: [Workload; 8] = [
    Workload::Subprocess,
    Workload::Starlark,
    Workload::Filesystem,
    Workload::RemoteCache,
    Workload::Scheduling,
    Workload::Skyframe,
    Workload::Gc,
    Workload::Other,
];
const WORKLOAD_NAMES: [&str; 8] = [
    "subprocess",
    "starlark",
    "filesystem",
    "remote_cache",
    "scheduling",
    "skyframe",
    "gc",
    "other",
];

fn us(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1e6
}

/// One loaded profile and the derived state the views share.
#[wasm_bindgen]
pub struct Trace {
    profile: Arc<Profile>,
    cut_off: bool,
    metrics: LaneMetrics,
    layout: LaneLayout,
    flamer: TrackFlamer,
    critical: Option<CriticalPath>,
    flame: Option<ScopeAggregate>,
    boxes: Vec<FlameBox>,
}

#[wasm_bindgen]
impl Trace {
    /// Parses a profile, gzipped or plain JSON, laying its lanes out with
    /// `header_height` px headers over `row_height` px slice rows.
    #[wasm_bindgen(constructor)]
    pub fn new(bytes: &[u8], header_height: f32, row_height: f32) -> Result<Trace, JsError> {
        let (profile, cut_off) = match Profile::from_bytes(bytes) {
            Ok(Recorded::Complete(p)) => (p, false),
            Ok(Recorded::CutOff(p)) => (p, true),
            Err(e) => return Err(JsError::new(&e.to_string())),
        };
        let profile = Arc::new(profile);
        let metrics = LaneMetrics {
            header_height,
            row_height,
            lane_gap: 2.0,
            counter_height: 0.0,
        };
        let layout = lay_out_lanes(&profile, &metrics);
        let critical = CriticalPath::of(&profile, None, None);
        Ok(Trace {
            flamer: TrackFlamer::new(&profile),
            profile,
            cut_off,
            metrics,
            layout,
            critical,
            flame: None,
            boxes: Vec::new(),
        })
    }

    /// The profile's metadata, time span, lanes as laid out, phase markers and
    /// critical-path partition, as JSON.
    pub fn summary(&self) -> String {
        let p = &self.profile;
        let m = p.metadata();
        let span = p.span();
        let lanes: Vec<Value> = self
            .layout
            .lanes
            .iter()
            .map(|lane| {
                let t = &p.threads()[lane.track];
                json!({
                    "track": lane.track,
                    "name": p.str(t.name),
                    "tid": t.tid,
                    "class": format!("{:?}", t.lane_class),
                    "top": lane.top,
                    "height": lane.height,
                    "rows": lane.depth_rows,
                    "slices": p.slices(t).len(),
                })
            })
            .collect();
        let markers: Vec<Value> = p
            .phase_markers()
            .iter()
            .map(|mk| json!({"name": p.str(mk.name), "start_us": mk.start_us}))
            .collect();
        json!({
            "bazel_version": p.opt_str(m.bazel_version),
            "build_id": p.opt_str(m.build_id),
            "start_us": span.start_us,
            "end_us": span.end_us,
            "cut_off": self.cut_off,
            "workloads": WORKLOAD_NAMES,
            "header_height": self.metrics.header_height,
            "row_height": self.metrics.row_height,
            "total_height": self.layout.total_height,
            "lanes": lanes,
            "markers": markers,
            "critical_path": self.critical.as_ref().map(critical_json),
        })
        .to_string()
    }

    /// The slices on screen for a timeline whose left edge is `start_us` at
    /// `px_per_us`, scrolled down `scroll_y` in a `width` × `height` viewport,
    /// packed [`SLICE_STRIDE`] floats per slice in paint order. Slices
    /// narrower than `min_px` merge into the boxes beside them, as bviz paints.
    pub fn visible(
        &self,
        start_us: f64,
        px_per_us: f64,
        width: f32,
        height: f32,
        scroll_y: f32,
        min_px: f32,
    ) -> Vec<f32> {
        let view = TimeView {
            start_us,
            px_per_us,
        };
        let viewport = Viewport {
            width,
            height,
            scroll_y,
        };
        let cull = Cull {
            view: &view,
            viewport: &viewport,
            metrics: &self.metrics,
            min_px,
        };
        let shown = visible_slices(&self.profile, &self.layout, cull);
        let mut out = Vec::with_capacity(shown.len() * SLICE_STRIDE);
        for v in shown {
            let t = &self.profile.threads()[v.track];
            let s = self.profile.slices(t)[v.slice];
            out.extend_from_slice(&[
                v.x,
                v.y,
                v.w,
                v.h,
                color(s.role),
                v.track as f32,
                v.slice as f32,
            ]);
        }
        out
    }

    /// The name a slice is labelled with.
    pub fn slice_name(&self, track: usize, slice: usize) -> String {
        let t = &self.profile.threads()[track];
        self.profile.str(self.profile.slices(t)[slice].name).to_string()
    }

    /// One slice's timing, category and Bazel args, as JSON.
    pub fn slice_info(&self, track: usize, slice: usize) -> String {
        let p = &self.profile;
        let t = &p.threads()[track];
        let s = p.slices(t)[slice];
        let d = p.details(t)[slice];
        let args: serde_json::Map<String, Value> = p
            .slice_others(&d)
            .iter()
            .map(|(k, v)| (p.str(*k).to_string(), Value::from(p.str(*v))))
            .collect();
        json!({
            "name": p.str(s.name),
            "lane": p.str(t.name),
            "category": p.str(d.category),
            "start_us": s.start_us,
            "dur_us": s.dur_us,
            "depth": s.depth,
            "workload": WORKLOAD_NAMES[color(s.role) as usize / 2],
            "payload": s.role.payload,
            "target": p.opt_str(d.target),
            "mnemonic": p.opt_str(d.mnemonic),
            "out": p.opt_str(d.out),
            "args": args,
        })
        .to_string()
    }

    /// Aggregates the flame graph over every lane but the critical path's, restricted to
    /// `[lo_us, hi_us)` unless the two are equal. `direction` is "top-down" or
    /// "bottom-up", `labels` "kind" or "name", `weight` "duration" or
    /// "wall". Returns the root's total in microseconds.
    pub fn build_flame(
        &mut self,
        direction: &str,
        labels: &str,
        weight: &str,
        lo_us: f64,
        hi_us: f64,
    ) -> f64 {
        let mode = FlameMode {
            direction: if direction == "bottom-up" {
                FlameDirection::BottomUp
            } else {
                FlameDirection::TopDown
            },
            labels: if labels == "name" {
                FlameLabels::Name
            } else {
                FlameLabels::Kind
            },
            weight: if weight == "wall" {
                FlameWeight::WallShare
            } else {
                FlameWeight::Duration
            },
        };
        let mut window = TimeSelection::default();
        if let Some(region) = span_between(lo_us as i64, hi_us as i64) {
            window.set(region);
        }
        // Every lane but the critical path's, which holds redirected copies of
        // worker time: bviz's own default selection, so nothing counts twice.
        let profile = &self.profile;
        let aggregate = ScopeAggregate::build_for_tracks_reusing(&self.flamer, mode, &window, |i| {
            profile.threads()[i].lane_class != LaneClass::CriticalPath
        });
        let total = aggregate.flame.root_total_us() as f64;
        self.flame = Some(aggregate);
        self.boxes.clear();
        total
    }

    /// The built flame graph's height in px at `row_height` px rows.
    pub fn flame_height(&self, row_height: f32) -> f32 {
        self.flame
            .as_ref()
            .map_or(0.0, |a| flame_height(&a.flame, &FlameMetrics { row_height }))
    }

    /// The built flame graph's boxes on a `view_w` px viewport over
    /// `content_w` px of graph scrolled to `scroll_x`, packed
    /// [`FLAME_STRIDE`] floats per box. Kind is 0 for a frame, 1 for a run of
    /// siblings too narrow to draw (bviz's remainder) and 2 for the rows
    /// beneath one; the index names the box for [`Trace::flame_box_info`].
    pub fn flame_boxes(
        &mut self,
        scroll_x: f64,
        view_w: f32,
        content_w: f64,
        row_height: f32,
        min_px: f32,
    ) -> Vec<f32> {
        let Some(aggregate) = &self.flame else {
            return Vec::new();
        };
        let metrics = FlameMetrics { row_height };
        self.boxes = lay_out_flame(
            &aggregate.flame,
            content_w,
            FlamePlacement {
                scroll_x,
                view_w,
                metrics: &metrics,
                min_px,
            },
        );
        let mut out = Vec::with_capacity(self.boxes.len() * FLAME_STRIDE);
        for (i, b) in self.boxes.iter().enumerate() {
            let (color, kind) = match b.content {
                FlameBoxContent::Frame(f) => (color(f.role), 0.0),
                FlameBoxContent::Remainder(r) => (r.role().map_or(-1.0, color), 1.0),
                FlameBoxContent::RemainderDepth(_) => (-1.0, 2.0),
            };
            out.extend_from_slice(&[b.x, b.y, b.w, b.h, color, kind, i as f32]);
        }
        out
    }

    /// The label, total and count of a box of the last [`Trace::flame_boxes`],
    /// as JSON.
    pub fn flame_box_info(&self, index: usize) -> String {
        let (Some(aggregate), Some(b)) = (&self.flame, self.boxes.get(index)) else {
            return "null".into();
        };
        let graph = &aggregate.flame;
        match b.content {
            FlameBoxContent::Frame(f) => json!({
                "name": if f.name.is_empty() && b.depth == 0 { "all" } else { graph.str(f.name) },
                "total_us": f.total_us,
                "count": f.count,
                "self_us": graph.span_self_us(f.node),
                "share": f.total_us as f64 / graph.root_total_us().max(1) as f64,
            }),
            FlameBoxContent::Remainder(r) | FlameBoxContent::RemainderDepth(r) => json!({
                "name": format!("({} others)", r.frames()),
                "total_us": graph.remainder_total_us(&r),
                "count": r.frames(),
                "share": graph.remainder_total_us(&r) as f64 / graph.root_total_us().max(1) as f64,
            }),
        }
        .to_string()
    }

    /// The built aggregate's stats table, widest self time first, as JSON.
    pub fn stats(&self, limit: usize) -> String {
        let Some(a) = &self.flame else {
            return "[]".into();
        };
        let rows: Vec<Value> = a
            .default_order
            .iter()
            .take(limit)
            .map(|&i| {
                let r = &a.stats[i as usize];
                json!({
                    "name": a.flame.str(r.key),
                    "workload": WORKLOAD_NAMES[color(r.role) as usize / 2],
                    "count": r.count,
                    "self_us": r.self_us,
                    "avg_us": r.avg_us(),
                    "max_us": r.max_us,
                    "share": r.self_us as f64 / a.grand_total.max(1) as f64,
                })
            })
            .collect();
        Value::from(rows).to_string()
    }
}

fn critical_json(cp: &CriticalPath) -> Value {
    let parts: serde_json::Map<String, Value> = bviz_critical_path::Part::ALL
        .iter()
        .map(|&part| (format!("{part:?}"), Value::from(us(cp.time_in(part)))))
        .collect();
    let steps: Vec<Value> = cp
        .steps()
        .iter()
        .map(|s| {
            json!({
                "name": s.name(),
                "mnemonic": s.mnemonic(),
                "target": s.target(),
                "start_us": s.start_us(),
                "gap_us": us(s.gap()),
                "own_us": us(s.own()),
            })
        })
        .collect();
    let levers: Vec<Value> = cp
        .levers()
        .into_iter()
        .map(|(lever, d)| json!({"lever": format!("{lever:?}"), "us": us(d)}))
        .collect();
    json!({
        "wall_us": us(cp.wall()),
        "path_us": us(cp.path()),
        "parts": parts,
        "steps": steps,
        "levers": levers,
    })
}
