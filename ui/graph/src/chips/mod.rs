//! The query bar: chips written in Bazel's query language, stacked into the
//! scope the panel draws within.
//!
//! What the stack and the chip being typed name is counted off the UI thread
//! as they change; the filter takes the stack up only once it is applied,
//! since restricting the drawing to a scope costs a whole relayout.

mod complete;
mod pill;
mod reorder;

pub(crate) use complete::Completions;

use crate::menu::over_window;
use crate::{GraphPanel, Palette, checklist};
use bviz_graph_layout::Input;
use bviz_graph_layout::query_lang::{
    ChipId, Command, Evaluation, Outcome, Prefixes, Stack, Universe,
};
use bviz_graph_layout::select::{Filter, Scope};
use bviz_ui_components::{
    Button, Menu, MenuEvent, MenuItem, QueryField, QueryFieldEvent, load_in_background, text_input,
};
use gpui::{
    Action, AnyElement, App, ClickEvent, ClipboardItem, Context, Entity, FocusHandle, Focusable,
    KeyBinding, Pixels, Point, ScrollHandle, SharedString, Subscription, Task, Window, actions,
    div, prelude::*, px,
};
use std::sync::Arc;

actions!(
    bviz_graph_chips,
    [
        /// Focus query field
        Focus,
        /// Add text as chip
        Commit,
        /// Apply query
        Apply,
        /// Leave query bar
        Leave,
        /// Select previous chip
        SelectPrevious,
        /// Select next chip
        SelectNext,
        /// Enable or disable chip
        Toggle,
        /// Negate chip
        Negate,
        /// Move chip back
        MoveBack,
        /// Move chip forward
        MoveForward,
        /// Remove chip
        Remove,
        /// Edit chip
        Edit,
        /// Copy chip
        CopyChip,
        /// Copy as Bazel query
        CopyStack,
        /// Clear query
        ClearQuery,
        /// Revert query
        RevertQuery,
        /// Aim at previous completion
        CompletePrevious,
        /// Aim at next completion
        CompleteNext,
        /// Accept completion
        AcceptCompletion,
        /// Close completions
        CloseCompletion,
    ]
);

/// The key context the bar carries, scoping the keys that act on its chips.
pub(crate) const KEY_CONTEXT: &str = "GraphChips";

/// The key context the bar's field carries, inside the bar's.
pub(crate) const FIELD_KEY_CONTEXT: &str = "GraphChipField";

/// The key context the field carries inside its own while the list
/// completing its word stands open, scoping the keys acting on the list.
pub(crate) const COMPLETION_KEY_CONTEXT: &str = "GraphChipCompletion";

/// The key context the field carries inside the completion's while the
/// arrow keys have moved onto a row, which Enter then accepts.
pub(crate) const AIMED_KEY_CONTEXT: &str = "GraphChipCompletionAimed";

/// Width the field is drawn at, wide enough for a call over a package.
const FIELD_WIDTH: Pixels = px(200.);

/// The widest the bar's notice stands before it wraps.
const NOTICE_WIDTH: Pixels = px(480.);

const PLACEHOLDER: &str = "query: //foo/... or deps()";

/// The bar's keys; `outside_text` scopes those reaching it from the panel.
pub(crate) fn bindings(outside_text: Option<&str>) -> Vec<KeyBinding> {
    let field = Some(FIELD_KEY_CONTEXT);
    let on_chips = format!("{KEY_CONTEXT} && !{FIELD_KEY_CONTEXT}");
    let on_chips = Some(on_chips.as_str());
    let completing = Some(COMPLETION_KEY_CONTEXT);
    let aimed = Some(AIMED_KEY_CONTEXT);
    vec![
        KeyBinding::new("up", CompletePrevious, completing),
        KeyBinding::new("down", CompleteNext, completing),
        KeyBinding::new("tab", AcceptCompletion, completing),
        KeyBinding::new("enter", AcceptCompletion, aimed),
        KeyBinding::new("escape", CloseCompletion, completing),
        KeyBinding::new("q", Focus, outside_text),
        KeyBinding::new("secondary-enter", Apply, outside_text),
        KeyBinding::new("secondary-enter", Apply, Some(KEY_CONTEXT)),
        KeyBinding::new("enter", Commit, field),
        KeyBinding::new("escape", Leave, field),
        KeyBinding::new("left", SelectPrevious, on_chips),
        KeyBinding::new("right", SelectNext, on_chips),
        KeyBinding::new("space", Toggle, on_chips),
        KeyBinding::new("!", Negate, on_chips),
        KeyBinding::new("alt-up", MoveBack, on_chips),
        KeyBinding::new("alt-down", MoveForward, on_chips),
        KeyBinding::new("delete", Remove, on_chips),
        KeyBinding::new("backspace", Remove, on_chips),
        KeyBinding::new("enter", Edit, on_chips),
        KeyBinding::new("escape", Leave, on_chips),
        KeyBinding::new("secondary-c", CopyChip, on_chips),
        KeyBinding::new("secondary-shift-c", CopyStack, outside_text),
        KeyBinding::new("secondary-shift-c", CopyStack, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-shift-backspace", ClearQuery, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-z", RevertQuery, Some(KEY_CONTEXT)),
    ]
}

/// The bar's state, held by the panel.
pub(crate) struct Chips {
    stack: Stack,
    field: Entity<QueryField>,
    /// Held by the bar while a chip is selected, so that the keys acting on
    /// chips reach the bar.
    focus_handle: FocusHandle,
    selected: Option<ChipId>,
    /// The chip the field holds the text of, which committing the field
    /// rewrites.
    editing: Option<ChipId>,
    /// Counts each change to the stack or the field, so that an evaluation of
    /// a stack since changed lands nothing.
    generation: u64,
    landed: Option<Landed>,
    /// The stack the filter's scope was last set from, and the query naming
    /// that scope; `None` while the filter holds no scope the bar set.
    applied: Option<(Stack, Arc<str>)>,
    /// Whether the stack, with the field's chip in place, names other than
    /// what was applied; restated on every change to either.
    staged: bool,
    /// What the bar last has to say of an action taken on it, until the stack
    /// or the field changes.
    notice: Option<SharedString>,
    /// The values of the stack's prefixes over the loaded graph, which only
    /// that graph's ids index.
    prefixes: Arc<Prefixes>,
    /// Scrolls the row of chips.
    scroll: ScrollHandle,
    /// Where a press on a pill stands, which carried far enough drags the
    /// chip.
    gesture: reorder::Gesture,
    completer: complete::Completer,
    menu: Option<OpenMenu>,
    _evaluating: Task<()>,
    /// Applies the stack committed last; replacing it gives up an apply still
    /// under way.
    _applying: Task<()>,
    /// Counts what the field's chip names once typing settles.
    _typed: Subscription,
}

/// What evaluating the stack with the field's chip in place came to.
struct Landed {
    generation: u64,
    /// The chip the field held, where it held text and edited none.
    draft: Option<ChipId>,
    evaluation: Evaluation,
}

struct OpenMenu {
    menu: Entity<Menu>,
    _dismissed: Subscription,
}

impl Chips {
    pub(crate) fn new(cx: &mut Context<GraphPanel>) -> Self {
        let field = cx.new(|cx| QueryField::new("chips", PLACEHOLDER, FIELD_WIDTH, cx));
        let typed = cx.subscribe(&field, |view, _, event: &QueryFieldEvent, cx| match event {
            QueryFieldEvent::CursorMoved => {
                complete::follow(view, cx);
                cx.notify();
            }
            QueryFieldEvent::Edited => {
                complete::follow(view, cx);
                view.chips.restage(cx);
                cx.notify();
            }
            QueryFieldEvent::Settled => changed(view, cx),
        });
        Chips {
            stack: Stack::default(),
            field,
            focus_handle: cx.focus_handle(),
            selected: None,
            editing: None,
            generation: 0,
            landed: None,
            applied: None,
            staged: false,
            notice: None,
            prefixes: Arc::default(),
            scroll: ScrollHandle::new(),
            gesture: reorder::Gesture::Idle,
            completer: complete::Completer::default(),
            menu: None,
            _evaluating: Task::ready(()),
            _applying: Task::ready(()),
            _typed: typed,
        }
    }

    /// The stack with the field's text in place: as the chip it edits, or as
    /// a chip on top where it edits none. Returns the chip added.
    fn with_field(&self, cx: &App) -> (Stack, Option<ChipId>) {
        let text = self.field.read(cx).text(cx);
        let mut stack = self.stack.clone();
        let text = text.trim();
        let draft = match self.editing {
            Some(id) => {
                if !text.is_empty() {
                    stack.set_text(id, text);
                }
                None
            }
            None => (!text.is_empty()).then(|| stack.push(text)),
        };
        (stack, draft)
    }

    /// The evaluation landed for the stack as it stands, if one has.
    fn current(&self) -> Option<&Landed> {
        self.landed
            .as_ref()
            .filter(|landed| landed.generation == self.generation)
    }

    /// Scrolls the row of chips to the chip `id`, wherever its group put it.
    /// The last source stands just ahead of the first refinement, so for it
    /// the row scrolls to that refinement instead: both come into view, with
    /// the rule between them, wherever the row holds room for the two.
    fn scroll_to(&self, id: ChipId) {
        let stack = &self.stack;
        let last_source = stack.sources().last().map(|chip| chip.id());
        let at = match last_source == Some(id) && !stack.refinements().is_empty() {
            true => Some(stack.sources().len()),
            false => stack.position(id),
        };
        if let Some(at) = at {
            self.scroll.scroll_to_item(at);
        }
    }

    fn restage(&mut self, cx: &App) {
        let (stack, _) = self.with_field(cx);
        let written = stack
            .compile()
            .map(|compiled| compiled.map(|c| c.expr.to_string()));
        let applied = self.applied.as_ref().map(|(_, query)| &**query);
        self.staged = match written {
            Ok(written) => written.as_deref() != applied,
            Err(_) => true,
        };
    }
}

/// Starts the bar over a freshly read graph: the chips stay, but what they
/// named was named of the graph replaced, so they are evaluated afresh, and
/// the stack that was applied is applied again.
pub(crate) fn reset(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    complete::release(view);
    let chips = &mut view.chips;
    chips.prefixes = Arc::default();
    chips.landed = None;
    let applied = chips.applied.take();
    changed(view, cx);
    if let Some((stack, _)) = applied {
        apply(view, stack, cx);
    }
}

/// Takes up a change to the stack or the field: counts it afresh off the UI
/// thread.
fn changed(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    view.chips.generation += 1;
    view.chips.notice = None;
    view.chips.restage(cx);
    evaluate(view, cx);
    cx.notify();
}

fn evaluate(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let (stack, draft) = view.chips.with_field(cx);
    let generation = view.chips.generation;
    // An empty stack names nothing, which needs no universe to know: a graph
    // nobody queries never prepares one.
    if stack.is_empty() {
        view.chips._evaluating = Task::ready(());
        let evaluation = Evaluation {
            chips: Vec::new(),
            value: None,
        };
        let nothing = Landed {
            generation,
            draft,
            evaluation,
        };
        landed(view, nothing, cx);
        return;
    }
    let Some(universe) = view.model.read(cx).universe() else {
        return;
    };
    let prefixes = Arc::clone(&view.chips.prefixes);
    view.chips._evaluating = load_in_background(
        cx,
        move || Landed {
            generation,
            draft,
            evaluation: stack.evaluate(universe.get(), &prefixes),
        },
        landed,
    );
}

fn landed(view: &mut GraphPanel, landed: Landed, cx: &mut Context<GraphPanel>) {
    if landed.generation == view.chips.generation {
        view.chips.landed = Some(landed);
        cx.notify();
    }
}

/// The scope `stack` names over `universe`, `None` where it names none; or
/// why it cannot be applied.
fn scope_of(
    stack: &Stack,
    universe: &Universe,
    prefixes: &Prefixes,
) -> Result<Option<Scope>, SharedString> {
    let compiled = match stack.compile() {
        Ok(None) => return Ok(None),
        Ok(Some(compiled)) => compiled,
        Err(invalid) => return Err(format!("not applied: {}", invalid.error.message).into()),
    };
    let evaluation = stack.evaluate(universe, prefixes);
    let Some(value) = &evaluation.value else {
        let failed = evaluation
            .chips
            .iter()
            .find_map(|(_, outcome)| match outcome {
                Outcome::Failed(error) => Some(error.message.clone()),
                _ => None,
            });
        let why = failed.unwrap_or_else(|| "a chip does not evaluate".to_string());
        return Err(format!("not applied: {why}").into());
    };
    Ok(Some(universe.scope(compiled.expr.to_string(), value)))
}

/// Scopes the filter to what `stack` names, off the UI thread, giving up an
/// apply of a stack committed before.
fn apply(view: &mut GraphPanel, stack: Stack, cx: &mut Context<GraphPanel>) {
    if matches!(stack.compile(), Ok(None)) {
        view.chips._applying = Task::ready(());
        scope_landed(view, stack, Ok(None), cx);
        return;
    }
    let Some(universe) = view.model.read(cx).universe() else {
        return;
    };
    let prefixes = Arc::clone(&view.chips.prefixes);
    view.chips._applying = load_in_background(
        cx,
        move || {
            let scope = scope_of(&stack, universe.get(), &prefixes);
            (stack, scope)
        },
        |view, (stack, scope), cx| scope_landed(view, stack, scope, cx),
    );
}

fn scope_landed(
    view: &mut GraphPanel,
    stack: Stack,
    scope: Result<Option<Scope>, SharedString>,
    cx: &mut Context<GraphPanel>,
) {
    let scope = match scope {
        Ok(scope) => scope,
        Err(why) => {
            view.chips.notice = Some(why);
            cx.notify();
            return;
        }
    };
    view.chips.applied = scope
        .as_ref()
        .map(|scope| (stack, Arc::from(scope.query())));
    view.chips.restage(cx);
    let filter = Filter {
        scope,
        ..view.derived.filter().clone()
    };
    checklist::set_filter_framed(view, filter, cx);
    cx.notify();
}

/// Puts the field's text on the stack, as the chip it edits or as a chip on
/// top, empties the field and applies the stack. Text that does not parse
/// stays in the field, the bar saying why.
fn commit(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let text = view.chips.field.read(cx).text(cx);
    let text = text.trim();
    if let Some(error) = syntax_error(text) {
        view.chips.notice = Some(format!("not added: {error}").into());
        cx.notify();
        return;
    }
    match view.chips.editing.take() {
        Some(id) if text.is_empty() => {
            view.chips.stack.remove(id);
        }
        Some(id) => {
            view.chips.stack.set_text(id, text);
            view.chips.scroll_to(id);
        }
        None if text.is_empty() => {}
        None => {
            let id = view.chips.stack.push(text);
            view.chips.scroll_to(id);
        }
    }
    view.chips.field.update(cx, |field, cx| field.clear(cx));
    changed(view, cx);
    let stack = view.chips.stack.clone();
    apply(view, stack, cx);
}

/// Why `text` does not parse as a chip; `None` where it does, or is empty.
fn syntax_error(text: &str) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    let mut probe = Stack::default();
    let id = probe.push(text);
    let chip = probe.chip(id)?;
    chip.parsed().err().map(|error| error.message.clone())
}

/// Adds a chip reading `text` on top of the stack and applies it.
pub(crate) fn push(view: &mut GraphPanel, text: &str, cx: &mut Context<GraphPanel>) {
    let id = view.chips.stack.push(text);
    view.chips.scroll_to(id);
    changed(view, cx);
    let stack = view.chips.stack.clone();
    apply(view, stack, cx);
}

/// Takes the focus into the field.
pub(crate) fn focus(view: &mut GraphPanel, window: &mut Window, cx: &mut Context<GraphPanel>) {
    view.chips.selected = None;
    let field = view.chips.field.read(cx).focus_handle(cx);
    field.focus(window, cx);
    cx.notify();
}

/// Gives the focus back to the panel, dropping an edit under way.
fn leave(view: &mut GraphPanel, window: &mut Window, cx: &mut Context<GraphPanel>) {
    view.chips.selected = None;
    complete::release(view);
    if view.chips.editing.take().is_some() {
        view.chips.field.update(cx, |field, cx| field.clear(cx));
    }
    view.focus_handle.focus(window, cx);
    cx.notify();
}

/// Selects the chip `id`, taking the focus onto the bar and scrolling the
/// chip into view.
fn select(view: &mut GraphPanel, id: ChipId, window: &mut Window, cx: &mut Context<GraphPanel>) {
    view.chips.selected = Some(id);
    if let Some(at) = view.chips.stack.position(id) {
        view.chips.scroll.scroll_to_item(at);
    }
    view.chips.focus_handle.focus(window, cx);
    cx.notify();
}

/// Moves the selection `step` chips along; past the last chip the field takes
/// the focus.
fn step(view: &mut GraphPanel, step: isize, window: &mut Window, cx: &mut Context<GraphPanel>) {
    let stack = &view.chips.stack;
    let at = (view.chips.selected).and_then(|id| stack.position(id));
    let Some(at) = at else {
        return;
    };
    let next = at.checked_add_signed(step);
    match next.map(|next| stack.get(next).map(|chip| chip.id())) {
        Some(Some(next)) => select(view, next, window, cx),
        Some(None) => focus(view, window, cx),
        None => {}
    }
}

/// Runs `edit` on the selected chip's stack entry and re-evaluates the stack.
fn edit_selected(
    view: &mut GraphPanel,
    cx: &mut Context<GraphPanel>,
    edit: fn(&mut Stack, ChipId),
) {
    if let Some(id) = view.chips.selected {
        edit(&mut view.chips.stack, id);
        view.chips.scroll_to(id);
        changed(view, cx);
    }
}

fn move_selected(view: &mut GraphPanel, by: isize, cx: &mut Context<GraphPanel>) {
    if let Some(id) = view.chips.selected
        && view.chips.stack.move_by(id, by)
    {
        view.chips.scroll_to(id);
        changed(view, cx);
    }
}

/// Takes the chip `id` off the stack, selecting the one that took its place.
fn remove(view: &mut GraphPanel, id: ChipId, window: &mut Window, cx: &mut Context<GraphPanel>) {
    let Some(at) = view.chips.stack.remove(id) else {
        return;
    };
    if view.chips.editing == Some(id) {
        view.chips.editing = None;
        view.chips.field.update(cx, |field, cx| field.clear(cx));
    }
    changed(view, cx);
    let stack = &view.chips.stack;
    let next = stack
        .get(at)
        .or_else(|| stack.chips().last())
        .map(|chip| chip.id());
    match next {
        Some(next) if view.chips.selected == Some(id) => select(view, next, window, cx),
        None if view.chips.selected == Some(id) => focus(view, window, cx),
        _ => {}
    }
}

/// Puts the chip `id`'s text in the field for editing.
fn edit(view: &mut GraphPanel, id: ChipId, window: &mut Window, cx: &mut Context<GraphPanel>) {
    let Some(chip) = view.chips.stack.chip(id) else {
        return;
    };
    let text = chip.text().to_string();
    view.chips.editing = Some(id);
    view.chips
        .field
        .update(cx, |field, cx| field.set_text(text, cx));
    complete::hold_closed(view, cx);
    focus(view, window, cx);
}

/// Copies the text of the chip `id`, the bar saying it did.
fn copy_chip(view: &mut GraphPanel, id: ChipId, _: &mut Window, cx: &mut Context<GraphPanel>) {
    let Some(chip) = view.chips.stack.chip(id) else {
        return;
    };
    cx.write_to_clipboard(ClipboardItem::new_string(chip.text().to_string()));
    view.chips.notice = Some(format!("copied {}", chip.text()).into());
    cx.notify();
}

/// Selects the last chip, for a backspace in the emptied field.
fn select_last(view: &mut GraphPanel, window: &mut Window, cx: &mut Context<GraphPanel>) -> bool {
    let empty = view.chips.field.read(cx).text(cx).is_empty();
    match view.chips.stack.chips().last().map(|chip| chip.id()) {
        Some(last) if empty => {
            select(view, last, window, cx);
            true
        }
        _ => false,
    }
}

/// Puts up the menu of what can be done with the chip `id` at `at`.
fn open_menu(
    view: &mut GraphPanel,
    id: ChipId,
    at: Point<Pixels>,
    window: &mut Window,
    cx: &mut Context<GraphPanel>,
) {
    if view.chips.stack.chip(id).is_none() {
        return;
    }
    let panel = cx.weak_entity();
    let act = |act: fn(&mut GraphPanel, ChipId, &mut Window, &mut Context<GraphPanel>)| {
        let panel = panel.clone();
        move |window: &mut Window, cx: &mut App| {
            panel.update(cx, |view, cx| act(view, id, window, cx)).ok();
        }
    };
    let keys_in = Some(&view.chips.focus_handle);
    let item = |action: &dyn Action, chosen| MenuItem::action(action, keys_in, window, cx, chosen);
    let items = vec![
        item(
            &Negate,
            act(|view, id, _, cx| {
                view.chips.stack.toggle_negated(id);
                view.chips.scroll_to(id);
                changed(view, cx);
            }),
        ),
        item(
            &Toggle,
            act(|view, id, _, cx| {
                view.chips.stack.toggle_enabled(id);
                changed(view, cx);
            }),
        ),
        item(&Edit, act(edit)),
        item(&Remove, act(remove)),
        item(&CopyChip, act(copy_chip)),
    ];
    view.chips.selected = Some(id);
    put_up(view, items, at, window, cx);
}

/// Puts up the menu of what can be done with the whole stack at `at`.
fn open_stack_menu(
    view: &mut GraphPanel,
    at: Point<Pixels>,
    window: &mut Window,
    cx: &mut Context<GraphPanel>,
) {
    let panel = cx.weak_entity();
    let act = |act: fn(&mut GraphPanel, &mut Context<GraphPanel>)| {
        let panel = panel.clone();
        move |_: &mut Window, cx: &mut App| {
            panel.update(cx, act).ok();
        }
    };
    let keys_in = Some(&view.chips.focus_handle);
    let item = |action: &dyn Action, chosen| MenuItem::action(action, keys_in, window, cx, chosen);
    let mut items = vec![
        item(&CopyStack, act(copy_stack)),
        item(&ClearQuery, act(clear_query)),
    ];
    if view.chips.staged {
        items.push(item(&RevertQuery, act(revert_query)));
    }
    put_up(view, items, at, window, cx);
}

/// Puts the stack applied last back in the bar, emptying the field, which
/// takes back every edit made since; with none applied, the bar is emptied.
fn revert_query(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    if !view.chips.staged {
        return;
    }
    let applied = view.chips.applied.as_ref().map(|(stack, _)| stack.clone());
    view.chips.stack = applied.unwrap_or_default();
    view.chips.selected = None;
    view.chips.editing = None;
    view.chips.field.update(cx, |field, cx| field.clear(cx));
    changed(view, cx);
}

/// Takes every chip off the stack and empties the field. Like every other
/// edit to the stack it is staged, the scope applied standing until the
/// stack is applied or the applied stack is reverted to.
fn clear_query(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let ids: Vec<ChipId> = view.chips.stack.chips().map(|chip| chip.id()).collect();
    for id in ids {
        view.chips.stack.remove(id);
    }
    view.chips.selected = None;
    view.chips.editing = None;
    view.chips.field.update(cx, |field, cx| field.clear(cx));
    changed(view, cx);
}

/// Puts up a menu of `items` at `at`, which gives the focus back to the bar
/// as it closes holding it.
fn put_up(
    view: &mut GraphPanel,
    items: Vec<MenuItem>,
    at: Point<Pixels>,
    window: &mut Window,
    cx: &mut Context<GraphPanel>,
) {
    let menu = cx.new(|cx| Menu::new(items, at, window, cx));
    let dismissed = cx.subscribe_in(&menu, window, |view, _, event: &MenuEvent, window, cx| {
        let MenuEvent::Dismissed { holding_focus } = event;
        view.chips.menu = None;
        if *holding_focus {
            view.chips.focus_handle.focus(window, cx);
        }
        cx.notify();
    });
    view.chips.menu = Some(OpenMenu {
        menu,
        _dismissed: dismissed,
    });
    cx.notify();
}

/// The Bazel command whose output `input` is.
pub(super) fn command(input: Input) -> Command {
    match input {
        Input::Aquery => Command::Aquery,
        Input::Cquery => Command::Cquery,
        Input::Query => Command::Query,
    }
}

/// Copies the stack as a Bazel query, or says on the bar why it is none.
///
/// The query is written for the deepest of the commands the graph was read
/// from, where that command reads every function the stack calls; and the bar
/// says so where the graph joins several commands' output, which no one
/// command reads alike.
pub(crate) fn copy_stack(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let inputs = view
        .model
        .read(cx)
        .loaded()
        .map(|loaded| loaded.shared.inputs);
    let deepest = inputs.and_then(|inputs| inputs.inputs().last());
    let preferred = deepest.map_or(Command::Query, command);
    let joint = inputs.is_some_and(|inputs| inputs.inputs().count() > 1);
    view.chips.notice = Some(match view.chips.stack.bazel_text(preferred) {
        Ok(query) => {
            let command = query.command.name();
            cx.write_to_clipboard(ClipboardItem::new_string(query.text));
            match joint {
                true => format!("copied as bazel {command}; read here over a joint graph").into(),
                false => format!("copied as bazel {command}").into(),
            }
        }
        Err(unportable) => format!("not a Bazel query: {unportable}").into(),
    });
    cx.notify();
}

/// The menu a chip or the stack put up, standing over the whole window.
pub(crate) fn menu(view: &GraphPanel, window: &Window) -> Option<impl IntoElement> {
    let open = view.chips.menu.as_ref()?;
    Some(over_window(&open.menu, window))
}

/// Commits the field and applies the stack, for the key that does so from
/// anywhere in the panel.
pub(crate) fn apply_staged(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    commit(view, cx);
}

/// The bar: the chips, the field, and what applies them.
pub(crate) fn bar(
    view: &GraphPanel,
    palette: &Palette,
    window: &mut Window,
    cx: &mut Context<GraphPanel>,
) -> AnyElement {
    let chips = &view.chips;
    let current = chips.current();
    let outcome = |id: ChipId| {
        let landed = current.or(chips.landed.as_ref())?;
        landed.evaluation.outcome(id).cloned()
    };
    let focused = chips.focus_handle.is_focused(window);
    let mut pills = Vec::with_capacity(chips.stack.len());
    let mut after_source = false;
    for chip in chips.stack.chips() {
        let selected = focused && chips.selected == Some(chip.id());
        let editing = chips.editing == Some(chip.id());
        let outcome = outcome(chip.id());
        let pill = pill::pill(chip, outcome, selected, editing, palette, cx);
        let joins = match (after_source, chip.is_source()) {
            (true, true) => Some(Joint::Or),
            (true, false) => Some(Joint::Then),
            (false, _) => None,
        };
        after_source = chip.is_source();
        // A pill and what joins it to the one before are one child of the
        // row, so the row's children stand at the chips' own positions.
        let (ahead, behind) = reorder::marked(view, chip.id());
        let drop_mark = |shown: bool| {
            shown.then(|| {
                div()
                    .w(DROP_MARK_WIDTH)
                    .h(JOINT_RULE_HEIGHT)
                    .bg(palette.colors.accent)
            })
        };
        pills.push(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .children(joins.map(|joint| joint.mark(palette)))
                .children(drop_mark(ahead))
                .child(pill)
                .children(drop_mark(behind)),
        );
    }
    let draft = current
        .and_then(|landed| Some((landed.draft?, landed)))
        .and_then(|(draft, landed)| landed.evaluation.outcome(draft).cloned());
    let staged = chips.staged;
    let completing = complete::is_shown(view, window, cx);
    let aimed = completing && complete::is_aimed(view);
    let completions = complete::list(view, palette, window, cx);
    let carrying = reorder::handlers(view, cx);
    div()
        .id("graph-chips")
        .relative()
        .key_context(KEY_CONTEXT)
        .track_focus(&chips.focus_handle)
        .on_action(cx.listener(|view, _: &Leave, window, cx| leave(view, window, cx)))
        .on_action(cx.listener(|view, _: &SelectPrevious, window, cx| step(view, -1, window, cx)))
        .on_action(cx.listener(|view, _: &SelectNext, window, cx| step(view, 1, window, cx)))
        .on_action(
            cx.listener(|view, _: &Toggle, _, cx| edit_selected(view, cx, Stack::toggle_enabled)),
        )
        .on_action(
            cx.listener(|view, _: &Negate, _, cx| edit_selected(view, cx, Stack::toggle_negated)),
        )
        .on_action(cx.listener(|view, _: &MoveBack, _, cx| move_selected(view, -1, cx)))
        .on_action(cx.listener(|view, _: &MoveForward, _, cx| move_selected(view, 1, cx)))
        .on_action(cx.listener(|view, _: &Remove, window, cx| {
            if let Some(id) = view.chips.selected {
                remove(view, id, window, cx);
            }
        }))
        .on_action(cx.listener(|view, _: &Edit, window, cx| {
            if let Some(id) = view.chips.selected {
                edit(view, id, window, cx);
            }
        }))
        .on_action(cx.listener(|view, _: &ClearQuery, _, cx| clear_query(view, cx)))
        .on_action(cx.listener(|view, _: &RevertQuery, _, cx| revert_query(view, cx)))
        .on_action(cx.listener(|view, _: &CopyChip, window, cx| {
            if let Some(id) = view.chips.selected {
                copy_chip(view, id, window, cx);
            }
        }))
        .flex()
        .min_w_0()
        .items_center()
        .gap_1()
        .children(carrying)
        .when(!pills.is_empty(), |bar| {
            bar.child(
                div()
                    .id("graph-chip-row")
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap_1()
                    .overflow_x_scroll()
                    .track_scroll(&chips.scroll)
                    .children(pills),
            )
        })
        .child(
            div()
                .flex_none()
                .relative()
                .key_context(FIELD_KEY_CONTEXT)
                .on_action(cx.listener(|view, _: &Commit, _, cx| commit(view, cx)))
                .capture_action(cx.listener(|view, _: &text_input::Backspace, window, cx| {
                    if select_last(view, window, cx) {
                        cx.stop_propagation();
                    }
                }))
                .child(
                    div()
                        .when(completing, |field| {
                            field.key_context(COMPLETION_KEY_CONTEXT)
                        })
                        .on_action(cx.listener(|view, _: &CompletePrevious, _, cx| {
                            complete::aim(view, -1, cx)
                        }))
                        .on_action(
                            cx.listener(|view, _: &CompleteNext, _, cx| complete::aim(view, 1, cx)),
                        )
                        .on_action(cx.listener(|view, _: &AcceptCompletion, window, cx| {
                            complete::accept(view, None, window, cx)
                        }))
                        .on_action(
                            cx.listener(|view, _: &CloseCompletion, _, cx| {
                                complete::close(view, cx)
                            }),
                        )
                        .child(
                            div()
                                .when(aimed, |field| field.key_context(AIMED_KEY_CONTEXT))
                                .child(chips.field.clone()),
                        ),
                )
                .children(completions),
        )
        .children(
            draft
                .and_then(|outcome| pill::draft(&outcome, palette))
                .map(|draft| draft.flex_none()),
        )
        .children(staged.then(|| {
            div()
                .flex_none()
                .size(STAGED_DOT)
                .rounded_full()
                .bg(palette.colors.accent)
        }))
        .child(
            div().flex_none().child(
                Button::new(
                    "chips-apply",
                    cx.listener(|view, _: &ClickEvent, _, cx| commit(view, cx)),
                )
                .label("apply")
                .compact()
                .disabled(!staged),
            ),
        )
        .children((!chips.stack.is_empty() || staged).then(|| {
            div().flex_none().child(
                Button::new(
                    "chips-menu",
                    cx.listener(|view, event: &ClickEvent, window, cx| {
                        open_stack_menu(view, event.position(), window, cx)
                    }),
                )
                .ghost()
                .icon("dots-three-vertical"),
            )
        }))
        .into_any_element()
}

/// What the bar last has to say, standing over the drawing's top-left corner
/// clear of the toolbar, whose row it would otherwise widen.
pub(crate) fn notice(view: &GraphPanel, palette: &Palette) -> Option<impl IntoElement> {
    let notice = view.chips.notice.clone()?;
    Some(
        div()
            .absolute()
            .left_2()
            .top_2()
            .max_w(NOTICE_WIDTH)
            .px_2()
            .py_1()
            .rounded_md()
            .bg(palette.container)
            .text_xs()
            .text_color(palette.label)
            .child(notice),
    )
}

/// What stands between a pill and the one before it: `or` between two
/// sources, whose union the stack starts from, and a rule where the
/// refinements of that union begin.
#[derive(Clone, Copy)]
enum Joint {
    Or,
    Then,
}

impl Joint {
    fn mark(self, palette: &Palette) -> AnyElement {
        match self {
            Joint::Or => div()
                .text_xs()
                .text_color(palette.colors.text_muted)
                .child("or")
                .into_any_element(),
            Joint::Then => div()
                .w(JOINT_RULE_WIDTH)
                .h(JOINT_RULE_HEIGHT)
                .bg(palette.colors.border)
                .into_any_element(),
        }
    }
}

/// The width of the mark standing where a dragged chip would land.
const DROP_MARK_WIDTH: Pixels = px(2.);

/// The rule standing where a stack's refinements begin.
const JOINT_RULE_WIDTH: Pixels = px(1.);
const JOINT_RULE_HEIGHT: Pixels = px(16.);

/// The size of the dot saying the stack holds changes not yet applied.
const STAGED_DOT: Pixels = px(6.);
