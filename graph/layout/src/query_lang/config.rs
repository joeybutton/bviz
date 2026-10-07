//! `config()`, which names the targets of a value as built in one
//! configuration, as cquery's `ConfiguredTargetQueryEnvironment` names them.
//!
//! `target` names the configurations Bazel builds its top-level targets in,
//! which neither cquery's proto nor aquery's marks. They are taken to be the
//! configurations, other than a tool's, of the configured targets nothing in
//! the graph depends on by a cquery edge: a configured target some other one
//! depends on, or whose target another depends on by label, is reached
//! through that dependent and is not top-level. A graph holding no cquery
//! edges, as an aquery's holds none, makes every configured target top-level,
//! and a top-level target that is also a dependency of another is missed.

use fixedbitset::FixedBitSet;
use rustc_hash::{FxHashMap, FxHashSet};

use super::universe::Universe;
use crate::facets::{Facets, Tri};
use crate::graph::{EdgeKind, Graph, Kind};
use crate::intern::Id;

/// The configurations a graph's configured targets are built in.
pub(super) struct Configurations {
    /// Each configuration's checksum, and what it builds, sorted by checksum.
    checksums: Vec<(Box<str>, Role)>,
    /// The configuration of each configured target, by its position in
    /// `checksums`.
    built_in: FxHashMap<Id, u32>,
}

impl Configurations {
    pub(super) fn new(graph: &Graph, facets: &Facets) -> Self {
        let strings = facets.strings();
        let mut checksums: Vec<(Box<str>, Role)> = (facets.configs())
            .map(|(config, entry)| {
                let role = match entry.is_tool {
                    Tri::Yes => Role::Tool,
                    Tri::No | Tri::Unknown => Role::Dependency,
                };
                (strings.resolve(config.sym()).into(), role)
            })
            .collect();
        checksums.sort_unstable();
        let depended: FxHashSet<Id> = (graph.edges.iter())
            .filter(|edge| edge.kind == EdgeKind::Cquery)
            .map(|edge| edge.from)
            .collect();
        let mut built_in = FxHashMap::default();
        for node in &graph.nodes {
            if node.kind != Kind::ConfiguredTarget {
                continue;
            }
            let Some(config) = facets.config(node.id) else {
                continue;
            };
            let checksum = strings.resolve(config.sym());
            let Ok(at) = checksums.binary_search_by(|(held, _)| (**held).cmp(checksum)) else {
                continue;
            };
            built_in.insert(node.id, at as u32);
            let top_level = !depended.contains(&node.id)
                && node.parent.is_none_or(|target| !depended.contains(&target));
            if top_level && checksums[at].1 == Role::Dependency {
                checksums[at].1 = Role::TopLevel;
            }
        }
        Configurations {
            checksums,
            built_in,
        }
    }
}

/// The configuration `config()` names as the first tool configuration by
/// checksum a target is built in: a bviz extension of the Bazel 9.2.0
/// language bviz reads, which Bazel accepts from 10.0.0-pre.20260128.2.
pub const ANY_EXEC: &str = "anyexec";

/// What a configuration builds, as far as `config()` tells configurations
/// apart.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Role {
    /// Building a configured target nothing depends on.
    TopLevel,
    /// Building only configured targets something depends on.
    Dependency,
    Tool,
}

/// The configuration `config()`'s second argument names.
#[derive(Clone, Copy)]
enum Wanted {
    /// A configuration of [`Role::TopLevel`].
    Target,
    /// No configuration, which is a source file's or a package group's.
    Null,
    /// The first tool configuration by checksum a target is built in.
    AnyExec,
    /// The configuration at this position in [`Configurations::checksums`].
    Checksum(u32),
}

impl Universe {
    /// The targets of `value` as built in the configuration `config` names.
    /// `written` is `value`'s expression as Bazel prints it, which the
    /// message for naming nothing quotes.
    pub(super) fn config(
        &self,
        value: &FixedBitSet,
        config: &str,
        written: &str,
    ) -> Result<FixedBitSet, String> {
        let mut targets: Vec<usize> = value
            .ones()
            .filter_map(|id| self.position(Id(id as u32)))
            .collect();
        targets.sort_unstable();
        targets.dedup();
        let mut named = self.empty();
        let mut by_checksum = false;
        if let Some(&first) = targets.first() {
            let wanted = self.wanted(config, &self.labels.entries()[first].1)?;
            by_checksum = matches!(wanted, Wanted::Checksum(_));
            for at in targets {
                self.built(self.labels.entries()[at].0, wanted, &mut named);
            }
        }
        if named.is_clear() {
            let configuration = match by_checksum {
                true => format!("configuration with checksum '{config}'"),
                false => format!("'{config}' configuration"),
            };
            return Err(format!(
                "No target (in) {written} could be found in the {configuration}"
            ));
        }
        Ok(named)
    }

    /// The configuration `config` names, or the message Bazel rejects it with
    /// while naming `label` in it.
    fn wanted(&self, config: &str, label: &str) -> Result<Wanted, String> {
        let checksums = &self.configurations.checksums;
        match config {
            "host" => Err(
                "'host' configuration no longer exists. Use a specific configuration \
                           hash instead"
                    .to_string(),
            ),
            "target" => Ok(Wanted::Target),
            "null" => Ok(Wanted::Null),
            ANY_EXEC if checksums.iter().any(|&(_, role)| role == Role::Tool) => {
                Ok(Wanted::AnyExec)
            }
            ANY_EXEC => Err(format!(
                "Unable to identify 'exec' configuration for {label}\n\
                 config()'s second argument must identify a unique configuration.\n\
                 \n\
                 Valid values:\n \
                 'target' for the default configuration\n \
                 'null' for source files (which have no configuration)\n \
                 'anyexec' for identifying any path to a exec tool configuration\n \
                 an arbitrary configuration's full or short ID\n\
                 \n\
                 A short ID is any prefix of a full ID. cquery shows short IDs. 'bazel config' \
                 shows full IDs.\n\
                 \n\
                 For more help, see https://bazel.build/docs/cquery."
            )),
            prefix => {
                let start = checksums.partition_point(|(checksum, _)| **checksum < *prefix);
                let run = checksums[start..].iter();
                let matching: Vec<&str> = run
                    .map(|(checksum, _)| &**checksum)
                    .take_while(|checksum| checksum.starts_with(prefix))
                    .collect();
                match matching[..] {
                    [_] => Ok(Wanted::Checksum(start as u32)),
                    [] => Err(format!(
                        "Unknown configuration ID '{prefix}'.\n\
                         config()'s second argument must identify a unique configuration.\n\
                         \n\
                         Valid values:\n \
                         'target' for the default configuration\n \
                         'null' for source files (which have no configuration)\n \
                         an arbitrary configuration's full or short ID\n\
                         \n\
                         A short ID is any prefix of a full ID. cquery shows short IDs. 'bazel \
                         config' shows full IDs.\n\
                         \n\
                         For more help, see https://bazel.build/docs/cquery."
                    )),
                    // Listed by checksum, the order Bazel's
                    // `PostAnalysisQueryProcessor.getTransitiveConfigurations`
                    // sorts them in.
                    _ => Err(format!(
                        "Configuration ID '{prefix}' is ambiguous.\n\
                         '{prefix}' is a prefix of multiple configurations:\n {}\n\
                         \n\
                         Use a longer prefix to uniquely identify one configuration.",
                        matching.join("\n ")
                    )),
                }
            }
        }
    }

    /// Adds to `named` the target `target` as built in `wanted`: the
    /// configured targets it holds built there, or itself where it is built
    /// in no configuration.
    fn built(&self, target: Id, wanted: Wanted, named: &mut FixedBitSet) {
        let configurations = &self.configurations;
        let configured = || {
            let held = self.index.children(target);
            held.filter_map(|child| Some((child, *configurations.built_in.get(&child)?)))
        };
        match wanted {
            Wanted::Null => {
                if matches!(
                    self.index.kind(target),
                    Some(Kind::Source | Kind::TargetGroup)
                ) {
                    named.insert(target.0 as usize);
                }
            }
            Wanted::Target => named.extend(
                configured()
                    .filter(|&(_, at)| configurations.checksums[at as usize].1 == Role::TopLevel)
                    .map(|(child, _)| child.0 as usize),
            ),
            Wanted::AnyExec => {
                let tools = configured()
                    .filter(|&(_, at)| configurations.checksums[at as usize].1 == Role::Tool);
                if let Some((child, _)) = tools.min_by_key(|&(_, at)| at) {
                    named.insert(child.0 as usize);
                }
            }
            Wanted::Checksum(wanted) => named.extend(
                configured()
                    .filter(|&(_, at)| at == wanted)
                    .map(|(child, _)| child.0 as usize),
            ),
        }
    }
}
