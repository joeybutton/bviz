//! The views of a graph document, as the controls each stands over it tell
//! them apart.

use bviz_ui_components::KeyScope;

/// A view of the graph document standing the controls every view shares: a
/// query bar and a jump palette.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Holder {
    Graph,
    Scatter,
    Exclusive,
}

impl Holder {
    pub(crate) const ALL: [Holder; 3] = [Holder::Graph, Holder::Scatter, Holder::Exclusive];

    /// The key scope of the view, from which the keys acting on a control it
    /// holds as a whole reach the control.
    pub(crate) const fn scope(self) -> KeyScope {
        match self {
            Holder::Graph => crate::KEY_CONTEXT,
            Holder::Scatter => crate::scatter::KEY_CONTEXT,
            Holder::Exclusive => crate::exclusive::KEY_CONTEXT,
        }
    }
}

/// The key scope of every [`Holder`].
pub(crate) const HOLDERS: [KeyScope; Holder::ALL.len()] = {
    let mut scopes = [crate::KEY_CONTEXT; Holder::ALL.len()];
    let mut at = 0;
    while at < scopes.len() {
        scopes[at] = Holder::ALL[at].scope();
        at += 1;
    }
    scopes
};
