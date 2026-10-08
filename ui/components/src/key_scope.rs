//! The key contexts bindings are scoped to, each under the heading the keys
//! help lists its bindings by.

use crate::keys_help::SCOPE_LABEL_MAX_CHARS;
use gpui::{App, Global, KeyContext};
use std::fmt;

/// A key context bindings are scoped to. Converts into the [`KeyContext`] an
/// element carries, and formats as the identifier a binding's predicate names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyScope {
    id: &'static str,
    label: &'static str,
    standing: Standing,
}

/// Where a scope stands among the others.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Standing {
    Alone,
    Within(Within),
    /// Around everything a window draws.
    Frame,
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
            label: fitting(label),
            standing: Standing::Alone,
        }
    }

    /// The scope of a window's frame, around everything the window draws. A
    /// binding is scoped to it only to rank below every binding of its keys
    /// scoped inside the window, so it reaches wherever focus stands in the
    /// window, and the keys help lists it with the bindings scoped to none.
    pub const fn frame(id: &'static str) -> Self {
        Self {
            id,
            label: UNSCOPED_LABEL,
            standing: Standing::Frame,
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
            label: fitting(label),
            standing: Standing::Within(Within {
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
        match self.standing {
            Standing::Within(within) => Some(within),
            Standing::Alone | Standing::Frame => None,
        }
    }

    pub(crate) fn is_frame(&self) -> bool {
        self.standing == Standing::Frame
    }
}

/// `label`, failing the build of a scope whose label runs past
/// [`SCOPE_LABEL_MAX_CHARS`].
const fn fitting(label: &'static str) -> &'static str {
    let bytes = label.as_bytes();
    let (mut at, mut chars) = (0, 0);
    while at < bytes.len() {
        if !is_utf8_continuation(bytes[at]) {
            chars += 1;
        }
        at += 1;
    }
    assert!(
        chars <= SCOPE_LABEL_MAX_CHARS,
        "a key scope's label runs past the keys help's heading"
    );
    label
}

const fn is_utf8_continuation(byte: u8) -> bool {
    byte & 0b1100_0000 == 0b1000_0000
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
