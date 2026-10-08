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
//! of a set operator, names its argument unfiltered. It gathers the patterns
//! of the calls on the chain into one filter, whatever their order. Here each
//! run of filter calls nested directly in one another filters as one
//! [`ActionFilters`], wherever it stands. A run continues through a variable
//! read, so `let v = inputs(p, X) in inputs(q, $v)` is one run, but not
//! through a `let` expression or any other function or operator.
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
use super::declared::{Attribute, FILE_KINDS, file_kind};
use super::universe::Universe;
use crate::capability::{CONSISTENT_LABELS_FLAG, Support};
use crate::exec_path::{ExecPath, LeftOutInputs};
use crate::facets::{RuleCategory, Sym};
use crate::graph::Kind;
use crate::intern::Id;
use crate::repo::{RepoCategory, RepoName};

/// The mnemonics of the actions that build a runfiles tree or lay down a
/// symlink, manifest or expanded template: the wiring between the actions that
/// do the build's work rather than work itself.
///
/// Bazel 9 names the runfiles-tree action `RunfilesTree` where Bazel 8 and
/// earlier name it `Middleman`, and bviz reads a dump from either, so both stand
/// here.
const RUNFILES_MNEMONICS: [&str; 10] = [
    "SymlinkTree",
    "SourceSymlinkManifest",
    "PackagingSourcesManifest",
    "RepoMappingManifest",
    "Symlink",
    "ExecutableSymlink",
    "FileWrite",
    "TemplateExpand",
    "RunfilesTree",
    "Middleman",
];

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

/// The aquery filter functions.
#[derive(Clone, Copy)]
pub(super) enum ActionFilter {
    Mnemonic,
    Inputs,
    Outputs,
}

impl ActionFilter {
    pub(super) fn of(function: Function) -> Option<ActionFilter> {
        match function {
            Function::Mnemonic => Some(ActionFilter::Mnemonic),
            Function::Inputs => Some(ActionFilter::Inputs),
            Function::Outputs => Some(ActionFilter::Outputs),
            _ => None,
        }
    }
}

/// The patterns of aquery filter calls, by function, as
/// `AqueryUtils.matchesAqueryFilters` applies them: an action passes where its
/// mnemonic matches every mnemonic pattern, one file it reads matches every
/// inputs pattern, and one file it writes matches every outputs pattern. A
/// function with no pattern passes every action.
#[derive(Default, Debug)]
pub(super) struct ActionFilters {
    mnemonic: Vec<Regex>,
    inputs: Vec<Regex>,
    outputs: Vec<Regex>,
}

impl ActionFilters {
    pub(super) fn add(&mut self, filter: ActionFilter, regex: Regex) {
        match filter {
            ActionFilter::Mnemonic => self.mnemonic.push(regex),
            ActionFilter::Inputs => self.inputs.push(regex),
            ActionFilter::Outputs => self.outputs.push(regex),
        }
    }

    /// Adds the patterns of `other`.
    pub(super) fn extend(&mut self, other: &ActionFilters) {
        self.mnemonic.extend_from_slice(&other.mnemonic);
        self.inputs.extend_from_slice(&other.inputs);
        self.outputs.extend_from_slice(&other.outputs);
    }
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

    /// `pattern` compiled to match a whole string for `function`, or the
    /// message Bazel's engine fails with for a pattern it cannot compile.
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

    /// The members of `value`, and the files its configured targets hold,
    /// whose kind `regex` matches: a file's own, as Bazel names a file target,
    /// and the target's of any other node.
    pub(super) fn kind(&self, regex: &Regex, value: &FixedBitSet) -> FixedBitSet {
        let matches = |kind: &str| regex.is_match(kind.as_bytes());
        let files = FILE_KINDS.map(|(file, name)| (file, matches(name)));
        let own = |id: Id| {
            let kind = self.index.kind(id)?;
            (files.iter()).find_map(|&(file, kept)| (file == kind).then_some(kept))
        };
        let mut considered = value.clone();
        for id in value.ones().map(|id| Id(id as u32)) {
            if self.index.kind(id) == Some(Kind::ConfiguredTarget) {
                let held = self
                    .index
                    .children(id)
                    .filter(|&child| own(child).is_some());
                considered.extend(held.map(|file| file.0 as usize));
            }
        }
        self.retain_unless_own(&considered, own, |at| {
            self.declared.kind(at).is_some_and(matches)
        })
    }

    /// The members of `value` whose target's repository is of a category whose
    /// [spelling](RepoCategory::spelling) `regex` matches, or why the graph
    /// cannot tell them for `function`.
    ///
    /// A repository named only by an apparent name that no module evidence
    /// places is of the unknown category, but may truly be a module's, an
    /// extension's or the builtin one; a pattern matching one of those and not
    /// unknown would drop it whatever it is, so it fails instead.
    pub(super) fn repo_category(
        &self,
        function: Function,
        regex: &Regex,
        value: &FixedBitSet,
    ) -> Result<FixedBitSet, String> {
        let matched: Vec<RepoCategory> = (RepoCategory::ALL.into_iter())
            .filter(|category| regex.is_match(category.spelling().as_bytes()))
            .collect();
        let hideable = [
            RepoCategory::Module,
            RepoCategory::ModuleExtension,
            RepoCategory::Builtin,
        ];
        let undecidable = !matched.contains(&RepoCategory::WorkspaceOrUnknown)
            && hideable.iter().any(|category| matched.contains(category));
        if let Some(apparent) = self.unplaced_apparent_repo().filter(|_| undecidable) {
            return Err(format!(
                "{}() requires {CONSISTENT_LABELS_FLAG} to tell where @{apparent} comes from",
                function.name()
            ));
        }
        Ok(self.retain(value, |at| {
            let repo = self.facets.repo(self.labels.entries()[at].0);
            repo.is_some_and(|repo| matched.contains(&self.facets.repo_category(repo)))
        }))
    }

    /// The first by name of the repositories named only by an apparent name
    /// that no module evidence places.
    fn unplaced_apparent_repo(&self) -> Option<&str> {
        let facets = &self.facets;
        (facets.repos())
            .filter(|&(repo, _)| facets.repo_category(repo) == RepoCategory::WorkspaceOrUnknown)
            .filter_map(|(repo, _)| match facets.repo_name(repo) {
                RepoName::Apparent(name) => Some(name),
                RepoName::Canonical(_) => None,
            })
            .min()
    }

    /// The members of `value` whose target is a rule of a class in a category
    /// whose [spelling](RuleCategory::spelling) `regex` matches. A file, being
    /// no rule, is in no category.
    pub(super) fn rule_category(&self, regex: &Regex, value: &FixedBitSet) -> FixedBitSet {
        let matched: Vec<RuleCategory> = (RuleCategory::ALL.into_iter())
            .filter(|category| regex.is_match(category.spelling().as_bytes()))
            .collect();
        let own = |id: Id| file_kind(self.index.kind(id)?).map(|_| false);
        self.retain_unless_own(value, own, |at| {
            let class = self.facets.rule_class(self.labels.entries()[at].0);
            let category = class.and_then(|class| self.facets.rule_category(class));
            category.is_some_and(|category| matched.contains(&category))
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

    /// The actions of `value`, and those its configured targets hold, that
    /// `filters` keeps, whether the graph holds a node for a file read or left
    /// it out.
    pub(super) fn filter_actions(
        &self,
        filters: &ActionFilters,
        value: &FixedBitSet,
    ) -> FixedBitSet {
        let ActionFilters {
            mnemonic,
            inputs,
            outputs,
        } = filters;
        let mut reads = (!inputs.is_empty()).then(|| {
            let left_out = LeftOutMatcher::new(self.facets.left_out_inputs());
            (PathMatcher::new(inputs), left_out)
        });
        let mut writes = (!outputs.is_empty()).then(|| PathMatcher::new(outputs));
        self.retain_actions(value, |action| {
            (mnemonic.is_empty() || self.mnemonic_matches(action, mnemonic))
                && reads.as_mut().is_none_or(|(matcher, left_out)| {
                    self.any_path_matches(self.index.dependencies(action), matcher)
                        || left_out.read_by(action, matcher)
                })
                && writes.as_mut().is_none_or(|matcher| {
                    self.any_path_matches(self.index.dependents(action), matcher)
                })
        })
    }

    /// The actions of `value`, and those its configured targets hold, whose
    /// mnemonic is one of [`RUNFILES_MNEMONICS`].
    pub(super) fn runfiles_plumbing(&self, value: &FixedBitSet) -> FixedBitSet {
        let strings = self.facets.strings();
        let mnemonics: Vec<Sym> = (RUNFILES_MNEMONICS.iter())
            .filter_map(|mnemonic| strings.get(mnemonic))
            .collect();
        self.retain_actions(value, |action| {
            (self.facets.mnemonic(action)).is_some_and(|mnemonic| mnemonics.contains(&mnemonic))
        })
    }

    /// Whether `action` has a mnemonic every one of `regexes` matches.
    fn mnemonic_matches(&self, action: Id, regexes: &[Regex]) -> bool {
        let mnemonic = self.facets.mnemonic(action);
        let mnemonic = mnemonic.map(|mnemonic| self.facets.strings().resolve(mnemonic));
        mnemonic.is_some_and(|mnemonic| {
            (regexes.iter()).all(|regex| regex.is_match(mnemonic.as_bytes()))
        })
    }

    /// Whether `matcher` matches the exec path of one of `files`.
    fn any_path_matches(&self, files: impl Iterator<Item = Id>, matcher: &mut PathMatcher) -> bool {
        (files.filter_map(|file| self.facets.exec_path(file))).any(|path| matcher.matches(path))
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
        self.retain_unless_own(value, |_| None, keeps)
    }

    /// [`Self::retain`], but a member `own` gives a verdict of its own is kept
    /// by that verdict.
    fn retain_unless_own(
        &self,
        value: &FixedBitSet,
        own: impl Fn(Id) -> Option<bool>,
        keeps: impl Fn(usize) -> bool,
    ) -> FixedBitSet {
        let mut verdicts: FxHashMap<usize, bool> = FxHashMap::default();
        let mut kept = self.empty();
        for id in value.ones() {
            let verdict = match own(Id(id as u32)) {
                Some(verdict) => verdict,
                None => match self.position(Id(id as u32)) {
                    Some(at) => *verdicts.entry(at).or_insert_with(|| keeps(at)),
                    None => false,
                },
            };
            if verdict {
                kept.insert(id);
            }
        }
        kept
    }
}

/// Matches exec paths against every one of some patterns, joining each into
/// one buffer.
struct PathMatcher<'a> {
    regexes: &'a [Regex],
    joined: Vec<u8>,
}

impl<'a> PathMatcher<'a> {
    fn new(regexes: &'a [Regex]) -> Self {
        PathMatcher {
            regexes,
            joined: Vec::new(),
        }
    }

    fn matches(&mut self, path: ExecPath) -> bool {
        path.write_into(&mut self.joined);
        (self.regexes.iter()).all(|regex| regex.is_match(&self.joined))
    }
}

/// Whether the inputs an action reads that the graph left out match a
/// [`PathMatcher`], each path and each nested set asked once.
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
