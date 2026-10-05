//! A table sorted by a click on a column's header.

use gpui::{Div, Hsla, IntoElement, ParentElement, Pixels, Styled, div};

/// Which column of a table its rows are ordered by, and which way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sort<C> {
    pub by: C,
    pub descending: bool,
}

impl<C: Copy + PartialEq> Sort<C> {
    /// The sort a click on `by`'s header asks for: the other way where the
    /// table is sorted by it, and otherwise by it `descending` or not, the
    /// way that column is first read.
    pub fn clicked(self, by: C, descending: bool) -> Sort<C> {
        match self.by == by {
            true => Sort {
                by,
                descending: !self.descending,
            },
            false => Sort { by, descending },
        }
    }

    /// Whether the table is sorted by `column`, and if so, descending.
    pub fn of(self, column: C) -> Option<bool> {
        (self.by == column).then_some(self.descending)
    }
}

/// A sortable column header's `title`, with a caret pointing the way the
/// table is sorted where `sorted` says it is sorted by this column. The caret
/// stands on the side away from the title's alignment, `right_aligned` or
/// not, so the title holds still as the sort moves between columns.
pub fn sort_header(
    title: impl IntoElement,
    sorted: Option<bool>,
    right_aligned: bool,
    caret_size: Pixels,
    color: impl Into<Hsla>,
) -> Div {
    let caret = sorted.map(|descending| {
        let name = match descending {
            true => "caret-down",
            false => "caret-up",
        };
        bviz_ui_assets::icon(name)
            .size(caret_size)
            .flex_shrink_0()
            .text_color(color.into())
    });
    let content = div().flex().items_center().gap_1();
    match right_aligned {
        true => content.children(caret).child(title),
        false => content.child(title).children(caret),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_turns_the_sorted_column_around_and_sorts_another_its_first_way() {
        let sort = Sort {
            by: 'a',
            descending: true,
        };
        assert_eq!(
            sort.clicked('a', true),
            Sort {
                by: 'a',
                descending: false
            }
        );
        assert_eq!(
            sort.clicked('b', false),
            Sort {
                by: 'b',
                descending: false
            }
        );
        assert_eq!(sort.of('a'), Some(true));
        assert_eq!(sort.of('b'), None);
    }
}
