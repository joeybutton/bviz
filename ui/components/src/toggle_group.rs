//! A joined row of toggles: one choice among several, or several independent
//! switches, which can instead stand collapsed into a trigger opening a menu of
//! the same toggles.

use crate::button::Button;
use crate::popup::{self, Popup};
use crate::{TextTooltip, key_text_in, menu_chrome};
use bviz_ui_theme::ThemeColors;
use gpui::{
    Action, App, ClickEvent, ElementId, Entity, FocusHandle, IntoElement, RenderOnce, SharedString,
    Window, div, prelude::*,
};
use std::rc::Rc;

/// What a toggle runs when it is clicked.
type OnClick = Box<dyn Fn(&mut Window, &mut App) + 'static>;

/// What one toggle of a [`ToggleGroup`] shows.
pub struct ToggleSegment {
    label: SharedString,
    icon: Option<SharedString>,
    /// What the tooltip reads, where it reads other than the help label of the
    /// action [`ToggleSegment::keys_for`] names.
    tooltip: Option<SharedString>,
    keys: Option<(Box<dyn Action>, FocusHandle)>,
    /// Why the toggle can do nothing, which it is drawn greyed for and its
    /// tooltip reads.
    unavailable: Option<SharedString>,
}

impl ToggleSegment {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            icon: None,
            tooltip: None,
            keys: None,
            unavailable: None,
        }
    }

    pub fn icon(mut self, icon: impl Into<SharedString>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    /// As [`Button::keys_for`]: the tooltip, and the menu row the toggle stands
    /// as while collapsed, name the keys that run `action` where `focus` stands.
    pub fn keys_for(mut self, action: &dyn Action, focus: &FocusHandle) -> Self {
        self.keys = Some((action.boxed_clone(), focus.clone()));
        self
    }

    /// Greys the toggle out, its tooltip reading `reason`. A toggle that is on
    /// still shows so.
    pub fn unavailable(mut self, reason: impl Into<SharedString>) -> Self {
        self.unavailable = Some(reason.into());
        self
    }

    /// What the tooltip reads where it reads anything but the action's help
    /// label.
    fn tip(&self) -> Option<SharedString> {
        self.unavailable.clone().or_else(|| self.tooltip.clone())
    }
}

impl FluentBuilder for ToggleSegment {}

/// One of several independent switches a [`ToggleGroup::any_of`] holds.
pub struct Switch {
    segment: ToggleSegment,
    on: bool,
    click: OnClick,
}

impl Switch {
    /// A switch standing `on`, asking `on_toggle` for the state a click flips
    /// it to.
    pub fn new(
        segment: ToggleSegment,
        on: bool,
        on_toggle: impl Fn(bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            segment,
            on,
            click: Box::new(move |window, cx| on_toggle(!on, window, cx)),
        }
    }
}

/// A toggle as the group draws it.
struct Entry {
    segment: ToggleSegment,
    click: OnClick,
}

/// The group's toggles, by the kind of choice they make.
enum Toggles {
    /// Exactly one stands chosen.
    One {
        before: Vec<Entry>,
        chosen: Entry,
        after: Vec<Entry>,
    },
    Any(Vec<Switch>),
}

impl Toggles {
    /// Every toggle in order, each with whether it stands on.
    fn iter(&self) -> Box<dyn Iterator<Item = (&ToggleSegment, bool)> + '_> {
        match self {
            Toggles::One {
                before,
                chosen,
                after,
            } => {
                fn unchosen(entry: &Entry) -> (&ToggleSegment, bool) {
                    (&entry.segment, false)
                }
                Box::new(
                    (before.iter().map(unchosen))
                        .chain(std::iter::once((&chosen.segment, true)))
                        .chain(after.iter().map(unchosen)),
                )
            }
            Toggles::Any(switches) => {
                Box::new(switches.iter().map(|switch| (&switch.segment, switch.on)))
            }
        }
    }

    /// Every toggle in order, with whether it stands on and what a click on it
    /// runs.
    fn into_entries(self) -> Vec<(ToggleSegment, bool, OnClick)> {
        match self {
            Toggles::One {
                before,
                chosen,
                after,
            } => {
                let unchosen = |entry: Entry| (entry.segment, false, entry.click);
                (before.into_iter().map(unchosen))
                    .chain(std::iter::once((chosen.segment, true, chosen.click)))
                    .chain(after.into_iter().map(unchosen))
                    .collect()
            }
            Toggles::Any(switches) => switches
                .into_iter()
                .map(|switch| (switch.segment, switch.on, switch.click))
                .collect(),
        }
    }
}

/// How a [`ToggleGroup`] stands.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum Presentation {
    /// Every toggle side by side.
    #[default]
    Joined,
    /// A trigger reading the group's label and its state, opening a menu of
    /// the toggles.
    Collapsed,
}

/// A joined row of toggles making one choice ([`one_of`](Self::one_of)) or
/// setting several switches ([`any_of`](Self::any_of)).
#[derive(IntoElement)]
pub struct ToggleGroup {
    id: ElementId,
    /// Names what the toggles set; read on the collapsed trigger.
    label: SharedString,
    toggles: Toggles,
    presentation: Presentation,
}

impl ToggleGroup {
    /// The choice among `values` of the one standing, each shown as `segment`
    /// draws it, a click asking `on_choose` for its value. `standing` is shown
    /// chosen where it first falls among `values`, and after them where they
    /// leave it out.
    pub fn one_of<T: Copy + PartialEq + 'static>(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        values: impl IntoIterator<Item = T>,
        standing: T,
        segment: impl Fn(T) -> ToggleSegment,
        on_choose: impl Fn(T, &mut Window, &mut App) + 'static,
    ) -> Self {
        let on_choose = Rc::new(on_choose);
        let entry = |value: T| Entry {
            segment: segment(value),
            click: Box::new({
                let on_choose = on_choose.clone();
                move |window, cx| on_choose(value, window, cx)
            }),
        };
        let mut values = values.into_iter();
        let before = values
            .by_ref()
            .take_while(|value| *value != standing)
            .map(&entry)
            .collect();
        let chosen = entry(standing);
        let after = values.map(&entry).collect();
        Self::new(
            id,
            label,
            Toggles::One {
                before,
                chosen,
                after,
            },
        )
    }

    /// Independent `switches`, side by side.
    pub fn any_of(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        switches: impl IntoIterator<Item = Switch>,
    ) -> Self {
        Self::new(id, label, Toggles::Any(switches.into_iter().collect()))
    }

    fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, toggles: Toggles) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            toggles,
            presentation: Presentation::default(),
        }
    }

    pub fn presentation(mut self, presentation: Presentation) -> Self {
        self.presentation = presentation;
        self
    }

    /// What the collapsed trigger reads: the label, then the value chosen or
    /// how many switches of how many are on.
    fn reading(&self) -> SharedString {
        match &self.toggles {
            Toggles::One { chosen, .. } => format!("{} {}", self.label, chosen.segment.label),
            Toggles::Any(switches) => {
                let on = switches.iter().filter(|switch| switch.on).count();
                format!("{} {on}/{}", self.label, switches.len())
            }
        }
        .into()
    }

    /// Everything the collapsed trigger can read, so it is sized to the widest
    /// and does not change width as the state moves.
    fn readings(&self) -> Vec<SharedString> {
        match &self.toggles {
            Toggles::One { .. } => self
                .toggles
                .iter()
                .map(|(segment, _)| format!("{} {}", self.label, segment.label).into())
                .collect(),
            Toggles::Any(switches) => {
                let total = switches.len();
                vec![format!("{} {total}/{total}", self.label).into()]
            }
        }
    }

    fn joined(self, colors: &ThemeColors) -> impl IntoElement + use<> {
        let segments = (self.toggles.into_entries().into_iter().enumerate())
            .map(|(index, (segment, on, click))| segment_button(index, segment, on, click));
        div()
            .id(self.id)
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap_1()
            .rounded_md()
            .border_1()
            .border_color(colors.border)
            .children(segments)
    }

    fn collapsed(
        self,
        colors: &ThemeColors,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let popup = window.use_keyed_state(self.id.clone(), cx, |_, cx| Popup::new(cx));
        let width = menu_chrome::widest(window, self.readings().into_iter());
        let trigger = menu_chrome::trigger(self.id.clone(), colors)
            .on_click({
                let popup = popup.clone();
                move |_: &ClickEvent, window, cx| {
                    popup.update(cx, |popup, cx| popup.toggle(window, cx));
                }
            })
            .child(menu_chrome::reading(width, self.reading()))
            .child(menu_chrome::caret(colors));
        let closes_on_choice = matches!(self.toggles, Toggles::One { .. });
        let menu = popup.read(cx).is_open().then(|| {
            menu(
                self.toggles.into_entries(),
                &popup,
                closes_on_choice,
                colors,
                window,
            )
        });
        popup::holder(&popup, cx).child(trigger).children(menu)
    }
}

impl RenderOnce for ToggleGroup {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = bviz_ui_theme::theme(window, cx).colors;
        match self.presentation {
            Presentation::Joined => self.joined(&colors).into_any_element(),
            Presentation::Collapsed => self.collapsed(&colors, window, cx).into_any_element(),
        }
    }
}

/// The toggle `segment` drawn joined, at `index` among the group's.
fn segment_button(index: usize, segment: ToggleSegment, on: bool, click: OnClick) -> Button {
    let tip = segment.tip();
    let button = Button::new(("segment", index), move |_, window, cx| click(window, cx))
        .ghost()
        .compact()
        .selected(on)
        .disabled(segment.unavailable.is_some())
        .label(segment.label)
        .when_some(segment.icon, Button::icon)
        .when_some(tip, Button::tooltip);
    match &segment.keys {
        Some((action, focus)) => button.keys_for(&**action, focus),
        None => button,
    }
}

/// The menu a collapsed group opens: one row a toggle, a click on one closing
/// the menu where `closes_on_choice`.
fn menu(
    entries: Vec<(ToggleSegment, bool, OnClick)>,
    popup: &Entity<Popup>,
    closes_on_choice: bool,
    colors: &ThemeColors,
    window: &Window,
) -> impl IntoElement + use<> {
    let tooltip = colors.tooltip();
    let rows = entries
        .into_iter()
        .enumerate()
        .map(|(index, (segment, on, click))| {
            let keys = (segment.keys.as_ref())
                .and_then(|(action, focus)| key_text_in(&**action, focus, window));
            let available = segment.unavailable.is_none();
            let tip = segment.tip();
            let popup = popup.clone();
            menu_chrome::row(("toggle-group-row", index).into(), colors, on)
                .when(!closes_on_choice, |row| {
                    row.child(menu_chrome::check(colors, on))
                })
                .children(segment.icon.map(|icon| {
                    bviz_ui_assets::icon(icon.as_ref())
                        .size(menu_chrome::MENU_ICON)
                        .text_color(colors.text)
                }))
                .child(div().whitespace_nowrap().child(segment.label.clone()))
                .children(keys.map(|keys| {
                    div()
                        .pl(menu_chrome::LABEL_GAP)
                        .child(menu_chrome::row_keys(keys, colors))
                }))
                .when_some(tip, |row, tip| {
                    row.tooltip(TextTooltip::builder(tip, tooltip))
                })
                .map(|row| match available {
                    true => row.on_click(move |_: &ClickEvent, window, cx| {
                        if closes_on_choice {
                            popup.update(cx, |popup, cx| popup.close(window, cx));
                        }
                        click(window, cx);
                    }),
                    false => row.cursor_default().text_color(colors.text_disabled),
                })
        });
    menu_chrome::under_trigger(menu_chrome::sheet(colors).children(rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Direction {
        Up,
        Down,
        Sideways,
    }

    fn one_of(values: &[Direction], standing: Direction) -> ToggleGroup {
        ToggleGroup::one_of(
            "under-test",
            "direction",
            values.iter().copied(),
            standing,
            |value| ToggleSegment::new(format!("{value:?}").to_lowercase()),
            |_, _, _| {},
        )
    }

    fn shown(group: &ToggleGroup) -> Vec<(String, bool)> {
        (group.toggles.iter())
            .map(|(segment, on)| (segment.label.to_string(), on))
            .collect()
    }

    const ALL: [Direction; 3] = [Direction::Up, Direction::Down, Direction::Sideways];

    #[test]
    fn one_of_shows_every_value_in_order_with_the_standing_one_chosen() {
        assert_eq!(
            shown(&one_of(&ALL, Direction::Down)),
            [
                ("up".to_owned(), false),
                ("down".to_owned(), true),
                ("sideways".to_owned(), false),
            ]
        );
    }

    /// The standing value is chosen whatever the values offered, so the group
    /// never shows nothing chosen.
    #[test]
    fn one_of_shows_a_standing_value_left_out_of_the_values_after_them() {
        assert_eq!(
            shown(&one_of(&[Direction::Up], Direction::Sideways)),
            [("up".to_owned(), false), ("sideways".to_owned(), true)]
        );
    }

    /// A value offered twice cannot show two toggles chosen.
    #[test]
    fn one_of_chooses_only_the_first_of_a_value_offered_twice() {
        let values = [Direction::Down, Direction::Up, Direction::Down];
        let chosen = shown(&one_of(&values, Direction::Down))
            .into_iter()
            .filter(|(_, on)| *on)
            .count();
        assert_eq!(chosen, 1);
    }

    #[test]
    fn a_collapsed_choice_reads_its_label_and_the_value_chosen() {
        let group = one_of(&ALL, Direction::Sideways);
        assert_eq!(group.reading(), "direction sideways");
        assert_eq!(
            group.readings(),
            ["direction up", "direction down", "direction sideways"]
        );
    }

    fn any_of(on: &[bool]) -> ToggleGroup {
        ToggleGroup::any_of(
            "under-test",
            "inputs",
            on.iter().enumerate().map(|(i, &on)| {
                Switch::new(ToggleSegment::new(format!("input {i}")), on, |_, _, _| {})
            }),
        )
    }

    #[test]
    fn collapsed_switches_read_how_many_of_how_many_are_on() {
        let group = any_of(&[true, false, true]);
        assert_eq!(group.reading(), "inputs 2/3");
        assert_eq!(group.readings(), ["inputs 3/3"]);
    }

    #[test]
    fn switches_show_as_they_stand() {
        let on: Vec<bool> = (any_of(&[false, true]).toggles.iter())
            .map(|(_, on)| on)
            .collect();
        assert_eq!(on, [false, true]);
    }
}
