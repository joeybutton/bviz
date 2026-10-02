//! How a reader names the points of a measure, and what each one is.

use crate::model::Shared;
use bviz_graph_layout::facets::Tri;
use bviz_graph_layout::metrics::Unit;
use bviz_graph_layout::select::labels::carried_label;
use bviz_graph_layout::{Id, Kind};

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

/// The configuration the node `id` is built in, by its mnemonic where it has
/// one, and marked where it builds a tool the build runs.
pub(crate) fn configuration(id: Id, shared: &Shared) -> Option<String> {
    let facets = &shared.facets;
    let config = facets.config(id)?;
    let entry = facets.config_entry(config);
    let name = entry
        .and_then(|entry| entry.mnemonic)
        .unwrap_or(config.sym());
    let name = facets.strings().resolve(name);
    Some(match entry.map(|entry| entry.is_tool) {
        Some(Tri::Yes) => format!("{name} (exec)"),
        _ => name.to_owned(),
    })
}

const MAIN_REPO: &str = "the main repository";
const UNKNOWN_REPO: &str = "an unnamed repository";
