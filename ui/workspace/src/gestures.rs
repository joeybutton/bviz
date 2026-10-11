//! The pointer gestures of the chrome a window frames its panels in: the tabs
//! and handles naming them, and the dividers between panes.

use bviz_ui_components::gesture::{
    Button, Clicks, Entry, GestureRow, Gestures, Input, Motion, Place,
};

/// What the chrome does for a gesture in [`CHROME_GESTURES`].
#[derive(Clone, Copy)]
pub(crate) enum ChromeIntent {
    Maximize,
    Activate,
    Move,
    OpenMenu,
    ScrollTabs,
    RestSplit,
    ResizeSplit,
}

const TAB_OR_HANDLE: Place = Place::On("a tab or handle");
const DIVIDER: Place = Place::On("a divider");

pub(crate) const OPEN_PANEL_MENU: Entry<ChromeIntent> = Entry::new(
    Input::press(Button::Right),
    TAB_OR_HANDLE,
    "Open panel menu",
    ChromeIntent::OpenMenu,
);

pub(crate) const CHROME_GESTURES: Gestures<ChromeIntent> = Gestures::new(&[
    Entry::new(
        Input::new(Motion::Click(Button::Left, Clicks::Double)),
        TAB_OR_HANDLE,
        "Maximize or restore pane",
        ChromeIntent::Maximize,
    ),
    Entry::new(
        Input::click(Button::Left),
        Place::On("a tab"),
        "Show tab's panel",
        ChromeIntent::Activate,
    ),
    Entry::new(
        Input::drag(Button::Left),
        TAB_OR_HANDLE,
        "Move panel",
        ChromeIntent::Move,
    ),
    OPEN_PANEL_MENU,
    Entry::new(
        Input::SCROLL,
        Place::On("a tab strip"),
        "Scroll tabs",
        ChromeIntent::ScrollTabs,
    ),
    Entry::new(
        Input::new(Motion::Press(Button::Left, Clicks::Repeated)),
        DIVIDER,
        "Restore split proportions",
        ChromeIntent::RestSplit,
    ),
    Entry::new(
        Input::drag(Button::Left),
        DIVIDER,
        "Resize panes",
        ChromeIntent::ResizeSplit,
    ),
]);

/// The gestures every window's chrome answers to, around whichever panels it
/// frames.
pub fn chrome_gestures() -> Vec<GestureRow> {
    CHROME_GESTURES
        .rows()
        .chain(bviz_ui_shell::title_bar_gestures())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_gesture_is_reachable() {
        assert_eq!(CHROME_GESTURES.unreachable(), []);
    }

    #[test]
    fn every_gesture_fits_the_keys_help() {
        assert_eq!(CHROME_GESTURES.overlong(), []);
    }
}
