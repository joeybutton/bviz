//! The dropdown choosing which unit a view of a graph's points draws.

use crate::point::unit_names;
use bviz_graph_layout::InputSet;
use bviz_graph_layout::metrics::{Scope, Unit};
use bviz_ui_components::{Dropdown, DropdownEvent};
use gpui::{App, AppContext, Context, Entity, Subscription};

/// The choice of which unit a view draws among those a graph's inputs
/// describe, and of the inputs each unit is drawn over. A unit keeps the
/// inputs it was last drawn over while another is chosen. The dropdown's
/// selection is the one record of which unit is chosen.
pub(crate) struct UnitPicker {
    finest: Scope,
    /// Coarsest last, in the dropdown's order after the finest.
    coarser: Vec<Scope>,
    dropdown: Entity<Dropdown>,
    _picked: Subscription,
}

impl UnitPicker {
    /// The picker among the units `inputs` describe, each over every input of
    /// `inputs` describing it and the finest chosen, which calls `picked` as
    /// each unit is chosen after; absent where `inputs` describe none.
    pub(crate) fn new<V: 'static>(
        id: &'static str,
        inputs: InputSet,
        picked: fn(&mut V, &mut Context<V>),
        cx: &mut Context<V>,
    ) -> Option<Self> {
        let mut scopes = Unit::ALL
            .into_iter()
            .filter_map(|unit| Scope::new(unit, inputs));
        let finest = scopes.next()?;
        let coarser: Vec<Scope> = scopes.collect();
        let names = std::iter::once(finest)
            .chain(coarser.iter().copied())
            .map(|scope| unit_names(scope.unit()).many);
        let dropdown = cx.new(|cx| Dropdown::new(id, names, 0, cx));
        let picked = cx.subscribe(&dropdown, move |view, _, _: &DropdownEvent, cx| {
            picked(view, cx);
        });
        Some(Self {
            finest,
            coarser,
            dropdown,
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

    pub(crate) fn dropdown(&self) -> Entity<Dropdown> {
        self.dropdown.clone()
    }
}
