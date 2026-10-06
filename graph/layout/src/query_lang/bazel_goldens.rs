//! Cases ported from Bazel's own tests of its query language, with their
//! inputs and expected text taken verbatim; NOTICE names the source.

use std::collections::BTreeSet;

use bazel_query_proto::analysis::CqueryResult;
use bazel_query_proto::blaze_query::ConfiguredRuleInput;
use fixedbitset::FixedBitSet;

use super::lex::{TokenKind, scan};
use super::{Function, Universe, parse};
use crate::graph::Kind;
use crate::intern::{Id, Names};
use crate::joint::build_joint;
use crate::test_support;

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

/// QueryParserTest's case, its function list being the one bviz accepts.
#[test]
fn an_unknown_function_lists_the_known_ones() {
    let known: Vec<_> = Function::ALL
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
/// with the `.dwp` and `.stripped` outputs each C++ rule implies; and its
/// package `t`:
///
/// ```text
/// cc_library(name='t', srcs=['t.cc'], data=['r'], testonly=0)
/// cc_library(name='t_test', srcs=['t.cc'], data=['r'], testonly=1)
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
        targets.push(test_support::rule(name, class, inputs, Vec::new()));
        targets.extend(
            outputs
                .iter()
                .map(|output| test_support::generated(output, name)),
        );
    }
    for (name, testonly) in [("//t:t", false), ("//t:t_test", true)] {
        let attributes = vec![test_support::boolean("testonly", testonly)];
        let inputs = ["//t:t.cc", "//t:r"];
        targets.push(test_support::rule(name, "cc_library", &inputs, attributes));
    }
    let joint = build_joint(None, None, Some(&targets), false);
    Universe::new(
        &joint.graph,
        &joint.names,
        &joint.facets,
        &joint.capabilities,
    )
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
    // testAttrOperatorOnBooleans
    ("attr(testonly, 0, t:*)", "//t:t"),
    ("attr(testonly, 1, t:*)", "//t:t_test"),
];

/// Expressions of AbstractQueryTest's `testAttrOperator`, reading attributes
/// bviz does not load into its graph.
const UNLOADED_ATTRIBUTES: &[&str] = &[
    "attr(cmd,':', c:*)",
    "attr(linkshared, 0, c:*)",
    "attr('data', 'r', c:*)",
    "attr('deps', '\\[\\]', c:*)",
];

#[test]
fn narrows_as_bazel_does() {
    let universe = abstract_query_test_packages();
    for &(input, expected) in EVALUATIONS {
        let value = universe.evaluate(&parse(input).unwrap()).unwrap();
        let named: BTreeSet<&str> = value
            .ones()
            .map(|id| universe.label(Id(id as u32)).unwrap())
            .collect();
        let named: Vec<&str> = named.into_iter().collect();
        assert_eq!(named.join(" "), expected, "{input}");
    }
    for &input in UNLOADED_ATTRIBUTES {
        let error = universe.evaluate(&parse(input).unwrap()).unwrap_err();
        assert!(
            error.message.starts_with("attr() needs each rule's '"),
            "{input}: {}",
            error.message
        );
    }
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
    let universe = Universe::new(
        &joint.graph,
        &joint.names,
        &joint.facets,
        &joint.capabilities,
    );
    (universe, joint.names)
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
