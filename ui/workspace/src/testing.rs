//! Values the crate's own tests share.

use crate::{Layout, PanelGroup};
use gpui::Axis;

/// A share is divided out of the flexes of the sides it is one of, so shares
/// that divide evenly in the rationals are even to within one division's
/// rounding — far under a pixel of any window.
pub(crate) const SHARE_ROUNDING: f32 = 1e-6;

/// The letter naming the pane or panel at `at` of the ones a test lays out, so
/// that an arrangement written out reads as the things it holds.
pub(crate) fn letter(at: usize) -> char {
    (b'a' + at as u8) as char
}

/// The arrangement `group` describes, each pane written as the panels it holds
/// in tab order.
pub(crate) fn arrange(group: PanelGroup<char>) -> Layout<String> {
    group.arrange(&mut |first, rest| std::iter::once(first).chain(rest).collect())
}

/// Each pane of `layout` in the order it is laid out, with the shares of the
/// window's width and height it is laid out at.
pub(crate) fn extents(layout: &Layout<String>) -> Vec<(String, f32, f32)> {
    let mut panes = Vec::new();
    extend_extents(layout, 1.0, 1.0, &mut panes);
    panes
}

fn extend_extents(
    layout: &Layout<String>,
    width: f32,
    height: f32,
    panes: &mut Vec<(String, f32, f32)>,
) {
    match layout {
        Layout::Pane(pane) => panes.push((pane.clone(), width, height)),
        Layout::Split(split) => {
            for side in split.sides() {
                let (width, height) = match split.axis() {
                    Axis::Horizontal => (width * side.share, height),
                    Axis::Vertical => (width, height * side.share),
                };
                extend_extents(side.layout, width, height, panes);
            }
        }
    }
}

/// A pane, and the shares of the window's width and height it is laid out at.
pub(crate) type Extent<'a> = (&'a str, f32, f32);

/// Asserts `layout` lays out the panes `expected` names, in that order, each at
/// the extent given for it.
pub(crate) fn assert_extents(layout: &Layout<String>, expected: &[Extent]) {
    let laid_out = extents(layout);
    let panes: Vec<&str> = laid_out.iter().map(|(pane, _, _)| pane.as_str()).collect();
    let wanted: Vec<&str> = expected.iter().map(|(pane, _, _)| *pane).collect();
    assert_eq!(panes, wanted);
    for ((pane, width, height), (_, want_width, want_height)) in laid_out.iter().zip(expected) {
        assert!(
            (width - want_width).abs() < SHARE_ROUNDING
                && (height - want_height).abs() < SHARE_ROUNDING,
            "{pane} is laid out at {width}x{height} of the window, not {want_width}x{want_height}"
        );
    }
}
