//! The keys help: the bindings in effect for a panel, grouped under the key
//! scopes their predicates name.

use crate::key_scope::{KeyScope, Nesting, UNSCOPED_LABEL};
use crate::keys::{KeyStyle, binding_text};
use gpui::{
    Action, KeyBinding, KeyBindingContextPredicate, KeyContext, Keymap, SharedString, is_no_action,
    is_unbind,
};

/// The bindings in effect under one scope, each action listed once.
#[derive(Clone, PartialEq, Debug)]
pub struct KeyGroup {
    pub label: &'static str,
    pub rows: Vec<KeyRow>,
}

/// An action and every keystroke that runs it within its group.
#[derive(Clone, PartialEq, Debug)]
pub struct KeyRow {
    pub label: SharedString,
    pub keys: Vec<SharedString>,
}

/// A scope bindings can be filed under, and the context stack in which it is
/// in effect.
struct Slot {
    label: &'static str,
    /// The identifier a predicate names the scope by; `None` for the bindings
    /// scoped to no key context.
    id: Option<&'static str>,
    /// How deep the scope stands in `stack`, by which a binding naming several
    /// scopes is filed under the innermost.
    depth: usize,
    stack: Vec<KeyContext>,
}

/// The bindings of `keymap` in effect for the panel scoped to `panel`, grouped
/// by the scope each binding's predicate names: the panel's own, then each
/// scope declared within it, then each enclosing scope `stack` holds from the
/// innermost out, then the bindings scoped to none. `stack` runs from the
/// window's root to the panel's own context. A binding is in effect where the
/// keymap dispatches it ahead of every other binding of its keystrokes, a
/// scope within the panel counting as entered, so that the keys of a field or
/// a state are listed while it is not in effect. `label` names an action's
/// row.
pub fn keys_in_effect(
    keymap: &Keymap,
    scopes: &[KeyScope],
    stack: &[KeyContext],
    panel: KeyScope,
    label: impl Fn(&dyn Action) -> SharedString,
    style: KeyStyle,
) -> Vec<KeyGroup> {
    let slots = slots(scopes, stack, panel);
    // Each group's rows, beside the name of the action each row runs.
    let mut filed: Vec<Vec<(&'static str, KeyRow)>> = slots.iter().map(|_| Vec::new()).collect();
    for binding in keymap.bindings() {
        let action = binding.action();
        if is_no_action(action) || is_unbind(action) {
            continue;
        }
        let Some(at) = slot_of(binding, &slots) else {
            continue;
        };
        if !dispatched_first(keymap, binding, &slots[at].stack) {
            continue;
        }
        let keys: SharedString = binding_text(binding, style).into();
        let rows = &mut filed[at];
        match rows.iter_mut().find(|(name, _)| *name == action.name()) {
            Some((_, row)) => {
                if !row.keys.contains(&keys) {
                    row.keys.push(keys);
                }
            }
            None => rows.push((
                action.name(),
                KeyRow {
                    label: label(action),
                    keys: vec![keys],
                },
            )),
        }
    }
    slots
        .iter()
        .zip(filed)
        .filter(|(_, rows)| !rows.is_empty())
        .map(|(slot, rows)| KeyGroup {
            label: slot.label,
            rows: rows.into_iter().map(|(_, row)| row).collect(),
        })
        .collect()
}

/// The scopes bindings are filed under, in the order the help lists them.
fn slots(scopes: &[KeyScope], stack: &[KeyContext], panel: KeyScope) -> Vec<Slot> {
    let panel_depth = stack.len().saturating_sub(1);
    let mut slots = vec![Slot {
        label: panel.label(),
        id: Some(panel.id()),
        depth: panel_depth,
        stack: stack.to_vec(),
    }];
    for scope in scopes {
        if let Some(chain) = chain_within(scope, panel, scopes) {
            let mut entered = stack.to_vec();
            for link in chain.iter().rev() {
                let nesting = link.within_scope().map(|within| within.nesting);
                match (nesting, entered.last_mut()) {
                    (Some(Nesting::State), Some(last)) => last.add(link.id()),
                    _ => entered.push(KeyContext::from(**link)),
                }
            }
            slots.push(Slot {
                label: scope.label(),
                id: Some(scope.id()),
                depth: panel_depth + chain.len(),
                stack: entered,
            });
        }
    }
    for depth in (0..panel_depth).rev() {
        for scope in scopes {
            if stack[depth].contains(scope.id()) {
                slots.push(Slot {
                    label: scope.label(),
                    id: Some(scope.id()),
                    depth,
                    stack: stack.to_vec(),
                });
            }
        }
    }
    slots.push(Slot {
        label: UNSCOPED_LABEL,
        id: None,
        depth: 0,
        stack: stack.to_vec(),
    });
    slots
}

/// The scopes leading from `scope` out to `panel`, `scope` first and `panel`
/// left out, where `scope` lies within `panel`.
fn chain_within<'a>(
    scope: &'a KeyScope,
    panel: KeyScope,
    scopes: &'a [KeyScope],
) -> Option<Vec<&'a KeyScope>> {
    let mut chain = vec![scope];
    let mut at = scope;
    while let Some(within) = at.within_scope() {
        if within.id == panel.id() {
            return Some(chain);
        }
        at = scopes.iter().find(|scope| scope.id() == within.id)?;
        chain.push(at);
    }
    None
}

/// The slot `binding` is filed under: the innermost scope its predicate names,
/// or the bindings scoped to none for a predicate naming no scope. `None` for a
/// binding naming only scopes that are neither the panel's nor around it.
fn slot_of(binding: &KeyBinding, slots: &[Slot]) -> Option<usize> {
    let Some(predicate) = binding.predicate() else {
        return slots.iter().position(|slot| slot.id.is_none());
    };
    let mut named = Vec::new();
    identifiers(&predicate, &mut named);
    if named.is_empty() {
        return slots.iter().position(|slot| slot.id.is_none());
    }
    slots
        .iter()
        .enumerate()
        .filter(|(_, slot)| {
            slot.id
                .is_some_and(|id| named.iter().any(|name| name == id))
        })
        .max_by_key(|(_, slot)| slot.depth)
        .map(|(at, _)| at)
}

/// The identifiers `predicate` requires a context to carry; one it requires
/// absent is no scope the binding belongs to.
fn identifiers(predicate: &KeyBindingContextPredicate, into: &mut Vec<SharedString>) {
    match predicate {
        KeyBindingContextPredicate::Identifier(id) => into.push(id.clone()),
        KeyBindingContextPredicate::Equal(..)
        | KeyBindingContextPredicate::NotEqual(..)
        | KeyBindingContextPredicate::Not(_) => {}
        KeyBindingContextPredicate::Descendant(left, right)
        | KeyBindingContextPredicate::And(left, right)
        | KeyBindingContextPredicate::Or(left, right) => {
            identifiers(left, into);
            identifiers(right, into);
        }
    }
}

/// Whether `keymap` dispatches `binding` for its keystrokes within `stack`
/// ahead of every other binding of them.
fn dispatched_first(keymap: &Keymap, binding: &KeyBinding, stack: &[KeyContext]) -> bool {
    let (matched, _) = keymap.bindings_for_input(binding.keystrokes(), stack);
    matched.first().is_some_and(|first| {
        first.action().partial_eq(binding.action()) && first.predicate() == binding.predicate()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    gpui::actions!(
        keys_help_test,
        [
            StepBack,
            SelectPrevious,
            ToggleMaximize,
            Highlight,
            Reduce,
            Dismiss,
            DismissResults,
            Aim,
            ScrollPageUp,
            Copy,
            Quit,
        ]
    );

    const WORKSPACE: KeyScope = KeyScope::new("Workspace", "Panes & windows");
    const GRAPH: KeyScope = KeyScope::new("Graph", "Graph");
    const SEARCH: KeyScope = KeyScope::within(&GRAPH, Nesting::Inside, "GraphSearch", "Search");
    const PICKING: KeyScope = KeyScope::within(&GRAPH, Nesting::State, "picking", "Pick list");
    const CONSOLE: KeyScope = KeyScope::new("Console", "Console");
    const TEXT: KeyScope = KeyScope::new("TextInput", "Text field");

    /// The help for the graph panel over `bindings`, each row read as its
    /// action's name followed by its keys.
    fn help(bindings: Vec<KeyBinding>) -> Vec<(&'static str, Vec<String>)> {
        let keymap = Keymap::new(bindings);
        let stack = [WORKSPACE.into(), GRAPH.into()];
        let scopes = [WORKSPACE, GRAPH, SEARCH, PICKING, CONSOLE, TEXT];
        let groups = keys_in_effect(
            &keymap,
            &scopes,
            &stack,
            GRAPH,
            |action| action.name().rsplit("::").next().unwrap_or_default().into(),
            KeyStyle::Words,
        );
        groups
            .into_iter()
            .map(|group| {
                let rows = group
                    .rows
                    .into_iter()
                    .map(|row| format!("{}: {}", row.label, row.keys.join(", ")))
                    .collect();
                (group.label, rows)
            })
            .collect()
    }

    /// The modifier `secondary` stands for here, as [`KeyStyle::Words`] names it.
    const SECONDARY: &str = if cfg!(target_os = "macos") {
        "Super"
    } else {
        "Ctrl"
    };

    #[test]
    fn bindings_are_grouped_by_the_innermost_scope_they_name() {
        let outside_search = Some("Graph && !GraphSearch");
        let read = help(vec![
            KeyBinding::new("left", StepBack, Some("Workspace && !TextInput")),
            KeyBinding::new("secondary-k m", ToggleMaximize, Some("Workspace")),
            KeyBinding::new("left", SelectPrevious, outside_search),
            KeyBinding::new("h", Highlight, outside_search),
            KeyBinding::new("escape", Dismiss, outside_search),
            KeyBinding::new("escape", DismissResults, Some("GraphSearch")),
            KeyBinding::new("up", Aim, Some("Graph && picking")),
            KeyBinding::new("pageup", ScrollPageUp, Some("Console")),
            KeyBinding::new("ctrl-c", Copy, Some("Graph")),
            KeyBinding::new("alt-c", Copy, Some("Graph")),
            KeyBinding::new("secondary-q", Quit, None),
        ]);
        assert_eq!(
            read,
            [
                (
                    "Graph",
                    vec![
                        "SelectPrevious: ←".to_string(),
                        "Highlight: H".to_string(),
                        "Dismiss: Esc".to_string(),
                        "Copy: Ctrl+C, Alt+C".to_string(),
                    ]
                ),
                ("Search", vec!["DismissResults: Esc".to_string()]),
                ("Pick list", vec!["Aim: ↑".to_string()]),
                (
                    "Panes & windows",
                    vec![format!("ToggleMaximize: {SECONDARY}+K M")]
                ),
                (UNSCOPED_LABEL, vec![format!("Quit: {SECONDARY}+Q")]),
            ]
        );
    }

    /// A binding another one shadows is not what its keys do, so it is left
    /// out; it is listed where nothing shadows it.
    #[test]
    fn a_shadowed_binding_is_left_out() {
        let playback = KeyBinding::new("left", StepBack, Some("Workspace"));
        let read = help(vec![
            playback.clone(),
            KeyBinding::new("left", SelectPrevious, Some("Graph")),
        ]);
        assert_eq!(read, [("Graph", vec!["SelectPrevious: ←".to_string()])]);
        let read = help(vec![playback]);
        assert_eq!(read, [("Panes & windows", vec!["StepBack: ←".to_string()])]);
    }

    /// A binding taken away by a later [`gpui::NoAction`] runs nothing, and the
    /// binding taking it away is no row of its own.
    #[test]
    fn a_disabled_binding_is_left_out() {
        let read = help(vec![
            KeyBinding::new("r", Reduce, Some("Graph")),
            KeyBinding::new("r", gpui::NoAction, Some("Graph")),
        ]);
        assert_eq!(read, []);
    }

    /// A field's keys stand down for the panel's own while the field is not
    /// entered, and the help lists them all the same.
    #[test]
    fn a_scope_within_the_panel_is_listed_as_entered() {
        let read = help(vec![
            KeyBinding::new("escape", Dismiss, Some("Graph && !GraphSearch")),
            KeyBinding::new("escape", DismissResults, Some("GraphSearch")),
        ]);
        assert_eq!(
            read,
            [
                ("Graph", vec!["Dismiss: Esc".to_string()]),
                ("Search", vec!["DismissResults: Esc".to_string()]),
            ]
        );
    }
}
