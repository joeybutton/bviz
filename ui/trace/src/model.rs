use crate::groups::{GROUP_ORDER, GroupChrome, build_grouped_layout, default_collapsed_groups};
use crate::{METRICS, Source};
use bviz_trace_layout::{LaneLayout, lay_out_lanes};
use bviz_trace_profile::{LaneClass, Profile};
use std::cell::RefCell;
use std::rc::Rc;

/// The shared trace document and the selection/visibility state derived from it:
/// which tracks the flame chart draws and which the aggregates cover, the
/// lane-class group collapse and show-all state, and the lane layouts cached from
/// them. Held by [`crate::TraceView`] as an entity so a panel reads this once and
/// the state lives in one place; the panel's own view state (scroll, hover, tabs,
/// layers) stays on the view.
pub(crate) struct TraceModel {
    source: Source,
    /// The full thread/counter lane layout for the loaded profile, built on first
    /// use. It depends only on the immutable profile, so it is cleared only when a
    /// new profile loads.
    lanes: Rc<RefCell<Option<Rc<LaneLayout>>>>,
    /// Which tracks the flame chart draws, indexed by `profile.threads()` order.
    visible_tracks: Vec<bool>,
    /// Which tracks the flame graph and stats aggregate over, indexed likewise.
    selected_tracks: Vec<bool>,
    /// Which lane-class groups are collapsed in the chart, indexed by
    /// [`GROUP_ORDER`]. Loading starts collapsed — it holds one lane per external
    /// repository, hundreds on a large build.
    collapsed_groups: [bool; GROUP_ORDER.len()],
    /// Which groups show every lane rather than the busiest, indexed by
    /// [`GROUP_ORDER`].
    group_show_all: [bool; GROUP_ORDER.len()],
    /// Bumped whenever `selected_tracks` changes, keying the aggregate build so a
    /// stale build's result is discarded when the selection moves on.
    selection_version: u64,
    /// The track a shift-click range-selection extends from, if any.
    select_anchor: Option<usize>,
    /// The lane layout restricted to the visible tracks and restacked into
    /// lane-class groups, derived from `lanes` and cleared whenever the
    /// visibility, collapse, or show-all state changes.
    visible_lanes: Rc<RefCell<Option<Rc<LaneLayout>>>>,
    /// The group headers and show-more rows over `visible_lanes`, built and
    /// cleared alongside it.
    group_chrome: Rc<RefCell<Option<Rc<GroupChrome>>>>,
}

impl TraceModel {
    pub(crate) fn new() -> Self {
        Self {
            source: Source::Loading,
            lanes: Rc::new(RefCell::new(None)),
            visible_tracks: Vec::new(),
            selected_tracks: Vec::new(),
            collapsed_groups: default_collapsed_groups(),
            group_show_all: [false; GROUP_ORDER.len()],
            selection_version: 0,
            select_anchor: None,
            visible_lanes: Rc::new(RefCell::new(None)),
            group_chrome: Rc::new(RefCell::new(None)),
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

    pub(crate) fn selection_version(&self) -> u64 {
        self.selection_version
    }

    /// Replaces the loaded document, dropping the lane layouts (which depend only
    /// on the profile) and, for a loaded profile, seeding the default track
    /// selection.
    pub(crate) fn set_source(&mut self, source: Source) {
        self.lanes.borrow_mut().take();
        self.visible_lanes.borrow_mut().take();
        self.source = source;
        if let Source::Loaded(profile) = &self.source {
            let profile = profile.clone();
            self.init_tracks(&profile);
        }
    }

    /// The default track selection for a freshly loaded profile: every
    /// non-critical-path lane selected. The critical-path lane holds redirected
    /// copies of worker time, so leaving it out of the default selection keeps
    /// the aggregate from double-counting that time.
    pub(crate) fn init_tracks(&mut self, profile: &Profile) {
        self.visible_tracks = vec![true; profile.threads().len()];
        self.collapsed_groups = default_collapsed_groups();
        self.group_show_all = [false; GROUP_ORDER.len()];
        self.selected_tracks = profile
            .threads()
            .iter()
            .map(|t| t.lane_class != LaneClass::CriticalPath)
            .collect();
        self.select_anchor = None;
        self.selection_version += 1;
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
    /// shift click leaves it, so the run can be re-adjusted. Bumps the selection
    /// version only when the set actually changes, so a repeat click is free.
    pub(crate) fn select_track(&mut self, track: usize, shift: bool, ctrl: bool) {
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
            self.selection_version += 1;
        }
    }

    /// Sets the selection to every visible track (the Ctrl-A gesture), returning
    /// whether the set changed.
    pub(crate) fn select_all_visible(&mut self) -> bool {
        if self.selected_tracks != self.visible_tracks {
            self.selected_tracks = self.visible_tracks.clone();
            self.selection_version += 1;
            true
        } else {
            false
        }
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
