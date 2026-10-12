//! The terminal width a build's output is reconstructed at.

use alacritty_terminal::term::MIN_COLUMNS;

/// Widest terminal the emulator runs at: past what a terminal window spans on a
/// 4K display even at a small font (3840 pixels at a 4-pixel cell is 960
/// columns).
const MAX_COLUMNS: u16 = 1024;

/// A terminal width the emulator can run at: no narrower than alacritty's
/// minimum, below which its grid has no last column, and no wider than
/// [`MAX_COLUMNS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Columns(u16);

impl Columns {
    /// Width Bazel falls back to when no terminal is detected; see
    /// `--terminal_columns`.
    pub(crate) const DEFAULT: Columns = Columns(80);

    /// `columns`, where the emulator can run at that width.
    pub(crate) fn new(columns: u16) -> Option<Columns> {
        (usize::from(columns) >= MIN_COLUMNS && columns <= MAX_COLUMNS).then_some(Columns(columns))
    }

    /// The width nearest `columns` that the emulator can run at, for a width a
    /// window's size gives, which may fit fewer columns than the emulator holds.
    pub(crate) fn fitting(columns: usize) -> Columns {
        let columns = columns.clamp(MIN_COLUMNS, usize::from(MAX_COLUMNS));
        Columns(u16::try_from(columns).expect("clamped to a u16 bound"))
    }

    pub(crate) fn get(self) -> usize {
        usize::from(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_widths_the_emulator_runs_at() {
        let narrowest = u16::try_from(MIN_COLUMNS).expect("alacritty's minimum fits");
        assert_eq!(Columns::new(narrowest).map(Columns::get), Some(MIN_COLUMNS));
        assert_eq!(
            Columns::new(MAX_COLUMNS).map(Columns::get),
            Some(usize::from(MAX_COLUMNS))
        );
        assert_eq!(Columns::new(100).map(Columns::get), Some(100));
    }

    #[test]
    fn fits_a_window_width_to_the_widths_the_emulator_runs_at() {
        assert_eq!(Columns::fitting(0).get(), MIN_COLUMNS);
        assert_eq!(Columns::fitting(1).get(), MIN_COLUMNS);
        assert_eq!(Columns::fitting(100).get(), 100);
        assert_eq!(Columns::fitting(usize::MAX).get(), usize::from(MAX_COLUMNS));
    }

    #[test]
    fn rejects_a_width_the_emulator_cannot_hold() {
        assert_eq!(Columns::new(0), None);
        assert_eq!(Columns::new(1), None);
        assert_eq!(Columns::new(MAX_COLUMNS + 1), None);
        assert_eq!(Columns::new(u16::MAX), None);
    }
}
