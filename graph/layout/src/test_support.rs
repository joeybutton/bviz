use bazel_query_proto::analysis::{
    Action, ActionGraphContainer, Artifact, Configuration, ConfiguredTarget, DepSetOfFiles,
    PathFragment,
};
use bazel_query_proto::blaze_query::{
    Attribute, GeneratedFile, Rule, SourceFile, Target, attribute::Discriminator as AttributeType,
    target::Discriminator,
};

use crate::graph::{Graph, Node};
use crate::intern::Names;
use std::collections::HashSet;

pub(crate) fn find<'a>(graph: &'a Graph, names: &Names, id: &str) -> &'a Node {
    graph
        .nodes
        .iter()
        .find(|n| names.resolve(n.id) == id)
        .unwrap()
}

pub(crate) fn parent_of<'a>(graph: &Graph, names: &'a Names, id: &str) -> Option<&'a str> {
    find(graph, names, id).parent.map(|p| names.resolve(p))
}

pub(crate) fn edges<'a>(graph: &Graph, names: &'a Names) -> HashSet<(&'a str, &'a str)> {
    graph
        .edges
        .iter()
        .map(|e| (names.resolve(e.from), names.resolve(e.to)))
        .collect()
}

/// A rule of class `class` as `bazel query --output=proto` writes it.
pub(crate) fn rule(name: &str, class: &str, inputs: &[&str], attributes: Vec<Attribute>) -> Target {
    Target {
        r#type: Discriminator::Rule as i32,
        rule: Some(Rule {
            name: name.to_string(),
            rule_class: class.to_string(),
            rule_input: inputs.iter().map(|input| input.to_string()).collect(),
            attribute: attributes,
            ..Default::default()
        }),
        ..Default::default()
    }
}

pub(crate) fn source(name: &str) -> Target {
    Target {
        r#type: Discriminator::SourceFile as i32,
        source_file: Some(SourceFile {
            name: name.to_string(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

pub(crate) fn generated(name: &str, rule: &str) -> Target {
    Target {
        r#type: Discriminator::GeneratedFile as i32,
        generated_file: Some(GeneratedFile {
            name: name.to_string(),
            generating_rule: rule.to_string(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

pub(crate) fn string_list(name: &str, values: &[&str]) -> Attribute {
    Attribute {
        name: name.to_string(),
        r#type: AttributeType::StringList as i32,
        string_list_value: values.iter().map(|value| value.to_string()).collect(),
        ..Default::default()
    }
}

pub(crate) fn boolean(name: &str, value: bool) -> Attribute {
    Attribute {
        name: name.to_string(),
        r#type: AttributeType::Boolean as i32,
        boolean_value: Some(value),
        ..Default::default()
    }
}

/// `target` built in the configuration cquery numbers `configuration_id`, `0`
/// standing for none.
pub(crate) fn configured(target: Target, configuration_id: u32) -> ConfiguredTarget {
    ConfiguredTarget {
        target: Some(target),
        configuration_id,
        ..Default::default()
    }
}

pub(crate) fn configuration(
    id: u32,
    checksum: &str,
    mnemonic: &str,
    is_tool: bool,
) -> Configuration {
    Configuration {
        id,
        checksum: checksum.to_string(),
        mnemonic: mnemonic.to_string(),
        is_tool,
        ..Default::default()
    }
}

/// One action of [`action_graph`]: the label of the target registering it,
/// its mnemonic, and the exec paths of the files it reads and writes, the
/// first written being its primary output.
pub(crate) struct ActionSpec<'a> {
    pub label: &'a str,
    pub mnemonic: &'a str,
    pub inputs: &'a [&'a str],
    pub outputs: &'a [&'a str],
}

/// An aquery of `actions`, all built in one configuration.
pub(crate) fn action_graph(actions: &[ActionSpec]) -> ActionGraphContainer {
    let mut graph = ActionGraphContainer {
        configuration: vec![configuration(1, "c0ffee", "k8-fastbuild", false)],
        ..Default::default()
    };
    for spec in actions {
        let target_id = match graph
            .targets
            .iter()
            .find(|target| target.label == spec.label)
        {
            Some(target) => target.id,
            None => {
                let id = graph.targets.len() as u32 + 1;
                graph.targets.push(bazel_query_proto::analysis::Target {
                    id,
                    label: spec.label.to_string(),
                    ..Default::default()
                });
                id
            }
        };
        let inputs: Vec<u32> = (spec.inputs.iter())
            .map(|path| artifact(&mut graph, path))
            .collect();
        let outputs: Vec<u32> = (spec.outputs.iter())
            .map(|path| artifact(&mut graph, path))
            .collect();
        let dep_set = graph.dep_set_of_files.len() as u32 + 1;
        graph.dep_set_of_files.push(DepSetOfFiles {
            id: dep_set,
            direct_artifact_ids: inputs,
            transitive_dep_set_ids: Vec::new(),
        });
        graph.actions.push(Action {
            target_id,
            mnemonic: spec.mnemonic.to_string(),
            configuration_id: 1,
            input_dep_set_ids: vec![dep_set],
            primary_output_id: outputs[0],
            output_ids: outputs,
            ..Default::default()
        });
    }
    graph
}

/// The id of the artifact at exec path `path` in `graph`, adding it if absent.
fn artifact(graph: &mut ActionGraphContainer, path: &str) -> u32 {
    let held = graph
        .path_fragments
        .iter()
        .find(|fragment| fragment.label == path);
    if let Some(fragment) = held {
        return fragment.id;
    }
    let id = graph.artifacts.len() as u32 + 1;
    graph.path_fragments.push(PathFragment {
        id,
        label: path.to_string(),
        parent_id: 0,
    });
    graph.artifacts.push(Artifact {
        id,
        path_fragment_id: id,
        is_tree_artifact: false,
    });
    id
}
