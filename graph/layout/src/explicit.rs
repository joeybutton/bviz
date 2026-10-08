//! The dependencies a rule's BUILD file names, which Bazel's
//! `--noimplicit_deps` keeps (`DependencyFilter.NO_IMPLICIT_DEPS`).

use std::borrow::Cow;
use std::collections::HashSet;

use bazel_query_proto::blaze_query::attribute::Discriminator;
use bazel_query_proto::blaze_query::{
    Attribute, LabelDictUnaryEntry, LabelKeyedStringDictEntry, LabelListDictEntry, Rule,
    StringListDictEntry,
};

use crate::graph::{Declared, DepClass};
use crate::label::RepoMapping;

/// The condition a `select` falls back to, which names no dependency.
const DEFAULT_CONDITION: &str = "//conditions:default";

/// The labels a rule's explicitly specified attributes name as dependencies,
/// each canonical: every label `AggregatingAttributeMapper.visitLabels`
/// visits, which is every branch of a `select` and every condition but the
/// default, and nothing of an output or `nodep` attribute.
pub(crate) struct Explicit<'a> {
    /// `None` where the rule's description states too little to tell.
    labels: Option<HashSet<Cow<'a, str>>>,
}

impl<'a> Explicit<'a> {
    /// What `rule`'s attributes name.
    pub(crate) fn of(rule: &'a Rule, repos: &RepoMapping) -> Self {
        let mut named: Vec<&str> = Vec::new();
        let mut classifies = false;
        for attribute in &rule.attribute {
            classifies |= states_name(attribute);
            if attribute.explicitly_specified == Some(true) && !attribute.nodep.unwrap_or(false) {
                named.extend(named_labels(attribute));
            }
        }
        let labels = classifies.then(|| {
            let canonical = named
                .into_iter()
                .map(|label| repos.canonicalize_label(label));
            canonical.collect()
        });
        Explicit { labels }
    }

    /// The class of the rule's dependency on the label `canonical` names.
    pub(crate) fn class(&self, canonical: &str) -> DepClass {
        DepClass::new(match &self.labels {
            Some(labels) if labels.contains(canonical) => Declared::Explicit,
            Some(_) => Declared::Implicit,
            None => Declared::Unknown,
        })
    }
}

/// Whether `rule`'s description states enough to classify its dependencies.
/// One stating no `name`, as a restricted `--proto:output_rule_attrs` writes
/// it, may leave out an attribute its BUILD file sets.
pub(crate) fn classifies(rule: &Rule) -> bool {
    rule.attribute.iter().any(states_name)
}

fn states_name(attribute: &Attribute) -> bool {
    attribute.name == "name"
}

/// The labels `attribute` names as dependencies, as written.
fn named_labels(attribute: &Attribute) -> Vec<&str> {
    let mut named = Vec::new();
    match attribute.selector_list.as_ref() {
        Some(list) if attribute.r#type == Discriminator::SelectorList as i32 => {
            let kind = list.r#type.unwrap_or(Discriminator::Unknown as i32);
            for entry in list.elements.iter().flat_map(|selector| &selector.entries) {
                named.extend((entry.label.as_deref()).filter(|&key| !is_default_condition(key)));
                values(
                    kind,
                    Values {
                        string: entry.string_value.as_deref(),
                        strings: &entry.string_list_value,
                        label_dict: &entry.label_dict_unary_value,
                        label_keyed: &entry.label_keyed_string_dict_value,
                        string_list_dict: &entry.string_list_dict_value,
                        label_list_dict: &entry.label_list_dict_value,
                    },
                    &mut named,
                );
            }
        }
        _ => values(
            attribute.r#type,
            Values {
                string: attribute.string_value.as_deref(),
                strings: &attribute.string_list_value,
                label_dict: &attribute.label_dict_unary_value,
                label_keyed: &attribute.label_keyed_string_dict_value,
                string_list_dict: &attribute.string_list_dict_value,
                label_list_dict: &attribute.label_list_dict_value,
            },
            &mut named,
        ),
    }
    named
}

fn is_default_condition(key: &str) -> bool {
    key.strip_prefix("@@").unwrap_or(key) == DEFAULT_CONDITION
}

/// The fields an attribute and a `select` branch may name labels in.
struct Values<'a> {
    string: Option<&'a str>,
    strings: &'a [String],
    label_dict: &'a [LabelDictUnaryEntry],
    label_keyed: &'a [LabelKeyedStringDictEntry],
    string_list_dict: &'a [StringListDictEntry],
    label_list_dict: &'a [LabelListDictEntry],
}

/// Pushes onto `named` the labels a value of type `kind` names as
/// dependencies.
fn values<'a>(kind: i32, values: Values<'a>, named: &mut Vec<&'a str>) {
    use Discriminator as D;
    match D::try_from(kind) {
        Ok(D::Label) => named.extend(values.string),
        // A dormant label is written as a label list's single string.
        Ok(D::LabelList) => {
            named.extend((values.strings.iter().map(String::as_str)).chain(values.string))
        }
        Ok(D::LabelDictUnary) => named.extend(values.label_dict.iter().map(|e| e.value.as_str())),
        Ok(D::LabelKeyedStringDict) => {
            named.extend(values.label_keyed.iter().map(|e| e.key.as_str()))
        }
        // Bazel writes a label-list dict into the string-list dict field.
        Ok(D::LabelListDict) => {
            let lists = (values.string_list_dict.iter().map(|e| &e.value))
                .chain(values.label_list_dict.iter().map(|e| &e.value));
            named.extend(lists.flatten().map(String::as_str));
        }
        _ => {}
    }
}
