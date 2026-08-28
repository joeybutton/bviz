//! A gpui view that renders a build's display [`Terminal`] as styled monospace
//! text: the visible viewport, reflowed to the window, one shaped line per row
//! with per-cell colors and background fills.

use std::time::{Duration, Instant};

use crate::model::{ConsoleModel, SourceReplaced};
use bviz_terminal::{CellPos, Replay, Scroll, SelectionType, Side, Terminal, Url};
use bviz_ui_components::{
    Button, Dropdown, Scrollbar, ScrubberColors, ScrubberState, TextInput, scrubber,
};
use bviz_ui_theme::ThemeColors;
use bviz_ui_workspace::{Panel, PanelEvent, PanelKind, ViewKind};
use gpui::{
    App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Point, Render,
    ScrollWheelEvent, SharedString, Subscription, Window, actions, div, prelude::*, px,
};

mod element;
use element::TerminalElement;

actions!(
    bviz_console,
    [
        PlayPause,
        StepBack,
        StepForward,
        SeekStart,
        SeekEnd,
        SpeedUp,
        SpeedDown,
        ScrollLineUp,
        ScrollLineDown,
        ScrollPageUp,
        ScrollPageDown,
        ScrollToTop,
        ScrollToBottom,
        Copy,
        Find,
        FindNext,
        FindPrev,
        FindClose
    ]
);

/// The key context scoping the console's own bindings, so a key pressed in this
/// panel reaches it and no other.
const CONSOLE_KEY_CONTEXT: &str = "Console";

/// The key context of the region the scrollback grid is painted in.
const GRID_KEY_CONTEXT: &str = "ConsoleGrid";

/// The key context of the bar the find box opens as.
const FIND_KEY_CONTEXT: &str = "ConsoleFind";

/// The key context of the playback bar.
const PLAYBACK_KEY_CONTEXT: &str = "ConsolePlayback";

pub fn register_keys(cx: &mut App) {
    let console_unless_find = format!("{CONSOLE_KEY_CONTEXT} && !{FIND_KEY_CONTEXT}");
    let console_unless_find = Some(console_unless_find.as_str());
    let grid_unless_find = format!("{GRID_KEY_CONTEXT} && !{FIND_KEY_CONTEXT}");
    let grid_unless_find = Some(grid_unless_find.as_str());
    let console = Some(CONSOLE_KEY_CONTEXT);
    let find = Some(FIND_KEY_CONTEXT);
    let playback = Some(PLAYBACK_KEY_CONTEXT);
    cx.bind_keys([
        // Keys that don't collide with terminal scrolling work in either region,
        // but stay inert while the find field has focus so they don't hijack
        // typing and cursor movement there.
        KeyBinding::new("space", PlayPause, console_unless_find),
        KeyBinding::new("left", StepBack, console_unless_find),
        KeyBinding::new("right", StepForward, console_unless_find),
        KeyBinding::new("]", SpeedUp, console_unless_find),
        KeyBinding::new("[", SpeedDown, console_unless_find),
        // The arrows and home/end collide: they scroll the focused terminal but
        // drive playback when the playback bar holds focus.
        KeyBinding::new("up", ScrollLineUp, grid_unless_find),
        KeyBinding::new("down", ScrollLineDown, grid_unless_find),
        KeyBinding::new("home", ScrollToTop, grid_unless_find),
        KeyBinding::new("end", ScrollToBottom, grid_unless_find),
        KeyBinding::new("up", SpeedUp, playback),
        KeyBinding::new("down", SpeedDown, playback),
        KeyBinding::new("home", SeekStart, playback),
        KeyBinding::new("end", SeekEnd, playback),
        // Paging and the commands work regardless of which region has focus.
        KeyBinding::new("pageup", ScrollPageUp, console),
        KeyBinding::new("pagedown", ScrollPageDown, console),
        KeyBinding::new("cmd-c", Copy, console),
        KeyBinding::new("ctrl-c", Copy, console),
        KeyBinding::new("cmd-f", Find, console),
        KeyBinding::new("ctrl-f", Find, console),
        KeyBinding::new("enter", FindNext, find),
        KeyBinding::new("shift-enter", FindPrev, find),
        KeyBinding::new("escape", FindClose, find),
    ]);
}

/// Lines moved per arrow-key press while the terminal is focused.
const SCROLL_STEP: i32 = 3;

/// Lines scrolled per frame while a selection drag is held outside the content.
const SELECTION_AUTOSCROLL_STEP: i32 = 1;

/// How long the scroll bar stays visible after the last scroll interaction.
const SCROLLBAR_HIDE: Duration = Duration::from_millis(1200);

/// Playback speed multipliers the speed control cycles through.
const SPEEDS: [f32; 4] = [1.0, 2.0, 4.0, 8.0];

/// Geometry of the painted terminal content, in window pixels, kept so mouse
/// positions can be mapped to grid cells for selection.
#[derive(Clone, Copy)]
struct ContentLayout {
    left: f32,
    top: f32,
    cell_width: f32,
    line_height: f32,
    columns: usize,
    rows: usize,
}

/// A view replaying a build's captured console output under playback controls.
pub struct ConsolePanel {
    /// Names the document shown, labelling this panel's tab.
    title: SharedString,
    /// The build replayed here, shared with every other view of it.
    model: Entity<ConsoleModel>,
    /// Redraws as the build is read.
    _changed: Subscription,
    /// Puts the playhead on the build as its read lands.
    _source_replaced: Subscription,
    /// The frame currently shown; an index into the build's [`Replay`] frames.
    current_frame: usize,
    /// Whether the timeline is advancing on its own.
    playing: bool,
    /// Playback speed multiplier (see [`SPEEDS`]).
    speed: f32,
    /// The speed selector; its chosen index drives [`speed`](Self::speed).
    speed_dropdown: Entity<Dropdown>,
    /// Keeps the subscription that applies the speed dropdown's selection.
    _speed_observer: Subscription,
    /// While playing, the wall-clock instant playback was last (re)started and the
    /// build time it was at then, so the clock can map elapsed real time to a frame.
    play_anchor: Option<(Instant, f64)>,
    /// The playhead's time in seconds, shown on the scrubber and readout. It tracks
    /// the playback clock continuously, so the playhead glides even when the shown
    /// frame (a coarse build may have few) only changes in steps.
    position: f64,
    scrubber: ScrubberState,
    /// The display terminal, built at the window size for [`current_frame`] and
    /// rebuilt when the frame changes.
    terminal: Option<Terminal>,
    /// The frame [`terminal`] was built for, to detect when it must be rebuilt.
    terminal_frame: Option<usize>,
    /// Whether to keep the viewport pinned to the newest output. Set by an active
    /// scroll to the bottom; cleared by scrolling up.
    follow_bottom: bool,
    /// The remembered scroll target when not following: the line, counted from the
    /// top of the output, held at the viewport top. Only an active user scroll
    /// changes it, so it survives scrubbing past where that line exists yet — the
    /// view clamps to the bottom meanwhile and returns to it when the line is back.
    scroll_top: usize,
    /// The painted scroll bar geometry, set each layout for hit-testing.
    scrollbar: Option<Scrollbar>,
    /// The painted content geometry, set each layout for mapping clicks to cells.
    content: Option<ContentLayout>,
    /// Whether a click-drag text selection is in progress.
    selecting: bool,
    /// The latest cursor position during a selection drag, kept so the selection
    /// can keep auto-scrolling while the cursor is held still outside the view.
    selection_drag: Option<Point<Pixels>>,
    /// The drag's fixed anchor in content coordinates — a line counted from the top
    /// of the scrollback, a column, and a side — so a rebuild can re-anchor it on
    /// the same text while its far end follows the cursor as the view scrolls.
    selection_anchor: Option<(i32, usize, Side)>,
    /// The URL under the cursor, highlighted and openable while hovered.
    hovered_url: Option<Url>,
    /// The URL pressed on, opened on release if the press was a click, not a drag.
    pressed_url: Option<String>,
    /// While dragging the thumb, the cursor's offset from the thumb's top.
    drag_grab: Option<f32>,
    /// Whether the scroll bar is currently shown; it auto-hides after a scroll.
    scrollbar_visible: bool,
    /// Bumped on each reveal so a stale hide timer does not hide a re-revealed bar.
    reveal_seq: u64,
    /// The terminal body's handle, focused so its scroll keys dispatch.
    body_focus: FocusHandle,
    /// The playback bar's handle; when it holds focus the shared arrow and
    /// home/end keys drive playback rather than scroll the terminal.
    playback_focus: FocusHandle,
    /// Whether the find box is open.
    find_active: bool,
    /// The find box's text field, created on first open and reused.
    find_input: Option<Entity<TextInput>>,
    /// Keeps the subscription that re-runs the search on every find-field edit.
    find_observation: Option<Subscription>,
}

impl ConsolePanel {
    pub(crate) fn new(
        title: SharedString,
        model: Entity<ConsoleModel>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Tab cycles focus through the regions (handled by the shell's tab action
        // via window.focus_next), in this order: playback bar, terminal, then the
        // find field when it is open (its tab index is set on creation).
        let body_focus = cx.focus_handle().tab_index(1).tab_stop(true);
        let playback_focus = cx.focus_handle().tab_index(0).tab_stop(true);
        // The speed dropdown is the source of truth for the speed index; applying
        // its selection keeps the numeric multiplier and the playback clock in sync.
        let labels = SPEEDS.iter().map(|speed| format!("{}×", *speed as i32));
        let speed_dropdown = cx.new(|cx| Dropdown::new("speed", labels, 0, cx));
        let speed_observer = cx.observe(&speed_dropdown, |view, dropdown, cx| {
            view.speed = SPEEDS[dropdown.read(cx).selected().min(SPEEDS.len() - 1)];
            if view.playing {
                view.anchor_clock();
            }
            cx.notify();
        });
        let changed = cx.observe(&model, |_, _, cx| cx.notify());
        let source_replaced = cx.subscribe(&model, |view, _, _: &SourceReplaced, cx| {
            view.show_completed_build(cx);
        });
        let mut view = Self {
            title,
            model,
            _changed: changed,
            _source_replaced: source_replaced,
            current_frame: 0,
            playing: false,
            speed: SPEEDS[0],
            speed_dropdown,
            _speed_observer: speed_observer,
            play_anchor: None,
            position: 0.0,
            scrubber: ScrubberState::default(),
            terminal: None,
            terminal_frame: None,
            follow_bottom: true,
            scroll_top: 0,
            scrollbar: None,
            content: None,
            selecting: false,
            selection_drag: None,
            selection_anchor: None,
            hovered_url: None,
            pressed_url: None,
            drag_grab: None,
            scrollbar_visible: false,
            reveal_seq: 0,
            body_focus,
            playback_focus,
            find_active: false,
            find_input: None,
            find_observation: None,
        };
        // Show the bar briefly on launch so the scrollable extent is visible,
        // then let it auto-hide.
        view.reveal_scrollbar(cx);
        view.show_completed_build(cx);
        view
    }

    /// The timeline this view plays, and `None` while the build's read has
    /// landed none.
    fn replay<'a>(&self, cx: &'a App) -> Option<&'a Replay> {
        self.model.read(cx).replay()
    }

    /// Put the playhead at the end of the build: a view opens on the completed
    /// output, ready to replay it from the start.
    fn show_completed_build(&mut self, cx: &App) {
        let Some(replay) = self.replay(cx) else {
            return;
        };
        self.current_frame = replay.frame_count().saturating_sub(1);
        self.position = replay.duration();
    }

    /// Show the scroll bar and schedule it to hide after a period of no further
    /// scroll interaction.
    fn reveal_scrollbar(&mut self, cx: &mut Context<Self>) {
        self.scrollbar_visible = true;
        self.reveal_seq = self.reveal_seq.wrapping_add(1);
        let seq = self.reveal_seq;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SCROLLBAR_HIDE).await;
            let _ = this.update(cx, |this, cx| {
                // Skip if a newer reveal happened or the thumb is being dragged.
                if this.reveal_seq == seq && this.drag_grab.is_none() {
                    this.scrollbar_visible = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn scroll(&mut self, scroll: Scroll, cx: &mut Context<Self>) {
        let Some(terminal) = self.terminal.as_mut() else {
            return;
        };
        terminal.scroll(scroll);
        // The hovered URL's highlight is in viewport coordinates, which scrolling
        // invalidates; the next pointer move recomputes it.
        self.hovered_url = None;
        self.remember_scroll();
        self.reveal_scrollbar(cx);
    }

    /// Record where the user scrolled to: follow the bottom when there, else the
    /// absolute top line, so a rebuild for a new frame restores that position.
    fn remember_scroll(&mut self) {
        if let Some(terminal) = self.terminal.as_ref() {
            self.follow_bottom = terminal.display_offset() == 0;
            self.scroll_top = terminal.history_size() - terminal.display_offset();
        }
    }

    /// Scroll so the thumb's top sits under the cursor, accounting for the grab
    /// offset, and translate that to a line via the terminal.
    fn drag_to(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let (Some(bar), Some(grab)) = (self.scrollbar, self.drag_grab) else {
            return;
        };
        let Some(terminal) = self.terminal.as_mut() else {
            return;
        };
        // The bar's offset runs from 0 (the bottom, no scrollback above) to the
        // history size (the top); a fraction of that history maps to a line.
        let history = terminal.history_size().max(1) as f32;
        let fraction = bar.offset_at(position, grab) / history;
        terminal.scroll_to_fraction(fraction);
        self.remember_scroll();
        cx.notify();
    }

    /// The grid cell (column, row from the top) and the side of it under a window
    /// position, or `None` before the first layout.
    fn cell_at(&self, position: Point<Pixels>) -> Option<(usize, usize, Side)> {
        let c = self.content?;
        let x = (f32::from(position.x) - c.left).max(0.0);
        let y = (f32::from(position.y) - c.top).max(0.0);
        let column = ((x / c.cell_width) as usize).min(c.columns.saturating_sub(1));
        let row = ((y / c.line_height) as usize).min(c.rows.saturating_sub(1));
        let side = if x % c.cell_width > c.cell_width / 2.0 {
            Side::Right
        } else {
            Side::Left
        };
        Some((column, row, side))
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(bar) = self.scrollbar
            && bar.track_hit(event.position)
        {
            self.begin_scrollbar_drag(bar, event.position, cx);
            return;
        }
        self.begin_selection(event, window, cx);
    }

    /// Grabbing the thumb keeps the cursor at the same spot on it, while
    /// clicking the track jumps the thumb's center to the cursor.
    fn begin_scrollbar_drag(
        &mut self,
        bar: Scrollbar,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.drag_grab = Some(bar.grab(position));
        self.drag_to(position, cx);
    }

    /// A press on the content focuses it and begins a text selection.
    fn begin_selection(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.body_focus.focus(window, cx);
        let Some((column, row, side)) = self.cell_at(event.position) else {
            return;
        };
        // A double click selects the word under the cursor, a triple click the
        // whole line; either can then be dragged to extend by words or lines.
        let selection = match event.click_count {
            2 => SelectionType::Semantic,
            3 => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        // A press on a URL opens it on release, unless the press turns into a
        // drag (a selection), so dragging from a link still selects its text.
        self.pressed_url = self.hovered_url.as_ref().map(|url| url.text().to_string());
        if let Some(terminal) = self.terminal.as_mut() {
            terminal.begin_selection(selection, CellPos { column, row }, side);
            // The anchor's line counted from the top of the scrollback, fixed to its
            // text so it survives rebuilds and scrolling under the drag.
            let anchor_line =
                row as i32 + terminal.history_size() as i32 - terminal.display_offset() as i32;
            self.selecting = true;
            self.selection_drag = Some(event.position);
            self.selection_anchor = Some((anchor_line, column, side));
            cx.notify();
        }
    }

    /// Track the URL under the cursor so it can be highlighted and opened.
    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            return;
        }
        // Moving the pointer onto the gutter reveals the hidden scroll bar.
        if let Some(bar) = self.scrollbar
            && bar.track_hit(event.position)
            && !self.scrollbar_visible
        {
            self.reveal_scrollbar(cx);
        }
        let cell = self.cell_at(event.position);
        let url = match (cell, self.terminal.as_mut()) {
            (Some((column, row, _)), Some(terminal)) => terminal.url_at(column, row),
            _ => None,
        };
        if url.as_ref().map(Url::text) != self.hovered_url.as_ref().map(Url::text) {
            self.hovered_url = url;
            cx.notify();
        }
    }

    /// Extend the in-progress selection to a window position.
    fn selection_drag_to(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.selection_drag = Some(position);
        let Some((column, row, side)) = self.cell_at(position) else {
            return;
        };
        if let Some(terminal) = self.terminal.as_mut() {
            terminal.extend_selection(CellPos { column, row }, side);
            cx.notify();
        }
    }

    /// While a selection drag is held above or below the content, scroll that way
    /// and grow the selection to the revealed edge, repeating each frame so it
    /// continues even when the cursor is still. Inside the content it does nothing,
    /// so the selection simply follows the cursor.
    fn autoscroll_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(content), Some(position)) = (self.content, self.selection_drag) else {
            return;
        };
        if !self.selecting {
            return;
        }
        let top = content.top;
        let bottom = content.top + content.line_height * content.rows as f32;
        let y = f32::from(position.y);
        // Positive scrolls toward earlier output (up); the cursor above the content
        // reveals earlier lines, below it reveals later ones.
        let lines = if y < top {
            SELECTION_AUTOSCROLL_STEP
        } else if y > bottom {
            -SELECTION_AUTOSCROLL_STEP
        } else {
            return;
        };
        let moved = match self.terminal.as_mut() {
            Some(terminal) => {
                let before = terminal.display_offset();
                terminal.scroll(Scroll::Delta(lines));
                terminal.display_offset() != before
            }
            None => return,
        };
        self.remember_scroll();
        // Extend to the cell under the cursor, which `cell_at` clamps to the edge
        // row now showing the revealed content.
        self.selection_drag_to(position, cx);
        // Keep going next frame while the cursor stays outside, cursor still or not;
        // stop once the scroll has bottomed out, to not spin at the limit.
        if moved {
            window.request_animation_frame();
        }
    }

    fn scroll_line_up(&mut self, _: &ScrollLineUp, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll(Scroll::Delta(SCROLL_STEP), cx);
    }

    fn scroll_line_down(&mut self, _: &ScrollLineDown, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll(Scroll::Delta(-SCROLL_STEP), cx);
    }

    fn scroll_page_up(&mut self, _: &ScrollPageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll(Scroll::PageUp, cx);
    }

    fn scroll_page_down(&mut self, _: &ScrollPageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll(Scroll::PageDown, cx);
    }

    fn scroll_to_top(&mut self, _: &ScrollToTop, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll(Scroll::Top, cx);
    }

    fn scroll_to_bottom(&mut self, _: &ScrollToBottom, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll(Scroll::Bottom, cx);
    }

    fn on_play_pause(&mut self, _: &PlayPause, _: &mut Window, cx: &mut Context<Self>) {
        self.play_pause(cx);
    }

    fn on_step_back(&mut self, _: &StepBack, _: &mut Window, cx: &mut Context<Self>) {
        self.step(-1, cx);
    }

    fn on_step_forward(&mut self, _: &StepForward, _: &mut Window, cx: &mut Context<Self>) {
        self.step(1, cx);
    }

    fn on_seek_start(&mut self, _: &SeekStart, _: &mut Window, cx: &mut Context<Self>) {
        self.seek_start(cx);
    }

    fn on_seek_end(&mut self, _: &SeekEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.seek_end(cx);
    }

    fn on_speed_up(&mut self, _: &SpeedUp, _: &mut Window, cx: &mut Context<Self>) {
        self.shift_speed(1, cx);
    }

    fn on_speed_down(&mut self, _: &SpeedDown, _: &mut Window, cx: &mut Context<Self>) {
        self.shift_speed(-1, cx);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.terminal.as_ref().and_then(Terminal::selection_text) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// Open the find box, creating and focusing its text field on first use.
    fn find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        // The field is drawn beside the scrollback grid, which stands only over a
        // loaded replay; opening it before then would focus a field no rendered
        // element carries, leaving the window answering no key at all.
        if self.replay(cx).is_none() {
            return;
        }
        self.find_active = true;
        if self.find_input.is_none() {
            // Tab index 2: after the playback bar (0) and terminal (1) in the cycle.
            let input = cx.new(|cx| TextInput::new(cx, "Find", 2));
            self.find_observation =
                Some(cx.observe(&input, |this, _input, cx| this.run_search(cx)));
            self.find_input = Some(input);
        }
        if let Some(input) = self.find_input.as_ref() {
            input.read(cx).focus_handle(cx).focus(window, cx);
        }
        self.run_search(cx);
        cx.notify();
    }

    /// Apply the find field's text as the terminal's search query.
    fn run_search(&mut self, cx: &mut Context<Self>) {
        if !self.find_active {
            return;
        }
        let query = match self.find_input.as_ref() {
            Some(input) => input.read(cx).content().to_string(),
            None => return,
        };
        if let Some(terminal) = self.terminal.as_mut() {
            terminal.set_search(&query);
        }
        // Jumping to a match counts as leaving auto-scroll, so the next frame holds
        // this position rather than snapping back to the bottom.
        self.remember_scroll();
        // The hovered URL's highlight is in viewport coordinates, which scrolling
        // to a match invalidates; the next pointer move recomputes it.
        self.hovered_url = None;
        cx.notify();
    }

    /// Focus the next/previous match, holding the scroll there so playback does not
    /// snap the view back to the bottom.
    fn select_next_match(&mut self, cx: &mut Context<Self>) {
        if let Some(terminal) = self.terminal.as_mut() {
            terminal.search_next();
        }
        self.remember_scroll();
        cx.notify();
    }

    fn select_prev_match(&mut self, cx: &mut Context<Self>) {
        if let Some(terminal) = self.terminal.as_mut() {
            terminal.search_prev();
        }
        self.remember_scroll();
        cx.notify();
    }

    fn find_next(&mut self, _: &FindNext, _: &mut Window, cx: &mut Context<Self>) {
        self.select_next_match(cx);
    }

    fn find_prev(&mut self, _: &FindPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.select_prev_match(cx);
    }

    fn find_close(&mut self, _: &FindClose, window: &mut Window, cx: &mut Context<Self>) {
        self.find_active = false;
        if let Some(terminal) = self.terminal.as_mut() {
            terminal.clear_search();
        }
        self.body_focus.focus(window, cx);
        cx.notify();
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let line_height = window.line_height();
        let lines = (f32::from(event.delta.pixel_delta(line_height).y) / f32::from(line_height))
            .round() as i32;
        if lines != 0 {
            self.scroll(Scroll::Delta(lines), cx);
        }
    }

    /// Toggle between playing and paused.
    fn play_pause(&mut self, cx: &mut Context<Self>) {
        if self.playing {
            self.pause(cx);
        } else {
            self.play(cx);
        }
    }

    /// Start advancing the timeline from the current frame, restarting from the
    /// beginning if it sits at the end. Playback is driven from rendering: while
    /// playing, each frame maps the elapsed real time to a timeline position and
    /// requests the next frame (see [`advance_playback`](Self::advance_playback)).
    fn play(&mut self, cx: &mut Context<Self>) {
        let last = match self.replay(cx) {
            Some(replay) => replay.frame_count().saturating_sub(1),
            None => return,
        };
        if last == 0 {
            return;
        }
        if self.current_frame >= last {
            self.current_frame = 0;
            self.position = 0.0;
        }
        self.playing = true;
        self.anchor_clock();
        cx.notify();
    }

    fn pause(&mut self, cx: &mut Context<Self>) {
        self.playing = false;
        self.play_anchor = None;
        cx.notify();
    }

    /// Anchor the playback clock at the playhead: real time elapsed from now,
    /// scaled by the speed, is added to the current position to find the frame to
    /// show. Anchoring on the playhead rather than the shown frame's time keeps
    /// playback continuous across a coarse timeline's large frame gaps.
    fn anchor_clock(&mut self) {
        self.play_anchor = Some((Instant::now(), self.position));
    }

    /// While playing, show the frame the clock has reached and request another
    /// frame so playback keeps running; stop at the end. Called each render so
    /// playback is paced by the window's frame loop rather than a detached timer,
    /// which a quiescent window stops pumping.
    fn advance_playback(&mut self, window: &mut Window, cx: &App) {
        if !self.playing {
            return;
        }
        let Some((started, base)) = self.play_anchor else {
            return;
        };
        let target = base + started.elapsed().as_secs_f64() * self.speed as f64;
        let (frame, last, duration) = match self.replay(cx) {
            Some(replay) => (
                replay.frame_at(target),
                replay.frame_count().saturating_sub(1),
                replay.duration(),
            ),
            None => return,
        };
        if frame >= last {
            self.current_frame = last;
            self.position = duration;
            self.playing = false;
            self.play_anchor = None;
        } else {
            self.current_frame = frame;
            self.position = target.clamp(0.0, duration);
            window.request_animation_frame();
        }
    }

    /// Show a frame, clamped to the timeline, moving the playhead to its time and
    /// keeping playback running from there.
    fn seek_frame(&mut self, frame: usize, cx: &mut Context<Self>) {
        let (frame, time) = match self.replay(cx) {
            Some(replay) => {
                let frame = frame.min(replay.frame_count().saturating_sub(1));
                (frame, replay.time_of(frame))
            }
            None => return,
        };
        self.current_frame = frame;
        self.position = time;
        if self.playing {
            self.anchor_clock();
        }
        cx.notify();
    }

    fn seek_start(&mut self, cx: &mut Context<Self>) {
        self.seek_frame(0, cx);
    }

    fn seek_end(&mut self, cx: &mut Context<Self>) {
        self.seek_frame(usize::MAX, cx);
    }

    /// Step the shown frame by `delta`, clamped to the timeline.
    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let frame = (self.current_frame as isize + delta).max(0) as usize;
        self.seek_frame(frame, cx);
    }

    /// Move `delta` steps through the playback speeds, wrapping around. Drives the
    /// dropdown, whose observer applies the new speed.
    fn shift_speed(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = SPEEDS.len() as isize;
        let current = self.speed_dropdown.read(cx).selected() as isize;
        let next = (current + delta).rem_euclid(count) as usize;
        self.speed_dropdown
            .update(cx, |dropdown, cx| dropdown.set_selected(next, cx));
    }

    /// Pause and seek to the timeline position at `fraction` along the scrubber.
    fn scrub_to(&mut self, fraction: f32, cx: &mut Context<Self>) {
        // Grabbing the scrubber pauses playback; a drag that follows only seeks,
        // so a keyboard play toggled mid-drag is left standing.
        if !self.scrubber.held() {
            self.playing = false;
            self.play_anchor = None;
        }
        let (frame, position) = match self.replay(cx) {
            Some(replay) => {
                let position = fraction as f64 * replay.duration();
                (replay.frame_at(position), position)
            }
            None => return,
        };
        self.current_frame = frame;
        self.position = position;
        cx.notify();
    }
}

impl EventEmitter<PanelEvent> for ConsolePanel {}

impl Focusable for ConsolePanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.body_focus.clone()
    }
}

impl Panel for ConsolePanel {
    fn kind() -> PanelKind {
        PanelKind {
            view: ViewKind::new("console"),
            label: "Console",
            key_context: CONSOLE_KEY_CONTEXT,
        }
    }

    fn title(&self, _cx: &App) -> SharedString {
        self.title.clone()
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if active {
            self.body_focus.focus(window, cx);
        }
    }
}

impl Render for ConsolePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Advance the timeline if playing, pacing playback by the frame loop.
        self.advance_playback(window, cx);
        self.autoscroll_selection(window, cx);
        let theme = bviz_ui_theme::theme(window, cx);
        let colors = &theme.colors;
        let chrome = div()
            .key_context(Self::kind().key_context)
            .on_action(cx.listener(Self::on_play_pause))
            .on_action(cx.listener(Self::on_step_back))
            .on_action(cx.listener(Self::on_step_forward))
            .on_action(cx.listener(Self::on_seek_start))
            .on_action(cx.listener(Self::on_seek_end))
            .on_action(cx.listener(Self::on_speed_up))
            .on_action(cx.listener(Self::on_speed_down))
            .on_action(cx.listener(Self::scroll_line_up))
            .on_action(cx.listener(Self::scroll_line_down))
            .on_action(cx.listener(Self::scroll_page_up))
            .on_action(cx.listener(Self::scroll_page_down))
            .on_action(cx.listener(Self::scroll_to_top))
            .on_action(cx.listener(Self::scroll_to_bottom))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::find))
            .flex()
            .flex_col()
            .size_full();
        match self.model.read(cx).source().placeholder(colors) {
            // One element tracks the body focus per frame: the grid where it is
            // drawn, and the chrome standing in for it where it is not. Tracking
            // it twice enters it in the tab order twice, and the pair then step
            // to each other instead of on to the next region.
            Some(placeholder) => chrome.track_focus(&self.body_focus).child(placeholder),
            None => {
                let find_box = if self.find_active {
                    self.render_find_box(colors, cx)
                } else {
                    None
                };
                // The terminal and the playback bar are separately focusable; the
                // focused one gets the shared arrow/home/end keys and a left accent.
                let terminal_focused = self.body_focus.is_focused(window);
                let playback_focused = self.playback_focus.is_focused(window);
                let controls = self.render_controls(colors, playback_focused, cx);
                // Unfocused, the accent takes the canvas it is drawn against, so
                // the grid keeps the same width either way.
                let terminal_accent = if terminal_focused {
                    colors.border_focused
                } else {
                    colors.background
                };
                chrome.child(controls).child(
                    div()
                        .key_context(GRID_KEY_CONTEXT)
                        .track_focus(&self.body_focus)
                        .relative()
                        .flex_1()
                        .overflow_hidden()
                        .border_l_2()
                        .border_color(terminal_accent)
                        .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                        .on_mouse_move(cx.listener(Self::on_mouse_move))
                        .when(self.hovered_url.is_some(), |body| body.cursor_pointer())
                        .font_family(bviz_ui_assets::MONO)
                        .text_size(px(13.))
                        .text_color(colors.text)
                        .child(TerminalElement { view: cx.entity() })
                        .children(find_box),
                )
            }
        }
    }
}

impl ConsolePanel {
    /// The find box overlaid at the top-right of the terminal: the query field,
    /// a match counter, and previous/next/close controls. `None` before the field
    /// has been created (i.e. the box has never been opened).
    fn render_find_box(
        &self,
        colors: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let input = self.find_input.as_ref()?.clone();
        let query_empty = input.read(cx).content().is_empty();
        let (cur, total) = self
            .terminal
            .as_ref()
            .map_or((None, 0), |t| (t.current_match(), t.search_count()));
        let count = if query_empty {
            String::new()
        } else if total == 0 {
            "No results".to_string()
        } else {
            format!("{} / {}", cur.unwrap_or(0), total)
        };

        let view = cx.entity();
        let prev_view = view.clone();
        let next_view = view.clone();
        let close_view = view;
        Some(
            div()
                .key_context(FIND_KEY_CONTEXT)
                .occlude()
                .absolute()
                .top_2()
                .right_4()
                .flex()
                .items_center()
                .gap_2()
                .p_1()
                .rounded_md()
                .bg(colors.panel)
                .border_1()
                .border_color(colors.border)
                .on_action(cx.listener(Self::find_next))
                .on_action(cx.listener(Self::find_prev))
                .on_action(cx.listener(Self::find_close))
                .child(div().w_48().child(input))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(colors.text)
                        .child(SharedString::from(count)),
                )
                .child(
                    Button::new("find-prev", move |_, _, cx| {
                        prev_view.update(cx, |this, cx| this.select_prev_match(cx))
                    })
                    .ghost()
                    .icon("caret-up"),
                )
                .child(
                    Button::new("find-next", move |_, _, cx| {
                        next_view.update(cx, |this, cx| this.select_next_match(cx))
                    })
                    .ghost()
                    .icon("caret-down"),
                )
                .child(
                    Button::new("find-close", move |_, window, cx| {
                        close_view.update(cx, |this, cx| this.find_close(&FindClose, window, cx))
                    })
                    .ghost()
                    .icon("x"),
                )
                .into_any_element(),
        )
    }

    /// The playback control bar above the terminal: jump-to-start, play/pause, and
    /// jump-to-end buttons, a draggable timeline scrubber, an elapsed/total
    /// readout, and a speed control. `focused` shows the focus accent and selects
    /// which region the shared arrow/home/end keys drive.
    fn render_controls(
        &self,
        colors: &ThemeColors,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let playing = self.playing;
        let (fraction, readout) = self.playback_readout(cx);

        let view = cx.entity();
        let start_view = view.clone();
        let play_view = view.clone();
        let end_view = view;

        div()
            .key_context(PLAYBACK_KEY_CONTEXT)
            .track_focus(&self.playback_focus)
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .h(px(40.))
            .bg(colors.surface)
            .border_b_1()
            .border_color(colors.border)
            .font_family(bviz_ui_assets::UI)
            .text_color(colors.text)
            // A click anywhere on the bar gives it focus, so the shared keys drive
            // playback.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.playback_focus.focus(window, cx);
                    cx.notify();
                }),
            )
            // A left accent marks the bar as the focused region.
            .when(focused, |bar| {
                bar.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(2.))
                        .bg(colors.border_focused),
                )
            })
            .child(
                Button::new("seek-start", move |_, _, cx| {
                    start_view.update(cx, |this, cx| this.seek_start(cx))
                })
                .ghost()
                .icon("skip-back"),
            )
            .child(
                Button::new("play-pause", move |_, _, cx| {
                    play_view.update(cx, |this, cx| this.play_pause(cx))
                })
                .ghost()
                .icon(if playing { "pause" } else { "play" }),
            )
            .child(
                Button::new("seek-end", move |_, _, cx| {
                    end_view.update(cx, |this, cx| this.seek_end(cx))
                })
                .ghost()
                .icon("skip-forward"),
            )
            .child(self.render_scrubber(fraction, colors, cx))
            .child(div().w(px(128.)).text_size(px(12.)).child(readout))
            .child(self.speed_dropdown.clone())
            .into_any_element()
    }

    /// The played fraction (0..=1) positioning the playhead, and the
    /// `current / total` readout text.
    fn playback_readout(&self, cx: &App) -> (f32, SharedString) {
        let (duration, real_time) = match self.replay(cx) {
            Some(replay) => (replay.duration(), replay.real_time()),
            None => (0.0, true),
        };
        // The playhead tracks the playback clock, so it glides smoothly even when
        // the shown frame jumps across a coarse timeline's large gaps.
        let current = self.position;
        let fraction = if duration > 0.0 {
            (current / duration).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        // Without --show_timestamps the playhead time is interpolated, so mark the
        // current time approximate; the total is still bracketed by the build's
        // start and finish events and needs no mark.
        let readout = if real_time {
            format!("{} / {}", fmt_time(current), fmt_time(duration))
        } else {
            format!("~{} / {}", fmt_time(current), fmt_time(duration))
        };
        (fraction, SharedString::from(readout))
    }

    /// The playback scrubber, drawn at `fraction` and driving a seek on press or
    /// drag.
    fn render_scrubber(
        &self,
        fraction: f32,
        colors: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // The played portion and the knob mark a position within the span the
        // track carries, so they take the text they are read among.
        let fills = ScrubberColors {
            track: colors.element_hover.into(),
            played: colors.text.into(),
            knob: colors.text.into(),
        };
        scrubber(
            "scrubber",
            fraction,
            fills,
            |this| &mut this.scrubber,
            cx.entity(),
            |this, fraction, cx| this.scrub_to(fraction, cx),
        )
    }
}

/// Format a number of seconds as `M:SS`.
fn fmt_time(seconds: f64) -> String {
    let whole = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", whole / 60, whole % 60)
}
