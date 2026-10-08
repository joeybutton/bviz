//! A query written as a stack of chips: sources, whose union the stack starts
//! from, and refinements, each taking the set before it and naming another.
//!
//! A refinement reads the set before it as `$_`, and three shorthands write
//! that for it: a chip opening with a set operator applies it to `$_`
//! (`+ //foo` is `$_ + //foo`); a call short only of its last expression
//! argument takes `$_` there (`kind(cc_.*)` is `kind(cc_.*, $_)`); and an
//! empty call whose first argument is an expression takes `$_` as it
//! (`deps()` is `deps($_)`). The call shorthands apply as well to a call
//! that is all of what a leading operator applies to (`except kind(test)` is
//! `$_ except kind(test, $_)`). A chip reading no `$_` is a source, unless it is
//! negated: a negated chip is a refinement taking what it names out of the set
//! before it. The stack holds its sources ahead of its refinements and is the
//! expression `let _ = (s1 + … + sm) in let _ = r1 in … rn` over its enabled
//! chips; with no source enabled, the first refinement reads every node.

use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::sync::{Arc, Mutex};

use fixedbitset::FixedBitSet;
use rustc_hash::FxHashMap;

use super::SyntaxError;
use super::ast::{Arg, ArgType, BinaryOp, Expr, ExprKind, Function, Params, Word};
use super::config::ANY_EXEC;
use super::eval::{EvalError, Named};
use super::lex::{self, Token, TokenKind};
use super::narrow::ActionFilter;
use super::parse::parse;
use super::universe::{Counts, Universe};

/// The variable a chip reads the set before it from.
const CONTINUED: &str = "_";

/// That variable as a chip writes it.
const CONTINUED_REF: &str = "$_";

/// The most chip prefixes a [`Prefixes`] keeps the value of. A stack holds few
/// chips, but a session of edits to one names many prefixes, so the cache
/// empties when full.
const MAX_CACHED_PREFIXES: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ChipId(u64);

impl ChipId {
    /// The chip's number, unique within its stack.
    pub fn number(self) -> u64 {
        self.0
    }
}

/// One chip of a [`Stack`].
#[derive(Clone, Debug)]
pub struct Chip {
    id: ChipId,
    text: String,
    written: Written,
    /// Spans point into `written.source`; a syntax error's points into `text`.
    parsed: Result<ChipExpr, SyntaxError>,
    enabled: bool,
    negated: bool,
}

/// What a chip's text says once its shorthand is written out.
#[derive(Clone, Debug)]
pub enum ChipExpr {
    /// Names a set reading nothing before it.
    Whole(Expr),
    /// Reads the set before it as `$_`.
    Continues(Expr),
}

impl ChipExpr {
    fn expr(&self) -> &Expr {
        match self {
            ChipExpr::Whole(expr) | ChipExpr::Continues(expr) => expr,
        }
    }
}

/// A chip's text with its shorthand written out, and where the writing
/// inserted text, so that a span into the one maps back onto the other.
#[derive(Clone, Debug)]
struct Written {
    source: String,
    /// Each insertion as the position in the chip's text it was made at and
    /// the length it inserted, by increasing position.
    insertions: Vec<(usize, usize)>,
    leading_operator: Option<Range<usize>>,
}

impl Written {
    fn of(text: &str) -> Self {
        let Ok(tokens) = lex::scan(text) else {
            return Written::inserting(text, &[]);
        };
        let opens_with_operator = matches!(
            tokens[0].kind,
            TokenKind::Plus
                | TokenKind::Minus
                | TokenKind::Caret
                | TokenKind::Intersect
                | TokenKind::Except
                | TokenKind::Union
        );
        let (leading, call) = match opens_with_operator {
            true => (Some((0, "$_ ")), &tokens[1..]),
            false => (None, &tokens[..]),
        };
        let inserts: Vec<(usize, &str)> = leading
            .into_iter()
            .chain(continued_argument(call))
            .collect();
        Written {
            leading_operator: opens_with_operator.then(|| tokens[0].span.clone()),
            ..Written::inserting(text, &inserts)
        }
    }

    /// `text` with each of `inserts`, by increasing position, inserted at its
    /// position.
    fn inserting(text: &str, inserts: &[(usize, &str)]) -> Self {
        let added: usize = inserts.iter().map(|(_, insert)| insert.len()).sum();
        let mut source = String::with_capacity(text.len() + added);
        let mut insertions = Vec::with_capacity(inserts.len());
        let mut copied = 0;
        for &(at, insert) in inserts {
            source.push_str(&text[copied..at]);
            source.push_str(insert);
            insertions.push((at, insert.len()));
            copied = at;
        }
        source.push_str(&text[copied..]);
        Written {
            source,
            insertions,
            leading_operator: None,
        }
    }

    /// The span of the chip's text that `span` of the source covers; a span
    /// within an inserted text covers the point it was inserted at.
    fn span_in_text(&self, span: &Range<usize>) -> Range<usize> {
        let at = |position: usize| {
            let mut inserted_before = 0;
            for &(at, len) in &self.insertions {
                let start = at + inserted_before;
                if position >= start + len {
                    inserted_before += len;
                } else if position > start {
                    return at;
                } else {
                    break;
                }
            }
            position - inserted_before
        };
        at(span.start)..at(span.end)
    }
}

/// Where a chip written as one call short of its last expression argument
/// takes `$_`, and the text inserted there.
fn continued_argument(tokens: &[Token<'_>]) -> Option<(usize, &'static str)> {
    let [name, open, ..] = tokens else {
        return None;
    };
    if name.kind != TokenKind::Word || open.kind != TokenKind::LParen {
        return None;
    }
    let function = Function::named(name.word)?;
    let (mut depth, mut commas, mut close) = (0usize, 0usize, None);
    for (at, token) in tokens.iter().enumerate().skip(1) {
        match token.kind {
            TokenKind::LParen => depth += 1,
            TokenKind::RParen => {
                depth -= 1;
                if depth == 0 {
                    close = Some(at);
                    break;
                }
            }
            TokenKind::Comma if depth == 1 => commas += 1,
            _ => {}
        }
    }
    let close = close?;
    if tokens[close + 1].kind != TokenKind::Eof {
        return None;
    }
    let given = if close == 2 { 0 } else { commas + 1 };
    let signature = function.signature();
    let Params::Fixed(params) = signature.params else {
        return None;
    };
    let at = tokens[close].span.start;
    if given == 0 && params.first() == Some(&ArgType::Expr) {
        return Some((at, CONTINUED_REF));
    }
    let last_expression = params.iter().rposition(|&param| param == ArgType::Expr)?;
    (given >= 1 && given == last_expression && given + 1 >= signature.mandatory)
        .then_some((at, ", $_"))
}

impl Chip {
    fn new(id: ChipId, text: &str) -> Self {
        let written = Written::of(text);
        let parsed = match parse(&written.source) {
            Ok(expr) if continued_refs(&expr) > 0 => Ok(ChipExpr::Continues(expr)),
            Ok(expr) => Ok(ChipExpr::Whole(expr)),
            Err(error) => Err(SyntaxError {
                span: written.span_in_text(&error.span),
                ..error
            }),
        };
        Chip {
            id,
            text: text.to_string(),
            written,
            parsed,
            enabled: true,
            negated: false,
        }
    }

    pub fn id(&self) -> ChipId {
        self.id
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn parsed(&self) -> Result<&ChipExpr, &SyntaxError> {
        self.parsed.as_ref()
    }

    /// The span of the chip's text holding the set operator it opens with.
    pub fn leading_operator(&self) -> Option<Range<usize>> {
        self.written.leading_operator.clone()
    }

    /// The chip's text with its shorthand written out.
    pub fn expanded(&self) -> &str {
        &self.written.source
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn negated(&self) -> bool {
        self.negated
    }

    /// Whether the chip names a set of its own, which the stack unions with
    /// its other sources, rather than refining the set before it.
    pub fn is_source(&self) -> bool {
        !self.negated && matches!(self.parsed, Ok(ChipExpr::Whole(_)))
    }

    /// The expression the chip, parsed as `parsed`, stands for in its stack,
    /// reading the set before it as `$_`: a source joins that set, unless it
    /// is the `first` enabled chip, which names the set alone.
    fn step(&self, parsed: &ChipExpr, first: bool) -> Expr {
        let expr = parsed.expr();
        let continued = || Expr {
            kind: ExprKind::Literal(Word {
                text: CONTINUED_REF.to_string(),
                span: expr.span.clone(),
            }),
            span: expr.span.clone(),
        };
        let applied = |op: BinaryOp| Expr {
            kind: ExprKind::Binary {
                op,
                first: Box::new(continued()),
                rest: vec![expr.clone()],
            },
            span: expr.span.clone(),
        };
        match parsed {
            _ if self.negated => applied(BinaryOp::Except),
            ChipExpr::Whole(_) if !first => applied(BinaryOp::Plus),
            ChipExpr::Whole(expr) | ChipExpr::Continues(expr) => expr.clone(),
        }
    }

    /// What the chip's stack fingerprints its value by, after the chips
    /// before it fingerprinted as `before`.
    fn fingerprint(&self, before: u64, first: bool) -> u64 {
        let mut hasher = DefaultHasher::new();
        (before, first, self.negated, &self.written.source).hash(&mut hasher);
        hasher.finish()
    }

    fn eval_error_in_text(&self, error: EvalError) -> EvalError {
        EvalError {
            span: self.written.span_in_text(&error.span),
            ..error
        }
    }
}

/// How many times `expr` reads `$_` where no `let` of its own rebinds `_`.
fn continued_refs(expr: &Expr) -> usize {
    match &expr.kind {
        ExprKind::Literal(word) => usize::from(word.text == CONTINUED_REF),
        ExprKind::Binary { first, rest, .. } => {
            continued_refs(first) + rest.iter().map(continued_refs).sum::<usize>()
        }
        ExprKind::Let { name, value, body } => {
            let body = match name.text == CONTINUED {
                true => 0,
                false => continued_refs(body),
            };
            continued_refs(value) + body
        }
        ExprKind::Set(words) => words.iter().filter(|w| w.text == CONTINUED_REF).count(),
        ExprKind::Call { args, .. } => args
            .iter()
            .map(|arg| match arg {
                Arg::Expr(expr) => continued_refs(expr),
                Arg::Word(_) | Arg::Int(_) => 0,
            })
            .sum(),
    }
}

/// `expr` with `with` in place of each `$_` it reads, `expr` ending the
/// expression it stands in where `tail`; `None` where `$_` stands as a word of
/// a `set`, which holds no expression, or where `with` is a `let` and the `$_`
/// is followed by more operands, which the `let` would take into its body.
fn substitute(expr: &Expr, with: &Expr, tail: bool) -> Option<Expr> {
    let kind = match &expr.kind {
        ExprKind::Literal(word) if word.text == CONTINUED_REF => {
            let swallows = !tail && matches!(with.kind, ExprKind::Let { .. });
            return (!swallows).then(|| with.clone());
        }
        ExprKind::Literal(_) => return Some(expr.clone()),
        ExprKind::Binary { op, first, rest } => ExprKind::Binary {
            op: *op,
            first: Box::new(substitute(first, with, rest.is_empty())?),
            rest: (rest.iter().enumerate())
                .map(|(at, operand)| substitute(operand, with, at + 1 == rest.len()))
                .collect::<Option<_>>()?,
        },
        ExprKind::Let { name, value, body } => ExprKind::Let {
            name: name.clone(),
            value: Box::new(substitute(value, with, true)?),
            body: match name.text == CONTINUED {
                true => body.clone(),
                false => Box::new(substitute(body, with, tail)?),
            },
        },
        ExprKind::Set(words) if words.iter().any(|w| w.text == CONTINUED_REF) => return None,
        ExprKind::Set(_) => return Some(expr.clone()),
        ExprKind::Call { function, args } => ExprKind::Call {
            function: *function,
            args: (args.iter())
                .map(|arg| match arg {
                    Arg::Expr(expr) => substitute(expr, with, true).map(Arg::Expr),
                    Arg::Word(_) | Arg::Int(_) => Some(arg.clone()),
                })
                .collect::<Option<_>>()?,
        },
    };
    Some(Expr {
        kind,
        span: expr.span.clone(),
    })
}

/// `body` reading `value` as `_`.
fn bound(value: Expr, body: Expr) -> Expr {
    Expr {
        span: value.span.clone(),
        kind: ExprKind::Let {
            name: Word {
                text: CONTINUED.to_string(),
                span: value.span.clone(),
            },
            value: Box::new(value),
            body: Box::new(body),
        },
    }
}

/// `step` reading `before` as `_`: written out in place of its `$_` where it
/// reads it once and the text reads back alike, and bound by a `let`
/// otherwise.
fn continued(before: Expr, step: &Expr) -> Expr {
    let inlined = (continued_refs(step) <= 1)
        .then(|| substitute(step, &before, true))
        .flatten();
    inlined.unwrap_or_else(|| bound(before, step.clone()))
}

/// `sources` joined by `+`; `None` where there are none.
fn union(sources: Vec<Expr>) -> Option<Expr> {
    let mut sources = sources.into_iter();
    let first = sources.next()?;
    let rest: Vec<Expr> = sources.collect();
    if rest.is_empty() {
        return Some(first);
    }
    let span = first.span.clone();
    Some(Expr {
        kind: ExprKind::Binary {
            op: BinaryOp::Plus,
            first: Box::new(first),
            rest,
        },
        span,
    })
}

/// `$_ + source`.
fn joined(source: Expr) -> Expr {
    let span = source.span.clone();
    let continued = Expr {
        kind: ExprKind::Literal(Word {
            text: CONTINUED_REF.to_string(),
            span: span.clone(),
        }),
        span: span.clone(),
    };
    Expr {
        kind: ExprKind::Binary {
            op: BinaryOp::Plus,
            first: Box::new(continued),
            rest: vec![source],
        },
        span,
    }
}

/// A query written as chips, its sources ahead of its refinements. Each chip
/// keeps its [`ChipId`] however the stack is edited around it.
#[derive(Clone, Default, Debug)]
pub struct Stack {
    /// Every chip here [`Chip::is_source`], and none of `refinements` is.
    sources: Vec<Chip>,
    refinements: Vec<Chip>,
    minted: u64,
}

/// The first enabled chip of a stack that does not parse.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Invalid {
    pub chip: ChipId,
    pub error: SyntaxError,
}

/// A stack's enabled chips as one expression.
#[derive(Clone, Debug)]
pub struct Compiled {
    pub expr: Expr,
    /// Whether no source is enabled, so the first refinement reads every node
    /// of the graph: no query of Bazel's names that.
    pub from_everything: bool,
}

impl Stack {
    /// The chips, the sources ahead of the refinements; a chip's position in
    /// the stack is its place in this order.
    pub fn chips(&self) -> impl DoubleEndedIterator<Item = &Chip> {
        self.sources.iter().chain(&self.refinements)
    }

    pub fn len(&self) -> usize {
        self.sources.len() + self.refinements.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn sources(&self) -> &[Chip] {
        &self.sources
    }

    pub fn refinements(&self) -> &[Chip] {
        &self.refinements
    }

    /// The chip at `at` among [`Self::chips`].
    pub fn get(&self, at: usize) -> Option<&Chip> {
        match at.checked_sub(self.sources.len()) {
            None => self.sources.get(at),
            Some(at) => self.refinements.get(at),
        }
    }

    pub fn chip(&self, id: ChipId) -> Option<&Chip> {
        self.chips().find(|chip| chip.id == id)
    }

    pub fn position(&self, id: ChipId) -> Option<usize> {
        self.chips().position(|chip| chip.id == id)
    }

    /// Adds a chip reading `text` at the end of its group.
    pub fn push(&mut self, text: &str) -> ChipId {
        let id = ChipId(self.minted);
        self.minted += 1;
        self.place(Chip::new(id, text));
        id
    }

    /// Puts `chip` at the end of its group.
    fn place(&mut self, chip: Chip) {
        match chip.is_source() {
            true => self.sources.push(chip),
            false => self.refinements.push(chip),
        }
    }

    /// The group holding the chip `id`, and where in it the chip stands.
    fn group_of(&mut self, id: ChipId) -> Option<(&mut Vec<Chip>, usize)> {
        if let Some(at) = self.sources.iter().position(|chip| chip.id == id) {
            return Some((&mut self.sources, at));
        }
        let at = self.refinements.iter().position(|chip| chip.id == id)?;
        Some((&mut self.refinements, at))
    }

    /// Runs `change` on the chip `id`, moving it to the end of the group it
    /// belongs to where the change took it out of its own.
    fn change(&mut self, id: ChipId, change: impl FnOnce(&mut Chip)) {
        let Some((group, at)) = self.group_of(id) else {
            return;
        };
        let was = group[at].is_source();
        change(&mut group[at]);
        if group[at].is_source() != was {
            let chip = group.remove(at);
            self.place(chip);
        }
    }

    /// Rewrites the chip `id` to read `text`, keeping whether it is enabled
    /// and negated.
    pub fn set_text(&mut self, id: ChipId, text: &str) {
        self.change(id, |chip| {
            *chip = Chip {
                enabled: chip.enabled,
                negated: chip.negated,
                ..Chip::new(id, text)
            };
        });
    }

    /// Takes the chip `id` off the stack, reporting where it stood.
    pub fn remove(&mut self, id: ChipId) -> Option<usize> {
        let at = self.position(id)?;
        let (group, within) = self.group_of(id)?;
        group.remove(within);
        Some(at)
    }

    pub fn toggle_enabled(&mut self, id: ChipId) {
        self.change(id, |chip| chip.enabled = !chip.enabled);
    }

    pub fn toggle_negated(&mut self, id: ChipId) {
        self.change(id, |chip| chip.negated = !chip.negated);
    }

    /// Moves the chip `id` `by` places within its group, held to the group;
    /// reports whether it moved.
    pub fn move_by(&mut self, id: ChipId, by: isize) -> bool {
        let Some((group, at)) = self.group_of(id) else {
            return false;
        };
        let to = at.saturating_add_signed(by).min(group.len() - 1);
        if to == at {
            return false;
        }
        let chip = group.remove(at);
        group.insert(to, chip);
        true
    }

    /// Moves the chip `id` to stand just ahead of the chip `before` of its
    /// group, or at the group's end where `before` is `None`; reports whether
    /// it moved. A `before` of the other group moves nothing, a chip standing
    /// only in its own group.
    pub fn move_before(&mut self, id: ChipId, before: Option<ChipId>) -> bool {
        let Some((group, at)) = self.group_of(id) else {
            return false;
        };
        let to = match before {
            None => group.len() - 1,
            Some(before) => match group.iter().position(|chip| chip.id == before) {
                None => return false,
                Some(to) if to > at => to - 1,
                Some(to) => to,
            },
        };
        if to == at {
            return false;
        }
        let chip = group.remove(at);
        group.insert(to, chip);
        true
    }

    /// The union of the enabled sources, and the expression each enabled
    /// refinement stands for, in order.
    ///
    /// A `let` takes the rest of the expression into its body, so sources
    /// written as one stand last in the union, where nothing follows them;
    /// where several do, the union is built a source at a time, binding what
    /// would be swallowed.
    fn steps(&self) -> Result<(Option<Expr>, Vec<Expr>), Invalid> {
        fn parsed(chip: &Chip) -> Result<&ChipExpr, Invalid> {
            chip.parsed.as_ref().map_err(|error| Invalid {
                chip: chip.id,
                error: error.clone(),
            })
        }
        let mut sources: Vec<Expr> = Vec::with_capacity(self.sources.len());
        for chip in self.sources.iter().filter(|chip| chip.enabled) {
            sources.push(parsed(chip)?.expr().clone());
        }
        let mut refinements = Vec::with_capacity(self.refinements.len());
        for chip in self.refinements.iter().filter(|chip| chip.enabled) {
            let first = sources.is_empty() && refinements.is_empty();
            refinements.push(chip.step(parsed(chip)?, first));
        }
        sources.sort_by_key(|source| matches!(source.kind, ExprKind::Let { .. }));
        let lets = (sources.iter()).filter(|source| matches!(source.kind, ExprKind::Let { .. }));
        let start = match lets.count() {
            0 | 1 => union(sources),
            _ => {
                let mut sources = sources.into_iter();
                let first = sources.next();
                first.map(|first| {
                    sources.fold(first, |before, source| bound(before, joined(source)))
                })
            }
        };
        Ok((start, refinements))
    }

    /// The enabled chips as one expression; `None` where no chip is enabled.
    pub fn compile(&self) -> Result<Option<Compiled>, Invalid> {
        let (start, refinements) = self.steps()?;
        let from_everything = start.is_none();
        let steps = start.into_iter().chain(refinements).collect::<Vec<_>>();
        let Some((last, before)) = steps.split_last() else {
            return Ok(None);
        };
        let chained =
            (before.iter().rev()).fold(last.clone(), |body, value| bound(value.clone(), body));
        Ok(Some(Compiled {
            expr: chained,
            from_everything,
        }))
    }

    /// The stack as an expression one of Bazel's query commands reads alike,
    /// or why there is none. Each chip's `$_` is written out as the chips
    /// before it where the chip reads it once, so that aquery's filters can
    /// stand on the chain of calls it filters by, and is bound by a `let`
    /// otherwise. The query is written for `preferred` where that command
    /// reads every function the stack calls, and for the first of query,
    /// cquery and aquery that does otherwise.
    pub fn bazel_text(&self, preferred: Command) -> Result<BazelQuery, Unportable> {
        let (start, refinements) = self.steps().map_err(|_| Unportable::Invalid)?;
        let Some(start) = start else {
            return match refinements.is_empty() {
                true => Err(Unportable::Empty),
                false => Err(Unportable::Extension(Extension::FromEverything)),
            };
        };
        let expr = (refinements.iter()).fold(start, continued);
        let mut calls = Calls::default();
        calls.gather(&expr);
        if let Some(&extension) = (calls.functions.iter()).find(|f| f.is_extension()) {
            return Err(Unportable::Extension(Extension::Function(extension)));
        }
        if calls.any_exec {
            return Err(Unportable::Extension(Extension::AnyExec));
        }
        let reads_all = |command: Command| calls.functions.iter().all(|&f| command.reads(f));
        let commands = [preferred, Command::Query, Command::Cquery, Command::Aquery];
        let Some(command) = commands.into_iter().find(|&command| reads_all(command)) else {
            return Err(Unportable::Extension(Extension::NoCommand));
        };
        if !action_filters_on_chain(&expr) {
            return Err(Unportable::Extension(Extension::ActionFilterOffChain));
        }
        Ok(BazelQuery {
            command,
            text: expr.to_string(),
        })
    }

    /// What each chip names over `universe`, reusing from `prefixes` the
    /// value of every run of chips evaluated before, so that an edit to one
    /// chip evaluates it and the chips above it alone.
    pub fn evaluate(&self, universe: &Universe, prefixes: &Prefixes) -> Evaluation {
        let mut chips = Vec::with_capacity(self.len());
        let mut before: Option<Evaluated> = None;
        let mut fingerprint = 0;
        let mut broken = false;
        for chip in self.chips() {
            let outcome = match (&chip.parsed, chip.enabled) {
                (_, false) => Outcome::Disabled,
                _ if broken => Outcome::Blocked,
                (Err(error), true) => {
                    broken = true;
                    Outcome::Invalid(error.clone())
                }
                (Ok(parsed), true) => {
                    let first = before.is_none();
                    let step = chip.step(parsed, first);
                    fingerprint = chip.fingerprint(fingerprint, first);
                    let named = prefixes.get_or_insert(fingerprint, || {
                        let (from, denied) = match before.as_ref() {
                            None => (
                                Named::new(universe.members().clone()),
                                Arc::new(universe.empty()),
                            ),
                            Some(before) => (before.named.clone(), Arc::clone(&before.denied)),
                        };
                        let (named, denied) = match subtrahends(&step) {
                            None => {
                                let read = Arc::clone(from.set());
                                let named = universe.evaluate_with(&step, CONTINUED, from)?;
                                let denied = readmitted(universe, denied, &read, named.set());
                                (named, denied)
                            }
                            Some(operands) => {
                                let mut taken = universe.empty();
                                for operand in operands {
                                    let named =
                                        universe.evaluate_with(operand, CONTINUED, from.clone())?;
                                    taken.union_with(&universe.with_held(named.set()));
                                }
                                let mut kept = from.set().as_ref().clone();
                                kept.difference_with(&taken);
                                taken.union_with(&denied);
                                (Named::new(kept), Arc::new(taken))
                            }
                        };
                        Ok(Evaluated {
                            counts: universe.counts(named.set()),
                            named,
                            denied,
                        })
                    });
                    match named {
                        Ok(named) => {
                            let outcome = Outcome::Named {
                                counts: named.counts,
                                before: before.as_ref().map(|b| b.counts),
                            };
                            before = Some(named);
                            outcome
                        }
                        Err(error) => {
                            broken = true;
                            Outcome::Failed(chip.eval_error_in_text(error))
                        }
                    }
                }
            };
            chips.push((chip.id, outcome));
        }
        Evaluation {
            chips,
            value: (before.filter(|_| !broken)).map(|before| Value {
                set: Arc::clone(before.named.set()),
                denied: before.denied,
            }),
        }
    }
}

/// The operands `step` takes out of the set before it, where it is `$_`
/// followed by a run of `except` and `-` operands.
fn subtrahends(step: &Expr) -> Option<Vec<&Expr>> {
    let ExprKind::Binary {
        op: BinaryOp::Except | BinaryOp::Minus,
        first,
        rest,
    } = &step.kind
    else {
        return None;
    };
    let mut operands = match &first.kind {
        ExprKind::Literal(word) if word.text == CONTINUED_REF => Vec::new(),
        _ => subtrahends(first)?,
    };
    operands.extend(rest);
    Some(operands)
}

/// `denied` without what a step reading `read` and naming `named` adds.
fn readmitted(
    universe: &Universe,
    denied: Arc<FixedBitSet>,
    read: &FixedBitSet,
    named: &FixedBitSet,
) -> Arc<FixedBitSet> {
    let mut added = named.clone();
    added.difference_with(read);
    if denied.is_clear() || added.is_clear() {
        return denied;
    }
    let mut readmitted = denied.as_ref().clone();
    readmitted.difference_with(&universe.with_held(&added));
    Arc::new(readmitted)
}

/// The functions an expression calls, and whether it names [`ANY_EXEC`].
#[derive(Default)]
struct Calls {
    functions: Vec<Function>,
    any_exec: bool,
}

impl Calls {
    fn gather(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Literal(_) | ExprKind::Set(_) => {}
            ExprKind::Binary { first, rest, .. } => {
                self.gather(first);
                rest.iter().for_each(|operand| self.gather(operand));
            }
            ExprKind::Let { value, body, .. } => {
                self.gather(value);
                self.gather(body);
            }
            ExprKind::Call { function, args } => {
                if !self.functions.contains(function) {
                    self.functions.push(*function);
                }
                if *function == Function::Config {
                    self.any_exec |= (args.iter())
                        .any(|arg| matches!(arg, Arg::Word(word) if word.text == ANY_EXEC));
                }
                for arg in args {
                    if let Arg::Expr(expr) = arg {
                        self.gather(expr);
                    }
                }
            }
        }
    }
}

/// Whether every aquery filter call in `expr` stands where Bazel's aquery
/// filters by it (`AqueryProcessor.buildActionFilters`): on the chain of calls
/// from the outermost expression through each call's first argument that is a
/// call, above every call of another function on it.
fn action_filters_on_chain(expr: &Expr) -> bool {
    let mut chain: Vec<&Expr> = Vec::new();
    let mut at = Some(expr);
    let mut below_other = false;
    while let Some(call) = at {
        let ExprKind::Call { function, args } = &call.kind else {
            break;
        };
        if ActionFilter::of(*function).is_some() {
            if below_other {
                return false;
            }
            chain.push(call);
        } else {
            below_other = true;
        }
        at = args.iter().find_map(|arg| match arg {
            Arg::Expr(expr) if matches!(expr.kind, ExprKind::Call { .. }) => Some(expr),
            _ => None,
        });
    }
    every_action_filter(expr, &mut |call| {
        chain.iter().any(|on| std::ptr::eq(*on, call))
    })
}

/// Whether `holds` holds of every aquery filter call in `expr`.
fn every_action_filter(expr: &Expr, holds: &mut impl FnMut(&Expr) -> bool) -> bool {
    match &expr.kind {
        ExprKind::Literal(_) | ExprKind::Set(_) => true,
        ExprKind::Binary { first, rest, .. } => {
            every_action_filter(first, holds)
                && rest
                    .iter()
                    .all(|operand| every_action_filter(operand, holds))
        }
        ExprKind::Let { value, body, .. } => {
            every_action_filter(value, holds) && every_action_filter(body, holds)
        }
        ExprKind::Call { function, args } => {
            (ActionFilter::of(*function).is_none() || holds(expr))
                && args.iter().all(|arg| match arg {
                    Arg::Expr(expr) => every_action_filter(expr, holds),
                    Arg::Word(_) | Arg::Int(_) => true,
                })
        }
    }
}

/// A stack written as a Bazel query, and the command reading it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BazelQuery {
    pub command: Command,
    pub text: String,
}

/// The Bazel query command a query is written for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    Query,
    Cquery,
    Aquery,
}

impl Command {
    /// Whether Bazel 9.2.0's command calls `function`: every command reads
    /// `QueryEnvironment.DEFAULT_QUERY_FUNCTIONS` but those
    /// `PostAnalysisQueryEnvironment` rejects (`buildfiles`, `loadfiles`,
    /// `siblings`); query adds `SkyQueryEnvironment`'s `allrdeps` and
    /// `rbuildfiles`, read under `--universe_scope`; cquery adds
    /// `CQUERY_FUNCTIONS` and aquery `AQUERY_FUNCTIONS`.
    pub fn reads(self, function: Function) -> bool {
        match function {
            Function::AllRdeps
            | Function::RBuildFiles
            | Function::BuildFiles
            | Function::LoadFiles
            | Function::Siblings => self == Command::Query,
            Function::Config => self == Command::Cquery,
            Function::Mnemonic | Function::Inputs | Function::Outputs => self == Command::Aquery,
            Function::AllPaths
            | Function::Attr
            | Function::Deps
            | Function::Executables
            | Function::Filter
            | Function::Kind
            | Function::Labels
            | Function::Rdeps
            | Function::SamePkgDirectRdeps
            | Function::Some
            | Function::SomePath
            | Function::Tests
            | Function::Visible => true,
            Function::RepoCategory
            | Function::RuleCategory
            | Function::RunfilesPlumbing
            | Function::ToolConfigured => false,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Command::Query => "query",
            Command::Cquery => "cquery",
            Command::Aquery => "aquery",
        }
    }
}

/// Why a stack has no Bazel query reading it alike.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unportable {
    Empty,
    Invalid,
    Extension(Extension),
}

/// What a stack uses of bviz's query language that Bazel 9.2.0's does not
/// read alike.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Extension {
    /// No source is enabled, so the first refinement reads every node of the
    /// graph.
    FromEverything,
    /// The stack calls one of bviz's [`Function::EXTENSIONS`].
    Function(Function),
    /// `config()` names [`ANY_EXEC`].
    AnyExec,
    /// An aquery filter stands off the chain of calls aquery filters by.
    ActionFilterOffChain,
    /// No one query command reads every function the stack calls.
    NoCommand,
}

impl fmt::Display for Unportable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unportable::Empty => f.write_str("no chip is enabled"),
            Unportable::Invalid => f.write_str("a chip does not parse"),
            Unportable::Extension(Extension::FromEverything) => {
                f.write_str("no source is enabled, so the first refinement reads every node")
            }
            Unportable::Extension(Extension::Function(function)) => {
                write!(
                    f,
                    "{}() is a bviz function, which no Bazel query command has",
                    function.name()
                )
            }
            Unportable::Extension(Extension::AnyExec) => {
                write!(f, "config() names '{ANY_EXEC}'")
            }
            Unportable::Extension(Extension::ActionFilterOffChain) => f.write_str(
                "an aquery filter stands where aquery does not filter: below another function, \
                 or off the chain of first arguments",
            ),
            Unportable::Extension(Extension::NoCommand) => {
                f.write_str("no one Bazel query command reads every function it calls")
            }
        }
    }
}

/// What one chip names, with its counts.
#[derive(Clone, Debug)]
struct Evaluated {
    named: Named,
    counts: Counts,
    /// [`Value::denied`] of the chips up to this one.
    denied: Arc<FixedBitSet>,
}

/// The values of the chip prefixes a stack has evaluated, by fingerprint,
/// shared by every evaluation of stacks over one universe.
#[derive(Default)]
pub struct Prefixes(Mutex<FxHashMap<u64, Result<Evaluated, EvalError>>>);

impl Prefixes {
    /// The value held for `fingerprint`, evaluated by `evaluate` where none is.
    /// The lock is let go while `evaluate` runs, so two evaluations of one
    /// prefix may race, each landing the same value.
    fn get_or_insert(
        &self,
        fingerprint: u64,
        evaluate: impl FnOnce() -> Result<Evaluated, EvalError>,
    ) -> Result<Evaluated, EvalError> {
        let lock = || {
            self.0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        };
        if let Some(held) = lock().get(&fingerprint) {
            return held.clone();
        }
        let evaluated = evaluate();
        let mut held = lock();
        if held.len() >= MAX_CACHED_PREFIXES {
            held.clear();
        }
        held.insert(fingerprint, evaluated.clone());
        evaluated
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

/// What one chip came to in an evaluation of its stack.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    Disabled,
    Invalid(SyntaxError),
    /// Its span points into the chip's text.
    Failed(EvalError),
    /// A chip below it is invalid or failed, leaving it nothing to read.
    Blocked,
    /// The set it names, counted, beside the count of the set it read.
    Named {
        counts: Counts,
        before: Option<Counts>,
    },
}

/// What a stack came to over one universe.
#[derive(Clone, Debug)]
pub struct Evaluation {
    /// Each chip, in the stack's order.
    pub chips: Vec<(ChipId, Outcome)>,
    /// `None` where no chip is enabled, or one fails.
    pub value: Option<Value>,
}

/// What a stack names, and what its scope is not to draw back.
#[derive(Clone, Debug)]
pub struct Value {
    pub set: Arc<FixedBitSet>,
    /// The nodes the stack's chips take out with `except`, which a scope over
    /// `set` draws only where `set` holds them. A refinement chip standing for
    /// `$_ except E1 … except En` (a negated chip, or one led by `except` or
    /// `-`, the two spellings of one operator) denies each `E` with every node
    /// it holds, the nodes it takes out of the set before it; an `except`
    /// within an argument or joined with any other operator in the chip's
    /// expression denies nothing. A later chip adding a node to the set before
    /// it lifts the denial of that node and every node it holds, as a set
    /// holding a node stands for what the node holds.
    pub denied: Arc<FixedBitSet>,
}

impl Evaluation {
    pub fn outcome(&self, id: ChipId) -> Option<&Outcome> {
        let found = self.chips.iter().find(|(chip, _)| *chip == id);
        found.map(|(_, outcome)| outcome)
    }
}

#[cfg(test)]
mod tests;
