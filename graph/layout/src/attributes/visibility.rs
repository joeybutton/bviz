//! The visibility of targets as a query states it beyond the `visibility`
//! attribute a rule holds.

use bazel_query_proto::blaze_query::{PackageGroup as PackageGroupPb, SourceFile};

use super::AttributesBuilder;
use crate::facets::{SparseFacet, SparseFacetBuilder, Sym};
use crate::intern::Id;
use crate::label::RepoMapping;

/// A package: the repository part its canonical labels carry, empty for the
/// main repository, and its path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Package {
    pub repo: Sym,
    pub path: Sym,
}

/// The packages one entry of a package group's `packages` names beyond
/// every one (`PackageSpecification`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Packages {
    Single(Package),
    /// The package and every one beneath it in its repository.
    Beneath(Package),
}

/// What one entry of a package group's `packages` names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Specified {
    All,
    Packages(Packages),
}

/// One entry of a package group's `packages`. A negative one takes what it
/// names out of the group whatever else in the group names it; `private`
/// names no package, so no entry holds it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PackageSpec {
    pub packages: Specified,
    pub negative: bool,
}

/// A target a visibility or a package group's includes names by label.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Named {
    Node(Id),
    /// A label naming no target the query holds.
    Absent(Sym),
}

impl Named {
    fn renumber(self, remap: &[Id]) -> Self {
        match self {
            Named::Node(node) => Named::Node(remap[node.0 as usize]),
            absent @ Named::Absent(_) => absent,
        }
    }
}

/// What a query describes a target that is no rule as.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Described {
    SourceFile,
    /// An output file, by the node of the rule generating it.
    OutputFile(Id),
    PackageGroup,
}

impl Described {
    fn renumber(self, remap: &[Id]) -> Self {
        match self {
            Described::OutputFile(rule) => Described::OutputFile(remap[rule.0 as usize]),
            described => described,
        }
    }
}

/// What a query states of the visibility of the targets of one graph, by
/// node.
#[derive(Clone, Default)]
pub struct Visibilities {
    described: SparseFacet<Described>,
    /// By source file, the labels its visibility declares.
    declared: SparseFacet<Sym>,
    /// By package group, the entries of its `packages`.
    specs: SparseFacet<PackageSpec>,
    groups: SparseFacet<Named>,
}

impl Visibilities {
    /// The labels the visibility of the source file `node` declares, `None`
    /// where no query describes it as one.
    pub fn declared(&self, node: Id) -> Option<&[Sym]> {
        let described = self.described(node) == Some(Described::SourceFile);
        described.then(|| self.declared.values(node))
    }

    /// The rule generating the output file `node`, where the query holds both.
    pub fn generating_rule(&self, node: Id) -> Option<Id> {
        match self.described(node)? {
            Described::OutputFile(rule) => Some(rule),
            Described::SourceFile | Described::PackageGroup => None,
        }
    }

    /// The entries of the `packages` of the package group `node`, where a
    /// query describes it as one.
    pub fn package_group(&self, node: Id) -> Option<&[PackageSpec]> {
        let described = self.described(node) == Some(Described::PackageGroup);
        described.then(|| self.specs.values(node))
    }

    /// The targets `node` depends on as a query's `deps` follows its
    /// visibility: those its visibility names that may be package groups, or
    /// those a package group includes, in the order written
    /// (`Target.getVisibilityDependencyLabels`, `PackageGroup.getIncludes`).
    pub fn groups(&self, node: Id) -> &[Named] {
        self.groups.values(node)
    }

    fn described(&self, node: Id) -> Option<Described> {
        self.described.values(node).first().copied()
    }

    /// Moves every node's entries to the node id `remap` gives it, under the
    /// same terms as [`super::Attributes::renumber`].
    pub(super) fn renumber(&mut self, remap: &[Id]) {
        (self.described).renumber_with(remap, |described| described.renumber(remap));
        self.declared.renumber(remap);
        self.specs.renumber(remap);
        (self.groups).renumber_with(remap, |named| named.renumber(remap));
    }
}

/// Collects a [`Visibilities`] table as a query states targets.
#[derive(Default)]
pub(super) struct VisibilitiesBuilder {
    described: SparseFacetBuilder<Described>,
    declared: SparseFacetBuilder<Sym>,
    specs: SparseFacetBuilder<PackageSpec>,
    groups: SparseFacetBuilder<Named>,
}

impl VisibilitiesBuilder {
    pub(super) fn seal(self) -> Visibilities {
        Visibilities {
            described: self.described.seal(),
            declared: self.declared.seal(),
            specs: self.specs.seal(),
            groups: self.groups.seal(),
        }
    }
}

impl AttributesBuilder {
    /// Makes room for `targets` more targets that are no rules, `declared`
    /// more labels source files' visibilities declare, `specs` more entries
    /// of package groups' `packages` and `groups` more targets visibilities
    /// and package groups name.
    pub(crate) fn reserve_visibilities(
        &mut self,
        targets: usize,
        declared: usize,
        specs: usize,
        groups: usize,
    ) {
        let visibilities = &mut self.visibilities;
        visibilities.described.reserve(targets);
        visibilities.declared.reserve(declared);
        visibilities.specs.reserve(specs);
        visibilities.groups.reserve(groups);
    }

    /// Records the labels the visibility of the source file `node` declares.
    pub(crate) fn describe_source_file(
        &mut self,
        node: Id,
        file: &SourceFile,
        repos: &RepoMapping,
    ) {
        self.visibilities
            .described
            .push(node, Described::SourceFile);
        for label in &file.visibility_label {
            let label = self.strings.intern(&repos.canonicalize_label(label));
            self.visibilities.declared.push(node, label);
        }
    }

    /// Records the rule generating the output file `node`.
    pub(crate) fn describe_generated_file(&mut self, node: Id, rule: Id) {
        let described = &mut self.visibilities.described;
        described.push(node, Described::OutputFile(rule));
    }

    /// Records the packages the package group `node` names.
    pub(crate) fn describe_package_group(
        &mut self,
        node: Id,
        group: &PackageGroupPb,
        repos: &RepoMapping,
    ) {
        self.visibilities
            .described
            .push(node, Described::PackageGroup);
        for written in &group.contained_package {
            if let Some(spec) = self.package_spec(written, repos) {
                self.visibilities.specs.push(node, spec);
            }
        }
    }

    /// Records that `node` depends on the target the canonical label
    /// `canonical` names, `found` where the query holds it, as a query's
    /// `deps` follows its visibility or a package group's includes.
    pub(crate) fn name_group(&mut self, node: Id, canonical: &str, found: Option<Id>) {
        let named = match found {
            Some(target) => Named::Node(target),
            None => Named::Absent(self.strings.intern(canonical)),
        };
        self.visibilities.groups.push(node, named);
    }

    /// The entry `written` stands for, as `PackageGroupContents.packageStrings`
    /// writes one under `--incompatible_package_group_includes_double_slash`,
    /// a Bazel 9.2.0 default; `None` for `private`. Without that flag Bazel
    /// writes a main-repository package without its `//`, which reads alike,
    /// and every package as `//...`, which reads as the main repository's.
    fn package_spec(&mut self, written: &str, repos: &RepoMapping) -> Option<PackageSpec> {
        let (negative, spec) = match written.strip_prefix('-') {
            Some(spec) => (true, spec),
            None => (false, written),
        };
        let packages = match spec {
            "public" => Specified::All,
            "private" => return None,
            spec => Specified::Packages(match spec.strip_suffix("//...") {
                Some(repo) => Packages::Beneath(self.package(repo, "", repos)),
                None => match spec.strip_suffix("/...") {
                    Some(package) => Packages::Beneath(self.written_package(package, repos)),
                    None => Packages::Single(self.written_package(spec, repos)),
                },
            }),
        };
        Some(PackageSpec { packages, negative })
    }

    /// `package`, written `{repo}//{path}` or as a main-repository path alone.
    fn written_package(&mut self, package: &str, repos: &RepoMapping) -> Package {
        let (repo, path) = package.split_once("//").unwrap_or(("", package));
        self.package(repo, path, repos)
    }

    fn package(&mut self, repo: &str, path: &str, repos: &RepoMapping) -> Package {
        Package {
            repo: self.strings.intern(&repos.canonicalize_repo(repo)),
            path: self.strings.intern(path),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attributes::Attributes;

    fn group(packages: &[&str]) -> (Attributes, Vec<PackageSpec>) {
        let mut builder = AttributesBuilder::default();
        let written = PackageGroupPb {
            name: "//g:g".to_string(),
            contained_package: packages.iter().map(|p| p.to_string()).collect(),
            ..Default::default()
        };
        builder.describe_package_group(Id(0), &written, &RepoMapping::default());
        let table = builder.seal();
        let specs = table.visibilities().package_group(Id(0)).unwrap().to_vec();
        (table, specs)
    }

    /// Each entry as Bazel would write it under the default flags.
    fn read(packages: &[&str]) -> Vec<String> {
        let (table, specs) = group(packages);
        let package = |package: Package| {
            let (repo, path) = (table.resolve(package.repo), table.resolve(package.path));
            format!("{repo}//{path}")
        };
        (specs.iter())
            .map(|spec| {
                let sign = if spec.negative { "-" } else { "" };
                let packages = match spec.packages {
                    Specified::All => "public".to_string(),
                    Specified::Packages(Packages::Single(single)) => package(single),
                    Specified::Packages(Packages::Beneath(root)) => match package(root) {
                        root if root.ends_with("//") => format!("{root}..."),
                        root => format!("{root}/..."),
                    },
                };
                format!("{sign}{packages}")
            })
            .collect()
    }

    #[test]
    fn a_package_group_reads_every_form_bazel_writes() {
        let written = [
            "//a/b",
            "//a/...",
            "//...",
            "@@ext+//p",
            "@@ext+//...",
            "public",
            "-//a/b/c",
            "-//d/...",
        ];
        assert_eq!(read(&written), written);
        assert_eq!(read(&["private"]), [""; 0]);
        assert_eq!(read(&["@@//c", "legacy/path"]), ["//c", "//legacy/path"]);
    }

    #[test]
    fn a_target_described_with_nothing_is_told_from_one_never_described() {
        let mut builder = AttributesBuilder::default();
        let source = SourceFile {
            name: "//a:in".to_string(),
            ..Default::default()
        };
        builder.describe_source_file(Id(0), &source, &RepoMapping::default());
        let empty = PackageGroupPb {
            name: "//a:g".to_string(),
            ..Default::default()
        };
        builder.describe_package_group(Id(1), &empty, &RepoMapping::default());
        let table = builder.seal();
        let visibilities = table.visibilities();
        assert_eq!(visibilities.declared(Id(0)), Some(&[][..]));
        assert_eq!(visibilities.declared(Id(1)), None);
        assert_eq!(visibilities.package_group(Id(1)), Some(&[][..]));
        assert_eq!(visibilities.package_group(Id(0)), None);
    }

    #[test]
    fn renumbering_moves_each_entry_and_each_node_named_with_its_node() {
        let mut builder = AttributesBuilder::default();
        builder.describe_generated_file(Id(0), Id(2));
        let source = SourceFile {
            name: "//a:in".to_string(),
            visibility_label: vec!["//visibility:public".to_string()],
            ..Default::default()
        };
        builder.describe_source_file(Id(1), &source, &RepoMapping::default());
        builder.name_group(Id(1), "//a:g", Some(Id(0)));
        builder.name_group(Id(1), "//a:absent", None);
        let mut table = builder.seal();
        table.renumber(&[Id(2), Id(0), Id(1)]);
        let visibilities = table.visibilities();
        assert_eq!(visibilities.generating_rule(Id(2)), Some(Id(1)));
        assert_eq!(visibilities.generating_rule(Id(0)), None);
        let declared: Vec<&str> = (visibilities.declared(Id(0)).unwrap().iter())
            .map(|&label| table.resolve(label))
            .collect();
        assert_eq!(declared, ["//visibility:public"]);
        let [Named::Node(group), Named::Absent(absent)] = visibilities.groups(Id(0)) else {
            panic!("{:?}", visibilities.groups(Id(0)));
        };
        assert_eq!((*group, table.resolve(*absent)), (Id(2), "//a:absent"));
    }
}
