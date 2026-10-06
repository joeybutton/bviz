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
