//! Evaluates a parsed expression over a [`Universe`], as Bazel's query engine
//! evaluates it (`query2/engine/`), failing where Bazel fails with the message
//! Bazel gives.
//!
//! A level of the graph a walk starts at is the shallowest it covers: seeds at
//! different levels walk apart, each held to its own, and their results are
//! joined. Bazel holds one level per query, so it states nothing of this.

use std::fmt;
use std::ops::Range;

use fixedbitset::FixedBitSet;
use regex_automata::meta::Regex;

use super::ast::{Arg, BinaryOp, Evaluable, Expr, ExprKind, Function, Word};
use super::universe::Universe;
use crate::target_pattern::TargetPattern;

/// Why an expression could not be evaluated.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EvalError {
    /// Bazel's message for the failure where Bazel fails alike.
    pub message: String,
    /// The text of the expression that failed.
    pub span: Range<usize>,
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for EvalError {}

/// The values of the `let` variables in scope, the innermost last.
type Scope<'a> = Vec<(&'a str, FixedBitSet)>;

impl Universe {
    /// The nodes `expr` names.
    pub fn evaluate(&self, expr: &Expr) -> Result<FixedBitSet, EvalError> {
        self.eval(expr, &mut Scope::new())
    }

    /// The nodes `expr` names with the variable `name` bound to `bound`.
    pub(super) fn evaluate_with<'a>(
        &self,
        expr: &'a Expr,
        name: &'a str,
        bound: &FixedBitSet,
    ) -> Result<FixedBitSet, EvalError> {
        self.eval(expr, &mut vec![(name, bound.clone())])
    }

    fn eval<'a>(&self, expr: &'a Expr, scope: &mut Scope<'a>) -> Result<FixedBitSet, EvalError> {
        let fail = |message: String| EvalError {
            message,
            span: expr.span.clone(),
        };
        match &expr.kind {
            ExprKind::Literal(word) => self.literal(word, scope).map_err(fail),
            ExprKind::Binary { op, first, rest } => {
                let mut value = self.eval(first, scope)?;
                for operand in rest {
                    let operand = self.eval(operand, scope)?;
                    match op {
                        BinaryOp::Intersect | BinaryOp::Caret => value.intersect_with(&operand),
                        BinaryOp::Union | BinaryOp::Plus => value.union_with(&operand),
                        BinaryOp::Except | BinaryOp::Minus => value.difference_with(&operand),
                    }
                }
                Ok(value)
            }
            ExprKind::Let { name, value, body } => {
                if !is_variable_name(&name.text) {
                    let name = &name.text;
                    return Err(fail(format!(
                        "invalid variable name '{name}' in let expression"
                    )));
                }
                let value = self.eval(value, scope)?;
                scope.push((&name.text, value));
                let body = self.eval(body, scope);
                scope.pop();
                body
            }
            ExprKind::Set(words) => {
                let mut value = self.empty();
                for word in words {
                    let named = self.literal(word, scope).map_err(|message| EvalError {
                        message,
                        span: word.span.clone(),
                    })?;
                    value.union_with(&named);
                }
                Ok(value)
            }
            ExprKind::Call { function, args } => self.call(expr, *function, args, scope),
        }
    }

    /// What the literal `word` names: the value of the variable it refers to,
    /// or the targets of the pattern it writes.
    fn literal(&self, word: &Word, scope: &Scope<'_>) -> Result<FixedBitSet, String> {
        let text = word.text.as_str();
        if let Some(name) = text.strip_prefix('$').filter(|name| is_variable_name(name)) {
            let bound = scope.iter().rev().find(|(bound, _)| *bound == name);
            return match bound {
                Some((_, value)) => Ok(value.clone()),
                None => Err(format!("undefined variable '{name}'")),
            };
        }
        let pattern = TargetPattern::parse(text)?;
        Ok(self.with_configured(self.targets.targets(&pattern)?))
    }

    fn call<'a>(
        &self,
        call: &'a Expr,
        function: Function,
        args: &'a [Arg],
        scope: &mut Scope<'a>,
    ) -> Result<FixedBitSet, EvalError> {
        let fail = |message: String| EvalError {
            message,
            span: call.span.clone(),
        };
        let evaluable = function.evaluable().map_err(fail)?;
        // The parser admits a call only with the arguments its signature
        // declares, so each position holds the type the function reads there.
        let mut arguments = || self.arguments(args, scope);
        match evaluable {
            Evaluable::Deps => {
                let (values, depth) = arguments()?;
                Ok(self.deps(&values[0], depth))
            }
            Evaluable::AllRdeps => {
                let (values, depth) = arguments()?;
                Ok(self.rdeps(None, &values[0], depth))
            }
            Evaluable::Rdeps => {
                let (values, depth) = arguments()?;
                if values[1].is_clear() {
                    return Ok(self.empty());
                }
                let universe = self.deps(&values[0], None);
                Ok(self.rdeps(Some(&universe), &values[1], depth))
            }
            Evaluable::AllPaths => {
                let (values, _) = arguments()?;
                Ok(self.all_paths(&values[0], &values[1]))
            }
            Evaluable::SomePath => {
                let (values, _) = arguments()?;
                Ok(self.some_path(&values[0], &values[1]))
            }
            Evaluable::Siblings => Ok(self.siblings(&arguments()?.0[0])),
            Evaluable::SamePkgDirectRdeps => {
                let (values, _) = arguments()?;
                self.same_package_direct_rdeps(&values[0]).map_err(fail)
            }
            Evaluable::Some => {
                let (values, count) = arguments()?;
                let some = self.some(&values[0], count.unwrap_or(1));
                some.ok_or_else(|| fail("argument set is empty".to_string()))
            }
            // Bazel compiles the pattern before it evaluates the expression
            // filtered, so a pattern it cannot compile fails first.
            Evaluable::Kind => {
                let regex = self.regex(function, words(args)[0]).map_err(fail)?;
                let (values, _) = arguments()?;
                Ok(self.kind(&regex, &values[0]))
            }
            Evaluable::Filter => {
                let regex = self.regex(function, words(args)[0]).map_err(fail)?;
                let (values, _) = arguments()?;
                Ok(self.filter(&regex, &values[0]))
            }
            Evaluable::Attr => {
                let words = words(args);
                let regex = self.regex(function, words[1]).map_err(fail)?;
                let attribute = self.attribute(words[0]).map_err(fail)?;
                let (values, _) = arguments()?;
                Ok(self.attr(attribute, &regex, &values[0]))
            }
            Evaluable::Config => {
                let (values, _) = arguments()?;
                let written = expressions(args)[0].to_string();
                let config = self.config(&values[0], words(args)[0], &written);
                config.map_err(fail)
            }
            Evaluable::Mnemonic => self.filter_actions(call, function, args, scope, Self::mnemonic),
            Evaluable::Inputs => self.filter_actions(call, function, args, scope, Self::inputs),
            Evaluable::Outputs => self.filter_actions(call, function, args, scope, Self::outputs),
        }
    }

    /// Evaluates a call of the aquery filter `function`, which keeps the
    /// actions `keep` keeps by the pattern of its first argument.
    fn filter_actions<'a>(
        &self,
        call: &'a Expr,
        function: Function,
        args: &'a [Arg],
        scope: &mut Scope<'a>,
        keep: fn(&Self, &Regex, &FixedBitSet) -> FixedBitSet,
    ) -> Result<FixedBitSet, EvalError> {
        let fail = |message: String| EvalError {
            message,
            span: call.span.clone(),
        };
        let regex = self.whole_regex(function, words(args)[0]).map_err(fail)?;
        self.filters_actions(function).map_err(fail)?;
        let (values, _) = self.arguments(args, scope)?;
        let Some(value) = values.first() else {
            return Err(fail(
                "aquery filter functions (inputs, outputs, mnemonics) must have exactly 2 \
                 arguments,except when --skyframe_state is used."
                    .to_string(),
            ));
        };
        Ok(keep(self, &regex, value))
    }

    /// The values of the expressions among `args`, in order, and the integer
    /// among them, which a function takes at most one of.
    fn arguments<'a>(
        &self,
        args: &'a [Arg],
        scope: &mut Scope<'a>,
    ) -> Result<(Vec<FixedBitSet>, Option<i32>), EvalError> {
        let mut values: Vec<FixedBitSet> = Vec::with_capacity(args.len());
        let mut int = None;
        for arg in args {
            match arg {
                Arg::Expr(expr) => values.push(self.eval(expr, scope)?),
                Arg::Int(value) => int = Some(*value),
                Arg::Word(_) => {}
            }
        }
        Ok((values, int))
    }
}

/// The expressions among `args`, in order.
fn expressions(args: &[Arg]) -> Vec<&Expr> {
    let expressions = args.iter().filter_map(|arg| match arg {
        Arg::Expr(expr) => Some(expr),
        Arg::Word(_) | Arg::Int(_) => None,
    });
    expressions.collect()
}

/// The words among `args`, in order.
fn words(args: &[Arg]) -> Vec<&str> {
    let words = args.iter().filter_map(|arg| match arg {
        Arg::Word(word) => Some(word.text.as_str()),
        Arg::Expr(_) | Arg::Int(_) => None,
    });
    words.collect()
}

/// Whether `name` is a variable name `let` binds (`LetExpression.NAME_PATTERN`).
fn is_variable_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests;
