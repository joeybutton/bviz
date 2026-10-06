//! The query functions keeping the part of a value whose targets match a
//! regular expression, each as Bazel's `RegexFilterExpression` of that name
//! keeps it.
//!
//! A pattern is matched with the `regex-automata` engine in its ASCII mode,
//! where `\w`, `\d`, `\s` and `\b` mean what they mean to `java.util.regex`,
//! and found anywhere in the string matched, as `Matcher.find` finds it. The
//! two engines part where `regex-syntax` has no counterpart: look-around and
//! backreferences fail to compile, naming the construct, and possessive
//! quantifiers, atomic groups and `\Q…\E` quoting read differently or not at
//! all. `.` steps over one byte where Java's steps over one character, which
//! only a label outside ASCII tells apart.

use std::sync::Mutex;

use fixedbitset::FixedBitSet;
use regex_automata::meta::Regex;
use regex_automata::util::syntax;
use regex_syntax::hir::Hir;
use rustc_hash::FxHashMap;

use super::ast::Function;
use super::declared::Attribute;
use super::universe::Universe;
use crate::capability::Support;
use crate::intern::Id;

/// The most patterns a [`Patterns`] cache keeps compiled. A query names few,
/// but a session of edits to one names many, so the cache empties when full.
const MAX_CACHED_PATTERNS: usize = 256;

/// The patterns a universe's queries have matched by, compiled, or why each
/// does not compile.
#[derive(Default)]
pub(super) struct Patterns {
    found: Cache,
}

type Cache = Mutex<FxHashMap<Box<str>, Result<Regex, String>>>;

impl Patterns {
    /// `pattern` compiled to be found anywhere in a string, or the reason it
    /// does not compile.
    fn found(&self, pattern: &str) -> Result<Regex, String> {
        cached(&self.found, pattern, compile)
    }
}

/// `pattern` as `compile` compiles it, held in `cache`.
fn cached(
    cache: &Cache,
    pattern: &str,
    compile: fn(&str) -> Result<Regex, String>,
) -> Result<Regex, String> {
    let mut cache = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(compiled) = cache.get(pattern) {
        return compiled.clone();
    }
    let compiled = compile(pattern);
    if cache.len() >= MAX_CACHED_PATTERNS {
        cache.clear();
    }
    cache.insert(pattern.into(), compiled.clone());
    compiled
}

/// `pattern`, read in ASCII mode over bytes, compiled, or the reason it does
/// not compile.
fn compile(pattern: &str) -> Result<Regex, String> {
    build(&parse(pattern)?)
}

fn parse(pattern: &str) -> Result<Hir, String> {
    let ascii = syntax::Config::new().unicode(false).utf8(false);
    syntax::parse_with(pattern, &ascii).map_err(|error| reason(&error.to_string()))
}

fn build(hir: &Hir) -> Result<Regex, String> {
    let bytes = Regex::config().utf8_empty(false);
    let built = Regex::builder().configure(bytes).build_from_hir(hir);
    built.map_err(|error| error.to_string())
}

impl Universe {
    /// `pattern` compiled for `function`, or the message Bazel's engine fails
    /// with for a pattern it cannot compile.
    pub(super) fn regex(&self, function: Function, pattern: &str) -> Result<Regex, String> {
        self.patterns.found(pattern).map_err(|why| {
            let name = function.name();
            format!("illegal '{name}' pattern regexp '{pattern}': {why}")
        })
    }

    /// The attribute `attr` reads by `name`, or why the graph cannot answer it.
    pub(super) fn attribute(&self, name: &str) -> Result<Attribute, String> {
        let unstated = |what: &str, support: Support| match support {
            Support::Unavailable { flag } => Err(format!(
                "attr() needs each rule's {what}, which the graph's inputs state only under {flag}"
            )),
            Support::Available | Support::Partial { .. } => Ok(()),
        };
        match Attribute::named(name) {
            Some(Attribute::Tags) => unstated("tags", self.tags).map(|()| Attribute::Tags),
            Some(Attribute::Testonly) => {
                unstated("testonly", self.testonly).map(|()| Attribute::Testonly)
            }
            Some(attribute) => Ok(attribute),
            None => Err(format!(
                "attr() needs each rule's '{name}' attribute, which bviz does not load into its \
                 graph"
            )),
        }
    }

    /// The members of `value` whose target's kind `regex` matches.
    pub(super) fn kind(&self, regex: &Regex, value: &FixedBitSet) -> FixedBitSet {
        self.retain(value, |at| {
            (self.declared.kind(at)).is_some_and(|kind| regex.is_match(kind.as_bytes()))
        })
    }

    /// The members of `value` whose target's label `regex` matches.
    pub(super) fn filter(&self, regex: &Regex, value: &FixedBitSet) -> FixedBitSet {
        self.retain(value, |at| {
            regex.is_match(self.labels.entries()[at].1.as_bytes())
        })
    }

    /// The members of `value` whose target is a rule carrying a value of
    /// `attribute` that `regex` matches.
    pub(super) fn attr(
        &self,
        attribute: Attribute,
        regex: &Regex,
        value: &FixedBitSet,
    ) -> FixedBitSet {
        self.retain(value, |at| {
            let label = &self.labels.entries()[at].1;
            let carried = self.declared.attribute(attribute, at, label);
            carried.is_some_and(|carried| regex.is_match(carried.as_bytes()))
        })
    }

    /// The members of `value` belonging to a target `keeps`, by its position
    /// among the labels, asked once for each target.
    fn retain(&self, value: &FixedBitSet, keeps: impl Fn(usize) -> bool) -> FixedBitSet {
        let mut verdicts: FxHashMap<usize, bool> = FxHashMap::default();
        let mut kept = self.empty();
        for id in value.ones() {
            let Some(at) = self.position(Id(id as u32)) else {
                continue;
            };
            if *verdicts.entry(at).or_insert_with(|| keeps(at)) {
                kept.insert(id);
            }
        }
        kept
    }
}

/// Why `regex-syntax` rejects a pattern, short of the excerpt of the pattern
/// its message `text` opens with.
fn reason(text: &str) -> String {
    let why = text.rsplit_once("error: ").map_or(text, |(_, why)| why);
    why.to_string()
}
