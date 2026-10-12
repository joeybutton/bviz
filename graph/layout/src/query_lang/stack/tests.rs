use bazel_query_proto::blaze_query::Target;

use super::*;
use crate::intern::{Id, Names};
use crate::joint::build_joint;
use crate::test_support::{generated, source};

fn rule(name: &str, class: &str, inputs: &[&str]) -> Target {
    crate::test_support::rule(name, class, inputs, Vec::new())
}

/// A query graph: `//a:lib` compiles `//a:lib.cc`, `//a:gen` turns it into
/// `//a:gen.out`, `//b:app` links both, and `//b:test` tests it.
struct Fixture {
    universe: Universe,
    names: Names,
}

impl Fixture {
    fn new() -> Self {
        let targets = [
            rule("//a:lib", "cc_library", &["//a:lib.cc"]),
            source("//a:lib.cc"),
            rule("//a:gen", "genrule", &["//a:lib"]),
            generated("//a:gen.out", "//a:gen"),
            rule("//b:app", "cc_binary", &["//a:lib", "//a:gen.out"]),
            rule("//b:test", "cc_test", &["//b:app"]),
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

    /// The labels the whole of `stack` names, sorted.
    fn named(&self, stack: &Stack) -> Vec<String> {
        let evaluation = stack.evaluate(&self.universe, &Prefixes::default());
        let value = evaluation.value.expect("a stack naming a set");
        let name = |id: usize| self.names.resolve(Id(id as u32));
        let mut labels: Vec<String> = (value.set.ones())
            .map(|id| name(id).trim_start_matches("target:").to_string())
            .collect();
        labels.sort();
        labels
    }
}

fn stack(chips: &[&str]) -> Stack {
    let mut stack = Stack::default();
    for chip in chips {
        stack.push(chip);
    }
    stack
}

/// The id of the chip at `at`.
fn id(stack: &Stack, at: usize) -> ChipId {
    stack.get(at).expect("a chip there").id()
}

/// What `text` reads as a chip once its shorthand is written out, and whether
/// it reads the set before it.
fn written(text: &str) -> (String, bool) {
    let chip = Chip::new(ChipId(0), text);
    let continues = matches!(chip.parsed(), Ok(ChipExpr::Continues(_)));
    (chip.written.source, continues)
}

#[test]
fn the_shorthand_writes_out_the_set_a_chip_reads() {
    let continuing = |text: &str| (text.to_string(), true);
    assert_eq!(written("+ //a:lib"), continuing("$_ + //a:lib"));
    assert_eq!(written("except //a:lib"), continuing("$_ except //a:lib"));
    assert_eq!(written("kind(genrule)"), continuing("kind(genrule, $_)"));
    assert_eq!(written("rdeps(//...)"), continuing("rdeps(//..., $_)"));
    assert_eq!(
        written("attr(tags, manual)"),
        continuing("attr(tags, manual, $_)")
    );
    assert_eq!(written("deps()"), continuing("deps($_)"));
    assert_eq!(
        written("mnemonic(Javac)"),
        continuing("mnemonic(Javac, $_)")
    );
    assert_eq!(
        written("repo_category(main)"),
        continuing("repo_category(main, $_)")
    );
    assert_eq!(
        written("runfiles_plumbing()"),
        continuing("runfiles_plumbing($_)")
    );
    assert_eq!(
        written("except tool_configured()"),
        continuing("$_ except tool_configured($_)")
    );
}

#[test]
fn a_chip_calling_an_extension_narrows_the_set_before_it() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.named(&stack(&["//b:all", "repo_category(main)"])),
        ["//b:app", "//b:test"]
    );
    assert_eq!(
        fixture.named(&stack(&["//b:all", "rule_category(test)"])),
        ["//b:test"]
    );
}

#[test]
fn the_call_a_leading_operator_applies_to_reads_the_set_before_it() {
    let continuing = |text: &str| (text.to_string(), true);
    assert_eq!(
        written("except kind('source file')"),
        continuing("$_ except kind('source file', $_)")
    );
    assert_eq!(written("- deps()"), continuing("$_ - deps($_)"));
    assert_eq!(
        written("except mnemonic(Javac)"),
        continuing("$_ except mnemonic(Javac, $_)")
    );
    assert_eq!(
        written("except kind(rule, //a:all)"),
        continuing("$_ except kind(rule, //a:all)")
    );
    let fixture = Fixture::new();
    assert_eq!(
        fixture.named(&stack(&["//a:*", "except kind('source file')"])),
        ["//a:gen", "//a:gen.out", "//a:lib"]
    );
}

#[test]
fn a_span_past_both_insertions_maps_back_onto_the_chip_as_typed() {
    let written = Written::of("except kind(x)");
    assert_eq!(written.source, "$_ except kind(x, $_)");
    let in_text = |span: Range<usize>| written.span_in_text(&span);
    assert_eq!(in_text(0..2), 0..0, "the leading `$_`");
    assert_eq!(in_text(15..16), 12..13, "the word as typed");
    assert_eq!(in_text(18..20), 13..13, "the continued `$_`");
    assert_eq!(in_text(10..21), 7..14, "the call as typed");
}

#[test]
fn a_leading_operator_applies_to_an_extension_call_reading_the_set_before_it() {
    let fixture = Fixture::new();
    let named = |chips: &[&str]| fixture.named(&stack(chips));
    let all = named(&["//b:all"]);
    assert_eq!(
        named(&["//b:all", "except rule_category(test)"]),
        ["//b:app"]
    );
    assert_eq!(named(&["//b:all", "except rule_category(plumbing)"]), all);
    assert_eq!(named(&["//b:all", "except repo_category(extension)"]), all);
    assert_eq!(
        named(&["//a:*", "except kind('generated file')"]),
        ["//a:gen", "//a:lib", "//a:lib.cc"]
    );
}

/// bviz reads a package group and an environment group as one kind of
/// target, so one chip takes both out.
#[test]
fn a_leading_except_takes_out_package_and_environment_groups_by_kind() {
    use bazel_query_proto::blaze_query::{EnvironmentGroup, PackageGroup, target::Discriminator};
    let targets = [
        rule("//a:lib", "cc_library", &[]),
        Target {
            r#type: Discriminator::PackageGroup as i32,
            package_group: Some(PackageGroup {
                name: "//a:visible".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        },
        Target {
            r#type: Discriminator::EnvironmentGroup as i32,
            environment_group: Some(EnvironmentGroup {
                name: "//a:platforms".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        },
    ];
    let joint = build_joint(None, None, Some(&targets), false);
    let fixture = Fixture {
        universe: Universe::new(
            &joint.graph,
            &joint.names,
            &Arc::new(joint.facets),
            &joint.capabilities,
        ),
        names: joint.names,
    };

    assert_eq!(
        fixture.named(&stack(&["//a:*"])),
        ["//a:lib", "//a:platforms", "//a:visible"]
    );
    assert_eq!(
        fixture.named(&stack(&["//a:*", "except kind('package group')"])),
        ["//a:lib"]
    );
}

#[test]
fn a_chip_naming_every_argument_is_read_as_written() {
    for text in [
        "deps(//a:lib)",
        "kind(genrule, //a:all)",
        "some(//a:all)",
        "config(//a:all)",
        "deps(//a:lib) + //b:app",
    ] {
        assert_eq!(written(text).0, text);
    }
    assert!(!written("deps(//a:lib)").1);
    assert!(written("deps($_, 1)").1);
}

#[test]
fn a_syntax_error_points_into_the_chip_as_typed() {
    let error = |text: &str| Chip::new(ChipId(0), text).parsed().unwrap_err().clone();
    assert_eq!(error("kind(genrule").span, 12..12);
    let after_operator = error("+ )");
    assert_eq!(after_operator.span, 2..3, "{}", after_operator.message);
}

/// The texts of the stack's chips, in order.
fn texts(stack: &Stack) -> Vec<&str> {
    stack.chips().map(Chip::text).collect()
}

/// Sources are unioned wherever they were added, and the refinements narrow
/// that union in order.
#[test]
fn sources_union_and_refinements_narrow_the_union() {
    let fixture = Fixture::new();
    let either = stack(&["//a:all", "kind('cc_.*')", "//b:all"]);
    assert_eq!(texts(&either), ["//a:all", "//b:all", "kind('cc_.*')"]);
    assert_eq!(fixture.named(&either), ["//a:lib", "//b:app", "//b:test"]);
    assert_eq!(
        fixture.named(&stack(&["//b:all", "kind('cc_.*')", "//a:all"])),
        fixture.named(&either),
        "the order of the sources is no part of what they name"
    );
    assert_eq!(
        fixture.named(&stack(&["//a:lib", "+ //b:app"])),
        ["//a:lib", "//b:app"]
    );
    assert_eq!(
        fixture.named(&stack(&["//...", "kind('cc_.*')"])),
        ["//a:lib", "//b:app", "//b:test"]
    );
}

/// A chip joins the end of its group as it is added, and moves to the end of
/// the other group where an edit or a negation changes which it reads as.
#[test]
fn a_chip_stands_in_the_group_it_reads_as() {
    let mut chips = stack(&["kind(x)", "//a:*", "- //a:lib", "//b:*"]);
    assert_eq!(texts(&chips), ["//a:*", "//b:*", "kind(x)", "- //a:lib"]);
    let kind = id(&chips, 2);
    chips.set_text(kind, "//c:*");
    assert_eq!(texts(&chips), ["//a:*", "//b:*", "//c:*", "- //a:lib"]);
    let a = id(&chips, 0);
    chips.toggle_negated(a);
    assert_eq!(texts(&chips), ["//b:*", "//c:*", "- //a:lib", "//a:*"]);
    assert!(
        !chips.move_by(id(&chips, 2), -1),
        "a refinement stays below the sources"
    );
    assert!(chips.move_by(a, -1));
    assert_eq!(texts(&chips), ["//b:*", "//c:*", "//a:*", "- //a:lib"]);
}

/// A chip dropped ahead of another of its group stands there, and one
/// dropped across into the other group stays where it was.
#[test]
fn a_chip_moves_ahead_of_another_of_its_group_alone() {
    let mut chips = stack(&["//a:*", "//b:*", "//c:*", "kind(x)", "- //a:lib"]);
    let [a, b, c, kind, minus] = [0, 1, 2, 3, 4].map(|at| id(&chips, at));
    assert!(chips.move_before(c, Some(a)));
    assert_eq!(
        texts(&chips),
        ["//c:*", "//a:*", "//b:*", "kind(x)", "- //a:lib"]
    );
    assert!(chips.move_before(c, None));
    assert_eq!(
        texts(&chips),
        ["//a:*", "//b:*", "//c:*", "kind(x)", "- //a:lib"]
    );
    assert!(chips.move_before(a, Some(c)));
    assert_eq!(
        texts(&chips),
        ["//b:*", "//a:*", "//c:*", "kind(x)", "- //a:lib"]
    );
    assert!(!chips.move_before(b, Some(kind)), "across the groups");
    assert!(!chips.move_before(kind, Some(minus)), "where it stands");
    assert!(chips.move_before(minus, Some(kind)));
    assert_eq!(
        texts(&chips),
        ["//b:*", "//a:*", "//c:*", "- //a:lib", "kind(x)"]
    );
}

#[test]
fn a_negated_chip_takes_out_what_it_names_and_a_disabled_one_is_skipped() {
    let fixture = Fixture::new();
    let mut negated = stack(&["//a:*", "//a:lib"]);
    negated.toggle_negated(id(&negated, 1));
    assert_eq!(
        fixture.named(&negated),
        ["//a:gen", "//a:gen.out", "//a:lib.cc"]
    );

    let mut disabled = stack(&["//a:*", "//a:lib"]);
    disabled.toggle_enabled(id(&disabled, 1));
    assert_eq!(fixture.named(&disabled).len(), 4);
}

/// The first chip reads the whole graph as the set before it.
#[test]
fn a_first_chip_reading_the_set_before_it_reads_every_node() {
    let fixture = Fixture::new();
    assert_eq!(fixture.named(&stack(&["kind(cc_test)"])), ["//b:test"]);
    let mut negated = stack(&["//...", "//b:all"]);
    negated.toggle_enabled(id(&negated, 0));
    negated.toggle_negated(id(&negated, 1));
    assert!(!fixture.named(&negated).contains(&"//b:app".to_string()));
}

/// A source is counted as the union it joins, beside the union before it.
#[test]
fn a_source_is_counted_with_what_it_adds_to_the_union() {
    let fixture = Fixture::new();
    let stack = stack(&["//a:lib", "//b:app"]);
    let evaluation = stack.evaluate(&fixture.universe, &Prefixes::default());
    let Some(Outcome::Named { counts, before }) = evaluation.outcome(id(&stack, 1)) else {
        panic!("{evaluation:?}");
    };
    assert_eq!((counts.nodes, before.map(|b| b.nodes)), (2, Some(1)));
}

#[test]
fn each_chip_is_counted_beside_the_set_it_read() {
    let fixture = Fixture::new();
    let stack = stack(&["//a:*", "kind(genrule)"]);
    let evaluation = stack.evaluate(&fixture.universe, &Prefixes::default());
    let counts = |at: usize| match evaluation.outcome(id(&stack, at)) {
        Some(Outcome::Named { counts, before }) => (counts.nodes, before.map(|b| b.nodes)),
        outcome => panic!("{outcome:?}"),
    };
    assert_eq!(counts(0), (4, None));
    assert_eq!(counts(1), (1, Some(4)));
}

#[test]
fn a_chip_that_fails_leaves_the_chips_above_it_nothing_to_read() {
    let fixture = Fixture::new();
    let stack = stack(&["//a:*", "+ //c:x", "kind(", "- //b:all"]);
    let evaluation = stack.evaluate(&fixture.universe, &Prefixes::default());
    let outcomes: Vec<&Outcome> = evaluation.chips.iter().map(|(_, o)| o).collect();
    assert!(matches!(outcomes[0], Outcome::Named { .. }));
    let Outcome::Failed(error) = outcomes[1] else {
        panic!("{:?}", outcomes[1]);
    };
    assert_eq!(error.message, "no package '//c' in the build");
    assert_eq!(error.span, 2..7, "the literal as typed");
    assert_eq!(outcomes[2], &Outcome::Blocked);
    assert_eq!(outcomes[3], &Outcome::Blocked);
    assert!(evaluation.value.is_none());

    let invalid = self::stack(&["//a:*", "kind(", "- //b:all"]);
    let evaluation = invalid.evaluate(&fixture.universe, &Prefixes::default());
    assert!(matches!(evaluation.chips[1].1, Outcome::Invalid(_)));
    assert_eq!(evaluation.chips[2].1, Outcome::Blocked);
}

#[test]
fn a_stack_compiles_to_a_let_chain_bazel_parses_back() {
    let mut chips = stack(&["//a:all", "kind(genrule)", "//a:gen", "//b:all"]);
    let generated = id(&chips, 1);
    chips.toggle_negated(generated);
    let compiled = chips.compile().unwrap().expect("enabled chips");
    let text = compiled.expr.to_string();
    assert_eq!(
        text,
        "let _ = (//a:all + //b:all) in let _ = kind('genrule', $_) in ($_ except //a:gen)"
    );
    assert_eq!(parse(&text).unwrap().to_string(), text);
    assert!(!compiled.from_everything);
    assert!(Stack::default().compile().unwrap().is_none());
    let invalid = stack(&["//a:all", "kind("]);
    assert_eq!(invalid.compile().unwrap_err().chip, id(&invalid, 1));
    let refined = stack(&["kind(genrule)"]).compile().unwrap();
    assert!(refined.expect("an enabled chip").from_everything);
}

/// An aquery of `//p:a`, whose Genrule reads `p/a.java` and `p/foo.txt` and
/// writes `a.class`, and of `//p:b`, whose Genrule reads `p/foo.java` and
/// writes `b.class`.
fn java() -> Fixture {
    use crate::test_support::{ActionSpec, action_graph};
    let aquery = [action_graph(&[
        ActionSpec {
            label: "//p:a",
            mnemonic: "Genrule",
            inputs: &["p/a.java", "p/foo.txt"],
            outputs: &["bazel-out/k8-fastbuild/bin/p/a.class"],
        },
        ActionSpec {
            label: "//p:b",
            mnemonic: "Genrule",
            inputs: &["p/foo.java"],
            outputs: &["bazel-out/k8-fastbuild/bin/p/b.class"],
        },
    ])];
    let joint = build_joint(Some(&aquery), None, None, false);
    Fixture {
        universe: Universe::new(
            &joint.graph,
            &joint.names,
            &Arc::new(joint.facets),
            &joint.capabilities,
        ),
        names: joint.names,
    }
}

/// Chips reading each set once write out nested aquery filter calls, which
/// aquery gathers into one filter, so evaluating the chips and their compiled
/// `let` chain gather them alike.
#[test]
fn chained_aquery_filters_filter_as_their_bazel_query_does() {
    let fixture = java();
    let chips = ["//p:all", "inputs('.*foo.*')", "inputs('.*java')"];
    let b = ["action:bazel-out/k8-fastbuild/bin/p/b.class"];
    assert_eq!(fixture.named(&stack(&chips)), b);
    let compiled = stack(&chips).compile().unwrap().expect("enabled chips");
    let value = fixture.universe.evaluate(&compiled.expr).unwrap();
    let name = |id: usize| fixture.names.resolve(Id(id as u32)).to_string();
    assert_eq!(value.ones().map(name).collect::<Vec<_>>(), b);
    assert_eq!(
        bazel(&chips),
        query(
            Command::Aquery,
            "inputs('.*java', inputs('.*foo.*', //p:all))"
        )
    );
}

/// A chip naming the set before it unchanged leaves the filter run standing,
/// so the chips around it filter as one.
#[test]
fn a_chip_naming_the_set_before_it_keeps_the_filter_run() {
    let fixture = java();
    let chips = ["//p:all", "inputs('.*foo.*')", "$_", "inputs('.*java')"];
    assert_eq!(
        fixture.named(&stack(&chips)),
        ["action:bazel-out/k8-fastbuild/bin/p/b.class"]
    );
}

/// A negated filter chip joins the run before it, and takes what the joined
/// run keeps out of what that run named.
#[test]
fn a_negated_filter_chip_takes_out_what_the_joined_run_keeps() {
    let fixture = java();
    let mut chips = stack(&["//p:all", "inputs('.*foo.*')", "inputs('.*java')"]);
    chips.toggle_negated(id(&chips, 2));
    assert_eq!(
        fixture.named(&chips),
        ["action:bazel-out/k8-fastbuild/bin/p/a.class"]
    );
}

fn bazel(chips: &[&str]) -> Result<BazelQuery, Unportable> {
    stack(chips).bazel_text(Command::Query)
}

fn query(command: Command, text: &str) -> Result<BazelQuery, Unportable> {
    Ok(BazelQuery {
        command,
        text: text.to_string(),
    })
}

#[test]
fn a_stack_reading_each_set_once_is_written_out_as_nested_calls() {
    assert_eq!(
        bazel(&["//a:all", "kind(genrule)"]),
        query(Command::Query, "kind('genrule', //a:all)")
    );
    assert_eq!(
        bazel(&["//a:all", "config($_, target)"]),
        query(Command::Cquery, "config(//a:all, 'target')")
    );
    assert_eq!(
        bazel(&["//p:all", "deps()", "mnemonic(Genrule)"]),
        query(Command::Aquery, "mnemonic('Genrule', deps(//p:all))")
    );
    assert_eq!(
        bazel(&["//a:all", "kind(genrule)", "//b:all", "//c:all"]),
        query(
            Command::Query,
            "kind('genrule', (//a:all + //b:all + //c:all))"
        )
    );
    assert_eq!(
        bazel(&["//a:all", "$_ + deps($_)"]),
        query(Command::Query, "let _ = //a:all in ($_ + deps($_))")
    );
}

/// A `let` takes the rest of the expression into its body, so one is written
/// out only where nothing follows it, and bound otherwise.
#[test]
fn a_let_is_written_out_only_where_it_ends_what_holds_it() {
    let bound = bazel(&["let x = //z:all in $x", "$_ + //a:all"]).unwrap();
    assert_eq!(
        bound.text,
        "let _ = let x = //z:all in $x in ($_ + //a:all)"
    );
    assert_eq!(parse(&bound.text).unwrap().to_string(), bound.text);
    assert_eq!(
        bazel(&["let x = //z:all in $x", "deps()"]),
        query(Command::Query, "deps(let x = //z:all in $x)")
    );
}

/// A `let` among the sources stands last in their union, where nothing
/// follows it to be swallowed; two are joined a source at a time.
#[test]
fn a_source_written_as_a_let_ends_the_union() {
    assert_eq!(
        bazel(&["let x = //a:all in $x", "//b:all", "//c:all"]),
        query(
            Command::Query,
            "(//b:all + //c:all + let x = //a:all in $x)"
        )
    );
    let two = bazel(&["let x = //a:all in $x", "let y = //b:all in $y"]).unwrap();
    assert_eq!(
        two.text,
        "let _ = let x = //a:all in $x in ($_ + let y = //b:all in $y)"
    );
    assert_eq!(parse(&two.text).unwrap().to_string(), two.text);
}

/// A step reading `$_` twice is bound alone, so an aquery filter above it
/// still stands on the chain aquery filters by.
#[test]
fn only_a_step_reading_the_set_before_it_twice_is_bound() {
    assert_eq!(
        bazel(&["//p:all", "$_ + deps($_)", "mnemonic(Genrule)"]),
        query(
            Command::Aquery,
            "mnemonic('Genrule', let _ = //p:all in ($_ + deps($_)))"
        )
    );
}

/// The query is written for the command asked for where it reads every
/// function the stack calls.
#[test]
fn a_stack_is_written_for_the_command_preferred_where_it_reads_the_stack() {
    let written = |chips: &[&str], preferred| stack(chips).bazel_text(preferred);
    assert_eq!(
        written(&["//a:all", "kind(genrule)"], Command::Cquery),
        query(Command::Cquery, "kind('genrule', //a:all)")
    );
    assert_eq!(
        written(&["//a:all", "siblings()"], Command::Cquery),
        query(Command::Query, "siblings(//a:all)")
    );
    assert_eq!(
        written(&["//a:all", "config($_, target)"], Command::Aquery),
        query(Command::Cquery, "config(//a:all, 'target')")
    );
}

#[test]
fn a_stack_using_an_extension_has_no_bazel_query() {
    let extension = |extension| Err(Unportable::Extension(extension));
    assert_eq!(
        bazel(&["kind(genrule)"]),
        extension(Extension::FromEverything)
    );
    assert_eq!(
        bazel(&["//a:all", "config($_, anyexec)"]),
        extension(Extension::AnyExec)
    );
    assert_eq!(
        bazel(&["//a:all", "repo_category(main)"]),
        extension(Extension::Function(Function::RepoCategory))
    );
    assert_eq!(
        bazel(&["//a:all", "rule_category(test)"]),
        extension(Extension::Function(Function::RuleCategory))
    );
    assert_eq!(
        bazel(&["//p:all", "runfiles_plumbing()"]),
        extension(Extension::Function(Function::RunfilesPlumbing))
    );
    assert_eq!(
        bazel(&["//a:all", "except tool_configured()"]),
        extension(Extension::Function(Function::ToolConfigured))
    );
    assert_eq!(
        Unportable::Extension(Extension::Function(Function::RepoCategory)).to_string(),
        "repo_category() is a bviz function, which no Bazel query command has"
    );
    assert_eq!(
        bazel(&["//p:all", "mnemonic(Genrule)", "deps()"]),
        extension(Extension::ActionFilterOffChain)
    );
    assert_eq!(
        bazel(&["//p:all", "mnemonic(Genrule)", "+ //p:lib"]),
        extension(Extension::ActionFilterOffChain)
    );
    assert_eq!(
        bazel(&["//p:all", "config($_, target)", "mnemonic(Genrule)"]),
        extension(Extension::NoCommand)
    );
    assert_eq!(bazel(&[]), Err(Unportable::Empty));
    assert_eq!(bazel(&["//a:all", "kind("]), Err(Unportable::Invalid));
}

#[test]
fn an_edit_evaluates_the_edited_chip_and_those_above_it_alone() {
    let fixture = Fixture::new();
    let prefixes = Prefixes::default();
    let mut chips = stack(&["//a:*", "kind(genrule)", "- //a:gen"]);
    chips.evaluate(&fixture.universe, &prefixes);
    assert_eq!(prefixes.len(), 3);

    chips.set_text(id(&chips, 2), "- //a:lib");
    chips.evaluate(&fixture.universe, &prefixes);
    assert_eq!(prefixes.len(), 4);

    chips.toggle_enabled(id(&chips, 1));
    chips.evaluate(&fixture.universe, &prefixes);
    assert_eq!(prefixes.len(), 5);
}

#[test]
fn a_chip_keeps_its_id_and_switches_through_edits_and_moves() {
    let mut chips = stack(&["//a:*", "//b:*", "//c:*"]);
    let [a, b, c] = [0, 1, 2].map(|at| id(&chips, at));
    chips.toggle_negated(b);
    chips.toggle_enabled(b);
    chips.set_text(b, "//d:*");
    let edited = chips.chip(b).unwrap();
    assert_eq!(edited.text(), "//d:*");
    assert!(edited.negated() && !edited.enabled());

    assert_eq!(texts(&chips), ["//a:*", "//c:*", "//d:*"]);
    assert!(chips.move_by(c, -2));
    assert_eq!(chips.position(c), Some(0));
    assert!(!chips.move_by(c, -1));
    assert!(chips.move_by(c, 5));
    assert_eq!(chips.position(c), Some(1), "held to the sources");
    assert_eq!(chips.remove(a), Some(0));
    assert_eq!(chips.position(b), Some(1));
}

#[test]
fn a_chip_names_the_set_operator_it_opens_with() {
    let chips = stack(&["except kind(test)", "kind(test)", "  - //a:all"]);
    let operators: Vec<_> = chips.chips().map(Chip::leading_operator).collect();
    assert_eq!(operators, [Some(0..6), None, Some(2..3)]);
}

/// A stack edited back to the chips it held equals what it was, and any edit
/// that changes a chip's text, its switches or the order of the chips does not.
#[test]
fn a_stack_equals_one_holding_the_same_chips_alike() {
    let original = stack(&["//a:lib", "deps($_)"]);
    let mut edited = original.clone();
    let first = id(&edited, 0);
    edited.toggle_negated(first);
    assert_ne!(edited, original);
    edited.toggle_negated(first);
    assert_eq!(edited, original);

    assert!(!edited.move_by(first, -1));
    assert_eq!(edited, original);
    edited.set_text(first, "//b:app");
    assert_ne!(edited, original);
}
