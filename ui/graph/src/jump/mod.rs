//! The jump palette: a fuzzy finder over the nodes of the panel's graph that
//! moves the view onto the one picked. Finding never changes what is drawn;
//! the filter is what does.

mod index;

use crate::derive::Drawn;
use crate::framing::{Frame, Held};
use crate::fuzzy::{self, Matching, Row, Standing, TEXT_SIZE};
use crate::mode::LayoutMode;
use crate::model::{Prepared, Shared};
use crate::point::kind_names;
use crate::reveal::{self, hidden_by};
use crate::{GraphPanel, Palette, chips, pins, why};
use bviz_graph_layout::select::Visibility;
use bviz_graph_layout::select::labels::{carried_label, package_label};
use bviz_graph_layout::{Id, Kind};
use bviz_ui_components::text_input::TextEdited;
use bviz_ui_components::{
    KeyScope, ListScroll, Nesting, SCROLLBAR_THICKNESS, TextInput, TextTooltip, TooltipColors,
    list_scrollbar, menu_chrome, overlay_canvas,
};
use gpui::{
    Bounds, ClickEvent, Context, Div, Entity, Focusable, Hsla, MouseButton, MouseDownEvent, Pixels,
    Rems, ScrollStrategy, SharedString, Stateful, Subscription, TextStyleRefinement, Window,
    actions, div, prelude::*, px, relative, rems, uniform_list,
};
use index::{Candidate, Scope};
pub(crate) use index::{Candidates, labelled, package_of};
use rustc_hash::FxHashSet;

actions!(
    bviz_graph_jump,
    [
        /// Open jump palette
        Open,
        /// Preview previous match
        Previous,
        /// Preview next match
        Next,
        /// Jump to match
        Go,
        /// Show why match is in build
        AskWhy,
        /// Always show match
        AlwaysShow,
        /// Add match as query chip
        AddChip,
        /// Close palette and go back
        Close,
        /// Search next scope
        NextScope,
        /// Search previous scope
        PreviousScope,
    ]
);

/// The key context the palette carries, scoping the keys that move through
/// and act on its rows.
pub(crate) const KEY_CONTEXT: KeyScope = KeyScope::within(
    &crate::KEY_CONTEXT,
    Nesting::Inside,
    "GraphJump",
    "Jump palette",
);

const WIDTH: Pixels = px(560.);

const PLACEHOLDER: &str = "jump to…";

/// The size the scope's name is set in, which its slot is measured at.
const SCOPE_TEXT_SIZE: Rems = rems(0.75);

/// The most rows the list stands tall, few enough to read at a glance; the
/// rest of the matches scroll through it.
const VISIBLE_ROWS: usize = 12;

/// How much more readily what holds a row's node gives up width than the
/// row's own text, enough that it is cut away before the text is cut at all.
const CONTAINER_SHRINK: f32 = 1000.;

/// The panel's jump palette.
#[derive(Default)]
pub(crate) struct Finder {
    /// The palette while it is open over the drawing.
    open: Option<Jump>,
    /// How far the open palette's rows have scrolled.
    scroll: ListScroll,
}

/// The palette while it is open over the drawing.
struct Jump {
    field: Entity<TextInput>,
    _edits: Subscription,
    scope: Scope,
    /// Matches the field's query against the indices of the candidates
    /// `scope` offers.
    matching: Matching,
    candidates: Prepared<Candidates>,
    /// The row the keys are on, which is previewed once a key moved it.
    aimed: usize,
    /// Where the palette was last painted, the band of the canvas a preview
    /// keeps the node it frames clear of.
    painted: Option<Bounds<Pixels>>,
    /// Where the view stood as the palette opened, and in which mode, so that
    /// closing it unpicked returns there.
    opened_at: Option<(LayoutMode, Held)>,
}

/// Opens the palette over the drawing, or takes the focus back to it where it
/// is open already. A graph still being read offers nothing to find.
pub(crate) fn open(view: &mut GraphPanel, window: &mut Window, cx: &mut Context<GraphPanel>) {
    if view.jump.open.is_none() {
        let Some(candidates) = view.model.read(cx).candidates() else {
            return;
        };
        let field = cx.new(|cx| TextInput::new(cx, PLACEHOLDER, 0));
        let edits = cx.subscribe(&field, |view, _, _: &TextEdited, cx| edited(view, cx));
        view.jump.open = Some(Jump {
            field,
            _edits: edits,
            scope: Scope::default(),
            matching: view.matchers.take(),
            candidates,
            aimed: 0,
            painted: None,
            opened_at: view.framing.hold().map(|held| (view.mode, held)),
        });
        view.jump.scroll.to_top();
        inject(view, cx);
    }
    if let Some(jump) = &view.jump.open {
        let field = jump.field.read(cx).focus_handle(cx);
        field.focus(window, cx);
    }
    cx.notify();
}

/// The open palette's matching.
fn matching(view: &mut GraphPanel) -> Option<&mut Matching> {
    Some(&mut view.jump.open.as_mut()?.matching)
}

/// Hands the matcher the candidates the scope offers, in place of any it
/// held, off the UI thread.
fn inject(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let Some(jump) = view.jump.open.as_ref() else {
        return;
    };
    let (candidates, scope) = (jump.candidates.clone(), jump.scope);
    let feed = move |feeder: &fuzzy::Feeder| {
        let candidates = candidates.get();
        for index in candidates.in_scope(scope) {
            if !feeder.push(index, &candidates.get(index).text) {
                return;
            }
        }
    };
    fuzzy::inject(view, matching, take_up, feed, cx);
}

/// Hands the matcher the query the field holds.
fn edited(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let Some(jump) = view.jump.open.as_mut() else {
        return;
    };
    let query = jump.field.read(cx).content();
    if !jump.matching.set_query(query) {
        return;
    }
    jump.aimed = 0;
    view.jump.scroll.to_top();
    fuzzy::poll(view, matching, take_up, cx);
}

/// Takes up what the matcher has found since the last poll, keeping the aim
/// on the row it was on.
fn take_up(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let drawn = drawn_ids(view);
    let Some(jump) = view.jump.open.as_mut() else {
        return;
    };
    let was = jump.aimed;
    let aimed = jump.matching.rows().get(was).map(|row| row.candidate);
    rerank(jump, &drawn);
    let rows = jump.matching.rows();
    jump.aimed = aimed
        .and_then(|aimed| rows.iter().position(|row| row.candidate == aimed))
        .unwrap_or(0);
    // The list follows the aim only when the aimed row moves, so a reader
    // scrolling it while matches stream in keeps their place.
    if jump.aimed != was {
        let aimed = jump.aimed;
        bring_into_view(&view.jump.scroll, aimed);
    }
    cx.notify();
}

/// Lists the rows the matcher's latest result ranks first.
fn rerank(jump: &mut Jump, drawn: &FxHashSet<Id>) {
    let candidates = &jump.candidates;
    jump.matching.rerank(|index| {
        // An item is pushed only once the candidates are prepared, so reading
        // them here never waits on their preparation.
        let candidate = candidates.get().get(index);
        Standing {
            path: candidate.path.clone(),
            drawn: drawn.contains(&candidate.id),
            weight: candidate.rdeps,
        }
    });
}

/// The nodes the drawing holds a box for at the zoom it is drawn at.
pub(crate) fn drawn_ids(view: &GraphPanel) -> FxHashSet<Id> {
    let drawn = view.drawn();
    let laid_out = drawn
        .as_ref()
        .and_then(|drawn| drawn.view(view.view_zoom(drawn)?));
    laid_out.map_or_else(FxHashSet::default, |laid_out| {
        laid_out.boxes.iter().map(|b| b.id).collect()
    })
}

/// Moves to the next scope, or with `back` the previous one, and offers its
/// candidates under the same query.
fn cycle_scope(view: &mut GraphPanel, back: bool, cx: &mut Context<GraphPanel>) {
    let Some(jump) = view.jump.open.as_mut() else {
        return;
    };
    jump.scope = match back {
        true => jump.scope.previous(),
        false => jump.scope.next(),
    };
    jump.aimed = 0;
    view.jump.scroll.to_top();
    inject(view, cx);
    cx.notify();
}

/// Moves the aim `step` rows and previews the row it lands on.
fn aim(view: &mut GraphPanel, step: isize, cx: &mut Context<GraphPanel>) {
    let Some(jump) = view.jump.open.as_mut() else {
        return;
    };
    let rows = jump.matching.rows().len();
    if rows == 0 {
        return;
    }
    jump.aimed = stepped(jump.aimed, step, rows);
    let aimed = jump.aimed;
    bring_into_view(&view.jump.scroll, aimed);
    preview(view, cx);
}

/// The row `step` rows on from `aimed` among `rows` rows, held to the list.
pub(crate) fn stepped(aimed: usize, step: isize, rows: usize) -> usize {
    aimed
        .saturating_add_signed(step)
        .min(rows.saturating_sub(1))
}

/// Scrolls the list just far enough to show the row `aimed`.
pub(crate) fn bring_into_view(scroll: &ListScroll, aimed: usize) {
    scroll
        .uniform_handle()
        .scroll_to_item(aimed, ScrollStrategy::Nearest);
}

/// The node of the aimed row.
fn aimed(jump: &Jump) -> Option<Id> {
    let row = jump.matching.rows().get(jump.aimed)?;
    Some(jump.candidates.get().get(row.candidate).id)
}

/// Moves the view onto the aimed row's node, or onto the innermost box
/// drawn around it where the drawing folds it away, and marks it as a hover
/// would. Nothing is selected or opened, so closing the palette unpicked
/// leaves the drawing as the palette found it.
fn preview(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let Some(id) = view.jump.open.as_ref().and_then(aimed) else {
        return;
    };
    let shown = view
        .model
        .read(cx)
        .loaded()
        .and_then(|loaded| drawn_holder(view, &loaded.shared, id));
    view.hovered = shown;
    let covered = view
        .jump
        .open
        .as_ref()
        .and_then(|jump| jump.painted)
        .map_or(px(0.), |painted| {
            (painted.bottom() - view.framing.viewport().top()).max(px(0.))
        });
    if let Some(shown) = shown {
        view.framing
            .request_frame_below(Frame::Node(shown), covered);
    }
    view.invalidate_canvas(cx);
}

/// `id`, or the innermost node holding it, whichever the drawing draws
/// first.
fn drawn_holder(view: &GraphPanel, shared: &Shared, id: Id) -> Option<Id> {
    let drawn = view.drawn()?;
    let draws = |id: Id| match &drawn {
        Drawn::Map { map, .. } => map.placed(id).is_some(),
        Drawn::Layered { view, .. } => view.boxes.iter().any(|b| b.id == id),
    };
    std::iter::successors(Some(id), |id| shared.base.nodes.get(id.0 as usize)?.parent)
        .find(|&id| draws(id))
}

/// Jumps to the aimed row's node.
fn go(view: &mut GraphPanel, window: &mut Window, cx: &mut Context<GraphPanel>) {
    if let Some(id) = view.jump.open.as_ref().and_then(aimed) {
        jump_to(view, id, window, cx);
    }
}

/// Jumps to `id` through [`reveal::reveal`], which offers to reveal a node the
/// drawing leaves out, and closes the palette there.
fn jump_to(view: &mut GraphPanel, id: Id, window: &mut Window, cx: &mut Context<GraphPanel>) {
    put_away(view, window, cx);
    reveal::reveal(view, id, cx);
}

/// Asks why the aimed row's node is in the build, closing the palette.
fn ask_why(view: &mut GraphPanel, window: &mut Window, cx: &mut Context<GraphPanel>) {
    let Some(id) = view.jump.open.as_ref().and_then(aimed) else {
        return;
    };
    put_away(view, window, cx);
    why::ask_why(view, id, cx);
}

/// Pins the aimed row's node to be shown whatever the filter says, or drops
/// that pin where the node carries it, and jumps to it, closing the palette.
fn always_show(view: &mut GraphPanel, window: &mut Window, cx: &mut Context<GraphPanel>) {
    let Some(id) = view.jump.open.as_ref().and_then(aimed) else {
        return;
    };
    put_away(view, window, cx);
    pins::set(view, id, Visibility::Shown, cx);
    reveal::reveal(view, id, cx);
}
/// Adds the aimed row's node to the query bar as a chip, closing the palette.
fn add_chip(view: &mut GraphPanel, window: &mut Window, cx: &mut Context<GraphPanel>) {
    let Some(id) = view.jump.open.as_ref().and_then(aimed) else {
        return;
    };
    let loaded = view.model.read(cx).loaded();
    let Some(literal) = loaded.and_then(|loaded| literal(&loaded.shared, id)) else {
        return;
    };
    put_away(view, window, cx);
    chips::push(view, &literal, cx);
}

/// A quoted target pattern naming `id`: the label of the target it belongs
/// to, or every target of the package a package box stands for.
fn literal(shared: &Shared, id: Id) -> Option<String> {
    let pattern = match carried_label(&shared.names, &shared.facets, id) {
        Some(label) => shared.labels.written(label).into_owned(),
        None => format!("{}:*", package_label(&shared.names, id)?),
    };
    let quote = if pattern.contains('"') { '\'' } else { '"' };
    Some(format!("{quote}{pattern}{quote}"))
}

/// Closes the palette unpicked, returning the view to where it stood as the
/// palette opened.
fn close(view: &mut GraphPanel, window: &mut Window, cx: &mut Context<GraphPanel>) {
    let opened_at = view.jump.open.as_ref().and_then(|jump| jump.opened_at);
    put_away(view, window, cx);
    if let Some((_, held)) = opened_at.filter(|&(mode, _)| mode == view.mode) {
        view.framing.return_to(held);
    }
    view.invalidate_canvas(cx);
}

/// Closes the palette, leaving the view where it stands, and gives the focus
/// back to the panel.
fn put_away(view: &mut GraphPanel, window: &mut Window, cx: &mut Context<GraphPanel>) {
    dismiss(view, cx);
    view.focus_handle.focus(window, cx);
}

/// Closes the palette, leaving the view where it stands.
pub(crate) fn dismiss(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    if close_palette(view) {
        view.hovered = None;
        view.invalidate_canvas(cx);
    }
}

/// Closes the palette, which offered the nodes of a graph that has been
/// replaced, with the preview its rows put up.
pub(crate) fn reset(view: &mut GraphPanel) {
    if close_palette(view) {
        view.hovered = None;
    }
}

/// Closes the palette, parking its matcher's pool; reports whether it was
/// open.
fn close_palette(view: &mut GraphPanel) -> bool {
    let Some(jump) = view.jump.open.take() else {
        return false;
    };
    view.matchers.park(jump.matching);
    true
}

/// The palette over the drawing, for the caller to place there; `None` while
/// it is closed.
pub(crate) fn overlay(
    view: &GraphPanel,
    palette: &Palette,
    window: &mut Window,
    cx: &mut Context<GraphPanel>,
) -> Option<Div> {
    let jump = view.jump.open.as_ref()?;
    let colors = &palette.colors;
    let text = TextStyleRefinement {
        font_size: Some(TEXT_SIZE.into()),
        ..TextStyleRefinement::default()
    };
    let row = window.with_text_style(Some(text), |window| menu_chrome::row_height(window));
    let scope_text = TextStyleRefinement {
        font_size: Some(SCOPE_TEXT_SIZE.into()),
        ..TextStyleRefinement::default()
    };
    let scope_width = window.with_text_style(Some(scope_text), |window| {
        let labels = Scope::CYCLE.iter().map(|scope| scope.label().into());
        menu_chrome::widest(window, labels)
    });
    let entity = cx.entity();
    Some(
        div()
            .absolute()
            .top_2()
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                menu_chrome::surface(div(), colors)
                    .key_context(KEY_CONTEXT)
                    .on_action(cx.listener(|view, _: &Previous, _, cx| aim(view, -1, cx)))
                    .on_action(cx.listener(|view, _: &Next, _, cx| aim(view, 1, cx)))
                    .on_action(cx.listener(|view, _: &Go, window, cx| go(view, window, cx)))
                    .on_action(
                        cx.listener(|view, _: &AskWhy, window, cx| ask_why(view, window, cx)),
                    )
                    .on_action(
                        cx.listener(|view, _: &AlwaysShow, window, cx| {
                            always_show(view, window, cx)
                        }),
                    )
                    .on_action(
                        cx.listener(|view, _: &AddChip, window, cx| add_chip(view, window, cx)),
                    )
                    .on_action(cx.listener(|view, _: &Close, window, cx| close(view, window, cx)))
                    .on_action(
                        cx.listener(|view, _: &NextScope, _, cx| cycle_scope(view, false, cx)),
                    )
                    .on_action(
                        cx.listener(|view, _: &PreviousScope, _, cx| cycle_scope(view, true, cx)),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, e: &MouseDownEvent, _, cx| {
                            view.jump.scroll.press(e.position);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .occlude()
                    .w(WIDTH)
                    .max_w(relative(0.9))
                    .flex()
                    .flex_col()
                    .text_size(TEXT_SIZE)
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap_2()
                            .px(menu_chrome::ROW_INSET)
                            .pb_1()
                            .child(div().flex_1().child(jump.field.clone()))
                            .child(
                                div()
                                    .flex_none()
                                    .w(scope_width)
                                    .flex()
                                    .justify_end()
                                    .text_size(SCOPE_TEXT_SIZE)
                                    .text_color(colors.text_muted)
                                    .child(jump.scope.label()),
                            ),
                    )
                    .children(
                        (!jump.matching.rows().is_empty())
                            .then(|| rows(jump, &view.jump.scroll, row, palette, cx)),
                    )
                    .child(footer(jump, colors.text_muted))
                    .child(overlay_canvas(move |bounds, _, cx| {
                        entity.update(cx, |view, _| {
                            if let Some(jump) = view.jump.open.as_mut() {
                                jump.painted = Some(bounds);
                            }
                        });
                    })),
            ),
    )
}

/// The list of the rows, `row` tall each and [`VISIBLE_ROWS`] of them in
/// view, drawing only those in view and scrolling under a bar.
fn rows(
    jump: &Jump,
    scroll: &ListScroll,
    row: Pixels,
    palette: &Palette,
    cx: &mut Context<GraphPanel>,
) -> impl IntoElement + use<> {
    let entity = cx.entity();
    let listing = *palette;
    let count = jump.matching.rows().len();
    let listed = uniform_list("jump-rows", count, move |range, _, cx| {
        let view = entity.read(cx);
        let (Some(jump), Some(loaded)) = (view.jump.open.as_ref(), view.model.read(cx).loaded())
        else {
            return Vec::new();
        };
        let candidates = jump.candidates.get();
        range
            .filter_map(|index| {
                let row = jump.matching.rows().get(index)?;
                let candidate = candidates.get(row.candidate);
                Some(listed(
                    view,
                    &loaded.shared,
                    candidate,
                    row,
                    index,
                    &listing,
                    &entity,
                ))
            })
            .collect()
    })
    .h(row * count.min(VISIBLE_ROWS) as f32)
    .w_full()
    .pr(px(SCROLLBAR_THICKNESS))
    .track_scroll(scroll.uniform_handle());
    div()
        .relative()
        .flex()
        .flex_col()
        .child(listed)
        .child(list_scrollbar(
            cx.entity(),
            |view: &GraphPanel| &view.jump.scroll,
            |view: &mut GraphPanel| &mut view.jump.scroll,
            palette.scroll,
        ))
}

/// The line under the rows: how many nodes the query matches, and the keys.
fn footer(jump: &Jump, muted: Hsla) -> Div {
    let matching = &jump.matching;
    let listed = matching.rows().len();
    let tally = match (matching.query().is_empty(), matching.matched()) {
        (true, _) => "type to find a node".to_owned(),
        (false, 0) if matching.streaming() => "matching…".to_owned(),
        (false, 0) => "no matches".to_owned(),
        (false, 1) => "1 match".to_owned(),
        (false, matched) if (listed as u32) < matched => {
            format!("{matched} matches, best {listed} listed")
        }
        (false, matched) => format!("{matched} matches"),
    };
    let last = KEY_HINTS.len() - 1;
    let hints = KEY_HINTS
        .iter()
        .enumerate()
        .map(move |(at, hint)| match at == last {
            true => SharedString::from(*hint),
            false => format!("{hint} ·").into(),
        });
    div()
        .flex()
        .justify_between()
        .gap_2()
        .px(menu_chrome::ROW_INSET)
        .pt_1()
        .text_xs()
        .text_color(muted)
        .child(div().flex_none().child(tally))
        .child(
            // The hints wrap between one another, so none runs past the
            // palette's edge.
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap_x_1()
                .children(hints),
        )
}

/// The keys the palette answers to, as its footer lists them.
const KEY_HINTS: &[&str] = &[
    "↑↓ preview",
    "enter jump",
    "shift-enter always show",
    "ctrl-enter why",
    "alt-enter add chip",
    "tab scope",
    "esc back",
];

/// One row: the node's text with the characters the query matched picked
/// out, what holds it where the text leaves that out, and its badges.
fn listed(
    view: &GraphPanel,
    shared: &Shared,
    candidate: &Candidate,
    row: &Row,
    index: usize,
    palette: &Palette,
    entity: &Entity<GraphPanel>,
) -> Stateful<Div> {
    let colors = &palette.colors;
    let aimed = view
        .jump
        .open
        .as_ref()
        .is_some_and(|jump| jump.aimed == index);
    let hidden = hidden_by(view, candidate.id);
    let tip = TooltipColors::from_colors(colors);
    let id = candidate.id;
    let entity = entity.clone();
    menu_chrome::aimed_row(("jump-row", index).into(), colors, aimed)
        .w_full()
        .on_click(move |_: &ClickEvent, window, cx| {
            entity.update(cx, |view, cx| jump_to(view, id, window, cx));
        })
        .child(
            div()
                .flex_grow_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis_start()
                .child(fuzzy::highlighted(
                    SharedString::from(candidate.text.to_string()),
                    &candidate.path,
                    &row.matched,
                    colors.text_muted,
                    palette.highlight.into(),
                )),
        )
        .children(index::container(shared, candidate).map(|container| {
            div()
                .flex_shrink(CONTAINER_SHRINK)
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis_start()
                .text_xs()
                .text_color(colors.text_muted)
                .child(container)
        }))
        .child(badge(kind_name(candidate.kind), colors.text_muted))
        .children(
            index::configuration(shared, candidate)
                .map(|configuration| badge(configuration, colors.text_muted)),
        )
        .children(hidden.map(|by| {
            badge("hidden", colors.warning)
                .id(("jump-hidden", index))
                .tooltip(TextTooltip::builder(by.describe().into(), tip))
        }))
}

/// How a row names the kind of its node.
fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Package => "package",
        _ => kind_names(kind).one,
    }
}

fn badge(text: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .flex_none()
        .px_1()
        .rounded_sm()
        .border_1()
        .border_color(color)
        .text_xs()
        .text_color(color)
        .child(text.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A node is added as the target it belongs to, and a package box as
    /// every target of its package.
    #[test]
    fn a_node_is_added_as_a_quoted_pattern_naming_its_target() {
        let shared = index::tests::shared();
        let literal = |name: &str| literal(&shared, shared.names.get(name).unwrap());
        assert_eq!(
            literal("target://app:lib").as_deref(),
            Some("\"//app:lib\"")
        );
        assert_eq!(
            literal("action:bazel-out/app/liblib.a").as_deref(),
            Some("\"//app:lib\"")
        );
        assert_eq!(literal("pkg:app").as_deref(), Some("\"//app:*\""));
    }

    /// The aim reaches every listed row, past those in view, and stops at
    /// either end of the list.
    #[test]
    fn the_aim_walks_every_row_and_stops_at_the_ends() {
        let rows = VISIBLE_ROWS * 3;
        let mut aimed = 0;
        for _ in 0..rows * 2 {
            aimed = stepped(aimed, 1, rows);
        }
        assert_eq!(aimed, rows - 1);
        assert_eq!(stepped(aimed, -2, rows), rows - 3);
        assert_eq!(stepped(1, -5, rows), 0);
        assert_eq!(stepped(0, 1, 0), 0);
    }
}
