use crate::derive::Shown;
use crate::neighborhood::{MarkingMemo, Neighborhood};
use crate::relation::Relation;
use bviz_graph_layout::Id;
use std::collections::HashSet;
use std::rc::Rc;

/// The node whose related nodes stay marked until another is picked, and which
/// of its relations the marking covers.
pub(crate) struct Selection {
    node: Option<Id>,
    relation: Relation,
    marked: MarkingMemo,
}

impl Selection {
    pub(crate) fn none() -> Self {
        Self {
            node: None,
            relation: Relation::default(),
            marked: MarkingMemo::default(),
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

    /// What the selection marks in `shown`. Empty while nothing is selected,
    /// and while the selected node is folded away into a container this view
    /// draws in its place.
    pub(crate) fn marking(&mut self, shown: Shown<'_>) -> Rc<Neighborhood> {
        self.marked.marking(shown, self.node, self.relation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::derive::Drawn;
    use crate::relation::Direction;
    use bviz_graph_layout::{Box, Connector, EdgeKind, Kind, LaidOut};
    use std::sync::Arc;

    /// Any zoom: a layered drawing shows the one view at every zoom.
    const ZOOM: f32 = 1.0;

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

    fn shown(drawn: &Drawn) -> Shown<'_> {
        drawn.shown(ZOOM).expect("a layered view")
    }

    /// Two leaves, the first an input of the second.
    fn pair() -> Drawn {
        let view = LaidOut {
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
        };
        Drawn::Layered {
            view: Arc::new(view),
            drawing: 0,
        }
    }

    #[test]
    fn nothing_selected_marks_nothing() {
        let mut selection = Selection::none();
        assert!(selection.marking(shown(&pair())).boxes.is_empty());
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
        assert_eq!(selection.marking(shown(&g)).boxes.len(), 1);
        selection.set_relation(Relation::Direct(Direction::Out));
        assert_eq!(selection.marking(shown(&g)).boxes.len(), 2);
        selection.set_relation(Relation::Direct(Direction::In));
        assert_eq!(selection.marking(shown(&g)).boxes.len(), 1);
    }
}
