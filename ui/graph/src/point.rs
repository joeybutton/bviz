//! How a reader names the points of a measure, and what each one is.

use crate::model::Shared;
use bviz_graph_layout::facets::{ConfigRef, Tri};
use bviz_graph_layout::metrics::Unit;
use bviz_graph_layout::select::labels::carried_label;
use bviz_graph_layout::{Facets, Id, Kind};
use std::collections::HashMap;

/// How a reader names one thing of a kind, and several.
pub(crate) struct KindNames {
    pub(crate) one: &'static str,
    pub(crate) many: &'static str,
}

impl KindNames {
    /// How a reader names `count` things of the kind, one or several.
    pub(crate) fn of(&self, count: u32) -> &'static str {
        match count {
            1 => self.one,
            _ => self.many,
        }
    }

    /// `count` and how a reader names that many things of the kind.
    pub(crate) fn counted(&self, count: u32) -> String {
        format!("{count} {}", self.of(count))
    }
}

/// How a reader names one point of `unit`, and several.
pub(crate) fn unit_names(unit: Unit) -> KindNames {
    let (one, many) = match unit {
        Unit::Action => ("action", "actions"),
        Unit::ConfiguredTarget => ("configured target", "configured targets"),
        Unit::Target => ("target", "targets"),
    };
    KindNames { one, many }
}

pub(crate) fn kind_names(kind: Kind) -> KindNames {
    let (one, many) = match kind {
        Kind::Action => ("action", "actions"),
        Kind::ConfiguredTarget => ("configured target", "configured targets"),
        Kind::Source => ("source file", "source files"),
        Kind::Artifact => ("generated file", "generated files"),
        Kind::TargetGroup => (
            "package or environment group",
            "package and environment groups",
        ),
        _ => ("rule target", "rule targets"),
    };
    KindNames { one, many }
}

/// Pushes onto `lines` what the point `id`, of `kind`, is as a point of
/// `unit`, a line at a time.
pub(crate) fn identify(id: Id, kind: Kind, unit: Unit, shared: &Shared, lines: &mut Vec<String>) {
    let facets = &shared.facets;
    let label = point_label(id, shared);
    match unit {
        Unit::Action => {
            lines.extend([mnemonic(id, shared).to_owned(), format!("of {label}")]);
            lines.extend(configuration(id, shared).map(|name| format!("in {name}")));
            let aspects = facets.aspects().values(id);
            if !aspects.is_empty() {
                let aspects = aspects.iter().map(|&a| facets.strings().resolve(a));
                lines.push(format!("via {}", aspects.collect::<Vec<_>>().join(", ")));
            }
            let output = primary_output(id, shared);
            lines.extend(output.map(|output| format!("writes {output}")));
        }
        Unit::ConfiguredTarget => {
            lines.push(label);
            lines.extend(configuration(id, shared).map(|name| format!("in {name}")));
        }
        Unit::Target => {
            let repo = match facets.repo(id) {
                Some(repo) => match shared.labels.repos().written(repo) {
                    "" => MAIN_REPO,
                    written => written,
                },
                None => UNKNOWN_REPO,
            };
            lines.extend([label, format!("{} in {repo}", kind_names(kind).one)]);
        }
    }
}

/// The label of the target the node `id` is or belongs to, where one is
/// carried, and its name otherwise.
pub(crate) fn point_label(id: Id, shared: &Shared) -> String {
    match carried_label(&shared.names, &shared.facets, id) {
        Some(label) => shared.labels.written(label).into_owned(),
        None => shared.names.resolve(id).to_owned(),
    }
}

/// The mnemonic of the action `id`.
pub(crate) fn mnemonic(id: Id, shared: &Shared) -> &str {
    let facets = &shared.facets;
    let mnemonic = facets.mnemonic(id);
    mnemonic.map_or("action", |m| facets.strings().resolve(m))
}

/// The path of the file the action `id` writes first.
pub(crate) fn primary_output(id: Id, shared: &Shared) -> Option<&str> {
    // The base graph holds each node at the position of its id.
    let output = shared.facets.primary_output(id)?;
    let node = shared.base.nodes.get(output.0 as usize)?;
    Some(&node.label)
}

/// The configuration the node `id` is built in, named apart from every other
/// configuration of the graph.
pub(crate) fn configuration(id: Id, shared: &Shared) -> Option<String> {
    let config = shared.facets.config(id)?;
    Some(match shared.config_names.get(config) {
        Some(name) => name.whole.clone(),
        None => shared.facets.strings().resolve(config.sym()).to_owned(),
    })
}

/// How the configuration the node `id` is built in reads, most compact
/// first: by its [`ConfigName::short`] name, then named apart from every
/// other configuration of the graph.
pub(crate) fn configuration_readings(id: Id, shared: &Shared) -> [Option<String>; 2] {
    let config = shared.facets.config(id);
    let name = config.and_then(|config| shared.config_names.get(config));
    [
        name.map(|name| name.short.clone()),
        configuration(id, shared),
    ]
}

/// How each configuration of a graph is named.
#[derive(Default)]
pub(crate) struct ConfigNames(HashMap<ConfigRef, ConfigName>);

/// How one configuration of a graph is named.
pub(crate) struct ConfigName {
    /// Its mnemonic, or its checksum where it has none, marked where it
    /// builds a tool the build runs, which several configurations can share.
    pub(crate) short: String,
    /// `short`, followed where another configuration shares it by the
    /// shortest prefix of the checksum telling apart all that do.
    pub(crate) whole: String,
}

impl ConfigNames {
    pub(crate) fn of(facets: &Facets) -> Self {
        let resolve = |sym| facets.strings().resolve(sym);
        let mut alike: HashMap<(&str, bool), Vec<ConfigRef>> = HashMap::new();
        for (config, entry) in facets.configs() {
            let mnemonic = resolve(entry.mnemonic.unwrap_or(config.sym()));
            let tool = entry.is_tool == Tri::Yes;
            alike.entry((mnemonic, tool)).or_default().push(config);
        }
        let mut names = HashMap::with_capacity(alike.values().map(Vec::len).sum());
        for ((mnemonic, tool), configs) in alike {
            let checksums: Vec<&str> = configs.iter().map(|c| resolve(c.sym())).collect();
            let prefix = (configs.len() > 1).then(|| distinguishing_prefix(&checksums));
            for (config, checksum) in configs.into_iter().zip(checksums) {
                let part = prefix.map(|length| checksum.get(..length).unwrap_or(checksum));
                let name = |part: Option<&str>| match (tool, part) {
                    (false, None) => mnemonic.to_owned(),
                    (true, None) => format!("{mnemonic} (exec)"),
                    (false, Some(part)) => format!("{mnemonic} ({part})"),
                    (true, Some(part)) => format!("{mnemonic} (exec, {part})"),
                };
                let (short, whole) = (name(None), name(part));
                names.insert(config, ConfigName { short, whole });
            }
        }
        Self(names)
    }

    pub(crate) fn get(&self, config: ConfigRef) -> Option<&ConfigName> {
        self.0.get(&config)
    }
}

/// The fewest characters a checksum is cut to, so the part read beside a
/// mnemonic reads as a checksum rather than as a stray letter or count.
const MIN_CHECKSUM_PREFIX: usize = 4;

/// The shortest length, at least [`MIN_CHECKSUM_PREFIX`], to which each of
/// the distinct `checksums` is cut and still differs from every other.
fn distinguishing_prefix(checksums: &[&str]) -> usize {
    let mut sorted = checksums.to_vec();
    sorted.sort_unstable();
    let shared_prefix =
        |a: &str, b: &str| a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    let parting = sorted
        .windows(2)
        .map(|pair| shared_prefix(pair[0], pair[1]) + 1);
    parting.max().unwrap_or(0).max(MIN_CHECKSUM_PREFIX)
}

const MAIN_REPO: &str = "the main repository";
const UNKNOWN_REPO: &str = "an unnamed repository";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_are_cut_to_where_each_parts_from_every_other() {
        assert_eq!(
            distinguishing_prefix(&["3f2a01", "9c0b77"]),
            MIN_CHECKSUM_PREFIX
        );
        assert_eq!(distinguishing_prefix(&["abcdef01", "abcdef02", "0"]), 8);
        assert_eq!(distinguishing_prefix(&["abcde1", "abcdf2"]), 5);
    }

    /// Two configurations sharing a mnemonic, one of them also the mnemonic
    /// of a tool's configuration, are named apart by their checksums; each
    /// other configuration by its mnemonic alone.
    #[test]
    fn configurations_sharing_a_mnemonic_are_named_apart_by_checksum() {
        let mut facets = Facets::default();
        let fast = facets.intern_config("3f2a01aa", Some("k8-fastbuild"), Tri::No);
        let trimmed = facets.intern_config("3f2b9900", Some("k8-fastbuild"), Tri::No);
        let tool = facets.intern_config("77e0", Some("k8-fastbuild"), Tri::Yes);
        let bare = facets.intern_config("c0ffee", None, Tri::No);
        let names = ConfigNames::of(&facets);
        let read = |config| {
            let name = names.get(config).unwrap();
            (name.short.as_str(), name.whole.as_str())
        };
        assert_eq!(read(fast), ("k8-fastbuild", "k8-fastbuild (3f2a)"));
        assert_eq!(read(trimmed), ("k8-fastbuild", "k8-fastbuild (3f2b)"));
        assert_eq!(read(tool), ("k8-fastbuild (exec)", "k8-fastbuild (exec)"));
        assert_eq!(read(bare), ("c0ffee", "c0ffee"));
    }
}
