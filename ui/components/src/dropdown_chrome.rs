//! The chrome a control that opens a menu under a trigger is drawn from, so the
//! controls built on it read alike wherever they sit.

use crate::{CONTROL_HEIGHT, laid_out_width};
use bviz_ui_theme::{ThemeColors, over};
use gpui::{
    Div, ElementId, Hsla, MouseButton, Pixels, SharedString, Stateful, Svg, Window, div,
    prelude::*, px,
};

/// Breathing room between a trigger and the menu it opens.
const MENU_GAP: Pixels = px(2.);

/// Side of the marks a menu's chrome draws.
pub(crate) const MENU_ICON: Pixels = px(12.);

/// Side of the square a group's fold mark is given, wider than the mark itself
/// so that what folds a group is no harder to hit than what checks it.
const MENU_FOLD: Pixels = px(18.);

/// Room a row leaves above and below its label.
const ROW_PADDING: Pixels = px(4.);

/// Room a row leaves at either end of what it holds, set in pixels like the rest
/// of a row's spacing so it holds whatever the root font size.
pub(crate) const ROW_INSET: Pixels = px(8.);

/// The line drawn around a sheet.
pub(crate) const SHEET_BORDER: Pixels = px(1.);

/// How tall a row stands under the window's inherited text style.
pub(crate) fn row_height(window: &Window) -> Pixels {
    bviz_ui_assets::text_metrics(window).line_height + ROW_PADDING * 2.
}

/// `bound` cut down to the whole rows `row` tall it holds, by which a list that
/// scrolls within it ends on a row entire rather than through the glyphs of one.
/// A bound shorter than a row holds the one row the list must still show.
pub(crate) fn whole_rows(row: Pixels, bound: Pixels) -> Pixels {
    row * (f32::from(bound) / f32::from(row)).floor().max(1.)
}

/// The fill under the row the cursor or the keyboard is on, over the sheet the
/// row stands on.
fn aimed_fill(colors: &ThemeColors) -> Hsla {
    Hsla::from(over(colors.element_hover, colors.panel))
}

/// The fill under the cursor on a row already chosen, composited so that row
/// keeps its accent while the cursor is on it.
fn chosen_hover(colors: &ThemeColors) -> Hsla {
    Hsla::from(over(
        colors.element_hover,
        over(colors.selection, colors.panel),
    ))
}

/// The button a menu opens under. The caller adds what it reads and the click
/// that opens the menu.
pub(crate) fn trigger(id: ElementId, colors: &ThemeColors) -> Stateful<Div> {
    let hover = colors.element_hover;
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px_2()
        .h(CONTROL_HEIGHT)
        // Hold the trigger's measured width in a flex parent (e.g. a toolbar);
        // without this it shrinks and the label wraps to a second line.
        .flex_none()
        .rounded_md()
        .cursor_pointer()
        .text_color(colors.text)
        .hover(move |style| style.bg(hover))
        // Keep the press from reaching an ancestor that focuses on mouse-down
        // (e.g. a toolbar); that would blur the menu and, via the controls'
        // dismiss-on-blur, close it just before this click reopens it.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// The mark on a trigger saying it opens a menu.
pub(crate) fn caret(colors: &ThemeColors) -> Svg {
    mark("caret-down", colors)
}

fn mark(icon: &'static str, colors: &ThemeColors) -> Svg {
    bviz_ui_assets::icon(icon)
        .size(MENU_ICON)
        .text_color(colors.text)
}

/// A line of text held to `width`, so what a control reads does not change how
/// wide it is drawn, and cut short where it runs wider than that.
pub(crate) fn reading(width: Pixels, label: SharedString) -> Div {
    div()
        .w(width)
        .flex_none()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .child(label)
}

/// The width of the widest of `labels` under the window's inherited text style.
pub(crate) fn widest(window: &mut Window, labels: impl Iterator<Item = SharedString>) -> Pixels {
    let metrics = bviz_ui_assets::text_metrics(window);
    let color = window.text_style().color;
    let mut measured = px(0.);
    for label in labels {
        let width = bviz_ui_assets::shape_line(
            window,
            label,
            metrics.font.clone(),
            metrics.font_size,
            color,
        )
        .width;
        measured = measured.max(width);
    }
    laid_out_width(measured)
}

/// The sheet a menu's rows sit on, anchored under the trigger. The caller defers
/// it so it paints over the content below; its layout stays in place.
///
/// No explicit width: the sheet shrinks to fit its rows, so a row never wraps.
pub(crate) fn sheet(colors: &ThemeColors) -> Div {
    surface(
        div()
            .occlude()
            .absolute()
            .top(CONTROL_HEIGHT + MENU_GAP)
            .left_0()
            .flex()
            .flex_col(),
        colors,
    )
}

/// `element` given the look of a sheet a menu's rows sit on, wherever it
/// stands.
pub(crate) fn surface<E: Styled>(element: E, colors: &ThemeColors) -> E {
    element
        .bg(colors.panel)
        .border(SHEET_BORDER)
        .border_color(colors.border)
        .rounded_md()
}

/// The row a menu's own actions stand on, above the values they act on and out
/// of the list those values scroll in. It is laid out like a row, so what it
/// holds lines up with what the rows hold.
pub(crate) fn header(colors: &ThemeColors) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_end()
        .px(ROW_INSET)
        .py(ROW_PADDING)
        .border_b_1()
        .border_color(colors.border)
}

/// One row of a menu, `chosen` filling in the value it names as one that stands.
pub(crate) fn row(id: ElementId, colors: &ThemeColors, chosen: bool) -> Stateful<Div> {
    let hover = if chosen {
        chosen_hover(colors)
    } else {
        aimed_fill(colors)
    };
    row_frame(id, colors)
        .when(chosen, |row| row.bg(colors.selection))
        .hover(move |style| style.bg(hover))
}

/// One row of a menu whose fill follows an aim the caller keeps, moved by the
/// cursor and the keys alike, so that the two never fill a row each.
pub(crate) fn aimed_row(id: ElementId, colors: &ThemeColors, aimed: bool) -> Stateful<Div> {
    row_frame(id, colors).when(aimed, |row| row.bg(aimed_fill(colors)))
}

fn row_frame(id: ElementId, colors: &ThemeColors) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px(ROW_INSET)
        .py(ROW_PADDING)
        .cursor_pointer()
        .text_color(colors.text)
}

/// The mark a group's row carries saying whether the values under it are listed
/// or folded away, which the caller hangs the gesture folding them on.
pub(crate) fn fold(id: ElementId, colors: &ThemeColors, folded: bool) -> Stateful<Div> {
    let hover = colors.element_hover;
    div()
        .id(id)
        .size(MENU_FOLD)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .cursor_pointer()
        .hover(move |mark| mark.bg(hover))
        .child(mark(
            if folded { "caret-right" } else { "caret-down" },
            colors,
        ))
}

/// The room a row leaves where it heads no group, so that in a menu holding
/// groups the values line up under the rows heading them.
pub(crate) fn gutter() -> Div {
    div().size(MENU_FOLD).flex_none()
}

/// The check mark a row carries while the value it names is chosen, and the
/// space it leaves where it does not, so the labels of a menu's rows line up
/// whichever of them are checked.
pub(crate) fn check(colors: &ThemeColors, checked: bool) -> Div {
    slot(checked.then_some("check"), colors)
}

/// The mark at the trailing end of a row that opens a submenu, and the space it
/// leaves where the row opens none, so the labels of a menu's rows line up
/// whichever of them open one.
pub(crate) fn submenu_mark(colors: &ThemeColors, opens: bool) -> Div {
    slot(opens.then_some("caret-right"), colors)
}

/// The room a row keeps for a mark, holding `icon` where it has one.
fn slot(icon: Option<&'static str>, colors: &ThemeColors) -> Div {
    div()
        .size(MENU_ICON)
        .flex_none()
        .children(icon.map(|icon| mark(icon, colors)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_ui_theme::{
        MIN_TEXT_CONTRAST, MIN_VISIBLE_STEP, built_in_themes, contrast_ratio, level_step,
    };

    /// A row names what choosing it does, so its label has to be read on the
    /// fill the row carries while the cursor or the keyboard is on it.
    #[test]
    fn the_aimed_row_keeps_its_label() {
        for theme in built_in_themes() {
            let colors = &theme.colors;
            let contrast = contrast_ratio(colors.text, aimed_fill(colors));
            assert!(
                contrast >= MIN_TEXT_CONTRAST,
                "{}: {contrast}:1",
                theme.name
            );
        }
    }

    /// Which row the cursor or the keyboard is on is said by the fill under it
    /// alone, so that fill has to stand off the sheet's own surface.
    #[test]
    fn the_aimed_row_stands_off_the_rows_around_it() {
        for theme in built_in_themes() {
            let colors = &theme.colors;
            let step = level_step(aimed_fill(colors), colors.panel);
            assert!(step >= MIN_VISIBLE_STEP, "{}: {step} levels", theme.name);
        }
    }

    /// A row names the value it stands for, so the label has to be read on the
    /// fill the row carries — including the deepest one, where the cursor rests
    /// on a row already chosen.
    #[test]
    fn a_chosen_row_under_the_cursor_keeps_its_label() {
        for theme in built_in_themes() {
            let colors = &theme.colors;
            let contrast = contrast_ratio(colors.text, chosen_hover(colors));
            assert!(
                contrast >= MIN_TEXT_CONTRAST,
                "{}: {contrast}:1",
                theme.name
            );
        }
    }

    /// Bringing the cursor onto a row already chosen has to show, or the row the
    /// cursor rests on is told only by the choice already made.
    #[test]
    fn the_cursor_shows_on_a_row_already_chosen() {
        for theme in built_in_themes() {
            let colors = &theme.colors;
            let chosen = over(colors.selection, colors.panel);
            let step = level_step(chosen_hover(colors), chosen);
            assert!(step >= MIN_VISIBLE_STEP, "{}: {step} levels", theme.name);
        }
    }
}
