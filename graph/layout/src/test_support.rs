use bazel_query_proto::analysis::{
    Action, ActionGraphContainer, Artifact, Configuration, ConfiguredTarget, DepSetOfFiles,
    PathFragment,
};
use bazel_query_proto::blaze_query::attribute::{
    Discriminator as AttributeType, Selector, SelectorEntry, SelectorList, Tristate,
};
use bazel_query_proto::blaze_query::{
    Attribute, GeneratedFile, License, Rule, SourceFile, StringDictEntry, Target,
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

/// A rule of class `class` as `bazel query --output=proto` writes it, its
/// attributes sorted by name as Bazel lists them.
pub(crate) fn rule(
    name: &str,
    class: &str,
    inputs: &[&str],
    mut attributes: Vec<Attribute>,
) -> Target {
    attributes.sort_by(|a, b| a.name.cmp(&b.name));
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

pub(crate) fn tristate(name: &str, value: Tristate) -> Attribute {
    Attribute {
        name: name.to_string(),
        r#type: AttributeType::Tristate as i32,
        tristate_value: Some(value as i32),
        ..Default::default()
    }
}

pub(crate) fn int(name: &str, value: i32) -> Attribute {
    Attribute {
        name: name.to_string(),
        r#type: AttributeType::Integer as i32,
        int_value: Some(value),
        ..Default::default()
    }
}

pub(crate) fn string(name: &str, value: &str) -> Attribute {
    Attribute {
        name: name.to_string(),
        r#type: AttributeType::String as i32,
        string_value: Some(value.to_string()),
        ..Default::default()
    }
}

pub(crate) fn label(name: &str, value: &str) -> Attribute {
    Attribute {
        r#type: AttributeType::Label as i32,
        ..string(name, value)
    }
}

pub(crate) fn label_list(name: &str, values: &[&str]) -> Attribute {
    Attribute {
        r#type: AttributeType::LabelList as i32,
        ..string_list(name, values)
    }
}

/// A label list of a type no dependency is drawn through, as Bazel writes
/// `visibility`: a string list marked `nodep`.
pub(crate) fn nodep_label_list(name: &str, values: &[&str]) -> Attribute {
    Attribute {
        nodep: Some(true),
        ..string_list(name, values)
    }
}

pub(crate) fn string_dict(name: &str, entries: &[(&str, &str)]) -> Attribute {
    Attribute {
        name: name.to_string(),
        r#type: AttributeType::StringDict as i32,
        string_dict_value: (entries.iter())
            .map(|&(key, value)| StringDictEntry {
                key: key.to_string(),
                value: value.to_string(),
            })
            .collect(),
        ..Default::default()
    }
}

pub(crate) fn license(name: &str, types: &[&str], exceptions: &[&str]) -> Attribute {
    Attribute {
        name: name.to_string(),
        r#type: AttributeType::License as i32,
        license: Some(License {
            license_type: types.iter().map(|value| value.to_string()).collect(),
            exception: exceptions.iter().map(|value| value.to_string()).collect(),
        }),
        ..Default::default()
    }
}

/// `attribute` as one written in the BUILD file.
pub(crate) fn explicit(attribute: Attribute) -> Attribute {
    Attribute {
        explicitly_specified: Some(true),
        ..attribute
    }
}

/// The attribute `name` written as the concatenation of `selects`, each a
/// list of branches: a condition label and the attribute whose value the
/// branch takes. A branch of `None` takes `none`'s value, the default of the
/// attribute's type, which also gives the type.
pub(crate) fn selector_list(
    name: &str,
    none: &Attribute,
    selects: &[&[(&str, Option<Attribute>)]],
) -> Attribute {
    let entry = |condition: &str, value: &Option<Attribute>| {
        let is_none = value.is_none();
        let value = value.as_ref().unwrap_or(none);
        SelectorEntry {
            label: Some(condition.to_string()),
            is_default_value: Some(is_none),
            int_value: value.int_value,
            string_value: value.string_value.clone(),
            boolean_value: value.boolean_value,
            tristate_value: value.tristate_value,
            string_list_value: value.string_list_value.clone(),
            license: value.license.clone(),
            string_dict_value: value.string_dict_value.clone(),
            ..Default::default()
        }
    };
    Attribute {
        name: name.to_string(),
        r#type: AttributeType::SelectorList as i32,
        nodep: none.nodep,
        selector_list: Some(SelectorList {
            r#type: Some(none.r#type),
            elements: (selects.iter())
                .map(|branches| Selector {
                    entries: (branches.iter())
                        .map(|(condition, value)| entry(condition, value))
                        .collect(),
                    ..Default::default()
                })
                .collect(),
        }),
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
