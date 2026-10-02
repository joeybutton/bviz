//! The list a click puts up where it lands on several of the scatter plot's
//! marks at once, of every point under it.

use super::plot::Spot;
use bviz_ui_components::menu_chrome::sheet_height;
use bviz_ui_components::{beside_anchor, room_beside};
use gpui::{Bounds, Pixels, Point, SharedString, Size, point, px, size};
use std::cmp::Reverse;
use std::rc::Rc;

/// The points under a click, and which of them the reader is aiming at.
pub(super) struct PickList {
    /// Where the click landed, which the list opens beside wherever the plot
    /// is drawn.
    pub(super) spot: Spot,
    /// In the order [`order`] puts them, shared with the frame drawing them.
    pub(super) rows: Rc<[PickRow]>,
    /// The row the cursor or the keys are on.
    pub(super) aim: Option<usize>,
    /// Measured by the first frame drawing the list, since measuring text
    /// needs the style the list inherits.
    pub(super) extents: Option<Extents>,
}

/// How large the list and the tooltips of its rows stand.
#[derive(Clone, Copy, Debug)]
pub(super) struct Extents {
    /// How wide the list stands to read each row whole.
    pub(super) list_width: Pixels,
    /// The widest and the tallest of the rows' tooltips, which the list leaves
    /// room for beside it.
    pub(super) tooltip: Size<Pixels>,
}

/// One point under the click, and what its row reads.
pub(super) struct PickRow {
    /// Its index among the plotted marks.
    pub(super) mark: usize,
    /// The mnemonic of the action the point is.
    pub(super) mnemonic: Option<SharedString>,
    /// The part of the point's label that names its package, read muted and
    /// cut short first.
    pub(super) package: SharedString,
    /// The rest of the point's label.
    pub(super) name: SharedString,
    /// What tells the point apart from the others whose rows read alike, as
    /// [`hints`] chooses it.
    pub(super) hint: Option<SharedString>,
    /// The point's measures along both axes as the row reads them.
    pub(super) measures: SharedString,
    /// Where the point falls along the horizontal axis's scale.
    pub(super) x: u32,
    /// Where the point falls along the vertical axis's scale.
    pub(super) y: u32,
    /// What the tooltip for the point reads while the row is aimed at.
    pub(super) tooltip: Vec<String>,
}

/// `label` parted into its package and the target's name, `:name`, at the last
/// colon, so that each can be cut short apart; all of it names the target
/// where it has no colon.
pub(super) fn split_label(label: &str) -> (&str, &str) {
    match label.rfind(':') {
        Some(colon) => label.split_at(colon),
        None => ("", label),
    }
}

/// `path` within the output tree of the configuration it is built in,
/// `bazel-out/<configuration>/bin/`, where it lies in one; all of it otherwise.
pub(super) fn in_output_tree(path: &str) -> &str {
    let Some((_, tree)) = path
        .strip_prefix("bazel-out/")
        .and_then(|path| path.split_once('/'))
    else {
        return path;
    };
    tree.strip_prefix("bin/").unwrap_or(tree)
}

/// Puts `rows` in the order the list reads: the highest on the plot first,
/// then the furthest right, then by what the rows read, and the order the marks
/// are drawn in among rows alike in all of those.
pub(super) fn order(rows: &mut [PickRow]) {
    rows.sort_by(|a, b| {
        let key = |row: &PickRow| (Reverse(row.y), Reverse(row.x));
        key(a)
            .cmp(&key(b))
            .then_with(|| a.mnemonic.cmp(&b.mnemonic))
            .then_with(|| a.package.cmp(&b.package))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.hint.cmp(&b.hint))
            .then_with(|| a.mark.cmp(&b.mark))
    });
}

/// The row `by` steps from `aim` in a list of `rows` rows, held at either end;
/// from no row at all, the first row stepping down and the last stepping up.
/// `None` for a list with no rows.
pub(super) fn stepped(aim: Option<usize>, by: isize, rows: usize) -> Option<usize> {
    let last = rows.checked_sub(1)?;
    Some(match aim {
        Some(row) => row.saturating_add_signed(by).min(last),
        None if by >= 0 => 0,
        None => last,
    })
}

/// The tallest the list stands before its rows scroll, short enough that it
/// leaves most of the plot it lists points of in view.
const MAX_LIST_HEIGHT: Pixels = px(360.);

/// The narrowest the list is cut to for its rows' tooltips to stand beside
/// it, wide enough that a row still reads a part of each of its parts.
const MIN_LIST_WIDTH: Pixels = px(360.);

/// Between the list and the tooltip set beside it.
const FLANK_GAP: Pixels = px(4.);

/// Where the list of `rows` rows, each and its header `row` tall, opens for a
/// click at `at` in a plot filling `bounds`: beside the click as a tooltip is,
/// no larger than the plot holds, past which its rows scroll and end on a whole
/// row. It leaves room for the tooltips of its rows on its side away from the
/// click where the plot has it: beside it, cut narrower than its rows read but
/// no narrower than [`MIN_LIST_WIDTH`], clear of the click where the plot holds
/// that much and over it otherwise; failing that, above or below it.
pub(super) fn frame(
    at: Point<Pixels>,
    rows: usize,
    row: Pixels,
    extents: Extents,
    bounds: Bounds<Pixels>,
) -> Bounds<Pixels> {
    let height = sheet_height(rows, row, row, MAX_LIST_HEIGHT.min(bounds.size.height));
    let tooltip = extents.tooltip;
    let flanked = |room: Pixels| room - FLANK_GAP - tooltip.width;
    let narrowest = extents.list_width.min(MIN_LIST_WIDTH);
    let across = [room_beside(at, bounds).width, bounds.size.width]
        .into_iter()
        .map(flanked)
        .find(|&width| width >= narrowest);
    let (list, room) = match across {
        Some(width) => {
            let list = size(extents.list_width.min(width), height);
            (list, size(list.width + FLANK_GAP + tooltip.width, height))
        }
        None => {
            let list = size(extents.list_width.min(bounds.size.width), height);
            let stacked = height + FLANK_GAP + tooltip.height;
            match stacked <= bounds.size.height {
                true => (list, size(list.width, stacked)),
                false => (list, list),
            }
        }
    };
    let room = Bounds::new(beside_anchor(at, room, bounds), room);
    let nearer =
        |at: Pixels, start: Pixels, end: Pixels, extent: Pixels| match at - start <= end - at {
            true => start,
            false => end - extent,
        };
    let origin = point(
        nearer(at.x, room.left(), room.right(), list.width),
        nearer(at.y, room.top(), room.bottom(), list.height),
    );
    Bounds::new(origin, list)
}

/// The top of the aimed row `aimed` of rows `row` tall, drawn in `rows`
/// scrolled by `offset`, held within `rows` where it has scrolled out of them.
pub(super) fn row_top(rows: Bounds<Pixels>, offset: Pixels, row: Pixels, aimed: usize) -> Pixels {
    let lowest = (rows.bottom() - row).max(rows.top());
    (rows.top() + offset + row * aimed as f32).clamp(rows.top(), lowest)
}

/// Where a tooltip of `extent` for the row aimed at goes beside the list
/// standing at `list`, opened for a click at `at`, in a plot filling `bounds`:
/// level with `level` on the side of the list away from the click, else on
/// its other side, else above or below it likewise, lined up with its left
/// edge; where the plot holds it on none of those, over the list on its side
/// with more room. Held inside the plot throughout.
pub(super) fn flank(
    at: Point<Pixels>,
    list: Bounds<Pixels>,
    extent: Size<Pixels>,
    level: Pixels,
    bounds: Bounds<Pixels>,
) -> Point<Pixels> {
    let rightmost = (bounds.right() - extent.width).max(bounds.left());
    let lowest = (bounds.bottom() - extent.height).max(bounds.top());
    let right = list.right() + FLANK_GAP;
    let left = list.left() - FLANK_GAP - extent.width;
    let beside = match at.x <= list.left() {
        true => [right, left],
        false => [left, right],
    };
    let level = level.clamp(bounds.top(), lowest);
    if let Some(x) = beside
        .into_iter()
        .find(|&x| bounds.left() <= x && x + extent.width <= bounds.right())
    {
        return point(x, level);
    }
    let below = list.bottom() + FLANK_GAP;
    let above = list.top() - FLANK_GAP - extent.height;
    let stacked = match at.y <= list.top() {
        true => [below, above],
        false => [above, below],
    };
    if let Some(y) = stacked
        .into_iter()
        .find(|&y| bounds.top() <= y && y + extent.height <= bounds.bottom())
    {
        return point(list.left().clamp(bounds.left(), rightmost), y);
    }
    let x = match bounds.right() - list.right() >= list.left() - bounds.left() {
        true => rightmost,
        false => bounds.left(),
    };
    point(x, level)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_ui_components::menu_chrome::{SHEET_BORDER, SHEET_PADDING};

    fn row(mark: usize, label: &str, x: u32, y: u32) -> PickRow {
        let (label, hint) = match label.split_once(' ') {
            Some((label, hint)) => (label, Some(hint.to_owned().into())),
            None => (label, None),
        };
        PickRow {
            mark,
            mnemonic: None,
            package: SharedString::default(),
            name: label.to_owned().into(),
            hint,
            measures: SharedString::default(),
            x,
            y,
            tooltip: Vec::new(),
        }
    }

    #[test]
    fn a_label_parts_at_the_colon_naming_the_target() {
        assert_eq!(
            split_label("//third_party:protobuf_package_metadata"),
            ("//third_party", ":protobuf_package_metadata")
        );
        assert_eq!(split_label("@repo//a/b:c:d"), ("@repo//a/b:c", ":d"));
        assert_eq!(split_label("//pkg"), ("", "//pkg"));
    }

    #[test]
    fn a_path_reads_within_its_configurations_output_tree() {
        let built = "bazel-out/k8-fastbuild-ST-1/bin/third_party/m.json";
        assert_eq!(in_output_tree(built), "third_party/m.json");
        assert_eq!(
            in_output_tree("bazel-out/k8/testlogs/a/b.xml"),
            "testlogs/a/b.xml"
        );
        assert_eq!(in_output_tree("external/x/y.h"), "external/x/y.h");
    }

    #[test]
    fn the_list_reads_highest_then_furthest_right_then_name() {
        let mut rows = vec![
            row(0, "b", 5, 10),
            row(1, "a y.o", 5, 10),
            row(2, "z", 9, 10),
            row(3, "y", 1, 40),
            row(4, "a", 5, 10),
            row(5, "a x.o", 5, 10),
            row(6, "a", 5, 10),
        ];
        order(&mut rows);
        let marks: Vec<usize> = rows.iter().map(|row| row.mark).collect();
        assert_eq!(marks, [3, 2, 4, 6, 5, 1, 0]);
    }

    #[test]
    fn the_keys_step_through_the_rows_and_hold_at_either_end() {
        assert_eq!(stepped(None, 1, 3), Some(0));
        assert_eq!(stepped(None, -1, 3), Some(2));
        assert_eq!(stepped(Some(0), 1, 3), Some(1));
        assert_eq!(stepped(Some(2), 1, 3), Some(2));
        assert_eq!(stepped(Some(0), -1, 3), Some(0));
        assert_eq!(stepped(None, 1, 0), None);
    }

    const ROW: Pixels = px(24.);
    const WIDTH: Pixels = px(520.);
    const TOOLTIP: Size<Pixels> = size(px(340.), px(110.));

    fn extents(list_width: Pixels) -> Extents {
        Extents {
            list_width,
            tooltip: TOOLTIP,
        }
    }

    fn plot() -> Bounds<Pixels> {
        Bounds::new(point(px(100.), px(50.)), size(px(900.), px(600.)))
    }

    #[test]
    fn the_list_opens_beside_the_click_and_flips_back_near_an_edge() {
        let open = frame(point(px(200.), px(100.)), 3, ROW, extents(WIDTH), plot());
        assert!(open.left() > px(200.) && open.top() > px(100.));
        let corner = point(plot().right() - px(10.), plot().bottom() - px(10.));
        let flipped = frame(corner, 3, ROW, extents(WIDTH), plot());
        assert!(flipped.right() < corner.x && flipped.bottom() < corner.y);
    }

    #[test]
    fn a_long_list_is_held_inside_the_plot_and_ends_on_a_whole_row() {
        let short = Bounds::new(point(px(0.), px(0.)), size(px(300.), px(200.)));
        for bounds in [plot(), short] {
            let at = bounds.origin + point(px(40.), px(40.));
            let open = frame(at, 500, ROW, extents(WIDTH), bounds);
            assert!(bounds.top() <= open.top() && open.bottom() <= bounds.bottom());
            assert!(bounds.left() <= open.left() && open.right() <= bounds.right());
            let inset = (SHEET_PADDING + SHEET_BORDER) * 2. + ROW;
            let rows = (open.size.height - inset) / ROW;
            assert!(rows >= 1.0 && rows == rows.floor(), "{rows} rows");
        }
    }

    #[test]
    fn a_plot_too_small_for_a_row_still_holds_the_list() {
        let tiny = Bounds::new(point(px(10.), px(10.)), size(px(80.), px(30.)));
        let at = tiny.origin + point(px(5.), px(5.));
        let open = frame(at, 40, ROW, extents(WIDTH), tiny);
        assert!(tiny.top() <= open.top() && open.bottom() <= tiny.bottom());
        assert!(tiny.left() <= open.left() && open.right() <= tiny.right());
    }

    /// Where the aimed row's tooltip goes for a list opened at `at` in
    /// `bounds`, aiming at the list's first row, and the list.
    fn placed(at: Point<Pixels>, width: Pixels, bounds: Bounds<Pixels>) -> [Bounds<Pixels>; 2] {
        let list = frame(at, 40, ROW, extents(width), bounds);
        let origin = flank(at, list, TOOLTIP, list.top(), bounds);
        let tooltip = Bounds::new(origin, TOOLTIP);
        assert!(bounds.left() <= tooltip.left() && tooltip.right() <= bounds.right());
        assert!(bounds.top() <= tooltip.top() && tooltip.bottom() <= bounds.bottom());
        [list, tooltip]
    }

    /// A list whose rows read nearly as wide as the plot is cut narrower, so
    /// the tooltip stands beside it, away from the click, level with the row.
    #[test]
    fn in_a_wide_plot_the_tooltip_stands_beside_the_list() {
        let wide = plot().size.width - px(20.);
        for at in [point(px(160.), px(90.)), point(px(960.), px(300.))] {
            let [list, tooltip] = placed(at, wide, plot());
            assert!(!list.intersects(&tooltip), "{list:?} {tooltip:?}");
            assert_eq!(tooltip.top(), list.top());
            let away = match at.x < list.left() {
                true => tooltip.left() == list.right() + FLANK_GAP,
                false => tooltip.right() + FLANK_GAP == list.left(),
            };
            assert!(away, "{at:?} {list:?} {tooltip:?}");
        }
        let [list, _] = placed(point(px(160.), px(90.)), px(300.), plot());
        assert_eq!(list.size.width, px(300.));
    }

    /// A plot too narrow for the list and the tooltip side by side has the
    /// tooltip stand above or below the list, clear of it.
    #[test]
    fn in_a_medium_plot_the_tooltip_stands_above_or_below_the_list() {
        let medium = Bounds::new(point(px(0.), px(0.)), size(px(560.), px(640.)));
        for at in [point(px(40.), px(40.)), point(px(500.), px(600.))] {
            let [list, tooltip] = placed(at, WIDTH, medium);
            assert!(!list.intersects(&tooltip), "{list:?} {tooltip:?}");
            assert!(tooltip.bottom() <= list.top() || list.bottom() <= tooltip.top());
            assert_eq!(tooltip.left(), list.left());
        }
    }

    /// A plot holding the tooltip neither beside nor above or below the list
    /// has it stand over the list.
    #[test]
    fn in_a_narrow_plot_the_tooltip_stands_over_the_list() {
        let narrow = Bounds::new(point(px(0.), px(0.)), size(px(420.), px(400.)));
        let [list, tooltip] = placed(point(px(40.), px(40.)), WIDTH, narrow);
        assert!(list.intersects(&tooltip));
    }

    #[test]
    fn the_tooltip_follows_its_row_and_stays_within_the_rows_in_view() {
        let rows = Bounds::new(point(px(0.), px(100.)), size(px(300.), ROW * 10.));
        assert_eq!(row_top(rows, px(0.), ROW, 3), px(100.) + ROW * 3.);
        assert_eq!(row_top(rows, -ROW * 2., ROW, 3), px(100.) + ROW);
        assert_eq!(row_top(rows, -ROW * 8., ROW, 3), rows.top());
        assert_eq!(row_top(rows, px(0.), ROW, 30), rows.bottom() - ROW);
    }
}
