//! Interned node identity: a small integer `Id` plus the `Names` table that maps
//! it to and from the string id, so graphs key on integers and resolve to the
//! string only when one is genuinely needed (naming a new node, debugging).
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Id(pub u32);

#[derive(Default, Clone)]
pub struct Names {
    strings: Vec<String>,
    lookup: HashMap<String, Id>,
}
impl Names {
    pub fn intern(&mut self, name: &str) -> Id {
        if let Some(&id) = self.lookup.get(name) { return id; }
        let id = Id(self.strings.len() as u32);
        self.strings.push(name.to_string());
        self.lookup.insert(name.to_string(), id);
        id
    }
    pub fn get(&self, name: &str) -> Option<Id> { self.lookup.get(name).copied() }
    pub fn resolve(&self, id: Id) -> &str { &self.strings[id.0 as usize] }
    pub fn len(&self) -> usize { self.strings.len() }
    pub fn is_empty(&self) -> bool { self.strings.is_empty() }
}
