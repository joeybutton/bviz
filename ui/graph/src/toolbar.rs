use crate::colour::{self, Colouring};
use crate::mode::{self, LayoutMode};
use crate::{
    CRITICAL_PATH_AT_REST, Controls, CycleColouring, GraphPanel, NEIGHBOURHOOD_AT_REST, Palette,
    ToggleCriticalPath, ToggleMap, ToggleNeighborhood, boxes, report, timecard,
};
use bviz_ui_components::{
    Dropdown, Legend, MenuItem, MultiSelect, OneRow, Presentation, Priority, Switch, ToggleGroup,
    ToggleSegment, ToolbarGroup, spinner,
};
use gpui::{
    Action, AnyElement, App, Context, Div, Entity, IntoElement, Pixels, SharedString, Window, div,
    prelude::*, px,
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
    window: &Window,
    cx: &Context<GraphPanel>,
) -> Div {
    let layout = ToolbarGroup::new(
        LAYOUT,
        group(LAYOUT_NAME, palette).child(mode::control(view)),
    )
    .or(unnamed(mode::control(view), palette))
    .overflow(
        picker_row(
            LAYOUT_NAME,
            &view.layout_picker,
            &ToggleMap,
            None,
            view,
            window,
            cx,
        ),
        view.mode != LayoutMode::default(),
    );
    let edges = ToolbarGroup::new(EDGES, unnamed(mode::reduce_toggle(view, cx), palette)).overflow(
        mode::reduce_row(view, EDGES_NAME, window, cx),
        mode::reduction(view).set(),
    );
    let colour = ToolbarGroup::new(
        COLOUR,
        group(COLOUR_NAME, palette).child(view.colour_picker.clone()),
    )
    .or(unnamed(view.colour_picker.clone(), palette))
    .overflow(
        picker_row(
            COLOUR_NAME,
            &view.colour_picker,
            &CycleColouring,
            Some(colour::hint(view.model.read(cx))),
            view,
            window,
            cx,
        ),
        view.colouring != Colouring::default(),
    );
    let marking = Marking::of(view, cx);
    let marks = ToolbarGroup::new(
        MARK,
        group(MARK_NAME, palette).child(marking.toggles(view, cx)),
    )
    .or(unnamed(marking.toggles(view, cx), palette))
    .or(unnamed(
        marking
            .toggles(view, cx)
            .presentation(Presentation::Collapsed),
        palette,
    ))
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
            row.status([
                busy("relaying-spinner", "laying out…", palette).into_any_element(),
                spinner("relaying-spinner", SPINNER_SIZE, palette.label).into_any_element(),
            ])
        })
        .trailing(panel_toggles(view, controls.legend, cx));
    let query = OneRow::new("graph-query-bar")
        .when(!view.box_classes.is_empty(), |query| {
            query.group(ToolbarGroup::new(BOXES, view.boxes.clone()).overflow(
                MenuItem::submenu(boxes::NAME, MultiSelect::submenu(&view.boxes, cx)),
                boxes::any_hidden(view.derived.filter()),
            ))
        })
        .filling(controls.chips);
    let strips: [OneRow; STRIPS] = [drawing, query];
    div().size_full().flex().flex_col().children(strips)
}

/// `picker` as the row of the overflow menu opening its choices, naming the
/// keys of the `action` stepping through them, each choice it cannot make
/// explained by `note`.
fn picker_row(
    name: &'static str,
    picker: &Entity<Dropdown>,
    action: &dyn Action,
    note: Option<SharedString>,
    view: &GraphPanel,
    window: &Window,
    cx: &App,
) -> MenuItem {
    MenuItem::submenu(name, Dropdown::submenu(picker, note, cx)).keys_for(
        action,
        &view.focus_handle,
        window,
    )
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
    }
}

const NEIGHBOURS: &str = "neighbours";
const CRITICAL_PATH: &str = "critical path";

/// A strip's controls grouped under the word `name`.
pub(crate) fn group(name: &'static str, palette: &Palette) -> Div {
    labelled(palette).child(name)
}

/// `controls` set as they stand under their group's name, so that they read
/// alike whether or not the name stands beside them.
fn unnamed(controls: impl IntoElement, palette: &Palette) -> Div {
    labelled(palette).child(controls)
}

/// The cue a strip shows that the work `doing` names is in flight, its spinner
/// keyed by `id`.
pub(crate) fn busy(id: &'static str, doing: &'static str, palette: &Palette) -> impl IntoElement {
    labelled(palette)
        .child(spinner(id, SPINNER_SIZE, palette.label))
        .child(doing)
}

const SPINNER_SIZE: Pixels = px(14.);

/// A row of a strip set in the strip's label colour.
fn labelled(palette: &Palette) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .text_color(palette.label)
}
