//! How the box under the cursor ended, as the card over a graph coloured by
//! playback names it.

use crate::Palette;
use crate::legend::state_swatch;
use crate::playback::{Ended, NodeState, NodeStates};
use bviz_graph_layout::{Id, LaidOut};
use bviz_invocation::{Cache, Done, EpochMicros, SpawnCache};
use gpui::{Div, div, prelude::*};

/// How a reader is told which cache answered.
fn cache_name(cache: Cache) -> &'static str {
    match cache {
        Cache::Action => "action cache",
        Cache::Spawn(SpawnCache::Remote) => "remote cache",
        Cache::Spawn(SpawnCache::Disk) => "disk cache",
        Cache::Spawn(SpawnCache::RemoteOrDisk) => "remote or disk cache",
    }
}

/// What an ending without a spawn may have been, which nothing seen tells
/// apart.
const SPAWNLESS_READINGS: &str = "up to date, run in Bazel, or cached";

/// What may have found an action up to date, where nothing seen tells which
/// did.
const UP_TO_DATE_READINGS: &str = "action cache or change pruning";

/// How a box whose attempts ended as `done` says is said to have ended.
fn words(label: &str, done: Done) -> String {
    let state = NodeState::Done(Ended::of(done)).label();
    match done {
        Done::Cached(cache) => format!("{label} ends {state} · {}", cache_name(cache)),
        Done::Spawnless => format!("{label} ends {state} · {SPAWNLESS_READINGS}"),
        Done::UpToDate => format!("{label} ends {state} · {UP_TO_DATE_READINGS}"),
        Done::Executed | Done::Failed => format!("{label} ends {state}"),
    }
}

/// The card naming how the box `hovered` names in `g` ended, `None` where no
/// box is hovered or it has not ended by `at`.
pub(crate) fn card(
    hovered: Option<Id>,
    g: &LaidOut,
    states: &NodeStates,
    at: EpochMicros,
    palette: &Palette,
) -> Option<Div> {
    let hovered = hovered?;
    let Some(NodeState::Done(_)) = states.at(hovered, at) else {
        return None;
    };
    let done = states.done(hovered)?;
    let label = &g.boxes.iter().find(|b| b.id == hovered)?.label;
    Some(
        div()
            .flex()
            .items_center()
            .gap_2()
            .p_2()
            .rounded_md()
            .bg(palette.container)
            .text_xs()
            .text_color(palette.label)
            .child(state_swatch(NodeState::Done(Ended::of(done)), palette))
            .child(words(label, done)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cached_ending_names_the_cache_that_answered() {
        let cached = |cache| words("//app:bin", Done::Cached(cache));
        assert_eq!(
            cached(Cache::Spawn(SpawnCache::Disk)),
            "//app:bin ends done, cached · disk cache"
        );
        assert_eq!(
            cached(Cache::Spawn(SpawnCache::RemoteOrDisk)),
            "//app:bin ends done, cached · remote or disk cache"
        );
        assert_eq!(
            cached(Cache::Action),
            "//app:bin ends up to date · action cache"
        );
        assert_eq!(words("a.o", Done::Failed), "a.o ends failed");
        assert_eq!(
            words("a.o", Done::Spawnless),
            "a.o ends done, no spawn · up to date, run in Bazel, or cached"
        );
        assert_eq!(
            words("a.o", Done::UpToDate),
            "a.o ends up to date · action cache or change pruning"
        );
        assert_eq!(words("a.o", Done::Executed), "a.o ends done, executed");
    }
}
