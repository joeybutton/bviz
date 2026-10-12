//! Which of the graph a reader has chosen to see, apart from how a view lays
//! out what that leaves.

use bviz_graph_layout::Id;
use bviz_graph_layout::select::{Filter, Pin, Rule, Scope};
use std::collections::HashMap;

/// The scope a query applied and the nodes a reader pinned: what decides which
/// nodes are any part of what is shown, whichever view shows them.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct DocumentFilter {
    pub(crate) scope: Option<Scope>,
    pub(crate) pins: HashMap<Id, Pin>,
}

impl DocumentFilter {
    /// The filter a view draws through, with `rules` of its own.
    pub(crate) fn with_rules(&self, rules: &[Rule]) -> Filter {
        Filter {
            scope: self.scope.clone(),
            rules: rules.to_vec(),
            pins: self.pins.clone(),
        }
    }
}
