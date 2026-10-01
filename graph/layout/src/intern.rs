//! Interned node identity: a small integer `Id` plus the `Names` table that maps
//! it to and from the string id, so graphs key on integers and resolve to the
//! string only when one is genuinely needed (naming a new node, debugging).
use std::collections::HashMap;
use std::hash::Hasher;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Id(pub u32);

/// A hash of `text` that every run computes alike, for keying on a name where
/// an [`Id`], numbered by the order the inputs list things in, would not do.
pub(crate) fn stable_hash(text: &str) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    hasher.write(text.as_bytes());
    hasher.finish()
}

#[derive(Default, Clone)]
pub struct Names {
    strings: Vec<String>,
    lookup: HashMap<String, Id>,
}
impl Names {
    pub fn intern(&mut self, name: &str) -> Id {
        if let Some(&id) = self.lookup.get(name) {
            return id;
        }
        // A 4-byte id keeps `Node`/`Edge` and the level-of-detail arrays compact;
        // a graph approaching u32::MAX nodes is wildly past anything Bazel emits,
        // so cap it with a panic rather than let the cast silently wrap an id onto
        // an existing one.
        let id = Id(u32::try_from(self.strings.len()).expect("more than u32::MAX node ids"));
        self.strings.push(name.to_string());
        self.lookup.insert(name.to_string(), id);
        id
    }
    pub fn get(&self, name: &str) -> Option<Id> {
        self.lookup.get(name).copied()
    }
    pub fn resolve(&self, id: Id) -> &str {
        &self.strings[id.0 as usize]
    }
    pub fn len(&self) -> usize {
        self.strings.len()
    }
    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }
}
