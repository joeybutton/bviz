//! A gpui view that renders a build's display [`Terminal`] as styled monospace
//! text: the visible viewport, reflowed to the window, one shaped line per row
//! with per-cell colors and background fills.

use std::rc::Rc;
use std::time::{Duration, Instant};

use bviz_terminal::{Cell, Flags, Replay, Scroll, SelectionType, Side, Terminal, Url};
use bviz_ui_components::{
    Axis, Button, Dropdown, Scrollbar, ScrubberLayout, TextInput, load_in_background, scrubber,
    spinner, track_drag,
};
use bviz_ui_shell::Chrome;
use gpui::{
    App, Bounds, ClipboardItem, Context, Element, Entity, FocusHandle,
    Focusable, Font, GlobalElementId, InspectorElementId, IntoElement, KeyBinding, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, PaintQuad, Pixels, Point, Render, Rgba,
    ScrollWheelEvent, ShapedLine, SharedString, Style, Subscription, Task, TextRun,
    Window, actions, div, fill, point, prelude::*, px, relative, size,
};

use crate::color::{CellStyle, ansi_palette, style_key};

actions!(
    bazel_terminal,
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

/// Lines moved per arrow-key press while the terminal is focused.
const SCROLL_STEP: i32 = 3;

/// Lines scrolled per frame while a selection drag is held outside the content.
const SELECTION_AUTOSCROLL_STEP: i32 = 1;

/// Width of the scroll bar gutter reserved on the right edge.
const SCROLLBAR_WIDTH: f32 = 8.0;

/// How long the scroll bar stays visible after the last scroll interaction.
const SCROLLBAR_HIDE: Duration = Duration::from_millis(1200);

/// Smallest the scroll bar thumb may shrink to.
const MIN_THUMB: f32 = 24.0;

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

/// The build's playback timeline, a load in progress, or the error that stopped
/// it.
enum TermSource {
    Loading,
    Loaded(Replay),
    Error(String),
}

/// The root view: the rendered build terminal inside the window chrome.
pub struct TerminalView {
    /// The playback timeline, loaded off the UI thread.
    source: TermSource,
    /// Holds the background load task so it isn't cancelled on drop.
    _load_task: Task<()>,
    /// The frame currently shown; an index into the loaded [`Replay`]'s frames.
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
    /// Whether the scrubber thumb is being dragged.
    scrubbing: bool,
    /// The painted scrubber track geometry, set each layout for hit-testing.
    scrubber: Rc<std::cell::Cell<Option<ScrubberLayout>>>,
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
    /// The chrome's handle, for the window controls and tab navigation.
    focus_handle: FocusHandle,
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

impl TerminalView {
    pub fn new(path: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::bind_keys(cx);
        // Tab cycles focus through the regions (handled by the shell's tab action
        // via window.focus_next), in this order: playback bar, terminal, then the
        // find field when it is open (its tab index is set on creation).
        let body_focus = cx.focus_handle().tab_index(1).tab_stop(true);
        body_focus.focus(window, cx);
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
        let mut view = Self {
            source: TermSource::Loading,
            _load_task: Task::ready(()),
            current_frame: 0,
            playing: false,
            speed: SPEEDS[0],
            speed_dropdown,
            _speed_observer: speed_observer,
            play_anchor: None,
            position: 0.0,
            scrubbing: false,
            scrubber: Rc::new(std::cell::Cell::new(None)),
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
            focus_handle: cx.focus_handle(),
            body_focus,
            playback_focus,
            find_active: false,
            find_input: None,
            find_observation: None,
        };
        // Show the bar briefly on launch so the scrollable extent is visible,
        // then let it auto-hide.
        view.reveal_scrollbar(cx);
        view.spawn_load(path, cx);
        view
    }

    fn bind_keys(cx: &mut Context<Self>) {
        cx.bind_keys([
            // Keys that don't collide with terminal scrolling work in either region,
            // but stay inert while the find field has focus so they don't hijack
            // typing and cursor movement there.
            KeyBinding::new("space", PlayPause, Some("BazelTerm && !Find")),
            KeyBinding::new("left", StepBack, Some("BazelTerm && !Find")),
            KeyBinding::new("right", StepForward, Some("BazelTerm && !Find")),
            KeyBinding::new("]", SpeedUp, Some("BazelTerm && !Find")),
            KeyBinding::new("[", SpeedDown, Some("BazelTerm && !Find")),
            // The arrows and home/end collide: they scroll the focused terminal but
            // drive playback when the playback bar holds focus.
            KeyBinding::new("up", ScrollLineUp, Some("Terminal && !Find")),
            KeyBinding::new("down", ScrollLineDown, Some("Terminal && !Find")),
            KeyBinding::new("home", ScrollToTop, Some("Terminal && !Find")),
            KeyBinding::new("end", ScrollToBottom, Some("Terminal && !Find")),
            KeyBinding::new("up", SpeedUp, Some("Playback")),
            KeyBinding::new("down", SpeedDown, Some("Playback")),
            KeyBinding::new("home", SeekStart, Some("Playback")),
            KeyBinding::new("end", SeekEnd, Some("Playback")),
            // Paging and the commands work regardless of which region has focus.
            KeyBinding::new("pageup", ScrollPageUp, Some("BazelTerm")),
            KeyBinding::new("pagedown", ScrollPageDown, Some("BazelTerm")),
            KeyBinding::new("cmd-c", Copy, Some("BazelTerm")),
            KeyBinding::new("ctrl-c", Copy, Some("BazelTerm")),
            KeyBinding::new("cmd-f", Find, Some("BazelTerm")),
            KeyBinding::new("ctrl-f", Find, Some("BazelTerm")),
            KeyBinding::new("enter", FindNext, Some("Find")),
            KeyBinding::new("shift-enter", FindPrev, Some("Find")),
            KeyBinding::new("escape", FindClose, Some("Find")),
        ]);
    }

    /// Read and reconstruct the build off the UI thread so a large log never
    /// blocks startup or the first frame.
    fn spawn_load(&mut self, path: String, cx: &mut Context<Self>) {
        self._load_task = load_in_background(
            cx,
            move || {
                let bytes = std::fs::read(&path).map_err(|e| format!("cannot read {path}: {e}"))?;
                let output = bviz_terminal::parse(&bytes)
                    .map_err(|e| format!("cannot parse build event stream: {e}"))?;
                Ok::<_, String>(bviz_terminal::Replay::from_output(output))
            },
            |view, result, _| {
                view.source = match result {
                    Ok(replay) => {
                        // Open on the completed build, the playhead at its end, ready
                        // to replay from the start.
                        view.current_frame = replay.frame_count().saturating_sub(1);
                        view.position = replay.duration();
                        TermSource::Loaded(replay)
                    }
                    Err(err) => TermSource::Error(err),
                };
            },
        );
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
        if let Some(bar) = self.scrollbar {
            if bar.track_hit(event.position) {
                self.begin_scrollbar_drag(bar, event.position, cx);
                return;
            }
        }
        self.begin_selection(event, window, cx);
    }

    /// Grabbing the thumb keeps the cursor at the same spot on it, while
    /// clicking the track jumps the thumb's center to the cursor.
    fn begin_scrollbar_drag(&mut self, bar: Scrollbar, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.drag_grab = Some(bar.grab(position));
        self.drag_to(position, cx);
    }

    /// A press on the content focuses it and begins a text selection.
    fn begin_selection(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
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
            terminal.begin_selection(selection, column, row, side);
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
        if let Some(bar) = self.scrollbar {
            if bar.track_hit(event.position) && !self.scrollbar_visible {
                self.reveal_scrollbar(cx);
            }
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
            terminal.extend_selection(column, row, side);
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
        let last = match &self.source {
            TermSource::Loaded(replay) => replay.frame_count().saturating_sub(1),
            _ => return,
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
    fn advance_playback(&mut self, window: &mut Window) {
        if !self.playing {
            return;
        }
        let Some((started, base)) = self.play_anchor else {
            return;
        };
        let target = base + started.elapsed().as_secs_f64() * self.speed as f64;
        let (frame, last, duration) = match &self.source {
            TermSource::Loaded(replay) => (
                replay.frame_at(target),
                replay.frame_count().saturating_sub(1),
                replay.duration(),
            ),
            _ => return,
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
        let (frame, time) = match &self.source {
            TermSource::Loaded(replay) => {
                let frame = frame.min(replay.frame_count().saturating_sub(1));
                (frame, replay.time_of(frame))
            }
            _ => return,
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
        if !self.scrubbing {
            self.scrubbing = true;
            self.playing = false;
            self.play_anchor = None;
        }
        let (frame, position) = match &self.source {
            TermSource::Loaded(replay) => {
                let position = fraction as f64 * replay.duration();
                (replay.frame_at(position), position)
            }
            _ => return,
        };
        self.current_frame = frame;
        self.position = position;
        cx.notify();
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Advance the timeline if playing, pacing playback by the frame loop.
        self.advance_playback(window);
        self.autoscroll_selection(window, cx);
        let colors = bviz_ui_theme::colors(window);
        let body = match &self.source {
            TermSource::Loaded(_) => {
                let find_box = if self.find_active {
                    self.render_find_box(&colors, cx)
                } else {
                    None
                };
                // The terminal and the playback bar are separately focusable; the
                // focused one gets the shared arrow/home/end keys and a left accent.
                let terminal_focused = self.body_focus.is_focused(window);
                let playback_focused = self.playback_focus.is_focused(window);
                let controls = self.render_controls(&colors, playback_focused, cx);
                let terminal_accent = if terminal_focused {
                    colors.selected
                } else {
                    colors.background
                };
                div()
                    .key_context("BazelTerm")
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
                    .size_full()
                    .bg(colors.background)
                    .child(controls)
                    .child(
                        div()
                            .key_context("Terminal")
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
                    .into_any_element()
            }
            TermSource::Loading => Self::loading_view(&colors).into_any_element(),
            TermSource::Error(msg) => Self::error_view(&colors, msg).into_any_element(),
        };
        Chrome::new(&self.focus_handle)
            .title("bazel_term")
            .child(body)
    }
}

impl TerminalView {
    /// A centered spinner over a "Loading…" caption, shown while the source loads.
    fn loading_view(colors: &bviz_ui_theme::Colors) -> impl IntoElement {
        div()
            .flex_1()
            .flex()
            .flex_col()
            .justify_center()
            .items_center()
            .gap_3()
            .child(spinner("spinner", px(36.), colors.text))
            .child(
                div()
                    .text_sm()
                    .text_color(colors.disabled)
                    .child("Loading…"),
            )
    }

    /// A centered, width-capped error message shown when the source fails to load.
    fn error_view(colors: &bviz_ui_theme::Colors, msg: &str) -> impl IntoElement {
        div()
            .flex_1()
            .flex()
            .flex_col()
            .justify_center()
            .items_center()
            .gap_4()
            .p_4()
            .child(
                div()
                    .max_w(px(400.))
                    .text_sm()
                    .text_color(colors.disabled)
                    .child(msg.to_string()),
            )
    }

    /// The find box overlaid at the top-right of the terminal: the query field,
    /// a match counter, and previous/next/close controls. `None` before the field
    /// has been created (i.e. the box has never been opened).
    fn render_find_box(
        &self,
        colors: &bviz_ui_theme::Colors,
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
                .key_context("Find")
                .occlude()
                .absolute()
                .top_2()
                .right_4()
                .flex()
                .items_center()
                .gap_2()
                .p_1()
                .rounded_md()
                .bg(colors.container)
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
        colors: &bviz_ui_theme::Colors,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let playing = self.playing;
        let (fraction, readout) = self.playback_readout();

        let view = cx.entity();
        let start_view = view.clone();
        let play_view = view.clone();
        let end_view = view;

        div()
            .key_context("Playback")
            .track_focus(&self.playback_focus)
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .h(px(40.))
            .bg(colors.container)
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
                        .bg(colors.selected),
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
            .child(
                div()
                    .w(px(128.))
                    .text_size(px(12.))
                    .child(readout),
            )
            .child(self.speed_dropdown.clone())
            .into_any_element()
    }

    /// The played fraction (0..=1) positioning the playhead, and the
    /// `current / total` readout text.
    fn playback_readout(&self) -> (f32, SharedString) {
        let (duration, real_time) = match &self.source {
            TermSource::Loaded(replay) => (replay.duration(), replay.real_time()),
            _ => (0.0, true),
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
        colors: &bviz_ui_theme::Colors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let track_color = bviz_ui_theme::mix(colors.text, colors.background, 0.6);
        scrubber(
            "scrubber",
            fraction,
            track_color,
            colors.text,
            colors.text,
            self.scrubber.clone(),
            cx.entity(),
            self.scrubbing,
            |this, fraction, cx| this.scrub_to(fraction, cx),
            |this, cx| {
                if this.scrubbing {
                    this.scrubbing = false;
                    cx.notify();
                }
            },
        )
    }
}

/// Custom element that sizes the terminal to its bounds, then shapes the visible
/// viewport and paints background fills and glyphs.
struct TerminalElement {
    view: Entity<TerminalView>,
}

/// Font metrics resolved once per layout from the inherited monospace text style.
struct Metrics {
    cell_width: Pixels,
    line_height: Pixels,
    font_size: Pixels,
    font: Font,
}

/// A shaped run of text and the origin it paints at. Runs are positioned per
/// grid column rather than as one line per row, so wide glyphs and the cells
/// after them stay column-aligned instead of drifting by the shaped advances.
struct PositionedLine {
    origin: Point<Pixels>,
    line: ShapedLine,
}

struct Prepaint {
    lines: Vec<PositionedLine>,
    backgrounds: Vec<PaintQuad>,
    /// Quads painted over the text, such as the hovered URL's underline.
    overlays: Vec<PaintQuad>,
    scrollbar: Option<PaintQuad>,
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl TerminalElement {
    /// Build the terminal at `cols`x`rows` for the played frame so alacritty
    /// wraps to the window, or reflow the existing one to that size. On rebuild,
    /// restore scroll, search, and selection so scrubbing keeps them; before the
    /// source loads there is nothing to build and the viewport reads empty.
    fn sync_terminal(&self, cols: usize, rows: usize, cx: &mut App) {
        self.view.update(cx, |view, cx| {
            let frame = view.current_frame;
            if view.terminal.is_none() || view.terminal_frame != Some(frame) {
                if let TermSource::Loaded(replay) = &view.source {
                    let source = replay.display_at(frame);
                    let mut terminal = Terminal::from_display(&source, cols, rows);
                    // Restore the user's remembered scroll. If the held line lies
                    // beyond the output at this frame, scroll_to_top_line clamps to
                    // the bottom without forgetting it, so scrubbing forward returns.
                    if view.follow_bottom {
                        terminal.scroll(Scroll::Bottom);
                    } else {
                        terminal.scroll_to_top_line(view.scroll_top);
                    }
                    // Carry an active search onto the rebuilt content, keeping the
                    // focused match rather than resetting it to the first.
                    if view.find_active {
                        if let Some(input) = view.find_input.as_ref() {
                            let query = input.read(cx).content().to_string();
                            let current = view
                                .terminal
                                .as_ref()
                                .and_then(Terminal::current_match_index);
                            terminal.restore_search(&query, current);
                        }
                    }
                    // Carry the text selection onto the rebuilt content. A live drag
                    // keeps its anchor on its text but lets the far end follow the
                    // cursor's cell, so it grows as playback scrolls under a held
                    // cursor; a settled selection just re-applies its range.
                    if let (true, Some(anchor), Some(cursor)) =
                        (view.selecting, view.selection_anchor, view.selection_drag)
                    {
                        if let Some(end) = view.cell_at(cursor) {
                            terminal.drag_select(anchor, end);
                        }
                    } else if let Some(span) =
                        view.terminal.as_ref().and_then(Terminal::selection_span)
                    {
                        terminal.restore_selection(span);
                    }
                    view.terminal = Some(terminal);
                    view.terminal_frame = Some(frame);
                }
            } else if let Some(terminal) = view.terminal.as_mut() {
                terminal.resize(cols, rows);
            }
        });
    }

    /// Size and place the scroll bar from how much is off-screen and how far the
    /// viewport is scrolled: the content is the whole scrollback plus the visible
    /// rows, and the offset is how far below the top the viewport sits
    /// (`history - offset`). The geometry is kept on the view for hit-testing even
    /// while hidden; the returned thumb quad is painted only while the bar is
    /// revealed or being dragged, with no track.
    fn scrollbar_quad(
        &self,
        bounds: Bounds<Pixels>,
        history: usize,
        offset: usize,
        rows: usize,
        colors: &bviz_ui_theme::Colors,
        cx: &mut App,
    ) -> Option<PaintQuad> {
        let scrollbar_layout = Scrollbar::new(
            Axis::Vertical,
            Bounds::new(
                point(
                    px(f32::from(bounds.right()) - SCROLLBAR_WIDTH),
                    bounds.top(),
                ),
                size(px(SCROLLBAR_WIDTH), bounds.size.height),
            ),
            (history + rows) as f32,
            rows as f32,
            (history - offset) as f32,
            MIN_THUMB,
        );
        self.view
            .update(cx, |view, _| view.scrollbar = scrollbar_layout);
        let view = self.view.read(cx);
        let show_scrollbar = view.scrollbar_visible || view.drag_grab.is_some();
        scrollbar_layout.filter(|_| show_scrollbar).map(|bar| {
            fill(
                bar.thumb(),
                bviz_ui_theme::mix(colors.text, colors.background, 0.6),
            )
        })
    }

    /// Shape the visible grid into one [`PositionedLine`] per contiguous
    /// same-style span and collect the non-default cell background fills.
    fn shape_viewport(
        viewport: &[Vec<Cell>],
        bounds: Bounds<Pixels>,
        metrics: &Metrics,
        fg: Rgba,
        bg: Rgba,
        ansi: &[u32; 16],
        window: &mut Window,
    ) -> (Vec<PositionedLine>, Vec<PaintQuad>) {
        let mut lines = Vec::with_capacity(viewport.len());
        let mut backgrounds = Vec::new();

        for (r, row) in viewport.iter().enumerate() {
            let y = bounds.top() + metrics.line_height * r as f32;

            let mut col = 0;
            while col < row.len() {
                let start = col;
                let key = style_key(&row[col]);
                // A wide character occupies two columns: its glyph cell carries
                // the character, the following spacer cell carries none.
                let mut run_text = String::new();
                while col < row.len() && style_key(&row[col]) == key {
                    if !row[col].flags.contains(Flags::WIDE_CHAR_SPACER) {
                        run_text.push(row[col].c);
                    }
                    col += 1;
                }
                let style = CellStyle::resolve(&row[start], fg, bg, ansi);
                let x = bounds.left() + metrics.cell_width * start as f32;

                if style.background != bg {
                    let width = metrics.cell_width * (col - start) as f32;
                    backgrounds.push(fill(
                        Bounds::new(point(x, y), size(width, metrics.line_height)),
                        style.background,
                    ));
                }

                if run_text.is_empty() {
                    continue;
                }
                let mut font = metrics.font.clone();
                font.weight = style.weight;
                font.style = style.font_style;
                let run = TextRun {
                    len: run_text.len(),
                    font,
                    color: style.foreground.into(),
                    background_color: None,
                    underline: style.underline,
                    strikethrough: style.strikethrough,
                };
                lines.push(PositionedLine {
                    origin: point(x, y),
                    line: window.text_system().shape_line(
                        run_text.into(),
                        metrics.font_size,
                        std::slice::from_ref(&run),
                        None,
                    ),
                });
            }
        }
        (lines, backgrounds)
    }

    /// Translucent tint over the selected cells; painted under the text so it
    /// stays legible.
    /// Full-cell fill covering the inclusive column span `first..=last` on `row`.
    fn span_fill(
        bounds: Bounds<Pixels>,
        metrics: &Metrics,
        row: usize,
        first: usize,
        last: usize,
        color: Rgba,
    ) -> PaintQuad {
        let x = bounds.left() + metrics.cell_width * first as f32;
        let y = bounds.top() + metrics.line_height * row as f32;
        let width = metrics.cell_width * (last - first + 1) as f32;
        fill(
            Bounds::new(point(x, y), size(width, metrics.line_height)),
            color,
        )
    }

    fn selection_quads(
        selection: &[Option<(usize, usize)>],
        bounds: Bounds<Pixels>,
        metrics: &Metrics,
        selected: Rgba,
    ) -> Vec<PaintQuad> {
        let selection_color = Rgba { a: 0.4, ..selected };
        let mut quads = Vec::new();
        for (r, span) in selection.iter().enumerate() {
            let Some((first, last)) = span else { continue };
            quads.push(Self::span_fill(bounds, metrics, r, *first, *last, selection_color));
        }
        quads
    }

    /// Yellow tint over search matches, the focused match more strongly.
    fn search_quads(
        search: &[Vec<(usize, usize, bool)>],
        bounds: Bounds<Pixels>,
        metrics: &Metrics,
    ) -> Vec<PaintQuad> {
        let mut quads = Vec::new();
        for (r, spans) in search.iter().enumerate() {
            for &(first, last, focused) in spans {
                let color = if focused {
                    Rgba {
                        r: 0.85,
                        g: 0.72,
                        b: 0.18,
                        a: 0.60,
                    }
                } else {
                    Rgba {
                        r: 0.85,
                        g: 0.72,
                        b: 0.18,
                        a: 0.30,
                    }
                };
                quads.push(Self::span_fill(bounds, metrics, r, first, last, color));
            }
        }
        quads
    }

    /// Underline the hovered URL in the accent color to mark it as a link.
    fn url_overlay(
        &self,
        bounds: Bounds<Pixels>,
        metrics: &Metrics,
        selected: Rgba,
        cx: &App,
    ) -> Vec<PaintQuad> {
        let mut overlays = Vec::new();
        if let Some(url) = self.view.read(cx).hovered_url.as_ref() {
            for (r, span) in url.spans().iter().enumerate() {
                let Some((first, last)) = span else { continue };
                let x = bounds.left() + metrics.cell_width * *first as f32;
                let y = bounds.top() + metrics.line_height * (r as f32 + 1.0) - px(2.0);
                let width = metrics.cell_width * (*last - *first + 1) as f32;
                overlays.push(fill(
                    Bounds::new(point(x, y), size(width, px(1.5))),
                    selected,
                ));
            }
        }
        overlays
    }

    /// While dragging the thumb, follow and end the drag with window-global
    /// handlers so it keeps working when the cursor leaves the window.
    fn install_thumb_drag(&self, window: &mut Window, cx: &App) {
        if self.view.read(cx).drag_grab.is_some() {
            track_drag(
                window,
                &self.view,
                |view, event, cx| view.drag_to(event.position, cx),
                |view, _, cx| {
                    if view.drag_grab.take().is_some() {
                        // Keep the bar up briefly after the drag, then hide.
                        view.reveal_scrollbar(cx);
                    }
                },
            );
        }
    }

    /// Follow and end a text-selection drag with window-global handlers, so
    /// dragging past the window edge keeps extending the selection and a release
    /// outside still ends it.
    fn install_selection_drag(&self, window: &mut Window, cx: &App) {
        if self.view.read(cx).selecting {
            track_drag(
                window,
                &self.view,
                |view, event, cx| view.selection_drag_to(event.position, cx),
                |view, _, cx| {
                    if view.selecting {
                        view.selecting = false;
                        view.selection_drag = None;
                        view.selection_anchor = None;
                        // A press that did not drag (no selection text) and
                        // landed on a URL opens it.
                        if let Some(url) = view.pressed_url.take() {
                            let dragged = view
                                .terminal
                                .as_ref()
                                .and_then(Terminal::selection_text)
                                .is_some();
                            if !dragged {
                                cx.open_url(&url);
                            }
                        }
                        cx.notify();
                    }
                },
            );
        }
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = Metrics;
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Metrics) {
        let metrics = bviz_ui_assets::text_metrics(window);
        // A monospace advance width, measured from a single glyph.
        let color = window.text_style().color;
        let cell_width =
            bviz_ui_assets::shape_line(window, "M", metrics.font.clone(), metrics.font_size, color)
                .width;

        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();

        (
            window.request_layout(style, [], cx),
            Metrics {
                cell_width,
                line_height: metrics.line_height,
                font_size: metrics.font_size,
                font: metrics.font,
            },
        )
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        metrics: &mut Metrics,
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let colors = bviz_ui_theme::colors(window);
        let fg = colors.text;
        let bg = colors.background;
        let ansi = ansi_palette(window);

        // Columns fit the width less the scroll bar gutter; rows fit the height.
        let cols = ((f32::from(bounds.size.width) - SCROLLBAR_WIDTH)
            / f32::from(metrics.cell_width))
        .floor()
        .max(1.0) as usize;
        let rows = (f32::from(bounds.size.height) / f32::from(metrics.line_height))
            .floor()
            .max(1.0) as usize;
        self.sync_terminal(cols, rows, cx);

        let (viewport, history, offset, selection, search) =
            match self.view.read(cx).terminal.as_ref() {
                Some(terminal) => (
                    terminal.viewport(),
                    terminal.history_size(),
                    terminal.display_offset(),
                    terminal.selection_spans(),
                    terminal.search_spans(),
                ),
                None => (Vec::new(), 0, 0, Vec::new(), Vec::new()),
            };
        self.view.update(cx, |view, _| {
            view.content = Some(ContentLayout {
                left: f32::from(bounds.left()),
                top: f32::from(bounds.top()),
                cell_width: f32::from(metrics.cell_width),
                line_height: f32::from(metrics.line_height),
                columns: cols,
                rows,
            });
        });

        let scrollbar = self.scrollbar_quad(bounds, history, offset, rows, &colors, cx);

        let (lines, mut backgrounds) =
            Self::shape_viewport(&viewport, bounds, metrics, fg, bg, ansi, window);
        backgrounds.extend(Self::selection_quads(&selection, bounds, metrics, colors.selected));
        backgrounds.extend(Self::search_quads(&search, bounds, metrics));
        let overlays = self.url_overlay(bounds, metrics, colors.selected, cx);

        Prepaint {
            lines,
            backgrounds,
            overlays,
            scrollbar,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        metrics: &mut Metrics,
        prepaint: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        for quad in prepaint.backgrounds.drain(..) {
            window.paint_quad(quad);
        }
        for positioned in prepaint.lines.iter() {
            positioned
                .line
                .paint(
                    positioned.origin,
                    metrics.line_height,
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                )
                .unwrap();
        }
        for quad in prepaint.overlays.drain(..) {
            window.paint_quad(quad);
        }
        if let Some(scrollbar) = prepaint.scrollbar.take() {
            window.paint_quad(scrollbar);
        }

        self.install_thumb_drag(window, cx);
        self.install_selection_drag(window, cx);
    }
}

/// Format a number of seconds as `M:SS`.
fn fmt_time(seconds: f64) -> String {
    let whole = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", whole / 60, whole % 60)
}

