//! The query functions reading the targets a rule's attribute names, through
//! what Bazel's target accessors give as its prerequisites: of a target,
//! every label the attribute names in any `select` branch
//! (`BlazeTargetAccessor.getPrerequisites`); of a configured target, the
//! dependencies its resolved attribute names (`ConfiguredTargetAccessor`).
//! A label naming a target the graph does not hold names nothing, where Bazel
//! would load the target's package to find it.

use fixedbitset::FixedBitSet;

use super::ast::Function;
use super::declared::TestRole;
use super::narrow::left_out;
use super::universe::Universe;
use crate::attributes::{RowId, Stated, Value};
use crate::facets::Sym;
use crate::graph::Kind;
use crate::intern::Id;

/// The nodes some rules' attributes name.
#[derive(Default)]
struct Prerequisites {
    /// Targets, each standing with its configured targets as a pattern naming
    /// it does.
    targets: Vec<Id>,
    /// Nodes standing for themselves alone.
    nodes: Vec<Id>,
}

/// The tags a test suite admits a test by (`TestsFunction.sortTagsBySense`):
/// `-x` excludes a test tagged `x`, `+x` and `x` require it, and `manual`,
/// marking the suite itself, does neither.
struct TagFilter<'a> {
    required: Vec<&'a str>,
    excluded: Vec<&'a str>,
}

impl<'a> TagFilter<'a> {
    fn of(tags: &[&'a str]) -> Self {
        let excluded = tags.iter().filter(|tag| tag.starts_with('-')).count();
        let mut filter = TagFilter {
            required: Vec::with_capacity(tags.len() - excluded),
            excluded: Vec::with_capacity(excluded),
        };
        for &tag in tags {
            if let Some(excluded) = tag.strip_prefix('-') {
                filter.excluded.push(excluded);
            } else if let Some(required) = tag.strip_prefix('+') {
                filter.required.push(required);
            } else if tag != "manual" {
                filter.required.push(tag);
            }
        }
        filter
    }

    /// Whether every test passes, whatever its tags.
    fn admits_all(&self) -> bool {
        self.required.is_empty() && self.excluded.is_empty()
    }

    /// Whether a test whose tags and size are `tags` passes.
    fn admits(&self, tags: &[&str]) -> bool {
        !self.excluded.iter().any(|tag| tags.contains(tag))
            && self.required.iter().all(|tag| tags.contains(tag))
    }
}

/// An attribute by name, and its handle where some rule states it.
#[derive(Clone, Copy)]
struct Attribute<'a> {
    name: &'a str,
    handle: Option<Sym>,
}

/// The most aliases a chain of them is followed through, past which it is
/// taken for a cycle, which Bazel rejects.
const ALIAS_STEPS: usize = 64;

impl Universe {
    /// What the attribute `name` of each rule among `value` names
    /// (`LabelsFunction`).
    pub(super) fn labels(&self, name: &str, value: &FixedBitSet) -> Result<FixedBitSet, String> {
        let attribute = self.attribute(name);
        let mut named = Prerequisites::default();
        let mut positions = Vec::new();
        for id in value.ones().map(|id| Id(id as u32)) {
            let context = |label: &str| format!("in '{name}' of rule {label}: ");
            self.prerequisites(
                Function::Labels,
                id,
                attribute,
                &context,
                &mut positions,
                &mut named,
            )?;
        }
        Ok(self.joined(named))
    }

    /// The tests among `value` and those the test suites among it name, as
    /// `TestsFunction` reads them outside `--strict_test_suite`: each suite
    /// keeps of the tests its `tests` and `$implicit_tests` name those its
    /// own tags admit, and takes in the suites its `tests` name, each once.
    pub(super) fn tests(&self, value: &FixedBitSet) -> Result<FixedBitSet, String> {
        let [listed, implicit, tags, size] =
            ["tests", "$implicit_tests", "tags", "size"].map(|name| self.attribute(name));
        let mut tests = self.empty();
        let mut expanded = self.empty();
        let mut open: Vec<Id> = Vec::new();
        for id in value.ones().map(|id| Id(id as u32)) {
            match self.test_role(id) {
                Some(TestRole::Test) => tests.insert(id.0 as usize),
                Some(TestRole::Suite) if !expanded.put(id.0 as usize) => open.push(id),
                Some(TestRole::Suite) | None => {}
            }
        }
        let mut admitted = Prerequisites::default();
        let (mut named, mut positions, mut strings) =
            (Prerequisites::default(), Vec::new(), Vec::new());
        while let Some(suite) = open.pop() {
            let mut filter = None;
            // `$implicit_tests` names tests alone.
            for (attribute, names_suites) in [(listed, true), (implicit, false)] {
                let name = attribute.name;
                let context = |label: &str| {
                    format!("couldn't expand '{name}' attribute of test_suite {label}: ")
                };
                self.prerequisites(
                    Function::Tests,
                    suite,
                    attribute,
                    &context,
                    &mut positions,
                    &mut named,
                )?;
                for (found, kept) in [
                    (&mut named.targets, &mut admitted.targets),
                    (&mut named.nodes, &mut admitted.nodes),
                ] {
                    for id in found.drain(..) {
                        match self.test_role(id) {
                            Some(TestRole::Test) => {
                                let filter = match &mut filter {
                                    Some(filter) => filter,
                                    None => {
                                        strings.clear();
                                        self.strings(suite, tags, &mut strings)?;
                                        filter.insert(TagFilter::of(&strings))
                                    }
                                };
                                if self.admits(filter, id, [tags, size], &mut strings)? {
                                    kept.push(id);
                                }
                            }
                            Some(TestRole::Suite) if names_suites => {
                                if !expanded.put(id.0 as usize) {
                                    open.push(id);
                                }
                            }
                            Some(TestRole::Suite) | None => {}
                        }
                    }
                }
            }
        }
        tests.union_with(&self.joined(admitted));
        Ok(tests)
    }

    /// Whether `filter` admits the test `id` by the attributes holding its
    /// tags and size, read only where the filter asks for any; `strings` is a
    /// buffer the call reuses.
    fn admits<'a>(
        &'a self,
        filter: &TagFilter<'_>,
        id: Id,
        tags_and_size: [Attribute<'_>; 2],
        strings: &mut Vec<&'a str>,
    ) -> Result<bool, String> {
        if filter.admits_all() {
            return Ok(true);
        }
        strings.clear();
        for attribute in tags_and_size {
            self.strings(id, attribute, strings)?;
        }
        Ok(filter.admits(strings))
    }

    /// What `id` is to `tests`, where it stands for a rule.
    fn test_role(&self, id: Id) -> Option<TestRole> {
        match self.index.kind(id) {
            Some(Kind::Target | Kind::ConfiguredTarget) => {
                self.declared.test_role(self.position(id)?)
            }
            _ => None,
        }
    }

    /// Adds to `into` the strings the string or string list `attribute` of
    /// `id` holds, none where the rule lacks it.
    fn strings<'a>(
        &'a self,
        id: Id,
        attribute: Attribute<'_>,
        into: &mut Vec<&'a str>,
    ) -> Result<(), String> {
        let row = match self.attribute_of(id, attribute.handle) {
            Stated::Row(row) => row,
            Stated::Absent | Stated::Undescribed => return Ok(()),
            Stated::LeftOut => return Err(left_out(Function::Tests, attribute.name)),
        };
        let resolve = |sym: &Sym| self.attributes.resolve(*sym);
        match &self.attributes.row(row).value {
            Value::String(string) => into.push(resolve(string)),
            Value::Strings(strings) => into.extend(strings.iter().map(resolve)),
            _ => {}
        }
        Ok(())
    }

    fn attribute<'a>(&self, name: &'a str) -> Attribute<'a> {
        Attribute {
            name,
            handle: self.attributes.name(name),
        }
    }

    /// Adds to `named` the prerequisites `attribute` of `id` names, for
    /// `function`; or fails as Bazel does where a configured target lacks
    /// the attribute, its message opened by `context` of the label of `id`.
    /// `positions` is a buffer the call reuses.
    fn prerequisites(
        &self,
        function: Function,
        id: Id,
        attribute: Attribute<'_>,
        context: &dyn Fn(&str) -> String,
        positions: &mut Vec<usize>,
        named: &mut Prerequisites,
    ) -> Result<(), String> {
        let (configured, rule) = match self.index.kind(id) {
            Some(Kind::Target) => (false, id),
            Some(Kind::ConfiguredTarget) => (true, self.actual(id, positions)),
            _ => return Ok(()),
        };
        let row = match self.attribute_of(rule, attribute.handle) {
            Stated::Row(row) => row,
            Stated::Undescribed => return Ok(()),
            Stated::Absent if !configured => return Ok(()),
            Stated::Absent => {
                let label = self.label(id).unwrap_or_default();
                let class = self.rule_class(rule).unwrap_or_default();
                let name = attribute.name;
                return Err(format!(
                    "{}configured target of type {class} does not have attribute '{name}'",
                    context(label)
                ));
            }
            Stated::LeftOut => return Err(left_out(function, attribute.name)),
        };
        if !configured {
            // Bazel stores no `visibility` a BUILD file leaves unset, and its
            // default names nothing; the proto states the package's default
            // there.
            let row = self.attributes.row(row);
            if attribute.name == "visibility" && !row.explicitly_specified {
                return Ok(());
            }
            let entries = self.labels.entries();
            let mut visit = |label: Sym| {
                let at = self.labels.position(self.attributes.resolve(label));
                named.targets.extend(at.map(|at| entries[at].0));
            };
            row.value.for_each_label(&mut visit);
            return Ok(());
        }
        self.named_positions(row, positions);
        for dependency in self.index.followed_dependencies(rule) {
            let named_here = self
                .position(dependency)
                .map(|at| positions.binary_search(&at));
            if let Some(Ok(_)) = named_here {
                // A dependency cquery names by its target alone, in no
                // configuration, stands for none of the label's configured
                // targets.
                named.nodes.push(dependency);
            }
        }
        Ok(())
    }

    /// Sets `positions` to those of the labels `row` names, sorted.
    fn named_positions(&self, row: RowId, positions: &mut Vec<usize>) {
        positions.clear();
        let mut visit = |label: Sym| {
            positions.extend(self.labels.position(self.attributes.resolve(label)));
        };
        self.attributes.row(row).value.for_each_label(&mut visit);
        positions.sort_unstable();
    }

    /// The configured target `id` stands for, as `CqueryNode.getActual`
    /// resolves it: where `id` is an alias, the configured target among its
    /// dependencies its `actual` attribute names, resolved in turn; `id`
    /// itself where it is none, or where the graph holds no such configured
    /// target. `positions` is a buffer the call reuses.
    fn actual(&self, id: Id, positions: &mut Vec<usize>) -> Id {
        let handle = self.attributes.name("actual");
        let mut actual = id;
        for _ in 0..ALIAS_STEPS {
            if self.rule_class(actual) != Some("alias") {
                break;
            }
            let Stated::Row(row) = self.attribute_of(actual, handle) else {
                break;
            };
            self.named_positions(row, positions);
            let named = |dependency: &Id| {
                self.index.kind(*dependency) == Some(Kind::ConfiguredTarget)
                    && (self.position(*dependency))
                        .is_some_and(|at| positions.binary_search(&at).is_ok())
            };
            match self.index.followed_dependencies(actual).find(named) {
                Some(next) => actual = next,
                None => break,
            }
        }
        actual
    }

    /// The value `named` stands for.
    fn joined(&self, named: Prerequisites) -> FixedBitSet {
        let mut value = self.with_configured(named.targets);
        value.extend(named.nodes.into_iter().map(|id| id.0 as usize));
        value.intersect_with(self.members());
        value
    }

    /// The class of the rule `id` belongs to.
    fn rule_class(&self, id: Id) -> Option<&str> {
        let target = self.labels.entries()[self.position(id)?].0;
        let class = self.facets.rule_class(target)?;
        Some(self.facets.strings().resolve(class.sym()))
    }
}

#[cfg(test)]
mod tests;
