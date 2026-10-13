use std::sync::Arc;

use bazel_query_proto::blaze_query::Target;
use fixedbitset::FixedBitSet;

use super::super::parse;
use super::super::stack::{Prefixes, Stack, Value};
use super::*;
use crate::capability::Axis;
use crate::facets::{FacetsBuilder, Tri};
use crate::graph::{EdgeKind, Graph, Kind, Node};
use crate::intern::{Id, Names};
use crate::joint::build_joint;
use crate::test_support::{generated, source, string_list};

fn rule(name: &str, inputs: &[&str], tags: &[&str]) -> Target {
    crate::test_support::rule(name, "genrule", inputs, vec![string_list("tags", tags)])
}

/// A query graph: `//a:lib` compiles `//a:lib.cc`, `//a:gen` turns it into
/// `//a:gen.out`, the manual `//b:app` links both, and `//b:test` tests it.
struct Fixture {
    universe: Universe,
    names: Names,
}

impl Fixture {
    fn new() -> Self {
        let targets = [
            rule("//a:lib", &["//a:lib.cc"], &[]),
            source("//a:lib.cc"),
            rule("//a:gen", &["//a:lib"], &[]),
            generated("//a:gen.out", "//a:gen"),
            rule("//b:app", &["//a:lib", "//a:gen.out"], &["manual"]),
            rule("//b:test", &["//b:app"], &[]),
        ];
        let joint = build_joint(None, None, Some(&targets), false);
        let universe = Universe::new(
            &joint.graph,
            &joint.names,
            &Arc::new(joint.facets),
            &joint.capabilities,
        );
        Fixture {
            universe,
            names: joint.names,
        }
    }

    /// The labels of what `expr` names, sorted, or the message it fails with.
    fn query(&self, expr: &str) -> Result<Vec<String>, String> {
        let parsed = parse(expr).map_err(|e| e.message)?;
        let value = self.universe.evaluate(&parsed).map_err(|e| e.message)?;
        Ok(labels(&self.names, &value))
    }

    fn ok(&self, expr: &str) -> Vec<String> {
        self.query(expr).unwrap_or_else(|e| panic!("{expr}: {e}"))
    }

    fn error(&self, expr: &str) -> String {
        self.query(expr).expect_err(expr)
    }
}

/// The names of `value`'s members, short of the `target:` every target name
/// starts with, sorted.
fn labels(names: &Names, value: &FixedBitSet) -> Vec<String> {
    let name = |id: usize| names.resolve(Id(id as u32));
    let mut labels: Vec<String> = (value.ones())
        .map(|id| name(id).trim_start_matches("target:").to_string())
        .collect();
    labels.sort();
    labels
}

#[test]
fn a_pattern_names_its_targets_and_a_query_keeps_the_manual_ones() {
    let fixture = Fixture::new();
    assert_eq!(fixture.ok("//b:all"), ["//b:app", "//b:test"]);
    assert_eq!(fixture.ok("//a:all"), ["//a:gen", "//a:lib"]);
    assert_eq!(fixture.ok("//a:*").len(), 4);
    assert_eq!(
        fixture.error("//c:x"),
        "no package '//c' in the build",
        "a pattern resolved against the graph"
    );
    assert_eq!(
        fixture.error("//a/...:x"),
        "Invalid target pattern //a/...:x: '...' can only be used with wildcard targets"
    );
}

#[test]
fn set_operators_run_left_to_right_at_one_precedence() {
    let fixture = Fixture::new();
    assert_eq!(fixture.ok("//a:* - //a:lib ^ //a:gen"), ["//a:gen"]);
    assert_eq!(fixture.ok("//a:lib + //b:app"), ["//a:lib", "//b:app"]);
    assert_eq!(fixture.ok("//a:all intersect //a:lib"), ["//a:lib"]);
    assert_eq!(fixture.ok("set(//a:lib //b:test)"), ["//a:lib", "//b:test"]);
}

#[test]
fn a_variable_is_bound_for_the_body_of_its_let_alone() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.ok("let x = //a:lib in $x + //b:app"),
        ["//a:lib", "//b:app"]
    );
    assert_eq!(
        fixture.ok("let x = //a:lib in let x = //b:app in $x"),
        ["//b:app"]
    );
    assert_eq!(fixture.error("$y"), "undefined variable 'y'");
    assert_eq!(
        fixture.error("(let x = //a:lib in $x) + $x"),
        "undefined variable 'x'"
    );
    assert_eq!(
        fixture.error("let 1x = //a:lib in //a:lib"),
        "invalid variable name '1x' in let expression"
    );
}

#[test]
fn deps_reach_down_as_far_as_the_depth_allows() {
    let fixture = Fixture::new();
    let all = ["//a:gen", "//a:gen.out", "//a:lib", "//a:lib.cc", "//b:app"];
    assert_eq!(fixture.ok("deps(//b:app)"), all);
    assert_eq!(
        fixture.ok("deps(//b:app, 1)"),
        ["//a:gen.out", "//a:lib", "//b:app"]
    );
    assert_eq!(fixture.ok("deps(//b:app, 0)"), ["//b:app"]);
    assert!(fixture.ok("deps(//b:app, \"-1\")").is_empty());
}

#[test]
fn rdeps_reach_up_within_their_universe() {
    let fixture = Fixture::new();
    assert_eq!(fixture.ok("allrdeps(//a:lib.cc)").len(), 6);
    assert_eq!(
        fixture.ok("rdeps(//b:app, //a:lib, 1)"),
        ["//a:gen", "//a:lib", "//b:app"]
    );
    assert_eq!(fixture.ok("rdeps(//b:app, //a:lib.cc)").len(), 5);
    assert!(fixture.ok("rdeps(//a:gen, //b:app)").is_empty(), "outside");
}

#[test]
fn all_paths_keep_every_node_between_their_ends() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.ok("allpaths(//b:test, //a:lib)"),
        ["//a:gen", "//a:gen.out", "//a:lib", "//b:app", "//b:test"]
    );
    assert!(fixture.ok("allpaths(//a:lib, //b:test)").is_empty());
}

#[test]
fn some_path_takes_one_of_fewest_edges() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.ok("somepath(//b:test, //a:lib.cc)"),
        ["//a:lib", "//a:lib.cc", "//b:app", "//b:test"]
    );
    assert_eq!(fixture.ok("somepath(//a:lib, //a:lib)"), ["//a:lib"]);
    assert!(fixture.ok("somepath(//a:lib, //b:test)").is_empty());
}

#[test]
fn some_takes_the_first_by_label_and_fails_on_nothing() {
    let fixture = Fixture::new();
    assert_eq!(fixture.ok("some(//a:*)"), ["//a:gen"]);
    assert_eq!(fixture.ok("some(//a:*, 2)"), ["//a:gen", "//a:gen.out"]);
    assert_eq!(fixture.ok("some(//b:all, 9)").len(), 2);
    assert_eq!(fixture.error("some(set())"), "argument set is empty");
    assert_eq!(fixture.error("some(//a:lib, 0)"), "argument set is empty");
}

#[test]
fn siblings_and_same_package_rdeps_stay_in_the_package() {
    let fixture = Fixture::new();
    assert_eq!(fixture.ok("siblings(//a:lib)").len(), 4);
    assert_eq!(fixture.ok("siblings(//a:lib + //b:app)").len(), 6);
    assert_eq!(fixture.ok("same_pkg_direct_rdeps(//a:lib)"), ["//a:gen"]);
    assert_eq!(
        fixture.ok("same_pkg_direct_rdeps(//a:gen)"),
        ["//a:gen.out"]
    );
}

#[test]
fn a_function_reading_what_the_graph_lacks_says_what_it_needs() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.error("visible(//a:lib, //b:app)"),
        "visible() needs each target's visibility, which bviz does not load into its graph"
    );
}

#[test]
fn attr_reads_tags_as_bazel_prints_a_list() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.ok("attr(tags, '^\\[manual\\]$', //...)"),
        ["//b:app"]
    );
    assert_eq!(
        fixture.ok("attr(tags, '^\\[\\]$', //...)"),
        ["//a:gen", "//a:lib", "//b:test"]
    );
    assert_eq!(
        fixture.ok("attr(name, '^lib$', //...:*)"),
        ["//a:lib"],
        "a file target carries no attributes"
    );
}

/// What `kind(rust_library, ...)` and `kind(alias, ...)` name of the
/// dependencies of the alias `@crates//:x-1.0` in `joint`.
fn kinds_behind_an_alias(joint: crate::joint::Joint) -> [Vec<String>; 2] {
    let universe = Universe::new(
        &joint.graph,
        &joint.names,
        &Arc::new(joint.facets),
        &joint.capabilities,
    );
    let query = |expr| configured_query(&universe, &joint.names, expr).unwrap();
    [
        query("kind(rust_library, deps(@crates//:x-1.0))"),
        query("kind(alias, deps(@crates//:x-1.0))"),
    ]
}

#[test]
fn kind_matches_an_alias_as_an_alias_rule_and_not_as_its_actual() {
    use crate::test_support::{configuration, configured};
    use bazel_query_proto::blaze_query::ConfiguredRuleInput;
    let alias = || {
        let mut alias =
            crate::test_support::rule("@crates//:x-1.0", "alias", &["@crates__x//:x"], Vec::new());
        alias.rule.as_mut().unwrap().configured_rule_input = vec![ConfiguredRuleInput {
            label: Some("@crates__x//:x".to_string()),
            configuration_id: Some(1),
            ..Default::default()
        }];
        alias
    };
    let actual = || crate::test_support::rule("@crates__x//:x", "rust_library", &[], Vec::new());
    assert_eq!(
        kinds_behind_an_alias(build_joint(None, None, Some(&[alias(), actual()]), false)),
        [["@crates__x//:x"], ["@crates//:x-1.0"]]
    );
    let cquery = [bazel_query_proto::analysis::CqueryResult {
        results: vec![configured(alias(), 1), configured(actual(), 1)],
        configurations: vec![configuration(1, "aa11", "k8-fastbuild", false)],
    }];
    assert_eq!(
        kinds_behind_an_alias(build_joint(None, Some(&cquery), None, false)),
        [
            ["@crates__x//:x", "ct:@crates__x//:x#aa11"],
            ["@crates//:x-1.0", "ct:@crates//:x-1.0#aa11"]
        ]
    );
}

#[test]
fn a_pattern_the_regex_crate_rejects_fails_naming_the_construct() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.error("filter('a(?=b)', //nowhere:x)"),
        "illegal 'filter' pattern regexp 'a(?=b)': look-around, including look-ahead and \
         look-behind, is not supported",
        "the pattern fails before the expression is evaluated"
    );
    assert_eq!(
        fixture.error("kind('(a)\\1', //a:lib)"),
        "illegal 'kind' pattern regexp '(a)\\1': backreferences are not supported"
    );
}

#[test]
fn a_pattern_matches_ascii_word_characters_alone() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.ok("filter('^//a:\\w+$', //a:*)"),
        ["//a:gen", "//a:lib"]
    );
}

#[test]
fn attr_names_an_attribute_bviz_does_not_load_or_the_inputs_left_out() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.error("attr(srcs, x, //a:lib)"),
        "attr() needs each rule's 'srcs' attribute, which bviz does not load into its graph"
    );
    let bare = [crate::test_support::rule(
        "//a:lib",
        "genrule",
        &[],
        Vec::new(),
    )];
    let joint = build_joint(None, None, Some(&bare), false);
    let universe = Universe::new(
        &joint.graph,
        &joint.names,
        &Arc::new(joint.facets),
        &joint.capabilities,
    );
    let error = universe
        .evaluate(&parse("attr(tags, manual, //a:lib)").unwrap())
        .unwrap_err();
    assert_eq!(
        error.message,
        "attr() needs each rule's tags, which the graph's inputs state only under \
         --proto:output_rule_attrs=all"
    );
}

/// A cquery of `//c:lib`, built in the top-level configuration and in two
/// exec configurations whose checksums share a prefix, and of `//c:in.txt`,
/// built in none; or, without `tools`, of `//c:lib` built in the top-level
/// configuration alone.
fn configured(tools: bool) -> (Universe, Names) {
    use crate::test_support::{configuration, configured, rule};
    let mut configurations = vec![configuration(1, "aa11", "k8-fastbuild", false)];
    let mut results = vec![
        configured(rule("//c:lib", "genrule", &[], Vec::new()), 1),
        configured(source("//c:in.txt"), 0),
    ];
    if tools {
        configurations.push(configuration(2, "bb22", "k8-opt-exec-ST-1", true));
        configurations.push(configuration(3, "bb33", "k8-opt-exec-ST-2", true));
        results.push(configured(rule("//c:lib", "genrule", &[], Vec::new()), 3));
        results.push(configured(rule("//c:lib", "genrule", &[], Vec::new()), 2));
    }
    let cquery = [bazel_query_proto::analysis::CqueryResult {
        results,
        configurations,
    }];
    let joint = build_joint(None, Some(&cquery), None, false);
    let universe = Universe::new(
        &joint.graph,
        &joint.names,
        &Arc::new(joint.facets),
        &joint.capabilities,
    );
    (universe, joint.names)
}

fn configured_query(universe: &Universe, names: &Names, expr: &str) -> Result<Vec<String>, String> {
    let value = universe
        .evaluate(&parse(expr).unwrap())
        .map_err(|e| e.message)?;
    Ok(labels(names, &value))
}

#[test]
fn config_names_a_configuration_by_a_unique_checksum_prefix() {
    let (universe, names) = configured(true);
    let query = |expr| configured_query(&universe, &names, expr);
    assert_eq!(query("config(//c:lib, bb3)").unwrap(), ["ct://c:lib#bb33"]);
    assert_eq!(
        query("config(//c:lib, bb)").unwrap_err(),
        "Configuration ID 'bb' is ambiguous.\n\
         'bb' is a prefix of multiple configurations:\n bb22\n bb33\n\
         \n\
         Use a longer prefix to uniquely identify one configuration."
    );
    assert_eq!(
        query("config(//c:lib, host)").unwrap_err(),
        "'host' configuration no longer exists. Use a specific configuration hash instead"
    );
    assert_eq!(
        query("config(//c:lib - //c:lib, host)").unwrap_err(),
        "No target (in) (//c:lib - //c:lib) could be found in the 'host' configuration",
        "an empty value names nothing before the configuration is read"
    );
    assert_eq!(query("config(//c:*, null)").unwrap(), ["//c:in.txt"]);
}

#[test]
fn config_target_leaves_out_a_configuration_only_a_dependency_is_built_in() {
    use crate::test_support::{configuration, configured, rule};
    use bazel_query_proto::blaze_query::ConfiguredRuleInput;
    // //c:app, built for the host platform, depends on //c:lib built for
    // another platform, whose configuration's mnemonic carries no `ST-`.
    let mut app = rule("//c:app", "genrule", &["//c:lib"], Vec::new());
    app.rule.as_mut().unwrap().configured_rule_input = vec![ConfiguredRuleInput {
        label: Some("//c:lib".to_string()),
        configuration_id: Some(2),
        ..Default::default()
    }];
    let cquery = [bazel_query_proto::analysis::CqueryResult {
        results: vec![
            configured(app, 1),
            configured(rule("//c:lib", "genrule", &[], Vec::new()), 1),
            configured(rule("//c:lib", "genrule", &[], Vec::new()), 2),
        ],
        configurations: vec![
            configuration(1, "aa11", "k8-fastbuild", false),
            configuration(2, "bb22", "aarch64-fastbuild", false),
        ],
    }];
    let joint = build_joint(None, Some(&cquery), None, false);
    let universe = Universe::new(
        &joint.graph,
        &joint.names,
        &Arc::new(joint.facets),
        &joint.capabilities,
    );
    assert_eq!(
        configured_query(&universe, &joint.names, "config(//c:lib, target)").unwrap(),
        ["ct://c:lib#aa11"]
    );
}

/// A cquery under `--transitions` of `//c:t`, built in configuration `aa11`,
/// and `//c:app`, built in `bb22`, each depending on `//c:lib` in its own
/// configuration, and `//c:t` also on `//c:app` in `aa11`.
fn two_top_level_configurations() -> (Universe, Names) {
    use crate::test_support::{configuration, configured, rule};
    use bazel_query_proto::blaze_query::ConfiguredRuleInput;
    let depending = |name: &str, deps: &[(&str, u32)]| {
        let labels: Vec<&str> = deps.iter().map(|&(label, _)| label).collect();
        let mut target = rule(name, "genrule", &labels, Vec::new());
        target.rule.as_mut().unwrap().configured_rule_input = (deps.iter())
            .map(|&(label, config)| ConfiguredRuleInput {
                label: Some(label.to_string()),
                configuration_id: Some(config),
                ..Default::default()
            })
            .collect();
        target
    };
    let cquery = [bazel_query_proto::analysis::CqueryResult {
        results: vec![
            configured(depending("//c:t", &[("//c:lib", 1), ("//c:app", 1)]), 1),
            configured(depending("//c:app", &[("//c:lib", 1)]), 1),
            configured(depending("//c:app", &[("//c:lib", 2)]), 2),
            configured(depending("//c:lib", &[]), 1),
            configured(depending("//c:lib", &[]), 2),
        ],
        configurations: vec![
            configuration(1, "aa11", "k8-fastbuild", false),
            configuration(2, "bb22", "aarch64-fastbuild", false),
        ],
    }];
    let joint = build_joint(None, Some(&cquery), None, false);
    let universe = Universe::new(
        &joint.graph,
        &joint.names,
        &Arc::new(joint.facets),
        &joint.capabilities,
    );
    (universe, joint.names)
}

#[test]
fn config_target_takes_a_dependency_in_the_first_top_level_configuration() {
    let (universe, names) = two_top_level_configurations();
    assert_eq!(
        configured_query(&universe, &names, "config(//c:lib, target)").unwrap(),
        ["ct://c:lib#aa11"]
    );
}

#[test]
fn config_target_takes_a_top_level_target_in_its_own_configuration() {
    let (universe, names) = two_top_level_configurations();
    assert_eq!(
        configured_query(&universe, &names, "config(//c:app, target)").unwrap(),
        ["ct://c:app#bb22"]
    );
}

#[test]
fn config_anyexec_takes_the_first_tool_configuration_by_checksum() {
    let (universe, names) = configured(true);
    assert_eq!(
        configured_query(&universe, &names, "config(//c:lib, anyexec)").unwrap(),
        ["ct://c:lib#bb22"]
    );
    let (universe, names) = configured(false);
    let error = configured_query(&universe, &names, "config(//c:lib, anyexec)").unwrap_err();
    assert!(
        error.starts_with("Unable to identify 'exec' configuration for //c:lib\n"),
        "{error}"
    );
}

/// An aquery of `//p:gen`, whose Genrule reads `p/in.txt` and writes
/// `p/out.h` and `p/out.c`, and of `//p:lib`, whose CppCompile reads
/// `p/out.c` and a header of another repository, which the graph leaves out.
fn actions() -> (Universe, Names) {
    use crate::test_support::{ActionSpec, action_graph};
    let out = |name: &str| format!("bazel-out/k8-fastbuild/bin/p/{name}");
    let (header, source) = (out("out.h"), out("out.c"));
    let aquery = [action_graph(&[
        ActionSpec {
            label: "//p:gen",
            mnemonic: "Genrule",
            inputs: &["p/in.txt"],
            outputs: &[&header, &source],
        },
        ActionSpec {
            label: "//p:lib",
            mnemonic: "CppCompile",
            inputs: &[&source, "external/repo+/include/x.h"],
            outputs: &[&out("lib.o")],
        },
    ])];
    let joint = build_joint(Some(&aquery), None, None, false);
    let universe = Universe::new(
        &joint.graph,
        &joint.names,
        &Arc::new(joint.facets),
        &joint.capabilities,
    );
    (universe, joint.names)
}

#[test]
fn config_target_leaves_out_a_configuration_an_aquery_builds_only_dependencies_in() {
    use crate::test_support::{ActionSpec, action_graph, configuration};
    let lib = |configuration: &str| format!("bazel-out/{configuration}/bin/p/lib.o");
    let (built_for_bin, built_for_host) = (lib("aarch64-fastbuild"), lib("k8-fastbuild"));
    let mut aquery = action_graph(&[
        ActionSpec {
            label: "//p:lib",
            mnemonic: "CppCompile",
            inputs: &["p/lib.cc"],
            outputs: &[&built_for_bin],
        },
        ActionSpec {
            label: "//p:lib",
            mnemonic: "CppCompile",
            inputs: &["p/lib.cc"],
            outputs: &[&built_for_host],
        },
        ActionSpec {
            label: "//p:bin",
            mnemonic: "CppLink",
            inputs: &[&built_for_bin],
            outputs: &["bazel-out/k8-fastbuild/bin/p/bin"],
        },
    ]);
    let other = configuration(2, "a0a0", "aarch64-fastbuild", false);
    aquery.actions[0].configuration_id = other.id;
    aquery.configuration.push(other);
    let joint = build_joint(Some(&[aquery]), None, None, false);
    let universe = Universe::new(
        &joint.graph,
        &joint.names,
        &Arc::new(joint.facets),
        &joint.capabilities,
    );
    assert_eq!(
        configured_query(&universe, &joint.names, "config(//p:lib, target)").unwrap(),
        ["ct://p:lib#c0ffee"]
    );
}

/// An aquery of `actions` with the query `targets` naming its labeled outputs,
/// each action of `in_a0a0` built in configuration `a0a0` and every other in
/// `c0ffee`.
fn labeled_outputs(
    actions: &[crate::test_support::ActionSpec],
    in_a0a0: &[usize],
    targets: &[Target],
) -> (Universe, Names) {
    use crate::test_support::{action_graph, configuration};
    let mut aquery = action_graph(actions);
    let other = configuration(2, "a0a0", "aarch64-fastbuild", false);
    for &at in in_a0a0 {
        aquery.actions[at].configuration_id = other.id;
    }
    aquery.configuration.push(other);
    let joint = build_joint(Some(&[aquery]), None, Some(targets), false);
    let universe = Universe::new(
        &joint.graph,
        &joint.names,
        &Arc::new(joint.facets),
        &joint.capabilities,
    );
    (universe, joint.names)
}

#[test]
fn config_target_takes_the_configured_target_of_a_top_level_rule_writing_a_labeled_output() {
    use crate::test_support::ActionSpec;
    let (universe, names) = labeled_outputs(
        &[
            ActionSpec {
                label: "//p:t",
                mnemonic: "JavaBuild",
                inputs: &["p/T.java"],
                outputs: &["bazel-out/k8-fastbuild/bin/p/t.jar"],
            },
            ActionSpec {
                label: "//p:t",
                mnemonic: "JavaBuild",
                inputs: &["p/T.java"],
                outputs: &["bazel-out/aarch64-fastbuild/bin/p/t.jar"],
            },
            ActionSpec {
                label: "//p:app",
                mnemonic: "Link",
                inputs: &["bazel-out/aarch64-fastbuild/bin/p/t.jar"],
                outputs: &["bazel-out/aarch64-fastbuild/bin/p/app"],
            },
        ],
        &[1, 2],
        &[
            rule("//p:t", &[], &[]),
            generated("//p:t.jar", "//p:t"),
            rule("//p:app", &["//p:t.jar"], &[]),
        ],
    );
    let query = |expr| configured_query(&universe, &names, expr).unwrap();
    assert_eq!(query("config(//p:t, target)"), ["ct://p:t#c0ffee"]);
    assert_eq!(query("config(//p:t.jar, target)"), ["ct://p:t.jar#c0ffee"]);
}

#[test]
fn config_target_leaves_out_a_configuration_whose_labeled_output_nothing_reads() {
    use crate::test_support::ActionSpec;
    let (universe, names) = labeled_outputs(
        &[
            ActionSpec {
                label: "//p:gen",
                mnemonic: "Gen",
                inputs: &[],
                outputs: &[
                    "bazel-out/aarch64-fastbuild/bin/p/out.txt",
                    "bazel-out/aarch64-fastbuild/bin/p/gen.o",
                ],
            },
            ActionSpec {
                label: "//p:gen",
                mnemonic: "Gen",
                inputs: &[],
                outputs: &[
                    "bazel-out/k8-fastbuild/bin/p/out.txt",
                    "bazel-out/k8-fastbuild/bin/p/gen.o",
                ],
            },
            ActionSpec {
                label: "//p:bin",
                mnemonic: "Link",
                inputs: &[
                    "bazel-out/aarch64-fastbuild/bin/p/gen.o",
                    "bazel-out/k8-fastbuild/bin/p/gen.o",
                ],
                outputs: &["bazel-out/k8-fastbuild/bin/p/bin"],
            },
        ],
        &[0],
        &[
            rule("//p:gen", &[], &[]),
            generated("//p:out.txt", "//p:gen"),
            rule("//p:bin", &["//p:gen"], &[]),
        ],
    );
    let query = |expr| configured_query(&universe, &names, expr).unwrap();
    assert_eq!(query("config(//p:gen, target)"), ["ct://p:gen#c0ffee"]);
    assert_eq!(
        query("config(//p:out.txt, target)"),
        ["ct://p:out.txt#c0ffee"]
    );
}

#[test]
fn config_target_takes_a_top_level_rule_generating_a_file_in_its_own_configuration() {
    use crate::test_support::{configuration, configured};
    use bazel_query_proto::blaze_query::ConfiguredRuleInput;
    let mut app = crate::test_support::rule("//p:app", "genrule", &["//p:t"], Vec::new());
    app.rule.as_mut().unwrap().configured_rule_input = vec![ConfiguredRuleInput {
        label: Some("//p:t".to_string()),
        configuration_id: Some(1),
        ..Default::default()
    }];
    let cquery = [bazel_query_proto::analysis::CqueryResult {
        results: vec![
            configured(rule("//p:t", &[], &[]), 1),
            configured(rule("//p:t", &[], &[]), 2),
            configured(generated("//p:t.jar", "//p:t"), 2),
            configured(app, 1),
        ],
        configurations: vec![
            configuration(1, "aa11", "k8-fastbuild", false),
            configuration(2, "bb22", "aarch64-fastbuild", false),
        ],
    }];
    let joint = build_joint(None, Some(&cquery), None, false);
    let universe = Universe::new(
        &joint.graph,
        &joint.names,
        &Arc::new(joint.facets),
        &joint.capabilities,
    );
    assert_eq!(
        configured_query(&universe, &joint.names, "config(//p:t, target)").unwrap(),
        ["ct://p:t#bb22"]
    );
}

#[test]
fn an_aquery_filter_keeps_the_actions_matching_in_full() {
    let (universe, names) = actions();
    let query = |expr| configured_query(&universe, &names, expr);
    let genrule = "action:bazel-out/k8-fastbuild/bin/p/out.h";
    let compile = "action:bazel-out/k8-fastbuild/bin/p/lib.o";
    assert_eq!(query("mnemonic(Genrule, //p:all)").unwrap(), [genrule]);
    assert!(query("mnemonic(Gen, //p:all)").unwrap().is_empty());
    assert_eq!(query("mnemonic('Gen|Cpp.*', //p:all)").unwrap(), [compile]);
    assert_eq!(query("inputs('.*\\.c', //p:all)").unwrap(), [compile]);
    assert_eq!(query("outputs('.*/out\\.c', //p:all)").unwrap(), [genrule]);
    assert_eq!(query("inputs('p/in.txt', //p:all)").unwrap(), [genrule]);
}

/// A scope over an action holds the files it writes, which stand beside it in
/// its configured target, and a scope over a target holds all it holds.
/// What a scope over `set` holds where no chip denies anything.
fn undenied_scope(universe: &Universe, set: FixedBitSet) -> FixedBitSet {
    universe.scope_nodes(&Value {
        set: Arc::new(set),
        denied: Arc::new(universe.empty()),
    })
}

/// Walking up from one node reads the scope as the whole scope does, for
/// every node, under stacks that deny what they take out.
#[test]
fn a_scope_holds_a_node_as_walking_up_from_it_reads() {
    let (universe, _) = actions();
    let stacks: [&[&str]; 5] = [
        &["//p:gen"],
        &["mnemonic(Genrule, //p:gen)"],
        &["//p:all", "- mnemonic(CppCompile, //p:all)"],
        &["//p:all", "- //p:lib", "+ mnemonic(CppCompile, //p:lib)"],
        &["deps(//p:lib)", "except //p:in.txt"],
    ];
    for chips in stacks {
        let mut stack = Stack::default();
        for chip in chips {
            stack.push(chip);
        }
        let evaluation = stack.evaluate(&universe, &Prefixes::default());
        let value = evaluation.value.expect("a stack naming a set");
        let scope = universe.scope_nodes(&value);
        for id in 0..universe.ids() {
            assert_eq!(
                universe.scope_holds(&value, Id(id as u32)),
                scope.contains(id),
                "{chips:?} at {id}"
            );
        }
    }
}

#[test]
fn a_scope_holds_what_a_node_holds_and_what_an_action_writes() {
    let (universe, names) = actions();
    let scope = |expr: &str| {
        let value = universe.evaluate(&parse(expr).unwrap()).unwrap();
        labels(&names, &undenied_scope(&universe, value))
    };
    let genrule = scope("mnemonic(Genrule, //p:gen)");
    assert_eq!(genrule.len(), 3, "{genrule:?}");
    assert!(genrule.contains(&"action:bazel-out/k8-fastbuild/bin/p/out.h".to_string()));
    assert_eq!(
        genrule
            .iter()
            .filter(|name| name.starts_with("artifact"))
            .count(),
        2,
        "{genrule:?}"
    );
    let target = scope("//p:gen");
    assert!(target.len() > genrule.len(), "{target:?}");
    assert!(
        genrule.iter().all(|name| target.contains(name)),
        "{target:?}"
    );
}

#[test]
fn an_aquery_filter_may_stand_inside_another_function() {
    let (universe, names) = actions();
    let found = configured_query(&universe, &names, "deps(mnemonic(CppCompile, //p:lib))");
    assert_eq!(
        found.unwrap(),
        [
            "//p:in.txt",
            "action:bazel-out/k8-fastbuild/bin/p/lib.o",
            "action:bazel-out/k8-fastbuild/bin/p/out.h",
            "artifact3",
        ]
    );
}

#[test]
fn an_aquery_filter_fails_as_bazel_does() {
    let (universe, names) = actions();
    let query = |expr| configured_query(&universe, &names, expr);
    assert_eq!(
        query("mnemonic(Genrule)").unwrap_err(),
        "aquery filter functions (inputs, outputs, mnemonics) must have exactly 2 \
         arguments,except when --skyframe_state is used."
    );
    assert_eq!(
        query("inputs('a)(b', //p:all)").unwrap_err(),
        "Illegal 'inputs' pattern regexp 'a)(b': unopened group",
        "a pattern closing a group early is no whole-string pattern"
    );
}

#[test]
fn inputs_matches_a_file_the_graph_left_out() {
    let (universe, names) = actions();
    assert_eq!(
        configured_query(&universe, &names, "inputs('external/.*/x\\.h', //p:all)").unwrap(),
        ["action:bazel-out/k8-fastbuild/bin/p/lib.o"]
    );
}

#[test]
fn nested_inputs_patterns_match_one_file_left_out_or_not() {
    let (universe, names) = actions();
    let query = |expr| configured_query(&universe, &names, expr).unwrap();
    assert!(query("inputs('.*x\\.h', inputs('.*out\\.c', //p:all))").is_empty());
    assert_eq!(
        query("inputs('external/.*', inputs('.*\\.h', //p:all))"),
        ["action:bazel-out/k8-fastbuild/bin/p/lib.o"]
    );
}

/// An aquery of `//p:a`, whose Genrule reads `p/a.java` and `p/foo.txt` and
/// writes `a.class` and `foo.log`, and of `//p:b`, whose Genrule reads
/// `p/foo.java` and writes `foo.class`.
fn java() -> (Universe, Names) {
    use crate::test_support::{ActionSpec, action_graph};
    let out = |name: &str| format!("bazel-out/k8-fastbuild/bin/p/{name}");
    let aquery = [action_graph(&[
        ActionSpec {
            label: "//p:a",
            mnemonic: "Genrule",
            inputs: &["p/a.java", "p/foo.txt"],
            outputs: &[&out("a.class"), &out("foo.log")],
        },
        ActionSpec {
            label: "//p:b",
            mnemonic: "Genrule",
            inputs: &["p/foo.java"],
            outputs: &[&out("foo.class")],
        },
    ])];
    let joint = build_joint(Some(&aquery), None, None, false);
    let universe = Universe::new(
        &joint.graph,
        &joint.names,
        &Arc::new(joint.facets),
        &joint.capabilities,
    );
    (universe, joint.names)
}

/// Bazel's aquery gathers the filter calls nested in one another into one
/// filter (`AqueryUtils.matchesAqueryFilters`), which one file read must
/// match in full, and one file written.
#[test]
fn nested_file_filters_match_one_file_as_aquery_does() {
    let (universe, names) = java();
    let query = |expr| configured_query(&universe, &names, expr).unwrap();
    let b = ["action:bazel-out/k8-fastbuild/bin/p/foo.class"];
    assert_eq!(query("inputs('.*java', inputs('.*foo.*', //p:all))"), b);
    assert_eq!(
        query("inputs('.*java', mnemonic(Genrule, inputs('.*foo.*', //p:all)))"),
        b
    );
    assert_eq!(query("outputs('.*class', outputs('.*foo.*', //p:all))"), b);
    assert_eq!(
        query("let v = inputs('.*foo.*', //p:all) in inputs('.*java', $v)"),
        b,
        "a run reading a variable bound to another run joins it"
    );
}

#[test]
fn a_variable_holding_a_filter_run_names_its_filtered_set() {
    let (universe, names) = java();
    let query = |expr| configured_query(&universe, &names, expr).unwrap();
    assert_eq!(
        query("let v = inputs('.*foo\\.java', //p:all) in $v + //p:a"),
        query("inputs('.*foo\\.java', //p:all) + //p:a")
    );
}

/// Bazel rejects a filter call below a call of another function, so a run
/// broken by one filters apart: a bviz extension.
#[test]
fn a_filter_run_broken_by_another_function_filters_apart() {
    let (universe, names) = java();
    let found = configured_query(
        &universe,
        &names,
        "inputs('.*java', deps(inputs('.*foo.*', //p:all)))",
    );
    assert!((found.unwrap()).contains(&"action:bazel-out/k8-fastbuild/bin/p/a.class".to_string()));
}

#[test]
fn a_verbose_pattern_matches_whole_strings_with_its_comment_left_out() {
    let (universe, names) = actions();
    assert_eq!(
        configured_query(
            &universe,
            &names,
            "mnemonic('(?x)Cpp # a comment', //p:all)"
        )
        .unwrap(),
        Vec::<String>::new(),
        "the whole mnemonic must match"
    );
    assert_eq!(
        configured_query(
            &universe,
            &names,
            "mnemonic('(?x)Cpp Compile # spaced', //p:all)"
        )
        .unwrap(),
        ["action:bazel-out/k8-fastbuild/bin/p/lib.o"]
    );
}

#[test]
fn an_aquery_filter_names_the_data_a_graph_lacks() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.error("mnemonic(Genrule, //a:lib)"),
        "mnemonic() requires an aquery"
    );
    let aquery = [bazel_query_proto::analysis::ActionGraphContainer {
        targets: vec![bazel_query_proto::analysis::Target {
            id: 1,
            label: "//p:gen".to_string(),
            ..Default::default()
        }],
        actions: vec![bazel_query_proto::analysis::Action {
            target_id: 1,
            mnemonic: "Genrule".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    }];
    let joint = build_joint(Some(&aquery), None, None, false);
    let universe = Universe::new(
        &joint.graph,
        &joint.names,
        &Arc::new(joint.facets),
        &joint.capabilities,
    );
    let query = |expr| configured_query(&universe, &joint.names, expr);
    assert_eq!(query("mnemonic(Genrule, //p:all)").unwrap().len(), 1);
    assert_eq!(
        query("outputs('.*', //p:all)").unwrap_err(),
        "outputs() requires an aquery with --include_artifacts"
    );
}

/// Package `a` holds targets `//a:t` and `//a:u`, holding configured targets
/// `ct:t` and `ct:u`, which hold actions `act:t` and `act:u`. `//a:t` feeds
/// `//a:u` by a label edge, `ct:t` feeds `ct:u`, and `act:t` feeds `act:u`.
fn levels() -> (Universe, Names) {
    let mut graph = Graph::new();
    let mut names = Names::default();
    let mut builder = FacetsBuilder::default();
    let package = names.intern("pkg:a");
    graph.add_node(Node::new(package, "//a", Kind::Package, None));
    let mut ids = Vec::new();
    for name in ["t", "u"] {
        let target = names.intern(&format!("target://a:{name}"));
        let configured = names.intern(&format!("ct:{name}"));
        let action = names.intern(&format!("act:{name}"));
        graph.add_node(Node::new(target, name, Kind::Target, Some(package)));
        graph.add_node(Node::new(
            configured,
            name,
            Kind::ConfiguredTarget,
            Some(target),
        ));
        graph.add_node(Node::new(action, name, Kind::Action, Some(configured)));
        builder.describe_label(target, Some(package), &format!("//a:{name}"));
        builder.facets().inherit(configured, target);
        builder.facets().inherit(action, configured);
        ids.push([target, configured, action]);
    }
    for (level, kind) in [EdgeKind::Query, EdgeKind::Cquery, EdgeKind::Aquery]
        .into_iter()
        .enumerate()
    {
        graph.add_edge(ids[0][level], ids[1][level], kind);
    }
    let (facets, capabilities) = builder.seal();
    (
        Universe::new(&graph, &names, &Arc::new(facets), &capabilities),
        names,
    )
}

fn value(universe: &Universe, names: &Names, members: &[&str]) -> FixedBitSet {
    let mut value = universe.empty();
    value.extend(
        members
            .iter()
            .map(|name| names.get(name).unwrap().0 as usize),
    );
    value
}

#[test]
fn a_target_literal_stands_for_its_configured_targets_too() {
    let (universe, names) = levels();
    let named = universe.evaluate(&parse("//a:t").unwrap()).unwrap();
    assert_eq!(labels(&names, &named), ["//a:t", "ct:t"]);
}

#[test]
fn deps_of_a_target_reach_through_what_it_holds() {
    let (universe, names) = levels();
    let found = universe.evaluate(&parse("deps(//a:u)").unwrap()).unwrap();
    assert_eq!(
        labels(&names, &found),
        ["//a:t", "//a:u", "act:t", "act:u", "ct:t", "ct:u"]
    );
}

#[test]
fn a_walk_from_a_deeper_level_climbs_no_higher() {
    let (universe, names) = levels();
    let seeds = value(&universe, &names, &["ct:u"]);
    assert_eq!(
        labels(&names, &universe.deps(&seeds, None)),
        ["act:t", "act:u", "ct:t", "ct:u"]
    );
    let seeds = value(&universe, &names, &["act:t"]);
    assert_eq!(
        labels(&names, &universe.rdeps(None, &seeds, None)),
        ["act:t", "act:u"]
    );
    let seeds = value(&universe, &names, &["ct:t", "act:t"]);
    assert_eq!(
        labels(&names, &universe.rdeps(None, &seeds, None)),
        ["act:t", "act:u", "ct:t", "ct:u"],
        "each level walks apart"
    );
}

#[test]
fn a_configured_target_and_an_action_match_as_their_target_does() {
    let (universe, names) = levels();
    let all = value(
        &universe,
        &names,
        &["target://a:t", "ct:t", "act:t", "ct:u"],
    );
    let regex = universe.regex(Function::Filter, ":t$").unwrap();
    assert_eq!(
        labels(&names, &universe.filter(&regex, &all)),
        ["//a:t", "act:t", "ct:t"]
    );
    let regex = universe.regex(Function::Kind, "^rule$").unwrap();
    assert_eq!(labels(&names, &universe.kind(&regex, &all)).len(), 4);
}

#[test]
fn same_package_rdeps_of_an_action_is_an_error() {
    let (universe, names) = levels();
    let seeds = value(&universe, &names, &["act:t"]);
    assert!(universe.same_package_direct_rdeps(&seeds).is_err());
    let seeds = value(&universe, &names, &["ct:t"]);
    let found = universe.same_package_direct_rdeps(&seeds).unwrap();
    assert_eq!(labels(&names, &found), ["ct:u"]);
}

/// A joint graph drawn from `rng`: packages of targets, package groups,
/// source files and generated files, each target holding configured targets
/// holding actions and the files they write, a generated file promoted to a
/// configured copy holding its file, and group boxes merging files. Label edges join the targets and files, aquery edges
/// the files and actions, and configured edges either join configured
/// targets or leave a source or generated file for one, as cquery states
/// them under `--transitions`, or leave a dependency's target or file for the
/// configured target depending on it, as it states them without. Edges may close cycles.
fn joint(rng: &mut fastrand::Rng) -> (Universe, Names) {
    let mut graph = Graph::new();
    let mut names = Names::default();
    let mut builder = FacetsBuilder::default();
    let transitions = rng.bool();
    let (mut labeled, mut sources, mut leaves, mut cts, mut files, mut actions) = (
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let add = |names: &mut Names, graph: &mut Graph, name: String, kind, parent| {
        let id = names.intern(&name);
        graph.add_node(Node::new(id, name, kind, parent));
        id
    };
    for p in 0..rng.usize(1..4) {
        let package = add(
            &mut names,
            &mut graph,
            format!("pkg:p{p}"),
            Kind::Package,
            None,
        );
        for t in 0..rng.usize(1..5) {
            let kind = match rng.u8(..7) {
                0 => Kind::Source,
                1 => Kind::Artifact,
                2 => Kind::TargetGroup,
                _ => Kind::Target,
            };
            let label = format!("//p{p}:t{t}");
            let target = add(
                &mut names,
                &mut graph,
                format!("target:{label}"),
                kind,
                Some(package),
            );
            builder.describe_label(target, Some(package), &label);
            labeled.push(target);
            if kind == Kind::Source {
                sources.push(target);
            }
            if matches!(kind, Kind::Source | Kind::Artifact) {
                leaves.push(target);
            }
            let copies = match kind {
                Kind::Source | Kind::TargetGroup => 0,
                Kind::Artifact => rng.usize(..2),
                _ => rng.usize(1..3),
            };
            for c in 0..copies {
                let ct = add(
                    &mut names,
                    &mut graph,
                    format!("ct:{label}#{c}"),
                    Kind::ConfiguredTarget,
                    Some(target),
                );
                builder.facets().inherit(ct, target);
                cts.push(ct);
                // Files a quotient pass merged sit in a group box of their own.
                let group = rng.u8(..4) == 0;
                let holder = match group {
                    true => add(
                        &mut names,
                        &mut graph,
                        format!("group:{label}#{c}"),
                        Kind::MergeGroup,
                        Some(ct),
                    ),
                    false => ct,
                };
                for a in 0..rng.usize(..3) {
                    let held = match kind {
                        Kind::Artifact => Kind::Artifact,
                        _ if a % 2 == 0 && !group => Kind::Action,
                        _ => Kind::Artifact,
                    };
                    let node = add(
                        &mut names,
                        &mut graph,
                        format!("{label}#{c}/{a}"),
                        held,
                        Some(holder),
                    );
                    builder.facets().inherit(node, ct);
                    match held {
                        Kind::Action => actions.push(node),
                        _ => files.push(node),
                    }
                }
            }
        }
        if rng.u8(..3) == 0 {
            let group = add(
                &mut names,
                &mut graph,
                format!("group:p{p}"),
                Kind::MergeGroup,
                Some(package),
            );
            for m in 0..rng.usize(1..3) {
                let label = format!("//p{p}:m{m}");
                let source = add(
                    &mut names,
                    &mut graph,
                    format!("target:{label}"),
                    Kind::Source,
                    Some(group),
                );
                builder.describe_label(source, Some(package), &label);
                labeled.push(source);
                sources.push(source);
                leaves.push(source);
            }
        }
    }
    let pick = |rng: &mut fastrand::Rng, from: &[Id]| from[rng.usize(..from.len())];
    for _ in 0..rng.usize(..2 * labeled.len()) {
        let (from, to) = (pick(rng, &labeled), pick(rng, &labeled));
        graph.add_edge(from, to, EdgeKind::Query);
    }
    let configured_edges = match cts.is_empty() {
        true => 0,
        false => rng.usize(..2 * cts.len()),
    };
    for _ in 0..configured_edges {
        let to = pick(rng, &cts);
        let from = match transitions {
            true if leaves.is_empty() || rng.bool() => pick(rng, &cts),
            true => pick(rng, &leaves),
            false => pick(rng, &labeled),
        };
        graph.add_edge(from, to, EdgeKind::Cquery);
    }
    let action_edges = match actions.is_empty() {
        true => 0,
        false => rng.usize(..2 * actions.len()),
    };
    for _ in 0..action_edges {
        let action = pick(rng, &actions);
        match rng.u8(..3) {
            0 if !sources.is_empty() => {
                graph.add_edge(pick(rng, &sources), action, EdgeKind::Aquery)
            }
            1 if !files.is_empty() => graph.add_edge(action, pick(rng, &files), EdgeKind::Aquery),
            _ if !files.is_empty() => graph.add_edge(pick(rng, &files), action, EdgeKind::Aquery),
            _ => {}
        }
    }
    let (facets, capabilities) = builder.seal();
    (
        Universe::new(&graph, &names, &Arc::new(facets), &capabilities),
        names,
    )
}

/// The graph seeds the mirror property is checked over.
const JOINT_SEEDS: std::ops::Range<u64> = 0..200;
/// The fewest of every this many pairs checked that must be related.
const MIN_RELATED_SHARE: usize = 10;

/// `x` is among the deps of `y` exactly where `y` is among the rdeps of `x`,
/// for every two nodes standing at one level, within any depth.
#[test]
fn deps_and_rdeps_mirror_each_other_on_joint_graphs() {
    let mut pairs = 0;
    let mut related = 0;
    for seed in JOINT_SEEDS {
        let (universe, names) = joint(&mut fastrand::Rng::with_seed(seed));
        let members: Vec<Id> = universe.members().ones().map(|id| Id(id as u32)).collect();
        let single = |id: Id| {
            let mut value = universe.empty();
            value.insert(id.0 as usize);
            value
        };
        for depth in [None, Some(0), Some(1), Some(2)] {
            let deps: Vec<FixedBitSet> = members
                .iter()
                .map(|&y| universe.deps(&single(y), depth))
                .collect();
            let rdeps: Vec<FixedBitSet> = members
                .iter()
                .map(|&x| universe.rdeps(None, &single(x), depth))
                .collect();
            for (j, &y) in members.iter().enumerate() {
                for (i, &x) in members.iter().enumerate() {
                    if universe.level(x) != universe.level(y) {
                        continue;
                    }
                    pairs += 1;
                    let down = deps[j].contains(x.0 as usize);
                    let up = rdeps[i].contains(y.0 as usize);
                    related += usize::from(down && x != y);
                    assert_eq!(
                        down,
                        up,
                        "graph {seed}, depth {depth:?}: {} in deps({}) is {down}, {} in rdeps({}) is {up}",
                        names.resolve(x),
                        names.resolve(y),
                        names.resolve(y),
                        names.resolve(x),
                    );
                }
            }
        }
    }
    // The graphs relate enough pairs that the property is no vacuous truth.
    assert!(
        related * MIN_RELATED_SHARE >= pairs,
        "{related} of {pairs} pairs related"
    );
}

/// An app whose action reads a file another target's action writes, with
/// no label or configured edge between them, as an aquery alone states it.
#[test]
fn deps_reach_the_target_whose_action_writes_a_file_read() {
    let mut graph = Graph::new();
    let mut names = Names::default();
    let mut builder = FacetsBuilder::default();
    let package = names.intern("pkg:a");
    graph.add_node(Node::new(package, "//a", Kind::Package, None));
    let mut held = |name: &str, kind, parent: Id| {
        let id = names.intern(name);
        graph.add_node(Node::new(id, name, kind, Some(parent)));
        id
    };
    let r = held("target://a:r", Kind::Target, package);
    let app = held("target://a:app", Kind::Target, package);
    let r_ct = held("ct:r", Kind::ConfiguredTarget, r);
    let app_ct = held("ct:app", Kind::ConfiguredTarget, app);
    let write = held("act:r", Kind::Action, r_ct);
    let file = held("out:r", Kind::Artifact, r_ct);
    let read = held("act:app", Kind::Action, app_ct);
    graph.add_edge(write, file, EdgeKind::Aquery);
    graph.add_edge(file, read, EdgeKind::Aquery);
    for (target, label) in [(r, "//a:r"), (app, "//a:app")] {
        builder.describe_label(target, Some(package), label);
    }
    let (facets, capabilities) = builder.seal();
    let universe = Universe::new(&graph, &names, &Arc::new(facets), &capabilities);

    let deps = universe.evaluate(&parse("deps(//a:app)").unwrap()).unwrap();
    assert!(deps.contains(r.0 as usize) && deps.contains(r_ct.0 as usize));
    let rdeps = universe
        .evaluate(&parse("allrdeps(//a:r)").unwrap())
        .unwrap();
    assert!(rdeps.contains(app.0 as usize));
}

/// A node's deps and rdeps take in those of everything it holds, while a
/// walk from a deeper level covers nothing above it, so across levels a node
/// can be among another's rdeps without the other being among its deps.
#[test]
fn a_walk_from_a_holder_covers_the_walks_from_what_it_holds() {
    let mut one_way = 0;
    for seed in JOINT_SEEDS {
        let (universe, _) = joint(&mut fastrand::Rng::with_seed(seed));
        let single = |id: Id| {
            let mut value = universe.empty();
            value.insert(id.0 as usize);
            value
        };
        let members: Vec<Id> = universe.members().ones().map(|id| Id(id as u32)).collect();
        for &y in &members {
            let Some(holder) = universe.index.parent(y) else {
                continue;
            };
            if !universe.members().contains(holder.0 as usize) {
                continue;
            }
            for depth in [None, Some(1), Some(2)] {
                let (held, holding) = (single(y), single(holder));
                let deps = universe.deps(&held, depth);
                assert!(
                    deps.is_subset(&universe.deps(&holding, depth)),
                    "graph {seed}"
                );
                let rdeps = universe.rdeps(None, &held, depth);
                assert!(
                    rdeps.is_subset(&universe.rdeps(None, &holding, depth)),
                    "graph {seed}"
                );
            }
            // The holder stands shallower than what it holds: among its own
            // rdeps that node is, and never among that node's deps.
            assert!(
                universe
                    .rdeps(None, &single(holder), None)
                    .contains(y.0 as usize)
            );
            assert!(!universe.deps(&single(y), None).contains(holder.0 as usize));
            one_way += 1;
        }
    }
    assert!(one_way > 0);
}

#[test]
fn deps_of_no_depth_name_the_argument_alone() {
    let (universe, names) = levels();
    let named = universe
        .evaluate(&parse("deps(//a:u, 0)").unwrap())
        .unwrap();
    assert_eq!(labels(&names, &named), ["//a:u", "ct:u"]);
    let named = universe
        .evaluate(&parse("allrdeps(//a:t, 0)").unwrap())
        .unwrap();
    assert_eq!(labels(&names, &named), ["//a:t", "ct:t"]);
}

/// A canonical repository name holding `+` names the repository's targets
/// written bare or quoted alike, and only that repository's, not those of
/// a name it starts.
#[test]
fn a_canonical_name_with_plus_names_its_repository_bare_or_quoted() {
    let repo = "@@rules_rs++crate+crates__elk-rs_1";
    let targets = [
        rule(&format!("{repo}//plugins/core:core"), &[], &[]),
        rule(
            &format!("{repo}//:root"),
            &[&format!("{repo}//plugins/core:core")],
            &[],
        ),
        rule("@@rules_rs++crate+crates__elk//:other", &[], &[]),
    ];
    let joint = build_joint(None, None, Some(&targets), false);
    let universe = Universe::new(
        &joint.graph,
        &joint.names,
        &Arc::new(joint.facets),
        &joint.capabilities,
    );
    let fixture = Fixture {
        universe,
        names: joint.names,
    };
    let named = [
        format!("{repo}//:root"),
        format!("{repo}//plugins/core:core"),
    ];
    assert_eq!(fixture.ok(&format!("{repo}//...")), named);
    assert_eq!(fixture.ok(&format!("\"{repo}//...\"")), named);
}

/// Package `a` holds the rules `//a:gen` and `//a:app`, each with a
/// configured target running an action, the source `//a:in.c` that `gen`'s
/// action reads, and the labeled output `//a:gen.out` with its configured copy
/// holding the file the action writes. `app`'s action reads that file and
/// writes the unlabeled `out:app`.
fn held() -> (Universe, Names) {
    let mut graph = Graph::new();
    let mut names = Names::default();
    let mut builder = FacetsBuilder::default();
    let package = names.intern("pkg:a");
    graph.add_node(Node::new(package, "//a", Kind::Package, None));
    let mut node = |name: &str, kind, parent: Id| {
        let id = names.intern(name);
        graph.add_node(Node::new(id, name, kind, Some(parent)));
        id
    };
    let rule = node("target://a:gen", Kind::Target, package);
    let app = node("target://a:app", Kind::Target, package);
    let source = node("target://a:in.c", Kind::Source, package);
    let output = node("target://a:gen.out", Kind::Artifact, package);
    let rule_ct = node("ct:gen", Kind::ConfiguredTarget, rule);
    let app_ct = node("ct:app", Kind::ConfiguredTarget, app);
    let output_ct = node("ct:gen.out", Kind::ConfiguredTarget, output);
    let generate = node("act:gen", Kind::Action, rule_ct);
    let link = node("act:app", Kind::Action, app_ct);
    let written = node("file:gen.out", Kind::Artifact, output_ct);
    let linked = node("out:app", Kind::Artifact, app_ct);
    for (target, label) in [
        (rule, "//a:gen"),
        (app, "//a:app"),
        (source, "//a:in.c"),
        (output, "//a:gen.out"),
    ] {
        builder.describe_label(target, Some(package), label);
    }
    for (child, holder) in [
        (rule_ct, rule),
        (app_ct, app),
        (output_ct, output),
        (generate, rule_ct),
        (link, app_ct),
        (written, output_ct),
        (linked, app_ct),
    ] {
        builder.facets().inherit(child, holder);
    }
    for (from, to, kind) in [
        (source, rule, EdgeKind::Query),
        (rule, output, EdgeKind::Query),
        (output, app, EdgeKind::Query),
        (rule_ct, output_ct, EdgeKind::Cquery),
        (output_ct, app_ct, EdgeKind::Cquery),
        (source, generate, EdgeKind::Aquery),
        (generate, written, EdgeKind::Aquery),
        (written, link, EdgeKind::Aquery),
        (link, linked, EdgeKind::Aquery),
    ] {
        graph.add_edge(from, to, kind);
    }
    let (facets, capabilities) = builder.seal();
    (
        Universe::new(&graph, &names, &Arc::new(facets), &capabilities),
        names,
    )
}

#[test]
fn taking_out_rules_takes_out_their_actions_and_unlabeled_outputs() {
    let (universe, names) = held();
    let left = universe
        .evaluate(&parse("deps(//a:app) - //...").unwrap())
        .unwrap();
    assert_eq!(
        labels(&names, &left),
        ["//a:gen.out", "//a:in.c", "ct:gen.out", "file:gen.out"]
    );
}

#[test]
fn keeping_rules_keeps_their_actions_and_unlabeled_outputs() {
    let (universe, names) = held();
    let kept = universe
        .evaluate(&parse("deps(//a:app) ^ //...").unwrap())
        .unwrap();
    assert_eq!(
        labels(&names, &kept),
        [
            "//a:app", "//a:gen", "act:app", "act:gen", "ct:app", "ct:gen", "out:app"
        ]
    );
}

#[test]
fn an_intersection_keeps_what_either_side_holds_of_the_other() {
    let (universe, names) = held();
    let one = |text: &str| labels(&names, &universe.evaluate(&parse(text).unwrap()).unwrap());
    assert_eq!(one("//... ^ deps(//a:app)"), one("deps(//a:app) ^ //..."));
    assert_eq!(one("deps(//a:app) ^ deps(//a:app)"), one("deps(//a:app)"));
}

#[test]
fn a_union_takes_in_no_more_than_its_operands_name() {
    let (universe, names) = held();
    let named = universe
        .evaluate(&parse("//a:in.c + //a:gen").unwrap())
        .unwrap();
    assert_eq!(labels(&names, &named), ["//a:gen", "//a:in.c", "ct:gen"]);
}

/// A query graph of `targets`.
fn query_graph(targets: &[Target]) -> Fixture {
    let joint = build_joint(None, None, Some(targets), false);
    let universe = Universe::new(
        &joint.graph,
        &joint.names,
        &Arc::new(joint.facets),
        &joint.capabilities,
    );
    Fixture {
        universe,
        names: joint.names,
    }
}

/// A query graph of `//a:app`, depending on `@@m+//m:lib` of a module and on
/// `@@m++ext+gen//e:lib` of that module's extension.
fn repositories() -> Fixture {
    query_graph(&[
        rule("//a:app", &["@@m+//m:lib", "@@m++ext+gen//e:lib"], &[]),
        rule("@@m+//m:lib", &[], &[]),
        rule("@@m++ext+gen//e:lib", &[], &[]),
    ])
}

#[test]
fn repo_category_matches_the_whole_spelling_of_each_origin() {
    let fixture = repositories();
    assert_eq!(
        fixture.ok("repo_category(main, deps(//a:app))"),
        ["//a:app"]
    );
    assert_eq!(
        fixture.ok("repo_category('module|extension', deps(//a:app))"),
        ["@@m++ext+gen//e:lib", "@@m+//m:lib"]
    );
    assert!(fixture.ok("repo_category(mod, deps(//a:app))").is_empty());
    assert_eq!(
        fixture.error("repo_category('a)(b', //a:app)"),
        "Illegal 'repo_category' pattern regexp 'a)(b': unopened group"
    );
}

#[test]
fn a_repository_is_dropped_by_its_pattern_under_either_name() {
    assert_eq!(
        repositories().ok("deps(//a:app) except @@m+//...:*"),
        ["//a:app", "@@m++ext+gen//e:lib"]
    );
    assert_eq!(
        apparent_repositories().ok("deps(//a:app) except @r//...:*"),
        ["//a:app", "@@m+//m:lib", "@m//m:api"]
    );
}

#[test]
fn repo_category_keeps_what_a_matching_target_holds() {
    let (universe, names) = held();
    let query = |expr| configured_query(&universe, &names, expr).unwrap();
    assert_eq!(
        query("repo_category(main, deps(//a:app))"),
        query("deps(//a:app)")
    );
    assert!(query("repo_category(module, deps(//a:app))").is_empty());
}

/// A query graph of `//a:app`, depending on `@@m+//m:lib` of a module, on
/// `@m//m:api`, whose apparent name that module places, and on `@r//r:lib`,
/// whose apparent name nothing places.
fn apparent_repositories() -> Fixture {
    query_graph(&[
        rule("//a:app", &["@@m+//m:lib", "@m//m:api", "@r//r:lib"], &[]),
        rule("@@m+//m:lib", &[], &[]),
        rule("@m//m:api", &[], &[]),
        rule("@r//r:lib", &[], &[]),
    ])
}

#[test]
fn repo_category_fails_where_an_apparent_name_may_hide_a_matched_origin() {
    let fixture = apparent_repositories();
    let message = "repo_category() requires --consistent_labels to tell where @r comes from";
    assert_eq!(
        fixture.error("repo_category(module, deps(//a:app))"),
        message
    );
    assert_eq!(
        fixture.error("repo_category('extension|main', deps(//a:app))"),
        message
    );
    assert_eq!(
        fixture.ok("repo_category(main, deps(//a:app))"),
        ["//a:app"]
    );
    assert_eq!(
        fixture.ok("repo_category(unknown, deps(//a:app))"),
        ["@r//r:lib"]
    );
    assert_eq!(
        fixture.ok("repo_category('.*', deps(//a:app))").len(),
        fixture.ok("deps(//a:app)").len()
    );
}

#[test]
fn repo_category_answers_where_module_evidence_places_every_apparent_name() {
    let fixture = query_graph(&[
        rule("//a:app", &["@@m+//m:lib", "@m//m:api"], &[]),
        rule("@@m+//m:lib", &[], &[]),
        rule("@m//m:api", &[], &[]),
    ]);
    assert_eq!(
        fixture.ok("repo_category(module, deps(//a:app))"),
        ["@@m+//m:lib", "@m//m:api"]
    );
}

#[test]
fn rule_category_matches_the_whole_spelling_of_a_rules_category() {
    use crate::test_support::rule;
    let targets = [
        rule("//r:test", "cc_test", &[], Vec::new()),
        rule("//r:alias", "alias", &[], Vec::new()),
        rule("//r:files", "filegroup", &[], Vec::new()),
        rule("//r:setting", "config_setting", &[], Vec::new()),
        rule("//r:lib", "cc_library", &["//r:lib.cc"], Vec::new()),
        source("//r:lib.cc"),
    ];
    let joint = build_joint(None, None, Some(&targets), false);
    let universe = Universe::new(
        &joint.graph,
        &joint.names,
        &Arc::new(joint.facets),
        &joint.capabilities,
    );
    let query = |expr| configured_query(&universe, &joint.names, expr).unwrap();
    assert_eq!(query("rule_category(test, //r:*)"), ["//r:test"]);
    assert_eq!(
        query("rule_category('alias|filegroup', //r:*)"),
        ["//r:alias", "//r:files"]
    );
    assert_eq!(query("rule_category(plumbing, //r:*)"), ["//r:setting"]);
    assert_eq!(
        query("rule_category(other, //r:*)"),
        ["//r:lib"],
        "a file is no rule"
    );
    assert!(query("rule_category(tes, //r:*)").is_empty());
}

#[test]
fn rule_category_keeps_what_a_matching_rule_holds() {
    let (universe, names) = configured(true);
    let query = |expr| configured_query(&universe, &names, expr).unwrap();
    assert_eq!(query("rule_category(other, //c:lib)"), query("//c:lib"));
    assert!(query("rule_category(test, //c:lib)").is_empty());
}

/// The output path of `name` in `//p`.
fn out(name: &str) -> String {
    format!("bazel-out/k8-fastbuild/bin/p/{name}")
}

/// An aquery of `//p:bin`, whose CppLink action links `bin` and whose
/// SourceSymlinkManifest and SymlinkTree actions wire its runfiles.
fn runfiles() -> (Universe, Names) {
    use crate::test_support::{ActionSpec, action_graph};
    let (binary, manifest, tree) = (
        out("bin"),
        out("bin.runfiles_manifest"),
        out("bin.runfiles"),
    );
    let aquery = [action_graph(&[
        ActionSpec {
            label: "//p:bin",
            mnemonic: "CppLink",
            inputs: &["p/main.o"],
            outputs: &[&binary],
        },
        ActionSpec {
            label: "//p:bin",
            mnemonic: "SourceSymlinkManifest",
            inputs: &[],
            outputs: &[&manifest],
        },
        ActionSpec {
            label: "//p:bin",
            mnemonic: "SymlinkTree",
            inputs: &[&manifest],
            outputs: &[&tree],
        },
    ])];
    let joint = build_joint(Some(&aquery), None, None, false);
    let universe = Universe::new(
        &joint.graph,
        &joint.names,
        &Arc::new(joint.facets),
        &joint.capabilities,
    );
    (universe, joint.names)
}

#[test]
fn runfiles_plumbing_keeps_the_actions_wiring_runfiles() {
    let (universe, names) = runfiles();
    let (manifest, tree) = (out("bin.runfiles_manifest"), out("bin.runfiles"));
    let query = |expr| configured_query(&universe, &names, expr).unwrap();
    let plumbing = [format!("action:{tree}"), format!("action:{manifest}")];
    assert_eq!(query("runfiles_plumbing(//p:bin)"), plumbing);
    assert_eq!(
        query("runfiles_plumbing(mnemonic(SymlinkTree, //p:bin))"),
        [format!("action:{tree}")]
    );
    assert!(query("runfiles_plumbing(mnemonic(CppLink, //p:bin))").is_empty());
    assert_eq!(
        Fixture::new().error("runfiles_plumbing(//a:lib)"),
        "runfiles_plumbing() requires an aquery"
    );
}

/// `//t:gen`, configured as `ct:target` in a target configuration and as
/// `ct:exec` in a tool one, each running an action that writes a file; with
/// `stated` false, an input left the tool configuration unstated.
fn tools(stated: bool) -> (Universe, Names) {
    let mut graph = Graph::new();
    let mut names = Names::default();
    let mut builder = FacetsBuilder::default();
    let package = names.intern("pkg:t");
    graph.add_node(Node::new(package, "//t", Kind::Package, None));
    let mut node = |name: &str, kind, parent: Id| {
        let id = names.intern(name);
        graph.add_node(Node::new(id, name, kind, Some(parent)));
        id
    };
    let target = node("target://t:gen", Kind::Target, package);
    let held = [
        ("ct:target", "act:target", "out:target", false),
        ("ct:exec", "act:exec", "out:exec", true),
    ];
    builder.describe_label(target, Some(package), "//t:gen");
    let mut writes = Vec::with_capacity(held.len());
    for (configured, action, output, tool) in held {
        let configured = node(configured, Kind::ConfiguredTarget, target);
        let action = node(action, Kind::Action, configured);
        let output = node(output, Kind::Artifact, configured);
        builder.facets().inherit(configured, target);
        let is_tool = if tool { Tri::Yes } else { Tri::No };
        let checksum = if tool { "exec" } else { "target" };
        let config = builder.facets().intern_config(checksum, None, is_tool);
        let config = Some(config).filter(|_| stated || !tool);
        builder.facets().set_config(configured, config);
        builder.observe(Axis::Configuration, config.is_some());
        for child in [action, output] {
            builder.facets().inherit(child, configured);
        }
        writes.push((action, output));
    }
    for (action, output) in writes {
        graph.add_edge(action, output, EdgeKind::Aquery);
    }
    let (facets, capabilities) = builder.seal();
    (
        Universe::new(&graph, &names, &Arc::new(facets), &capabilities),
        names,
    )
}

#[test]
fn tool_configured_keeps_what_a_tool_configuration_builds() {
    let (universe, names) = tools(true);
    let mut everything = universe.empty();
    everything.insert_range(..);
    let kept = universe.tool_configured(&everything);
    assert_eq!(labels(&names, &kept), ["act:exec", "ct:exec", "out:exec"]);
    assert_eq!(
        configured_query(&universe, &names, "tool_configured(//t:gen)").unwrap(),
        ["ct:exec"]
    );
    assert_eq!(
        configured_query(&universe, &names, "//t:gen except tool_configured(//t:gen)").unwrap(),
        ["//t:gen", "ct:target"]
    );
}

#[test]
fn tool_configured_keeps_nothing_of_a_graph_without_configurations() {
    assert!(Fixture::new().ok("tool_configured(//...)").is_empty());
}

#[test]
fn tool_configured_needs_every_configured_targets_configuration() {
    let (universe, names) = tools(false);
    assert_eq!(
        configured_query(&universe, &names, "tool_configured(//t:gen)").unwrap_err(),
        "tool_configured() requires every configured target's configuration"
    );
}

/// What the stack of `chips` scopes `universe` to, the chips at `negated`
/// negated and those at `disabled` disabled.
fn scoped(
    (universe, names): &(Universe, Names),
    chips: &[&str],
    negated: &[usize],
    disabled: &[usize],
) -> Vec<String> {
    let mut stack = Stack::default();
    let ids: Vec<_> = chips.iter().map(|chip| stack.push(chip)).collect();
    negated.iter().for_each(|&at| stack.toggle_negated(ids[at]));
    disabled
        .iter()
        .for_each(|&at| stack.toggle_enabled(ids[at]));
    let evaluation = stack.evaluate(universe, &Prefixes::default());
    let value = evaluation.value.expect("a stack naming a set");
    labels(names, &universe.scope_nodes(&value))
}

#[test]
fn a_chip_taking_out_the_tool_configurations_leaves_them_undrawn() {
    let tools = tools(true);
    let everything = scoped(&tools, &["//..."], &[], &[]);
    assert_eq!(
        everything,
        [
            "//t:gen",
            "act:exec",
            "act:target",
            "ct:exec",
            "ct:target",
            "out:exec",
            "out:target"
        ]
    );
    let built = ["//t:gen", "act:target", "ct:target", "out:target"];
    assert_eq!(
        scoped(&tools, &["//...", "tool_configured()"], &[1], &[]),
        built
    );
    assert_eq!(
        scoped(&tools, &["//...", "except tool_configured()"], &[], &[]),
        built
    );
    assert_eq!(
        scoped(&tools, &["//...", "- tool_configured()"], &[], &[]),
        built
    );
    assert_eq!(
        scoped(
            &tools,
            &[
                "//...",
                "$_ - mnemonic(NoSuch, $_) except tool_configured($_)"
            ],
            &[],
            &[]
        ),
        built,
        "a run of except and - is one operator"
    );
    assert_eq!(
        scoped(&tools, &["//...", "except tool_configured()"], &[], &[1]),
        everything,
        "a disabled chip denies nothing"
    );
}

/// A chip adding a target back adds it and its configured targets, which a
/// pattern names, and each of those stands for what it holds, so the scope
/// draws what a chip below took out of them.
#[test]
fn a_node_added_back_draws_what_it_holds_that_a_chip_below_took_out() {
    let tools = tools(true);
    assert_eq!(
        scoped(
            &tools,
            &["//...", "except tool_configured()", "+ //t:gen"],
            &[],
            &[]
        ),
        scoped(&tools, &["//..."], &[], &[])
    );
}

/// An `except` within a chip's expression narrows its value alone, so the
/// scope draws what the nodes it keeps hold.
#[test]
fn an_except_within_a_chip_denies_nothing() {
    let tools = tools(true);
    let drawn = scoped(
        &tools,
        &["//...", "deps($_ except tool_configured($_))"],
        &[],
        &[],
    );
    assert!(drawn.contains(&"act:exec".to_string()), "{drawn:?}");
    let drawn = scoped(
        &tools,
        &["//...", "$_ except tool_configured($_) + //t:gen"],
        &[],
        &[],
    );
    assert!(drawn.contains(&"act:exec".to_string()), "{drawn:?}");
}

#[test]
fn a_chip_taking_out_runfiles_plumbing_leaves_its_actions_undrawn() {
    let runfiles = runfiles();
    let drawn = scoped(
        &runfiles,
        &["//p:bin", "except runfiles_plumbing()"],
        &[],
        &[],
    );
    let actions: Vec<&String> = drawn.iter().filter(|n| n.starts_with("action:")).collect();
    assert_eq!(actions, [&format!("action:{}", out("bin"))]);
}

#[test]
fn a_chip_taking_out_every_mnemonic_leaves_no_action_drawn() {
    let runfiles = runfiles();
    let all = scoped(&runfiles, &["//p:bin"], &[], &[]);
    assert_eq!(all.iter().filter(|n| n.starts_with("action:")).count(), 3);
    let drawn = scoped(&runfiles, &["//p:bin", "except mnemonic('.*')"], &[], &[]);
    assert!(!drawn.iter().any(|n| n.starts_with("action:")), "{drawn:?}");
    assert!(drawn.iter().any(|n| n.starts_with("artifact")), "{drawn:?}");
}

/// `//p:bin`, a `cc_binary` compiling the source `//p:main.cc` to `main.o`
/// and linking that into `bin`, as query and aquery describe it.
fn linked() -> (Universe, Names) {
    use crate::test_support::{ActionSpec, action_graph};
    let (object, binary) = (out("main.o"), out("bin"));
    let targets = [
        crate::test_support::rule("//p:bin", "cc_binary", &["//p:main.cc"], Vec::new()),
        source("//p:main.cc"),
    ];
    let aquery = [action_graph(&[
        ActionSpec {
            label: "//p:bin",
            mnemonic: "CppCompile",
            inputs: &["p/main.cc"],
            outputs: &[&object],
        },
        ActionSpec {
            label: "//p:bin",
            mnemonic: "CppLink",
            inputs: &[&object],
            outputs: &[&binary],
        },
    ])];
    let joint = build_joint(Some(&aquery), None, Some(&targets), false);
    let universe = Universe::new(
        &joint.graph,
        &joint.names,
        &Arc::new(joint.facets),
        &joint.capabilities,
    );
    (universe, joint.names)
}

/// Whether `name` is the name of an output with no label of its own.
fn unlabeled_output(name: &str) -> bool {
    name.starts_with("artifact")
}

#[test]
fn kind_matches_a_file_by_its_own_class() {
    let (universe, names) = linked();
    let query = |expr| configured_query(&universe, &names, expr).unwrap();
    let outputs = query("kind('generated file', //p:bin)");
    assert_eq!(outputs.len(), 2, "{outputs:?}");
    assert!(
        outputs.iter().all(|name| unlabeled_output(name)),
        "{outputs:?}"
    );
    assert_eq!(query("kind('source file', deps(//p:bin))"), ["//p:main.cc"]);
    let rule = query("kind('cc_binary rule', deps(//p:bin))");
    assert!(!rule.iter().any(|name| unlabeled_output(name)), "{rule:?}");
    assert_eq!(query("rule_category(other, deps(//p:bin))"), rule);
}

#[test]
fn a_scope_over_a_rule_draws_the_files_it_writes() {
    let (universe, names) = linked();
    let value = universe
        .evaluate(&parse("kind('cc_binary rule', //p:bin)").unwrap())
        .unwrap();
    let drawn = labels(&names, &undenied_scope(&universe, value));
    assert_eq!(
        drawn.iter().filter(|&name| unlabeled_output(name)).count(),
        2
    );
}

#[test]
fn a_chip_taking_out_a_file_class_leaves_its_files_undrawn() {
    let linked = linked();
    let drawn = scoped(
        &linked,
        &["//...", "except kind('generated file')"],
        &[],
        &[],
    );
    assert!(
        !drawn.iter().any(|name| unlabeled_output(name)),
        "{drawn:?}"
    );
    assert!(drawn.contains(&"//p:bin".to_string()));
    let drawn = scoped(
        &linked,
        &["//...:*", "except kind('source file')"],
        &[],
        &[],
    );
    assert!(!drawn.contains(&"//p:main.cc".to_string()), "{drawn:?}");
    assert_eq!(
        drawn.iter().filter(|&name| unlabeled_output(name)).count(),
        2
    );
}
