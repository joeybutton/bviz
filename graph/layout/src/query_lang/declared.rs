//! What each target of a graph declares that `kind` and `attr` match of it
//! beyond the attributes it states, read once per target and spelled as
//! Bazel's query engine spells it.

use fixedbitset::FixedBitSet;
use rustc_hash::FxHashMap;

use crate::facets::Facets;
use crate::graph::Kind;
use crate::intern::Id;
use crate::reach::Index;

/// The node kinds of a file, each with the kind Bazel names a file target of
/// it by.
pub(super) const FILE_KINDS: [(Kind, &str); 2] = [
    (Kind::Source, "source file"),
    (Kind::Artifact, "generated file"),
];

/// The kind Bazel names a file target of `kind` by; `None` for a node kind
/// that is no file.
pub(super) fn file_kind(kind: Kind) -> Option<&'static str> {
    (FILE_KINDS.iter())
        .find(|&&(file, _)| file == kind)
        .map(|&(_, name)| name)
}

/// The declarations of a graph's targets, by each target's position among the
/// labels.
pub(super) struct Declared {
    kinds: Column,
    rules: FixedBitSet,
}

impl Declared {
    /// Reads the declaration of each of `targets`, given in label order, from
    /// the kind of its node and from `facets`.
    pub(super) fn new(targets: &[(Id, String)], index: &Index, facets: &Facets) -> Self {
        let strings = facets.strings();
        let mut kind_column = ColumnBuilder::with_capacity(targets.len());
        let mut rules = FixedBitSet::with_capacity(targets.len());
        let mut kind = String::new();
        for (at, &(id, _)) in targets.iter().enumerate() {
            let node_kind = index.kind(id);
            rules.set(at, node_kind == Some(Kind::Target));
            kind.clear();
            match node_kind {
                Some(Kind::Target) => {
                    if let Some(class) = facets.rule_class(id) {
                        kind.push_str(strings.resolve(class.sym()));
                        kind.push(' ');
                    }
                    kind.push_str("rule");
                }
                Some(Kind::TargetGroup) => kind.push_str("package group"),
                Some(node_kind) => {
                    if let Some(name) = file_kind(node_kind) {
                        kind.push_str(name);
                    }
                }
                None => {}
            }
            kind_column.push((!kind.is_empty()).then_some(kind.as_str()));
        }
        Declared {
            kinds: kind_column.column,
            rules,
        }
    }

    /// The kind Bazel names the target at `at` by: `<rule class> rule`,
    /// `source file`, `generated file` or `package group`.
    pub(super) fn kind(&self, at: usize) -> Option<&str> {
        self.kinds.get(at)
    }

    /// The `name` attribute of the rule at `at`, the part of its `label`
    /// after the colon; `None` for a target that is no rule.
    pub(super) fn name<'a>(&self, at: usize, label: &'a str) -> Option<&'a str> {
        if !self.rules.contains(at) {
            return None;
        }
        label.split_once(':').map(|(_, name)| name)
    }
}

/// One string or none for each target, each distinct string held once, as
/// most targets share their kind and their tags with many others.
struct Column {
    strings: Vec<Box<str>>,
    /// The index into `strings` of each target's string, [`ABSENT`] for none.
    at: Vec<u32>,
}

const ABSENT: u32 = u32::MAX;

impl Column {
    fn get(&self, at: usize) -> Option<&str> {
        let string = *self.at.get(at)?;
        (string != ABSENT).then(|| &*self.strings[string as usize])
    }
}

struct ColumnBuilder {
    column: Column,
    index: FxHashMap<Box<str>, u32>,
}

impl ColumnBuilder {
    fn with_capacity(targets: usize) -> Self {
        ColumnBuilder {
            column: Column {
                strings: Vec::new(),
                at: Vec::with_capacity(targets),
            },
            index: FxHashMap::default(),
        }
    }

    /// Appends the next target's string.
    fn push(&mut self, string: Option<&str>) {
        let Some(string) = string else {
            self.column.at.push(ABSENT);
            return;
        };
        let at = match self.index.get(string) {
            Some(&at) => at,
            None => {
                let at = u32::try_from(self.column.strings.len()).expect("over u32::MAX strings");
                self.column.strings.push(string.into());
                self.index.insert(string.into(), at);
                at
            }
        };
        self.column.at.push(at);
    }
}
