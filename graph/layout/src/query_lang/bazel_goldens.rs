//! Cases ported from Bazel's own tests of its query language, with their
//! inputs and expected text taken verbatim; NOTICE names the source.

use std::collections::BTreeSet;
use std::sync::Arc;

use bazel_query_proto::analysis::CqueryResult;
use bazel_query_proto::blaze_query::ConfiguredRuleInput;
use fixedbitset::FixedBitSet;

use super::lex::{TokenKind, scan};
use super::{Function, Universe, parse};
use crate::graph::Kind;
use crate::intern::{Id, Names};
use crate::joint::build_joint;
use crate::sources::{Deps, Sources};
use crate::test_support::{
    self, boolean, explicit, label_list, selector_list, string, string_list, through,
};
use bazel_query_proto::blaze_query::Attribute;
use bazel_query_proto::blaze_query::attribute::Discriminator as AttributeType;

/// Each input, and its tokens as LexerTest prints them.
const SCANS: &[(&str, &str)] = &[
    ("", "EOF"),
    ("foo bar wiz intersect", "foo bar wiz intersect EOF"),
    (
        "foo(bar,wiz)deps=intersect",
        "foo ( bar , wiz ) deps = intersect EOF",
    ),
    ("deps(//pkg:target)", "deps ( //pkg:target ) EOF"),
    ("* foo*", "* foo* EOF"),
    ("-foo foo-bar", "- foo foo-bar EOF"),
    ("...", "... EOF"),
    ("foo bar 'foo bar'", "foo bar foo bar EOF"),
    ("set 'set' \"set\"", "set set set EOF"),
    ("set(//foo_bar:.*@4)", "set ( //foo_bar:.*@4 ) EOF"),
    ("set(\"//foo_bar:.*@4\")", "set ( //foo_bar:.*@4 ) EOF"),
    (
        "set(\"//foo:foo=base/2~123[]+asd\")",
        "set ( //foo:foo=base/2~123[]+asd ) EOF",
    ),
    (
        "set(//a:b=bar./@_:~-*$123[]+asd)",
        "set ( //a:b = bar./@_:~-*$123[] + asd ) EOF",
    ),
    (
        "somepath(@foo+@bar+//baz+@@foo +bar,  @@rules_jvm_external++maven+maven//:bar)",
        "somepath ( @foo + @bar + //baz + @@foo + bar , @@rules_jvm_external++maven+maven//:bar ) EOF",
    ),
];

/// Each input, and the kind of each token LexerTest checks.
const SCAN_KINDS: &[(&str, &[TokenKind])] = {
    use TokenKind::*;
    &[
        ("foo bar wiz intersect", &[Word, Word, Word, Intersect, Eof]),
        ("foo bar 'foo bar'", &[Word, Word, Word]),
        ("set 'set' \"set\"", &[Set, Word, Word]),
        ("set(//foo_bar:.*@4)", &[Set, LParen, Word, RParen]),
        ("set(\"//foo_bar:.*@4\")", &[Set, LParen, Word, RParen]),
        (
            "set(\"//foo:foo=base/2~123[]+asd\")",
            &[Set, LParen, Word, RParen],
        ),
        (
            "set(//a:b=bar./@_:~-*$123[]+asd)",
            &[Set, LParen, Word, Equals, Word, Plus, Word, RParen],
        ),
        (
            "somepath(@foo+@bar+//baz+@@foo +bar,  @@rules_jvm_external++maven+maven//:bar)",
            &[
                Word, LParen, Word, Plus, Word, Plus, Word, Plus, Word, Plus, Word, Comma, Word,
                RParen,
            ],
        ),
    ]
};

/// Each expression QueryParserTest prints back, and the input it parses it
/// from.
const PRINTS: &[(&str, &str)] = &[
    ("x", "x"),
    ("//x", "//x"),
    ("//x:y", "//x:y"),
    ("x/...:all-targets", "x/...:all-targets"),
    ("\"set\"", "\"set\""),
    ("\"\"", "\"\""),
    ("(a - b)", "a - b"),
    ("(a intersect b)", "a intersect b"),
    ("(a intersect b intersect c)", "a intersect b intersect c"),
    ("(a union b)", "a union b"),
    ("(a union b union c)", "a union b union c"),
    ("(a except b)", "a except b"),
    ("(a except b except c)", "a except b except c"),
    ("((a union b) except c)", "a union b except c"),
    ("((a except b) union c)", "a except b union c"),
    ("some(x)", "some(x)"),
    ("somepath(x, y)", "somepath(x, y)"),
    ("allpaths(x, y)", "allpaths(x, y)"),
    ("deps(x)", "deps(x)"),
    ("deps(x, 1)", "deps(x, 1)"),
    ("rdeps(x, y)", "rdeps(x, y)"),
    ("rdeps(x, y, 1)", "rdeps(x, y, 1)"),
    ("kind('rule', x)", "kind(rule, x)"),
    ("kind('source file', x)", "kind('source file', x)"),
    ("kind('.*', x)", "kind('.*', x)"),
    ("attr('linkshared', '1', x)", "attr(linkshared,1,x)"),
    ("filter('jar$', x)", "filter(jar$, x)"),
    ("let x = e1 in e2", "let x = e1 in e2"),
    ("labels('srcs', x)", "labels('srcs', x)"),
    ("tests(x)", "tests(x)"),
    ("executables(x)", "executables(x)"),
    ("set()", "set()"),
    ("set(//a)", "set(//a)"),
    ("set(//a //b)", "set(//a //b)"),
    (
        "((((a union b) intersect c) except d) intersect e)",
        "a union b intersect c except d intersect e",
    ),
    ("//foo:.*@4", "\"//foo:.*@4\""),
    ("set(//foo)", "set(\"//foo\")"),
    ("\"set(//foo)\"", "\"set(//foo)\""),
    ("\"set('//foo')\"", "\"set('//foo')\""),
    ("\"//foo:xx+xx\"", "\"//foo:xx+xx\""),
    ("(//foo:xx + xx)", "//foo:xx+xx"),
    ("\"//foo:xx=xx\"", "\"//foo:xx=xx\""),
    (
        "\"foo[]^$asd.|asd?*+{})_asd()2\"",
        "'foo[]^$asd.|asd?*+{})_asd()2'",
    ),
    (
        "\"foo[]^$asd.|asd?*+{})_asd()2\"",
        "\"foo[]^$asd.|asd?*+{})_asd()2\"",
    ),
    ("\" #&()+,;<=>?[]{|}\"", "\" #&()+,;<=>?[]{|}\""),
    ("a.b", "a.b"),
    ("a/b", "a/b"),
    ("a@b", "a@b"),
    ("a_b", "a_b"),
    ("a:b", "a:b"),
    ("a~b", "a~b"),
    ("a-b", "a-b"),
    ("a*b", "a*b"),
    ("a$b", "a$b"),
    ("\"a+b\"", "\"a+b\""),
    ("aaa", "\"aaa\""),
    ("'set(\"//foo\" + \"bar\")'", "'set(\"//foo\" + \"bar\")'"),
    ("\"set('//foo' + 'bar')\"", "\"set('//foo' + 'bar')\""),
    ("\"a'a\"", "\"a'a\""),
    ("'a\"a'", "'a\"a'"),
    ("//:äöüÄÖÜß🌱", "'//:äöüÄÖÜß🌱'"),
    ("//:äöüÄÖÜß🌱", "//:äöüÄÖÜß🌱"),
];

/// Each input QueryParserTest rejects, and the message it rejects it with.
const FAILURES: &[(&str, &str)] = &[
    ("rdeps(", "premature end of input"),
    ("rdeps(,", "syntax error at ','"),
    ("rdeps(a", "premature end of input"),
    ("rdeps(a, ", "premature end of input"),
    ("rdeps(a, )", "syntax error at ')'"),
    ("rdeps(a, b", "premature end of input"),
    ("rdeps(a, b, ", "premature end of input"),
    ("rdeps(a, b, )", "syntax error at ')'"),
    ("rdeps(a, b, 3", "premature end of input"),
    (
        "rdeps(a, b, 3, ",
        "too many arguments to function 'rdeps' at ','",
    ),
    ("rdeps(a, b, c, d)", "expected an integer literal: 'c'"),
    ("set(", "premature end of input"),
    ("set(a", "premature end of input"),
    ("set(a b", "premature end of input"),
    ("set(a, ", "syntax error at ','"),
    ("set(a, b)", "syntax error at ', b )'"),
    ("foo intersect", "premature end of input"),
    (
        "//foo:xx=xx",
        "unexpected token '=' after query expression '//foo:xx'",
    ),
    ("\"-x\"", "target literal must not begin with (-): -x"),
    ("\"*x\"", "target literal must not begin with (*): *x"),
    ("\"a", "unclosed quotation"),
    ("'a", "unclosed quotation"),
    ("a\"a", "unclosed quotation"),
    ("a'a", "unclosed quotation"),
    ("a'\"a", "unclosed quotation"),
    ("a\"'a", "unclosed quotation"),
    ("'a\"'a'", "unclosed quotation"),
    ("\"a'\"a\"", "unclosed quotation"),
    (
        "'\"a\" + 'a''",
        "unexpected token 'a' after query expression ''\"a\" + ''",
    ),
    (
        "\"'a' + \"a\"\"",
        "unexpected token 'a' after query expression '\"'a' + \"'",
    ),
    (
        "\"set(\"//foo\" + \"bar\")\"",
        "unexpected token '//foo' after query expression '\"set(\"'",
    ),
    (
        "'set('//foo' + 'bar')'",
        "unexpected token '//foo' after query expression '\"set(\"'",
    ),
];

#[test]
fn scans_as_bazel_does() {
    for &(input, expected) in SCANS {
        let tokens = scan(input).unwrap();
        let printed: Vec<_> = tokens.iter().map(ToString::to_string).collect();
        assert_eq!(printed.join(" "), expected, "{input:?}");
    }
    for &(input, kinds) in SCAN_KINDS {
        let tokens = scan(input).unwrap();
        let scanned: Vec<_> = tokens.iter().map(|token| token.kind).collect();
        assert_eq!(&scanned[..kinds.len()], kinds, "{input:?}");
    }
    assert_eq!(scan("foo bar 'foo bar'").unwrap()[2].word, "foo bar");
    assert_eq!(scan("'foo").unwrap_err().message, "unclosed quotation");
}

#[test]
fn prints_back_as_bazel_does() {
    for &(expected, input) in PRINTS {
        let printed = parse(input).unwrap().to_string();
        assert_eq!(printed, expected, "{input:?}");
        // What Bazel prints parses back to itself.
        assert_eq!(parse(&printed).unwrap().to_string(), expected, "{input:?}");
    }
}

#[test]
fn fails_as_bazel_does() {
    for &(input, expected) in FAILURES {
        assert_eq!(parse(input).unwrap_err().message, expected, "{input:?}");
    }
}

/// QueryParserTest's case, its function list being Bazel's.
#[test]
fn an_unknown_function_lists_the_known_ones() {
    let known: Vec<_> = Function::BAZEL
        .iter()
        .map(|function| format!("'{}'", function.name()))
        .collect();
    assert_eq!(
        parse("badfunc('foo', 'bar', 'qux', 'zyc')")
            .unwrap_err()
            .message,
        format!(
            "unknown function 'badfunc' at 'badfunc ( foo'; expected one of [{}]",
            known.join(", ")
        )
    );
}

/// AbstractQueryTest's package `c`, as `bazel query --output=proto` writes
/// its BUILD file:
///
/// ```text
/// genrule(name='c', srcs=['p', 'q'], outs=['r', 's'], cmd=':')
/// cc_binary(name='d', srcs=['e.cc'], data=['r'])
/// cc_test(name='f', srcs=['g.cc'])
/// ```
///
/// with the `.dwp` and `.stripped` outputs each C++ rule implies, and those
/// of its rules' attributes the cases read, the ones the BUILD file leaves
/// out at their defaults; its package `t`:
///
/// ```text
/// cc_library(name='t', srcs=['t.cc'], data=['r'], testonly=0)
/// cc_library(name='t_test', srcs=['t.cc'], data=['r'], testonly=1)
/// ```
///
/// and its package `configurable`:
///
/// ```text
/// cc_binary(
///     name = 'main',
///     srcs = ['main.cc'],
///     deps = select({
///         '//conditions:a': [':adep'],
///         '//conditions:b': [':bdep'],
///         '//conditions:default': [':defaultdep'],
///     }))
/// cc_library(name = 'adep', srcs = ['adep.cc'])
/// cc_library(name = 'bdep', srcs = ['bdep.cc'])
/// cc_library(name = 'defaultdep', srcs = ['defaultdep.cc'])
/// ```
fn abstract_query_test_packages() -> Universe {
    let mut targets = Vec::new();
    for source in [
        "//c:BUILD",
        "//c:e.cc",
        "//c:g.cc",
        "//c:p",
        "//c:q",
        "//t:BUILD",
        "//t:t.cc",
        "//t:r",
    ] {
        targets.push(test_support::source(source));
    }
    let c_rules = [
        (
            "//c:c",
            "genrule",
            &["//c:p", "//c:q"][..],
            &["//c:r", "//c:s"],
        ),
        (
            "//c:d",
            "cc_binary",
            &["//c:e.cc", "//c:r"],
            &["//c:d.dwp", "//c:d.stripped"],
        ),
        (
            "//c:f",
            "cc_test",
            &["//c:g.cc"],
            &["//c:f.dwp", "//c:f.stripped"],
        ),
    ];
    for (name, class, inputs, outputs) in c_rules {
        targets.push(test_support::rule(name, class, inputs, c_attributes(name)));
        targets.extend(
            outputs
                .iter()
                .map(|output| test_support::generated(output, name)),
        );
    }
    let configurable = [
        ("//configurable:main", "cc_binary"),
        ("//configurable:adep", "cc_library"),
        ("//configurable:bdep", "cc_library"),
        ("//configurable:defaultdep", "cc_library"),
    ];
    for (name, class) in configurable {
        let (_, short) = name.split_once(':').expect("a label");
        let source = format!("//configurable:{short}.cc");
        let mut attributes = vec![
            explicit(string("name", short)),
            explicit(label_list("srcs", &[&source])),
        ];
        let mut inputs = vec![source.clone()];
        if short == "main" {
            let deps = [":adep", ":bdep", ":defaultdep"].map(|dep| format!("//configurable{dep}"));
            attributes.push(explicit(selector_list(
                "deps",
                &label_list("", &[]),
                &[&[
                    ("//conditions:a", Some(label_list("", &[&deps[0]]))),
                    ("//conditions:b", Some(label_list("", &[&deps[1]]))),
                    ("//conditions:default", Some(label_list("", &[&deps[2]]))),
                ]],
            )));
            inputs.extend(deps);
        } else {
            attributes.push(label_list("deps", &[]));
        }
        let inputs: Vec<&str> = inputs.iter().map(String::as_str).collect();
        targets.push(test_support::rule(name, class, &inputs, attributes));
        targets.push(test_support::source(&source));
    }
    for (name, testonly) in [("//t:t", false), ("//t:t_test", true)] {
        let attributes = vec![test_support::boolean("testonly", testonly)];
        let inputs = ["//t:t.cc", "//t:r"];
        targets.push(test_support::rule(name, "cc_library", &inputs, attributes));
    }
    let joint = build_joint(None, None, Some(&targets), false);
    Universe::of_joint(joint).0
}

/// The attributes `bazel query --output=proto` writes for the rule `name` of
/// AbstractQueryTest's package `c` that its cases read.
fn c_attributes(name: &str) -> Vec<Attribute> {
    let (_, short) = name.split_once(':').expect("a label");
    let unset_deprecation = Attribute {
        name: "deprecation".to_string(),
        r#type: AttributeType::String as i32,
        ..Default::default()
    };
    let mut attributes = vec![
        explicit(string("name", short)),
        unset_deprecation,
        string_list("tags", &[]),
    ];
    match short {
        "c" => attributes.extend([
            explicit(label_list("srcs", &["//c:p", "//c:q"])),
            explicit(Attribute {
                r#type: AttributeType::OutputList as i32,
                ..label_list("outs", &["//c:r", "//c:s"])
            }),
            explicit(string("cmd", ":")),
            label_list("data", &[]),
        ]),
        "d" => attributes.extend([
            explicit(label_list("srcs", &["//c:e.cc"])),
            explicit(label_list("data", &["//c:r"])),
            label_list("deps", &[]),
            boolean("linkshared", false),
        ]),
        _ => attributes.extend([
            explicit(label_list("srcs", &["//c:g.cc"])),
            label_list("data", &[]),
            label_list("deps", &[]),
            boolean("linkshared", false),
        ]),
    }
    attributes
}

/// Each expression AbstractQueryTest evaluates over [`abstract_query_test_packages`],
/// and the labels it names, as `evalToString` prints them.
const EVALUATIONS: &[(&str, &str)] = &[
    (
        "c:*",
        "//c:BUILD //c:c //c:d //c:d.dwp //c:d.stripped //c:e.cc //c:f //c:f.dwp //c:f.stripped //c:g.cc //c:p //c:q //c:r //c:s",
    ),
    // testKindOperator
    ("kind(rule, c:*)", "//c:c //c:d //c:f"),
    ("kind(genrule, c:*)", "//c:c"),
    ("kind(cc.*, c:*)", "//c:d //c:f"),
    (
        "kind(file, c:*)",
        "//c:BUILD //c:d.dwp //c:d.stripped //c:e.cc //c:f.dwp //c:f.stripped //c:g.cc //c:p //c:q //c:r //c:s",
    ),
    (
        "kind(gener.*, c:*)",
        "//c:d.dwp //c:d.stripped //c:f.dwp //c:f.stripped //c:r //c:s",
    ),
    (
        "kind(gen.*, c:*)",
        "//c:c //c:d.dwp //c:d.stripped //c:f.dwp //c:f.stripped //c:r //c:s",
    ),
    (
        "kind(source, c:*)",
        "//c:BUILD //c:e.cc //c:g.cc //c:p //c:q",
    ),
    (
        "kind('source file', c:*)",
        "//c:BUILD //c:e.cc //c:g.cc //c:p //c:q",
    ),
    // testFilterOperator
    ("filter(BUILD, c:*)", "//c:BUILD"),
    ("filter('\\.cc$', c:*)", "//c:e.cc //c:g.cc"),
    ("filter(//c.*cc$, c:*)", "//c:e.cc //c:g.cc"),
    (
        "filter(:.$, c:*)",
        "//c:c //c:d //c:f //c:p //c:q //c:r //c:s",
    ),
    // testAttrOperatorOnName
    ("attr(name, '.*', '//c:*')", "//c:c //c:d //c:f"),
    ("attr(name, '.+', '//c:*')", "//c:c //c:d //c:f"),
    ("attr(name, '.*d.*', '//c:*')", "//c:d"),
    ("attr(name, '.*e.*', '//c:*')", ""),
    // testAttrOperator
    ("attr(cmd,':', c:*)", "//c:c"),
    ("attr(cmd,'', c:*)", "//c:c"),
    ("attr(linkshared, 0, c:*)", "//c:d //c:f"),
    ("attr('data', 'r', c:*)", "//c:d"),
    ("attr('deps', '\\[\\]', c:*)", "//c:d //c:f"),
    ("attr('deps', '^..$', c:*)", "//c:d //c:f"),
    ("attr('srcs', '\\[[^,]+\\]', c:*)", "//c:d //c:f"),
    (
        "attr('deps', 'bdep', //configurable/...)",
        "//configurable:main",
    ),
    ("attr('deps', 'nomatch', //configurable/...)", ""),
    // testNullAttrOperator
    ("attr(deprecation, ' ', c:*)", ""),
    // testAttrOperatorOnBooleans
    ("attr(testonly, 0, t:*)", "//t:t"),
    ("attr(testonly, 1, t:*)", "//t:t_test"),
];

/// The labels `input` names over `universe`, as `evalToString` prints them.
fn eval_to_string(universe: &Universe, input: &str) -> String {
    let value = universe.evaluate(&parse(input).unwrap()).unwrap();
    let named: BTreeSet<&str> = value
        .ones()
        .map(|id| universe.label(Id(id as u32)).unwrap())
        .collect();
    named.into_iter().collect::<Vec<&str>>().join(" ")
}

#[test]
fn narrows_as_bazel_does() {
    let universe = abstract_query_test_packages();
    for &(input, expected) in EVALUATIONS {
        assert_eq!(eval_to_string(&universe, input), expected, "{input}");
    }
}

/// AbstractQueryTest's `testAttrOnPackageDefaultVisibility`, over its package
/// `t` as `bazel query --output=proto` writes it, the rule stating the
/// visibility its package gives it by default:
///
/// ```text
/// package(default_visibility=['//visibility:public'])
/// cc_library(name='t', srcs=['t.cc'])
/// ```
#[test]
fn reads_a_package_default_visibility_as_bazel_does() {
    let attributes = vec![
        explicit(string("name", "t")),
        explicit(label_list("srcs", &["//t:t.cc"])),
        test_support::nodep_label_list("visibility", &["//visibility:public"]),
    ];
    let targets = [
        test_support::rule("//t:t", "cc_library", &["//t:t.cc"], attributes),
        test_support::source("//t:t.cc"),
        test_support::source("//t:BUILD"),
    ];
    let joint = build_joint(None, None, Some(&targets), false);
    let universe = Universe::new(
        &joint.graph,
        &joint.names,
        &Arc::new(joint.facets),
        &Arc::new(joint.attributes),
        &joint.capabilities,
    );
    assert_eq!(
        eval_to_string(&universe, "attr(visibility, public, t:*)"),
        "//t:t"
    );
}

/// The configured targets of ConfiguredTargetQuerySemanticsTest's `config()`
/// cases, as a cquery under `--transitions` writes them: the top-level
/// configuration (1), an exec configuration (2), and one a Starlark
/// transition reached (3), whose checksums share a prefix.
fn configured_target_query_semantics_test_build() -> (Universe, Names) {
    use test_support::{configuration, configured, rule, source};
    let configurations = vec![
        configuration(1, "0a1b2c3d4e5f", "k8-fastbuild", false),
        configuration(2, "9f8e7d6c5b4a", "k8-opt-exec-ST-d57f47055a04", true),
        configuration(3, "0a1bff000011", "k8-fastbuild-ST-4c3a9e1b2d7f", false),
    ];
    // Each configured target: its label, rule class and configuration, and
    // those it depends on in their configurations.
    type Built<'a> = (&'a str, &'a str, u32, &'a [(&'a str, u32)]);
    let built: [Built; 12] = [
        ("//test:target_dep", "simple_rule", 1, &[("//test:dep", 1)]),
        ("//test:exec_dep", "simple_rule", 2, &[("//test:dep", 2)]),
        ("//test:dep", "simple_rule", 1, &[]),
        ("//test:dep", "simple_rule", 2, &[]),
        ("//test:my_java", "java_library", 1, &[]),
        ("//test:my_flag", "string_flag", 1, &[]),
        (
            "//test:buildme",
            "rule_with_deps_transition",
            1,
            &[("//test:mydep", 3)],
        ),
        ("//test:mydep", "simple_rule", 3, &[]),
        (
            "//test:transitioner",
            "rule_with_deps_transition",
            1,
            &[("//test:simple", 3)],
        ),
        ("//test:simple", "simple_rule", 1, &[]),
        ("//test:simple", "simple_rule", 3, &[]),
        ("//mytest:mytarget", "simple_rule", 1, &[]),
    ];
    let mut results: Vec<_> = built
        .into_iter()
        .map(|(name, class, config, deps)| {
            let labels: Vec<&str> = deps.iter().map(|&(label, _)| label).collect();
            let mut target = rule(name, class, &labels, Vec::new());
            let rule = target.rule.as_mut().expect("a rule");
            rule.configured_rule_input = (deps.iter())
                .map(|&(label, config)| ConfiguredRuleInput {
                    label: Some(label.to_string()),
                    configuration_id: Some(config),
                    ..Default::default()
                })
                .collect();
            configured(target, config)
        })
        .collect();
    results.push(configured(source("//test:foo.java"), 0));
    let cquery = [CqueryResult {
        results,
        configurations,
    }];
    let joint = build_joint(None, Some(&cquery), None, false);
    Universe::of_joint(joint)
}

/// The configured targets among `value`, by their names in the graph.
fn configured_targets(universe: &Universe, names: &Names, value: &FixedBitSet) -> Vec<String> {
    let configured = value
        .ones()
        .map(|id| Id(id as u32))
        .filter(|&id| universe.index.kind(id) == Some(Kind::ConfiguredTarget));
    configured.map(|id| names.resolve(id).to_string()).collect()
}

#[test]
fn config_names_configured_targets_as_cquery_does() {
    let (universe, names) = configured_target_query_semantics_test_build();
    let eval = |expr: &str| universe.evaluate(&parse(expr).unwrap());
    let configured = |expr: &str| configured_targets(&universe, &names, &eval(expr).unwrap());

    // testConfig_target
    assert_eq!(eval("config(//test:target_dep, target)").unwrap(), {
        let mut literal = eval("//test:target_dep").unwrap();
        literal.set(
            names.get("target://test:target_dep").unwrap().0 as usize,
            false,
        );
        literal
    });
    assert_eq!(
        eval("config(//test:exec_dep, target)").unwrap_err().message,
        "No target (in) //test:exec_dep could be found in the 'target' configuration"
    );
    assert_eq!(
        configured("config(//test:dep, target)"),
        ["ct://test:dep#0a1b2c3d4e5f"]
    );

    // testConfig_nullConfig
    let null = eval("config(//test:foo.java,null)").unwrap();
    assert_eq!(null.count_ones(..), 1);
    assert!(configured_targets(&universe, &names, &null).is_empty());

    // testConfig_configHash
    assert_eq!(configured("config(//test:mydep, 0a1bff000011)").len(), 1);
    assert!(
        eval("config(//test:mydep, 0a1b2c3d4e5f)")
            .unwrap_err()
            .message
            .contains(
                "No target (in) //test:mydep could be found in the configuration with checksum"
            )
    );

    // testConfig_configHashPrefix
    assert_eq!(
        configured("config(//mytest:mytarget,0a1b2c)"),
        configured("//mytest:mytarget")
    );

    // testConfig_configHashUnknownPrefix
    assert!(
        eval("config(//mytest:mytarget,0a1b2d)")
            .unwrap_err()
            .message
            .contains("config()'s second argument must identify a unique configuration")
    );

    // testConfig_exprArgumentFailure
    assert!(
        eval("config(filter(\"??not-a-valid-regex\", //test:foo.java), null)")
            .unwrap_err()
            .message
            .starts_with("illegal 'filter' pattern regexp '??not-a-valid-regex'")
    );

    // testLabelExpressionsMatchesAllConfiguredTargetsWithLabel
    assert_eq!(configured("//test:simple").len(), 2);

    // testConfigFunctionRefinesMultipleMatches
    assert_eq!(configured("config(//test:simple, target)").len(), 1);
}

/// ConfiguredTargetQuerySemanticsTest's package `configurable`, as a cquery
/// writes it built in a configuration where its `:enabled` setting holds when
/// `enabled`, and in one where it does not otherwise:
///
/// ```text
/// config_setting(
///     name = "enabled",
///     define_values = {"test_enable": "true"})
/// rule_with_list(
///     name = 'target',
///     string_values = select({
///         ':enabled': ['foo', 'bar'],
///         '//conditions:default': ['quux'],
///     }),
/// )
/// ```
fn configurable_target(enabled: bool) -> Universe {
    use test_support::{configuration, configured, rule};
    let values: &[&str] = match enabled {
        true => &["foo", "bar"],
        false => &["quux"],
    };
    let attributes = vec![
        explicit(string("name", "target")),
        explicit(string_list("string_values", values)),
    ];
    let target = rule("//configurable:target", "rule_with_list", &[], attributes);
    let cquery = [CqueryResult {
        results: vec![configured(target, 1)],
        configurations: vec![configuration(1, "0a1b2c3d4e5f", "k8-fastbuild", false)],
    }];
    let joint = build_joint(None, Some(&cquery), None, false);
    Universe::new(
        &joint.graph,
        &joint.names,
        &Arc::new(joint.facets),
        &Arc::new(joint.attributes),
        &joint.capabilities,
    )
}

/// ConfiguredTargetQuerySemanticsTest's `testAttrRespectsConfiguration`: in
/// the default configuration and then under `--define=test_enable=true`.
#[test]
fn reads_attributes_as_configured_as_cquery_does() {
    let cases = [
        (
            false,
            [
                ("attr(string_values, 'foo', '//configurable:target')", ""),
                ("attr(string_values, 'bar', '//configurable:target')", ""),
                (
                    "attr(string_values, 'quux', '//configurable:target')",
                    "//configurable:target",
                ),
            ],
        ),
        (
            true,
            [
                (
                    "attr(string_values, 'foo', '//configurable:target')",
                    "//configurable:target",
                ),
                (
                    "attr(string_values, 'bar', '//configurable:target')",
                    "//configurable:target",
                ),
                ("attr(string_values, 'quux', '//configurable:target')", ""),
            ],
        ),
    ];
    for (enabled, evaluations) in cases {
        let universe = configurable_target(enabled);
        for (input, expected) in evaluations {
            assert_eq!(
                eval_to_string(&universe, input),
                expected,
                "{input}, enabled: {enabled}"
            );
        }
    }
}

/// The actions of a target of aquery_test.sh's BUILD files: its label, and
/// one `(mnemonic, inputs, outputs)` for each action it registers, each file
/// named relative to the package.
type Registered = (
    &'static str,
    &'static [(&'static str, &'static str, &'static str)],
);

const FOO: Registered = (
    "foo",
    &[("Genrule", "foo_matching_in.java", "foo_matching_out")],
);
const WRONG_INPUT: Registered = (
    "wrong_input",
    &[("Genrule", "wrong_input.java", "wi_matching_out")],
);
const WRONG_OUTPUT: Registered = (
    "wrong_output",
    &[("Genrule", "wo_matching_in.java", "wrong_out")],
);
/// `java_library(name = "wrong_mnemonic", srcs = ["wm_matching_in.java"])`.
const WRONG_MNEMONIC: Registered = (
    "wrong_mnemonic",
    &[("Javac", "wm_matching_in.java", "libwrong_mnemonic.jar")],
);

/// Each of aquery_test.sh's filtering tests: its package, named after the
/// test, the targets its BUILD file declares, its query, and the mnemonic of
/// the one action of `foo` the query names.
const AQUERY_FILTERS: &[(&str, &[Registered], &str, &str)] = &[
    (
        "test_aquery_all_filters_only_match_foo",
        &[FOO, WRONG_INPUT, WRONG_OUTPUT, WRONG_MNEMONIC],
        "inputs(
    '.*matching_in.java', outputs('.*matching_out', mnemonic('Genrule', //$pkg:all)))",
        "Genrule",
    ),
    (
        "test_aquery_inputs_filter_only_mach_foo",
        &[FOO, WRONG_INPUT],
        "inputs('.*matching_in.java', //$pkg:all)",
        "Genrule",
    ),
    (
        "test_aquery_outputs_filter_only_mach_foo",
        &[FOO, WRONG_OUTPUT],
        "outputs('.*matching_out', //$pkg:all)",
        "Genrule",
    ),
    (
        "test_aquery_mnemonic_filter_only_mach_foo",
        &[FOO, WRONG_MNEMONIC],
        "mnemonic('.*rule', //$pkg:all)",
        "Genrule",
    ),
    (
        "test_aquery_inputs_filter_exact_filename_only_mach_foo",
        &[FOO, WRONG_INPUT],
        "inputs('$pkg/foo_matching_in.java', //$pkg:all)",
        "Genrule",
    ),
    (
        "test_aquery_outputs_filter_exact_filename_only_mach_foo",
        &[FOO, WRONG_OUTPUT],
        "outputs('.*/$pkg/foo_matching_out', //$pkg:all)",
        "Genrule",
    ),
    (
        "test_aquery_mnemonic_filter_exact_mnemonic_only_mach_foo",
        &[FOO, WRONG_MNEMONIC],
        "mnemonic('Genrule', //$pkg:all)",
        "Genrule",
    ),
    (
        "test_aquery_filters_chain_inputs_only_match_one",
        &[
            FOO,
            ("bar", &[("Javac", "in_bar.java", "libbar.jar")]),
            (
                "foo2",
                &[("Genrule", "foo_matching_in.java_not", "foo_matching_out2")],
            ),
        ],
        "inputs('.*java', inputs('.*foo.*', //$pkg:all))",
        "Genrule",
    ),
    (
        "test_aquery_filters_chain_outputs_only_match_one",
        &[
            FOO,
            (
                "foo2",
                &[("Genrule", "foo_matching_in.java", "foo_matching_out_not")],
            ),
            (
                "foo3",
                &[("Genrule", "foo_matching_in.java", "not_matching_out")],
            ),
        ],
        "outputs('.*out', outputs('.*foo.*', //$pkg:all))",
        "Genrule",
    ),
    (
        "test_aquery_filters_chain_mnemonic_only_match_one",
        // java_library(name='foo', srcs=['Foo.java']), whose actions'
        // mnemonics the test lists.
        &[(
            "foo",
            &[
                ("Javac", "Foo.java", "libfoo.jar"),
                ("JavaSourceJar", "Foo.java", "libfoo-src.jar"),
                ("Turbine", "Foo.java", "libfoo-hjar.jar"),
            ],
        )],
        "mnemonic('Java.*', mnemonic('.*e.*', //$pkg:all))",
        "JavaSourceJar",
    ),
];

#[test]
fn filters_actions_as_aquery_does() {
    for &(pkg, targets, query, mnemonic) in AQUERY_FILTERS {
        let mut actions: Vec<(String, &str, Vec<String>, Vec<String>)> = Vec::new();
        for &(name, registered) in targets {
            for &(mnemonic, input, output) in registered {
                actions.push((
                    format!("//{pkg}:{name}"),
                    mnemonic,
                    vec![format!("{pkg}/{input}")],
                    vec![format!("bazel-out/k8-fastbuild/bin/{pkg}/{output}")],
                ));
            }
        }
        let inputs: Vec<Vec<&str>> = (actions.iter())
            .map(|(_, _, inputs, _)| inputs.iter().map(String::as_str).collect())
            .collect();
        let outputs: Vec<Vec<&str>> = (actions.iter())
            .map(|(_, _, _, outputs)| outputs.iter().map(String::as_str).collect())
            .collect();
        let specs: Vec<test_support::ActionSpec> = (actions.iter().enumerate())
            .map(|(at, (label, mnemonic, _, _))| test_support::ActionSpec {
                label,
                mnemonic,
                inputs: &inputs[at],
                outputs: &outputs[at],
            })
            .collect();
        let aquery = [test_support::action_graph(&specs)];
        let joint = build_joint(Some(&aquery), None, None, false);
        let facets = Arc::new(joint.facets);
        let universe = Universe::new(
            &joint.graph,
            &joint.names,
            &facets,
            &Arc::new(joint.attributes),
            &joint.capabilities,
        );

        let query = query.replace("$pkg", pkg);
        let value = universe.evaluate(&parse(&query).unwrap()).unwrap();
        let named: Vec<(&str, &str)> = value
            .ones()
            .map(|id| {
                let id = Id(id as u32);
                let mnemonic = facets.mnemonic(id).map(|sym| facets.strings().resolve(sym));
                (universe.label(id).unwrap(), mnemonic.unwrap())
            })
            .collect();
        let foo = format!("//{pkg}:foo");
        assert_eq!(named, [(foo.as_str(), mnemonic)], "{query}");
    }
}

/// A rule `name` of `class` as `bazel query --output=proto` writes it: its
/// inputs, the attributes its BUILD file sets, and those it does not.
fn declared_rule(
    name: &str,
    class: &str,
    inputs: &[&str],
    mut attributes: Vec<Attribute>,
) -> bazel_query_proto::blaze_query::Target {
    attributes.push(test_support::name_attribute(name));
    test_support::rule(name, class, inputs, attributes)
}

/// AbstractQueryTest's `testNoImplicitDeps`, over its package `x` as `bazel
/// query --output=proto` writes it with the Bazel tools it depends on
/// implicitly:
///
/// ```text
/// cc_binary(name='x', srcs=['x.cc'])
/// ```
///
/// The cases leaving tool dependencies out are run by a query, which tells
/// none, and C++ toolchain resolution states no edge in a dump.
#[test]
fn leaves_implicit_dependencies_out_as_bazel_does() {
    use test_support::{label, source};
    let tools = |name: &str| format!("@@bazel_tools//tools/{name}");
    let (malloc, extra, extra_cc) = (
        tools("cpp:malloc"),
        tools("cpp:link_extra_lib"),
        tools("cpp:linkextra.cc"),
    );
    let (def_parser, def_parser_exe) = (
        tools("def_parser:def_parser"),
        tools("def_parser:def_parser.exe"),
    );
    let targets = [
        declared_rule(
            "//x:x",
            "cc_binary",
            &["//x:x.cc", &malloc, &extra, &def_parser],
            vec![
                explicit(label_list("srcs", &["//x:x.cc"])),
                label("malloc", &malloc),
                label("link_extra_lib", &extra),
                label("$def_parser", &def_parser),
            ],
        ),
        source("//x:x.cc"),
        declared_rule(&malloc, "cc_library", &[], Vec::new()),
        declared_rule(
            &extra,
            "cc_library",
            &[&extra_cc],
            vec![explicit(label_list("srcs", &[&extra_cc]))],
        ),
        source(&extra_cc),
        declared_rule(
            &def_parser,
            "filegroup",
            &[&def_parser_exe],
            vec![explicit(label_list("srcs", &[&def_parser_exe]))],
        ),
        source(&def_parser_exe),
    ];
    let joint = build_joint(None, None, Some(&targets), false);
    let every = Sources::reading(&joint.capabilities);
    let explicit = every.with_deps(Deps::Implicit, false).unwrap();

    let host = [malloc.as_str(), &extra, &extra_cc];
    let implicit = [def_parser.as_str(), &def_parser_exe];
    let mut expected: Vec<&str> = ["//x:x", "//x:x.cc"].into_iter().chain(host).collect();
    expected.extend(implicit);
    expected.sort();
    assert_eq!(
        eval_to_string(&through(&joint, every), "deps(//x)"),
        expected.join(" ")
    );
    assert_eq!(
        eval_to_string(&through(&joint, explicit), "deps(//x)"),
        "//x:x //x:x.cc"
    );
}

/// AbstractQueryTest's `testDefaultVisibilityReturnedInDeps` and
/// `testDefaultVisibilityReturnedInDepsForInputFiles`, over their packages as
/// `bazel query --output=proto` writes them, the rule and the file stating
/// the visibility their package gives them by default:
///
/// ```text
/// # kiwi/BUILD, which the second test declares `kiwi` in with srcs=['kiwi.sh']
/// package(default_visibility=['//mango:mango'])
/// foo_library(name='kiwi')
/// # mango/BUILD
/// package_group(name='mango', packages=[])
/// ```
#[test]
fn follows_a_package_default_visibility_as_bazel_does() {
    let mut script = test_support::source("//kiwi:kiwi.sh");
    let file = script.source_file.as_mut().unwrap();
    file.package_group.push("//mango:mango".to_string());
    file.visibility_label.push("//mango:mango".to_string());
    let targets = [
        declared_rule(
            "//kiwi:kiwi",
            "foo_library",
            &[],
            vec![test_support::nodep_label_list(
                "visibility",
                &["//mango:mango"],
            )],
        ),
        script,
        test_support::package_group("//mango:mango", &[]),
    ];
    let joint = build_joint(None, None, Some(&targets), false);
    let universe = through(&joint, Sources::reading(&joint.capabilities));

    assert_eq!(
        eval_to_string(&universe, "deps(//kiwi:kiwi)"),
        "//kiwi:kiwi //mango:mango"
    );
    assert_eq!(
        eval_to_string(&universe, "deps(//kiwi:kiwi.sh)"),
        "//kiwi:kiwi.sh //mango:mango"
    );
}

/// AbstractQueryTest's `testIncludesReturnedInDeps` and
/// `testNodepDeps_defaultIsTrue`, over their packages as `bazel query
/// --output=proto` writes them:
///
/// ```text
/// # peach/BUILD
/// package_group(name='peach', includes=[':seed'])
/// package_group(name='seed', includes=[':cyanide'])
/// package_group(name='cyanide', packages=['//hydrogen', '//nitrogen', '//carbon'])
/// foo_library(name='dessert', visibility=[':peach'])
/// # foo/BUILD
/// foo_library(name = 't1', deps = [':t2'], visibility = [':pg'])
/// foo_library(name = 't2')
/// package_group(name = 'pg')
/// ```
///
/// `testNodepDeps_false` runs under `--nonodep_deps`, which bviz offers no
/// counterpart of.
#[test]
fn follows_visibility_and_package_group_includes_as_bazel_does() {
    use test_support::nodep_label_list;
    let targets = [
        test_support::package_group("//peach:peach", &["//peach:seed"]),
        test_support::package_group("//peach:seed", &["//peach:cyanide"]),
        test_support::package_group("//peach:cyanide", &[]),
        declared_rule(
            "//peach:dessert",
            "foo_library",
            &[],
            vec![explicit(nodep_label_list("visibility", &["//peach:peach"]))],
        ),
        declared_rule(
            "//foo:t1",
            "foo_library",
            &["//foo:t2"],
            vec![
                explicit(label_list("deps", &["//foo:t2"])),
                explicit(nodep_label_list("visibility", &["//foo:pg"])),
            ],
        ),
        declared_rule("//foo:t2", "foo_library", &[], Vec::new()),
        test_support::package_group("//foo:pg", &[]),
    ];
    let joint = build_joint(None, None, Some(&targets), false);
    let universe = through(&joint, Sources::reading(&joint.capabilities));

    assert_eq!(
        eval_to_string(&universe, "deps(//peach:dessert)"),
        "//peach:cyanide //peach:dessert //peach:peach //peach:seed"
    );
    assert!(eval_to_string(&universe, "deps(//foo:t1)").contains("//foo:pg"));
}

/// ConfiguredTargetQuerySemanticsTest's
/// `testExecTransitionNotFilteredByNoToolDeps`, over the package of its
/// `createConfigRulesAndBuild` as a cquery under `--transitions` writes it,
/// built in the top-level configuration (1) and the exec configuration (2)
/// `my_rule`'s `exec` attribute transitions to:
///
/// ```text
/// my_rule(name = "my_rule", exec = ":exec_dep", target = ":target_dep", deps = [":dep"])
/// simple_rule(name = "target_dep", dep = ":dep")
/// simple_rule(name = "exec_dep", dep = ":dep")
/// simple_rule(name = "dep")
/// ```
#[test]
fn leaves_tool_dependencies_out_as_cquery_does() {
    use test_support::{configuration, configured, configured_inputs, label};
    let with_inputs = |name: &str, class, config: u32, deps: &[(&str, u32)], set| {
        let labels: Vec<&str> = deps.iter().map(|&(label, _)| label).collect();
        let mut target = declared_rule(name, class, &labels, set);
        let configured_deps: Vec<(&str, Option<u32>)> = deps
            .iter()
            .map(|&(label, config)| (label, Some(config)))
            .collect();
        target.rule.as_mut().expect("a rule").configured_rule_input =
            configured_inputs(&configured_deps);
        configured(target, config)
    };
    let my_rule = vec![
        explicit(label("exec", "//test:exec_dep")),
        explicit(label("target", "//test:target_dep")),
        explicit(label_list("deps", &["//test:dep"])),
    ];
    let simple = || vec![explicit(label("dep", "//test:dep"))];
    let cquery = [CqueryResult {
        results: vec![
            with_inputs(
                "//test:my_rule",
                "my_rule",
                1,
                &[
                    ("//test:exec_dep", 2),
                    ("//test:target_dep", 1),
                    ("//test:dep", 1),
                ],
                my_rule,
            ),
            with_inputs(
                "//test:target_dep",
                "simple_rule",
                1,
                &[("//test:dep", 1)],
                simple(),
            ),
            with_inputs(
                "//test:exec_dep",
                "simple_rule",
                2,
                &[("//test:dep", 2)],
                simple(),
            ),
            with_inputs("//test:dep", "simple_rule", 1, &[], Vec::new()),
            with_inputs("//test:dep", "simple_rule", 2, &[], Vec::new()),
        ],
        configurations: vec![
            configuration(1, "0a1b2c3d4e5f", "k8-fastbuild", false),
            configuration(2, "9f8e7d6c5b4a", "k8-opt-exec-ST-d57f47055a04", true),
        ],
    }];
    let joint = build_joint(None, Some(&cquery), None, false);
    let every = Sources::reading(&joint.capabilities);
    let filtered = (every.with_deps(Deps::Tool, false))
        .and_then(|sources| sources.with_deps(Deps::Implicit, false))
        .unwrap();

    assert_eq!(
        eval_to_string(&through(&joint, filtered), "deps(//test:my_rule)"),
        "//test:dep //test:my_rule //test:target_dep"
    );
    assert_eq!(
        eval_to_string(&through(&joint, every), "deps(//test:my_rule)"),
        "//test:dep //test:exec_dep //test:my_rule //test:target_dep"
    );
}
