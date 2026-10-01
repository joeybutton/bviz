//! Painting the icicle: each cell a box filled for what it holds and named
//! where it is wide enough to read.

use super::icicle::{self, Cell, Holds};
use crate::Palette;
use crate::hue::Colouring;
use crate::model::Shared;
use crate::paint::UNMARKED_FADE;
use crate::point::{mnemonic, point_label, unit_names};
use bviz_graph_layout::Id;
use bviz_graph_layout::dominators::Dominators;
use bviz_graph_layout::metrics::Unit;
use bviz_graph_layout::select::{Verdict, Verdicts};
use bviz_ui_assets::shape_line;
use bviz_ui_components::{LabelBoxStyle, paint_labeled_box};
use bviz_ui_theme::{label_on, mix};
use gpui::{App, BorderStyle, Bounds, Hsla, Pixels, Rgba, Window, px, quad, size};
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
    pub(crate) focus: Option<u32>,
    pub(crate) shared: Shared,
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
    let cells = icicle::lay_out(&*scene.tree, scene.focus, bounds, row, MIN_CELL_WIDTH);
    for cell in cells {
        let fill = fill(scene, cell.holds);
        let (rect, label) = (drawn_rect(&cell), label_on(fill));
        let name = match cell.holds {
            _ if cell.rect.size.width < LABEL_MIN_WIDTH => String::new(),
            Holds::Rest { count, .. } => {
                let (room, font_size) = style.label_room(rect);
                let font = bviz_ui_assets::text_metrics(window).font;
                rest_name(count, room, |text| {
                    let line = shape_line(window, text.to_owned(), font.clone(), font_size, label);
                    f32::from(line.width())
                })
            }
            holds => cell_name(&scene.tree, &scene.shared, holds),
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
        Holds::Build => palette.package,
        Holds::Node(node) => {
            let (tree, facets) = (&scene.tree, &scene.shared.facets);
            let hue = scene.colouring.hue(tree.id(node), tree.kind(node), facets);
            hue.color(palette)
        }
        Holds::Rest { .. } => mix(palette.data.neutral, palette.background, REST_RECEDE),
    }
}

/// What the cell holding `holds` of `tree` reads.
pub(crate) fn cell_name(tree: &Dominators, shared: &Shared, holds: Holds) -> String {
    let many = unit_names(tree.scope().unit()).many;
    match holds {
        Holds::Build => format!("whole build · {} {many}", tree.total()),
        Holds::Node(node) => point_name(tree.id(node), tree.scope().unit(), shared),
        Holds::Rest { count, .. } => format!("{count} more"),
    }
}

/// What a cell gathering `count` siblings reads within `room` as `width`
/// measures text: a count cut short says nothing, so its name is left whole,
/// or failing that the bare count, or failing that nothing.
fn rest_name(count: u32, room: f32, mut width: impl FnMut(&str) -> f32) -> String {
    [format!("{count} more"), count.to_string()]
        .into_iter()
        .find(|name| width(name) <= room)
        .unwrap_or_default()
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
    fn a_rest_cell_names_its_count_whole_or_not_at_all() {
        assert_eq!(rest_name(22, 7.0, chars), "22 more");
        assert_eq!(rest_name(22, 6.0, chars), "22");
        assert_eq!(rest_name(22, 2.0, chars), "22");
        assert_eq!(rest_name(22, 1.0, chars), "");
    }
}
