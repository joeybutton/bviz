//! How a label or package path a reader writes is shown on screen, which may
//! read shorter than it is written.

use std::ops::Range;

/// Text as it is shown, and where each char of it as written stands in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    text: String,
    /// The chars of the written text the shown text leaves out of its
    /// repository part.
    dropped: Range<u32>,
}

impl Shown {
    /// `written` shown as it is written.
    pub(crate) fn whole(written: &str) -> Self {
        Self {
            text: written.to_owned(),
            dropped: 0..0,
        }
    }

    /// `written` with the chars `dropped` left out.
    pub(crate) fn dropping(written: &str, dropped: Range<u32>) -> Self {
        let mut text = String::with_capacity(written.len());
        let kept = (0u32..).zip(written.chars());
        text.extend(kept.filter(|(at, _)| !dropped.contains(at)).map(|(_, c)| c));
        Self { text, dropped }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn into_text(self) -> String {
        self.text
    }

    /// The char of the shown text standing for the char `written` of the text
    /// as written, where one does.
    pub fn at(&self, written: u32) -> Option<u32> {
        let Range { start, end } = self.dropped;
        match written {
            _ if written < start => Some(written),
            _ if written < end => None,
            _ => Some(written - (end - start)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_char_left_out_stands_nowhere_and_those_after_it_move_up() {
        let shown = Shown::dropping("@@a+b//c:d", 1..4);
        assert_eq!(shown.text(), "@b//c:d");
        assert_eq!(shown.at(0), Some(0));
        assert_eq!(shown.at(3), None);
        assert_eq!(shown.at(4), Some(1));
        assert_eq!(shown.at(9), Some(6));
    }
}
