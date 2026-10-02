use super::{Report, Row};
use crate::column::{Folding, line};
use crate::legend::status_swatch;
use crate::model::Loaded;
use crate::{GraphPanel, Palette, reveal};
use bviz_invocation::Configurations;
use bviz_ui_components::{
    Button, ListScroll, SCROLLBAR_THICKNESS, TextTooltip, TooltipColors, list_scrollbar,
};
use gpui::{
    App, ClickEvent, Context, Div, Entity, ListSizingBehavior, MouseButton, MouseDownEvent,
    Stateful, div, prelude::*, px, uniform_list,
};

/// What the panel holds of the card reading what the build reported.
pub(crate) struct Card {
    /// Where the reader put the card, up or away; `None` until they do
    /// either, while it is up for a build that left something to look into.
    chosen: Option<bool>,
    scroll: ListScroll,
}

impl Card {
    pub(crate) fn new() -> Self {
        Card {
            chosen: None,
            scroll: ListScroll::new(),
        }
    }

    fn is_up(&self, report: &Report) -> bool {
        self.chosen.unwrap_or_else(|| report.troubled())
    }

    /// Puts the card reading `report` away where it is up, and up where it
    /// is away.
    fn toggle(&mut self, report: &Report) {
        self.chosen = Some(!self.is_up(report));
    }
}

/// What the card reads of a graph whose build reported anything.
#[derive(Clone, Copy)]
struct Reported<'a> {
    loaded: &'a Loaded,
    /// The configurations of the invocation the build is of.
    configurations: &'a Configurations,
}

impl<'a> Reported<'a> {
    /// `None` while no graph is loaded, and for one whose build reported
    /// nothing: that of a graph opened as part of no invocation did not.
    fn of(view: &GraphPanel, cx: &'a App) -> Option<Self> {
        let model = view.model.read(cx);
        let loaded = model.loaded()?;
        let configurations = model.configurations(cx)?;
        (!loaded.reported().is_empty()).then_some(Reported {
            loaded,
            configurations,
        })
    }

    fn report(self) -> &'a Report {
        self.loaded.reported()
    }
}

fn toggle(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    if let Some(reported) = Reported::of(view, cx) {
        view.report.toggle(reported.report());
        cx.notify();
    }
}

/// The toolbar's control putting the card up and away; `None` where the build
/// reported nothing for the card to read.
pub(crate) fn control(view: &GraphPanel, cx: &Context<GraphPanel>) -> Option<Button> {
    let report = Reported::of(view, cx)?.report();
    Some(
        Button::new(
            "report-toggle",
            cx.listener(|view, _: &ClickEvent, _, cx| toggle(view, cx)),
        )
        .label("outcomes")
        .selected(view.report.is_up(report)),
    )
}

/// The row `index` of the card's list, which a click shows the node of where
/// the graph holds one to show. A configuration is named as `configurations`
/// names it.
fn row(
    index: usize,
    row: &Row,
    configurations: &Configurations,
    view: &Entity<GraphPanel>,
    palette: &Palette,
) -> Stateful<Div> {
    let colors = &palette.colors;
    let name = div()
        .flex()
        .gap_1()
        .min_w_0()
        .children(
            row.lead()
                .map(|lead| div().flex_none().whitespace_nowrap().child(lead)),
        )
        // Labels sharing a package differ at their tails.
        .child(line(row.subject(configurations)).text_ellipsis_start());
    let listed = div()
        .id(("report-row", index))
        .w_full()
        .flex()
        .items_center()
        .gap_1()
        .px_1()
        .rounded_sm()
        .tooltip(TextTooltip::builder(
            row.hover(configurations).into(),
            TooltipColors::from_colors(colors),
        ))
        .child(status_swatch(row.listed.status(), palette).flex_none())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(name)
                .child(line(row.detail()).text_color(colors.text_muted)),
        );
    match row.shown.node() {
        Some(node) => {
            let (hover, view) = (colors.element_hover, view.clone());
            listed
                .cursor_pointer()
                .hover(move |row| row.bg(hover))
                .on_click(move |_, _, cx| view.update(cx, |view, cx| reveal(view, node, cx)))
        }
        None => listed.text_color(colors.text_muted),
    }
}

/// The `rows` rows of the card, as tall as they stand until the card runs
/// out of room for them, and from there scrolling under a bar the rows keep
/// clear of.
fn listed(view: &GraphPanel, rows: usize, palette: &Palette, cx: &Context<GraphPanel>) -> Div {
    let (entity, palette) = (cx.entity(), *palette);
    let rows = uniform_list("report-rows", rows, {
        let entity = entity.clone();
        move |range, _, cx| {
            let reported = Reported::of(entity.read(cx), cx);
            let rows = reported.into_iter().flat_map(|reported| {
                let listed = reported.report().listed(range.clone());
                listed.map(move |listed| (listed, reported.configurations))
            });
            range
                .clone()
                .zip(rows)
                .map(|(index, (listed, configurations))| {
                    row(index, listed, configurations, &entity, &palette)
                })
                .collect::<Vec<_>>()
        }
    })
    .with_sizing_behavior(ListSizingBehavior::Infer)
    .w_full()
    .pr(px(SCROLLBAR_THICKNESS))
    .track_scroll(view.report.scroll.uniform_handle());
    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .text_xs()
        .text_color(palette.label)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|view, e: &MouseDownEvent, _, cx| {
                view.report.scroll.press(e.position);
                cx.notify();
            }),
        )
        .child(rows)
        .child(list_scrollbar(
            entity,
            |view: &GraphPanel| &view.report.scroll,
            |view: &mut GraphPanel| &mut view.report.scroll,
            palette.scroll,
        ))
}

/// What the build reported, for the caller to place over the drawing; `None`
/// while the card is away, and where the build reported nothing.
pub(crate) fn card(
    view: &GraphPanel,
    palette: &Palette,
    cx: &Context<GraphPanel>,
) -> Option<Folding> {
    let report = Reported::of(view, cx)?.report();
    if !view.report.is_up(report) {
        return None;
    }
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
                .child(line(report.builds.line()))
                .child(
                    Button::new(
                        "report-close",
                        cx.listener(|view, _: &ClickEvent, _, cx| toggle(view, cx)),
                    )
                    .label("close")
                    .compact(),
                ),
        )
        .children(report.tests.line().map(line))
        .children(
            report
                .mismatches
                .lines()
                .map(|mismatch| div().min_w_0().text_color(colors.warning).child(mismatch)),
        );
    let rows = report.rows();
    let rows = (rows > 0).then(|| listed(view, rows, palette, cx));
    Some(Folding::new(head, rows, palette))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_is_up_for_a_troubled_build_until_the_reader_puts_it_elsewhere() {
        let (quiet, troubled) = super::super::tests::quiet_and_troubled();
        assert!(!quiet.troubled() && troubled.troubled());

        let mut card = Card::new();
        assert!(!card.is_up(&quiet));
        assert!(card.is_up(&troubled));

        card.toggle(&troubled);
        assert!(!card.is_up(&troubled) && !card.is_up(&quiet));
        card.toggle(&troubled);
        assert!(card.is_up(&troubled) && card.is_up(&quiet));

        let mut card = Card::new();
        card.toggle(&quiet);
        assert!(card.is_up(&quiet));
    }
}
