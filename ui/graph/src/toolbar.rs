use crate::colour::{self, Colouring};
use crate::mode::{self, LayoutMode};
use crate::{
    CRITICAL_PATH_AT_REST, Controls, CycleColouring, GraphPanel, NEIGHBOURHOOD_AT_REST, Palette,
    ToggleCriticalPath, ToggleMap, ToggleNeighborhood, boxes, chips, report, timecard,
};
use bviz_ui_components::{
    Dropdown, Legend, MenuItem, MultiSelect, OneRow, Presentation, Priority, Switch, ToggleGroup,
    ToggleSegment, ToolbarGroup,
};
use gpui::{
    Action, AnyElement, App, Context, Div, Entity, FocusHandle, IntoElement, Pixels, SharedString,
    Window, div, prelude::*,
};

/// How many strips [`toolbar`] stacks.
const STRIPS: usize = 2;

/// The height of [`toolbar`], whatever its strips hold.
pub(crate) fn height() -> Pixels {
    OneRow::height() * STRIPS as f32
}

/// The panel's controls in two strips, each one row tall, so that chips being
/// added do not move the drawing: how the graph is drawn, ending in the toggles
/// putting panels up and away, then what it holds.
pub(crate) fn toolbar(
    view: &GraphPanel,
    controls: Controls,
    palette: &Palette,
    window: &mut Window,
    cx: &Context<GraphPanel>,
) -> Div {
    let layout = PickerGroup {
        name: LAYOUT_NAME,
        dropdown: &view.layout_picker,
        action: &ToggleMap,
        note: None,
        set: view.mode != LayoutMode::default(),
    }
    .group(LAYOUT, &view.focus_handle, window, cx);
    let edges = ToolbarGroup::new(EDGES, mode::reduce_toggle(view, cx)).overflow(
        mode::reduce_row(view, EDGES_NAME, window, cx),
        mode::reduction(view).set(),
    );
    let colour = PickerGroup {
        name: COLOUR_NAME,
        dropdown: &view.colour_picker,
        action: &CycleColouring,
        note: Some(colour::hint(view.model.read(cx))),
        set: view.colouring != Colouring::default(),
    }
    .group(COLOUR, &view.focus_handle, window, cx);
    let marking = Marking::of(view, cx);
    let marks = ToolbarGroup::named(MARK, MARK_NAME, || marking.toggles(view, cx))
        .or(marking
            .toggles(view, cx)
            .presentation(Presentation::Collapsed))
        .overflow(
            MenuItem::submenu(MARK_NAME, marking.toggles(view, cx).submenu(window)),
            marking.set(),
        )
        .parted();
    let drawing = OneRow::new("graph-view-controls")
        .group(layout)
        .group(edges)
        .group(colour)
        .group(marks)
        .when(view.derived.deriving(), |row| {
            // The prior layout stays interactive while a relayout runs.
            row.busy("relaying-spinner", "laying out…")
        })
        .trailing(panel_toggles(view, controls.legend, cx));
    let query = OneRow::new("graph-query-bar")
        .when(!view.box_classes.is_empty(), |query| {
            query.group(ToolbarGroup::new(BOXES, view.boxes.clone()).overflow(
                MenuItem::submenu(boxes::NAME, MultiSelect::submenu(&view.boxes, cx)),
                boxes::any_hidden(view.derived.filter()),
            ))
        })
        .filling(|| chips::bar(view, palette, window, cx));
    let strips: [OneRow; STRIPS] = [drawing, query];
    div().size_full().flex().flex_col().children(strips)
}

/// `picker` as the row of the overflow menu opening its choices, naming the
/// keys of the `action` stepping through them where `focus` stands, each
/// choice it cannot make explained by `note`.
fn picker_row(
    name: &'static str,
    picker: &Entity<Dropdown>,
    action: &dyn Action,
    note: Option<SharedString>,
    focus: &FocusHandle,
    window: &Window,
    cx: &App,
) -> MenuItem {
    MenuItem::submenu(name, Dropdown::submenu(picker, note, cx)).keys_for(action, focus, window)
}

/// How long each group of the toolbar holds out against the window narrowing,
/// the views it chooses longest and the panel toggles least.
const LAYOUT: Priority = 4;
const EDGES: Priority = 4;
const COLOUR: Priority = 3;
const MARK: Priority = 2;
const PANELS: Priority = 1;
const BOXES: Priority = 1;

/// The words naming each group, over it in the toolbar and on its row of the
/// overflow menu.
const LAYOUT_NAME: &str = "layout";
const EDGES_NAME: &str = "edges";
const COLOUR_NAME: &str = "colour";
const MARK_NAME: &str = "mark";

/// The toggles putting panels up and away, labelled and then icon-only.
fn panel_toggles(view: &GraphPanel, legend: Legend, cx: &Context<GraphPanel>) -> ToolbarGroup {
    let toggles = |labelled: bool| -> Vec<AnyElement> {
        let report = report::control(view, labelled, cx).map(IntoElement::into_any_element);
        let timecard = timecard::control(view, labelled, cx).map(IntoElement::into_any_element);
        let toggle = legend.clone().toggle();
        let legend = match labelled {
            true => toggle,
            false => toggle.icon_only(),
        }
        .into_any_element();
        [report, timecard]
            .into_iter()
            .flatten()
            .chain([legend])
            .collect()
    };
    ToolbarGroup::new(PANELS, row().children(toggles(true)))
        .or(row().children(toggles(false)))
        .parted()
}

/// Controls laid side by side.
fn row() -> Div {
    div().flex().items_center().gap_2()
}

/// What the drawing marks, and whether the graph offers to mark its critical
/// path at all.
struct Marking {
    neighbourhood: bool,
    critical_path: Option<bool>,
}

impl Marking {
    fn of(view: &GraphPanel, cx: &Context<GraphPanel>) -> Self {
        Self {
            neighbourhood: view.neighborhood_enabled,
            critical_path: view.traced(cx).then_some(view.critical_path_enabled),
        }
    }

    /// Whether the drawing marks other than a freshly opened graph does.
    fn set(&self) -> bool {
        self.neighbourhood != NEIGHBOURHOOD_AT_REST
            || self
                .critical_path
                .is_some_and(|on| on != CRITICAL_PATH_AT_REST)
    }

    /// The toggles choosing what the drawing marks.
    fn toggles(&self, view: &GraphPanel, cx: &Context<GraphPanel>) -> ToggleGroup {
        let panel = cx.entity();
        let focus = &view.focus_handle;
        let neighbourhood = Switch::new(
            ToggleSegment::new(NEIGHBOURS).keys_for(&ToggleNeighborhood, focus),
            self.neighbourhood,
            {
                let panel = panel.clone();
                move |_, _, cx| panel.update(cx, |view, cx| view.toggle_neighborhood(cx))
            },
        );
        let critical_path = self.critical_path.map(|enabled| {
            Switch::new(
                ToggleSegment::new(CRITICAL_PATH).keys_for(&ToggleCriticalPath, focus),
                enabled,
                move |_, _, cx| panel.update(cx, |view, cx| view.toggle_critical_path(cx)),
            )
        });
        ToggleGroup::any_of(
            "graph-marks",
            MARK_NAME,
            std::iter::once(neighbourhood).chain(critical_path),
        )
        .tooltip("Marks drawn")
    }
}

const NEIGHBOURS: &str = "neighbours";
const CRITICAL_PATH: &str = "critical path";

/// A picker as a group of a strip: standing beside the word naming it, then
/// alone, then as the overflow menu's row opening its choices.
pub(crate) struct PickerGroup<'a> {
    pub name: &'static str,
    pub dropdown: &'a Entity<Dropdown>,
    /// Steps through the picker's choices.
    pub action: &'static dyn Action,
    /// What the overflow menu says of each choice the picker cannot make.
    pub note: Option<SharedString>,
    /// Whether the picker stands other than at rest.
    pub set: bool,
}

impl PickerGroup<'_> {
    /// The group, holding out by `priority`, its overflow row naming the
    /// action's keys where `focus` stands.
    pub(crate) fn group(
        self,
        priority: Priority,
        focus: &FocusHandle,
        window: &Window,
        cx: &App,
    ) -> ToolbarGroup {
        let row = picker_row(
            self.name,
            self.dropdown,
            self.action,
            self.note.clone(),
            focus,
            window,
            cx,
        );
        ToolbarGroup::named(priority, self.name, || self.dropdown.clone()).overflow(row, self.set)
    }
}
