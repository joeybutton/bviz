//! The keys help: the bindings in effect for a panel, grouped under the key
//! scopes their predicates name, and the pointer gestures beside them, put up
//! over the panel as a sheet a filter narrows.

use crate::gesture::GestureRow;
use crate::key_scope::{KeyScope, Nesting, UNSCOPED_LABEL};
use crate::keys::{KeyStyle, binding_text};
use crate::list_scroll::{ListScroll, list_scrollbar};
use crate::menu_chrome::{self, ROW_INSET};
use crate::scrollbar::{SCROLLBAR_THICKNESS, ScrollbarColors};
use crate::{QueryField, QueryFieldEvent};
use bviz_ui_theme::ThemeColors;
use gpui::{
    Action, App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, KeyBinding,
    KeyBindingContextPredicate, KeyContext, Keymap, MouseButton, MouseDownEvent, Pixels, Render,
    SharedString, Subscription, Window, actions, div, is_no_action, is_unbind, prelude::*, px,
};

actions!(
    bviz_keys_help,
    [
        /// Close help
        Close,
    ]
);

/// The name the keys help goes by wherever it is offered.
pub const KEYS_HELP_NAME: &str = "Keys & Gestures";

/// The key context the keys help carries, which [`Close`] is scoped to.
pub const KEY_CONTEXT: KeyScope = KeyScope::new("KeysHelp", KEYS_HELP_NAME);

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", Close, Some(KEY_CONTEXT.id()))]);
}

/// How wide the sheet is drawn, room for a label and the keys beside it.
const SHEET_WIDTH: Pixels = px(520.);

/// The most characters an action's help label or a gesture's description may
/// run to: beside a gesture of [`GESTURE_MAX_CHARS`], it still fits one line
/// of [`SHEET_WIDTH`].
pub const LABEL_MAX_CHARS: usize = 36;

/// The most characters a gesture may run to, as [`Gesture::text`] spells it.
///
/// [`Gesture::text`]: crate::gesture::Gesture::text
pub const GESTURE_MAX_CHARS: usize = 30;

/// The most characters a key scope's label may run to, which a heading of
/// the sheet holds on one line behind [`POINTER_HEADING`].
pub(crate) const SCOPE_LABEL_MAX_CHARS: usize = 24;

/// Room the filter field leaves beside it for the button emptying it.
const CLEAR_ROOM: Pixels = px(36.);

/// Room between a label and the keys beside it.
const COLUMN_GAP: Pixels = px(16.);

/// What heads a group of gestures, ahead of the scope they belong to.
const POINTER_HEADING: &str = "Pointer";

/// The headline the sheet reads while the filter matches nothing.
const NO_MATCH: &str = "Nothing matches";

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
    holds: Holds,
    /// How deep the scope stands in `stack`, by which a binding naming several
    /// scopes is filed under the innermost.
    depth: usize,
    stack: Vec<KeyContext>,
}

/// Which bindings a [`Slot`] files.
enum Holds {
    /// Those naming the scope a predicate names by this identifier.
    Scope(&'static str),
    /// Those scoped to no key context, or to the frames of these identifiers.
    Unscoped(Vec<&'static str>),
}

impl Holds {
    fn named_by(&self, named: &[SharedString]) -> bool {
        let names = |id: &str| named.iter().any(|name| name == id);
        match self {
            Holds::Scope(id) => names(id),
            Holds::Unscoped(frames) => frames.iter().any(|frame| names(frame)),
        }
    }
}

/// The bindings of `keymap` in effect for the panel scoped to `panel`, grouped
/// by the scope each binding's predicate names: the panel's own, then each
/// scope declared within it, then each enclosing scope `stack` holds from the
/// innermost out, then the bindings scoped to none or to a window's frame.
/// `stack` runs from the window's root to the panel's own context. A binding is
/// in effect where the keymap dispatches it ahead of every other binding of its
/// keystrokes, a scope within the panel counting as entered, so that the keys
/// of a field or a state are listed while it is not in effect. `label` names an
/// action's row.
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
        holds: Holds::Scope(panel.id()),
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
                holds: Holds::Scope(scope.id()),
                depth: panel_depth + chain.len(),
                stack: entered,
            });
        }
    }
    let mut frames = Vec::new();
    for depth in (0..panel_depth).rev() {
        for scope in scopes
            .iter()
            .filter(|scope| stack[depth].contains(scope.id()))
        {
            if scope.is_frame() {
                frames.push(scope.id());
                continue;
            }
            slots.push(Slot {
                label: scope.label(),
                holds: Holds::Scope(scope.id()),
                depth,
                stack: stack.to_vec(),
            });
        }
    }
    slots.push(Slot {
        label: UNSCOPED_LABEL,
        holds: Holds::Unscoped(frames),
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
/// or the bindings scoped to none for a predicate naming no scope or only a
/// window's frame. `None` for a binding naming only scopes that are neither the
/// panel's nor around it.
fn slot_of(binding: &KeyBinding, slots: &[Slot]) -> Option<usize> {
    let unscoped = || {
        slots
            .iter()
            .position(|slot| matches!(slot.holds, Holds::Unscoped(_)))
    };
    let Some(predicate) = binding.predicate() else {
        return unscoped();
    };
    let mut named = Vec::new();
    identifiers(&predicate, &mut named);
    if named.is_empty() {
        return unscoped();
    }
    slots
        .iter()
        .enumerate()
        .filter(|(_, slot)| slot.holds.named_by(&named))
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

/// Gestures a scope answers to, headed by that scope's label.
pub struct GestureGroup {
    pub scope: &'static str,
    pub rows: Vec<GestureRow>,
}

/// One line of the sheet: what something does, and what does it.
struct Line {
    label: SharedString,
    keys: SharedString,
}

impl Line {
    fn matches(&self, filter: &str) -> bool {
        self.label.to_lowercase().contains(filter) || self.keys.to_lowercase().contains(filter)
    }
}

struct Section {
    heading: SharedString,
    lines: Vec<Line>,
}

/// What [`KeysHelp`] tells its holder.
pub enum KeysHelpEvent {
    /// The help has closed and is to be let go. `holding_focus` says whether
    /// focus was still within it, which the holder is then to place.
    Dismissed { holding_focus: bool },
}

/// The keys and gestures of a panel as a sheet, narrowed by a filter field at
/// its top, closing once focus leaves it.
pub struct KeysHelp {
    sections: Vec<Section>,
    filter: Entity<QueryField>,
    focus_handle: FocusHandle,
    scroll: ListScroll,
    /// The tallest the sheet stands, past which its lines scroll.
    max_height: Pixels,
    /// Whether it has told its holder it closed, after which it answers
    /// nothing in the frame before the holder lets it go.
    closed: bool,
    _while_up: [Subscription; 2],
}

impl EventEmitter<KeysHelpEvent> for KeysHelp {}

impl KeysHelp {
    /// The help listing `keys`, then `gestures`, standing at most
    /// `max_height` tall and taking focus into its filter.
    pub fn new(
        keys: Vec<KeyGroup>,
        gestures: Vec<GestureGroup>,
        max_height: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut sections = Vec::with_capacity(keys.len() + gestures.len());
        sections.extend(keys.into_iter().map(|group| {
            Section {
                heading: group.label.into(),
                lines: group
                    .rows
                    .into_iter()
                    .map(|row| Line {
                        label: row.label,
                        keys: row.keys.join(", ").into(),
                    })
                    .collect(),
            }
        }));
        sections.extend(
            gestures
                .into_iter()
                .filter(|group| !group.rows.is_empty())
                .map(|group| Section {
                    heading: format!("{POINTER_HEADING}: {}", group.scope).into(),
                    lines: group
                        .rows
                        .into_iter()
                        .map(|row| Line {
                            label: row.does.into(),
                            keys: row.gesture.text(KeyStyle::NATIVE).into(),
                        })
                        .collect(),
                }),
        );
        let filter_width = SHEET_WIDTH - ROW_INSET * 2. - CLEAR_ROOM;
        let filter = cx.new(|cx| {
            QueryField::new(
                "keys-help-filter",
                "Filter keys and gestures",
                filter_width,
                cx,
            )
        });
        let edited = cx.subscribe(&filter, |help, _, event: &QueryFieldEvent, cx| {
            if *event == QueryFieldEvent::Edited {
                help.scroll.to_top();
                cx.notify();
            }
        });
        let focus_handle = cx.focus_handle();
        let left = cx.on_focus_out(&focus_handle, window, |help, _, window, cx| {
            help.close(window, cx);
        });
        filter.read(cx).focus_handle(cx).focus(window, cx);
        Self {
            sections,
            filter,
            focus_handle,
            scroll: ListScroll::new(),
            max_height,
            closed: false,
            _while_up: [edited, left],
        }
    }

    pub fn set_max_height(&mut self, max_height: Pixels, cx: &mut Context<Self>) {
        if self.max_height != max_height {
            self.max_height = max_height;
            cx.notify();
        }
    }

    /// Closes the help, telling its holder whether focus was within it.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.closed = true;
        let holding_focus = self.focus_handle.contains_focused(window, cx);
        cx.emit(KeysHelpEvent::Dismissed { holding_focus });
    }

    fn list(&self, filter: &str, colors: &ThemeColors, cx: &mut Context<Self>) -> impl IntoElement {
        let listed = self.sections.iter().map(|section| section.lines.len() + 1);
        let mut children = Vec::with_capacity(listed.sum());
        for section in &self.sections {
            let mut lines = section
                .lines
                .iter()
                .filter(|line| line.matches(filter))
                .peekable();
            if lines.peek().is_none() {
                continue;
            }
            children.push(heading(section.heading.clone(), colors).into_any_element());
            children.extend(lines.map(|line| row(line, colors).into_any_element()));
        }
        if children.is_empty() {
            children.push(
                div()
                    .px(ROW_INSET)
                    .py_2()
                    .text_color(colors.text_muted)
                    .child(NO_MATCH)
                    .into_any_element(),
            );
        }
        let list = div()
            .id("keys-help-list")
            .flex()
            .flex_col()
            .w_full()
            .pr(px(SCROLLBAR_THICKNESS))
            .overflow_y_scroll()
            .track_scroll(self.scroll.handle())
            .children(children);
        let bar = list_scrollbar(
            cx.entity(),
            |help: &Self| &help.scroll,
            |help: &mut Self| &mut help.scroll,
            ScrollbarColors::from_colors(colors),
        );
        div()
            .relative()
            .flex()
            .flex_1()
            .min_h_0()
            .child(list)
            .child(bar)
    }
}

/// The line heading a section.
fn heading(text: SharedString, colors: &ThemeColors) -> impl IntoElement {
    div()
        .px(ROW_INSET)
        .pt_2()
        .pb_1()
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(colors.text_accent)
        .child(text)
}

/// One line of a section: what it does on the left, what does it on the right
/// in the ink a menu names keys in. What does it keeps to one line, so a line
/// too long for the sheet wraps its label and not the keys or gesture.
fn row(line: &Line, colors: &ThemeColors) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_start()
        .gap(COLUMN_GAP)
        .px(ROW_INSET)
        .py_0p5()
        .text_sm()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(colors.text)
                .child(line.label.clone()),
        )
        .child(
            div()
                .flex_none()
                .whitespace_nowrap()
                .text_right()
                .text_color(colors.text_muted)
                .child(line.keys.clone()),
        )
}

impl Focusable for KeysHelp {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for KeysHelp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = bviz_ui_theme::theme(window, cx).colors;
        let filter = self.filter.read(cx).text(cx).to_lowercase();
        menu_chrome::surface(div(), &colors)
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|help, _: &Close, window, cx| help.close(window, cx)))
            .occlude()
            .flex()
            .flex_col()
            .w(SHEET_WIDTH)
            .max_h(self.max_height)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|help, event: &MouseDownEvent, _, cx| {
                    if help.scroll.press(event.position) {
                        cx.notify();
                    }
                }),
            )
            // The sheet tracks focus only to tell when focus leaves it, so a
            // press within it leaves focus in the filter.
            .on_any_mouse_down(|_, window, _| window.prevent_default())
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex_none()
                    .px(ROW_INSET)
                    .pb_1()
                    .border_b_1()
                    .border_color(colors.border)
                    .child(self.filter.clone()),
            )
            .child(self.list(&filter, &colors, cx))
    }
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

    /// A frame's bindings reach wherever focus stands in the window, so they
    /// are listed with the bindings scoped to none, after every scope around
    /// the panel however deep the frame stands among them.
    #[test]
    fn a_frames_bindings_are_listed_with_those_scoped_to_none() {
        const FRAME: KeyScope = KeyScope::frame("Window");
        let keymap = Keymap::new(vec![
            KeyBinding::new("tab", StepBack, Some("Window")),
            KeyBinding::new("secondary-k m", ToggleMaximize, Some("Workspace")),
            KeyBinding::new("secondary-q", Quit, None),
        ]);
        let stack = [WORKSPACE.into(), FRAME.into(), GRAPH.into()];
        let groups = keys_in_effect(
            &keymap,
            &[WORKSPACE, FRAME, GRAPH],
            &stack,
            GRAPH,
            |action| action.name().rsplit("::").next().unwrap_or_default().into(),
            KeyStyle::Words,
        );
        let read: Vec<(&str, Vec<SharedString>)> = groups
            .into_iter()
            .map(|group| {
                (
                    group.label,
                    group.rows.into_iter().map(|row| row.label).collect(),
                )
            })
            .collect();
        assert_eq!(
            read,
            [
                ("Panes & windows", vec!["ToggleMaximize".into()]),
                (UNSCOPED_LABEL, vec!["StepBack".into(), "Quit".into()]),
            ]
        );
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
