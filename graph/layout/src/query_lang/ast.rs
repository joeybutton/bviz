//! Parsed query expressions, which print back exactly as Bazel's
//! `QueryExpression.toString()` prints the same expression.

use std::fmt;
use std::ops::Range;

use super::lex;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    /// The expression's text, with any parentheses written around it.
    pub span: Range<usize>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ExprKind {
    /// A target pattern, or a `$name` reference to a `let` variable.
    Literal(Word),
    /// One run of the same operator: `a + b + c` is one node with `rest`
    /// `[b, c]`, `a + b - c` a `-` whose `first` is `a + b`.
    Binary {
        op: BinaryOp,
        first: Box<Expr>,
        rest: Vec<Expr>,
    },
    Let {
        name: Word,
        value: Box<Expr>,
        body: Box<Expr>,
    },
    Set(Vec<Word>),
    Call {
        function: Function,
        args: Vec<Arg>,
    },
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Word {
    /// Without the quotes the word may have been written in.
    pub text: String,
    /// Takes in the quotes, if any.
    pub span: Range<usize>,
}

/// A set operator, by the spelling it was written in: Bazel prints back the
/// one it read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinaryOp {
    Intersect,
    Caret,
    Union,
    Plus,
    Except,
    Minus,
}

impl BinaryOp {
    pub fn spelling(self) -> &'static str {
        match self {
            BinaryOp::Intersect => "intersect",
            BinaryOp::Caret => "^",
            BinaryOp::Union => "union",
            BinaryOp::Plus => "+",
            BinaryOp::Except => "except",
            BinaryOp::Minus => "-",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Arg {
    Expr(Expr),
    Word(Word),
    Int(i32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArgType {
    Expr,
    Word,
    Int,
}

/// The arguments a function takes, in order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Params {
    Fixed(&'static [ArgType]),
    /// Any number of arguments of the one type.
    Repeated(ArgType),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Signature {
    pub params: Params,
    /// How many leading parameters must be given; the rest may be left off.
    pub mandatory: usize,
}

impl Signature {
    /// The type of the argument at `index`, or `None` past the last parameter.
    pub fn param(self, index: usize) -> Option<ArgType> {
        match self.params {
            Params::Fixed(types) => types.get(index).copied(),
            Params::Repeated(ty) => Some(ty),
        }
    }
}

/// Every function `bazel query`, `cquery` and `aquery` accept between them.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Function {
    AllPaths,
    AllRdeps,
    Attr,
    BuildFiles,
    Config,
    Deps,
    Executables,
    Filter,
    Inputs,
    Kind,
    Labels,
    LoadFiles,
    Mnemonic,
    Outputs,
    RBuildFiles,
    Rdeps,
    SamePkgDirectRdeps,
    Siblings,
    Some,
    SomePath,
    Tests,
    Visible,
}

/// A function bviz evaluates over its graph.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Evaluable {
    AllPaths,
    AllRdeps,
    Attr,
    Config,
    Deps,
    Filter,
    Inputs,
    Kind,
    Mnemonic,
    Outputs,
    Rdeps,
    SamePkgDirectRdeps,
    Siblings,
    Some,
    SomePath,
}

impl Function {
    /// Every function, sorted by name.
    pub const ALL: [Function; 22] = [
        Function::AllPaths,
        Function::AllRdeps,
        Function::Attr,
        Function::BuildFiles,
        Function::Config,
        Function::Deps,
        Function::Executables,
        Function::Filter,
        Function::Inputs,
        Function::Kind,
        Function::Labels,
        Function::LoadFiles,
        Function::Mnemonic,
        Function::Outputs,
        Function::RBuildFiles,
        Function::Rdeps,
        Function::SamePkgDirectRdeps,
        Function::Siblings,
        Function::Some,
        Function::SomePath,
        Function::Tests,
        Function::Visible,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Function::AllPaths => "allpaths",
            Function::AllRdeps => "allrdeps",
            Function::Attr => "attr",
            Function::BuildFiles => "buildfiles",
            Function::Config => "config",
            Function::Deps => "deps",
            Function::Executables => "executables",
            Function::Filter => "filter",
            Function::Inputs => "inputs",
            Function::Kind => "kind",
            Function::Labels => "labels",
            Function::LoadFiles => "loadfiles",
            Function::Mnemonic => "mnemonic",
            Function::Outputs => "outputs",
            Function::RBuildFiles => "rbuildfiles",
            Function::Rdeps => "rdeps",
            Function::SamePkgDirectRdeps => "same_pkg_direct_rdeps",
            Function::Siblings => "siblings",
            Function::Some => "some",
            Function::SomePath => "somepath",
            Function::Tests => "tests",
            Function::Visible => "visible",
        }
    }

    pub fn named(name: &str) -> Option<Function> {
        Function::ALL
            .into_iter()
            .find(|function| function.name() == name)
    }

    /// Why no graph evaluates the function, as evaluating it fails; `None`
    /// for a function bviz evaluates.
    pub fn unevaluable(self) -> Option<String> {
        self.evaluable().err()
    }

    /// The function as one bviz evaluates, or why no graph evaluates it: it
    /// reads what bviz does not load into its graph.
    pub(super) fn evaluable(self) -> Result<Evaluable, String> {
        let unloaded = match self {
            Function::AllPaths => return Ok(Evaluable::AllPaths),
            Function::AllRdeps => return Ok(Evaluable::AllRdeps),
            Function::Attr => return Ok(Evaluable::Attr),
            Function::Config => return Ok(Evaluable::Config),
            Function::Deps => return Ok(Evaluable::Deps),
            Function::Filter => return Ok(Evaluable::Filter),
            Function::Inputs => return Ok(Evaluable::Inputs),
            Function::Kind => return Ok(Evaluable::Kind),
            Function::Mnemonic => return Ok(Evaluable::Mnemonic),
            Function::Outputs => return Ok(Evaluable::Outputs),
            Function::Rdeps => return Ok(Evaluable::Rdeps),
            Function::SamePkgDirectRdeps => return Ok(Evaluable::SamePkgDirectRdeps),
            Function::Siblings => return Ok(Evaluable::Siblings),
            Function::Some => return Ok(Evaluable::Some),
            Function::SomePath => return Ok(Evaluable::SomePath),
            Function::Labels => "the labels each rule's attributes name",
            Function::Visible => "each target's visibility",
            Function::Tests => "the tests each test suite names",
            Function::BuildFiles | Function::LoadFiles | Function::RBuildFiles => {
                "the BUILD and .bzl files of each package"
            }
            Function::Executables => "which rules are executable",
        };
        let name = self.name();
        Err(format!(
            "{name}() needs {unloaded}, which bviz does not load into its graph"
        ))
    }

    /// The signature Bazel declares for the function.
    pub fn signature(self) -> Signature {
        use ArgType::{Expr as E, Int as I, Word as W};
        let (params, mandatory): (&[ArgType], usize) = match self {
            Function::AllPaths | Function::SomePath | Function::Visible => (&[E, E], 2),
            Function::AllRdeps | Function::Deps | Function::Some => (&[E, I], 1),
            Function::Attr => (&[W, W, E], 3),
            Function::BuildFiles
            | Function::Executables
            | Function::LoadFiles
            | Function::SamePkgDirectRdeps
            | Function::Siblings
            | Function::Tests => (&[E], 1),
            Function::Config => (&[E, W], 2),
            Function::Filter | Function::Kind | Function::Labels => (&[W, E], 2),
            // Bazel lets aquery's filters drop the expression for
            // `--skyframe_state`, and rejects that when it evaluates them.
            Function::Inputs | Function::Mnemonic | Function::Outputs => (&[W, E], 1),
            Function::Rdeps => (&[E, E, I], 2),
            Function::RBuildFiles => {
                return Signature {
                    params: Params::Repeated(W),
                    mandatory: 1,
                };
            }
        };
        Signature {
            params: Params::Fixed(params),
            mandatory,
        }
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ExprKind::Literal(word) => write_literal(f, &word.text),
            ExprKind::Binary { op, first, rest } => {
                write!(f, "({first}")?;
                for operand in rest {
                    write!(f, " {} {operand}", op.spelling())?;
                }
                f.write_str(")")
            }
            ExprKind::Let { name, value, body } => {
                write!(f, "let {} = {value} in {body}", name.text)
            }
            ExprKind::Set(words) => {
                f.write_str("set(")?;
                for (index, word) in words.iter().enumerate() {
                    if index > 0 {
                        f.write_str(" ")?;
                    }
                    write_literal(f, &word.text)?;
                }
                f.write_str(")")
            }
            ExprKind::Call { function, args } => {
                write!(f, "{}(", function.name())?;
                for (index, arg) in args.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    match arg {
                        Arg::Expr(expr) => write!(f, "{expr}")?,
                        Arg::Word(word) => write!(f, "'{}'", word.text)?,
                        Arg::Int(int) => write!(f, "{int}")?,
                    }
                }
                f.write_str(")")
            }
        }
    }
}

/// Writes a target literal, quoted where it would not scan back as one word.
fn write_literal(f: &mut fmt::Formatter<'_>, pattern: &str) -> fmt::Result {
    const NEEDS_QUOTING: &[char] = &[
        ' ', '"', '#', '&', '\'', '(', ')', '+', ',', ';', '<', '=', '>', '?', '[', ']', '{', '|',
        '}',
    ];
    let needs_quoting = lex::is_reserved(pattern)
        || pattern.is_empty()
        || pattern.starts_with(['-', '*'])
        || pattern.contains(NEEDS_QUOTING);
    if !needs_quoting {
        return f.write_str(pattern);
    }
    f.write_str(&quoted(pattern))
}

/// `word` in the quotes that keep it whole: double quotes, or single ones
/// around a word holding a double quote.
pub(super) fn quoted(word: &str) -> String {
    let quote = if word.contains('"') { '\'' } else { '"' };
    format!("{quote}{word}{quote}")
}
