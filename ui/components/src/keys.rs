//! How the keymap's bindings read where the interface names them.

use gpui::{
    Action, App, FocusHandle, KeyBinding, KeyContext, KeybindingKeystroke, Keymap, Modifiers,
    SharedString, Window,
};

/// How a keystroke is spelled out for display.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyStyle {
    /// Modifier glyphs run straight into the key, in the order Apple's Human
    /// Interface Guidelines give them: `⌃⌥⇧⌘W`.
    Glyphs,
    /// Modifier names joined to the key by `+`: `Ctrl+Shift+W`.
    Words,
}

impl KeyStyle {
    /// The style the platform's own menus spell keystrokes in.
    pub const NATIVE: KeyStyle = if cfg!(target_os = "macos") {
        KeyStyle::Glyphs
    } else {
        KeyStyle::Words
    };
}

/// The name the platform key of [`Modifiers::platform`] goes by.
const PLATFORM_KEY: &str = if cfg!(target_os = "windows") {
    "Win"
} else {
    "Super"
};

/// `keystroke` as `style` spells it.
pub fn keystroke_text(keystroke: &KeybindingKeystroke, style: KeyStyle) -> String {
    let modifiers = keystroke.modifiers();
    let key = key_name(keystroke.key(), style);
    match style {
        KeyStyle::Glyphs => {
            let mut text: String = glyphs(modifiers).collect();
            text.push_str(&key);
            text
        }
        KeyStyle::Words => {
            let mut parts = words(modifiers).collect::<Vec<_>>();
            parts.push(&key);
            parts.join("+")
        }
    }
}

/// The keystrokes `binding` answers to, a chord's strokes set apart by a space.
pub fn binding_text(binding: &KeyBinding, style: KeyStyle) -> String {
    let strokes: Vec<String> = binding
        .keystrokes()
        .iter()
        .map(|keystroke| keystroke_text(keystroke, style))
        .collect();
    strokes.join(" ")
}

/// The keys that run `action` where `focus` stands, as the platform spells
/// them: the binding the keymap gives precedence among those no other binding
/// shadows in that context, or `None` where none reaches it.
pub fn key_text_in(
    action: &dyn Action,
    focus: &FocusHandle,
    window: &Window,
) -> Option<SharedString> {
    let binding = window.highest_precedence_binding_for_action_in(action, focus)?;
    Some(binding_text(&binding, KeyStyle::NATIVE).into())
}

/// The keys that run `action` from where focus stands now, as [`key_text_in`]
/// finds them.
pub fn key_text(action: &dyn Action, window: &Window) -> Option<SharedString> {
    let binding = window.highest_precedence_binding_for_action(action)?;
    Some(binding_text(&binding, KeyStyle::NATIVE).into())
}

/// The keys that run `action` within `stack`, a context stack from the
/// outermost context in, chosen as [`key_text_in`] chooses them for a focus
/// handle: for a control naming keys that depend on a context the frame being
/// drawn adds, which the last frame drawn does not carry.
pub fn key_text_within(
    action: &dyn Action,
    stack: &[KeyContext],
    cx: &App,
) -> Option<SharedString> {
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    let binding = binding_within(&keymap, action, stack)?;
    Some(binding_text(binding, KeyStyle::NATIVE).into())
}

/// The binding of `keymap` [`key_text_within`] names the keys of.
pub fn binding_within<'a>(
    keymap: &'a Keymap,
    action: &'a dyn Action,
    stack: &[KeyContext],
) -> Option<&'a KeyBinding> {
    keymap
        .bindings_for_action(action)
        .rev()
        .find(|binding| dispatched_first(keymap, binding, stack))
}

/// Whether `keymap` dispatches `binding` for its keystrokes within `stack`
/// ahead of every other binding of them.
pub(crate) fn dispatched_first(
    keymap: &Keymap,
    binding: &KeyBinding,
    stack: &[KeyContext],
) -> bool {
    let (matched, _) = keymap.bindings_for_input(binding.keystrokes(), stack);
    matched.first().is_some_and(|first| {
        first.action().partial_eq(binding.action()) && first.predicate() == binding.predicate()
    })
}

/// The help label of `action`: the first line of its documentation.
pub fn action_label(action: &dyn Action, cx: &App) -> Option<&'static str> {
    let documentation = cx.action_documentation().get(action.name())?;
    documentation.lines().next()
}

pub(crate) fn glyphs(modifiers: &Modifiers) -> impl Iterator<Item = &'static str> {
    [
        (modifiers.function, "fn"),
        (modifiers.control, "⌃"),
        (modifiers.alt, "⌥"),
        (modifiers.shift, "⇧"),
        (modifiers.platform, "⌘"),
    ]
    .into_iter()
    .filter_map(|(held, glyph)| held.then_some(glyph))
}

pub(crate) fn words(modifiers: &Modifiers) -> impl Iterator<Item = &'static str> {
    [
        (modifiers.function, "Fn"),
        (modifiers.control, "Ctrl"),
        (modifiers.alt, "Alt"),
        (modifiers.shift, "Shift"),
        (modifiers.platform, PLATFORM_KEY),
    ]
    .into_iter()
    .filter_map(|(held, word)| held.then_some(word))
}

/// How the key gpui names `key` reads in `style`. A key without a name of its
/// own reads as the character it types, a letter in capitals as it is printed
/// on the keycap.
fn key_name(key: &str, style: KeyStyle) -> String {
    let glyphs = style == KeyStyle::Glyphs;
    let named = match key {
        "left" => "←",
        "right" => "→",
        "up" => "↑",
        "down" => "↓",
        "enter" if glyphs => "↩",
        "enter" => "Enter",
        "backspace" if glyphs => "⌫",
        "backspace" => "Backspace",
        "delete" if glyphs => "⌦",
        "delete" => "Delete",
        "tab" if glyphs => "⇥",
        "tab" => "Tab",
        "escape" => "Esc",
        "space" => "Space",
        "home" if glyphs => "↖",
        "home" => "Home",
        "end" if glyphs => "↘",
        "end" => "End",
        "pageup" if glyphs => "⇞",
        "pageup" => "PgUp",
        "pagedown" if glyphs => "⇟",
        "pagedown" => "PgDn",
        _ => return key.to_uppercase(),
    };
    named.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Keystroke;

    fn stroke(modifiers: Modifiers, key: &str) -> KeybindingKeystroke {
        KeybindingKeystroke::from_keystroke(Keystroke {
            modifiers,
            key: key.into(),
            key_char: None,
        })
    }

    fn every_modifier() -> Modifiers {
        Modifiers {
            control: true,
            alt: true,
            shift: true,
            platform: true,
            function: false,
        }
    }

    #[test]
    fn glyphs_follow_apple_order_and_run_into_the_key() {
        let text = keystroke_text(&stroke(every_modifier(), "w"), KeyStyle::Glyphs);
        assert_eq!(text, "⌃⌥⇧⌘W");
    }

    #[test]
    fn words_join_modifiers_to_the_key() {
        let text = keystroke_text(&stroke(Modifiers::control_shift(), "w"), KeyStyle::Words);
        assert_eq!(text, "Ctrl+Shift+W");
        let text = keystroke_text(&stroke(every_modifier(), "w"), KeyStyle::Words);
        assert_eq!(text, format!("Ctrl+Alt+Shift+{PLATFORM_KEY}+W"));
    }

    #[test]
    fn a_bare_key_reads_alone() {
        for style in [KeyStyle::Glyphs, KeyStyle::Words] {
            assert_eq!(keystroke_text(&stroke(Modifiers::none(), "h"), style), "H");
            assert_eq!(keystroke_text(&stroke(Modifiers::none(), "["), style), "[");
            assert_eq!(
                keystroke_text(&stroke(Modifiers::none(), "f1"), style),
                "F1"
            );
        }
    }

    #[test]
    fn named_keys_read_in_the_style_asked_for() {
        let alt = Modifiers::alt();
        assert_eq!(
            keystroke_text(&stroke(alt, "left"), KeyStyle::Words),
            "Alt+←"
        );
        assert_eq!(keystroke_text(&stroke(alt, "left"), KeyStyle::Glyphs), "⌥←");
        let none = Modifiers::none();
        assert_eq!(
            keystroke_text(&stroke(none, "enter"), KeyStyle::Words),
            "Enter"
        );
        assert_eq!(
            keystroke_text(&stroke(none, "enter"), KeyStyle::Glyphs),
            "↩"
        );
        assert_eq!(
            keystroke_text(&stroke(none, "escape"), KeyStyle::Glyphs),
            "Esc"
        );
    }

    #[test]
    fn a_chord_sets_its_strokes_apart() {
        let binding = KeyBinding::new("ctrl-k shift-left", gpui::NoAction, None);
        assert_eq!(binding_text(&binding, KeyStyle::Words), "Ctrl+K Shift+←");
        assert_eq!(binding_text(&binding, KeyStyle::Glyphs), "⌃K ⇧←");
    }
}
