use crate::derive::ViewKey;
use crate::neighborhood::{Neighborhood, compute_neighborhood};
use crate::relation::Relation;
use bviz_graph_layout::{Id, LaidOut};
use std::collections::HashSet;
use std::rc::Rc;

/// The node whose related nodes stay marked until another is picked, and which
/// of its relations the marking covers.
pub(crate) struct Selection {
    node: Option<Id>,
    relation: Relation,
    /// The marking last computed, kept so redrawing one view under an unchanged
    /// selection walks the graph once rather than once a frame.
    marked: Option<Marked>,
}

/// A marking together with everything it was derived from, so it is recomputed
/// exactly when one of those moves.
struct Marked {
    view: ViewKey,
    node: Id,
    relation: Relation,
    neighborhood: Rc<Neighborhood>,
}

impl Marked {
    fn stands_for(&self, view: ViewKey, node: Id, relation: Relation) -> bool {
        self.view == view && self.node == node && self.relation == relation
    }
}

impl Selection {
    pub(crate) fn none() -> Self {
        Self {
            node: None,
            relation: Relation::default(),
            marked: None,
        }
    }

    pub(crate) fn node(&self) -> Option<Id> {
        self.node
    }

    pub(crate) fn relation(&self) -> Relation {
        self.relation
    }

    /// Selects `node`, or drops the selection where it is already the selected
    /// one, so picking the same node twice puts the marking away.
    pub(crate) fn toggle(&mut self, node: Id) {
        self.node = (self.node != Some(node)).then_some(node);
    }

    /// Selects `node`, whichever node was selected before.
    pub(crate) fn select(&mut self, node: Id) {
        self.node = Some(node);
    }

    pub(crate) fn clear(&mut self) {
        self.node = None;
    }

    /// Drops the selection where `gone` holds the node it stands on, which a
    /// drawing no longer holding that node leaves nothing to mark from.
    pub(crate) fn forget(&mut self, gone: &HashSet<Id>) {
        if self.node.is_some_and(|node| gone.contains(&node)) {
            self.clear();
        }
    }

    pub(crate) fn set_relation(&mut self, relation: Relation) {
        self.relation = relation;
    }

    /// What the selection marks in `g`, the view `view` identifies. Empty while
    /// nothing is selected, and while the selected node is folded away into a
    /// container this view draws in its place.
    pub(crate) fn marking(&mut self, g: &LaidOut, view: ViewKey) -> Rc<Neighborhood> {
        let Some(node) = self.node else {
            self.marked = None;
            return Rc::new(Neighborhood::none());
        };
        let relation = self.relation;
        let marked = match self.marked.take() {
            Some(marked) if marked.stands_for(view, node, relation) => marked,
            _ => Marked {
                view,
                node,
                relation,
                neighborhood: Rc::new(compute_neighborhood(g, Some(node), relation)),
            },
        };
        let neighborhood = Rc::clone(&marked.neighborhood);
        self.marked = Some(marked);
        neighborhood
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relation::Direction;
    use bviz_graph_layout::{Box, Connector, EdgeKind, Kind};

    fn leaf(id: u32) -> Box {
        Box {
            id: Id(id),
            label: String::new(),
            kind: Kind::Source,
            x: f64::from(id) * 200.0,
            y: 0.0,
            width: 100.0,
            height: 40.0,
            compound: false,
            group: false,
        }
    }

    /// Two leaves, the first an input of the second.
    fn pair() -> LaidOut {
        LaidOut {
            boxes: vec![leaf(0), leaf(1)],
            connectors: vec![Connector {
                points: Vec::new(),
                kinds: vec![EdgeKind::Query],
                from: Id(0),
                to: Id(1),
                transition: None,
            }],
            full_edges: vec![(Id(0), Id(1))],
            ..Default::default()
        }
    }

    const VIEW: ViewKey = ViewKey::Map {
        drawing: 0,
        level: 0,
    };
    const NEXT_LEVEL: ViewKey = ViewKey::Map {
        drawing: 0,
        level: 1,
    };

    #[test]
    fn nothing_selected_marks_nothing() {
        let mut selection = Selection::none();
        assert!(selection.marking(&pair(), VIEW).boxes.is_empty());
    }

    #[test]
    fn picking_the_selected_node_again_drops_the_selection() {
        let mut selection = Selection::none();
        selection.toggle(Id(0));
        assert_eq!(selection.node(), Some(Id(0)));
        selection.toggle(Id(1));
        assert_eq!(selection.node(), Some(Id(1)));
        selection.toggle(Id(1));
        assert_eq!(selection.node(), None);
    }

    #[test]
    fn selecting_the_selected_node_keeps_it_selected() {
        let mut selection = Selection::none();
        selection.select(Id(0));
        selection.select(Id(0));
        assert_eq!(selection.node(), Some(Id(0)));
        selection.select(Id(1));
        assert_eq!(selection.node(), Some(Id(1)));
    }

    #[test]
    fn a_marking_is_reused_until_what_it_was_derived_from_moves() {
        let g = pair();
        let mut selection = Selection::none();
        selection.toggle(Id(0));
        let first = selection.marking(&g, VIEW);
        assert!(Rc::ptr_eq(&first, &selection.marking(&g, VIEW)));
        // Each of the three things a marking is derived from retires it.
        assert!(!Rc::ptr_eq(&first, &selection.marking(&g, NEXT_LEVEL)));
        selection.set_relation(Relation::Transitive(Direction::Out));
        let widened = selection.marking(&g, VIEW);
        assert!(!Rc::ptr_eq(&first, &widened));
        selection.toggle(Id(1));
        assert!(!Rc::ptr_eq(&widened, &selection.marking(&g, VIEW)));
    }

    /// A node the drawing no longer holds has nothing to mark, so a filter that
    /// takes it out takes the selection with it and leaves one on any other node
    /// standing.
    #[test]
    fn a_selection_on_a_node_a_filter_drops_goes_with_it() {
        let mut selection = Selection::none();
        selection.toggle(Id(0));
        selection.forget(&[Id(1)].into_iter().collect());
        assert_eq!(selection.node(), Some(Id(0)));
        selection.forget(&[Id(0)].into_iter().collect());
        assert_eq!(selection.node(), None);
    }

    /// The relation is the reader's standing answer to what a selection should
    /// mark, so it outlives the box it was set on and the next one is marked to
    /// the same reach without setting it again.
    #[test]
    fn the_relation_outlives_the_selection_it_was_set_on() {
        let mut selection = Selection::none();
        selection.toggle(Id(0));
        selection.set_relation(Relation::Transitive(Direction::In));
        selection.clear();
        assert_eq!(selection.relation(), Relation::Transitive(Direction::In));
        selection.toggle(Id(1));
        assert_eq!(selection.relation(), Relation::Transitive(Direction::In));
    }

    #[test]
    fn a_marking_covers_the_relation_it_was_asked_for() {
        let g = pair();
        let mut selection = Selection::none();
        selection.toggle(Id(0));
        selection.set_relation(Relation::None);
        assert_eq!(selection.marking(&g, VIEW).boxes.len(), 1);
        selection.set_relation(Relation::Direct(Direction::Out));
        assert_eq!(selection.marking(&g, VIEW).boxes.len(), 2);
        selection.set_relation(Relation::Direct(Direction::In));
        assert_eq!(selection.marking(&g, VIEW).boxes.len(), 1);
    }
}
