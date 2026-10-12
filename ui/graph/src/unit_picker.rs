//! The dropdown choosing which unit a view of a graph's points draws.

use crate::point::unit_names;
use crate::toolbar::PickerGroup;
use bviz_graph_layout::InputSet;
use bviz_graph_layout::metrics::{Scope, Unit};
use bviz_ui_components::{Dropdown, DropdownEvent, Priority, ToolbarGroup};
use gpui::{App, AppContext, Context, Entity, FocusHandle, Subscription, Window};

gpui::actions!(
    bviz_graph,
    [
        /// Cycle points
        CyclePoints,
    ]
);

/// The word naming the picker, beside it in a toolbar and on its row of the
/// overflow menu.
const NAME: &str = "points";

/// The choice of which unit a view draws among those a graph's inputs
/// describe, and of the inputs each unit is drawn over. A unit keeps the
/// inputs it was last drawn over while another is chosen. The dropdown's
/// selection is the one record of which unit is chosen.
pub(crate) struct UnitPicker {
    finest: Scope,
    /// Coarsest last, in the dropdown's order after the finest.
    coarser: Vec<Scope>,
    dropdown: Entity<Dropdown>,
    /// The dropdown's choice as the picker opened.
    opened: usize,
    _picked: Subscription,
}

impl UnitPicker {
    /// The picker among the units `inputs` describe, each over every input of
    /// `inputs` describing it, which calls `picked` as each unit is chosen
    /// after and whose `hint` says what a unit is drawn as, beside the keys of
    /// [`CyclePoints`] where `focus` stands; absent where `inputs` describe
    /// none. The unit `opening` is chosen first where `inputs` describe it,
    /// and the finest otherwise.
    pub(crate) fn new<V: 'static>(
        id: &'static str,
        hint: &'static str,
        inputs: InputSet,
        opening: Option<Unit>,
        focus: &FocusHandle,
        picked: fn(&mut V, &mut Context<V>),
        cx: &mut Context<V>,
    ) -> Option<Self> {
        let mut scopes = Unit::ALL
            .into_iter()
            .filter_map(|unit| Scope::new(unit, inputs));
        let finest = scopes.next()?;
        let coarser: Vec<Scope> = scopes.collect();
        let units = std::iter::once(finest)
            .chain(coarser.iter().copied())
            .map(Scope::unit);
        let opened = units
            .clone()
            .position(|unit| Some(unit) == opening)
            .unwrap_or(0);
        let names = units.map(|unit| unit_names(unit).many);
        let dropdown = cx.new(|cx| {
            Dropdown::new(id, names, opened, cx)
                .tooltip(hint)
                .keys_for(&CyclePoints, focus)
        });
        let picked = cx.subscribe(&dropdown, move |view, _, _: &DropdownEvent, cx| {
            picked(view, cx);
        });
        Some(Self {
            finest,
            coarser,
            dropdown,
            opened,
            _picked: picked,
        })
    }

    /// The unit chosen, over the inputs it is drawn over.
    pub(crate) fn chosen(&self, cx: &App) -> Scope {
        match self.coarser_chosen(cx) {
            None => self.finest,
            Some(index) => self.coarser[index],
        }
    }

    /// Draws the unit chosen over `inputs` where they describe it, and
    /// returns whether they do.
    pub(crate) fn draw_over(&mut self, inputs: InputSet, cx: &App) -> bool {
        let chosen = match self.coarser_chosen(cx) {
            None => &mut self.finest,
            Some(index) => &mut self.coarser[index],
        };
        let Some(scope) = Scope::new(chosen.unit(), inputs) else {
            return false;
        };
        *chosen = scope;
        true
    }

    /// Where the unit chosen is not the finest, its place in `coarser`.
    fn coarser_chosen(&self, cx: &App) -> Option<usize> {
        self.dropdown.read(cx).selected().checked_sub(1)
    }

    /// Whether the unit chosen is the one the picker opened on.
    fn at_rest(&self, cx: &App) -> bool {
        self.dropdown.read(cx).selected() == self.opened
    }

    /// Steps the picker to the unit after the one chosen.
    pub(crate) fn cycle(&self, cx: &mut App) {
        self.dropdown.update(cx, |dropdown, cx| dropdown.cycle(cx));
    }

    /// The picker as a toolbar group holding out by `priority`, its overflow
    /// row naming the keys of [`CyclePoints`] where `focus` stands.
    pub(crate) fn group(
        &self,
        priority: Priority,
        focus: &FocusHandle,
        window: &Window,
        cx: &App,
    ) -> ToolbarGroup {
        PickerGroup {
            name: NAME,
            dropdown: &self.dropdown,
            action: &CyclePoints,
            note: None,
            set: !self.at_rest(cx),
        }
        .group(priority, focus, window, cx)
    }
}
