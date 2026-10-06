//! The word a cursor stands in while a query is typed, which a completion
//! replaces.

use std::ops::Range;

use super::ast::quoted;
use super::lex::{self, Lone, TokenKind};
use std::borrow::Cow;

/// A word being typed where a target pattern can stand.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TypedWord<'a> {
    /// Where the word is written, its quotes and all.
    pub span: Range<usize>,
    /// The word without its quotes.
    pub text: &'a str,
}

/// The word of `input` the cursor at byte `cursor` stands in or just past,
/// where a target pattern can stand there.
///
/// Words are scanned as the lexer scans them, but for a quotation still open,
/// which runs on from the cursor only as far as an unquoted word would, so
/// that what follows it is no part of the word. A quoted word is always a pattern. An
/// unquoted one is none where it is a keyword, a function's name (followed by
/// `(`), a variable (`$x`, or the name `let` binds), or a number.
pub fn word_at(input: &str, cursor: usize) -> Option<TypedWord<'_>> {
    let bytes = input.as_bytes();
    let cursor = cursor.min(bytes.len());
    let mut pos = 0;
    let mut after_let = false;
    while pos < bytes.len() && pos < cursor {
        let start = pos;
        let byte = bytes[pos];
        let (span, text, quoted) = match (lex::lone(byte), byte) {
            (Some(lone), _) => {
                pos += 1;
                if let Lone::Token(_) = lone {
                    after_let = false;
                }
                continue;
            }
            (None, b'\'' | b'"') => {
                let open = start + 1;
                let close = bytes[open..].iter().position(|&b| b == byte);
                let (text_end, end) = match close {
                    Some(close) => (open + close, open + close + 1),
                    None => {
                        let end = cursor + unquoted_word_len(&bytes[cursor..]);
                        (end, end)
                    }
                };
                (start..end, &input[open..text_end], true)
            }
            (None, _) => {
                let end = lex::word_end(bytes, start);
                (start..end, &input[start..end], false)
            }
        };
        pos = span.end;
        let bound = after_let;
        after_let = !quoted && text == "let";
        if cursor > span.end {
            continue;
        }
        let pattern = quoted || !(bound || unquoted_non_pattern(text, &input[span.end..]));
        return pattern.then_some(TypedWord { span, text });
    }
    None
}

/// `word` written to scan back as that one word: bare where it scans so,
/// quoted where it would not, as a keyword or a word holding a byte that ends
/// one would.
pub fn written_word(word: &str) -> Cow<'_, str> {
    let bare = match lex::scan(word).as_deref() {
        Ok([only, _eof]) => only.kind == TokenKind::Word && only.span == (0..word.len()),
        _ => false,
    };
    match bare {
        true => Cow::Borrowed(word),
        false => Cow::Owned(quoted(word)),
    }
}

/// How many of `bytes` lead up to the first that ends an unquoted word or
/// opens a quotation.
fn unquoted_word_len(bytes: &[u8]) -> usize {
    let ends = |&b: &u8| lex::lone(b).is_some() || b == b'\'' || b == b'"';
    bytes.iter().position(ends).unwrap_or(bytes.len())
}

/// Whether the unquoted word `text`, followed by `rest`, stands for anything
/// but a target pattern.
fn unquoted_non_pattern(text: &str, rest: &str) -> bool {
    lex::is_reserved(text)
        || text.starts_with('$')
        || text.bytes().all(|b| b.is_ascii_digit())
        || rest.trim_start().starts_with('(')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The word at the cursor written `|` into `typed`, as written and
    /// unquoted.
    fn at(typed: &str) -> Option<(String, String)> {
        let cursor = typed.find('|').expect("a cursor");
        let input = typed.replacen('|', "", 1);
        let word = word_at(&input, cursor)?;
        Some((input[word.span.clone()].to_owned(), word.text.to_owned()))
    }

    fn word(written: &str, text: &str) -> Option<(String, String)> {
        Some((written.to_owned(), text.to_owned()))
    }

    #[test]
    fn a_pattern_is_found_from_within_or_just_past_it() {
        assert_eq!(at("//app:li|"), word("//app:li", "//app:li"));
        assert_eq!(at("deps(//ap|p:lib)"), word("//app:lib", "//app:lib"));
        assert_eq!(at("a + @rep|"), word("@rep", "@rep"));
        assert_eq!(at("x except :b|in"), word(":bin", ":bin"));
    }

    #[test]
    fn a_free_word_is_a_pattern_too() {
        assert_eq!(at("cbind|"), word("cbind", "cbind"));
        assert_eq!(at("kind(rule, lib|)"), word("lib", "lib"));
    }

    #[test]
    fn a_quoted_word_is_a_pattern_closed_or_not() {
        assert_eq!(
            at("\"@@a++b//:c|\" + x"),
            word("\"@@a++b//:c\"", "@@a++b//:c")
        );
        assert_eq!(at("deps('//a b|"), word("'//a b", "//a b"));
        assert_eq!(at("deps(\"//ap|) + x"), word("\"//ap", "//ap"));
        assert_eq!(at("deps(\"//a|p:lib) + x"), word("\"//ap:lib", "//ap:lib"));
        assert_eq!(at("\"in|\""), word("\"in\"", "in"));
    }

    #[test]
    fn keywords_functions_variables_and_numbers_are_no_patterns() {
        assert_eq!(at("x inters|"), word("inters", "inters"));
        assert_eq!(at("x intersect|"), None);
        assert_eq!(at("dep|s(x)"), None);
        assert_eq!(at("deps |(x)"), None);
        assert_eq!(at("let v|"), None);
        assert_eq!(at("let v = x in $v|"), None);
        assert_eq!(at("deps(x, 2|)"), None);
    }

    /// A canonical repository name holds `+`, which continues a word that
    /// starts `@@`, so it is written bare; what would scan apart is quoted.
    #[test]
    fn a_word_is_quoted_only_where_it_would_not_scan_whole() {
        assert_eq!(written_word("//app:lib"), "//app:lib");
        assert_eq!(written_word("@@a++b+c//..."), "@@a++b+c//...");
        assert_eq!(written_word("@a+b//..."), "\"@a+b//...\"");
        assert_eq!(written_word("//a:my lib"), "\"//a:my lib\"");
        assert_eq!(written_word("//a:\"b\""), "'//a:\"b\"'");
        assert_eq!(written_word("in"), "\"in\"");
        assert_eq!(written_word("-x"), "\"-x\"");
    }

    #[test]
    fn a_cursor_between_words_stands_in_none() {
        assert_eq!(at("a |b"), None);
        assert_eq!(at("a(|"), None);
        assert_eq!(at("|"), None);
    }
}
