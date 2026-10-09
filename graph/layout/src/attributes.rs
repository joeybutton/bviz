//! The attributes each rule states, as `--output=proto` writes them, and the
//! strings Bazel's `attr` query function matches for each value
//! (`TargetUtils.getAttrAsString`).
//!
//! A rule's attributes are stated once per description: a target holds the
//! values its query dump states, unconfigured, with every `select` branch; a
//! configured target holds those its cquery dump states, resolved in its
//! configuration. Most rules share most of their values with many others, so
//! each distinct (name, value, explicitly specified) row is held once and a
//! node holds the ids of its rows. Beside them the table holds what a query
//! states of the visibility of the targets that are no rules.

use std::fmt::Write as _;

use bazel_query_proto::blaze_query::attribute::{
    Discriminator, Selector as SelectorPb, SelectorEntry, Tristate,
};
use bazel_query_proto::blaze_query::{
    Attribute, LabelDictUnaryEntry, LabelKeyedStringDictEntry, LabelListDictEntry,
    License as LicensePb, Rule, StringDictEntry, StringListDictEntry,
};
use rustc_hash::FxHashMap;

use crate::facets::{FacetStrings, SparseFacet, SparseFacetBuilder, Sym};
use crate::intern::Id;
use crate::label::RepoMapping;

mod visibility;

use visibility::VisibilitiesBuilder;
pub use visibility::{Named, Package, PackageSpec, Packages, Specified, Visibilities};

/// Attributes a proto states after a rule's own and before its aspects': a
/// hash of the rule's attributes or of its implementation, which `attr`
/// cannot read.
const SYNTHETIC: [&str; 2] = ["$internal_attr_hash", "$rule_implementation_hash"];

/// One value an attribute takes. Strings and labels are interned; a label is
/// held in the canonical form Bazel prints it in.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Value {
    /// Stated with no value: an attribute left unset with no default, or a
    /// `select` branch of `None` for a label.
    Null,
    Integer(i32),
    Boolean(bool),
    Tristate(Tristate),
    String(Sym),
    Label(Sym),
    Strings(Box<[Sym]>),
    /// A `DISTRIBUTION_SET`, which Bazel concatenates as a set.
    StringSet(Box<[Sym]>),
    Labels(Box<[Sym]>),
    Integers(Box<[i32]>),
    StringDict(Box<[(Sym, Sym)]>),
    /// String keys, label values.
    LabelDict(Box<[(Sym, Sym)]>),
    /// Label keys, string values.
    LabelKeyedStringDict(Box<[(Sym, Sym)]>),
    StringListDict(Box<[(Sym, Box<[Sym]>)]>),
    LabelListDict(Box<[(Sym, Box<[Sym]>)]>),
    License(Box<License>),
    /// The concatenation of these selects, each choosing one of its branches.
    Select(Box<[Selector]>),
}

/// A license's types and the labels it makes exceptions of.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct License {
    pub types: Box<[Sym]>,
    pub exceptions: Box<[Sym]>,
}

/// One `select`: its branches in the order written, each the condition label
/// and the value it chooses. A branch of `None` holds the default of the
/// attribute's type, as Bazel substitutes it.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Selector(Box<[(Sym, Value)]>);

/// One attribute as one description of a rule states it.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Row {
    pub name: Sym,
    pub value: Value,
    pub explicitly_specified: bool,
}

/// A [`Row`]'s position in an [`Attributes`] table.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct RowId(u32);

impl RowId {
    /// The position, below the table's [`Attributes::row_count`].
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// What one node's description says of one attribute of the rule.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stated {
    Row(RowId),
    /// The description states every attribute of the rule, and not this one.
    Absent,
    /// The description leaves some attributes of the rule out, this one
    /// among those it does not state.
    LeftOut,
    /// No description states attributes of the node.
    Undescribed,
}

/// The attribute rows of every rule node of one graph, and the
/// [`Visibilities`] of its other targets.
#[derive(Clone)]
pub struct Attributes {
    strings: FacetStrings,
    /// Every name some row carries, in handle order.
    names: Box<[Sym]>,
    rows: Box<[Row]>,
    held: SparseFacet<RowId>,
    /// The nodes some description of which leaves attributes out, ascending.
    incomplete: Vec<Id>,
    visibilities: Visibilities,
}

impl Default for Attributes {
    fn default() -> Self {
        AttributesBuilder::default().seal()
    }
}

impl Attributes {
    pub fn resolve(&self, string: Sym) -> &str {
        self.strings.resolve(string)
    }

    /// The handle of the attribute name `name`, where some rule states it.
    pub fn name(&self, name: &str) -> Option<Sym> {
        let name = self.strings.get(name)?;
        self.names.binary_search(&name).is_ok().then_some(name)
    }

    /// The rows `node` holds, in the order its descriptions state them.
    pub fn rows(&self, node: Id) -> &[RowId] {
        self.held.values(node)
    }

    pub fn visibilities(&self) -> &Visibilities {
        &self.visibilities
    }

    pub fn row(&self, row: RowId) -> &Row {
        &self.rows[row.0 as usize]
    }

    /// The number of distinct rows the table holds.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// What the descriptions of `node` say of its attribute `name`, `None`
    /// standing for a name no rule states.
    pub fn stated(&self, node: Id, name: Option<Sym>) -> Stated {
        let rows = self.rows(node);
        let named = rows.iter().find(|&&row| Some(self.row(row).name) == name);
        if let Some(&row) = named {
            return Stated::Row(row);
        }
        if self.incomplete.binary_search(&node).is_ok() {
            Stated::LeftOut
        } else if rows.is_empty() {
            Stated::Undescribed
        } else {
            Stated::Absent
        }
    }

    /// Moves every node's rows to the node id `remap` gives it, which must map
    /// each id of `0..remap.len()` to a distinct id of that same range.
    pub fn renumber(&mut self, remap: &[Id]) {
        self.held.renumber(remap);
        for node in &mut self.incomplete {
            *node = remap[node.0 as usize];
        }
        self.incomplete.sort_unstable();
        self.visibilities.renumber(remap);
    }

    /// Whether `matches` holds of one of the strings `value` prints as, which
    /// `attr` finds its pattern in: one string for each combination of
    /// branches of a `select`, a value Bazel computes no string for printing
    /// none.
    pub fn any_printed(&self, value: &Value, mut matches: impl FnMut(&str) -> bool) -> bool {
        let mut printed = String::new();
        let mut found = false;
        let mut test = |parts: &[&Value]| {
            printed.clear();
            if !found && self.print(parts, &mut printed) {
                found = matches(&printed);
            }
            !found
        };
        match value {
            Value::Select(selectors) => combinations(selectors, &mut test),
            value => {
                test(&[value]);
            }
        }
        found
    }

    /// Writes into `out` what Bazel prints for the concatenation of `parts`,
    /// returning whether it prints anything, which a value Bazel cannot
    /// concatenate or holds as null does not.
    fn print(&self, parts: &[&Value], out: &mut String) -> bool {
        let text = |sym: Sym| self.strings.resolve(sym);
        if let [part] = parts {
            return self.print_one(part, out);
        }
        let lists: Option<Vec<_>> = parts.iter().map(|part| list_items(part)).collect();
        if let Some(lists) = lists {
            let mut items: Vec<Item> = lists.into_iter().flatten().collect();
            // `SetType.concat` collects into an `ImmutableSet`, which keeps
            // each element where it was first added.
            if matches!(parts[0], Value::StringSet(_)) {
                let mut kept = 0;
                for at in 0..items.len() {
                    if !items[..kept].contains(&items[at]) {
                        items[kept] = items[at];
                        kept += 1;
                    }
                }
                items.truncate(kept);
            }
            write_list(items.into_iter(), &text, out);
            return true;
        }
        let strings: Option<Vec<Sym>> = (parts.iter())
            .map(|part| match part {
                Value::String(sym) => Some(*sym),
                _ => None,
            })
            .collect();
        if let Some(strings) = strings {
            strings.into_iter().for_each(|sym| out.push_str(text(sym)));
            return true;
        }
        let integers: Option<Vec<i32>> = (parts.iter())
            .map(|part| match part {
                Value::Integer(value) => Some(*value),
                _ => None,
            })
            .collect();
        if let Some(integers) = integers {
            let sum = integers.into_iter().fold(0i32, i32::wrapping_add);
            write!(out, "{sum}").expect("writing to a String");
            return true;
        }
        let dicts: Option<Vec<_>> = parts.iter().map(|part| dict_entries(part)).collect();
        if let Some(dicts) = dicts {
            // `Type.concat` of maps keeps each key where it was first put and
            // the value it was last put with.
            let mut merged: Vec<(Sym, Item)> = Vec::new();
            for (key, value) in dicts.into_iter().flatten() {
                match merged.iter_mut().find(|(held, _)| *held == key) {
                    Some(entry) => entry.1 = value,
                    None => merged.push((key, value)),
                }
            }
            write_dict(merged.into_iter(), &text, out);
            return true;
        }
        false
    }

    fn print_one(&self, value: &Value, out: &mut String) -> bool {
        let text = |sym: Sym| self.strings.resolve(sym);
        match value {
            Value::Null | Value::Select(_) => return false,
            Value::Integer(value) => write!(out, "{value}").expect("writing to a String"),
            // `TargetUtils.convertAttributeValue` prints a boolean and a
            // tristate as the integers they were once stored as.
            Value::Boolean(value) => out.push_str(if *value { "1" } else { "0" }),
            Value::Tristate(value) => out.push_str(match value {
                Tristate::Auto => "-1",
                Tristate::No => "0",
                Tristate::Yes => "1",
            }),
            Value::String(sym) | Value::Label(sym) => out.push_str(text(*sym)),
            Value::Strings(_) | Value::StringSet(_) | Value::Labels(_) | Value::Integers(_) => {
                write_list(list_items(value).into_iter().flatten(), &text, out)
            }
            Value::StringDict(_)
            | Value::LabelDict(_)
            | Value::LabelKeyedStringDict(_)
            | Value::StringListDict(_)
            | Value::LabelListDict(_) => {
                write_dict(dict_entries(value).into_iter().flatten(), &text, out)
            }
            // `License.toString`.
            Value::License(license) => {
                let License { types, exceptions } = license.as_ref();
                let start = out.len();
                write_list(types.iter().map(|&sym| Item::Sym(sym)), &text, out);
                out[start..].make_ascii_lowercase();
                if !exceptions.is_empty() {
                    out.push_str(" with exceptions ");
                    write_list(exceptions.iter().map(|&sym| Item::Sym(sym)), &text, out);
                }
            }
        }
        true
    }
}

impl Value {
    /// Calls `visit` with each label the value names as Bazel's types visit
    /// them (`Type.visitLabels`): those of every `select` branch, but no
    /// condition, and none of a license.
    pub fn for_each_label(&self, visit: &mut impl FnMut(Sym)) {
        match self {
            Value::Label(label) => visit(*label),
            Value::Labels(labels) => labels.iter().copied().for_each(visit),
            Value::LabelDict(entries) => entries.iter().for_each(|&(_, label)| visit(label)),
            Value::LabelKeyedStringDict(entries) => {
                entries.iter().for_each(|&(label, _)| visit(label))
            }
            Value::LabelListDict(entries) => (entries.iter())
                .flat_map(|(_, labels)| labels.iter().copied())
                .for_each(visit),
            Value::Select(selectors) => (selectors.iter())
                .flat_map(|selector| selector.0.iter())
                .for_each(|(_, value)| value.for_each_label(visit)),
            Value::Null
            | Value::Integer(_)
            | Value::Boolean(_)
            | Value::Tristate(_)
            | Value::String(_)
            | Value::Strings(_)
            | Value::StringSet(_)
            | Value::Integers(_)
            | Value::StringDict(_)
            | Value::StringListDict(_)
            | Value::License(_) => {}
        }
    }
}

/// The elements of a list value, `None` for a value that is no list.
fn list_items(value: &Value) -> Option<Box<dyn Iterator<Item = Item<'_>> + '_>> {
    match value {
        Value::Strings(items) | Value::StringSet(items) | Value::Labels(items) => {
            Some(Box::new(items.iter().map(|&sym| Item::Sym(sym))))
        }
        Value::Integers(items) => Some(Box::new(items.iter().map(|&value| Item::Integer(value)))),
        _ => None,
    }
}

/// One element of a list, or one value of a dict.
#[derive(Clone, Copy, PartialEq)]
enum Item<'a> {
    Sym(Sym),
    Integer(i32),
    List(&'a [Sym]),
}

/// Writes `item` as Java's `toString` prints it.
fn write_item<'s>(item: Item, text: &impl Fn(Sym) -> &'s str, out: &mut String) {
    match item {
        Item::Sym(sym) => out.push_str(text(sym)),
        Item::Integer(value) => write!(out, "{value}").expect("writing to a String"),
        Item::List(syms) => write_list(syms.iter().map(|&sym| Item::Sym(sym)), text, out),
    }
}

/// Writes `items` as `List.toString` prints them: `[a, b]`.
fn write_list<'a, 's>(
    items: impl Iterator<Item = Item<'a>>,
    text: &impl Fn(Sym) -> &'s str,
    out: &mut String,
) {
    out.push('[');
    for (at, item) in items.enumerate() {
        if at > 0 {
            out.push_str(", ");
        }
        write_item(item, text, out);
    }
    out.push(']');
}

/// Writes `entries` as `Map.toString` prints them: `{k=v, k2=v2}`.
fn write_dict<'a, 's>(
    entries: impl Iterator<Item = (Sym, Item<'a>)>,
    text: &impl Fn(Sym) -> &'s str,
    out: &mut String,
) {
    out.push('{');
    for (at, (key, value)) in entries.enumerate() {
        if at > 0 {
            out.push_str(", ");
        }
        out.push_str(text(key));
        out.push('=');
        write_item(value, text, out);
    }
    out.push('}');
}

/// The entries of a dict value, `None` for a value that is no dict.
fn dict_entries(value: &Value) -> Option<Box<dyn Iterator<Item = (Sym, Item<'_>)> + '_>> {
    match value {
        Value::StringDict(entries)
        | Value::LabelDict(entries)
        | Value::LabelKeyedStringDict(entries) => Some(Box::new(
            entries.iter().map(|&(key, value)| (key, Item::Sym(value))),
        )),
        Value::StringListDict(entries) | Value::LabelListDict(entries) => Some(Box::new(
            entries
                .iter()
                .map(|(key, values)| (*key, Item::List(values))),
        )),
        _ => None,
    }
}

/// Calls `visit` with the branch values of each combination of branches of
/// `selectors` that `AggregatingAttributeMapper.getAllValues` visits, in no
/// particular order, until it returns false. A combination it visits holds
/// the branch of each select in order; selects with the same conditions take
/// the branch of the same condition, as that walk binds them.
fn combinations<'v>(selectors: &'v [Selector], visit: &mut impl FnMut(&[&'v Value]) -> bool) {
    let Some(first) = selectors.first() else {
        return;
    };
    if selectors.len() == 1 {
        for (_, value) in first.0.iter() {
            if !visit(&[value]) {
                return;
            }
        }
        return;
    }
    let conditions: Vec<Vec<Sym>> = selectors
        .iter()
        .map(|selector| {
            let mut keys: Vec<Sym> = selector.0.iter().map(|&(key, _)| key).collect();
            keys.sort_unstable();
            keys
        })
        .collect();
    let branch = |at: usize, key: Sym| {
        let found = selectors[at].0.iter().find(|&&(held, _)| held == key);
        &found.expect("a select with the same conditions").1
    };
    // The walk is depth first and reuses the condition each set of
    // conditions was last bound to across paths, as Bazel's does.
    let mut bound: FxHashMap<&[Sym], (Sym, usize)> = FxHashMap::default();
    let mut open: Vec<(usize, Sym, Vec<&Value>)> = Vec::with_capacity(first.0.len());
    open.extend(first.0.iter().map(|(key, value)| (0, *key, vec![value])));
    while let Some((at, key, mut path)) = open.pop() {
        let next = at + 1;
        if next == selectors.len() {
            if !visit(&path) {
                return;
            }
            continue;
        }
        if let Some(&(earlier, offset)) = bound.get(conditions[next].as_slice())
            && offset < at
        {
            path.push(branch(next, earlier));
            open.push((next, earlier, path));
            continue;
        }
        bound.insert(&conditions[at], (key, at));
        if conditions[at] == conditions[next] {
            path.push(branch(next, key));
            open.push((next, key, path));
            continue;
        }
        for (other, value) in selectors[next].0.iter() {
            let mut taken = Vec::with_capacity(path.len() + 1);
            taken.extend_from_slice(&path);
            taken.push(value);
            open.push((next, *other, taken));
        }
    }
}

/// Collects an [`Attributes`] table as the inputs state rules.
#[derive(Default)]
pub struct AttributesBuilder {
    strings: FacetStrings,
    rows: FxHashMap<Row, RowId>,
    held: SparseFacetBuilder<RowId>,
    incomplete: Vec<Id>,
    visibilities: VisibilitiesBuilder,
}

impl AttributesBuilder {
    /// Makes room for `attributes` more rows held by nodes.
    pub fn reserve(&mut self, attributes: usize) {
        self.held.reserve(attributes);
    }

    /// Records the attributes `rule` states of its own as those of `node`,
    /// returning whether they read as every attribute of the rule.
    pub(crate) fn describe(&mut self, node: Id, rule: &Rule, repos: &RepoMapping) -> bool {
        let own = own_attributes(rule);
        let complete = states_every_attribute(own);
        if !complete {
            self.incomplete.push(node);
        }
        for attribute in own {
            let row = Row {
                name: self.strings.intern(&attribute.name),
                value: self.attribute_value(attribute, repos),
                explicitly_specified: attribute.explicitly_specified.unwrap_or(false),
            };
            let id = self.intern(row);
            self.held.push(node, id);
        }
        complete
    }

    fn intern(&mut self, row: Row) -> RowId {
        let next =
            RowId(u32::try_from(self.rows.len()).expect("more than u32::MAX attribute rows"));
        *self.rows.entry(row).or_insert(next)
    }

    fn attribute_value(&mut self, attribute: &Attribute, repos: &RepoMapping) -> Value {
        let nodep = attribute.nodep.unwrap_or(false);
        match attribute.selector_list.as_ref() {
            Some(list) if attribute.r#type == Discriminator::SelectorList as i32 => {
                let kind = list.r#type.unwrap_or(Discriminator::Unknown as i32);
                let selectors = (list.elements.iter())
                    .map(|selector| self.selector(selector, kind, nodep, repos))
                    .collect();
                Value::Select(selectors)
            }
            _ => self.value(
                attribute.r#type,
                nodep,
                &Fields::of_attribute(attribute),
                repos,
            ),
        }
    }

    fn selector(
        &mut self,
        selector: &SelectorPb,
        kind: i32,
        nodep: bool,
        repos: &RepoMapping,
    ) -> Selector {
        let branches = selector.entries.iter().map(|entry| {
            let condition = entry.label.as_deref().unwrap_or_default();
            let condition = self.label(condition, repos);
            let value = self.value(kind, nodep, &Fields::of_entry(entry), repos);
            (condition, value)
        });
        Selector(branches.collect())
    }

    /// The value `fields` state for an attribute of type `kind`; `nodep` for a
    /// string type marks strings holding labels.
    fn value(&mut self, kind: i32, nodep: bool, fields: &Fields, repos: &RepoMapping) -> Value {
        use Discriminator as D;
        let Ok(kind) = D::try_from(kind) else {
            return Value::Null;
        };
        let syms = |builder: &mut Self, items: &[String], labels: bool| -> Box<[Sym]> {
            (items.iter())
                .map(|item| builder.string_or_label(item, labels, repos))
                .collect()
        };
        match kind {
            D::Integer => fields.int.map_or(Value::Null, Value::Integer),
            D::Boolean => match (fields.boolean, fields.int) {
                (Some(value), _) => Value::Boolean(value),
                (None, Some(value)) => Value::Boolean(value != 0),
                (None, None) => Value::Null,
            },
            D::Tristate => {
                let stated = fields
                    .tristate
                    .and_then(|value| Tristate::try_from(value).ok());
                let encoded = fields.int.and_then(|value| match value {
                    -1 => Some(Tristate::Auto),
                    0 => Some(Tristate::No),
                    1 => Some(Tristate::Yes),
                    _ => None,
                });
                stated.or(encoded).map_or(Value::Null, Value::Tristate)
            }
            D::String if nodep => match fields.string {
                Some(label) => Value::Label(self.label(label, repos)),
                None => Value::Null,
            },
            D::String => match fields.string {
                Some(string) => Value::String(self.strings.intern(string)),
                None => Value::Null,
            },
            D::Label | D::Output => match fields.string {
                Some(label) => Value::Label(self.label(label, repos)),
                None => Value::Null,
            },
            D::LabelList | D::OutputList => match fields.string {
                // A dormant label is written as a label list's single string.
                Some(label) if fields.strings.is_empty() => Value::Label(self.label(label, repos)),
                _ => Value::Labels(syms(self, fields.strings, true)),
            },
            D::StringList if nodep => Value::Labels(syms(self, fields.strings, true)),
            D::StringList => Value::Strings(syms(self, fields.strings, false)),
            D::DistributionSet => Value::StringSet(syms(self, fields.strings, false)),
            D::IntegerList => Value::Integers(fields.ints.into()),
            D::StringDict => Value::StringDict(
                (fields.string_dict.iter())
                    .map(|entry| {
                        (
                            self.strings.intern(&entry.key),
                            self.strings.intern(&entry.value),
                        )
                    })
                    .collect(),
            ),
            D::LabelDictUnary => Value::LabelDict(
                (fields.label_dict.iter())
                    .map(|entry| {
                        (
                            self.strings.intern(&entry.key),
                            self.label(&entry.value, repos),
                        )
                    })
                    .collect(),
            ),
            D::LabelKeyedStringDict => Value::LabelKeyedStringDict(
                (fields.label_keyed.iter())
                    .map(|entry| {
                        (
                            self.label(&entry.key, repos),
                            self.strings.intern(&entry.value),
                        )
                    })
                    .collect(),
            ),
            D::StringListDict => Value::StringListDict(
                (fields.string_list_dict.iter())
                    .map(|entry| {
                        let key = self.strings.intern(&entry.key);
                        (key, syms(self, &entry.value, false))
                    })
                    .collect(),
            ),
            // Bazel writes a label-list dict into the string-list dict field.
            D::LabelListDict => {
                let written = (fields.string_list_dict.iter())
                    .map(|entry| (&entry.key, &entry.value))
                    .chain((fields.label_list_dict.iter()).map(|entry| (&entry.key, &entry.value)));
                let entries = written.map(|(key, values)| {
                    let key = self.strings.intern(key);
                    (key, syms(self, values, true))
                });
                Value::LabelListDict(entries.collect())
            }
            D::License => match fields.license {
                Some(license) => Value::License(Box::new(License {
                    types: syms(self, &license.license_type, false),
                    exceptions: syms(self, &license.exception, true),
                })),
                None => Value::Null,
            },
            D::FilesetEntryList | D::Unknown | D::SelectorList | D::DeprecatedStringDictUnary => {
                Value::Null
            }
        }
    }

    fn string_or_label(&mut self, text: &str, label: bool, repos: &RepoMapping) -> Sym {
        match label {
            true => self.label(text, repos),
            false => self.strings.intern(text),
        }
    }

    /// `label` interned in the canonical form `Label.toString` prints.
    fn label(&mut self, label: &str, repos: &RepoMapping) -> Sym {
        self.strings.intern(&repos.canonicalize_label(label))
    }

    pub fn seal(mut self) -> Attributes {
        let mut rows: Vec<(RowId, Row)> =
            self.rows.into_iter().map(|(row, id)| (id, row)).collect();
        rows.sort_unstable_by_key(|&(id, _)| id);
        self.strings.shrink_to_fit();
        let mut names: Vec<Sym> = rows.iter().map(|(_, row)| row.name).collect();
        names.sort_unstable();
        names.dedup();
        self.incomplete.sort_unstable();
        self.incomplete.dedup();
        Attributes {
            strings: self.strings,
            names: names.into(),
            rows: rows.into_iter().map(|(_, row)| row).collect(),
            held: self.held.seal(),
            incomplete: self.incomplete,
            visibilities: self.visibilities.seal(),
        }
    }
}

/// The attributes `rule` states of its own. A proto lists them sorted by
/// name, then the [`SYNTHETIC`] ones, then the attributes of the aspects
/// applying to the rule, sorted by name. Without a synthetic attribute to cut
/// at, the rule's own end where the names stop ascending, so the attributes of
/// an aspect sorting after every one of the rule's own read as the rule's.
fn own_attributes(rule: &Rule) -> &[Attribute] {
    let attributes = rule.attribute.as_slice();
    let synthetic = (attributes.iter()).position(|attribute| SYNTHETIC.contains(&&*attribute.name));
    let end = synthetic.unwrap_or_else(|| {
        let unsorted = attributes
            .windows(2)
            .position(|pair| pair[0].name >= pair[1].name);
        unsorted.map_or(attributes.len(), |at| at + 1)
    });
    &attributes[..end]
}

/// Whether `own` reads as every attribute of its rule: it holds `name`, which
/// a restricted `--proto:output_rule_attrs` rarely keeps, and some attribute
/// the BUILD file leaves to its default, which `--noproto:default_values`
/// drops.
fn states_every_attribute(own: &[Attribute]) -> bool {
    own.iter().any(|attribute| attribute.name == "name")
        && (own.iter()).any(|attribute| attribute.explicitly_specified != Some(true))
}

/// The value fields an [`Attribute`] and a [`SelectorEntry`] share.
struct Fields<'a> {
    int: Option<i32>,
    string: Option<&'a str>,
    boolean: Option<bool>,
    tristate: Option<i32>,
    strings: &'a [String],
    ints: &'a [i32],
    license: Option<&'a LicensePb>,
    string_dict: &'a [StringDictEntry],
    label_dict: &'a [LabelDictUnaryEntry],
    label_keyed: &'a [LabelKeyedStringDictEntry],
    string_list_dict: &'a [StringListDictEntry],
    label_list_dict: &'a [LabelListDictEntry],
}

macro_rules! fields_of {
    ($source:expr) => {{
        let source = $source;
        Fields {
            int: source.int_value,
            string: source.string_value.as_deref(),
            boolean: source.boolean_value,
            tristate: source.tristate_value,
            strings: &source.string_list_value,
            ints: &source.int_list_value,
            license: source.license.as_ref(),
            string_dict: &source.string_dict_value,
            label_dict: &source.label_dict_unary_value,
            label_keyed: &source.label_keyed_string_dict_value,
            string_list_dict: &source.string_list_dict_value,
            label_list_dict: &source.label_list_dict_value,
        }
    }};
}

impl<'a> Fields<'a> {
    fn of_attribute(attribute: &'a Attribute) -> Self {
        fields_of!(attribute)
    }

    fn of_entry(entry: &'a SelectorEntry) -> Self {
        fields_of!(entry)
    }
}

#[cfg(test)]
mod tests;
