//! The per-node metadata a graph filter selects on, held as interned handles.
//!
//! Facet values — a canonical repository name, a configuration key, a rule
//! class — are strings shared by many nodes, so they are interned once into a
//! [`FacetStrings`] table and carried per node as a four-byte handle. The table
//! is the filter's own; node ids keep theirs, whose indices double as positions
//! in the node arrays.

use std::collections::HashMap;
use std::num::NonZeroU32;

/// A string interned in a [`FacetStrings`] table. The index is stored biased
/// off one, so the handle is never zero and `Option<Sym>` is as wide as `Sym`
/// without spending an index on a sentinel.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Sym(NonZeroU32);

impl Sym {
    fn at(index: usize) -> Self {
        let sym = u32::try_from(index)
            .ok()
            .and_then(|index| NonZeroU32::MIN.checked_add(index));
        Sym(sym.expect("more than u32::MAX facet strings"))
    }

    fn index(self) -> usize {
        self.0.get() as usize - 1
    }
}

/// Declares a facet handle: a [`Sym`] tagged with the facet it names, so one
/// facet's value cannot be passed where another's is expected. A handle exists
/// only for a string the table has already interned, since a `Sym` is the sole
/// way to build one.
macro_rules! facet_handle {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
        pub struct $name(Sym);

        impl $name {
            pub fn of(value: Sym) -> Self {
                $name(value)
            }

            pub fn sym(self) -> Sym {
                self.0
            }
        }
    };
}

facet_handle! {
    /// The canonical name of the repository a node's label names.
    RepoRef
}

facet_handle! {
    /// The key of the configuration a node is built in.
    ConfigRef
}

facet_handle! {
    /// The Bazel rule class a target was declared by.
    RuleClassRef
}

/// A yes/no facet that an input may leave unsaid. A filter must keep
/// `Unknown` distinct from `No`: the node is not known to lack the property,
/// only not known to have it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Tri {
    #[default]
    Unknown,
    No,
    Yes,
}

/// The strings every facet handle resolves through.
#[derive(Default, Clone)]
pub struct FacetStrings {
    strings: Vec<String>,
    lookup: HashMap<String, Sym>,
}

impl FacetStrings {
    pub fn intern(&mut self, value: &str) -> Sym {
        if let Some(&sym) = self.lookup.get(value) {
            return sym;
        }
        let sym = Sym::at(self.strings.len());
        self.strings.push(value.to_string());
        self.lookup.insert(value.to_string(), sym);
        sym
    }

    pub fn get(&self, value: &str) -> Option<Sym> {
        self.lookup.get(value).copied()
    }

    pub fn resolve(&self, sym: Sym) -> &str {
        &self.strings[sym.index()]
    }

    pub fn len(&self) -> usize {
        self.strings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_the_same_string_twice_yields_one_handle() {
        let mut facets = FacetStrings::default();

        let first = facets.intern("rules_rust+");
        let second = facets.intern("rules_rust+");
        let other = facets.intern("rules_cc+");

        assert_eq!(first, second);
        assert_ne!(first, other);
        assert_eq!(facets.len(), 2);
    }

    #[test]
    fn a_handle_resolves_to_the_string_it_was_interned_from() {
        let mut facets = FacetStrings::default();

        let first = facets.intern("k8-fastbuild");
        let last = facets.intern("k8-opt-exec");

        assert_eq!(facets.resolve(first), "k8-fastbuild");
        assert_eq!(facets.resolve(last), "k8-opt-exec");
        assert_eq!(facets.get("k8-opt-exec"), Some(last));
        assert_eq!(facets.get("k8-dbg"), None);
    }

    #[test]
    fn an_empty_table_holds_no_strings() {
        let mut facets = FacetStrings::default();
        assert!(facets.is_empty());

        facets.intern("");

        assert!(!facets.is_empty());
        assert_eq!(facets.resolve(facets.get("").unwrap()), "");
    }

    /// The non-zero inner value is what lets an absent facet cost nothing: a
    /// missing facet is `None`, not a reserved index.
    #[test]
    fn an_optional_handle_is_no_wider_than_the_handle() {
        assert_eq!(size_of::<Option<Sym>>(), 4);
        assert_eq!(size_of::<Option<RepoRef>>(), 4);
        assert_eq!(size_of::<Option<ConfigRef>>(), 4);
        assert_eq!(size_of::<Option<RuleClassRef>>(), 4);
    }

    #[test]
    fn a_facet_handle_carries_the_string_it_tags() {
        let mut facets = FacetStrings::default();

        let repo = RepoRef::of(facets.intern("rules_java+"));

        assert_eq!(facets.resolve(repo.sym()), "rules_java+");
    }
}
