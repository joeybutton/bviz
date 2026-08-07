use crate::groups::{GROUP_ORDER, GroupChrome, build_grouped_layout, default_collapsed_groups};
use crate::rebuild::Rebuild;
use crate::{METRICS, Source};
use bviz_trace_layout::{
    FlameMode, LaneLayout, ScopeAggregate, TimeSelection, TrackFlamer, lay_out_lanes,
};
use bviz_trace_profile::{LaneClass, Profile};
use gpui::{Context, EventEmitter};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

/// A background aggregate build started for the current selection. Carried as an
/// event rather than a notification because nothing the model exposes has
/// changed yet — the previously built aggregate stays the shown one — so it
/// concerns only a panel that signals the rebuild in progress. A notification
/// would instead reach every observer, whose follow-up is written for a *replaced*
/// aggregate: dropping memoized layouts and re-sorting for a graph that has not
/// moved.
pub(crate) struct AggregateBuildStarted;

/// The shared trace document and the selection and visibility state derived from
/// it, plus the layouts and aggregates cached from those. Held by
/// [`crate::TraceView`] as an entity so every panel reads one source of truth; a
/// panel's own view state stays on the panel.
pub(crate) struct TraceModel {
    source: Source,
    /// The full thread/counter lane layout for the loaded profile, built on first
    /// use. It depends only on the immutable profile, so it is cleared only when a
    /// new profile loads.
    ///
    /// This and the two layouts derived from it are the model's lazy caches, and
    /// the cell around each is what keeps their readers read-only: every hit-test
    /// and paint asks for a layout through a shared `&TraceModel`, several of them
    /// from inside a `read` of a panel that holds the model, where no `&mut` to
    /// the model can be taken. Filling on demand through the cell keeps
    /// [`Self::lane_layout`] callable from those paths; the alternative is either
    /// an exclusive borrow threaded through every read helper or building the
    /// layouts eagerly on each visibility change, which would move the cost of
    /// [`lay_out_lanes`] onto the load path.
    lanes: RefCell<Option<Rc<LaneLayout>>>,
    /// Which tracks the flame chart draws, indexed by `profile.threads()` order.
    visible_tracks: Vec<bool>,
    /// Which tracks the flame graph and stats aggregate over, indexed likewise.
    selected_tracks: Vec<bool>,
    /// The profile time the flame graph and stats aggregate over, intersected
    /// with the selected tracks; empty for the whole timeline.
    time_selection: TimeSelection,
    /// Which lane-class groups are collapsed in the chart, indexed by
    /// [`GROUP_ORDER`]. Loading starts collapsed — it holds one lane per external
    /// repository, hundreds on a large build.
    collapsed_groups: [bool; GROUP_ORDER.len()],
    /// Which groups show every lane rather than the busiest, indexed by
    /// [`GROUP_ORDER`].
    group_show_all: [bool; GROUP_ORDER.len()],
    flame_mode: FlameMode,
    /// Bumped whenever an input the aggregate is built from changes — the track
    /// selection, the time selection, or the flame mode — keying the build so a
    /// stale build's result is discarded once the inputs have moved on.
    aggregate_key: u64,
    /// The track a shift-click range-selection extends from, if any.
    select_anchor: Option<usize>,
    /// The lane layout restricted to the visible tracks and restacked into
    /// lane-class groups, derived from `lanes` and cleared whenever the
    /// visibility, collapse, or show-all state changes.
    visible_lanes: RefCell<Option<Rc<LaneLayout>>>,
    /// The group headers and show-more rows over `visible_lanes`, built and
    /// cleared alongside it.
    group_chrome: RefCell<Option<Rc<GroupChrome>>>,
    /// The selected tracks' aggregate — its flame graph and stats rows, held
    /// together so the rows' and laid-out boxes' name handles resolve against
    /// the graph that produced them. Built on demand off the UI thread (walking
    /// every slice would freeze a paint) and swapped in when ready; the previous
    /// value stays shown while a rebuild runs, so a selection change doesn't
    /// blank the view.
    aggregates: Option<Arc<ScopeAggregate>>,
    /// The loaded profile's whole-profile flame name scan, reused across track
    /// selections; `None` until the first aggregate build scans it. Valid for any
    /// selection, so it is dropped only when a new profile loads.
    flame_scan: Option<Arc<TrackFlamer>>,
    /// Background-build bookkeeping for `aggregates`, keyed on
    /// `aggregate_key`.
    agg: Rebuild<u64>,
}

impl EventEmitter<AggregateBuildStarted> for TraceModel {}

impl TraceModel {
    pub(crate) fn new() -> Self {
        Self {
            source: Source::Loading,
            lanes: RefCell::new(None),
            visible_tracks: Vec::new(),
            selected_tracks: Vec::new(),
            time_selection: TimeSelection::default(),
            collapsed_groups: default_collapsed_groups(),
            group_show_all: [false; GROUP_ORDER.len()],
            flame_mode: FlameMode::default(),
            aggregate_key: 0,
            select_anchor: None,
            visible_lanes: RefCell::new(None),
            group_chrome: RefCell::new(None),
            aggregates: None,
            flame_scan: None,
            agg: Rebuild::new(),
        }
    }

    pub(crate) fn source(&self) -> &Source {
        &self.source
    }

    pub(crate) fn visible_tracks(&self) -> &[bool] {
        &self.visible_tracks
    }

    pub(crate) fn selected_tracks(&self) -> &[bool] {
        &self.selected_tracks
    }

    pub(crate) fn time_selection(&self) -> &TimeSelection {
        &self.time_selection
    }

    /// Restricts the aggregate to `selection`, reporting whether that differs
    /// from the standing one. Keys a rebuild only for a set that actually
    /// changed, so re-committing the same regions costs nothing.
    pub(crate) fn set_time_selection(
        &mut self,
        selection: TimeSelection,
        cx: &mut Context<Self>,
    ) -> bool {
        if selection == self.time_selection {
            return false;
        }
        self.time_selection = selection;
        self.aggregate_key += 1;
        self.ensure_aggregates(cx);
        true
    }

    /// Replaces the loaded document, dropping the lane layouts (which depend only
    /// on the profile) and the time selection (whose instants address the old
    /// profile's timeline) and, for a loaded profile, seeding the default track
    /// selection.
    pub(crate) fn set_source(&mut self, source: Source, cx: &mut Context<Self>) {
        self.lanes.borrow_mut().take();
        self.visible_lanes.borrow_mut().take();
        self.time_selection.clear();
        self.source = source;
        if let Source::Loaded(profile) = &self.source {
            let profile = profile.clone();
            self.init_tracks(&profile, cx);
        }
    }

    /// The default track selection for a freshly loaded profile: every
    /// non-critical-path lane selected. The critical-path lane holds redirected
    /// copies of worker time, so leaving it out of the default selection keeps
    /// the aggregate from double-counting that time.
    pub(crate) fn init_tracks(&mut self, profile: &Profile, cx: &mut Context<Self>) {
        self.visible_tracks = vec![true; profile.threads().len()];
        self.collapsed_groups = default_collapsed_groups();
        self.group_show_all = [false; GROUP_ORDER.len()];
        self.selected_tracks = profile
            .threads()
            .iter()
            .map(|t| t.lane_class != LaneClass::CriticalPath)
            .collect();
        self.select_anchor = None;
        self.aggregate_key += 1;
        self.ensure_aggregates(cx);
    }

    /// The full lane layout for the loaded profile, built on first use and reused
    /// across paints and mouse handlers. `None` until a profile is loaded.
    fn full_lane_layout(&self) -> Option<Rc<LaneLayout>> {
        let Source::Loaded(profile) = &self.source else {
            return None;
        };
        if self.lanes.borrow().is_none() {
            let t = std::time::Instant::now();
            let layout = lay_out_lanes(profile, &METRICS);
            if std::env::var_os("BVIZ_TRACE_TIMING").is_some() {
                eprintln!("[timing] lay_out_lanes: {:.3}s", t.elapsed().as_secs_f64());
            }
            *self.lanes.borrow_mut() = Some(Rc::new(layout));
        }
        self.lanes.borrow().clone()
    }

    /// The lane layout the flame chart draws: the full layout restricted to the
    /// visible tracks and restacked into lane-class groups, cached until the
    /// visibility, collapse, or show-all state changes. Every paint and hit-test
    /// works from this, so a lane hidden by its group is neither drawn nor
    /// selectable.
    pub(crate) fn lane_layout(&self) -> Option<Rc<LaneLayout>> {
        self.ensure_grouped();
        self.visible_lanes.borrow().clone()
    }

    /// The group headers and show-more rows over [`Self::lane_layout`].
    pub(crate) fn group_chrome(&self) -> Option<Rc<GroupChrome>> {
        self.ensure_grouped();
        self.group_chrome.borrow().clone()
    }

    /// Builds and caches the grouped lane layout and its chrome if stale.
    fn ensure_grouped(&self) {
        if self.visible_lanes.borrow().is_some() {
            return;
        }
        let Some(full) = self.full_lane_layout() else {
            return;
        };
        let Source::Loaded(profile) = &self.source else {
            return;
        };
        let (layout, chrome) = build_grouped_layout(
            profile,
            &full,
            &self.visible_tracks,
            &self.collapsed_groups,
            &self.group_show_all,
        );
        *self.visible_lanes.borrow_mut() = Some(Rc::new(layout));
        *self.group_chrome.borrow_mut() = Some(Rc::new(chrome));
    }

    /// Drops the cached visible layout and chrome so they rebuild and restack.
    fn invalidate_visible_lanes(&mut self) {
        self.visible_lanes.borrow_mut().take();
        self.group_chrome.borrow_mut().take();
    }

    /// Toggles a lane-class group's collapse and rebuilds the grouped layout.
    pub(crate) fn toggle_group_collapsed(&mut self, group: usize) {
        if let Some(c) = self.collapsed_groups.get_mut(group) {
            *c = !*c;
            self.invalidate_visible_lanes();
        }
    }

    /// Toggles whether a group shows every lane or only the busiest.
    pub(crate) fn toggle_group_show_all(&mut self, group: usize) {
        if let Some(s) = self.group_show_all.get_mut(group) {
            *s = !*s;
            self.invalidate_visible_lanes();
        }
    }

    /// The number of visible tracks and the total.
    pub(crate) fn visible_count(&self) -> (usize, usize) {
        let visible = self.visible_tracks.iter().filter(|v| **v).count();
        (visible, self.visible_tracks.len())
    }

    /// Toggles whether track `i` is drawn in the flame chart, dropping the cached
    /// visible layout so it restacks.
    pub(crate) fn toggle_track(&mut self, i: usize) {
        if let Some(v) = self.visible_tracks.get_mut(i) {
            *v = !*v;
            self.invalidate_visible_lanes();
        }
    }

    /// Shows or hides every track at once.
    pub(crate) fn set_all_visible(&mut self, visible: bool) {
        for v in &mut self.visible_tracks {
            *v = visible;
        }
        self.invalidate_visible_lanes();
    }

    /// Shows or hides every track in one lane-class group at once.
    pub(crate) fn set_group_visible(&mut self, group: usize, visible: bool) {
        let class = GROUP_ORDER[group];
        let targets: Vec<usize> = {
            let Source::Loaded(profile) = &self.source else {
                return;
            };
            profile
                .threads()
                .iter()
                .enumerate()
                .filter(|(_, t)| t.lane_class == class)
                .map(|(i, _)| i)
                .collect()
        };
        for i in targets {
            if let Some(v) = self.visible_tracks.get_mut(i) {
                *v = visible;
            }
        }
        self.invalidate_visible_lanes();
    }

    /// Selects `track` for the flame graph and stats, with file-manager gesture
    /// semantics. A plain click selects it alone; a `ctrl` (or cmd) click flips it
    /// in the current set; a `shift` click replaces the selection with the run of
    /// visible lanes from the anchor to `track`; a `shift`+`ctrl` click sets that
    /// run to the anchor's own state, extending a just-added run or clearing a
    /// just-removed one. A plain or ctrl click moves the anchor to `track`; a
    /// shift click leaves it, so the run can be re-adjusted. Keys a rebuild only
    /// when the set actually changes, so a repeat click is free.
    pub(crate) fn select_track(
        &mut self,
        track: usize,
        shift: bool,
        ctrl: bool,
        cx: &mut Context<Self>,
    ) {
        let n = self.selected_tracks.len();
        if track >= n {
            return;
        }
        // The anchor run and the anchor's own selected state, read before any
        // mutation so the range extends from the existing anchor.
        let run = self.visible_run_to(track);
        let anchor_state = self.select_anchor.and_then(|a| self.selected_tracks.get(a).copied());
        let sel = match (shift, ctrl, run) {
            // Plain click: select the one track and anchor there.
            (false, false, _) => {
                let mut sel = vec![false; n];
                sel[track] = true;
                self.select_anchor = Some(track);
                sel
            }
            // Ctrl-click: flip the one track, keeping the rest, and re-anchor.
            (false, true, _) => {
                let mut sel = self.selected_tracks.clone();
                sel[track] = !sel[track];
                self.select_anchor = Some(track);
                sel
            }
            // Shift-click: replace the selection with the anchor run.
            (true, false, Some(run)) => {
                let mut sel = vec![false; n];
                for t in run {
                    sel[t] = true;
                }
                sel
            }
            // Shift-Ctrl-click: set the anchor run to the anchor's state,
            // extending a just-added run or clearing a just-removed one.
            (true, true, Some(run)) => {
                let state = anchor_state.unwrap_or(true);
                let mut sel = self.selected_tracks.clone();
                for t in run {
                    sel[t] = state;
                }
                sel
            }
            // A shift click with no anchor (or an anchor no longer visible) falls
            // back to selecting the one track.
            (true, _, None) => {
                let mut sel = vec![false; n];
                sel[track] = true;
                self.select_anchor = Some(track);
                sel
            }
        };
        if sel != self.selected_tracks {
            self.selected_tracks = sel;
            self.aggregate_key += 1;
        }
        self.ensure_aggregates(cx);
    }

    pub(crate) fn flame_mode(&self) -> FlameMode {
        self.flame_mode
    }

    /// Puts the flame graph under `mode` and kicks off the rebuild it needs.
    /// Reports what [`Self::adopt_flame_mode`] did, so a caller can skip the work
    /// that only a mode change calls for.
    pub(crate) fn set_flame_mode(&mut self, mode: FlameMode, cx: &mut Context<Self>) -> bool {
        if !self.adopt_flame_mode(mode) {
            return false;
        }
        // The mode controls render from the mode itself, so their selected state
        // must not wait for the rebuild to land.
        cx.notify();
        self.ensure_aggregates(cx);
        true
    }

    /// Adopts `mode` and keys a rebuild for it, or reports false when it is
    /// already the standing mode, so re-choosing one costs nothing.
    fn adopt_flame_mode(&mut self, mode: FlameMode) -> bool {
        if self.flame_mode == mode {
            return false;
        }
        self.flame_mode = mode;
        self.aggregate_key += 1;
        true
    }

    /// Sets the selection to every visible track (the Ctrl-A gesture), returning
    /// whether the set changed.
    pub(crate) fn select_all_visible(&mut self, cx: &mut Context<Self>) -> bool {
        let changed = if self.selected_tracks != self.visible_tracks {
            self.selected_tracks = self.visible_tracks.clone();
            self.aggregate_key += 1;
            true
        } else {
            false
        };
        self.ensure_aggregates(cx);
        changed
    }

    /// The aggregated flame graph and stats for the current track selection, or
    /// `None` until the first background build for the loaded profile completes.
    /// Reflects the previous selection while a rebuild is still running;
    /// [`Self::ensure_aggregates`] owns the (re)build.
    pub(crate) fn aggregates(&self) -> Option<Arc<ScopeAggregate>> {
        self.aggregates.clone()
    }

    /// The `aggregate_key` the ready aggregate reflects, if any; the stats
    /// panel keys its sort order on it.
    pub(crate) fn aggregate_version(&self) -> Option<u64> {
        self.agg.ready()
    }

    /// Whether a background aggregate build is currently running.
    pub(crate) fn aggregate_building(&self) -> bool {
        self.agg.is_building()
    }

    /// Makes the current selection's aggregate the shown one: built on demand
    /// off the UI thread (walking every slice is far too costly for a paint) and
    /// swapped in on completion, the previously built graph staying shown until
    /// then. An in-flight build whose selection the inputs have moved past is
    /// abandoned. Idempotent — a no-op once the right value is ready or being
    /// computed. Emits [`AggregateBuildStarted`] when it spawns a build and
    /// notifies when one lands, so a panel showing the rebuild in progress and a
    /// panel holding state derived from the aggregate each hear only their own
    /// step, exactly once per build.
    pub(crate) fn ensure_aggregates(&mut self, cx: &mut Context<Self>) {
        let profile = match &self.source {
            Source::Loaded(p) => p.clone(),
            _ => return,
        };
        let key = self.aggregate_key;
        if self.agg.is_ready(key) {
            // The shown value is already current; cancel a build for a key the
            // inputs have since left so its late result can't clobber it.
            self.agg.cancel_inflight();
            return;
        }
        if self.agg.is_inflight(key) {
            return;
        }
        let generation = self.agg.start(key);
        cx.emit(AggregateBuildStarted);
        let selected = self.selected_tracks.clone();
        let time_window = self.time_selection.clone();
        let mode = self.flame_mode;
        let cached_scan = self.flame_scan.clone();
        let task = cx.spawn(async move |model, cx| {
            let (scan, aggregate) = cx
                .background_executor()
                .spawn(async move {
                    let t = std::time::Instant::now();
                    let scan =
                        cached_scan.unwrap_or_else(|| Arc::new(TrackFlamer::new(&profile)));
                    let aggregate = ScopeAggregate::build_for_tracks_reusing(
                        &scan,
                        mode,
                        &time_window,
                        |i| selected.get(i).copied().unwrap_or(false),
                    );
                    if std::env::var_os("BVIZ_TRACE_TIMING").is_some() {
                        eprintln!(
                            "[timing] track_aggregate: {:.3}s",
                            t.elapsed().as_secs_f64()
                        );
                    }
                    (scan, aggregate)
                })
                .await;
            model
                .update(cx, |m, cx| {
                    // Discard a result the inputs have moved past since this spawn.
                    if !m.agg.accept(generation, key) {
                        return;
                    }
                    m.flame_scan = Some(scan);
                    m.aggregates = Some(Arc::new(aggregate));
                    cx.notify();
                })
                .ok();
        });
        self.agg.set_task(task);
    }

    /// Drops the cached flame graph and abandons any in-flight build, so the next
    /// [`Self::ensure_aggregates`] rebuilds from scratch. Called when a new
    /// profile loads — unlike a mere selection change, the old graph is no longer
    /// a valid thing to keep showing.
    pub(crate) fn invalidate_aggregates(&mut self) {
        self.aggregates = None;
        self.flame_scan = None;
        self.agg.invalidate();
    }

    /// The visible-lane tracks from the selection anchor to `track` inclusive, in
    /// display order; `None` with no anchor or one no longer a visible lane.
    fn visible_run_to(&self, track: usize) -> Option<Vec<usize>> {
        let anchor = self.select_anchor?;
        let order: Vec<usize> = self.lane_layout()?.lanes.iter().map(|l| l.track).collect();
        let ai = order.iter().position(|&t| t == anchor)?;
        let ti = order.iter().position(|&t| t == track)?;
        Some(order[ai.min(ti)..=ai.max(ti)].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::TraceModel;
    use bviz_trace_layout::{FlameDirection, FlameLabels, FlameMode};

    #[test]
    fn re_choosing_the_standing_flame_mode_keys_no_rebuild() {
        let mut model = TraceModel::new();
        let key = model.aggregate_key;
        let standing = model.flame_mode();
        assert!(!model.adopt_flame_mode(standing));
        assert_eq!(model.aggregate_key, key);
    }

    #[test]
    fn each_half_of_the_flame_mode_keys_a_rebuild_of_its_own() {
        let mut model = TraceModel::new();
        let key = model.aggregate_key;

        let inverted = FlameMode {
            direction: FlameDirection::BottomUp,
            ..model.flame_mode()
        };
        assert!(model.adopt_flame_mode(inverted));
        assert_eq!(model.flame_mode(), inverted);
        assert_eq!(model.aggregate_key, key + 1);

        // The labelling moves independently of the direction, which the new mode
        // carries over.
        let renamed = FlameMode { labels: FlameLabels::Name, ..inverted };
        assert!(model.adopt_flame_mode(renamed));
        assert_eq!(model.flame_mode(), renamed);
        assert_eq!(model.aggregate_key, key + 2);
    }
}
