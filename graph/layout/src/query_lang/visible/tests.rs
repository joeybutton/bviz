use bazel_query_proto::analysis::CqueryResult;
use bazel_query_proto::blaze_query::{Attribute, Target};

use super::super::{Readable, parse};
use super::*;
use crate::joint::{Joint, build_joint};
use crate::provenance::{Input, InputSet};
use crate::sources::Sources;
use crate::test_support::{
    ActionSpec, action_graph, configuration, configured, explicit, generated, name_attribute,
    nodep_label_list, package_group_of, rule, string_list, through, visible_source,
};

/// A rule `name` as a query states it, every attribute but its visibility
/// left to its default, which is `visibility` where given.
fn declaring(name: &str, visibility: Option<&[&str]>) -> Target {
    let mut attributes: Vec<Attribute> = vec![name_attribute(name), string_list("tags", &[])];
    attributes.extend(visibility.map(|labels| explicit(nodep_label_list("visibility", labels))));
    rule(name, "filegroup", &[], attributes)
}

/// The universe of a query of `targets`, every input read.
fn queried(targets: &[Target]) -> Universe {
    let joint = build_joint(None, None, Some(targets), false);
    through(&joint, Sources::reading(&joint.capabilities))
}

/// The labels of what `expr` names over `universe`, sorted; or the message
/// it fails with.
fn labels(universe: &Universe, expr: &str) -> Result<Vec<String>, String> {
    let value = (universe.evaluate(&parse(expr).unwrap())).map_err(|e| e.message)?;
    let mut labels: Vec<String> = (value.ones())
        .map(|id| universe.label(Id(id as u32)).unwrap().to_string())
        .collect();
    labels.sort();
    labels.dedup();
    Ok(labels)
}

/// The targets of `//t` visible to every target of `seers`.
fn visible_to(universe: &Universe, seers: &str) -> Vec<String> {
    labels(universe, &format!("visible({seers}, //t:*)")).unwrap()
}

/// Package `t` declaring a target of each kind of visibility, the groups it
/// names in package `g`, and a target in each package seeing them.
fn packages() -> Universe {
    let mut targets = vec![
        declaring("//t:public", Some(&["//visibility:public"])),
        declaring("//t:private", Some(&["//visibility:private"])),
        declaring("//t:pkg", Some(&["//a:__pkg__"])),
        declaring("//t:subpackages", Some(&["//a:__subpackages__"])),
        declaring("//t:group", Some(&["//g:outer"])),
        declaring("//t:not_a_group", Some(&["//a:a"])),
        declaring("//t:pkg_or_group", Some(&["//b:__pkg__", "//g:inner"])),
        visible_source("//t:in.txt", &["//a:__pkg__"]),
        declaring("//t:gen", Some(&["//b:__pkg__"])),
        generated("//t:gen.out", "//t:gen"),
        // Each includes the other, and the inner one names all of `a` but
        // `a/x` beneath it.
        package_group_of("//g:outer", &["//b"], &["//g:inner"]),
        package_group_of("//g:inner", &["//a/...", "-//a/x/..."], &["//g:outer"]),
    ];
    for package in ["a", "a/b", "a/x", "a/x/y", "b", "c", "g"] {
        targets.push(declaring(&format!("//{package}:a"), None));
    }
    queried(&targets)
}

#[test]
fn a_target_is_visible_where_its_visibility_grants() {
    let universe = packages();
    let everywhere = [
        "//t:gen.out",
        "//t:in.txt",
        "//t:pkg",
        "//t:public",
        "//t:subpackages",
    ];
    // The `//a:a` a visibility names is no package group, so grants nothing.
    let mut expected = vec!["//t:group", "//t:pkg_or_group"];
    expected.extend_from_slice(&everywhere[1..]);
    expected.sort();
    assert_eq!(visible_to(&universe, "//a:a"), expected);
    assert_eq!(
        visible_to(&universe, "//a/b:a"),
        [
            "//t:group",
            "//t:pkg_or_group",
            "//t:public",
            "//t:subpackages"
        ]
    );
    assert_eq!(
        visible_to(&universe, "//a/x/y:a"),
        ["//t:public", "//t:subpackages"],
        "a negative entry takes a package out of its group"
    );
    assert_eq!(
        visible_to(&universe, "//b:a"),
        [
            "//t:gen",
            "//t:gen.out",
            "//t:group",
            "//t:pkg_or_group",
            "//t:public"
        ],
        "an output file is visible where its rule is"
    );
    assert_eq!(visible_to(&universe, "//c:a"), ["//t:public"]);
}

#[test]
fn a_target_is_visible_to_all_of_its_own_package() {
    let universe = packages();
    let every = labels(&universe, "//t:*").unwrap();
    assert_eq!(visible_to(&universe, "//t:private"), every);
    assert_eq!(
        visible_to(&universe, "set()"),
        every,
        "no seer sees nothing"
    );
    assert_eq!(
        visible_to(&universe, "//a:a + //b:a"),
        ["//t:group", "//t:pkg_or_group", "//t:public"],
        "a target is kept only where every seer sees it"
    );
}

#[test]
fn a_package_group_is_public() {
    let universe = packages();
    assert_eq!(
        labels(&universe, "visible(//c:a, //g:*)").unwrap(),
        ["//g:inner", "//g:outer"]
    );
}

#[test]
fn java_is_visible_from_javatests_and_not_the_reverse() {
    let universe = queried(&[
        declaring("//java/x:a", Some(&["//visibility:private"])),
        declaring("//javatests/x:a", Some(&["//visibility:private"])),
        declaring("//javatests/y:a", Some(&["//visibility:private"])),
    ]);
    assert_eq!(
        labels(&universe, "visible(//javatests/x:a, //java/x:a)").unwrap(),
        ["//java/x:a"]
    );
    assert!((labels(&universe, "visible(//java/x:a, //javatests/x:a)").unwrap()).is_empty());
    assert!((labels(&universe, "visible(//javatests/y:a, //java/x:a)").unwrap()).is_empty());
}

#[test]
fn the_same_package_of_another_repository_sees_a_target() {
    let universe = queried(&[
        declaring("//p:a", Some(&["//visibility:private"])),
        declaring("@@ext+//p:a", Some(&["//visibility:private"])),
        declaring("@@ext+//q:a", Some(&["//p:__pkg__"])),
    ]);
    assert_eq!(
        labels(&universe, "visible(@@ext+//p:a, //p:a)").unwrap(),
        ["//p:a"]
    );
    assert!(
        (labels(&universe, "visible(@@ext+//p:a, @@ext+//q:a)").unwrap()).is_empty(),
        "a package granted is one of the repository the label names"
    );
}

/// A query of `//t:lib`, visible to package `a` alone, and `//a:bin`, and a
/// cquery of both in one configuration.
fn configured_pair(query: bool) -> Joint {
    let lib = declaring("//t:lib", Some(&["//a:__pkg__"]));
    let bin = declaring("//a:bin", None);
    let cquery = [CqueryResult {
        results: vec![configured(lib.clone(), 1), configured(bin.clone(), 1)],
        configurations: vec![configuration(1, "aa11", "k8-fastbuild", false)],
    }];
    let targets = [lib, bin];
    build_joint(None, Some(&cquery), query.then_some(&targets[..]), false)
}

#[test]
fn a_configured_target_stands_with_its_target() {
    let joint = configured_pair(true);
    let universe = through(&joint, Sources::reading(&joint.capabilities));
    let value = universe
        .evaluate(&parse("visible(//a:bin, //t:lib)").unwrap())
        .unwrap();
    let names: Vec<&str> = (value.ones())
        .map(|id| joint.names.resolve(Id(id as u32)))
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!((labels(&universe, "visible(//t:lib, //a:bin)").unwrap()).is_empty());
}

#[test]
fn visible_reads_a_query_stating_visibility() {
    let readable = |joint: &Joint, inputs: &[Input]| {
        let inputs = InputSet::of(inputs.iter().copied()).unwrap();
        Readable::of(&joint.graph, &joint.capabilities.reading(inputs)).check(Function::Visible)
    };
    let joint = configured_pair(true);
    assert_eq!(readable(&joint, &[Input::Query, Input::Cquery]), Ok(()));
    assert_eq!(
        readable(&joint, &[Input::Cquery]),
        Err("visible() is not supported on configured targets".to_owned())
    );
    let cquery = configured_pair(false);
    let universe = through(&cquery, Sources::reading(&cquery.capabilities));
    assert_eq!(
        labels(&universe, "visible(//a:bin, //t:lib)"),
        Err("visible() is not supported on configured targets".to_owned())
    );

    let unstated = build_joint(
        None,
        None,
        Some(&[rule("//t:lib", "filegroup", &[], Vec::new())]),
        false,
    );
    assert_eq!(
        readable(&unstated, &[Input::Query]),
        Err(
            "visible() requires a query with --proto:output_rule_attrs=all \
             --proto:default_values"
                .to_owned()
        )
    );
}

#[test]
fn a_visibility_naming_a_label_the_query_lacks_fails() {
    let universe = queried(&[
        declaring("//t:lib", Some(&["//g:absent"])),
        declaring("//t:via", Some(&["//g:held"])),
        declaring("//t:none", Some(&["//visibility:private"])),
        package_group_of("//g:held", &["//a"], &["//g:gone"]),
        declaring("//a:a", None),
    ]);
    let missing = |label: &str| Err(absent_group(label));
    assert_eq!(
        labels(&universe, "visible(//a:a, //t:lib)"),
        missing("//g:absent")
    );
    assert_eq!(
        labels(&universe, "visible(//t:none, //t:lib)"),
        missing("//g:absent"),
        "a visibility is read even where a seer shares the target's package"
    );
    assert_eq!(
        labels(&universe, "visible(//a:a, //t:via)"),
        missing("//g:gone"),
        "an include is read as the visibility naming its group"
    );
    assert_eq!(
        labels(&universe, "visible(set(), //t:lib)").unwrap(),
        ["//t:lib"]
    );
    assert_eq!(
        labels(&universe, "visible(//a:a, //t:none)").unwrap(),
        [""; 0]
    );
}

#[test]
fn a_target_whose_visibility_the_query_leaves_unstated_is_visible_to_its_package() {
    let universe = queried(&[
        generated("//t:orphan.out", "//t:absent"),
        declaring("//t:a", Some(&["//visibility:private"])),
        declaring("//c:a", None),
    ]);
    assert_eq!(visible_to(&universe, "//t:a"), ["//t:a", "//t:orphan.out"]);
    assert_eq!(visible_to(&universe, "//c:a"), [""; 0]);
}

#[test]
fn a_seer_belonging_to_no_package_sees_nothing() {
    let aquery = [action_graph(&[ActionSpec {
        label: "//p:lib",
        mnemonic: "CppCompile",
        inputs: &["tools/x.h"],
        outputs: &["bazel-out/k8-fastbuild/bin/p/lib.o"],
    }])];
    let targets = [
        declaring("//p:lib", None),
        declaring("//t:public", Some(&["//visibility:public"])),
    ];
    let joint = build_joint(Some(&aquery), None, Some(&targets), false);
    let universe = through(&joint, Sources::reading(&joint.capabilities));
    // The action and the header it reads, which belongs to no target.
    let seers = "deps(inputs('.*', //p:all), 1)";
    let value = universe.evaluate(&parse(seers).unwrap()).unwrap();
    let seer_labels: Vec<Option<&str>> = (value.ones())
        .map(|id| universe.label(Id(id as u32)))
        .collect();
    assert_eq!(seer_labels, [Some("//p:lib"), None]);
    assert_eq!(
        labels(&universe, &format!("visible({seers}, //t:public)")).unwrap(),
        [""; 0]
    );
}
