//! Layout-agnostic compound graph model.
//!
//! Nodes form a containment forest through `parent`: a node with no parent is a
//! root, and a node that is some other node's parent is a container. Edges run
//! between node ids and may cross containment levels; the layout engine places
//! each edge in the lowest common ancestor of its endpoints.

/// A coarse node category, used to pick a fill when rendering and to decide
/// which leaves may merge. `Group` is a Bazel `package_group`/`environment_group`
/// target and also the kind of the container an expanded merge-group renders as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Package,
    Target,
    ConfiguredTarget,
    Action,
    Artifact,
    Source,
    Group,
}

/// A box in the graph. A node is a container if any other node names it as
/// parent, and a leaf otherwise.
#[derive(Clone)]
pub struct Node {
    pub id: String,
    pub label: String,
    pub kind: Kind,
    /// A synthetic node a quotient pass folds interchangeable `Source`/`Artifact`
    /// leaves into. A collapsed merge-group keeps its members' kind (so it fills
    /// like them), so this — not the kind — marks the boxes that can expand or
    /// collapse a group.
    pub group: bool,
    pub parent: Option<String>,
}

/// Which Bazel query an edge came from, so the renderer can style the static
/// (query), configured (cquery), and execution-phase (aquery) dependencies
/// distinctly when several are drawn in one joint graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EdgeKind {
    Query,
    Cquery,
    Aquery,
}

impl EdgeKind {
    /// The provenance name shown in the renderer's legend.
    pub fn label(self) -> &'static str {
        match self {
            EdgeKind::Query => "query",
            EdgeKind::Cquery => "cquery",
            EdgeKind::Aquery => "aquery",
        }
    }
}

/// A directed dependency from one node to another, both by id, tagged with the
/// query it came from.
#[derive(Clone)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
}

#[derive(Clone, Default)]
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
        kind: Kind,
        parent: Option<String>,
    ) {
        self.nodes.push(Node {
            id: id.into(),
            label: label.into(),
            kind,
            group: false,
            parent,
        });
    }

    pub fn add_edge(&mut self, from: impl Into<String>, to: impl Into<String>, kind: EdgeKind) {
        self.edges.push(Edge {
            from: from.into(),
            to: to.into(),
            kind,
        });
    }
}
