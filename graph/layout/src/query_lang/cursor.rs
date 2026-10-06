//! The word a cursor stands in while a query is typed, which a completion
//! replaces.

use std::ops::Range;

use super::ast::{ArgType, Function, quoted};
use super::lex::{self, Lone, TokenKind};
use std::borrow::Cow;

/// A word being typed where a target pattern or a function's name can
/// stand.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TypedWord<'a> {
    /// Where the word is written, its quotes and all.
    pub span: Range<usize>,
    /// The word without its quotes.
    pub text: &'a str,
    pub place: Place,
}

/// What a typed word can be, by where it stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Place {
    /// A target pattern alone: a quoted word, or one past where an
    /// expression starts.
    Pattern,
    /// A target pattern or a function's name, where an expression starts:
    /// the start of the query, or past `(`, `,`, `=`, a set operator or `in`.
    Operand,
    /// The name of a function, followed by its `(`.
    Callee,
}

/// The word of `input` the cursor at byte `cursor` stands in or just past,
/// and what it can be there; `None` where it can be neither a pattern nor a
/// function's name.
///
/// Words are scanned as the lexer scans them, but for a quotation still open,
/// which runs on from the cursor only as far as an unquoted word would, so
/// that what follows it is no part of the word. A word is neither where it
/// stands for an argument a function reads as a word or a number, nor
/// unquoted where it is a keyword, a variable (`$x`, or the name `let`
/// binds), or a number; within `set(…)` it is a pattern alone.
pub fn word_at(input: &str, cursor: usize) -> Option<TypedWord<'_>> {
    let bytes = input.as_bytes();
    let cursor = cursor.min(bytes.len());
    let mut pos = 0;
    let mut after_let = false;
    let mut operand = true;
    // The parentheses open before the cursor, and the unquoted word just
    // before the next one, which names what a `(` opens.
    let mut frames: Vec<Frame> = Vec::new();
    let mut callee: Option<&str> = None;
    while pos < bytes.len() && pos < cursor {
        let start = pos;
        let byte = bytes[pos];
        let (span, text, quoted) = match (lex::lone(byte), byte) {
            (Some(lone), _) => {
                pos += 1;
                if let Lone::Token(kind) = lone {
                    after_let = false;
                    operand = opens_operand(kind);
                    enclose(&mut frames, kind, callee.take());
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
        let (bound, at_operand) = (after_let, operand);
        after_let = !quoted && text == "let";
        operand = !quoted && OPERATOR_WORDS.contains(&text);
        callee = (!quoted).then_some(text);
        if cursor > span.end {
            continue;
        }
        let place = match (frames.last().map_or(Slot::Expr, Frame::slot), quoted) {
            (Slot::Other, _) => None,
            (Slot::Expr, false) => unquoted_place(text, &input[span.end..], bound, at_operand),
            (Slot::Expr, true) => Some(Place::Pattern),
            (Slot::Pattern, false) => unquoted_place(text, &input[span.end..], bound, false)
                .filter(|&place| place == Place::Pattern),
            (Slot::Pattern, true) => Some(Place::Pattern),
        };
        return place.map(|place| TypedWord { span, text, place });
    }
    None
}

/// What a `(` opens.
enum Frame {
    /// A call of `function`, at the argument `argument` counts from zero.
    Call { function: Function, argument: usize },
    /// The words of `set(…)`.
    Set,
    /// An expression grouped.
    Group,
}

/// What a word can stand for.
enum Slot {
    Expr,
    /// A target pattern alone.
    Pattern,
    /// Something completed by neither a pattern nor a function: a word or a
    /// number a function reads, or an argument past its last.
    Other,
}

impl Frame {
    /// What a word stands for directly within this.
    fn slot(&self) -> Slot {
        match self {
            Frame::Group => Slot::Expr,
            Frame::Set => Slot::Pattern,
            Frame::Call { function, argument } => match function.signature().param(*argument) {
                Some(ArgType::Expr) => Slot::Expr,
                Some(ArgType::Word | ArgType::Int) | None => Slot::Other,
            },
        }
    }
}

/// Takes the one-byte token `kind` into the parentheses `frames` holds open:
/// a `(` opens what `callee`, the unquoted word just before it, names; a `,`
/// moves a call on to its next argument; a `)` closes the innermost.
fn enclose(frames: &mut Vec<Frame>, kind: TokenKind, callee: Option<&str>) {
    match kind {
        TokenKind::LParen => frames.push(match callee {
            Some("set") => Frame::Set,
            Some(name) => match Function::named(name) {
                Some(function) => Frame::Call {
                    function,
                    argument: 0,
                },
                None => Frame::Group,
            },
            None => Frame::Group,
        }),
        TokenKind::Comma => {
            if let Some(Frame::Call { argument, .. }) = frames.last_mut() {
                *argument += 1;
            }
        }
        TokenKind::RParen => {
            frames.pop();
        }
        _ => {}
    }
}

/// The keywords past which an expression starts.
const OPERATOR_WORDS: &[&str] = &["in", "intersect", "union", "except"];

/// Whether an expression starts past the one-byte token `kind`.
fn opens_operand(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::LParen
            | TokenKind::Comma
            | TokenKind::Equals
            | TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Caret
    )
}

/// What the unquoted word `text`, followed by `rest`, can be: `None` for a
/// keyword, a variable, the name a `let` binds (`bound`) or a number; a
/// function's name where `(` follows; and a pattern, or a function's name
/// too `at_operand`, otherwise.
fn unquoted_place(text: &str, rest: &str, bound: bool, at_operand: bool) -> Option<Place> {
    if bound || lex::is_reserved(text) || text.starts_with('$') {
        return None;
    }
    if rest.trim_start().starts_with('(') {
        return Some(Place::Callee);
    }
    if text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(match at_operand {
        true => Place::Operand,
        false => Place::Pattern,
    })
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
        assert_eq!(at("deps |(x)"), None);
        assert_eq!(at("let v|"), None);
        assert_eq!(at("let v = x in $v|"), None);
        assert_eq!(at("deps(x, 2|)"), None);
    }

    /// What the word at the cursor written `|` into `typed` can be.
    fn place(typed: &str) -> Option<Place> {
        let cursor = typed.find('|').expect("a cursor");
        let input = typed.replacen('|', "", 1);
        word_at(&input, cursor).map(|word| word.place)
    }

    /// A function's name can stand wherever an expression starts, and a
    /// word already followed by `(` is one.
    #[test]
    fn a_function_can_stand_where_an_expression_starts() {
        assert_eq!(place("dep|"), Some(Place::Operand));
        assert_eq!(place("deps(ki|"), Some(Place::Operand));
        assert_eq!(place("rdeps(//a, so|"), Some(Place::Operand));
        assert_eq!(place("x + ki|"), Some(Place::Operand));
        assert_eq!(place("x intersect ki|"), Some(Place::Operand));
        assert_eq!(place("let v = ki|"), Some(Place::Operand));
        assert_eq!(place("let v = x in de|"), Some(Place::Operand));
        assert_eq!(place("dep|s(x)"), Some(Place::Callee));
        assert_eq!(place("x + rd|eps (x)"), Some(Place::Callee));
        assert_eq!(place("\"de|"), Some(Place::Pattern));
        assert_eq!(place("deps(x) ki|"), Some(Place::Pattern));
        assert_eq!(place("'//a' ki|"), Some(Place::Pattern));
    }

    /// A word standing for an argument a function reads as a word or a
    /// number is completed by nothing; within `set(…)` by patterns alone.
    #[test]
    fn a_functions_argument_takes_what_its_signature_reads() {
        assert_eq!(place("kind(de|"), None);
        assert_eq!(place("kind(rule, de|"), Some(Place::Operand));
        assert_eq!(place("deps(x, de|"), None);
        assert_eq!(place("deps(de|"), Some(Place::Operand));
        assert_eq!(place("rdeps(x, de|"), Some(Place::Operand));
        assert_eq!(place("rdeps(x, y, de|"), None);
        assert_eq!(place("attr(tags, de|"), None);
        assert_eq!(place("set(de|"), Some(Place::Pattern));
        assert_eq!(place("set(//a de|"), Some(Place::Pattern));
        assert_eq!(place("(de|"), Some(Place::Operand));
        assert_eq!(place("kind(rule, deps(x)) + de|"), Some(Place::Operand));
        assert_eq!(place("kind(rule, deps(x, 1), de|"), None);
        assert_eq!(place("rd|eps(x)"), Some(Place::Callee));
        assert_eq!(place("deps(rd|eps(x))"), Some(Place::Callee));
        assert_eq!(place("kind(rd|eps(x)"), None);
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
