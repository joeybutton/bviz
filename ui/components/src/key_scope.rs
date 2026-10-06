//! The key contexts bindings are scoped to, each under the heading the keys
//! help lists its bindings by.

use gpui::{App, Global, KeyContext};
use std::fmt;

/// A key context bindings are scoped to. Converts into the [`KeyContext`] an
/// element carries, and formats as the identifier a binding's predicate names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyScope {
    id: &'static str,
    label: &'static str,
    within: Option<Within>,
}

/// How a scope narrows the scope it belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Nesting {
    /// Carried by an element drawn inside the enclosing scope's element.
    Inside,
    /// Carried alongside the enclosing scope's own identifier while that scope
    /// is in some state.
    State,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Within {
    pub(crate) id: &'static str,
    pub(crate) nesting: Nesting,
}

/// The heading the keys help lists bindings scoped to no key context under.
pub const UNSCOPED_LABEL: &str = "Global";

impl KeyScope {
    /// A scope belonging to no other: a window, a panel, or a control any of
    /// them may hold.
    pub const fn new(id: &'static str, label: &'static str) -> Self {
        Self {
            id,
            label,
            within: None,
        }
    }

    /// A scope narrowing `parent`, whose help lists this scope's bindings too
    /// whether or not this scope is in effect.
    pub const fn within(
        parent: &KeyScope,
        nesting: Nesting,
        id: &'static str,
        label: &'static str,
    ) -> Self {
        Self {
            id,
            label,
            within: Some(Within {
                id: parent.id,
                nesting,
            }),
        }
    }

    pub const fn id(&self) -> &'static str {
        self.id
    }

    /// The heading the keys help lists this scope's bindings under.
    pub const fn label(&self) -> &'static str {
        self.label
    }

    pub(crate) fn within_scope(&self) -> Option<Within> {
        self.within
    }
}

impl fmt::Display for KeyScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id)
    }
}

impl From<KeyScope> for KeyContext {
    fn from(scope: KeyScope) -> Self {
        let mut context = KeyContext::default();
        context.add(scope.id);
        context
    }
}

/// Every key scope the application's crates declared, in declaration order.
#[derive(Default)]
struct KeyScopes(Vec<KeyScope>);

impl Global for KeyScopes {}

/// Makes `scopes` known to the keys help, which lists a scope's bindings only
/// under a scope it knows.
pub fn declare_scopes(cx: &mut App, scopes: impl IntoIterator<Item = KeyScope>) {
    cx.default_global::<KeyScopes>().0.extend(scopes);
}

/// The scopes [`declare_scopes`] has made known.
pub fn declared_scopes(cx: &App) -> &[KeyScope] {
    cx.try_global::<KeyScopes>()
        .map_or(&[], |scopes| scopes.0.as_slice())
}
