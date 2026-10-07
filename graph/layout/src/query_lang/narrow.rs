//! The query functions keeping the part of a value that matches a regular
//! expression. `kind`, `filter` and `attr` keep the members whose target
//! matches, as Bazel's `RegexFilterExpression` keeps them, finding the
//! pattern anywhere in the string as `Matcher.find` does. aquery's
//! `mnemonic`, `inputs` and `outputs` keep the actions matching, as
//! `AqueryActionFilter` keeps them, matching the whole string as
//! `Matcher.matches` does.
//!
//! Bazel's aquery (`AqueryProcessor.buildActionFilters`) filters only by the
//! calls on the chain that starts at the outermost expression and follows
//! each call's first argument that is itself a call, and rejects such a call
//! below a call of another function. A call anywhere else, such as an operand
//! of a set operator, names its argument unfiltered. Here each call filters
//! wherever it stands.
//!
//! A pattern is matched with the `regex-automata` engine in its ASCII mode,
//! where `\w`, `\d`, `\s` and `\b` mean what they mean to `java.util.regex`.
//! The two engines part where `regex-syntax` has no counterpart: look-around and
//! backreferences fail to compile, naming the construct, and possessive
//! quantifiers, atomic groups and `\Q…\E` quoting read differently or not at
//! all. `.` steps over one byte where Java's steps over one character, which
//! only a label outside ASCII tells apart.

use std::sync::Mutex;

use fixedbitset::FixedBitSet;
use regex_automata::meta::Regex;
use regex_automata::util::syntax;
use regex_syntax::hir::{Hir, Look};
use rustc_hash::FxHashMap;

use super::ast::Function;
use super::declared::Attribute;
use super::universe::Universe;
use crate::capability::Support;
use crate::exec_path::{ExecPath, LeftOutInputs};
use crate::graph::Kind;
use crate::intern::Id;

/// The most patterns a [`Patterns`] cache keeps compiled. A query names few,
/// but a session of edits to one names many, so the cache empties when full.
const MAX_CACHED_PATTERNS: usize = 256;

/// The patterns a universe's queries have matched by, compiled, or why each
/// does not compile.
#[derive(Default)]
pub(super) struct Patterns {
    found: Cache,
    whole: Cache,
}

type Cache = Mutex<FxHashMap<Box<str>, Result<Regex, String>>>;

impl Patterns {
    /// `pattern` compiled to be found anywhere in a string, or the reason it
    /// does not compile.
    fn found(&self, pattern: &str) -> Result<Regex, String> {
        cached(&self.found, pattern, compile)
    }

    /// `pattern` compiled to match only a whole string, or the reason it does
    /// not compile.
    fn whole(&self, pattern: &str) -> Result<Regex, String> {
        cached(&self.whole, pattern, compile_whole)
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

/// `pattern` compiled as [`compile`] compiles it, held to start at the start
/// of a string and end at its end. The pattern is read alone first, so that
/// its flags and groups stay its own.
fn compile_whole(pattern: &str) -> Result<Regex, String> {
    let whole = [
        Hir::look(Look::Start),
        parse(pattern)?,
        Hir::look(Look::End),
    ];
    build(&Hir::concat(whole.into()))
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

    /// `pattern` compiled to match a whole string for the aquery filter
    /// `function`, or the message Bazel's engine fails with for a pattern it
    /// cannot compile.
    pub(super) fn whole_regex(&self, function: Function, pattern: &str) -> Result<Regex, String> {
        self.patterns.whole(pattern).map_err(|why| {
            let name = function.name();
            format!("Illegal '{name}' pattern regexp '{pattern}': {why}")
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

    /// The actions of `value`, and those its configured targets hold, whose
    /// mnemonic `regex` matches.
    pub(super) fn mnemonic(&self, regex: &Regex, value: &FixedBitSet) -> FixedBitSet {
        self.retain_actions(value, |action| {
            let mnemonic = self.facets.mnemonic(action);
            let mnemonic = mnemonic.map(|mnemonic| self.facets.strings().resolve(mnemonic));
            mnemonic.is_some_and(|mnemonic| regex.is_match(mnemonic.as_bytes()))
        })
    }

    /// The actions of `value`, and those its configured targets hold, reading
    /// a file whose exec path `regex` matches, whether the graph holds a node
    /// for the file or left it out.
    pub(super) fn inputs(&self, regex: &Regex, value: &FixedBitSet) -> FixedBitSet {
        let mut paths = PathMatcher::new(regex);
        let mut left_out = LeftOutMatcher::new(self.facets.left_out_inputs());
        self.retain_actions(value, |action| {
            let files = self.index.dependencies(action);
            (files.filter_map(|file| self.facets.exec_path(file))).any(|path| paths.matches(path))
                || left_out.read_by(action, &mut paths)
        })
    }

    /// The actions of `value`, and those its configured targets hold, writing
    /// a file whose exec path `regex` matches.
    pub(super) fn outputs(&self, regex: &Regex, value: &FixedBitSet) -> FixedBitSet {
        let mut paths = PathMatcher::new(regex);
        self.retain_actions(value, |action| {
            let files = self.index.dependents(action);
            (files.filter_map(|file| self.facets.exec_path(file))).any(|path| paths.matches(path))
        })
    }

    /// Why `function` cannot filter this graph's actions, if it cannot: the
    /// graph holds none, or its inputs left out the files they read and write.
    pub(super) fn filters_actions(&self, function: Function) -> Result<(), String> {
        let name = function.name();
        if !self.holds_actions {
            return Err(format!(
                "{name}() needs the actions of an aquery, which the graph's inputs do not include"
            ));
        }
        match (function, self.artifacts) {
            (Function::Inputs | Function::Outputs, Support::Unavailable { flag }) => Err(format!(
                "{name}() needs the files each action reads and writes, which the graph's \
                     inputs state only under {flag}"
            )),
            _ => Ok(()),
        }
    }

    /// The actions among `value` and among what its configured targets hold
    /// that `keeps`.
    fn retain_actions(
        &self,
        value: &FixedBitSet,
        mut keeps: impl FnMut(Id) -> bool,
    ) -> FixedBitSet {
        let mut kept = self.empty();
        let mut consider = |id: Id| {
            if self.index.kind(id) == Some(Kind::Action) && keeps(id) {
                kept.insert(id.0 as usize);
            }
        };
        for id in value.ones().map(|id| Id(id as u32)) {
            match self.index.kind(id) {
                Some(Kind::ConfiguredTarget) => self.index.children(id).for_each(&mut consider),
                _ => consider(id),
            }
        }
        kept
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

/// Matches exec paths against one pattern, joining each into one buffer.
struct PathMatcher<'a> {
    regex: &'a Regex,
    joined: Vec<u8>,
}

impl<'a> PathMatcher<'a> {
    fn new(regex: &'a Regex) -> Self {
        PathMatcher {
            regex,
            joined: Vec::new(),
        }
    }

    fn matches(&mut self, path: ExecPath) -> bool {
        path.write_into(&mut self.joined);
        self.regex.is_match(&self.joined)
    }
}

/// Whether the inputs an action reads that the graph left out match a
/// pattern, each path and each nested set asked once.
struct LeftOutMatcher<'a> {
    inputs: &'a LeftOutInputs,
    paths: Vec<Option<bool>>,
    sets: Vec<Option<bool>>,
    open: Vec<(u32, bool)>,
}

impl<'a> LeftOutMatcher<'a> {
    fn new(inputs: &'a LeftOutInputs) -> Self {
        LeftOutMatcher {
            inputs,
            paths: vec![None; inputs.paths()],
            sets: vec![None; inputs.sets()],
            open: Vec::new(),
        }
    }

    /// Whether `action` reads a left-out file whose path `matcher` matches.
    fn read_by(&mut self, action: Id, matcher: &mut PathMatcher) -> bool {
        let roots = self.inputs.read_by(action);
        roots.iter().any(|&set| self.holds(set, matcher))
    }

    /// Whether `set` holds such a file, directly or in a set it nests. Nested
    /// sets are settled before the sets nesting them.
    fn holds(&mut self, set: u32, matcher: &mut PathMatcher) -> bool {
        self.open.push((set, false));
        while let Some((set, closing)) = self.open.pop() {
            let at = set as usize;
            if closing {
                let inputs = self.inputs;
                let nested = inputs.nested(set).iter();
                let holds = (nested.copied())
                    .any(|nested| self.sets[nested as usize] == Some(true))
                    || (inputs.direct(set).iter()).any(|&path| {
                        *self.paths[path as usize]
                            .get_or_insert_with(|| matcher.matches(inputs.path(path)))
                    });
                self.sets[at] = Some(holds);
                continue;
            }
            if self.sets[at].is_some() {
                continue;
            }
            // Settled for now, so that a set reached again before it closes is
            // not opened twice.
            self.sets[at] = Some(false);
            self.open.push((set, true));
            let nested = self.inputs.nested(set).iter();
            let unsettled = nested.filter(|&&nested| self.sets[nested as usize].is_none());
            self.open.extend(unsettled.map(|&nested| (nested, false)));
        }
        self.sets[set as usize] == Some(true)
    }
}

/// Why `regex-syntax` rejects a pattern, short of the excerpt of the pattern
/// its message `text` opens with.
fn reason(text: &str) -> String {
    let why = text.rsplit_once("error: ").map_or(text, |(_, why)| why);
    why.to_string()
}
