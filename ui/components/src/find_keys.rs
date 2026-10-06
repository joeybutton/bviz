//! The keys that open a panel's finder — its search field, filter or jump
//! palette — shared so that every panel with one answers the same keys.

use crate::KeyScope;
use crate::text_input;
use gpui::{Action, KeyBinding};

/// The bindings running `find` in a panel carrying `scope`. The slash types
/// itself while a text field in the panel holds the focus. A panel handles
/// `find` only while its field is drawn: focus given to a field no element
/// carries leaves the window answering no key.
pub fn find_key_bindings<A: Action + Clone>(scope: KeyScope, find: A) -> [KeyBinding; 2] {
    let unless_typing = text_input::unless_typing(scope);
    [
        KeyBinding::new("secondary-f", find.clone(), Some(scope.id())),
        KeyBinding::new("/", find, Some(unless_typing.as_str())),
    ]
}
