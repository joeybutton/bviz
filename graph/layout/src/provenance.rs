//! Which of the Bazel queries a joint graph merges each node came from.

use std::num::NonZeroU8;

use crate::intern::Id;

/// One of the Bazel queries a joint graph is read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Input {
    Query,
    Cquery,
    Aquery,
}

impl Input {
    pub const ALL: [Input; 3] = [Input::Query, Input::Cquery, Input::Aquery];

    fn bit(self) -> NonZeroU8 {
        const BITS: [NonZeroU8; Input::ALL.len()] = [
            NonZeroU8::new(0b001).unwrap(),
            NonZeroU8::new(0b010).unwrap(),
            NonZeroU8::new(0b100).unwrap(),
        ];
        BITS[self as usize]
    }
}

/// A non-empty set of inputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InputSet(NonZeroU8);

impl InputSet {
    pub const ALL: InputSet = InputSet(NonZeroU8::new(0b111).unwrap());

    /// The set of `inputs`, absent where they are none.
    pub fn of(inputs: impl IntoIterator<Item = Input>) -> Option<Self> {
        let bits = inputs
            .into_iter()
            .fold(0, |bits, input| bits | input.bit().get());
        NonZeroU8::new(bits).map(Self)
    }

    pub fn contains(self, input: Input) -> bool {
        self.0.get() & input.bit().get() != 0
    }

    pub fn with(self, input: Input) -> Self {
        self.union(input.into())
    }

    /// The set less `input`, absent where that leaves none.
    pub fn without(self, input: Input) -> Option<Self> {
        NonZeroU8::new(self.0.get() & !input.bit().get()).map(Self)
    }

    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The inputs both sets hold, absent where they share none.
    pub fn intersection(self, other: Self) -> Option<Self> {
        NonZeroU8::new(self.0.get() & other.0.get()).map(Self)
    }

    pub fn intersects(self, other: Self) -> bool {
        self.intersection(other).is_some()
    }

    pub fn inputs(self) -> impl Iterator<Item = Input> {
        Input::ALL
            .into_iter()
            .filter(move |&input| self.contains(input))
    }
}

impl From<Input> for InputSet {
    fn from(input: Input) -> Self {
        Self(input.bit())
    }
}

/// The inputs that built each node of a joint graph: every input naming a node
/// shares it, so a node may have several.
#[derive(Clone, Debug, Default)]
pub struct Provenance(Vec<InputSet>);

impl Provenance {
    /// The provenance whose node of id `i` the inputs `by_id[i]` built.
    pub(crate) fn new(by_id: Vec<InputSet>) -> Self {
        Self(by_id)
    }

    /// The inputs that built the node `id`, absent for an id naming no node
    /// of the joint graph.
    pub fn of(&self, id: Id) -> Option<InputSet> {
        self.0.get(id.0 as usize).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_holds_what_it_was_made_of() {
        let set = InputSet::of([Input::Query, Input::Aquery]).unwrap();
        let held: Vec<Input> = set.inputs().collect();
        assert_eq!(held, [Input::Query, Input::Aquery]);
        assert!(!set.contains(Input::Cquery));
        assert_eq!(InputSet::of([]), None);
        assert_eq!(
            InputSet::ALL.inputs().collect::<Vec<_>>(),
            Input::ALL.to_vec()
        );
    }

    #[test]
    fn removing_the_last_input_leaves_no_set() {
        let one = InputSet::from(Input::Cquery);
        assert_eq!(one.without(Input::Cquery), None);
        assert_eq!(
            one.with(Input::Query).without(Input::Cquery),
            Some(Input::Query.into())
        );
    }

    #[test]
    fn disjoint_sets_share_nothing() {
        let query = InputSet::from(Input::Query);
        let rest = InputSet::of([Input::Cquery, Input::Aquery]).unwrap();
        assert_eq!(query.intersection(rest), None);
        assert!(!query.intersects(rest));
        assert_eq!(query.union(rest), InputSet::ALL);
        assert_eq!(InputSet::ALL.intersection(rest), Some(rest));
    }
}
