//! The query functions reading the targets a rule's attribute names, through
//! what Bazel's target accessors give as its prerequisites: of a target,
//! every label the attribute names in any `select` branch
//! (`BlazeTargetAccessor.getPrerequisites`); of a configured target, the
//! dependencies its resolved attribute names (`ConfiguredTargetAccessor`).
//! A label naming a target the graph does not hold names nothing, where Bazel
//! would load the target's package to find it.

use fixedbitset::FixedBitSet;

use super::ast::Function;
use super::narrow::left_out;
use super::universe::Universe;
use crate::attributes::{RowId, Stated};
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
