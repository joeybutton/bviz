//! The query function keeping the targets some targets may see, as Bazel
//! 9.2.0's `VisibleFunction` keeps them through `BlazeTargetAccessor`'s
//! visibility.

use fixedbitset::FixedBitSet;
use rustc_hash::{FxHashMap, FxHashSet};

use super::ast::Function;
use super::narrow::left_out;
use super::universe::Universe;
use crate::attributes::{Grant, Named, Package, PackageSpec, Packages, Specified, Stated};
use crate::facets::Sym;
use crate::graph::Kind;
use crate::intern::Id;
use crate::label::parse_package;

/// The packages one target's visibility grants, beyond its own
/// (`BlazeTargetAccessor.getVisibility`).
enum Visibility<'a> {
    Public,
    Granted {
        /// The packages its labels name directly.
        direct: Vec<Packages<&'a str>>,
        /// The entries of each package group it names or one of those
        /// includes, each group's taken alone, as a negative entry takes a
        /// package out of its own group only.
        groups: Vec<&'a [PackageSpec]>,
    },
}

const PRIVATE: Visibility<'static> = Visibility::Granted {
    direct: Vec::new(),
    groups: Vec::new(),
};

impl Universe {
    /// The members of `value` whose target every target of `to` may see,
    /// all of them where `to` is empty; or why the graph cannot tell for one.
    pub(super) fn visible(
        &self,
        to: &FixedBitSet,
        value: &FixedBitSet,
    ) -> Result<FixedBitSet, String> {
        let mut kept = self.empty();
        if to.is_clear() {
            kept.union_with(value);
            return Ok(kept);
        }
        let seers: Option<Vec<Package<&str>>> = (to.ones())
            .map(|id| self.label(Id(id as u32)).and_then(package_of))
            .collect();
        // A seer belonging to no package sees nothing.
        let Some(mut seers) = seers else {
            return Ok(kept);
        };
        seers.sort_unstable();
        seers.dedup();
        let mut verdicts: FxHashMap<usize, bool> = FxHashMap::default();
        for id in value.ones() {
            let Some(at) = self.position(Id(id as u32)) else {
                continue;
            };
            let verdict = match verdicts.get(&at) {
                Some(&verdict) => verdict,
                None => {
                    let verdict = self.visible_to_all(at, &seers)?;
                    verdicts.insert(at, verdict);
                    verdict
                }
            };
            if verdict {
                kept.insert(id);
            }
        }
        Ok(kept)
    }

    /// Whether the target at `at` among the labels is visible to a target in
    /// each of `seers`. Its visibility is read whether or not a seer shares
    /// its package, so that a label it names the graph lacks fails as it does
    /// in Bazel.
    fn visible_to_all(&self, at: usize, seers: &[Package<&str>]) -> Result<bool, String> {
        let (target, label) = &self.labels.entries()[at];
        let Some(own) = package_of(label) else {
            return Ok(false);
        };
        let visibility = self.visibility_of(*target)?;
        let attributes = &self.attributes;
        let grants = |seer: Package<&str>| match &visibility {
            Visibility::Public => true,
            Visibility::Granted { direct, groups } => {
                direct.iter().any(|packages| packages.hold(seer))
                    || groups.iter().any(|specs| {
                        let holds = |spec: &PackageSpec| match spec.packages {
                            Specified::All => true,
                            Specified::Packages(packages) => {
                                packages.map(|sym| attributes.resolve(sym)).hold(seer)
                            }
                        };
                        // `PackageGroupContents.containsPackage`.
                        !specs.iter().any(|spec| spec.negative && holds(spec))
                            && specs.iter().any(|spec| !spec.negative && holds(spec))
                    })
            }
        };
        Ok(seers
            .iter()
            .all(|&seer| same_package(own.path, seer.path) || grants(seer)))
    }

    /// The visibility the query describes `target` with, private where it
    /// describes none (`Target.getVisibility`).
    fn visibility_of(&self, target: Id) -> Result<Visibility<'_>, String> {
        let visibilities = self.attributes.visibilities();
        match self.index.kind(target) {
            Some(Kind::Target) => self.rule_visibility(target),
            Some(Kind::Source) => match visibilities.declared(target) {
                Some(declared) => self.granted(target, declared),
                None => Ok(PRIVATE),
            },
            Some(Kind::Artifact) => match visibilities.generating_rule(target) {
                Some(rule) => self.rule_visibility(rule),
                None => Ok(PRIVATE),
            },
            // An environment group is private.
            Some(Kind::TargetGroup) => match visibilities.package_group(target) {
                Some(_) => Ok(Visibility::Public),
                None => Ok(PRIVATE),
            },
            _ => Ok(PRIVATE),
        }
    }

    fn rule_visibility(&self, rule: Id) -> Result<Visibility<'_>, String> {
        match self.attribute_of(rule, self.visibility) {
            Stated::Row(row) => {
                let mut labels = Vec::new();
                (self.attributes.row(row).value).for_each_label(&mut |label| labels.push(label));
                self.granted(rule, &labels)
            }
            Stated::Absent | Stated::Undescribed => Ok(PRIVATE),
            Stated::LeftOut => Err(left_out(Function::Visible, "visibility")),
        }
    }

    /// What the visibility of `owner`, declaring `labels`, grants, the
    /// package groups it names read as the query's dependencies on them run.
    fn granted(&self, owner: Id, labels: &[Sym]) -> Result<Visibility<'_>, String> {
        let attributes = &self.attributes;
        let mut direct = Vec::with_capacity(labels.len());
        for &label in labels {
            match Grant::of(attributes.resolve(label)) {
                Grant::Public => return Ok(Visibility::Public),
                Grant::Packages(packages) => direct.push(packages),
                Grant::Private | Grant::Group => {}
            }
        }
        let visibilities = attributes.visibilities();
        let mut groups = Vec::new();
        let mut seen: FxHashSet<Id> = FxHashSet::default();
        // Each group's includes before its own entries, depth first in the
        // order written, so the first label the graph lacks is Bazel's
        // (`BlazeTargetAccessor.addAllPackageGroupsRecursive`).
        let mut open: Vec<Named> = visibilities.groups(owner).iter().rev().copied().collect();
        while let Some(named) = open.pop() {
            let node = match named {
                Named::Node(node) => node,
                Named::Absent(label) => return Err(absent_group(attributes.resolve(label))),
            };
            if !seen.insert(node) {
                continue;
            }
            if let Some(specs) = visibilities.package_group(node) {
                groups.push(specs);
                open.extend(visibilities.groups(node).iter().rev());
            }
        }
        Ok(Visibility::Granted { direct, groups })
    }
}

/// The package a label names a target of.
fn package_of(label: &str) -> Option<Package<&str>> {
    parse_package(label).map(|(repo, path)| Package { repo, path })
}

/// Why `visible` fails where a visibility or a package group it reaches
/// names `label`, which the graph holds no target of.
fn absent_group(label: &str) -> String {
    format!(
        "visible() cannot read the visibility label '{label}', which names no target of the \
         query; a query of //...:*, or of deps() over the targets, holds every package group a \
         visibility names"
    )
}

/// Whether a target of the package at path `own` is visible to one at
/// `seer` whatever its visibility: in the same package, of whichever
/// repository, or in `javatests/x` where it is in `java/x`
/// (`QueryVisibility.samePackage`).
fn same_package(own: &str, seer: &str) -> bool {
    own == seer
        || seer
            .strip_prefix("javatests/")
            .is_some_and(|tested| own.strip_prefix("java/") == Some(tested))
}

#[cfg(test)]
mod tests;
