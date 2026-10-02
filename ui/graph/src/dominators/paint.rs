//! Painting the icicle: each cell a box filled for what it holds and named
//! where it is wide enough to read.

use super::icicle::{self, Cell, Holds, Parent, Run};
use super::{SEVERAL, WHOLE_BUILD, what_is_shared};
use crate::Palette;
use crate::hints::hints_where_alike;
use crate::hue::Colouring;
use crate::model::Shared;
use crate::paint::UNMARKED_FADE;
use crate::point::{configuration_readings, mnemonic, point_label, primary_output, unit_names};
use bviz_graph_layout::Id;
use bviz_graph_layout::dominators::Dominators;
use bviz_graph_layout::metrics::Unit;
use bviz_graph_layout::select::{Verdict, Verdicts};
use bviz_ui_assets::shape_line;
use bviz_ui_components::{LabelBoxStyle, paint_labeled_box};
use bviz_ui_theme::{label_on, mix};
use gpui::{App, BorderStyle, Bounds, Hsla, Pixels, Rgba, Window, px, quad, size};
use std::collections::HashMap;
use std::sync::Arc;

/// Inset of a cell's name from its edges, above and below as at either side.
const TEXT_PAD: f32 = 3.0;

/// Room between one row of cells and the next, so a cell reads apart from the
/// one beneath it where the two are filled alike.
const ROW_GAP: Pixels = px(1.);

/// The narrowest a cell is drawn on its own; narrower siblings share one.
pub(crate) const MIN_CELL_WIDTH: Pixels = px(4.);

/// Only cells at least this wide are named; narrower ones leave too little
/// room for legible text.
const LABEL_MIN_WIDTH: Pixels = px(28.);

/// How far the seam dividing a cell from its left neighbour is mixed from the
/// cell's fill towards the canvas: enough to read as a divider on any fill.
const SEAM_MIX: f32 = 0.35;

/// How far a cell gathering siblings too narrow to draw recedes towards the
/// canvas from the neutral it is filled with, so it reads as no point itself.
const REST_RECEDE: f32 = 0.5;

/// How tall a row of the icicle stands: a name at the inherited text size,
/// padded above and below, and the gap to the next row.
pub(crate) fn row_height(window: &Window) -> Pixels {
    bviz_ui_assets::text_metrics(window).font_size + px(2. * TEXT_PAD) + ROW_GAP
}

/// What the icicle paints from, owned so the `'static` paint closure holds no
/// borrow of the panel.
pub(crate) struct Scene {
    pub(crate) tree: Arc<Dominators>,
    pub(crate) colouring: Arc<Colouring>,
    /// What the cell spanning the top row holds.
    pub(crate) top: Holds,
    pub(crate) shared: Shared,
    /// The dominators the breadcrumb names above the cell on top, whose names
    /// the cells' are told apart from.
    pub(crate) above: Vec<u32>,
    /// The verdict of the filter the cells are faded by on each node, absent
    /// where none fades them.
    pub(crate) verdicts: Option<Arc<Verdicts>>,
    pub(crate) palette: Palette,
}

/// Paints the icicle of `scene` filling `bounds`.
pub(crate) fn paint_icicle(
    scene: &Scene,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let palette = &scene.palette;
    let style = LabelBoxStyle {
        background: palette.background,
        seam_mix: SEAM_MIX,
        label_min_px: f32::from(LABEL_MIN_WIDTH),
        text_pad: TEXT_PAD,
    };
    let row = row_height(window);
    let cells = icicle::lay_out(&*scene.tree, scene.top, bounds, row, MIN_CELL_WIDTH);
    let hints = hints_among(&scene.tree, &scene.shared, &scene.above, &cells);
    for (index, cell) in cells.into_iter().enumerate() {
        let fill = fill(scene, cell.holds);
        let (rect, label) = (drawn_rect(&cell), label_on(fill));
        let name = match (index, cell.holds) {
            _ if cell.rect.size.width < LABEL_MIN_WIDTH => String::new(),
            (0, Holds::Rest(run)) => run_name(&scene.tree, &scene.shared, run),
            (_, Holds::Rest(run)) => {
                let (room, font_size) = style.label_room(rect);
                let width = text_width(window, font_size, label);
                rest_name(run.count(), room, width)
            }
            (_, Holds::Shared) => {
                let (room, font_size) = style.label_room(rect);
                let whole = cell_name(&scene.tree, &scene.shared, Holds::Shared);
                let count = scene.tree.shared_tops().len();
                let width = text_width(window, font_size, label);
                shared_cell_name(whole, count, room, width)
            }
            (_, holds) => {
                let name = cell_name(&scene.tree, &scene.shared, holds);
                let hint = match holds {
                    Holds::Node(node) => hints.get(&node),
                    Holds::Build | Holds::Shared | Holds::Rest(_) => None,
                };
                match hint {
                    Some(hint) => {
                        let (room, font_size) = style.label_room(rect);
                        fit_apart(&name, hint, room, text_width(window, font_size, label))
                    }
                    None => name,
                }
            }
        };
        let strength = match cell.holds {
            Holds::Node(node) if faded(scene, scene.tree.id(node)) => UNMARKED_FADE,
            _ => 1.0,
        };
        paint_labeled_box(
            rect,
            Rgba {
                a: fill.a * strength,
                ..fill
            }
            .into(),
            &name,
            Hsla {
                a: label.a * strength,
                ..label
            },
            style,
            window,
            cx,
        );
    }
}

/// The part of `cell`'s row its box covers, clear of the gap to the next row.
pub(crate) fn drawn_rect(cell: &Cell) -> Bounds<Pixels> {
    let rect = cell.rect;
    Bounds::new(
        rect.origin,
        size(rect.size.width, rect.size.height - ROW_GAP),
    )
}

const RING_WIDTH: Pixels = px(2.);

/// Rings the box of `cell`.
pub(crate) fn paint_ring(cell: &Cell, palette: &Palette, window: &mut Window) {
    window.paint_quad(quad(
        drawn_rect(cell),
        px(0.),
        Hsla::transparent_black(),
        RING_WIDTH,
        palette.label,
        BorderStyle::Solid,
    ));
}

/// Whether the filter fading the cells of `scene` draws the node `id` as no
/// part of what it selects.
fn faded(scene: &Scene, id: Id) -> bool {
    scene
        .verdicts
        .as_ref()
        .is_some_and(|verdicts| matches!(verdicts.get(id), Verdict::Hidden | Verdict::Faded))
}

fn fill(scene: &Scene, holds: Holds) -> Rgba {
    let palette = &scene.palette;
    match holds {
        Holds::Build => build_fill(palette),
        Holds::Shared => shared_fill(palette),
        Holds::Node(node) => {
            let (tree, facets) = (&scene.tree, &scene.shared.facets);
            let hue = scene.colouring.hue(tree.id(node), tree.kind(node), facets);
            hue.color(palette)
        }
        Holds::Rest(_) => rest_fill(palette),
    }
}

fn build_fill(palette: &Palette) -> Rgba {
    palette.package
}

/// The fill of the cell of the shared tops, which is no point and stands for
/// more than a rest cell does: the neutral carried towards the ink the panel
/// sets text in, where a rest cell's is carried towards the canvas.
fn shared_fill(palette: &Palette) -> Rgba {
    mix(palette.data.neutral, palette.label, SHARED_STAND_OUT)
}

/// How far the fill of the cell of the shared tops is mixed from the neutral
/// towards the text ink: far enough to read apart from a point drawn in the
/// neutral, and short of where it nears a hue.
const SHARED_STAND_OUT: f32 = 0.35;

fn rest_fill(palette: &Palette) -> Rgba {
    mix(palette.data.neutral, palette.background, REST_RECEDE)
}

/// What the cell holding `holds` of `tree` reads.
pub(crate) fn cell_name(tree: &Dominators, shared: &Shared, holds: Holds) -> String {
    let many = unit_names(tree.scope().unit()).many;
    match holds {
        Holds::Build => format!("whole build · {} {many}", tree.total()),
        Holds::Shared => {
            let (name, count) = (shared_name(tree), tree.shared_tops().len());
            format!("{name} · {count}")
        }
        Holds::Node(node) => point_name(tree.id(node), tree.scope().unit(), shared),
        Holds::Rest(run) => format!("{} more", run.count()),
    }
}

/// How a reader names the shared tops of `tree`.
pub(crate) fn shared_name(tree: &Dominators) -> String {
    super::shared_by(SEVERAL, unit_names(tree.scope().unit()).many)
}

/// Measures text as `window` shapes it at `font_size` in `ink`, in the font
/// the window inherits.
fn text_width(window: &mut Window, font_size: Pixels, ink: Hsla) -> impl FnMut(&str) -> f32 {
    let font = bviz_ui_assets::text_metrics(window).font;
    move |text| {
        let line = shape_line(window, text.to_owned(), font.clone(), font_size, ink);
        f32::from(line.width())
    }
}

/// What tells apart, by node, the names that read alike among the crumbs
/// naming `crumbs` and the cells `cells` of `tree` wide enough to name. Each
/// such name is told apart by its configuration, as
/// [`configuration_readings`] reads it, and an action's also by the file it
/// writes first. A node whose name no other reads alike has none.
pub(crate) fn hints_among(
    tree: &Dominators,
    shared: &Shared,
    crumbs: &[u32],
    cells: &[Cell],
) -> HashMap<u32, String> {
    let unit = tree.scope().unit();
    let named = cells.iter().filter_map(|cell| match cell.holds {
        Holds::Node(node) if cell.rect.size.width >= LABEL_MIN_WIDTH => Some(node),
        _ => None,
    });
    let nodes: Vec<u32> = crumbs.iter().copied().chain(named).collect();
    let names: Vec<String> = nodes
        .iter()
        .map(|&node| point_name(tree.id(node), unit, shared))
        .collect();
    let hints = hints_where_alike(&names, |row| {
        let id = tree.id(nodes[row]);
        let [short, whole] = configuration_readings(id, shared);
        match unit {
            Unit::Action => {
                let output = primary_output(id, shared).map(str::to_owned);
                vec![short, whole, output]
            }
            Unit::ConfiguredTarget | Unit::Target => vec![short, whole],
        }
    });
    let told = nodes.into_iter().zip(hints);
    told.filter_map(|(node, hint)| Some((node, hint?)))
        .collect()
}

/// Between a name and what tells it apart from others reading alike.
pub(crate) const HINT_SEPARATOR: &str = " · ";

/// `name` followed by `hint` within `room` as `width` measures text: whole
/// where it fits, else with `name` cut short so the hint still reads beside
/// it, and where that leaves the name less than [`NAME_SHARE`] of the room,
/// `name` alone.
fn fit_apart(name: &str, hint: &str, room: f32, mut width: impl FnMut(&str) -> f32) -> String {
    let whole = format!("{name}{HINT_SEPARATOR}{hint}");
    if width(&whole) <= room {
        return whole;
    }
    let tail = format!("{ELLIPSIS}{HINT_SEPARATOR}{hint}");
    let name_room = room - width(&tail);
    if name_room < room * NAME_SHARE {
        return name.to_owned();
    }
    let chars: Vec<char> = name.chars().collect();
    // Bisects for the most characters of `name` fitting `name_room`.
    let (mut fits, mut over) = (0, chars.len());
    while over - fits > 1 {
        let mid = (fits + over) / 2;
        match width(&chars[..mid].iter().collect::<String>()) <= name_room {
            true => fits = mid,
            false => over = mid,
        }
    }
    let kept: String = chars[..fits].iter().collect();
    format!("{kept}{tail}")
}

/// The least of a cell's room a name keeps beside the hint telling it apart,
/// so a long hint never reads in place of the name.
const NAME_SHARE: f32 = 0.5;

/// Ends a name cut short.
const ELLIPSIS: char = '…';

/// What a run of `tree` drawn on top reads, naming what its siblings stand
/// under.
pub(crate) fn run_name(tree: &Dominators, shared: &Shared, run: Run) -> String {
    let count = run.count();
    match run.parent() {
        Parent::Node(parent) => {
            let parent = point_name(tree.id(parent), tree.scope().unit(), shared);
            format!("{count} more under {parent}")
        }
        Parent::Shared => {
            let shared = what_is_shared(unit_names(tree.scope().unit()).many);
            format!("{count} more of {shared}")
        }
        Parent::Build => format!("{count} more under {WHOLE_BUILD}"),
    }
}

/// What a cell gathering `count` siblings reads within `room` as `width`
/// measures text: a count cut short says nothing, so its name is left whole,
/// or failing that the bare count, or failing that nothing.
fn rest_name(count: u32, room: f32, width: impl FnMut(&str) -> f32) -> String {
    first_fitting([format!("{count} more"), count.to_string()], room, width)
}

/// What the cell gathering `count` shared tops, named `whole`, reads within
/// `room` as `width` measures text: its name whole, or failing that one word
/// and the count, the word alone, or nothing.
fn shared_cell_name(
    whole: String,
    count: usize,
    room: f32,
    width: impl FnMut(&str) -> f32,
) -> String {
    let names = [whole, format!("{SHARED} · {count}"), SHARED.to_owned()];
    first_fitting(names, room, width)
}

/// What a cell too narrow to say what shares its tops reads of them.
const SHARED: &str = "shared";

/// The first of `names` no wider than `room` as `width` measures text, or
/// nothing where none fits.
fn first_fitting<const N: usize>(
    names: [String; N],
    room: f32,
    mut width: impl FnMut(&str) -> f32,
) -> String {
    let mut names = names.into_iter();
    names.find(|name| width(name) <= room).unwrap_or_default()
}

/// How a cell names the point `id` of `unit`.
pub(crate) fn point_name(id: Id, unit: Unit, shared: &Shared) -> String {
    let label = point_label(id, shared);
    match unit {
        Unit::Action => format!("{} {label}", mnemonic(id, shared)),
        Unit::ConfiguredTarget | Unit::Target => label,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Text as wide as it has characters.
    fn chars(text: &str) -> f32 {
        text.chars().count() as f32
    }

    #[test]
    fn a_name_cut_short_keeps_the_hint_while_it_keeps_the_larger_part() {
        let label = "@llvm//toolchain:linux_x86_64_cc_toolchain";
        let fit = |room| fit_apart(label, "k8", room, chars);
        assert_eq!(fit(100.0), format!("{label} · k8"));
        assert_eq!(fit(40.0), "@llvm//toolchain:linux_x86_64_cc_t… · k8");
        assert_eq!(fit(20.0), "@llvm//toolcha… · k8");
        let long = "a-long-configuration-name (exec, 3f2a)";
        assert_eq!(fit_apart(label, long, 60.0, chars), label);
    }

    #[test]
    fn a_rest_cell_names_its_count_whole_or_not_at_all() {
        assert_eq!(rest_name(22, 7.0, chars), "22 more");
        assert_eq!(rest_name(22, 6.0, chars), "22");
        assert_eq!(rest_name(22, 2.0, chars), "22");
        assert_eq!(rest_name(22, 1.0, chars), "");
    }

    /// The cell of the shared tops reads apart from every hue a point is
    /// drawn in, from a rest cell and from the whole build.
    #[test]
    fn the_cell_of_the_shared_tops_is_filled_unlike_any_other_cell() {
        use bviz_ui_theme::{MIN_DISTINCT_COLOR, built_in_themes, distance};
        for theme in built_in_themes().iter() {
            let palette = Palette::from_theme(theme);
            let shared = shared_fill(&palette);
            let hues = palette.data.chromatic().into_iter().map(Rgba::from);
            let others = hues.chain([
                palette.data.neutral.into(),
                rest_fill(&palette),
                build_fill(&palette),
            ]);
            for other in others {
                let apart = distance(shared, other);
                assert!(
                    apart >= MIN_DISTINCT_COLOR,
                    "{}: {apart} from {other:?}",
                    theme.name
                );
            }
        }
    }

    #[test]
    fn the_cell_of_the_shared_tops_reads_as_much_of_its_name_as_fits_whole() {
        let whole = "shared by several top-level targets · 12";
        let read = |room| shared_cell_name(whole.to_owned(), 12, room, chars);
        assert_eq!(read(40.0), whole);
        assert_eq!(read(39.0), "shared · 12");
        assert_eq!(read(10.0), "shared");
        assert_eq!(read(5.0), "");
    }
}
