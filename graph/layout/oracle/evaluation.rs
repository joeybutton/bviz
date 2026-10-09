//! Evaluates expressions drawn at random over query graphs drawn at random,
//! with bviz's engine and with Bazel's, and checks they name the same targets.
//!
//! A graph is written as Bazel writes `query --output=streamed_proto`, read by
//! bviz's query loader, and handed to Bazel's engine as targets whose
//! dependencies are those `LabelVisitationUtils.visitTarget` visits: a rule's
//! inputs and the package groups its visibility names, Bazel's own
//! `RuleVisibility` parsing them, a file's generating rule and package
//! groups, and a package group's includes. A rule's inputs are every label
//! its dependency attributes name and some no attribute does, as an aspect's.
//! Each graph is also queried under `--noimplicit_deps`, over the graph a view
//! leaving implicit dependencies out walks and, on Bazel's side, over the
//! labels the rule's explicitly specified attributes name as Bazel's own types
//! and `Selector`s visit them.
//!
//! `attr` is drawn for attributes of every type the graphs drawn state, as
//! Bazel writes them by default: every rule its name and tags, and some rules
//! each of the others, some as `select`s, and for one no rule states. Bazel's
//! engine reads each value through Bazel's own types, the combinations of
//! `select` branches its `AggregatingAttributeMapper` visits, and its
//! `TargetUtils.convertAttributeValue`.
//!
//! `labels` and `tests` read a rule's attributes through Bazel's own
//! `LabelsFunction` and `TestsFunction`, over the prerequisites, string lists
//! and test roles the oracle's accessor reads off the proto. The graphs drawn
//! hold tests of drawn tags and sizes, and test suites of drawn filtering tags
//! naming tests, suites, other targets and themselves, nested and in cycles.
//!
//! `visible` reads a target's visibility through Bazel's own `VisibleFunction`
//! and `QueryVisibility`, over what the oracle's accessor gives as
//! `BlazeTargetAccessor.getVisibility` does, a package group's entries parsed
//! by Bazel's `PackageSpecification`. The graphs drawn hold packages under
//! `java/` and `javatests/`, visibilities of every kind of label, package
//! groups naming packages, negatively or not, and including one another, and
//! output files whose rule the dump lacks. Some name a package group the dump
//! lacks, where Bazel's `getTarget` fails, which the oracle's accessor fails
//! with bviz's wording.
//!
//! `somepath` and `some` may each answer any of several sets. Drawn only at
//! the top of an expression, where nothing reads their answer further, each is
//! checked for being one Bazel's engine could give: a path of fewest edges
//! from the first argument down to the second, and as many of the first
//! argument as Bazel takes.
//!
//! An error's message is compared word for word, which checks Bazel's own
//! wording wherever Bazel's code raises it: a target pattern it cannot parse,
//! a variable `let` leaves unbound or cannot bind, an empty `some`. A pattern
//! naming a target, package or repository the dump lacks fails in the
//! oracle's resolver with bviz's wording, the dump holding no BUILD file for
//! Bazel's, so the comparison checks only that both fail there. Patterns
//! resolve as a query resolves them, keeping rules tagged `manual`; a build
//! command line's leaving them out has no counterpart in a query engine and
//! is checked against Bazel's loading-phase tests in `target_pattern`.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use bazel_query_proto::blaze_query::attribute::{
    Discriminator as AttributeType, Selector, SelectorEntry, SelectorList, Tristate,
};
use bazel_query_proto::blaze_query::{
    Attribute, GeneratedFile, LabelDictUnaryEntry, LabelKeyedStringDictEntry, License,
    PackageGroup, Rule, SourceFile, StringDictEntry, StringListDictEntry, Target,
    target::Discriminator,
};
use bviz_graph_layout::query_lang::{Universe, parse};
use bviz_graph_layout::streamed_proto::read_query_targets;
use bviz_graph_layout::{Deps, Sources, build_joint};
use fastrand::Rng;
use prost::Message;

use super::{REPORTED_MISMATCHES, ask_bazel, seeds};

/// The seeds graphs are drawn from, unless `QUERY_ORACLE_GRAPH_SEEDS` lists
/// others, comma-separated.
const GRAPH_SEEDS: &[u64] = &[1, 2, 3, 5, 8, 13, 21, 34];
const EXPRESSIONS_PER_GRAPH: usize = 400;
/// The fewest of every this many expressions each outcome — naming nothing,
/// naming something, failing — must be drawn for.
const MIN_OUTCOME_SHARE: usize = 10;
/// How deeply drawn expressions nest.
const MAX_DEPTH: u32 = 3;

/// The packages a graph may hold, by the repository part their labels carry.
const PACKAGES: &[(&str, &str)] = &[
    ("", ""),
    ("", "a"),
    ("", "a/b"),
    ("", "a/b/c"),
    ("", "ab"),
    ("", "d"),
    ("", "java/x"),
    ("", "javatests/x"),
    ("", "javatests/y"),
    ("@@ext+", ""),
    ("@@ext+", "p"),
    ("@@ext+", "p/q"),
];

const RULE_NAMES: &[&str] = &["lib", "app", "test", "tool", "all", "a", "b"];
const SOURCE_NAMES: &[&str] = &["s.txt", "x/y.txt", "data.json"];
const RULE_CLASSES: &[&str] = &[
    "cc_library",
    "genrule",
    "java_binary",
    "sh_test",
    "cc_test",
    "test_suite",
];
const TEST_TAGS: &[&str] = &["manual", "local", "flaky", "x"];
const TEST_SIZES: &[&str] = &["small", "medium", "large"];
/// The most entries a drawn package group's `packages` holds, many enough
/// that a negative one often takes out a package a positive one names.
const MAX_CONTAINED: usize = 6;
const SUITE_TAGS: &[&str] = &[
    "manual", "-flaky", "+x", "x", "-local", "small", "-small", "+large",
];

/// Patterns naming what no graph holds, or that no graph can resolve.
/// The label of a package group no graph holds.
const ABSENT_GROUP: &str = "//zz:group";
const MISSING: &[&str] = &[
    "//zz:x",
    "//a:nope",
    "//zz/...",
    "@@nope//x:y",
    "@nope//...",
    "@@ext//p:all",
    "//a/...:lib",
    "nowhere/at/all.txt",
];

/// Patterns over every graph.
const WIDE: &[&str] = &[
    "//...",
    "//...:*",
    "//...:all-targets",
    "...",
    "@ext//...",
    "@@ext+//...:*",
    "a/...",
    "a/b",
    "a:all",
];

#[test]
fn evaluates_as_bazel_does() {
    let mut cases = 0;
    // Expressions naming nothing, naming something, and failing.
    let mut outcomes = [0; 3];
    let mut mismatches: Vec<String> = Vec::new();
    for seed in seeds("QUERY_ORACLE_GRAPH_SEEDS", GRAPH_SEEDS) {
        let mut rng = Rng::with_seed(seed);
        let graph = Graph::draw(&mut rng);
        let path = graph.write(seed);
        let targets = read_query_targets(&std::fs::read(&path).unwrap()).unwrap();
        let joint = build_joint(None, None, Some(&targets), false);
        let every = Sources::reading(&joint.capabilities);
        let explicit = every.with_deps(Deps::Implicit, false).unwrap();
        let settings = [
            (every, &graph.deps, &[][..]),
            (explicit, &graph.explicit_deps, &[Deps::Implicit.flag()][..]),
        ];
        for (sources, deps, flags) in settings {
            let walked = sources.mask(&joint.graph, &joint.provenance);
            let universe = Universe::new(
                &walked,
                &joint.names,
                &Arc::new(joint.facets.clone()),
                &Arc::new(joint.attributes.clone()),
                &joint.capabilities,
            );
            let ours = Ours {
                universe: &universe,
                deps,
            };

            let expressions: Vec<Drawn> = (0..EXPRESSIONS_PER_GRAPH)
                .map(|_| Drawn::draw(&mut rng, &graph))
                .collect();
            let asked: Vec<&str> = expressions.iter().flat_map(Drawn::asked).collect();
            let answers = ask_bazel(Some(&path), flags, asked.iter().copied());
            assert_eq!(
                answers.len(),
                asked.len(),
                "the oracle answers every expression"
            );
            let mut answers = answers.iter().map(String::as_str);
            for drawn in &expressions {
                cases += 1;
                let bazel: Vec<&str> = drawn.asked().map(|_| answers.next().unwrap()).collect();
                match named(bazel[0]) {
                    Some(labels) if labels.is_empty() => outcomes[0] += 1,
                    Some(_) => outcomes[1] += 1,
                    None => outcomes[2] += 1,
                }
                if let Err(why) = ours.check(drawn, &bazel) {
                    let flags = flags.join(" ");
                    mismatches.push(format!("graph {seed} {flags}: {}\n  {why}", drawn.text));
                }
            }
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} of {cases} expressions evaluated differently:\n{}",
        mismatches.len(),
        mismatches[..mismatches.len().min(REPORTED_MISMATCHES)].join("\n")
    );
    // Each outcome is exercised, so no part of the comparison is idle.
    assert!(
        outcomes
            .iter()
            .all(|&count| count * MIN_OUTCOME_SHARE >= cases),
        "{outcomes:?} of {cases}"
    );
}

/// A graph's targets, and the dependencies each states.
struct Graph {
    targets: Vec<Target>,
    /// Every label, in the order drawn.
    labels: Vec<String>,
    /// The packages holding a target, as `{repo}//{path}`.
    packages: Vec<(String, String)>,
    /// The labels each target depends on that the graph holds.
    deps: HashMap<String, Vec<String>>,
    /// The same under `--noimplicit_deps`.
    explicit_deps: HashMap<String, Vec<String>>,
}

impl Graph {
    fn draw(rng: &mut Rng) -> Self {
        let mut packages: Vec<(String, String)> = PACKAGES
            .iter()
            .filter(|_| rng.u8(..4) != 0)
            .map(|&(repo, path)| (repo.to_string(), path.to_string()))
            .collect();
        if packages.is_empty() {
            packages.push((String::new(), "a".to_string()));
        }
        let mut rules: Vec<(String, Vec<String>, Vec<String>)> = Vec::new();
        let mut files: Vec<Target> = Vec::new();
        let mut labels: Vec<String> = Vec::new();
        let mut groups: Vec<String> = Vec::new();
        for (repo, path) in &packages {
            let label = |name: &str| format!("{repo}//{path}:{name}");
            let mut names: Vec<&str> = RULE_NAMES.to_vec();
            rng.shuffle(&mut names);
            let held = &names[..rng.usize(1..4)];
            for name in held {
                let tags = match rng.u8(..4) {
                    0 => vec!["manual".to_string()],
                    1 => vec!["local".to_string(), "manual".to_string()],
                    _ => Vec::new(),
                };
                rules.push((label(name), Vec::new(), tags));
                labels.push(label(name));
            }
            for name in SOURCE_NAMES.iter().filter(|_| rng.bool()) {
                files.push(source(&label(name)));
                labels.push(label(name));
            }
            for index in 0..rng.usize(..3) {
                let owner = rules.len() - 1 - rng.usize(..held.len());
                let output = label(&format!("out{index}.o"));
                rules[owner].1.push(output.clone());
                files.push(generated(&output, &rules[owner].0));
                labels.push(output);
            }
            if rng.bool() {
                groups.push(label("group"));
                labels.push(label("group"));
            }
            // An output file whose rule the dump lacks.
            if rng.u8(..8) == 0 {
                let output = label("orphan.o");
                files.push(generated(&output, &label("gone")));
                labels.push(output);
            }
        }
        let drawn_from = Labels {
            all: &labels,
            groups: &groups,
            absent: (rng.u8(..4) == 0).then_some(ABSENT_GROUP),
        };
        for file in &mut files {
            if let Some(source) = file.source_file.as_mut() {
                let declared = visibility_labels(rng, &drawn_from);
                source.package_group = visibility_dependencies(&declared);
                source.visibility_label = declared;
            }
        }
        for group in &groups {
            let mut includes: Vec<String> = (0..rng.usize(..3))
                .map(|_| match drawn_from.absent {
                    Some(absent) if rng.u8(..8) == 0 => absent.to_string(),
                    _ => groups[rng.usize(..groups.len())].clone(),
                })
                .filter(|included| included != group)
                .collect();
            includes.sort();
            includes.dedup();
            let contained = (0..rng.usize(..MAX_CONTAINED))
                .map(|_| contained_package(rng, &packages))
                .collect();
            files.push(package_group(group, contained, includes));
        }
        let mut targets: Vec<Target> = Vec::with_capacity(rules.len() + files.len());
        let mut deps: HashMap<String, Vec<String>> = HashMap::new();
        let mut explicit_deps: HashMap<String, Vec<String>> = HashMap::new();
        // By rule, the package groups its visibility names.
        let mut visibilities: HashMap<String, Vec<String>> = HashMap::new();
        let classes: Vec<&str> = (rules.iter())
            .map(|_| RULE_CLASSES[rng.usize(..RULE_CLASSES.len())])
            .collect();
        // The tests and suites, which a suite mostly names.
        let tested: Vec<String> = (rules.iter().zip(&classes))
            .filter(|(_, class)| class.ends_with("_test") || **class == "test_suite")
            .map(|((name, _, _), _)| name.clone())
            .collect();
        for ((name, outputs, tags), &class) in rules.iter().zip(&classes) {
            let tags = match class {
                "test_suite" => many(rng, |rng| pick(rng, SUITE_TAGS)),
                class if class.ends_with("_test") => many(rng, |rng| pick(rng, TEST_TAGS)),
                _ => tags.clone(),
            };
            let (_, short) = name.split_once(':').unwrap();
            let mut attributes = vec![
                Attribute {
                    name: "name".to_string(),
                    r#type: AttributeType::String as i32,
                    string_value: Some(short.to_string()),
                    explicitly_specified: Some(true),
                    ..Default::default()
                },
                string_list("tags", &tags),
            ];
            if rng.u8(..3) == 0 {
                attributes.push(boolean("testonly", rng.bool()));
            }
            match class {
                "test_suite" => attributes.extend(suite_attributes(rng, &tested, &labels)),
                class if class.ends_with("_test") => attributes.push(Attribute {
                    explicitly_specified: Some(rng.bool()),
                    ..Value::String(pick(rng, TEST_SIZES)).attribute("size")
                }),
                _ => {}
            }
            attributes.extend(typed_attributes(rng, &drawn_from));
            attributes.sort_by(|a, b| a.name.cmp(&b.name));
            // A rule's inputs are what its attributes name and what aspects
            // add, which no attribute does.
            let mut inputs: Vec<String> = (0..rng.usize(..3))
                .map(|_| labels[rng.usize(..labels.len())].clone())
                .chain(attributes.iter().flat_map(dependency_labels))
                .filter(|input| input != name)
                .collect();
            if rng.u8(..8) == 0 {
                inputs.push("//zz:absent".to_string());
            }
            inputs.sort();
            inputs.dedup();
            let named_explicitly: Vec<String> = (attributes.iter())
                .filter(|attribute| attribute.explicitly_specified == Some(true))
                .flat_map(dependency_labels)
                .collect();
            targets.push(Target {
                r#type: Discriminator::Rule as i32,
                rule: Some(Rule {
                    name: name.clone(),
                    rule_class: class.to_string(),
                    rule_input: inputs.clone(),
                    rule_output: outputs.clone(),
                    attribute: attributes,
                    ..Default::default()
                }),
                ..Default::default()
            });
            let (visibility, explicit_visibility) =
                visibility_groups(targets.last().unwrap()).unwrap_or_default();
            visibilities.insert(name.clone(), visibility.clone());
            let held = |named: Vec<String>| -> Vec<String> {
                named
                    .into_iter()
                    .filter(|input| labels.contains(input))
                    .collect()
            };
            let explicit_visibility = visibility.iter().filter(|_| explicit_visibility);
            let explicit = named_explicitly
                .into_iter()
                .chain(explicit_visibility.cloned());
            explicit_deps.insert(name.clone(), held(explicit.collect()));
            deps.insert(
                name.clone(),
                held(inputs.into_iter().chain(visibility).collect()),
            );
        }
        for file in &files {
            let (name, depending) = if let Some(generated) = &file.generated_file {
                let rule = &generated.generating_rule;
                let groups = visibilities.get(rule).into_iter().flatten().cloned();
                let depending = std::iter::once(rule.clone()).chain(groups).collect();
                (&generated.name, depending)
            } else if let Some(source) = &file.source_file {
                (&source.name, source.package_group.clone())
            } else if let Some(group) = &file.package_group {
                (&group.name, group.included_package_group.clone())
            } else {
                continue;
            };
            deps.insert(name.clone(), depending.clone());
            explicit_deps.insert(name.clone(), depending);
        }
        targets.extend(files);
        rng.shuffle(&mut targets);
        Graph {
            targets,
            labels,
            packages,
            deps,
            explicit_deps,
        }
    }

    /// Writes the graph as a streamed_proto dump, returning its path.
    fn write(&self, seed: u64) -> PathBuf {
        let directory = std::env::var_os("TEST_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let path = directory.join(format!("graph{seed}.binpb"));
        let mut bytes: Vec<u8> = Vec::new();
        for target in &self.targets {
            target.encode_length_delimited(&mut bytes).unwrap();
        }
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// A target pattern, mostly one naming what the graph holds.
    fn pattern(&self, rng: &mut Rng) -> String {
        let (repo, path) = &self.packages[rng.usize(..self.packages.len())];
        let slash = |path: &str| match path {
            "" => String::new(),
            path => format!("/{path}"),
        };
        match rng.u8(..32) {
            0..=11 => self.labels[rng.usize(..self.labels.len())].clone(),
            12 | 13 => format!("{repo}//{path}:all"),
            14 | 15 => format!("{repo}//{path}:*"),
            16 => format!("{repo}//{path}:all-targets"),
            17 | 18 => format!("{repo}/{}/...", slash(path)),
            19 => format!("{repo}/{}/...:*", slash(path)),
            20 => match path.as_str() {
                "" => format!("{repo}//:all"),
                path => format!("{repo}//{path}"),
            },
            21 | 22 if repo.is_empty() && !path.is_empty() => {
                let file = SOURCE_NAMES[rng.usize(..SOURCE_NAMES.len())];
                [path.clone(), format!("{path}/{file}")][rng.usize(..2)].clone()
            }
            23 => MISSING[rng.usize(..MISSING.len())].to_string(),
            24..=27 => WIDE[rng.usize(..WIDE.len())].to_string(),
            _ => self.labels[rng.usize(..self.labels.len())].clone(),
        }
    }
}

/// The `tests` a drawn test suite names, mostly among `tested`, and its
/// `$implicit_tests`.
fn suite_attributes(rng: &mut Rng, tested: &[String], labels: &[String]) -> [Attribute; 2] {
    let named = |rng: &mut Rng| {
        let count = rng.usize(..4);
        (0..count)
            .map(|_| match rng.u8(..4) {
                0 => label(rng, labels),
                _ => label(rng, tested),
            })
            .collect::<Vec<String>>()
    };
    let tests = named(rng);
    let implicit = match tests.is_empty() {
        true => named(rng),
        false => Vec::new(),
    };
    let implicit_written = tests.is_empty();
    [
        Attribute {
            explicitly_specified: Some(true),
            ..Value::Labels(tests).attribute("tests")
        },
        Attribute {
            explicitly_specified: Some(implicit_written),
            ..Value::Labels(implicit).attribute("$implicit_tests")
        },
    ]
}

/// The package groups a drawn rule's visibility names, as
/// [`visibility_dependencies`] reads them; and whether the BUILD file sets the
/// visibility.
fn visibility_groups(target: &Target) -> Option<(Vec<String>, bool)> {
    let rule = target.rule.as_ref()?;
    let visibility = rule.attribute.iter().find(|a| a.name == "visibility")?;
    let groups = visibility_dependencies(&visibility.string_list_value);
    Some((groups, visibility.explicitly_specified == Some(true)))
}

/// The package groups a visibility declaring `labels` names, as Bazel's
/// `RuleVisibility` reads them, each written without the repository a
/// main-repository label may be spelled with.
fn visibility_dependencies(labels: &[String]) -> Vec<String> {
    let labels: Vec<String> = labels.iter().map(|l| unspelled(l)).collect();
    if labels.iter().any(|label| label == PUBLIC) {
        return Vec::new();
    }
    let direct = |label: &str| label.ends_with(":__pkg__") || label.ends_with(":__subpackages__");
    (labels.into_iter())
        .filter(|label| label != PRIVATE && !direct(label))
        .collect()
}

const PUBLIC: &str = "//visibility:public";
const PRIVATE: &str = "//visibility:private";

/// `label` written without the repository a main-repository label may be
/// spelled with.
fn unspelled(label: &str) -> String {
    match label.strip_prefix("@@//") {
        Some(main) => format!("//{main}"),
        None => label.to_string(),
    }
}

/// The labels `attribute` names as dependencies, each [`unspelled`]: those
/// `AggregatingAttributeMapper.visitLabels` visits of every branch of a select
/// and every condition but the default, and none of an output or nodep
/// attribute.
fn dependency_labels(attribute: &Attribute) -> Vec<String> {
    if attribute.nodep == Some(true) {
        return Vec::new();
    }
    let mut named: Vec<String> = Vec::new();
    match &attribute.selector_list {
        Some(list) => {
            let kind = list.r#type.unwrap();
            for entry in list.elements.iter().flat_map(|selector| &selector.entries) {
                let key = unspelled(entry.label.as_deref().unwrap());
                if key != "//conditions:default" {
                    named.push(key);
                }
                let fields = Attribute {
                    r#type: kind,
                    string_value: entry.string_value.clone(),
                    string_list_value: entry.string_list_value.clone(),
                    label_dict_unary_value: entry.label_dict_unary_value.clone(),
                    label_keyed_string_dict_value: entry.label_keyed_string_dict_value.clone(),
                    string_list_dict_value: entry.string_list_dict_value.clone(),
                    ..Default::default()
                };
                named.extend(value_labels(&fields));
            }
        }
        None => named.extend(value_labels(attribute)),
    }
    named
}

/// The labels the value `attribute` states names as dependencies, each
/// [`unspelled`].
fn value_labels(attribute: &Attribute) -> Vec<String> {
    let labels: Vec<&String> = match AttributeType::try_from(attribute.r#type) {
        Ok(AttributeType::Label) => attribute.string_value.iter().collect(),
        Ok(AttributeType::LabelList) => attribute.string_list_value.iter().collect(),
        Ok(AttributeType::LabelDictUnary) => (attribute.label_dict_unary_value.iter())
            .map(|e| &e.value)
            .collect(),
        Ok(AttributeType::LabelKeyedStringDict) => (attribute.label_keyed_string_dict_value.iter())
            .map(|e| &e.key)
            .collect(),
        Ok(AttributeType::LabelListDict) => (attribute.string_list_dict_value.iter())
            .flat_map(|e| &e.value)
            .collect(),
        _ => Vec::new(),
    };
    labels.into_iter().map(|label| unspelled(label)).collect()
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

fn package_group(name: &str, contained: Vec<String>, includes: Vec<String>) -> Target {
    Target {
        r#type: Discriminator::PackageGroup as i32,
        package_group: Some(PackageGroup {
            name: name.to_string(),
            contained_package: contained,
            included_package_group: includes,
        }),
        ..Default::default()
    }
}

/// An entry of a package group's `packages`, as Bazel writes one by default:
/// every package or none, or one of `packages` or one no target is in alone
/// or with those beneath it, now and then negative.
fn contained_package(rng: &mut Rng, packages: &[(String, String)]) -> String {
    let (repo, path) = match rng.u8(..12) {
        0 => return "public".to_string(),
        1 => return "private".to_string(),
        2 => ("", "zz"),
        _ => {
            let (repo, path) = &packages[rng.usize(..packages.len())];
            (repo.as_str(), path.as_str())
        }
    };
    let written = match (rng.bool(), path) {
        (false, path) => format!("{repo}//{path}"),
        (true, "") => format!("{repo}//..."),
        (true, path) => format!("{repo}//{path}/..."),
    };
    match rng.u8(..3) {
        0 => format!("-{written}"),
        _ => written,
    }
}

/// The conditions a drawn `select` branches on.
const CONDITIONS: &[&str] = &["//c:x", "//c:y", "@@ext+//c:z", "//conditions:default"];
const STRINGS: &[&str] = &["a", "b c", "-x", "=", ""];
const DICT_KEYS: &[&str] = &["K", "L"];
/// Few, so that concatenated selects of a set often repeat an element.
const DISTRIBS: &[&str] = &["a", "b c"];

/// Typed attributes a rule states besides its name, tags and `testonly`, as
/// Bazel writes them by default: some of those a rule may have, some written
/// in its BUILD file and some as `select`s.
fn typed_attributes(rng: &mut Rng, labels: &Labels) -> Vec<Attribute> {
    let mut drawn: Vec<(&str, Draw)> = vec![
        ("srcs", |rng, labels| {
            Value::Labels(many(rng, |rng| label(rng, labels)))
        }),
        ("src", |rng, labels| Value::Label(Some(label(rng, labels)))),
        ("cmd", |rng, _| Value::String(pick(rng, STRINGS))),
        ("shard_count", |rng, _| Value::Int(rng.i32(-2..3))),
        ("ports", |rng, _| {
            Value::Ints(many(rng, |rng| rng.i32(-2..3)))
        }),
        ("linkstatic", |rng, _| Value::Boolean(rng.bool())),
        ("stamp", |rng, _| {
            let tristates = [Tristate::No, Tristate::Yes, Tristate::Auto];
            Value::Tristate(tristates[rng.usize(..tristates.len())])
        }),
        ("copts", |rng, _| {
            Value::Strings(many(rng, |rng| pick(rng, STRINGS)))
        }),
        ("distribs", |rng, _| {
            let at = distinct(rng, DISTRIBS.len());
            Value::StringSet(at.into_iter().map(|at| DISTRIBS[at].to_string()).collect())
        }),
        ("env", |rng, _| {
            let at = distinct(rng, DICT_KEYS.len());
            let entries = at
                .into_iter()
                .map(|at| (DICT_KEYS[at].to_string(), pick(rng, STRINGS)));
            Value::Dict(entries.collect())
        }),
        ("aliases", |rng, labels| {
            let at = distinct(rng, DICT_KEYS.len());
            let entries =
                (at.into_iter()).map(|at| (DICT_KEYS[at].to_string(), label(rng, labels)));
            Value::LabelDict(entries.collect())
        }),
        ("args_by_label", |rng, labels| {
            let at = distinct(rng, labels.len());
            let entries =
                (at.into_iter()).map(|at| (respelled(rng, &labels[at]), pick(rng, STRINGS)));
            Value::LabelKeyedDict(entries.collect())
        }),
        ("flags_by_key", |rng, _| {
            let at = distinct(rng, DICT_KEYS.len());
            let entries = (at.into_iter()).map(|at| {
                (
                    DICT_KEYS[at].to_string(),
                    many(rng, |rng| pick(rng, STRINGS)),
                )
            });
            Value::StringListDict(entries.collect())
        }),
        ("deps_by_key", |rng, labels| {
            let at = distinct(rng, DICT_KEYS.len());
            let entries = (at.into_iter()).map(|at| {
                (
                    DICT_KEYS[at].to_string(),
                    many(rng, |rng| label(rng, labels)),
                )
            });
            Value::LabelListDict(entries.collect())
        }),
        ("compatible_with_one", |rng, labels| {
            Value::NodepLabel(Some(label(rng, labels)))
        }),
        ("out", |rng, labels| Value::Output(label(rng, labels))),
        ("outs", |rng, labels| {
            Value::Outputs(many(rng, |rng| label(rng, labels)))
        }),
        ("visibility", |rng, labels| {
            Value::NodepLabels(visibility_labels(rng, labels))
        }),
        ("licenses", |rng, labels| {
            let types = vec![pick(rng, &["notice", "restricted", "unencumbered"])];
            let at = distinct(rng, labels.len());
            let mut exceptions: Vec<String> = (at.into_iter())
                .map(|at| respelled(rng, &labels[at]))
                .collect();
            exceptions.sort_by_cached_key(|label| label_order(label));
            Value::License(types, exceptions)
        }),
    ];
    rng.shuffle(&mut drawn);
    let stated = rng.usize(..drawn.len() + 1);
    let drawn = drawn.into_iter().take(stated).map(|(name, draw)| {
        // A rule's visibility, licenses and outputs may not be selects.
        let configurable = !matches!(name, "visibility" | "licenses" | "out" | "outs");
        let attribute = match rng.u8(..3) {
            0 if configurable => select(rng, name, labels, draw),
            _ => draw(rng, labels).attribute(name),
        };
        Attribute {
            explicitly_specified: Some(rng.bool()),
            ..attribute
        }
    });
    drawn.collect()
}

/// Draws a value from the labels of a graph.
type Draw = fn(&mut Rng, &Labels) -> Value;

/// The labels of a graph, and those of its package groups.
struct Labels<'a> {
    all: &'a [String],
    groups: &'a [String],
    /// A label visibilities and includes may name that names no target of the
    /// graph, which fails `visible` over a target reaching it.
    absent: Option<&'a str>,
}

impl std::ops::Deref for Labels<'_> {
    type Target = [String];

    fn deref(&self) -> &[String] {
        self.all
    }
}

/// The labels a visibility declares, at least one as Bazel writes them.
fn visibility_labels(rng: &mut Rng, labels: &Labels) -> Vec<String> {
    (0..rng.usize(1..3))
        .map(|_| visibility_label(rng, labels))
        .collect()
}

/// A label a visibility names: mostly a package group, now and then every
/// package or none, one package or a package and those beneath it directly,
/// a target no package group is, or the graph's absent label.
fn visibility_label(rng: &mut Rng, labels: &Labels) -> String {
    if let Some(absent) = labels.absent.filter(|_| rng.u8(..16) == 0) {
        return absent.to_string();
    }
    let package = |rng: &mut Rng| {
        let (package, _) = labels[rng.usize(..labels.len())].rsplit_once(':').unwrap();
        package.to_string()
    };
    match rng.u8(..10) {
        0 => PUBLIC.to_string(),
        1 => PRIVATE.to_string(),
        2 => format!("{}:__pkg__", package(rng)),
        3 => format!("{}:__subpackages__", package(rng)),
        4 => label(rng, labels),
        _ if labels.groups.is_empty() => label(rng, labels),
        _ => {
            let at = rng.usize(..labels.groups.len());
            respelled(rng, &labels.groups[at])
        }
    }
}

/// One of `labels`, now and then a main-repository one spelled with its
/// repository, as `--consistent_labels` writes it.
fn label(rng: &mut Rng, labels: &[String]) -> String {
    let at = rng.usize(..labels.len());
    respelled(rng, &labels[at])
}

/// `label`, now and then spelled with its repository if a main-repository one.
fn respelled(rng: &mut Rng, label: &str) -> String {
    match label.strip_prefix("//") {
        Some(main) if rng.u8(..3) == 0 => format!("@@//{main}"),
        _ => label.to_string(),
    }
}

/// The order Java sorts labels in (`Label.compareTo`): by repository, then
/// package, then name.
fn label_order(label: &str) -> (String, String, String) {
    let label = label
        .strip_prefix("@@//")
        .map_or(label.to_string(), |main| format!("//{main}"));
    let (repo, rest) = label.split_once("//").unwrap();
    let (package, name) = rest.split_once(':').unwrap();
    (repo.to_string(), package.to_string(), name.to_string())
}

/// Up to two distinct positions in a list `len` long.
fn distinct(rng: &mut Rng, len: usize) -> Vec<usize> {
    let mut at: Vec<usize> = (0..len).collect();
    rng.shuffle(&mut at);
    at.truncate(rng.usize(..3.min(len + 1)));
    at
}

fn pick(rng: &mut Rng, from: &[&str]) -> String {
    from[rng.usize(..from.len())].to_string()
}

fn many<T>(rng: &mut Rng, mut draw: impl FnMut(&mut Rng) -> T) -> Vec<T> {
    (0..rng.usize(..3)).map(|_| draw(rng)).collect()
}

/// A drawn attribute value, of the type it is written as.
enum Value {
    Int(i32),
    Ints(Vec<i32>),
    Boolean(bool),
    Tristate(Tristate),
    String(String),
    /// `None` for no label, as a `select` branch of `None` writes one.
    Label(Option<String>),
    /// A label no dependency is drawn through, `None` as for [`Value::Label`].
    NodepLabel(Option<String>),
    Output(String),
    Strings(Vec<String>),
    /// A `DISTRIBUTION_SET`, its elements distinct.
    StringSet(Vec<String>),
    Labels(Vec<String>),
    NodepLabels(Vec<String>),
    Outputs(Vec<String>),
    Dict(Vec<(String, String)>),
    LabelDict(Vec<(String, String)>),
    LabelKeyedDict(Vec<(String, String)>),
    StringListDict(Vec<(String, Vec<String>)>),
    LabelListDict(Vec<(String, Vec<String>)>),
    /// The license types, and the exceptions in the order Java sorts them.
    License(Vec<String>, Vec<String>),
}

impl Value {
    fn discriminator(&self) -> AttributeType {
        match self {
            Value::Int(_) => AttributeType::Integer,
            Value::Ints(_) => AttributeType::IntegerList,
            Value::Boolean(_) => AttributeType::Boolean,
            Value::Tristate(_) => AttributeType::Tristate,
            Value::String(_) | Value::NodepLabel(_) => AttributeType::String,
            Value::Label(_) => AttributeType::Label,
            Value::Output(_) => AttributeType::Output,
            Value::Strings(_) | Value::NodepLabels(_) => AttributeType::StringList,
            Value::StringSet(_) => AttributeType::DistributionSet,
            Value::Labels(_) => AttributeType::LabelList,
            Value::Outputs(_) => AttributeType::OutputList,
            Value::Dict(_) => AttributeType::StringDict,
            Value::LabelDict(_) => AttributeType::LabelDictUnary,
            Value::LabelKeyedDict(_) => AttributeType::LabelKeyedStringDict,
            Value::StringListDict(_) => AttributeType::StringListDict,
            Value::LabelListDict(_) => AttributeType::LabelListDict,
            Value::License(..) => AttributeType::License,
        }
    }

    fn nodep(&self) -> Option<bool> {
        matches!(self, Value::NodepLabel(_) | Value::NodepLabels(_)).then_some(true)
    }

    /// The default of this value's type, which a `select` branch of `None`
    /// takes and Bazel writes.
    fn none(&self) -> Value {
        match self {
            Value::Int(_) => Value::Int(0),
            Value::Ints(_) => Value::Ints(Vec::new()),
            Value::Boolean(_) => Value::Boolean(false),
            Value::Tristate(_) => Value::Tristate(Tristate::Auto),
            Value::String(_) => Value::String(String::new()),
            Value::Label(_) | Value::Output(_) => Value::Label(None),
            Value::NodepLabel(_) => Value::NodepLabel(None),
            Value::Strings(_) => Value::Strings(Vec::new()),
            Value::StringSet(_) => Value::StringSet(Vec::new()),
            Value::Labels(_) => Value::Labels(Vec::new()),
            Value::NodepLabels(_) => Value::NodepLabels(Vec::new()),
            Value::Outputs(_) => Value::Outputs(Vec::new()),
            Value::Dict(_) => Value::Dict(Vec::new()),
            Value::LabelDict(_) => Value::LabelDict(Vec::new()),
            Value::LabelKeyedDict(_) => Value::LabelKeyedDict(Vec::new()),
            Value::StringListDict(_) => Value::StringListDict(Vec::new()),
            Value::LabelListDict(_) => Value::LabelListDict(Vec::new()),
            Value::License(..) => Value::License(vec!["none".to_string()], Vec::new()),
        }
    }

    fn attribute(self, name: &str) -> Attribute {
        let mut attribute = Attribute {
            name: name.to_string(),
            r#type: self.discriminator() as i32,
            nodep: self.nodep(),
            ..Default::default()
        };
        match self {
            Value::Int(value) => attribute.int_value = Some(value),
            Value::Ints(values) => attribute.int_list_value = values,
            Value::Boolean(value) => attribute.boolean_value = Some(value),
            Value::Tristate(value) => attribute.tristate_value = Some(value as i32),
            Value::String(value) | Value::Output(value) => attribute.string_value = Some(value),
            Value::Label(value) | Value::NodepLabel(value) => attribute.string_value = value,
            Value::Strings(values)
            | Value::StringSet(values)
            | Value::Labels(values)
            | Value::NodepLabels(values)
            | Value::Outputs(values) => attribute.string_list_value = values,
            Value::Dict(entries) => {
                attribute.string_dict_value = (entries.into_iter())
                    .map(|(key, value)| StringDictEntry { key, value })
                    .collect()
            }
            Value::LabelDict(entries) => {
                attribute.label_dict_unary_value = (entries.into_iter())
                    .map(|(key, value)| LabelDictUnaryEntry { key, value })
                    .collect()
            }
            Value::LabelKeyedDict(entries) => {
                attribute.label_keyed_string_dict_value = (entries.into_iter())
                    .map(|(key, value)| LabelKeyedStringDictEntry { key, value })
                    .collect()
            }
            // Bazel writes a label-list dict into the string-list dict field.
            Value::StringListDict(entries) | Value::LabelListDict(entries) => {
                attribute.string_list_dict_value = (entries.into_iter())
                    .map(|(key, value)| StringListDictEntry { key, value })
                    .collect()
            }
            Value::License(types, exceptions) => {
                attribute.license = Some(License {
                    license_type: types,
                    exception: exceptions,
                })
            }
        }
        attribute
    }

    /// This value as a `select` branch on `condition`, `None` where `none`.
    fn branch(self, condition: &str, none: bool) -> SelectorEntry {
        let attribute = self.attribute("");
        SelectorEntry {
            label: Some(condition.to_string()),
            is_default_value: Some(none),
            int_value: attribute.int_value,
            string_value: attribute.string_value,
            boolean_value: attribute.boolean_value,
            tristate_value: attribute.tristate_value,
            string_list_value: attribute.string_list_value,
            int_list_value: attribute.int_list_value,
            string_dict_value: attribute.string_dict_value,
            label_dict_unary_value: attribute.label_dict_unary_value,
            label_keyed_string_dict_value: attribute.label_keyed_string_dict_value,
            string_list_dict_value: attribute.string_list_dict_value,
            ..Default::default()
        }
    }
}

/// The attribute `name` written as a `select`, or for a type that
/// concatenates up to three, a later one now and then on the same conditions
/// as the first, each branch drawn by `draw` or `None`. Its conditions are
/// drawn from [`CONDITIONS`] and one target of the graph, which the select
/// then depends on.
fn select(rng: &mut Rng, name: &str, labels: &Labels, draw: Draw) -> Attribute {
    let kind = draw(rng, labels);
    let target = labels[rng.usize(..labels.len())].clone();
    let pool: Vec<&str> = CONDITIONS
        .iter()
        .copied()
        .chain([target.as_str()])
        .collect();
    let mut first: Vec<&str> = pool.clone();
    rng.shuffle(&mut first);
    first.truncate(rng.usize(1..pool.len() + 1));
    // Bazel concatenates selects only of a type that concatenates.
    let concatenates = !matches!(
        kind,
        Value::Boolean(_) | Value::Tristate(_) | Value::Label(_) | Value::NodepLabel(_)
    );
    let count = match concatenates {
        true => rng.usize(1..4),
        false => 1,
    };
    let selects = (0..count).map(|at| {
        let conditions = match at > 0 && rng.bool() {
            true => first.clone(),
            false => {
                let mut conditions = pool.clone();
                rng.shuffle(&mut conditions);
                conditions.truncate(rng.usize(1..pool.len() + 1));
                conditions
            }
        };
        let entries = conditions.into_iter().map(|condition| match rng.u8(..4) {
            0 => kind.none().branch(condition, true),
            _ => draw(rng, labels).branch(condition, false),
        });
        Selector {
            entries: entries.collect(),
            ..Default::default()
        }
    });
    Attribute {
        name: name.to_string(),
        r#type: AttributeType::SelectorList as i32,
        nodep: kind.nodep(),
        selector_list: Some(SelectorList {
            r#type: Some(kind.discriminator() as i32),
            elements: selects.collect(),
        }),
        ..Default::default()
    }
}

fn string_list(name: &str, values: &[String]) -> Attribute {
    Attribute {
        name: name.to_string(),
        r#type: AttributeType::StringList as i32,
        string_list_value: values.to_vec(),
        ..Default::default()
    }
}

fn boolean(name: &str, value: bool) -> Attribute {
    Attribute {
        name: name.to_string(),
        r#type: AttributeType::Boolean as i32,
        boolean_value: Some(value),
        ..Default::default()
    }
}

/// An expression drawn at random, and how its answer is checked.
struct Drawn {
    text: String,
    check: Check,
}

enum Check {
    /// The two engines name the same targets.
    Same,
    /// `somepath(from, to)`, whose arguments are asked of Bazel as well.
    SomePath { from: String, to: String },
    /// `some(of, count)`, whose argument is asked of Bazel as well.
    Some { of: String, count: Option<i32> },
}

impl Drawn {
    fn draw(rng: &mut Rng, graph: &Graph) -> Self {
        let mut draw = Drawer { rng, graph };
        match draw.rng.u8(..10) {
            0 | 1 => {
                let (from, to) = (draw.expr(MAX_DEPTH - 1), draw.expr(MAX_DEPTH - 1));
                Drawn {
                    text: format!("somepath({from}, {to})"),
                    check: Check::SomePath { from, to },
                }
            }
            2 => {
                let of = draw.expr(MAX_DEPTH - 1);
                let count = draw.rng.bool().then(|| draw.rng.i32(-1..4));
                let text = match count {
                    Some(count) => format!("some({of}, {})", int(count)),
                    None => format!("some({of})"),
                };
                Drawn {
                    text,
                    check: Check::Some { of, count },
                }
            }
            _ => Drawn {
                text: draw.expr(MAX_DEPTH),
                check: Check::Same,
            },
        }
    }

    /// The expressions asked of Bazel for this one, itself first.
    fn asked(&self) -> impl Iterator<Item = &str> {
        let arguments: Vec<&str> = match &self.check {
            Check::Same => Vec::new(),
            Check::SomePath { from, to } => vec![from, to],
            Check::Some { of, .. } => vec![of],
        };
        std::iter::once(self.text.as_str()).chain(arguments)
    }
}

/// An integer argument as a query writes it, a negative one quoted, since the
/// lexer reads a bare `-` as the operator.
fn int(value: i32) -> String {
    match value < 0 {
        true => format!("'{value}'"),
        false => value.to_string(),
    }
}

struct Drawer<'a> {
    rng: &'a mut Rng,
    graph: &'a Graph,
}

/// The patterns `kind`, `filter` and `attr` are drawn with: constructs Java's
/// and the `regex` crate's engines read alike over ASCII text, which leaves out
/// look-around, backreferences, possessive quantifiers, atomic groups and
/// `\Q…\E` quoting.
const KIND_PATTERNS: &[&str] = &[
    "rule",
    "file",
    "source",
    "gen.*",
    "^genrule rule$",
    "_test",
    "package",
    "java|sh",
    "e r",
    "d f",
    "^s",
    "",
];
const LABEL_PATTERNS: &[&str] = &[
    "lib",
    "\\.txt$",
    "^@@ext",
    "//a/b",
    ":a",
    "out\\d",
    "a.*b",
    "^//a:",
    "x/y",
    "[.]json",
    "(?i)APP",
    "\\w+\\.o$",
];
const ATTRIBUTE_PATTERNS: &[(&str, &[&str])] = &[
    (
        "tags",
        &["manual", "^\\[\\]$", "local", "\\[manual\\]", "", ", "],
    ),
    ("testonly", &["1", "0", "", "^1$"]),
    ("name", &["^a", "lib|app", "t$", "", "^[a-c]$"]),
    (
        "srcs",
        &["^\\[\\]$", "//a", "@@", "\\.txt", ", //", "^\\[//"],
    ),
    ("src", &["//", "^//a:", "@@ext\\+", "out\\d", ""]),
    ("cmd", &["^$", "a", "b c", "-x", "=", "^a"]),
    ("shard_count", &["^0$", "^-", "1", "2"]),
    ("stamp", &["-1", "^0$", "1"]),
    ("ports", &["^\\[\\]$", "-", "1, ", "\\[0"]),
    ("linkstatic", &["^0$", "^1$"]),
    (
        "distribs",
        &["^\\[\\]$", "a, ", "a, a", "b c, b c", "^\\[b"],
    ),
    ("aliases", &["^\\{\\}$", "K=//", "=@@", "L="]),
    (
        "args_by_label",
        &["^\\{\\}$", "\\{//", "@@", "=b c", ", //"],
    ),
    ("flags_by_key", &["^\\{\\}$", "K=\\[\\]", "=\\[a", "-x\\]"]),
    ("deps_by_key", &["^\\{\\}$", "K=\\[//", "@@", ", L="]),
    ("compatible_with_one", &["^//", "@@ext\\+", ":a$", ""]),
    ("out", &["^//", ":out", "@@"]),
    ("outs", &["^\\[\\]$", "//", ", "]),
    ("copts", &["^\\[\\]$", "-x", "a, ", "\\[=\\]", "b c"]),
    ("env", &["^\\{\\}$", "K=", "L=b c", "=, ", "\\{K"]),
    ("visibility", &["^\\[\\]$", "//", "@@", "\\]$"]),
    (
        "licenses",
        &["notice", "^\\[restricted\\]$", "exceptions", "//a"],
    ),
    ("no_such_attribute", &["", "x"]),
];

/// The attributes `labels` is drawn for: those of every type the graphs
/// drawn state, and one no rule states.
const LABEL_ATTRIBUTES: &[&str] = &[
    "srcs",
    "src",
    "out",
    "outs",
    "aliases",
    "args_by_label",
    "deps_by_key",
    "compatible_with_one",
    "visibility",
    "licenses",
    "tests",
    "$implicit_tests",
    "cmd",
    "tags",
    "no_such_attribute",
];

/// The variables a drawn `let` binds; a reference to one may fall outside it.
const VARIABLES: &[&str] = &["v", "w"];

impl Drawer<'_> {
    fn expr(&mut self, depth: u32) -> String {
        let choice = if depth == 0 { 0 } else { self.rng.u8(..12) };
        match choice {
            0 | 1 => self.literal(),
            2 | 3 => {
                let mut text = self.expr(depth - 1);
                for _ in 0..self.rng.usize(1..3) {
                    let ops = ["+", "-", "^", "union", "except", "intersect"];
                    let op = ops[self.rng.usize(..ops.len())];
                    text = format!("{text} {op} {}", self.expr(depth - 1));
                }
                text
            }
            4 => format!("({})", self.expr(depth - 1)),
            5 => {
                let variable = VARIABLES[self.rng.usize(..VARIABLES.len())];
                let value = self.expr(depth - 1);
                let body = match self.rng.bool() {
                    true => format!("${variable} + {}", self.expr(depth - 1)),
                    false => self.expr(depth - 1),
                };
                format!("let {variable} = {value} in {body}")
            }
            6 => {
                let words: Vec<String> = (0..self.rng.usize(..3))
                    .map(|_| self.graph.pattern(self.rng))
                    .collect();
                format!("set({})", words.join(" "))
            }
            _ => self.call(depth),
        }
    }

    fn literal(&mut self) -> String {
        match self.rng.u8(..24) {
            0 => format!("${}", VARIABLES[self.rng.usize(..VARIABLES.len())]),
            _ => self.graph.pattern(self.rng),
        }
    }

    fn depth(&mut self) -> String {
        match self.rng.u8(..8) {
            0 => int(-1),
            _ => int(self.rng.i32(0..4)),
        }
    }

    /// One of `patterns`, quoted.
    fn pick(&mut self, patterns: &[&str]) -> String {
        format!("'{}'", patterns[self.rng.usize(..patterns.len())])
    }

    fn call(&mut self, depth: u32) -> String {
        let mut arg = || self.expr(depth - 1);
        let (one, two) = (arg(), arg());
        let bounded = self.rng.bool();
        let depth = self.depth();
        match self.rng.u8(..12) {
            0 if bounded => format!("deps({one}, {depth})"),
            0 => format!("deps({one})"),
            1 if bounded => format!("allrdeps({one}, {depth})"),
            1 => format!("allrdeps({one})"),
            2 if bounded => format!("rdeps({one}, {two}, {depth})"),
            2 => format!("rdeps({one}, {two})"),
            3 => format!("allpaths({one}, {two})"),
            4 => format!("siblings({one})"),
            5 => format!("same_pkg_direct_rdeps({one})"),
            6 => format!("kind({}, {one})", self.pick(KIND_PATTERNS)),
            7 => format!("filter({}, {one})", self.pick(LABEL_PATTERNS)),
            8 => {
                let (attribute, patterns) =
                    ATTRIBUTE_PATTERNS[self.rng.usize(..ATTRIBUTE_PATTERNS.len())];
                format!("attr({attribute}, {}, {one})", self.pick(patterns))
            }
            9 => format!("labels({}, {one})", self.pick(LABEL_ATTRIBUTES)),
            // Mostly by one target over a wide pattern, as a seer in more
            // than a package or two leaves little visible.
            10 => {
                let labels = &self.graph.labels;
                let seer = match self.rng.u8(..4) {
                    0 => one,
                    _ => labels[self.rng.usize(..labels.len())].clone(),
                };
                let seen = match self.rng.bool() {
                    true => WIDE[self.rng.usize(..WIDE.len())].to_string(),
                    false => two,
                };
                format!("visible({seer}, {seen})")
            }
            // Over the suites of a wide pattern, now and then, so that what
            // suites admit is often what names a test.
            _ if self.rng.bool() => {
                let wide = WIDE[self.rng.usize(..WIDE.len())];
                format!("tests(kind(test_suite, {wide}))")
            }
            _ => format!("tests({one})"),
        }
    }
}

/// bviz's engine over one graph.
struct Ours<'a> {
    universe: &'a Universe,
    /// The labels each target depends on in the graph the universe walks.
    deps: &'a HashMap<String, Vec<String>>,
}

impl Ours<'_> {
    /// bviz's answer to `expr`, written as Bazel's oracle writes its own.
    fn answer(&self, expr: &str) -> String {
        let parsed = match parse(expr) {
            Ok(parsed) => parsed,
            Err(error) => return format!("error\t{}", error.message),
        };
        match self.universe.evaluate(&parsed) {
            Ok(value) => {
                let named = value.ones().map(|id| {
                    let id = bviz_graph_layout::Id(id as u32);
                    self.universe
                        .label(id)
                        .expect("a query graph labels every node")
                });
                let labels: BTreeSet<&str> = named.collect();
                let labels: Vec<&str> = labels.into_iter().collect();
                format!("ok\t{}", labels.join(" "))
            }
            Err(error) => format!("error\t{}", error.message),
        }
    }

    /// Whether bviz answers `drawn` as Bazel could, Bazel having answered
    /// `bazel` to [`Drawn::asked`].
    fn check(&self, drawn: &Drawn, bazel: &[&str]) -> Result<(), String> {
        let asked: Vec<&str> = drawn.asked().collect();
        let ours: Vec<String> = asked.iter().map(|expr| self.answer(expr)).collect();
        for (at, (ours, bazel)) in ours.iter().zip(bazel).enumerate().skip(1) {
            if ours != bazel {
                return Err(format!(
                    "{}\n  bazel: {bazel:?}\n  bviz:  {ours:?}",
                    asked[at]
                ));
            }
        }
        let differ = || Err(format!("bazel: {:?}\n  bviz:  {:?}", bazel[0], ours[0]));
        let (Some(ours_named), Some(bazel_named)) = (named(&ours[0]), named(bazel[0])) else {
            return match ours[0] == bazel[0] {
                true => Ok(()),
                false => differ(),
            };
        };
        match &drawn.check {
            Check::Same if ours_named == bazel_named => Ok(()),
            Check::Same => differ(),
            Check::SomePath { .. } => {
                let from = named(bazel[1]).unwrap_or_default();
                let to = named(bazel[2]).unwrap_or_default();
                let fewest = self.fewest_edges(&from, &to);
                let path_of_fewest = |named: &BTreeSet<&str>| match fewest {
                    None => named.is_empty(),
                    Some(edges) => named.len() == edges + 1 && self.joins(named, &from, &to),
                };
                match path_of_fewest(&ours_named) && bazel_named.is_empty() != fewest.is_some() {
                    true => Ok(()),
                    false => differ(),
                }
            }
            Check::Some { count, .. } => {
                let of = named(bazel[1]).unwrap_or_default();
                let taken = usize::try_from(count.unwrap_or(1)).unwrap_or(0);
                let fits = ours_named.is_subset(&of)
                    && ours_named.len() == taken.min(of.len())
                    && ours_named.len() == bazel_named.len();
                match fits {
                    true => Ok(()),
                    false => differ(),
                }
            }
        }
    }

    /// The fewest edges a path from one of `from` down to one of `to` takes.
    fn fewest_edges(&self, from: &BTreeSet<&str>, to: &BTreeSet<&str>) -> Option<usize> {
        let mut fewest: HashMap<&str, usize> = from.iter().map(|&label| (label, 0)).collect();
        let mut open: VecDeque<&str> = from.iter().copied().collect();
        while let Some(at) = open.pop_front() {
            if to.contains(at) {
                return Some(fewest[at]);
            }
            for dep in self.deps.get(at).into_iter().flatten() {
                if !fewest.contains_key(dep.as_str()) {
                    fewest.insert(dep, fewest[at] + 1);
                    open.push_back(dep);
                }
            }
        }
        None
    }

    /// Whether `path` holds a path from one of `from` down to one of `to`
    /// running through `path` alone.
    fn joins(&self, path: &BTreeSet<&str>, from: &BTreeSet<&str>, to: &BTreeSet<&str>) -> bool {
        let mut seen: HashSet<&str> = path.intersection(from).copied().collect();
        let mut open: Vec<&str> = seen.iter().copied().collect();
        while let Some(at) = open.pop() {
            if to.contains(at) {
                return true;
            }
            for dep in self.deps.get(at).into_iter().flatten() {
                if path.contains(dep.as_str()) && seen.insert(dep) {
                    open.push(dep);
                }
            }
        }
        false
    }
}

/// The labels an answer names, `None` for an error.
fn named(answer: &str) -> Option<BTreeSet<&str>> {
    let labels = answer.strip_prefix("ok\t")?;
    Some(
        labels
            .split(' ')
            .filter(|label| !label.is_empty())
            .collect(),
    )
}
