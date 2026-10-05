//! Cases ported from Bazel's own tests of its query lexer and parser, with
//! their inputs and expected text taken verbatim; NOTICE names the source.

use super::lex::{TokenKind, scan};
use super::{Function, parse};

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
