//! What a set of key bindings dispatches, read without a window, for tests of
//! a crate's bindings. Each function panics on a keystroke or a key context
//! that does not parse.

use gpui::{Action, KeyBinding, KeyContext, Keymap, Keystroke};

/// The action `keys` dispatch under `bindings` with focus in the innermost of
/// `contexts`, which run outermost first, each parsed as a key context.
pub fn dispatched(
    bindings: Vec<KeyBinding>,
    keys: &str,
    contexts: &[&str],
) -> Option<Box<dyn Action>> {
    let keymap = Keymap::new(bindings);
    let stack: Vec<KeyContext> = contexts
        .iter()
        .map(|context| KeyContext::parse(context).expect("a key context"))
        .collect();
    let keystroke = Keystroke::parse(keys).expect("a keystroke");
    let (matched, _) = keymap.bindings_for_input(&[keystroke], &stack);
    matched
        .first()
        .map(|binding| binding.action().boxed_clone())
}

/// Whether `action` is `expected`.
pub fn is<A: Action>(action: Option<Box<dyn Action>>, expected: A) -> bool {
    action.is_some_and(|action| action.partial_eq(&expected))
}
