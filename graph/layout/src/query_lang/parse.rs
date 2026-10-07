//! The parser, a port of Bazel's
//! `com.google.devtools.build.lib.query2.engine.QueryParser`: every operator
//! has the same precedence and associates to the left, and `let` takes the
//! whole rest of the expression as its body.

use super::SyntaxError;
use super::ast::{Arg, ArgType, BinaryOp, Expr, ExprKind, Function, Word};
use super::lex::{self, Token, TokenKind};

/// How many tokens an error message quotes from where parsing stopped.
const ERROR_CONTEXT_TOKENS: usize = 3;

/// The longest expression an error message quotes in full, in bytes.
const MAX_QUOTED_EXPRESSION_BYTES: usize = 1000;
const TRUNCATION_MARK: &str = "[truncated]";

/// How deeply expressions may nest, so a parse, and every walk of the tree it
/// returns, stays well within a thread's stack. Bazel sets no bound of its own;
/// its parser fails only once the JVM's stack runs out.
const MAX_NESTING_DEPTH: usize = 200;

pub fn parse(input: &str) -> Result<Expr, SyntaxError> {
    let tokens = lex::scan(input)?;
    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
        depth: 0,
    };
    let expr = parser.expression()?;
    let token = parser.token();
    if token.kind != TokenKind::Eof {
        return Err(SyntaxError {
            message: format!(
                "unexpected token '{token}' after query expression '{}'",
                truncated(expr.to_string())
            ),
            span: token.span.clone(),
        });
    }
    Ok(expr)
}

struct Parser<'t, 'a> {
    /// Ends with the one [`TokenKind::Eof`], which `pos` never moves past.
    tokens: &'t [Token<'a>],
    pos: usize,
    /// How many expressions enclose the one being parsed.
    depth: usize,
}

impl<'t, 'a> Parser<'t, 'a> {
    fn token(&self) -> &'t Token<'a> {
        &self.tokens[self.pos]
    }

    fn advance(&mut self) -> &'t Token<'a> {
        let token = self.token();
        if token.kind != TokenKind::Eof {
            self.pos += 1;
        }
        token
    }

    fn consume(&mut self, kind: TokenKind) -> Result<&'t Token<'a>, SyntaxError> {
        if self.token().kind != kind {
            return Err(self.syntax_error());
        }
        Ok(self.advance())
    }

    fn consume_word(&mut self) -> Result<Word, SyntaxError> {
        let token = self.consume(TokenKind::Word)?;
        Ok(Word {
            text: token.word.to_owned(),
            span: token.span.clone(),
        })
    }

    fn expression(&mut self) -> Result<Expr, SyntaxError> {
        if self.depth == MAX_NESTING_DEPTH {
            return Err(SyntaxError {
                message: format!("expression nests deeper than {MAX_NESTING_DEPTH} levels"),
                span: self.token().span.clone(),
            });
        }
        self.depth += 1;
        let expr = self.primary().and_then(|first| self.binary_tail(first));
        self.depth -= 1;
        expr
    }

    fn binary_tail(&mut self, lhs: Expr) -> Result<Expr, SyntaxError> {
        let Some(mut op) = binary_op(self.token().kind) else {
            return Ok(lhs);
        };
        let mut first = lhs;
        let mut rest = Vec::new();
        while let Some(next) = binary_op(self.token().kind) {
            self.advance();
            if next != op {
                first = binary(op, first, std::mem::take(&mut rest));
                op = next;
            }
            rest.push(self.primary()?);
        }
        Ok(binary(op, first, rest))
    }

    fn primary(&mut self) -> Result<Expr, SyntaxError> {
        let start = self.token().span.start;
        match self.token().kind {
            TokenKind::Word => {
                let name_pos = self.pos;
                let word = self.consume_word()?;
                if self.token().kind != TokenKind::LParen {
                    let span = word.span.clone();
                    return Ok(Expr {
                        kind: ExprKind::Literal(target_literal(word)?),
                        span,
                    });
                }
                let Some(function) = Function::named(&word.text) else {
                    return Err(self.unknown_function(name_pos));
                };
                let args = self.arguments(function)?;
                let close = self.consume(TokenKind::RParen)?;
                Ok(Expr {
                    kind: ExprKind::Call { function, args },
                    span: start..close.span.end,
                })
            }
            TokenKind::Let => {
                self.advance();
                let name = self.consume_word()?;
                self.consume(TokenKind::Equals)?;
                let value = self.expression()?;
                self.consume(TokenKind::In)?;
                let body = self.expression()?;
                let end = body.span.end;
                Ok(Expr {
                    kind: ExprKind::Let {
                        name,
                        value: Box::new(value),
                        body: Box::new(body),
                    },
                    span: start..end,
                })
            }
            TokenKind::LParen => {
                self.advance();
                let mut expr = self.expression()?;
                let close = self.consume(TokenKind::RParen)?;
                expr.span = start..close.span.end;
                Ok(expr)
            }
            TokenKind::Set => {
                self.advance();
                self.consume(TokenKind::LParen)?;
                let mut words = Vec::new();
                while self.token().kind == TokenKind::Word {
                    words.push(target_literal(self.consume_word()?)?);
                }
                let close = self.consume(TokenKind::RParen)?;
                Ok(Expr {
                    kind: ExprKind::Set(words),
                    span: start..close.span.end,
                })
            }
            _ => Err(self.syntax_error()),
        }
    }

    /// Parses a call's arguments from its opening parenthesis up to, but not
    /// including, its closing one.
    fn arguments(&mut self, function: Function) -> Result<Vec<Arg>, SyntaxError> {
        let signature = function.signature();
        let mut args = Vec::new();
        let mut separator = TokenKind::LParen;
        while let Some(ty) = signature.param(args.len()) {
            if self.token().kind == TokenKind::RParen {
                if args.len() >= signature.mandatory {
                    break;
                }
                return Err(self.argument_count(function, "too few"));
            }
            self.consume(separator)?;
            separator = TokenKind::Comma;
            if args.is_empty() && self.token().kind == TokenKind::RParen {
                return Err(self.argument_count(function, "too few"));
            }
            args.push(match ty {
                ArgType::Expr => Arg::Expr(self.expression()?),
                ArgType::Word => Arg::Word(self.consume_word()?),
                ArgType::Int => {
                    let word = self.consume_word()?;
                    match word.text.parse() {
                        Ok(int) => Arg::Int(int),
                        Err(_) => {
                            return Err(SyntaxError {
                                message: format!("expected an integer literal: '{}'", word.text),
                                span: word.span,
                            });
                        }
                    }
                }
            });
        }
        if self.token().kind == TokenKind::Comma && !args.is_empty() {
            return Err(self.argument_count(function, "too many"));
        }
        Ok(args)
    }

    fn syntax_error(&self) -> SyntaxError {
        let token = self.token();
        let message = match token.kind {
            TokenKind::Eof => "premature end of input".into(),
            _ => format!("syntax error at '{}'", self.context(self.pos)),
        };
        SyntaxError {
            message,
            span: token.span.clone(),
        }
    }

    fn unknown_function(&self, name_pos: usize) -> SyntaxError {
        let token = &self.tokens[name_pos];
        let known: Vec<_> = Function::BAZEL.iter().map(|f| f.name()).collect();
        SyntaxError {
            message: format!(
                "unknown function '{token}' at '{}'; expected one of ['{}']",
                self.context(name_pos),
                known.join("', '")
            ),
            span: token.span.clone(),
        }
    }

    fn argument_count(&self, function: Function, count: &str) -> SyntaxError {
        SyntaxError {
            message: format!(
                "{count} arguments to function '{}' at '{}'",
                function.name(),
                self.context(self.pos)
            ),
            span: self.token().span.clone(),
        }
    }

    /// The tokens from `pos` an error quotes, short of the end of input.
    fn context(&self, pos: usize) -> String {
        let end = (pos + ERROR_CONTEXT_TOKENS).min(self.tokens.len() - 1);
        let quoted: Vec<_> = self.tokens[pos..end].iter().map(Token::to_string).collect();
        quoted.join(" ")
    }
}

fn binary_op(kind: TokenKind) -> Option<BinaryOp> {
    Some(match kind {
        TokenKind::Intersect => BinaryOp::Intersect,
        TokenKind::Caret => BinaryOp::Caret,
        TokenKind::Union => BinaryOp::Union,
        TokenKind::Plus => BinaryOp::Plus,
        TokenKind::Except => BinaryOp::Except,
        TokenKind::Minus => BinaryOp::Minus,
        _ => return None,
    })
}

fn binary(op: BinaryOp, first: Expr, rest: Vec<Expr>) -> Expr {
    let end = rest.last().map_or(first.span.end, |last| last.span.end);
    let span = first.span.start..end;
    Expr {
        kind: ExprKind::Binary {
            op,
            first: Box::new(first),
            rest,
        },
        span,
    }
}

/// Rejects a target literal that would read as an operator or a wildcard,
/// quoted or not.
fn target_literal(word: Word) -> Result<Word, SyntaxError> {
    match word.text.chars().next() {
        Some(lead @ ('-' | '*')) => Err(SyntaxError {
            message: format!("target literal must not begin with ({lead}): {}", word.text),
            span: word.span,
        }),
        _ => Ok(word),
    }
}

/// Shortens an expression an error quotes as Bazel's `QueryExpression
/// .toTrunctatedString()` does.
fn truncated(mut text: String) -> String {
    if text.len() <= MAX_QUOTED_EXPRESSION_BYTES {
        return text;
    }
    let mut cut = MAX_QUOTED_EXPRESSION_BYTES - TRUNCATION_MARK.len();
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    text.push_str(TRUNCATION_MARK);
    text
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use super::*;

    fn span_of(input: &str, needle: &str) -> Range<usize> {
        let start = input.find(needle).unwrap();
        start..start + needle.len()
    }

    #[test]
    fn spans_cover_what_each_expression_was_written_as() {
        let input = "deps((a + b) ^ c, 2)";
        let expr = parse(input).unwrap();
        assert_eq!(expr.span, 0..input.len());
        let ExprKind::Call { args, .. } = expr.kind else {
            panic!("{expr:?}");
        };
        let Arg::Expr(intersection) = &args[0] else {
            panic!("{args:?}");
        };
        assert_eq!(intersection.span, span_of(input, "(a + b) ^ c"));
        let ExprKind::Binary { first, .. } = &intersection.kind else {
            panic!("{intersection:?}");
        };
        assert_eq!(first.span, span_of(input, "(a + b)"));
    }

    #[test]
    fn an_error_points_at_the_token_parsing_stopped_at() {
        let input = "deps(a, 1, c)";
        let extra = input.rfind(',').unwrap();
        assert_eq!(parse(input).unwrap_err().span, extra..extra + 1);
        let input = "kind(x";
        assert_eq!(parse(input).unwrap_err().span, input.len()..input.len());
        let input = "nope(x)";
        assert_eq!(parse(input).unwrap_err().span, span_of(input, "nope"));
    }

    #[test]
    fn nesting_is_bounded() {
        let nested = |depth: usize| format!("{}a{}", "(".repeat(depth), ")".repeat(depth));
        // The outermost expression is one level, each parenthesis another.
        assert!(parse(&nested(MAX_NESTING_DEPTH - 1)).is_ok());
        let error = parse(&nested(MAX_NESTING_DEPTH)).unwrap_err();
        assert_eq!(error.span, MAX_NESTING_DEPTH..MAX_NESTING_DEPTH + 1);
        // Far past the bound, the parse fails rather than the stack.
        assert!(parse(&"(".repeat(1_000_000)).is_err());
        let lets = "let x = a in ".repeat(MAX_NESTING_DEPTH) + "x";
        assert!(parse(&lets).is_err());
    }

    #[test]
    fn rbuildfiles_takes_any_number_of_words() {
        assert_eq!(
            parse("rbuildfiles(a, b, c)").unwrap().to_string(),
            "rbuildfiles('a', 'b', 'c')"
        );
    }

    #[test]
    fn functions_are_listed_by_name() {
        for listed in [&Function::BAZEL[..], &Function::EXTENSIONS[..]] {
            let names: Vec<_> = listed.iter().map(|f| f.name()).collect();
            assert!(names.is_sorted(), "{names:?}");
        }
    }

    #[test]
    fn each_function_is_listed_once_as_bazels_or_an_extension() {
        for function in Function::ALL {
            let listed = (Function::ALL.iter()).filter(|&&listed| listed == function);
            assert_eq!(listed.count(), 1, "{function:?}");
            let list = match function.is_extension() {
                true => &Function::EXTENSIONS[..],
                false => &Function::BAZEL[..],
            };
            assert!(list.contains(&function), "{function:?}");
        }
    }

    #[test]
    fn an_extension_is_named_apart_from_bazels_functions() {
        for extension in Function::EXTENSIONS {
            assert!(!Function::BAZEL.contains(&extension), "{extension:?}");
            assert_eq!(Function::named(extension.name()), Some(extension));
        }
    }

    #[test]
    fn a_long_expression_is_quoted_truncated() {
        let long = "x".repeat(MAX_QUOTED_EXPRESSION_BYTES + 1);
        let message = parse(&format!("{long} )")).unwrap_err().message;
        let quoted = format!(
            "{}{TRUNCATION_MARK}",
            &long[..MAX_QUOTED_EXPRESSION_BYTES - TRUNCATION_MARK.len()]
        );
        assert_eq!(
            message,
            format!("unexpected token ')' after query expression '{quoted}'")
        );
    }
}
