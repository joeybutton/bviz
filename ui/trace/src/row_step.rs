//! Stepping through the rows of a table in the order it shows them.

/// The row `by` rows from `from` in `order`, stopping at either end, or the
/// first row where `from` is none of it; `None` for no rows at all.
pub(crate) fn moved<T: Copy + PartialEq>(order: &[T], from: Option<T>, by: isize) -> Option<T> {
    let last = order.len().checked_sub(1)?;
    let position = match from.and_then(|from| position(order, from)) {
        Some(position) => position.saturating_add_signed(by).min(last),
        None => 0,
    };
    Some(order[position])
}

/// Where row `index` stands in `order`.
pub(crate) fn position<T: PartialEq>(order: &[T], index: T) -> Option<usize> {
    order.iter().position(|row| *row == index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stepping_through_rows_follows_the_order_shown() {
        let order = [2, 0, 3, 1];
        assert_eq!(moved(&order, None, 1), Some(2));
        assert_eq!(moved(&order, Some(0), 1), Some(3));
        assert_eq!(moved(&order, Some(0), -1), Some(2));
        assert_eq!(moved(&order, Some(2), -1), Some(2));
        assert_eq!(moved(&order, Some(1), 1), Some(1));
        assert_eq!(moved::<usize>(&[], None, 1), None);
        assert_eq!(position(&order, 3), Some(2));
    }
}
