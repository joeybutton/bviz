//! Where a line of text breaks into rows that fit a width.

use std::ops::Range;

/// Share of the width a row must fill before its break may be a worse kind
/// than one further along: past it, the better break wins over the longer row.
const MIN_ROW_FILL: f32 = 0.5;

/// A place a row may end, best first. A long label or path breaks only at
/// its own seams, so that each row reads as a whole part of it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Break {
    /// At a run of spaces, which the break consumes.
    Space,
    /// Before a `//`, so that a label's `//pkg` or `//:target` stays whole.
    BeforeDoubleSlash,
    /// After a lone `/`, ending a path segment.
    AfterSlash,
    /// Before an `@` or `:` that starts a repository or target name.
    BeforeSigil,
    /// After other punctuation, ending a part of a word.
    AfterPunctuation,
    /// Between any two characters but the two of a `//` or those after it.
    Anywhere,
}

const KINDS: usize = Break::Anywhere as usize + 1;

/// The kind of break between `before` and `at`, where `pre` precedes
/// `before` and `after` follows `at`; `None` where none may fall.
fn break_kind(pre: Option<char>, before: char, at: char, after: Option<char>) -> Option<Break> {
    let follows_double_slash = before == '/' && pre == Some('/');
    if before == ' ' || (before == '/' && at == '/') || follows_double_slash {
        return None;
    }
    Some(if at == ' ' {
        Break::Space
    } else if at == '/' && after == Some('/') {
        Break::BeforeDoubleSlash
    } else if before == '/' {
        Break::AfterSlash
    } else if (at == '@' && before != '@') || at == ':' {
        Break::BeforeSigil
    } else if before.is_ascii_punctuation() && at.is_alphanumeric() {
        Break::AfterPunctuation
    } else {
        Break::Anywhere
    })
}

/// The byte ranges of the rows `text` breaks into, each at most `max_width`
/// wide where `x_at` gives the offset of a byte index of `text` from its
/// start. A row ends at the best kind of break that fills enough of it, else
/// at the furthest break that fits; a row too narrow for any break holds its
/// first character, or its first `//`, whatever its width.
pub fn rows(text: &str, x_at: impl Fn(usize) -> f32, max_width: f32) -> Vec<Range<usize>> {
    let mut rows = Vec::new();
    let mut start = 0;
    loop {
        let left = x_at(start);
        if x_at(text.len()) - left <= max_width {
            rows.push(start..text.len());
            return rows;
        }
        let (end, next) = row_end(text, start, |ix| x_at(ix) - left, max_width);
        rows.push(start..end);
        start = next;
    }
}

/// Where the row starting at `start` ends and the next one starts, for a
/// `text` that runs past `max_width` from `start` as `width_to` measures it.
fn row_end(
    text: &str,
    start: usize,
    width_to: impl Fn(usize) -> f32,
    max_width: f32,
) -> (usize, usize) {
    let mut furthest: [Option<usize>; KINDS] = [None; KINDS];
    let mut chars = text[start..].char_indices().map(|(ix, c)| (start + ix, c));
    let mut pre = None;
    let Some((_, mut before)) = chars.next() else {
        return (text.len(), text.len());
    };
    let mut chars = chars.peekable();
    while let Some((ix, at)) = chars.next() {
        if width_to(ix) > max_width {
            break;
        }
        let after = chars.peek().map(|&(_, c)| c);
        if let Some(kind) = break_kind(pre, before, at, after) {
            furthest[kind as usize] = Some(ix);
        }
        pre = Some(before);
        before = at;
    }
    let min_fill = max_width * MIN_ROW_FILL;
    let end = furthest
        .iter()
        .flatten()
        .find(|&&ix| width_to(ix) >= min_fill)
        .or_else(|| furthest.iter().flatten().max())
        .copied()
        .unwrap_or_else(|| first_unit_end(&text[start..]) + start);
    let next = end + text[end..].len() - text[end..].trim_start_matches(' ').len();
    (end, next)
}

/// The end of the first character of `text`, or of its first two where they
/// are a `//`.
fn first_unit_end(text: &str) -> usize {
    if text.starts_with("//") {
        2
    } else {
        text.chars().next().map_or(0, char::len_utf8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Monospace: every character one unit wide.
    fn mono(text: &str, width: usize) -> Vec<&str> {
        rows(text, |ix| text[..ix].chars().count() as f32, width as f32)
            .into_iter()
            .map(|row| &text[row])
            .collect()
    }

    #[test]
    fn text_that_fits_is_one_row() {
        assert_eq!(mono("a short line", 40), ["a short line"]);
        assert_eq!(mono("", 40), [""]);
    }

    #[test]
    fn rows_break_at_spaces_and_drop_them() {
        assert_eq!(mono("one two  three four", 9), ["one two", "three", "four"]);
    }

    #[test]
    fn a_double_slash_is_never_split() {
        let label = "@@rules_rs++crate+crates__zerovec-derive-0.11.3//:zerovec-derive";
        for width in 1..label.len() {
            let rows = mono(label, width);
            assert_eq!(rows.concat(), label);
            for pair in rows.windows(2) {
                let split = pair[0].ends_with('/') && pair[1].starts_with('/');
                let dangling = pair[0].ends_with("//") && pair[0] != "//";
                assert!(!split && !dangling, "{width}: {rows:?}");
            }
        }
    }

    #[test]
    fn a_label_breaks_before_its_double_slash() {
        assert_eq!(
            mono(
                "Compiling Rust proc-macro @@crates__zerovec-derive-0.11.3//:zerovec-derive v0.11.3",
                64
            ),
            [
                "Compiling Rust proc-macro @@crates__zerovec-derive-0.11.3",
                "//:zerovec-derive v0.11.3",
            ]
        );
    }

    #[test]
    fn a_path_breaks_after_a_slash() {
        assert_eq!(
            mono(
                "Writing script external/llvm++llvm-project/compiler-rt/clang_rt.crtbegin.cppmap",
                60
            ),
            [
                "Writing script external/llvm++llvm-project/compiler-rt/",
                "clang_rt.crtbegin.cppmap",
            ]
        );
    }

    #[test]
    fn a_space_too_early_in_the_row_gives_way_to_a_later_seam() {
        assert_eq!(
            mono("at //some/long/package/path:target", 20),
            ["at //some/long/", "package/path:target"]
        );
    }

    #[test]
    fn a_word_with_no_seam_breaks_where_it_overflows() {
        assert_eq!(mono("abcdefghij", 4), ["abcd", "efgh", "ij"]);
    }

    #[test]
    fn a_row_too_narrow_for_anything_holds_its_first_unit() {
        assert_eq!(mono("//ab", 1), ["//", "a", "b"]);
    }
}
