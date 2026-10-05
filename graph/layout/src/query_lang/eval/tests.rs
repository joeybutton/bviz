use bazel_query_proto::blaze_query::{
    Attribute, GeneratedFile, Rule, SourceFile, Target, attribute::Discriminator as AttributeType,
    target::Discriminator,
};
use fixedbitset::FixedBitSet;

use super::super::parse;
use super::*;
use crate::facets::FacetsBuilder;
use crate::graph::{EdgeKind, Graph, Kind, Node};
use crate::intern::{Id, Names};
use crate::joint::build_joint;

fn rule(name: &str, inputs: &[&str], tags: &[&str]) -> Target {
    let tags = Attribute {
        name: "tags".to_string(),
        r#type: AttributeType::StringList as i32,
        string_list_value: tags.iter().map(|tag| tag.to_string()).collect(),
        ..Default::default()
    };
    Target {
        r#type: Discriminator::Rule as i32,
        rule: Some(Rule {
            name: name.to_string(),
            rule_class: "genrule".to_string(),
            rule_input: inputs.iter().map(|input| input.to_string()).collect(),
            attribute: vec![tags],
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn source(name: &str) -> Target {
    Target {
        r#type: Discriminator::SourceFile as i32,
        source_file: Some(SourceFile {
            name: name.to_string(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn generated(name: &str, rule: &str) -> Target {
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
            &joint.facets,
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
    assert_eq!(
        fixture.error("kind(rule, //a:lib)"),
        "bviz does not evaluate kind()"
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
    (Universe::new(&graph, &names, &facets, &capabilities), names)
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
    (Universe::new(&graph, &names, &facets, &capabilities), names)
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
    let universe = Universe::new(&graph, &names, &facets, &capabilities);

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
