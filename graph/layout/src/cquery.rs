//! Builds a compound graph from `bazel cquery --output=streamed_proto` output.
//! Each `CqueryResult` carries configured targets wrapping an unconfigured
//! `blaze_query.Target`, so the query mapping is reused over those inner targets.
//! Configurations are not incorporated; each configured target is mapped by its
//! label like a query target.

use bazel_query_proto::analysis::CqueryResult;

use crate::graph::Graph;

pub fn build(results: &[CqueryResult]) -> Graph {
    let targets: Vec<_> = results
        .iter()
        .flat_map(|r| r.results.iter())
        .filter_map(|ct| ct.target.as_ref())
        .collect();
    crate::query::build_targets(&targets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bazel_query_proto::analysis::ConfiguredTarget;
    use bazel_query_proto::blaze_query::{target::Discriminator, Rule, Target};

    fn rule_target(name: &str) -> Target {
        Target {
            r#type: Discriminator::Rule as i32,
            rule: Some(Rule {
                name: name.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn configured(name: &str, config_id: u32) -> ConfiguredTarget {
        ConfiguredTarget {
            target: Some(rule_target(name)),
            configuration_id: config_id,
            ..Default::default()
        }
    }

    #[test]
    fn configured_target_maps_like_its_query_target() {
        let results = vec![CqueryResult {
            results: vec![configured("//foo:app", 1)],
            ..Default::default()
        }];

        let graph = build(&results);

        let node = graph.nodes.iter().find(|n| n.id == "q://foo:app").unwrap();
        assert_eq!(node.kind, "target");
        assert_eq!(node.parent.as_deref(), Some("pkg:foo"));
    }

    #[test]
    fn same_label_across_configurations_collapses_to_one_node() {
        let results = vec![CqueryResult {
            results: vec![configured("//foo:app", 1), configured("//foo:app", 2)],
            ..Default::default()
        }];

        let graph = build(&results);

        assert_eq!(
            graph.nodes.iter().filter(|n| n.id == "q://foo:app").count(),
            1
        );
    }
}
