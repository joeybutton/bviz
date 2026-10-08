//! Bazel's query language, read exactly as Bazel 9.2.0 reads it.
//!
//! Spans are byte ranges into the parsed text, so an error or an expression
//! can be pointed at where it was written.

mod ast;
mod config;
mod cursor;
mod declared;
mod eval;
mod functions;
mod lex;
mod narrow;
mod parse;
mod stack;
mod universe;

#[cfg(test)]
mod bazel_goldens;

use std::fmt;
use std::ops::Range;

pub use ast::{
    Arg, ArgType, BinaryOp, Expr, ExprKind, Function, Params, Signature, Vocabulary, Word,
};
pub use config::ANY_EXEC;
pub use cursor::{Place, TypedWord, word_at, written_word};
pub use eval::EvalError;
pub use parse::parse;
pub use stack::{
    BazelQuery, Chip, ChipExpr, ChipId, Command, Compiled, Evaluation, Extension, Invalid, Outcome,
    Prefixes, Stack, Unportable, Value,
};
pub use universe::{Counts, Readable, Universe};

/// Where an expression fails to scan or parse, with the message Bazel gives for
/// it word for word, but for two cases. A long expression Bazel quotes cut
/// inside a character is quoted cut before it, as a `String` must stay UTF-8;
/// and an expression nested past the parser's bound fails here, where Bazel
/// would parse it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SyntaxError {
    pub message: String,
    /// The text the error is about: the offending token, or the empty range at
    /// the end of the input when it ends too soon.
    pub span: Range<usize>,
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SyntaxError {}
