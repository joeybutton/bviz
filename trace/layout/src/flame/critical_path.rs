use std::collections::HashMap;

use bviz_trace_profile::{LaneClass, Profile, Slice, SliceDetail};
use rustc_hash::{FxHashMap, FxHashSet};

use super::{Builder, Flamer, Forest, containment_forest};

/// The inner of an `action '…'` name, `None` for any other shape. A
/// critical-path component is named `action '<desc>'` where `<desc>` is the
/// action processing slice's name elsewhere; stripping the wrapper lets the
/// name index recover its mnemonic.
pub(crate) fn action_inner(name: &str) -> Option<&str> {
    name.strip_prefix("action '")?.strip_suffix('\'')
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

/// Resolve a critical-path component's name to the worker span it copies:
/// prefer the action inside an `action '…'` wrapper, falling back to the bare
/// name for components that name their span directly.
fn resolve_action<'a>(
    name: &str,
    actions: &'a FxHashMap<&str, ActionLoc>,
) -> Option<&'a ActionLoc> {
    action_inner(name)
        .and_then(|inner| actions.get(inner))
        .or_else(|| actions.get(name))
}

/// The lanes a critical-path build walks: every critical-path lane plus the
/// lane of every action a component resolves to. Selecting them needs only the
/// component names and the action index — both cheap — so the expensive name
/// scan can be restricted to this set.
pub(crate) fn critical_lanes(
    profile: &Profile,
    actions: &FxHashMap<&str, ActionLoc>,
) -> FxHashSet<usize> {
    let threads = profile.threads();
    let mut lanes = FxHashSet::default();
    for (i, thread) in threads.iter().enumerate() {
        if thread.lane_class != LaneClass::CriticalPath {
            continue;
        }
        lanes.insert(i);
        for cp in profile.slices(thread) {
            let name = profile.str(cp.name);
            if let Some(loc) = resolve_action(name, actions) {
                lanes.insert(loc.lane);
            }
        }
    }
    lanes
}

/// Graft a matched action's worker span into `root`: its whole subtree merges
/// under the root so the critical action shows its internal breakdown, and its
/// duration adds to the root total. Each worker lane's containment forest is
/// built once, on first use, and cached in `forests`.
fn graft_action(
    profile: &Profile,
    flamer: &Flamer,
    loc: &ActionLoc,
    forests: &mut HashMap<usize, Forest>,
    root: &mut Builder,
) {
    let &ActionLoc { lane, slice } = loc;
    let threads = profile.threads();
    let lane_slices = profile.slices(&threads[lane]);
    let lane_details = profile.details(&threads[lane]);
    let forest = forests
        .entry(lane)
        .or_insert_with(|| containment_forest(lane_slices));
    root.total_us += lane_slices[slice].dur_us;
    flamer.merge_span(root, lane_slices, lane_details, forest, slice);
}

/// Merge a component with no matching action (e.g. "Writing file …") as a bare
/// leaf under `root`, adding its duration to the leaf frame and the root total.
fn merge_leaf(flamer: &Flamer, cp: &Slice, cp_detail: &SliceDetail, root: &mut Builder) {
    root.total_us += cp.dur_us;
    let seg = flamer.span_label(cp, cp_detail);
    let node = root.child(seg, || flamer.ranks.rank_of(seg.label));
    node.total_us += cp.dur_us;
    node.count += 1;
}

/// Graft the critical path's components into `root`. Each component on a
/// `LaneClass::CriticalPath` lane names an action (`action '<desc>'`); when
/// `resolve_action` finds its worker span in `actions` (an [`action_locations`]
/// index) [`graft_action`] copies that span's subtree, otherwise
/// [`merge_leaf`] merges the component directly. `flamer`'s scan must cover the
/// lanes [`critical_lanes`] selects from the same index.
pub(crate) fn build_critical_path(
    profile: &Profile,
    flamer: &Flamer,
    actions: &FxHashMap<&str, ActionLoc>,
    root: &mut Builder,
) {
    let mut forests: HashMap<usize, Forest> = HashMap::new();
    for thread in profile.threads() {
        if thread.lane_class != LaneClass::CriticalPath {
            continue;
        }
        for (cp, cp_detail) in profile.slices(thread).iter().zip(profile.details(thread)) {
            let name = profile.str(cp.name);
            match resolve_action(name, actions) {
                Some(loc) => graft_action(profile, flamer, loc, &mut forests, root),
                None => merge_leaf(flamer, cp, cp_detail, root),
            }
        }
    }
}
