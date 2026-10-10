use super::{Shown, Slow, Timecard, words};
use crate::column::{Folding, line, scrolling};
use crate::legend;
use crate::model::Loaded;
use crate::reveal::reveal;
use crate::timing::{Ran, Slowest};
use crate::{GraphPanel, Palette};
use bviz_graph_layout::Id;
use bviz_ui_components::gesture::{
    Button as PointerButton, Entry, GestureRow, Gestures, Input, Place, Pointer,
};
use bviz_ui_components::{Button, ListScroll, TextTooltip, TooltipColors};
use gpui::{
    App, ClickEvent, Context, Div, Entity, ListSizingBehavior, Stateful, div, prelude::*,
    uniform_list,
};

/// What the card does for a gesture in [`GESTURES`].
#[derive(Clone, Copy)]
enum TimecardIntent {
    JumpTo,
}

const GESTURES: Gestures<TimecardIntent> = Gestures::new(&[Entry::new(
    Input::click(PointerButton::Left),
    Place::On("a slow action"),
    "Jump to node",
    TimecardIntent::JumpTo,
)]);

/// The gestures the card answers to.
pub(crate) fn gestures() -> impl Iterator<Item = GestureRow> {
    GESTURES.rows()
}

/// What the panel holds of the card saying where the profile's action time
/// went.
pub(crate) struct Card {
    up: bool,
    scroll: ListScroll,
}

impl Card {
    pub(crate) fn new() -> Self {
        Card {
            up: false,
            scroll: ListScroll::new(),
        }
    }
}

/// The loaded graph, where the profile of its invocation records any action,
/// whether or not the graph places it; `None` otherwise.
fn timed<'a>(view: &GraphPanel, cx: &'a App) -> Option<&'a Loaded> {
    let loaded = view.model.read(cx).loaded()?;
    (!loaded.timecard().is_empty()).then_some(loaded)
}

fn toggle(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    view.timecard.up = !view.timecard.up;
    cx.notify();
}

/// The toolbar's control putting the card up and away; `None` where no
/// profile records an action.
pub(crate) fn control(view: &GraphPanel, cx: &Context<GraphPanel>) -> Option<Button> {
    timed(view, cx)?;
    Some(
        Button::new(
            "timecard-toggle",
            cx.listener(|view, _: &ClickEvent, _, cx| toggle(view, cx)),
        )
        .label("timing")
        .tooltip("Toggle action timing")
        .selected(view.timecard.up),
    )
}

/// The row `index` of the card's list, which a click shows the node it is
/// [shown](Shown) on where the graph holds one; `mnemonic` says what kind of
/// action it lists.
fn row(
    index: usize,
    slow: &Slow,
    mnemonic: Option<&str>,
    view: &Entity<GraphPanel>,
    palette: &Palette,
) -> Stateful<Div> {
    let colors = &palette.colors;
    let name = div()
        .flex()
        .gap_1()
        .min_w_0()
        .children(mnemonic.map(|mnemonic| {
            div()
                .flex_none()
                .whitespace_nowrap()
                .child(mnemonic.to_owned())
        }))
        // Paths sharing a directory differ at their tails.
        .child(line(slow.subject().to_owned()).text_ellipsis_start());
    let listed = div()
        .id(("timecard-row", index))
        .w_full()
        .flex()
        .items_center()
        .gap_1()
        .px_1()
        .rounded_sm()
        .tooltip(TextTooltip::builder(
            slow.hover(mnemonic).into(),
            TooltipColors::from_colors(colors),
        ))
        .child(legend::ran(Ran::On(slow.timing.runner), palette))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(name)
                .child(line(slow.detail()).text_color(colors.text_muted)),
        );
    match slow.shown.node() {
        Some(node) => {
            let (hover, view) = (colors.element_hover, view.clone());
            listed
                .cursor_pointer()
                .hover(move |row| row.bg(hover))
                .on_click(move |event: &ClickEvent, _, cx| {
                    GESTURES.dispatch(Pointer::click(event), |intent| match intent {
                        TimecardIntent::JumpTo => {
                            view.update(cx, |view, cx| reveal(view, node, cx));
                            true
                        }
                    });
                })
        }
        None => listed.text_color(colors.text_muted),
    }
}

/// The `rows` slowest actions.
fn listed(view: &GraphPanel, rows: usize, palette: &Palette, cx: &Context<GraphPanel>) -> Div {
    let (entity, palette) = (cx.entity(), *palette);
    let rows = uniform_list("timecard-rows", rows, move |range, _, cx| {
        let Some(loaded) = timed(entity.read(cx), cx) else {
            return Vec::new();
        };
        range
            .clone()
            .zip(loaded.timecard().listed(range))
            .map(|(index, slow)| {
                let mnemonic = match slow.shown {
                    Shown::Action(node) => mnemonic(loaded, node),
                    Shown::Owner { .. } | Shown::Off => None,
                };
                row(index, slow, mnemonic, &entity, &palette)
            })
            .collect::<Vec<_>>()
    })
    .with_sizing_behavior(ListSizingBehavior::Infer)
    .track_scroll(view.timecard.scroll.uniform_handle());
    scrolling(
        rows,
        |view| &view.timecard.scroll,
        |view| &mut view.timecard.scroll,
        &palette,
        cx,
    )
}

/// The share each runner holds of `timecard`'s actions, each beside the fill
/// a box it ran wears.
fn runners(timecard: &Timecard, palette: &Palette) -> impl Iterator<Item = Div> {
    let palette = *palette;
    timecard.runner_lines().map(move |(ran, said)| {
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(legend::ran(ran, &palette))
            .child(line(said))
    })
}

/// The mnemonic of the action node `id`, where the graph's inputs state one.
fn mnemonic(loaded: &Loaded, id: Id) -> Option<&str> {
    let facets = &loaded.shared.facets;
    Some(facets.strings().resolve(facets.mnemonic(id)?))
}

/// What the profile says of the selected box `node`, over the control
/// showing the slowest action the box holds, which is offered where that
/// action is not the box itself.
fn selected(node: Id, loaded: &Loaded, cx: &Context<GraphPanel>) -> Div {
    let timed = loaded.times().ok().and_then(|times| times.get(node));
    let slowest = timed
        .and_then(|timed| timed.slowest)
        .filter(|slowest| slowest.action != node);
    let said = div().w_full().min_w_0().child(words::selected(node, timed));
    div()
        .flex()
        .flex_col()
        .items_end()
        .gap_1()
        .child(said)
        .children(slowest.map(|slowest| {
            let Slowest { action, .. } = slowest;
            Button::new(
                "timecard-slowest",
                cx.listener(move |view, _: &ClickEvent, _, cx| reveal(view, action, cx)),
            )
            .label(words::slowest(slowest, mnemonic(loaded, action)))
            .compact()
        }))
}

/// Where the profile's action time went, for the caller to place over the
/// drawing; `None` while the card is away, and where no profile records an
/// action.
pub(crate) fn card(
    view: &GraphPanel,
    palette: &Palette,
    cx: &Context<GraphPanel>,
) -> Option<Folding> {
    if !view.timecard.up {
        return None;
    }
    let loaded = timed(view, cx)?;
    let timecard = loaded.timecard();
    let colors = &palette.colors;
    let head = div()
        .flex()
        .flex_col()
        .gap_1()
        .text_xs()
        .text_color(palette.label)
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(line(timecard.line()))
                .child(
                    Button::new(
                        "timecard-close",
                        cx.listener(|view, _: &ClickEvent, _, cx| toggle(view, cx)),
                    )
                    .label("close")
                    .tooltip("Close card")
                    .compact(),
                ),
        )
        .children(runners(timecard, palette))
        .children(view.selection.node().map(|node| selected(node, loaded, cx)))
        .child(
            div()
                .min_w_0()
                .pt_1()
                .text_color(colors.text_muted)
                .child(timecard.list_line()),
        );
    let rows = timecard.rows();
    let rows = (rows > 0).then(|| listed(view, rows, palette, cx));
    Some(Folding::new(head, rows, palette))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_gesture_is_reachable_and_fits_the_help() {
        assert_eq!(GESTURES.unreachable(), []);
        assert_eq!(GESTURES.overlong(), []);
    }
}
