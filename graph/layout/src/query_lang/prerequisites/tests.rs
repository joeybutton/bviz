use bazel_query_proto::analysis::CqueryResult;
use bazel_query_proto::blaze_query::attribute::Discriminator as AttributeType;
use bazel_query_proto::blaze_query::{Attribute, Target};

use super::super::{Readable, parse};
use super::*;
use crate::intern::Names;
use crate::joint::{Joint, build_joint};
use crate::provenance::{Input, InputSet};
use crate::sources::{Deps, Sources};
use crate::test_support::{
    configuration, configured, configured_inputs, explicit, generated, label, label_list,
    name_attribute, nodep_label_list, package_group, rule, selector_list, source, string,
    string_list, through,
};

/// `attributes` with the `name` of `label` and the tags it leaves to their
/// default, as a rule states every attribute.
fn stating_every_attribute(label: &str, mut attributes: Vec<Attribute>) -> Vec<Attribute> {
    attributes.push(name_attribute(label));
    attributes.push(string_list("tags", &[]));
    attributes
}

/// The universe of `joint`'s graph with every input read.
fn every(joint: &Joint) -> Universe {
    through(joint, Sources::reading(&joint.capabilities))
}

/// The names of what `expr` names over `universe`, short of the `target:`
/// every target's name starts with, sorted; or the message it fails with.
fn query(universe: &Universe, names: &Names, expr: &str) -> Result<Vec<String>, String> {
    let value = universe
        .evaluate(&parse(expr).unwrap())
        .map_err(|e| e.message)?;
    let mut named: Vec<String> = (value.ones())
        .map(|id| {
            names
                .resolve(Id(id as u32))
                .trim_start_matches("target:")
                .to_string()
        })
        .collect();
    named.sort();
    Ok(named)
}

/// A query of `//a:gen`, whose `srcs` select a source on each branch, whose
/// `outs` name a file it writes, whose `tools` name a target the dump lacks
/// and whose visibility is its package's default `//a:pg`; and of `//a:bin`,
/// depending on it and visible to `//a:pg`.
fn generating() -> Joint {
    let outs = Attribute {
        r#type: AttributeType::OutputList as i32,
        ..label_list("outs", &["//a:gen.out"])
    };
    let gen_attributes = stating_every_attribute(
        "//a:gen",
        vec![
            explicit(selector_list(
                "srcs",
                &label_list("", &[]),
                &[&[
                    ("//c:x", Some(label_list("", &["//a:in.txt"]))),
                    (
                        "//conditions:default",
                        Some(label_list("", &["//a:other.txt"])),
                    ),
                ]],
            )),
            explicit(outs),
            explicit(label_list("tools", &["//zz:absent"])),
            explicit(string("cmd", "//a:in.txt")),
            nodep_label_list("visibility", &["//a:pg"]),
        ],
    );
    let bin_attributes = stating_every_attribute(
        "//a:bin",
        vec![
            explicit(label_list("deps", &["//a:gen"])),
            explicit(nodep_label_list("visibility", &["//a:pg"])),
        ],
    );
    let targets: Vec<Target> = vec![
        rule(
            "//a:gen",
            "genrule",
            &["//a:in.txt", "//a:other.txt"],
            gen_attributes,
        ),
        rule("//a:bin", "cc_binary", &["//a:gen"], bin_attributes),
        source("//a:in.txt"),
        source("//a:other.txt"),
        generated("//a:gen.out", "//a:gen"),
        package_group("//a:pg", &[]),
    ];
    build_joint(None, None, Some(&targets), false)
}

#[test]
fn labels_of_a_target_are_every_label_its_attribute_names() {
    let joint = generating();
    let universe = every(&joint);
    let labels = |expr| query(&universe, &joint.names, expr).unwrap();

    assert_eq!(
        labels("labels(srcs, //a:gen)"),
        ["//a:in.txt", "//a:other.txt"],
        "every branch of a select"
    );
    assert_eq!(labels("labels(outs, //a:gen)"), ["//a:gen.out"]);
    assert_eq!(labels("labels(deps, //a:gen + //a:bin)"), ["//a:gen"]);
    assert!(
        labels("labels(tools, //a:gen)").is_empty(),
        "absent from the graph"
    );
    assert!(labels("labels(cmd, //a:gen)").is_empty(), "no label type");
    assert!(labels("labels(no_such_attr, //a:all)").is_empty());
    assert!(
        labels("labels(srcs, //a:in.txt)").is_empty(),
        "a file is no rule"
    );
    assert_eq!(labels("labels(visibility, //a:bin)"), ["//a:pg"]);
    assert!(
        labels("labels(visibility, //a:gen)").is_empty(),
        "a visibility left to its package's default"
    );
}

#[test]
fn labels_fails_by_an_attribute_the_dump_leaves_out() {
    let targets = [
        rule(
            "//a:lib",
            "cc_library",
            &[],
            vec![name_attribute("//a:lib")],
        ),
        rule(
            "//a:bin",
            "cc_binary",
            &[],
            stating_every_attribute("//a:bin", Vec::new()),
        ),
    ];
    let joint = build_joint(None, None, Some(&targets), false);
    let universe = every(&joint);

    assert_eq!(
        query(&universe, &joint.names, "labels(deps, //a:all)").unwrap_err(),
        "labels() needs each rule's 'deps' attribute, which the graph's inputs state only \
         under --proto:output_rule_attrs=all --proto:default_values"
    );
}

/// A cquery of `//t:my_rule`, built in the top-level configuration, whose
/// `deps` name `//t:a`, built in an exec configuration, and `//t:b`, each
/// also built in the configuration the other is, and of `//t:al`, an alias of
/// `//t:my_rule`; and where `query`, a query of them but the alias. The
/// cquery states the configuration of each dependency where `transitions`.
fn configured_deps(query: bool, transitions: bool) -> Joint {
    let deps = || explicit(label_list("deps", &["//t:a", "//t:b"]));
    let mut my_rule = rule(
        "//t:my_rule",
        "my_rule",
        &["//t:a", "//t:b"],
        stating_every_attribute("//t:my_rule", vec![deps(), explicit(string("cmd", "x"))]),
    );
    let mut alias = rule(
        "//t:al",
        "alias",
        &["//t:my_rule"],
        stating_every_attribute("//t:al", vec![explicit(label("actual", "//t:my_rule"))]),
    );
    if transitions {
        my_rule.rule.as_mut().unwrap().configured_rule_input =
            configured_inputs(&[("//t:a", Some(2)), ("//t:b", Some(1))]);
        alias.rule.as_mut().unwrap().configured_rule_input =
            configured_inputs(&[("//t:my_rule", Some(1))]);
    }
    let leaf = |name: &str| {
        rule(
            name,
            "simple_rule",
            &[],
            stating_every_attribute(name, Vec::new()),
        )
    };
    let cquery = [CqueryResult {
        results: vec![
            configured(my_rule, 1),
            configured(alias, 1),
            configured(leaf("//t:a"), 1),
            configured(leaf("//t:a"), 2),
            configured(leaf("//t:b"), 1),
            configured(leaf("//t:b"), 2),
        ],
        configurations: vec![
            configuration(1, "aa11", "k8-fastbuild", false),
            configuration(2, "bb22", "k8-opt-exec-ST-1", true),
        ],
    }];
    let targets = [
        rule(
            "//t:my_rule",
            "my_rule",
            &["//t:a", "//t:b"],
            stating_every_attribute("//t:my_rule", vec![deps()]),
        ),
        leaf("//t:a"),
        leaf("//t:b"),
    ];
    let query = query.then_some(&targets[..]);
    build_joint(None, Some(&cquery), query, false)
}

/// The configured target `//t:my_rule`, alone of what its label names.
const MY_RULE: &str = "(//t:my_rule - //t:my_rule) + config(//t:my_rule, target)";

#[test]
fn labels_of_a_configured_target_are_the_dependencies_its_attribute_names() {
    let joint = configured_deps(false, true);
    let universe = every(&joint);
    let labels = |attribute: &str| {
        let expr = format!("labels({attribute}, {MY_RULE})");
        query(&universe, &joint.names, &expr)
    };

    assert_eq!(labels("deps").unwrap(), ["ct://t:a#bb22", "ct://t:b#aa11"]);
    assert!(labels("cmd").unwrap().is_empty());
    assert_eq!(
        labels("fake_attr").unwrap_err(),
        "in 'fake_attr' of rule //t:my_rule: configured target of type my_rule does not have \
         attribute 'fake_attr'"
    );
}

/// A dependency cquery states without its configuration stands for its
/// target alone, which a pattern naming it would widen to every configured
/// target of the label.
#[test]
fn labels_of_a_configured_target_name_a_dependency_in_no_configuration_by_its_target() {
    let joint = configured_deps(false, false);
    let universe = every(&joint);
    let expr = format!("labels(deps, {MY_RULE})");

    assert_eq!(
        query(&universe, &joint.names, &expr).unwrap(),
        ["//t:a", "//t:b"]
    );
}

/// A configured alias reads the attributes of the configured target it
/// resolves to, as cquery's accessor reads `getActual`.
#[test]
fn labels_of_a_configured_alias_are_those_of_its_actual() {
    let joint = configured_deps(false, true);
    let universe = every(&joint);
    let alias = "(//t:al - //t:al) + config(//t:al, target)";
    let labels = |attribute: &str| {
        let expr = format!("labels({attribute}, {alias})");
        query(&universe, &joint.names, &expr)
    };

    assert_eq!(labels("deps").unwrap(), ["ct://t:a#bb22", "ct://t:b#aa11"]);
    assert_eq!(
        labels("fake_attr").unwrap_err(),
        "in 'fake_attr' of rule //t:al: configured target of type my_rule does not have \
         attribute 'fake_attr'"
    );
}

#[test]
fn labels_of_a_configured_target_keep_to_the_tool_side_without_tool_deps() {
    let joint = configured_deps(false, true);
    let targets = (Sources::reading(&joint.capabilities))
        .with_deps(Deps::Tool, false)
        .unwrap();
    let universe = through(&joint, targets);
    let expr = format!("labels(deps, {MY_RULE})");

    assert_eq!(
        query(&universe, &joint.names, &expr).unwrap(),
        ["ct://t:b#aa11"]
    );
}

#[test]
fn labels_of_a_target_stand_for_their_configured_targets() {
    let joint = configured_deps(true, true);
    let universe = every(&joint);

    assert_eq!(
        query(&universe, &joint.names, "labels(deps, //t:my_rule)").unwrap(),
        [
            "//t:a",
            "//t:b",
            "ct://t:a#aa11",
            "ct://t:a#bb22",
            "ct://t:b#aa11",
            "ct://t:b#bb22"
        ]
    );
}

#[test]
fn labels_reads_a_query_or_a_cquery_stating_attributes() {
    let joint = configured_deps(true, true);
    let readable = |inputs: &[Input]| {
        let inputs = InputSet::of(inputs.iter().copied()).unwrap();
        Readable::of(&joint.graph, &joint.capabilities.reading(inputs)).check(Function::Labels)
    };

    assert_eq!(readable(&[Input::Cquery]), Ok(()));
    assert_eq!(
        readable(&[Input::Aquery]),
        Err("labels() requires a query or a cquery".to_owned())
    );
    let unstated = generating_without_attributes();
    assert_eq!(
        Readable::of(&unstated.graph, &unstated.capabilities).check(Function::Labels),
        Err(
            "labels() requires a query or a cquery with --proto:output_rule_attrs=all \
             --proto:default_values"
                .to_owned()
        )
    );
}

/// A query of `//a:gen` as `--noproto:default_values` writes it.
fn generating_without_attributes() -> Joint {
    let targets = [rule(
        "//a:gen",
        "genrule",
        &[],
        vec![name_attribute("//a:gen")],
    )];
    build_joint(None, None, Some(&targets), false)
}
