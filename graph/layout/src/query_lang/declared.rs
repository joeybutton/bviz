//! What each target of a graph declares that `kind` and `attr` match, read
//! once per target and spelled as Bazel's query engine spells it.

use fixedbitset::FixedBitSet;
use rustc_hash::FxHashMap;

use crate::facets::{Facets, Tri};
use crate::graph::Kind;
use crate::intern::Id;
use crate::reach::Index;

/// An attribute `attr` reads, of the few bviz loads into its graph.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Attribute {
    Name,
    Tags,
    Testonly,
}

impl Attribute {
    pub(super) fn named(name: &str) -> Option<Self> {
        match name {
            "name" => Some(Attribute::Name),
            "tags" => Some(Attribute::Tags),
            "testonly" => Some(Attribute::Testonly),
            _ => None,
        }
    }
}

/// The declarations of a graph's targets, by each target's position among the
/// labels.
pub(super) struct Declared {
    kinds: Column,
    /// Each rule's tags as Bazel prints a list attribute's value, `[a, b]`,
    /// but for a tag a rule repeats, which the tags facet holds once and so
    /// prints once.
    tags: Column,
    testonly: Vec<Tri>,
    rules: FixedBitSet,
}

impl Declared {
    /// Reads the declaration of each of `targets`, given in label order, from
    /// the kind of its node and from `facets`.
    pub(super) fn new(targets: &[(Id, String)], index: &Index, facets: &Facets) -> Self {
        let strings = facets.strings();
        let mut kind_column = ColumnBuilder::with_capacity(targets.len());
        let mut tags = ColumnBuilder::with_capacity(targets.len());
        let mut testonly: Vec<Tri> = Vec::with_capacity(targets.len());
        let mut rules = FixedBitSet::with_capacity(targets.len());
        let mut kind = String::new();
        let mut printed = String::new();
        for (at, &(id, _)) in targets.iter().enumerate() {
            let node_kind = index.kind(id);
            let rule = node_kind == Some(Kind::Target);
            kind.clear();
            match node_kind {
                Some(Kind::Target) => {
                    if let Some(class) = facets.rule_class(id) {
                        kind.push_str(strings.resolve(class.sym()));
                        kind.push(' ');
                    }
                    kind.push_str("rule");
                }
                Some(Kind::Source) => kind.push_str("source file"),
                Some(Kind::Artifact) => kind.push_str("generated file"),
                Some(Kind::TargetGroup) => kind.push_str("package group"),
                _ => {}
            }
            if rule {
                printed.clear();
                printed.push('[');
                for (index, &tag) in facets.tags().values(id).iter().enumerate() {
                    if index > 0 {
                        printed.push_str(", ");
                    }
                    printed.push_str(strings.resolve(tag));
                }
                printed.push(']');
                rules.insert(at);
            }
            kind_column.push((!kind.is_empty()).then_some(kind.as_str()));
            tags.push(rule.then_some(printed.as_str()));
            testonly.push(if rule {
                facets.testonly(id)
            } else {
                Tri::Unknown
            });
        }
        Declared {
            kinds: kind_column.column,
            tags: tags.column,
            testonly,
            rules,
        }
    }

    /// The kind Bazel names the target at `at` by: `<rule class> rule`,
    /// `source file`, `generated file` or `package group`.
    pub(super) fn kind(&self, at: usize) -> Option<&str> {
        self.kinds.get(at)
    }

    /// The value of `attribute` the rule at `at` carries, as Bazel's `attr`
    /// prints it, `None` for a target that is no rule or a value the graph
    /// does not know. The target's own name is the part of its `label`
    /// after the colon.
    pub(super) fn attribute<'a>(
        &'a self,
        attribute: Attribute,
        at: usize,
        label: &'a str,
    ) -> Option<&'a str> {
        if !self.rules.contains(at) {
            return None;
        }
        match attribute {
            Attribute::Name => label.split_once(':').map(|(_, name)| name),
            Attribute::Tags => self.tags.get(at),
            Attribute::Testonly => match self.testonly[at] {
                Tri::Yes => Some("1"),
                Tri::No => Some("0"),
                Tri::Unknown => None,
            },
        }
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
