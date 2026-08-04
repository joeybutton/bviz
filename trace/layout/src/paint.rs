/// A horizontal span of pixels, `[x, x + w)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Span {
    pub x: f64,
    pub w: f64,
}

impl Span {
    pub(crate) fn end(self) -> f64 {
        self.x + self.w
    }
}

/// Where `content` — a box's own span — is painted at its depth, against
/// `covered`, the right edge painted at that depth so far ([`f64::NEG_INFINITY`]
/// where nothing is).
///
/// A box at least `min` wide is painted at its own span. A narrower one is
/// widened into a marker of where to zoom in, at least `min` wide, starting at
/// the later of the covered edge — so it takes up exactly where the box before it
/// left off — and the pixel column its own start falls in, so a marker opening a
/// run sits on the pixel grid and the run holds its phase as the view scrolls.
/// `None` where the painted region reaches past the box's start and to its end:
/// the box before it stands for it, which is what holds a run of sub-pixel boxes
/// to one painted box per `min` of width.
///
/// Painted spans therefore touch wherever the boxes they stand for touch, so a
/// contiguous run paints as an unbroken band.
pub(crate) fn painted(content: Span, covered: f64, min: f64) -> Option<Span> {
    if content.w >= min {
        return Some(content);
    }
    if covered > content.x && covered >= content.end() {
        return None;
    }
    let x = content.x.floor().max(covered);
    Some(Span {
        x,
        w: (content.end() - x).max(min),
    })
}
