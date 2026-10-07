//! A chip drawn as a pill: its text, what it named, and its menu.

use super::{GraphPanel, edit, open_menu, reorder, select};
use crate::Palette;
use bviz_graph_layout::query_lang::{Chip, Counts, Outcome};
use bviz_ui_components::{Button, TextTooltip, TooltipColors};
use gpui::{
    ClickEvent, Context, Div, HighlightStyle, Hsla, MouseButton, MouseDownEvent, Pixels,
    SharedString, Stateful, StrikethroughStyle, StyledText, UnderlineStyle, div, prelude::*, px,
};
use std::ops::Range;

/// The weight of the line marking a chip's error or a disabled chip's text.
const MARK_THICKNESS: Pixels = px(1.);

/// The widest a pill stands: a longer text is cut at its start, which keeps
/// the end of a label, its name, in view; the tooltip reads it whole.
const PILL_MAX_WIDTH: Pixels = px(240.);

/// `chip`, which evaluated to `outcome` where it has been evaluated.
pub(super) fn pill(
    chip: &Chip,
    outcome: Option<Outcome>,
    selected: bool,
    editing: bool,
    palette: &Palette,
    cx: &mut Context<GraphPanel>,
) -> Stateful<Div> {
    let id = chip.id();
    let colors = &palette.colors;
    let ink = match chip.enabled() {
        true => colors.text,
        false => colors.text_disabled,
    };
    let error = chip.parsed().err().map(|error| error.span.clone());
    let marks = marks(chip.text(), error, !chip.enabled(), colors.error, ink);
    let text = StyledText::new(SharedString::from(chip.text().to_string())).with_highlights(marks);
    let explained = explain(chip, outcome.as_ref());
    let border = match selected {
        true => colors.border_focused,
        false => colors.border,
    };
    let fill = match editing {
        true => colors.selection,
        false => colors.panel,
    };
    let hover = colors.element_hover;
    div()
        .id(("graph-chip", chip.id().number() as usize))
        .flex()
        .items_center()
        .gap_1()
        .pl_2()
        .flex_none()
        .max_w(PILL_MAX_WIDTH)
        .rounded_md()
        .border_1()
        .border_color(border)
        .bg(fill)
        .text_sm()
        .text_color(ink)
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |view, event: &MouseDownEvent, window, cx| {
                reorder::press(view, id, event.position, window, cx);
            }),
        )
        .on_click(cx.listener(move |view, event: &ClickEvent, window, cx| {
            if reorder::swallows_click(view) {
                return;
            }
            let double = matches!(event, ClickEvent::Mouse(click) if click.down.click_count >= 2);
            match double {
                true => edit(view, id, window, cx),
                false => select(view, id, window, cx),
            }
        }))
        .children(
            chip.negated()
                .then(|| div().flex_none().text_color(colors.warning).child("not")),
        )
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis_start()
                .child(text),
        )
        .children(
            outcome
                .as_ref()
                .and_then(|outcome| count(outcome, palette))
                .map(|count| count.flex_none()),
        )
        .child(
            Button::new(
                ("graph-chip-menu", chip.id().number() as usize),
                cx.listener(move |view, event: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    open_menu(view, id, event.position(), window, cx);
                }),
            )
            .ghost()
            .compact()
            .icon("dots-three-vertical")
            .tooltip("Chip menu"),
        )
        .tooltip(TextTooltip::builder(
            explained,
            TooltipColors::from_colors(colors),
        ))
}

/// What the chip being typed named, beside the field, explaining itself by
/// its tooltip.
pub(super) fn draft(outcome: &Outcome, palette: &Palette) -> Option<Stateful<Div>> {
    let counted = count(outcome, palette)?;
    let explained = explain_outcome(outcome);
    let colors = TooltipColors::from_colors(&palette.colors);
    Some(
        div()
            .id("graph-chip-draft")
            .child(counted)
            .when_some(explained, |draft, explained| {
                draft.tooltip(TextTooltip::builder(explained, colors))
            }),
    )
}

/// How a chip's text is marked: a wavy underline under its error, and a line
/// through the whole of it while it is disabled.
fn marks(
    text: &str,
    error: Option<Range<usize>>,
    disabled: bool,
    error_ink: Hsla,
    ink: Hsla,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let underline = UnderlineStyle {
        thickness: MARK_THICKNESS,
        color: Some(error_ink),
        wavy: true,
    };
    let strikethrough = disabled.then_some(StrikethroughStyle {
        thickness: MARK_THICKNESS,
        color: Some(ink),
    });
    let style = |underlined: bool| HighlightStyle {
        underline: underlined.then_some(underline),
        strikethrough,
        ..HighlightStyle::default()
    };
    let Some(error) = error.map(|span| underlined(text, span)) else {
        return match disabled {
            true => vec![(0..text.len(), style(false))],
            false => Vec::new(),
        };
    };
    let mut marks = Vec::with_capacity(3);
    if disabled && error.start > 0 {
        marks.push((0..error.start, style(false)));
    }
    marks.push((error.clone(), style(true)));
    if disabled && error.end < text.len() {
        marks.push((error.end..text.len(), style(false)));
    }
    marks
}

/// The span of `text` an error at `span` underlines: the whole text where the
/// error points between characters, at the end of a chip cut short.
fn underlined(text: &str, span: Range<usize>) -> Range<usize> {
    let span = span.start.min(text.len())..span.end.min(text.len());
    match span.is_empty() {
        true => 0..text.len(),
        false => span,
    }
}

/// How many nodes a chip named, and how many fewer or more than the set it
/// read; what failed where it named nothing.
fn count(outcome: &Outcome, palette: &Palette) -> Option<Div> {
    let colors = &palette.colors;
    let (text, ink): (SharedString, _) = match outcome {
        Outcome::Disabled => ("off".into(), colors.text_disabled),
        Outcome::Invalid(_) => return None,
        Outcome::Blocked => ("–".into(), colors.text_disabled),
        Outcome::Failed(_) => ("error".into(), colors.error),
        Outcome::Named { counts, before } => (counted(*counts, *before).into(), colors.text_muted),
    };
    Some(div().text_xs().text_color(ink).child(text))
}

fn counted(counts: Counts, before: Option<Counts>) -> String {
    let nodes = counts.nodes;
    match before.map(|before| before.nodes) {
        Some(before) if before > nodes => format!("{nodes} −{}", before - nodes),
        Some(before) if before < nodes => format!("{nodes} +{}", nodes - before),
        _ => nodes.to_string(),
    }
}

/// What a chip's tooltip says, a line each: the chip's whole text, which the
/// pill may cut short; what it expands to, where that reads otherwise; and why
/// it does not parse or failed, or what its count is made of.
fn explain(chip: &Chip, outcome: Option<&Outcome>) -> SharedString {
    let explained = match chip.parsed() {
        Err(error) => Some(error.message.clone().into()),
        Ok(_) => outcome.and_then(explain_outcome),
    };
    let expanded = (chip.expanded() != chip.text()).then(|| chip.expanded());
    let role = match chip.is_source() {
        true => "a source, joining the union the stack starts from",
        false => "a refinement of the set before it",
    };
    let lines = [
        Some(chip.text()),
        expanded,
        Some(role),
        explained.as_deref(),
    ];
    let lines: Vec<&str> = lines.into_iter().flatten().collect();
    lines.join("\n").into()
}

fn explain_outcome(outcome: &Outcome) -> Option<SharedString> {
    match outcome {
        Outcome::Invalid(error) => Some(error.message.clone().into()),
        Outcome::Failed(error) => Some(error.message.clone().into()),
        Outcome::Blocked => Some("a chip before it does not evaluate".into()),
        Outcome::Disabled => None,
        Outcome::Named { counts, .. } => Some(
            format!(
                "{} nodes: {} targets, {} configured targets, {} actions",
                counts.nodes, counts.targets, counts.configured_targets, counts.actions
            )
            .into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_count_says_how_far_a_chip_moved_the_set_it_read() {
        let nodes = |nodes| Counts {
            nodes,
            ..Counts::default()
        };
        assert_eq!(counted(nodes(5), None), "5");
        assert_eq!(counted(nodes(5), Some(nodes(8))), "5 −3");
        assert_eq!(counted(nodes(9), Some(nodes(8))), "9 +1");
        assert_eq!(counted(nodes(8), Some(nodes(8))), "8");
    }

    /// A disabled chip is struck through whole, its error underlined as
    /// well.
    #[test]
    fn a_disabled_chip_is_struck_through_around_and_over_its_error() {
        let ink = Hsla::default();
        let marked = marks("kind( x", Some(4..5), true, ink, ink);
        let spans: Vec<_> = marked.iter().map(|(span, _)| span.clone()).collect();
        assert_eq!(spans, [0..4, 4..5, 5..7]);
        assert!(
            marked
                .iter()
                .all(|(_, style)| style.strikethrough.is_some())
        );
        assert!(marked[1].1.underline.is_some() && marked[0].1.underline.is_none());
        assert_eq!(marks("x", None, false, ink, ink).len(), 0);
    }

    /// The pill may cut its text short, so the tooltip leads with it whole,
    /// then what it expands to and which of the stack's groups it is in.
    #[test]
    fn the_tooltip_reads_the_whole_text_then_its_expansion_and_role() {
        use bviz_graph_layout::query_lang::Stack;
        let mut stack = Stack::default();
        let long = "//a/long/package/path/reaching/past/the/pill:and_its_name";
        let first = stack.push(long);
        let second = stack.push("except //a:b");
        let explained = explain(stack.chip(first).unwrap(), None);
        let lines: Vec<&str> = explained.lines().collect();
        assert_eq!(
            lines,
            [long, "a source, joining the union the stack starts from"]
        );
        let chip = stack.chip(second).unwrap();
        assert_ne!(chip.expanded(), chip.text());
        let explained = explain(chip, None);
        let lines: Vec<&str> = explained.lines().collect();
        assert_eq!(
            lines,
            [
                chip.text(),
                chip.expanded(),
                "a refinement of the set before it"
            ]
        );
    }

    #[test]
    fn an_error_between_characters_underlines_the_whole_chip() {
        assert_eq!(underlined("kind(x", 6..6), 0..6);
        assert_eq!(underlined("a + )", 4..5), 4..5);
        assert_eq!(underlined("ab", 1..9), 1..2);
    }
}
