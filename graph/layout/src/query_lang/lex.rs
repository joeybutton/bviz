//! The tokenizer, a port of Bazel's
//! `com.google.devtools.build.lib.query2.engine.Lexer`.

use std::fmt;
use std::ops::Range;

use super::SyntaxError;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum TokenKind {
    Word,
    Eof,
    Comma,
    Equals,
    LParen,
    Minus,
    Plus,
    RParen,
    Caret,
    In,
    Let,
    Set,
    Intersect,
    Except,
    Union,
}

impl TokenKind {
    /// The keyword an unquoted word spells, if any.
    fn keyword(word: &str) -> Option<TokenKind> {
        Some(match word {
            "in" => TokenKind::In,
            "let" => TokenKind::Let,
            "set" => TokenKind::Set,
            "intersect" => TokenKind::Intersect,
            "except" => TokenKind::Except,
            "union" => TokenKind::Union,
            _ => return None,
        })
    }

    /// Bazel's name for the kind, which its error messages quote.
    pub(super) fn pretty_name(self) -> &'static str {
        match self {
            TokenKind::Word => "word",
            TokenKind::Eof => "EOF",
            TokenKind::Comma => ",",
            TokenKind::Equals => "=",
            TokenKind::LParen => "(",
            TokenKind::Minus => "-",
            TokenKind::Plus => "+",
            TokenKind::RParen => ")",
            TokenKind::Caret => "^",
            TokenKind::In => "in",
            TokenKind::Let => "let",
            TokenKind::Set => "set",
            TokenKind::Intersect => "intersect",
            TokenKind::Except => "except",
            TokenKind::Union => "union",
        }
    }
}

/// Whether `word` would scan as a keyword were it written unquoted.
pub(super) fn is_reserved(word: &str) -> bool {
    TokenKind::keyword(word).is_some()
}

#[derive(Clone, Debug)]
pub(super) struct Token<'a> {
    pub(super) kind: TokenKind,
    /// A word's text without its quotes; empty for every other kind.
    pub(super) word: &'a str,
    pub(super) span: Range<usize>,
}

impl fmt::Display for Token<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            TokenKind::Word => f.write_str(self.word),
            kind => f.write_str(kind.pretty_name()),
        }
    }
}

/// Splits `input` into tokens, ending with one [`TokenKind::Eof`].
///
/// Bazel scans the UTF-8 bytes of a query one at a time, so scanning bytes here
/// reads every input alike. Every non-ASCII byte continues a word, which keeps
/// each token's span on character boundaries.
pub(super) fn scan(input: &str) -> Result<Vec<Token<'_>>, SyntaxError> {
    let bytes = input.as_bytes();
    let mut tokens = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        let start = pos;
        let byte = bytes[pos];
        pos += 1;
        let kind = match byte {
            b'(' => TokenKind::LParen,
            b')' => TokenKind::RParen,
            b',' => TokenKind::Comma,
            b'+' => TokenKind::Plus,
            b'-' => TokenKind::Minus,
            b'=' => TokenKind::Equals,
            b'^' => TokenKind::Caret,
            b'\n' | b' ' | b'\t' | b'\r' => continue,
            b'\'' | b'"' => {
                let Some(close) = bytes[pos..].iter().position(|&b| b == byte) else {
                    return Err(SyntaxError {
                        message: "unclosed quotation".into(),
                        span: start..bytes.len(),
                    });
                };
                pos += close + 1;
                tokens.push(Token {
                    kind: TokenKind::Word,
                    word: &input[start + 1..pos - 1],
                    span: start..pos,
                });
                continue;
            }
            _ => {
                pos = word_end(bytes, start);
                let word = &input[start..pos];
                match TokenKind::keyword(word) {
                    Some(kind) => kind,
                    None => {
                        tokens.push(Token {
                            kind: TokenKind::Word,
                            word,
                            span: start..pos,
                        });
                        continue;
                    }
                }
            }
        };
        tokens.push(Token {
            kind,
            word: "",
            span: start..pos,
        });
    }
    tokens.push(Token {
        kind: TokenKind::Eof,
        word: "",
        span: bytes.len()..bytes.len(),
    });
    Ok(tokens)
}

/// The end of the unquoted word whose first byte is at `start`, which belongs
/// to the word whatever it is.
fn word_end(bytes: &[u8], start: usize) -> usize {
    // `+` continues only a canonical label, so `@@a++b//:c` is one word while
    // `@a+@b` is two operands.
    let canonical = bytes[start..].starts_with(b"@@");
    let mut pos = start + 1;
    while let Some(&byte) = bytes.get(pos) {
        let continues = match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' => true,
            b'*' | b'/' | b'@' | b'.' | b'-' | b'_' | b':' | b'$' | b'~' | b'[' | b']' => true,
            b'+' => canonical,
            _ => !byte.is_ascii(),
        };
        if !continues {
            break;
        }
        pos += 1;
    }
    pos
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_cover_each_token_and_its_quotes() {
        let tokens = scan("deps( 'a b' ,ü)").unwrap();
        let spans: Vec<_> = tokens.iter().map(|token| token.span.clone()).collect();
        assert_eq!(spans, [0..4, 4..5, 6..11, 12..13, 13..15, 15..16, 16..16]);
        assert_eq!(tokens[2].word, "a b");
    }

    #[test]
    fn an_unclosed_quotation_spans_to_the_end() {
        let error = scan("a 'b c").unwrap_err();
        assert_eq!(error.span, 2..6);
    }

    #[test]
    fn a_word_takes_its_first_byte_whatever_it_is() {
        let tokens = scan("!x #y").unwrap();
        let words: Vec<_> = tokens.iter().map(|token| token.to_string()).collect();
        assert_eq!(words, ["!x", "#y", "EOF"]);
    }
}
