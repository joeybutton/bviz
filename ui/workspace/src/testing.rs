//! Values the crate's own tests share.

/// A share is divided out of the flexes or the weights of the sides it is one
/// of, so shares that divide evenly in the rationals are even to within one
/// division's rounding — far under a pixel of any window.
pub(crate) const SHARE_ROUNDING: f32 = 1e-6;

/// The letter naming the pane or panel at `at` of the ones a test lays out, so
/// that an arrangement written out reads as the things it holds.
pub(crate) fn letter(at: usize) -> char {
    (b'a' + at as u8) as char
}
