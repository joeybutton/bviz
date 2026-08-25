use crate::Pane;
use gpui::{Axis, Entity};
use std::sync::atomic::{AtomicU64, Ordering};

/// The share of a split's extent neither side may be squeezed below, so a
/// divider dragged to one end leaves the pane it shrinks visible and its own
/// divider still within reach.
const MIN_PANE_SHARE: f32 = 0.05;

/// The share a split gives its first side when nothing asks for another.
const EVEN_SHARE: f32 = 0.5;

/// Identity of one split, by which a divider names the split it belongs to
/// across the frames of a drag and in its element id.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SplitId(u64);

impl SplitId {
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// How a window's panes are arranged: a binary tree of panes and the splits
/// between them, in a normal form where every split has two non-empty sides.
/// [`Layout::remove`] upholds it by putting a split that loses a side out of the
/// way and leaving the side that remains in its place, at every level it
/// unwinds through.
///
/// The leaf type is a parameter so the arrangement's algebra can be exercised on
/// plain values, away from the window real panes need.
#[derive(Clone, Debug)]
pub enum Layout<P = Entity<Pane>> {
    Pane(P),
    Split(Split<P>),
}

/// Two arrangements laid out along an axis, one after the other, with a
/// draggable divider between them.
#[derive(Clone, Debug)]
pub struct Split<P = Entity<Pane>> {
    id: SplitId,
    axis: Axis,
    /// Share of the extent given to `first`, as a drag last left it and so
    /// unbounded; [`Split::ratio`] is the share the layout is built from.
    ratio: f32,
    first: Box<Layout<P>>,
    second: Box<Layout<P>>,
}

impl<P> Split<P> {
    fn new(axis: Axis, ratio: f32, first: Layout<P>, second: Layout<P>) -> Self {
        Self {
            id: SplitId::next(),
            axis,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    pub fn id(&self) -> SplitId {
        self.id
    }

    pub fn axis(&self) -> Axis {
        self.axis
    }

    /// The share of the split's extent the first side is laid out at, bounded so
    /// neither side collapses.
    pub fn ratio(&self) -> f32 {
        self.ratio.clamp(MIN_PANE_SHARE, 1.0 - MIN_PANE_SHARE)
    }

    pub fn first(&self) -> &Layout<P> {
        &self.first
    }

    pub fn second(&self) -> &Layout<P> {
        &self.second
    }
}

/// What removing a pane did to an arrangement.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Removal {
    /// No pane here is that one.
    NotFound,
    /// The pane is gone and panes are left.
    Removed,
    /// The arrangement was that one pane, so nothing of it is left and whoever
    /// holds it is done with it.
    Emptied,
}

impl<P> Layout<P> {
    /// `first` and `second` either side of a divider along `axis`, the first
    /// side laid out at `ratio` of the extent they share.
    pub fn split_at(axis: Axis, ratio: f32, first: Layout<P>, second: Layout<P>) -> Self {
        Layout::Split(Split::new(axis, ratio, first, second))
    }

    /// The pane laid out first, which a window opens showing.
    pub fn first_pane(&self) -> &P {
        match self {
            Layout::Pane(pane) => pane,
            Layout::Split(split) => split.first.first_pane(),
        }
    }

    /// The panes, in the order they are laid out.
    pub fn panes(&self) -> Panes<'_, P> {
        Panes {
            unvisited: vec![self],
        }
    }

    /// Hands the split `id` names the share a drag has moved its divider to,
    /// reporting whether the arrangement holds such a split.
    pub fn move_divider(&mut self, id: SplitId, ratio: f32) -> bool {
        let Layout::Split(split) = self else {
            return false;
        };
        if split.id == id {
            split.ratio = ratio;
            return true;
        }
        split.first.move_divider(id, ratio) || split.second.move_divider(id, ratio)
    }
}

impl<P: Clone + PartialEq> Layout<P> {
    pub fn contains(&self, target: &P) -> bool {
        self.panes().any(|pane| pane == target)
    }

    /// Puts `new` alongside `target` under a split along `axis`, reporting
    /// whether the arrangement holds `target`.
    pub fn split(&mut self, target: &P, axis: Axis, new: P) -> bool {
        match self {
            Layout::Pane(pane) => {
                if pane != target {
                    return false;
                }
                let pane = Layout::Pane(pane.clone());
                *self = Layout::split_at(axis, EVEN_SHARE, pane, Layout::Pane(new));
                true
            }
            Layout::Split(split) => match split.first.contains(target) {
                true => split.first.split(target, axis, new),
                false => split.second.split(target, axis, new),
            },
        }
    }

    /// Takes `target` out of the arrangement, the side that remains taking the
    /// place of a split it leaves with one side.
    pub fn remove(&mut self, target: &P) -> Removal {
        let split = match self {
            Layout::Pane(pane) => {
                return match pane == target {
                    true => Removal::Emptied,
                    false => Removal::NotFound,
                };
            }
            Layout::Split(split) => split,
        };
        let remaining = match split.first.remove(target) {
            Removal::Removed => return Removal::Removed,
            Removal::Emptied => (*split.second).clone(),
            Removal::NotFound => match split.second.remove(target) {
                Removal::NotFound => return Removal::NotFound,
                Removal::Removed => return Removal::Removed,
                Removal::Emptied => (*split.first).clone(),
            },
        };
        *self = remaining;
        Removal::Removed
    }
}

/// The panes of an arrangement, left to right and top to bottom.
pub struct Panes<'a, P> {
    unvisited: Vec<&'a Layout<P>>,
}

impl<'a, P> Iterator for Panes<'a, P> {
    type Item = &'a P;

    fn next(&mut self) -> Option<&'a P> {
        while let Some(layout) = self.unvisited.pop() {
            match layout {
                Layout::Pane(pane) => return Some(pane),
                Layout::Split(split) => {
                    self.unvisited.push(&split.second);
                    self.unvisited.push(&split.first);
                }
            }
        }
        None
    }
}

/// An arrangement written out: a pane as its name, a split as its two sides in
/// parentheses around `|` where it lays them out left to right and `/` where it
/// lays them out one above the other.
#[cfg(test)]
pub(crate) fn shape<P: std::fmt::Display>(layout: &Layout<P>) -> String {
    match layout {
        Layout::Pane(pane) => pane.to_string(),
        Layout::Split(split) => {
            let between = match split.axis {
                Axis::Horizontal => '|',
                Axis::Vertical => '/',
            };
            format!("({}{between}{})", shape(&split.first), shape(&split.second))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pane and the two more it is split into, laid out left to right.
    fn three_panes() -> Layout<char> {
        let mut layout = Layout::Pane('a');
        assert!(layout.split(&'a', Axis::Horizontal, 'b'));
        assert!(layout.split(&'b', Axis::Horizontal, 'c'));
        layout
    }

    #[test]
    fn splitting_a_pane_puts_it_and_the_new_pane_under_a_split() {
        let mut layout = Layout::Pane('a');
        assert!(layout.split(&'a', Axis::Vertical, 'b'));
        assert_eq!(shape(&layout), "(a/b)");
        assert_eq!(layout.panes().copied().collect::<Vec<_>>(), ['a', 'b']);
    }

    #[test]
    fn splitting_reaches_a_pane_at_any_depth() {
        let mut layout = three_panes();
        assert_eq!(shape(&layout), "(a|(b|c))");
        assert!(layout.split(&'c', Axis::Vertical, 'd'));
        assert_eq!(shape(&layout), "(a|(b|(c/d)))");
    }

    #[test]
    fn splitting_a_pane_no_arrangement_holds_changes_nothing() {
        let mut layout = three_panes();
        assert!(!layout.split(&'z', Axis::Horizontal, 'd'));
        assert_eq!(shape(&layout), "(a|(b|c))");
    }

    #[test]
    fn closing_either_side_of_a_split_leaves_the_other_in_its_place() {
        let mut first_closed = Layout::Pane('a');
        first_closed.split(&'a', Axis::Horizontal, 'b');
        assert_eq!(first_closed.remove(&'a'), Removal::Removed);
        assert_eq!(shape(&first_closed), "b");

        let mut second_closed = Layout::Pane('a');
        second_closed.split(&'a', Axis::Horizontal, 'b');
        assert_eq!(second_closed.remove(&'b'), Removal::Removed);
        assert_eq!(shape(&second_closed), "a");
    }

    #[test]
    fn a_split_left_with_one_side_is_replaced_by_it_at_every_depth() {
        let mut layout = Layout::Pane('a');
        layout.split(&'a', Axis::Horizontal, 'd');
        layout.split(&'a', Axis::Vertical, 'c');
        layout.split(&'a', Axis::Horizontal, 'b');
        assert_eq!(shape(&layout), "(((a|b)/c)|d)");

        // The innermost split loses a side, and the side left takes its place
        // while the two splits above it stand.
        assert_eq!(layout.remove(&'b'), Removal::Removed);
        assert_eq!(shape(&layout), "((a/c)|d)");
        // Each further close collapses the split that held the pane, one level
        // further out each time.
        assert_eq!(layout.remove(&'c'), Removal::Removed);
        assert_eq!(shape(&layout), "(a|d)");
        assert_eq!(layout.remove(&'a'), Removal::Removed);
        assert_eq!(shape(&layout), "d");
    }

    #[test]
    fn closing_the_last_pane_empties_the_arrangement() {
        let mut layout = Layout::Pane('a');
        assert_eq!(layout.remove(&'a'), Removal::Emptied);
    }

    #[test]
    fn closing_a_pane_no_arrangement_holds_changes_nothing() {
        let mut layout = three_panes();
        assert_eq!(layout.remove(&'z'), Removal::NotFound);
        assert_eq!(shape(&layout), "(a|(b|c))");
    }

    #[test]
    fn a_divider_is_moved_by_the_split_it_belongs_to() {
        let mut layout = three_panes();
        let ids: Vec<SplitId> = split_ids(&layout);
        assert!(layout.move_divider(ids[1], 0.25));
        assert_eq!(split_at(&layout, ids[0]).ratio(), 0.5);
        assert_eq!(split_at(&layout, ids[1]).ratio(), 0.25);
    }

    #[test]
    fn a_divider_drag_is_held_as_it_landed_and_bounded_where_it_is_read() {
        let mut layout = Layout::Pane('a');
        layout.split(&'a', Axis::Horizontal, 'b');
        let id = split_ids(&layout)[0];

        assert!(layout.move_divider(id, 0.8));
        assert_eq!(split_at(&layout, id).ratio(), 0.8);

        // A drag past the end is held where it asked for, so a later one back
        // toward the middle starts from there rather than from the bound.
        assert!(layout.move_divider(id, 1.5));
        assert_eq!(split_at(&layout, id).ratio(), 1.0 - MIN_PANE_SHARE);
        assert_eq!(split_at(&layout, id).ratio, 1.5);
        assert!(layout.move_divider(id, -0.5));
        assert_eq!(split_at(&layout, id).ratio(), MIN_PANE_SHARE);
    }

    #[test]
    fn moving_a_divider_of_no_split_here_changes_nothing() {
        let mut layout = three_panes();
        let elsewhere = SplitId::next();
        assert!(!layout.move_divider(elsewhere, 0.25));
        for id in split_ids(&layout) {
            assert_eq!(split_at(&layout, id).ratio(), 0.5);
        }
    }

    /// The splits of `layout`, outermost first.
    fn split_ids(layout: &Layout<char>) -> Vec<SplitId> {
        match layout {
            Layout::Pane(_) => Vec::new(),
            Layout::Split(split) => {
                let mut ids = vec![split.id];
                ids.extend(split_ids(&split.first));
                ids.extend(split_ids(&split.second));
                ids
            }
        }
    }

    fn find_split(layout: &Layout<char>, id: SplitId) -> Option<&Split<char>> {
        match layout {
            Layout::Pane(_) => None,
            Layout::Split(split) if split.id == id => Some(split),
            Layout::Split(split) => {
                find_split(&split.first, id).or_else(|| find_split(&split.second, id))
            }
        }
    }

    fn split_at(layout: &Layout<char>, id: SplitId) -> &Split<char> {
        find_split(layout, id).expect("the arrangement holds that split")
    }
}
