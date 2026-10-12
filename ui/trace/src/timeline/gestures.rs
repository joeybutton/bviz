use crate::model::TrackPick;
use bviz_ui_components::gesture::{Button, Clicks, Entry, Gestures, Held, Input, Motion, Place};

/// What the timeline does for a gesture in [`TIMELINE_GESTURES`].
#[derive(Clone, Copy)]
pub(crate) enum TimelineIntent {
    ShowWhole,
    ShowAllOfGroup,
    FoldGroup,
    PickTrack(TrackPick),
    SelectTime { on: TimeBand, toggles: bool },
    Pan,
    Unchoose,
    Zoom,
    PanTime,
    Scroll,
    Read,
}

/// Where a press takes hold of the time selection.
#[derive(Clone, Copy)]
pub(crate) enum TimeBand {
    Ruler,
    Lanes,
}

const GROUP_HEADER: Place = Place::On("a group header");
const TRACK_LABEL: Place = Place::On("a track label");
const RULER: Place = Place::On("the ruler");
const LANES: Place = Place::On("the lanes");

pub(crate) const TIMELINE_GESTURES: Gestures<TimelineIntent> = Gestures::new(&[
    Entry::new(
        Input::press(Button::Left),
        Place::On("a show-all icon"),
        "Show all lanes or cap them",
        TimelineIntent::ShowAllOfGroup,
    ),
    Entry::new(
        Input::press(Button::Left),
        GROUP_HEADER,
        "Fold or unfold group",
        TimelineIntent::FoldGroup,
    ),
    Entry::new(
        Input::press(Button::Left).holding(Held::SHIFT.and(Held::SECONDARY)),
        TRACK_LABEL,
        "Set tracks to here like anchor",
        TimelineIntent::PickTrack(TrackPick::SetRun),
    ),
    Entry::new(
        Input::press(Button::Left).holding(Held::SHIFT),
        TRACK_LABEL,
        "Select from anchor to here",
        TimelineIntent::PickTrack(TrackPick::Run),
    ),
    Entry::new(
        Input::press(Button::Left).holding(Held::SECONDARY),
        TRACK_LABEL,
        "Add or remove track",
        TimelineIntent::PickTrack(TrackPick::Flip),
    ),
    Entry::new(
        Input::press(Button::Left),
        TRACK_LABEL,
        "Select track alone",
        TimelineIntent::PickTrack(TrackPick::Only),
    ),
    Entry::new(
        Input::drag(Button::Left).holding(Held::SECONDARY),
        RULER,
        "Add or remove time region",
        TimelineIntent::SelectTime {
            on: TimeBand::Ruler,
            toggles: true,
        },
    ),
    Entry::new(
        Input::drag(Button::Left),
        RULER,
        "Select or adjust time region",
        TimelineIntent::SelectTime {
            on: TimeBand::Ruler,
            toggles: false,
        },
    ),
    Entry::new(
        Input::drag(Button::Left).holding(Held::SHIFT.and(Held::SECONDARY)),
        LANES,
        "Add or remove time region",
        TimelineIntent::SelectTime {
            on: TimeBand::Lanes,
            toggles: true,
        },
    ),
    Entry::new(
        Input::drag(Button::Left).holding(Held::SHIFT),
        LANES,
        "Select time region",
        TimelineIntent::SelectTime {
            on: TimeBand::Lanes,
            toggles: false,
        },
    ),
    Entry::new(
        Input::new(Motion::Press(Button::Left, Clicks::Double)),
        Place::Empty,
        "Show whole timeline",
        TimelineIntent::ShowWhole,
    ),
    Entry::new(
        Input::drag(Button::Left),
        Place::Anywhere,
        "Pan",
        TimelineIntent::Pan,
    ),
    Entry::new(
        Input::click(Button::Left),
        Place::Anywhere,
        "Clear chosen row",
        TimelineIntent::Unchoose,
    ),
    Entry::new(
        Input::SCROLL.holding(Held::SECONDARY),
        Place::Anywhere,
        "Zoom about pointer",
        TimelineIntent::Zoom,
    ),
    Entry::new(
        Input::PINCH,
        Place::Anywhere,
        "Zoom about pointer",
        TimelineIntent::Zoom,
    ),
    Entry::new(
        Input::SCROLL.holding(Held::SHIFT),
        Place::Anywhere,
        "Pan across",
        TimelineIntent::PanTime,
    ),
    Entry::new(
        Input::SCROLL,
        Place::Anywhere,
        "Pan lanes or counters",
        TimelineIntent::Scroll,
    ),
    Entry::new(
        Input::HOVER,
        Place::Anywhere,
        "Read time and slice",
        TimelineIntent::Read,
    ),
]);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_gesture_is_reachable() {
        assert_eq!(TIMELINE_GESTURES.unreachable(), []);
    }

    #[test]
    fn every_gesture_fits_the_keys_help() {
        assert_eq!(TIMELINE_GESTURES.overlong(), []);
    }
}
