//! Layout-agnostic compound graph model.
//!
//! Nodes form a containment forest through `parent`: a node with no parent is a
//! root, and a node that is some other node's parent is a container. Edges run
//! between node ids and may cross containment levels; the layout engine places
//! each edge in the lowest common ancestor of its endpoints.

/// A box in the graph. A node is a container if any other node names it as
/// parent, and a leaf otherwise.
pub struct Node {
    pub id: String,
    pub label: String,
    /// A coarse category (`target`, `configured-target`, `action`, `artifact`,
    /// `source`) used to pick a fill when rendering.
    pub kind: String,
    pub parent: Option<String>,
}

/// A directed dependency from one node to another, both by id.
pub struct Edge {
    pub from: String,
    pub to: String,
}

#[derive(Default)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_node(
        &mut self,
        id: impl Into<String>,
        label: impl Into<String>,
        kind: impl Into<String>,
        parent: Option<String>,
    ) {
        self.nodes.push(Node {
            id: id.into(),
            label: label.into(),
            kind: kind.into(),
            parent,
        });
    }

    pub fn add_edge(&mut self, from: impl Into<String>, to: impl Into<String>) {
        self.edges.push(Edge {
            from: from.into(),
            to: to.into(),
        });
    }
}
