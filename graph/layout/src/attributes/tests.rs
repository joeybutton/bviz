use bazel_query_proto::blaze_query::attribute::Discriminator as AttributeType;

use super::*;
use crate::test_support::{
    boolean, explicit, int, label, label_list, license, nodep_label_list, selector_list, string,
    string_dict, string_list, tristate,
};

/// A table of `rules`, each node `Id(i)` stating the `i`th list of
/// attributes as written, unsorted.
fn table(rules: &[Vec<Attribute>]) -> Attributes {
    let mut builder = AttributesBuilder::default();
    for (node, attributes) in rules.iter().enumerate() {
        let rule = Rule {
            attribute: attributes.clone(),
            ..Default::default()
        };
        builder.describe(Id(node as u32), &rule, &RepoMapping::default());
    }
    builder.seal()
}

/// Every string `attr` matches the attribute `name` of `node` against.
fn printed(table: &Attributes, node: Id, name: &str) -> Vec<String> {
    let mut strings = Vec::new();
    let Some(name) = table.name(name) else {
        return strings;
    };
    if let Stated::Row(row) = table.stated(node, Some(name)) {
        table.any_printed(&table.row(row).value, |string| {
            strings.push(string.to_string());
            false
        });
    }
    strings
}

/// The strings `attribute`, the one attribute of a rule, prints as.
fn print(attribute: Attribute) -> Vec<String> {
    let name = attribute.name.clone();
    printed(&table(&[vec![attribute]]), Id(0), &name)
}

#[test]
fn a_scalar_prints_as_bazel_prints_it_for_attr() {
    assert_eq!(print(int("shard_count", -3)), ["-3"]);
    assert_eq!(print(string("cmd", "echo $@")), ["echo $@"]);
    assert_eq!(print(boolean("testonly", true)), ["1"]);
    assert_eq!(print(boolean("testonly", false)), ["0"]);
    assert_eq!(print(tristate("stamp", Tristate::Auto)), ["-1"]);
    assert_eq!(print(tristate("stamp", Tristate::No)), ["0"]);
    assert_eq!(print(tristate("stamp", Tristate::Yes)), ["1"]);
}

#[test]
fn a_label_prints_in_canonical_form() {
    assert_eq!(print(label("src", "@@//a:b")), ["//a:b"]);
    assert_eq!(print(label("src", "@@rules_x+//a:b")), ["@@rules_x+//a:b"]);
    assert_eq!(
        print(nodep_label_list("visibility", &["@@//visibility:public"])),
        ["[//visibility:public]"]
    );
}

#[test]
fn a_collection_prints_as_java_prints_it() {
    assert_eq!(print(string_list("tags", &["a", "b"])), ["[a, b]"]);
    assert_eq!(print(string_list("tags", &[])), ["[]"]);
    assert_eq!(
        print(label_list("deps", &["//a:b", "@@//c:d"])),
        ["[//a:b, //c:d]"]
    );
    let ints = Attribute {
        name: "ints".to_string(),
        r#type: AttributeType::IntegerList as i32,
        int_list_value: vec![1, -2],
        ..Default::default()
    };
    assert_eq!(print(ints), ["[1, -2]"]);
    assert_eq!(
        print(string_dict("env", &[("B", "1"), ("A", "x=y")])),
        ["{B=1, A=x=y}"]
    );
    let label_list_dict = Attribute {
        name: "resources".to_string(),
        r#type: AttributeType::LabelListDict as i32,
        string_list_dict_value: vec![StringListDictEntry {
            key: "k".to_string(),
            value: vec!["@@//a:b".to_string(), "//c:d".to_string()],
        }],
        ..Default::default()
    };
    assert_eq!(print(label_list_dict), ["{k=[//a:b, //c:d]}"]);
}

#[test]
fn a_license_prints_its_types_in_lower_case_and_its_exceptions() {
    assert_eq!(print(license("licenses", &["NOTICE"], &[])), ["[notice]"]);
    assert_eq!(
        print(license("licenses", &["RESTRICTED"], &["@@//a:B"])),
        ["[restricted] with exceptions [//a:B]"]
    );
}

#[test]
fn an_attribute_stated_without_a_value_prints_nothing() {
    let unset = Attribute {
        name: "src".to_string(),
        r#type: AttributeType::Label as i32,
        ..Default::default()
    };
    assert!(print(unset).is_empty());
}

/// A label attribute's `None` branch, which Bazel writes with no value.
fn no_label() -> Attribute {
    Attribute {
        r#type: AttributeType::Label as i32,
        ..Default::default()
    }
}

#[test]
fn a_select_prints_one_string_per_branch() {
    let select = selector_list(
        "srcs",
        &label_list("", &[]),
        &[&[
            ("@@//c:x", Some(label_list("", &["//a:x"]))),
            ("@@//conditions:default", None),
        ]],
    );
    assert_eq!(print(select), ["[//a:x]", "[]"]);
}

#[test]
fn a_select_branch_of_none_for_a_label_prints_nothing() {
    let select = selector_list(
        "src",
        &no_label(),
        &[&[
            ("//c:x", Some(label("", "//a:x"))),
            ("//conditions:default", None),
        ]],
    );
    assert_eq!(print(select), ["//a:x"]);
}

#[test]
fn concatenated_selects_print_every_combination_of_their_branches() {
    let mut strings = print(selector_list(
        "copts",
        &string_list("", &[]),
        &[
            &[("//conditions:default", Some(string_list("", &["-O"])))],
            &[
                ("//c:x", Some(string_list("", &["-x"]))),
                ("//c:y", Some(string_list("", &["-y"]))),
            ],
            &[
                ("//c:p", Some(string_list("", &["-p"]))),
                ("//c:q", Some(string_list("", &["-q"]))),
            ],
        ],
    ));
    strings.sort();
    assert_eq!(
        strings,
        [
            "[-O, -x, -p]",
            "[-O, -x, -q]",
            "[-O, -y, -p]",
            "[-O, -y, -q]"
        ]
    );
}

#[test]
fn selects_on_the_same_conditions_take_the_same_branch() {
    let branches = |x: &str, y: &str| {
        [
            ("//c:x", Some(string_list("", &[x]))),
            ("//c:y", Some(string_list("", &[y]))),
        ]
    };
    let (first, second) = (branches("1", "2"), branches("3", "4"));
    let mut strings = print(selector_list(
        "copts",
        &string_list("", &[]),
        &[&first, &second],
    ));
    strings.sort();
    assert_eq!(strings, ["[1, 3]", "[2, 4]"]);
}

#[test]
fn concatenated_strings_and_integers_join_and_add() {
    let strings = selector_list(
        "cmd",
        &string("", ""),
        &[
            &[("//conditions:default", Some(string("", "a")))],
            &[("//c:x", Some(string("", "b"))), ("//c:y", None)],
        ],
    );
    let mut printed = print(strings);
    printed.sort();
    assert_eq!(printed, ["a", "ab"]);
    let integers = selector_list(
        "shard_count",
        &int("", 0),
        &[
            &[("//conditions:default", Some(int("", i32::MAX)))],
            &[("//conditions:default", Some(int("", 2)))],
        ],
    );
    assert_eq!(print(integers), [(i32::MAX.wrapping_add(2)).to_string()]);
}

#[test]
fn concatenated_dicts_keep_each_key_where_first_put_with_its_last_value() {
    let select = selector_list(
        "env",
        &string_dict("", &[]),
        &[
            &[(
                "//conditions:default",
                Some(string_dict("", &[("A", "1"), ("B", "2")])),
            )],
            &[(
                "//conditions:default",
                Some(string_dict("", &[("C", "3"), ("A", "4")])),
            )],
        ],
    );
    assert_eq!(print(select), ["{A=4, B=2, C=3}"]);
}

/// A `DISTRIBUTION_SET` holding `values`.
fn string_set(values: &[&str]) -> Attribute {
    Attribute {
        r#type: AttributeType::DistributionSet as i32,
        ..string_list("", values)
    }
}

#[test]
fn concatenated_string_sets_keep_each_element_once_where_first_added() {
    let select = selector_list(
        "distribs",
        &string_set(&[]),
        &[
            &[("//conditions:default", Some(string_set(&["web", "client"])))],
            &[("//conditions:default", Some(string_set(&["client", "x"])))],
        ],
    );
    assert_eq!(print(select), ["[web, client, x]"]);
}

#[test]
fn concatenated_booleans_print_nothing() {
    let select = selector_list(
        "testonly",
        &boolean("", false),
        &[
            &[("//conditions:default", Some(boolean("", true)))],
            &[("//conditions:default", Some(boolean("", true)))],
        ],
    );
    assert!(print(select).is_empty());
}

#[test]
fn the_attributes_after_the_rules_own_and_the_synthetic_hashes_are_no_attributes() {
    let table = table(&[vec![
        string("deps", "x"),
        string("name", "lib"),
        string("$internal_attr_hash", "f00"),
        string("$rule_implementation_hash", "ba5"),
        string("$aspect_attr", "y"),
        string("zz_aspect_attr", "z"),
    ]]);

    assert_eq!(printed(&table, Id(0), "name"), ["lib"]);
    for name in [
        "$internal_attr_hash",
        "$rule_implementation_hash",
        "$aspect_attr",
    ] {
        assert!(table.name(name).is_none(), "{name}");
    }
}

#[test]
fn a_rule_stating_none_of_its_own_attributes_has_none_of_its_aspects() {
    let table = table(&[vec![
        string("$rule_implementation_hash", "ba5"),
        string("aspect_attr", "y"),
    ]]);

    assert!(table.name("aspect_attr").is_none());
    assert!(table.rows(Id(0)).is_empty());
}

/// A rule's attributes as Bazel writes them by default: its `name` and the
/// attributes its BUILD file leaves to their defaults besides `attributes`.
fn every_attribute(mut attributes: Vec<Attribute>) -> Vec<Attribute> {
    attributes.push(explicit(string("name", "lib")));
    attributes.push(string_list("tags", &[]));
    attributes.sort_by(|a, b| a.name.cmp(&b.name));
    attributes
}

#[test]
fn a_description_tells_an_attribute_the_rule_lacks_from_one_it_left_out() {
    let table = table(&[
        every_attribute(vec![explicit(string("cmd", "x"))]),
        // As `--noproto:default_values` writes it.
        vec![
            explicit(string("cmd", "y")),
            explicit(string("name", "lib")),
        ],
        // As `--proto:output_rule_attrs=tags` writes it.
        vec![string_list("tags", &[])],
        every_attribute(Vec::new()),
    ]);
    let [cmd, tags] = ["cmd", "tags"].map(|name| Some(table.name(name).unwrap()));

    assert!(matches!(table.stated(Id(0), cmd), Stated::Row(_)));
    assert!(matches!(table.stated(Id(1), cmd), Stated::Row(_)));
    assert_eq!(table.stated(Id(1), tags), Stated::LeftOut);
    assert_eq!(table.stated(Id(2), cmd), Stated::LeftOut);
    assert_eq!(table.stated(Id(3), cmd), Stated::Absent);
    assert_eq!(table.stated(Id(4), cmd), Stated::Undescribed);
}

#[test]
fn rules_stating_one_attribute_alike_share_its_row() {
    let table = table(&[
        vec![explicit(string_list("tags", &["a"])), string("x", "1")],
        vec![explicit(string_list("tags", &["a"])), string("x", "2")],
        vec![string_list("tags", &["a"])],
    ]);

    let [first, second, third] = [0, 1, 2].map(|node| table.rows(Id(node)));
    assert_eq!(first[0], second[0]);
    assert_ne!(first[1], second[1]);
    assert_ne!(
        first[0], third[0],
        "a row is as explicitly specified as stated"
    );
    assert!(table.row(first[0]).explicitly_specified);
    assert!(!table.row(third[0]).explicitly_specified);
}

#[test]
fn renumbering_moves_each_nodes_rows_to_its_new_id() {
    let mut table = table(&[
        every_attribute(vec![string("a", "0")]),
        vec![],
        vec![string("a", "2")],
    ]);

    table.renumber(&[Id(2), Id(0), Id(1)]);

    assert_eq!(printed(&table, Id(2), "a"), ["0"]);
    assert_eq!(printed(&table, Id(1), "a"), ["2"]);
    assert!(table.rows(Id(0)).is_empty());
    let a = table.name("a");
    assert_eq!(
        table.stated(Id(0), a),
        Stated::LeftOut,
        "a node keeps the completeness of its description"
    );
}

#[test]
fn an_attribute_table_is_shared_across_threads() {
    fn shared<T: Send + Sync>() {}
    shared::<Attributes>();
}

/// Every label the attribute `name` of `node` names, as `labels` reads them.
fn named_labels(table: &Attributes, node: Id, name: &str) -> Vec<String> {
    let mut labels = Vec::new();
    if let Stated::Row(row) = table.stated(node, table.name(name)) {
        let mut visit = |label: Sym| labels.push(table.resolve(label).to_string());
        table.row(row).value.for_each_label(&mut visit);
    }
    labels
}

#[test]
fn a_value_names_the_labels_of_every_branch_and_no_condition() {
    let select = selector_list(
        "srcs",
        &label_list("", &[]),
        &[&[
            ("@@//c:x", Some(label_list("", &["//a:x", "@@//a:y"]))),
            ("//conditions:default", None),
        ]],
    );
    let dict = Attribute {
        name: "deps_by_key".to_string(),
        r#type: AttributeType::LabelListDict as i32,
        string_list_dict_value: vec![StringListDictEntry {
            key: "K".to_string(),
            value: vec!["//a:z".to_string()],
        }],
        ..Default::default()
    };
    let table = table(&[vec![
        dict,
        license("licenses", &["NOTICE"], &["//a:e"]),
        select,
        string_list("tags", &["//a:t"]),
    ]]);

    assert_eq!(named_labels(&table, Id(0), "srcs"), ["//a:x", "//a:y"]);
    assert_eq!(named_labels(&table, Id(0), "deps_by_key"), ["//a:z"]);
    assert!(named_labels(&table, Id(0), "tags").is_empty());
    assert!(named_labels(&table, Id(0), "licenses").is_empty());
}
