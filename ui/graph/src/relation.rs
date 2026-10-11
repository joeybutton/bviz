/// Which way along the dependency edges a relation runs. An edge runs from an
/// input to whatever consumes it, so a node's inputs lie against the edges and
/// its outputs along them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Direction {
    Both,
    In,
    Out,
}

impl Direction {
    pub(crate) fn inward(self) -> bool {
        matches!(self, Self::Both | Self::In)
    }

    pub(crate) fn outward(self) -> bool {
        matches!(self, Self::Both | Self::Out)
    }
}

/// Which of a selected node's related nodes stay marked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Relation {
    /// The selected node alone.
    None,
    /// The nodes one edge away.
    Direct(Direction),
    /// Every node a path of edges reaches.
    Transitive(Direction),
}

impl Relation {
    /// Every relation, in the order the selector offers them.
    pub(crate) const ALL: [Self; 7] = [
        Self::None,
        Self::Direct(Direction::Both),
        Self::Direct(Direction::In),
        Self::Direct(Direction::Out),
        Self::Transitive(Direction::Both),
        Self::Transitive(Direction::In),
        Self::Transitive(Direction::Out),
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::None => "No highlight",
            Self::Direct(Direction::Both) => "Direct neighbours",
            Self::Direct(Direction::In) => "Direct inputs",
            Self::Direct(Direction::Out) => "Direct outputs",
            Self::Transitive(Direction::Both) => "All neighbours",
            Self::Transitive(Direction::In) => "All inputs",
            Self::Transitive(Direction::Out) => "All outputs",
        }
    }

    /// This relation's place in [`ALL`](Self::ALL), which the selector reports
    /// its choice by.
    pub(crate) fn index(self) -> usize {
        match self {
            Self::None => 0,
            Self::Direct(Direction::Both) => 1,
            Self::Direct(Direction::In) => 2,
            Self::Direct(Direction::Out) => 3,
            Self::Transitive(Direction::Both) => 4,
            Self::Transitive(Direction::In) => 5,
            Self::Transitive(Direction::Out) => 6,
        }
    }

    pub(crate) fn at(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }
}

impl Default for Relation {
    /// What a fresh selection marks: the same nodes a hover picks out, so
    /// selecting a node pins what hovering it already shows.
    fn default() -> Self {
        Self::Direct(Direction::Both)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_relation_sits_at_the_index_it_reports() {
        for relation in Relation::ALL {
            assert_eq!(Relation::at(relation.index()), Some(relation));
        }
        assert_eq!(Relation::at(Relation::ALL.len()), None);
    }

    #[test]
    fn every_relation_is_offered_under_a_name_of_its_own() {
        let mut labels: Vec<&str> = Relation::ALL.map(Relation::label).into();
        labels.sort_unstable();
        let offered = labels.len();
        labels.dedup();
        assert_eq!(labels.len(), offered, "two relations share a label");
    }
}
