//! A gpui view that renders a build's display [`Terminal`] as styled monospace
//! text: the visible viewport, reflowed to the window, one shaped line per row
//! with per-cell colors and background fills.

use std::time::{Duration, Instant};

use bviz_terminal::{
    Cell, Color, Flags, NamedColor, Replay, Rgb, Scroll, SelectionType, Side, Terminal, Url,
};
use bviz_ui_components::{load_in_background, track_drag, Button, TextInput};
use bviz_ui_shell::Chrome;
use gpui::{
    actions, canvas, div, fill, percentage, point, prelude::*, px, relative, rgb, size, Animation,
    AnimationExt, App, Bounds, ClipboardItem, Context, Element, Entity, FocusHandle, Focusable,
    Font, FontStyle, FontWeight, GlobalElementId, InspectorElementId, IntoElement, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, PaintQuad, Pixels, Point, Render, Rgba,
    ScrollWheelEvent, ShapedLine, SharedString, StrikethroughStyle, Style, Subscription, Task,
    TextRun, Transformation, UnderlineStyle, Window, WindowAppearance,
};

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
        ScrollPageUp,
        ScrollPageDown,
        Copy,
        Find,
        FindNext,
        FindPrev,
        FindClose
    ]
);

/// Width of the scroll bar gutter reserved on the right edge.
const SCROLLBAR_WIDTH: f32 = 8.0;

/// How long the scroll bar stays visible after the last scroll interaction.
const SCROLLBAR_HIDE: Duration = Duration::from_millis(1200);

/// Smallest the scroll bar thumb may shrink to.
const MIN_THUMB: f32 = 24.0;

/// Playback speed multipliers the speed control cycles through.
const SPEEDS: [f32; 4] = [1.0, 2.0, 4.0, 8.0];

/// Geometry of the painted scroll bar, in window pixels, kept so mouse events can
/// hit-test the thumb and map a drag back to a scroll position.
#[derive(Clone, Copy)]
struct ScrollbarLayout {
    /// Left edge of the gutter; clicks left of this are terminal content.
    gutter_left: f32,
    track_top: f32,
    track_height: f32,
    thumb_top: f32,
    thumb_height: f32,
}

/// Geometry of the painted playback scrubber track, in window pixels, kept so a
/// click or drag on it maps to a position in the timeline.
#[derive(Clone, Copy)]
struct ScrubberLayout {
    left: f32,
    width: f32,
}

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
    scrubber: Option<ScrubberLayout>,
    /// The display terminal, built at the window size for [`current_frame`] and
    /// rebuilt when the frame changes.
    terminal: Option<Terminal>,
    /// The frame [`terminal`] was built for, to detect when it must be rebuilt.
    terminal_frame: Option<usize>,
    /// The painted scroll bar geometry, set each layout for hit-testing.
    scrollbar: Option<ScrollbarLayout>,
    /// The painted content geometry, set each layout for mapping clicks to cells.
    content: Option<ContentLayout>,
    /// Whether a click-drag text selection is in progress.
    selecting: bool,
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
    /// Whether the find box is open.
    find_active: bool,
    /// The find box's text field, created on first open and reused.
    find_input: Option<Entity<TextInput>>,
    /// Keeps the subscription that re-runs the search on every find-field edit.
    find_observation: Option<Subscription>,
}

impl TerminalView {
    pub fn new(path: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.bind_keys([
            // Playback keys are inert while the find field has focus, so they don't
            // hijack typing and cursor movement there.
            KeyBinding::new("space", PlayPause, Some("Terminal && !Find")),
            KeyBinding::new("left", StepBack, Some("Terminal && !Find")),
            KeyBinding::new("right", StepForward, Some("Terminal && !Find")),
            KeyBinding::new("home", SeekStart, Some("Terminal && !Find")),
            KeyBinding::new("end", SeekEnd, Some("Terminal && !Find")),
            KeyBinding::new("up", SpeedUp, Some("Terminal && !Find")),
            KeyBinding::new("down", SpeedDown, Some("Terminal && !Find")),
            KeyBinding::new("]", SpeedUp, Some("Terminal && !Find")),
            KeyBinding::new("[", SpeedDown, Some("Terminal && !Find")),
            KeyBinding::new("pageup", ScrollPageUp, Some("Terminal")),
            KeyBinding::new("pagedown", ScrollPageDown, Some("Terminal")),
            KeyBinding::new("cmd-c", Copy, Some("Terminal")),
            KeyBinding::new("ctrl-c", Copy, Some("Terminal")),
            KeyBinding::new("cmd-f", Find, Some("Terminal")),
            KeyBinding::new("ctrl-f", Find, Some("Terminal")),
            KeyBinding::new("cmd-f", Find, Some("Find")),
            KeyBinding::new("ctrl-f", Find, Some("Find")),
            KeyBinding::new("enter", FindNext, Some("Find")),
            KeyBinding::new("shift-enter", FindPrev, Some("Find")),
            KeyBinding::new("escape", FindClose, Some("Find")),
        ]);
        let body_focus = cx.focus_handle();
        body_focus.focus(window, cx);
        let mut view = Self {
            source: TermSource::Loading,
            _load_task: Task::ready(()),
            current_frame: 0,
            playing: false,
            speed: SPEEDS[0],
            play_anchor: None,
            position: 0.0,
            scrubbing: false,
            scrubber: None,
            terminal: None,
            terminal_frame: None,
            scrollbar: None,
            content: None,
            selecting: false,
            hovered_url: None,
            pressed_url: None,
            drag_grab: None,
            scrollbar_visible: false,
            reveal_seq: 0,
            focus_handle: cx.focus_handle(),
            body_focus,
            find_active: false,
            find_input: None,
            find_observation: None,
        };
        // Show the bar briefly on launch so the scrollable extent is visible,
        // then let it auto-hide.
        view.reveal_scrollbar(cx);
        // Read and reconstruct the build off the UI thread so a large log never
        // blocks startup or the first frame.
        view._load_task = load_in_background(
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
        view
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
        if let Some(terminal) = self.terminal.as_mut() {
            terminal.scroll(scroll);
            // The hovered URL's highlight is in viewport coordinates, which
            // scrolling invalidates; the next pointer move recomputes it.
            self.hovered_url = None;
            self.reveal_scrollbar(cx);
        }
    }

    /// Scroll so the thumb's top sits under the cursor, accounting for the grab
    /// offset, and translate that to a line via the terminal.
    fn drag_to(&mut self, mouse_y: f32, cx: &mut Context<Self>) {
        let (Some(layout), Some(grab)) = (self.scrollbar, self.drag_grab) else {
            return;
        };
        let span = (layout.track_height - layout.thumb_height).max(1.0);
        let thumb_top = (mouse_y - grab).clamp(layout.track_top, layout.track_top + span);
        // Fraction down the track: 0 is the first line, 1 is the last.
        let fraction = (thumb_top - layout.track_top) / span;
        if let Some(terminal) = self.terminal.as_mut() {
            terminal.scroll_to_fraction(fraction);
            cx.notify();
        }
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
        let y = f32::from(event.position.y);
        // A click on the scroll-bar gutter drives the thumb; grabbing the thumb
        // keeps the cursor at the same spot on it, while clicking the track jumps
        // the thumb's center to the cursor.
        if let Some(layout) = self.scrollbar {
            if f32::from(event.position.x) >= layout.gutter_left {
                let on_thumb = y >= layout.thumb_top && y <= layout.thumb_top + layout.thumb_height;
                self.drag_grab = Some(if on_thumb {
                    y - layout.thumb_top
                } else {
                    layout.thumb_height / 2.0
                });
                self.drag_to(y, cx);
                return;
            }
        }
        // A click on the content begins a text selection.
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
            self.selecting = true;
            cx.notify();
        }
    }

    /// Track the URL under the cursor so it can be highlighted and opened.
    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            return;
        }
        // Moving the pointer onto the gutter reveals the hidden scroll bar.
        if let Some(layout) = self.scrollbar {
            if f32::from(event.position.x) >= layout.gutter_left && !self.scrollbar_visible {
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
        let Some((column, row, side)) = self.cell_at(position) else {
            return;
        };
        if let Some(terminal) = self.terminal.as_mut() {
            terminal.extend_selection(column, row, side);
            cx.notify();
        }
    }

    fn scroll_page_up(&mut self, _: &ScrollPageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll(Scroll::PageUp, cx);
    }

    fn scroll_page_down(&mut self, _: &ScrollPageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll(Scroll::PageDown, cx);
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
            let input = cx.new(|cx| TextInput::new(cx, "Find", 0));
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
        // The hovered URL's highlight is in viewport coordinates, which scrolling
        // to a match invalidates; the next pointer move recomputes it.
        self.hovered_url = None;
        cx.notify();
    }

    fn find_next(&mut self, _: &FindNext, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(terminal) = self.terminal.as_mut() {
            terminal.search_next();
        }
        cx.notify();
    }

    fn find_prev(&mut self, _: &FindPrev, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(terminal) = self.terminal.as_mut() {
            terminal.search_prev();
        }
        cx.notify();
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

    /// Move `delta` steps through the playback speeds, wrapping around.
    fn shift_speed(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = SPEEDS.len() as isize;
        let current = SPEEDS.iter().position(|&s| s == self.speed).unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(count) as usize;
        self.speed = SPEEDS[next];
        if self.playing {
            self.anchor_clock();
        }
        cx.notify();
    }

    /// Begin a scrubber drag: pause and jump to the pressed position.
    fn scrub_begin(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.scrubbing = true;
        self.playing = false;
        self.play_anchor = None;
        self.scrub_to_x(f32::from(event.position.x), cx);
    }

    /// Seek to the timeline position under a window x-coordinate on the scrubber.
    fn scrub_to_x(&mut self, x: f32, cx: &mut Context<Self>) {
        let Some(layout) = self.scrubber else {
            return;
        };
        let fraction = ((x - layout.left) / layout.width.max(1.0)).clamp(0.0, 1.0);
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
        let colors = bviz_ui_theme::colors(window);
        let body = match &self.source {
            TermSource::Loaded(_) => {
                let find_box = if self.find_active {
                    self.render_find_box(&colors, cx)
                } else {
                    None
                };
                let controls = self.render_controls(&colors, cx);
                div()
                    .key_context("Terminal")
                    .track_focus(&self.body_focus)
                    .on_action(cx.listener(Self::on_play_pause))
                    .on_action(cx.listener(Self::on_step_back))
                    .on_action(cx.listener(Self::on_step_forward))
                    .on_action(cx.listener(Self::on_seek_start))
                    .on_action(cx.listener(Self::on_seek_end))
                    .on_action(cx.listener(Self::on_speed_up))
                    .on_action(cx.listener(Self::on_speed_down))
                    .on_action(cx.listener(Self::scroll_page_up))
                    .on_action(cx.listener(Self::scroll_page_down))
                    .on_action(cx.listener(Self::copy))
                    .on_action(cx.listener(Self::find))
                    .flex()
                    .flex_col()
                    .size_full()
                    .bg(colors.background)
                    .child(controls)
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .overflow_hidden()
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
            TermSource::Loading => div()
                .flex_1()
                .flex()
                .flex_col()
                .justify_center()
                .items_center()
                .gap_3()
                .child(
                    bviz_ui_assets::icon("circle-notch")
                        .size(px(36.))
                        .text_color(colors.text)
                        .with_animation(
                            "spinner",
                            Animation::new(Duration::from_millis(900)).repeat(),
                            |icon, delta| {
                                icon.with_transformation(Transformation::rotate(percentage(delta)))
                            },
                        ),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(colors.disabled)
                        .child("Loading…"),
                )
                .into_any_element(),
            TermSource::Error(msg) => div()
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
                        .child(msg.clone()),
                )
                .into_any_element(),
        };
        Chrome::new(&self.focus_handle)
            .title("bazel_term")
            .child(body)
    }
}

impl TerminalView {
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
                        prev_view.update(cx, |this, cx| {
                            if let Some(terminal) = this.terminal.as_mut() {
                                terminal.search_prev();
                            }
                            cx.notify();
                        })
                    })
                    .ghost()
                    .icon("caret-up"),
                )
                .child(
                    Button::new("find-next", move |_, _, cx| {
                        next_view.update(cx, |this, cx| {
                            if let Some(terminal) = this.terminal.as_mut() {
                                terminal.search_next();
                            }
                            cx.notify();
                        })
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

    /// The playback control bar below the terminal: jump-to-start, play/pause, and
    /// jump-to-end buttons, a draggable timeline scrubber, an elapsed/total
    /// readout, and a speed control.
    fn render_controls(
        &self,
        colors: &bviz_ui_theme::Colors,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
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
        let playing = self.playing;

        let view = cx.entity();
        let start_view = view.clone();
        let play_view = view.clone();
        let end_view = view.clone();
        let speed_view = view.clone();
        let bounds_view = view.clone();
        let drag_view = view;

        let track_color = bviz_ui_theme::mix(colors.text, colors.background, 0.6);
        let played_color = colors.text;
        let knob_color = colors.text;

        // An interpolated timeline (captured without --show_timestamps) is marked
        // approximate.
        let readout = if real_time {
            format!("{} / {}", fmt_time(current), fmt_time(duration))
        } else {
            format!("{} / ~{}", fmt_time(current), fmt_time(duration))
        };

        div()
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
            .child(
                div()
                    .id("scrubber")
                    .flex_1()
                    .h_full()
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::scrub_begin))
                    .child(
                        canvas(
                            move |bounds, _, cx| {
                                bounds_view.update(cx, |this, _| {
                                    this.scrubber = Some(ScrubberLayout {
                                        left: f32::from(bounds.origin.x),
                                        width: f32::from(bounds.size.width),
                                    });
                                });
                            },
                            move |bounds, _, window, cx| {
                                paint_scrubber(
                                    bounds,
                                    fraction,
                                    track_color,
                                    played_color,
                                    knob_color,
                                    window,
                                );
                                // Keep a scrub drag live past the window edge.
                                if drag_view.read(cx).scrubbing {
                                    track_drag(
                                        window,
                                        &drag_view,
                                        |this, event, cx| {
                                            this.scrub_to_x(f32::from(event.position.x), cx)
                                        },
                                        |this, _, cx| {
                                            if this.scrubbing {
                                                this.scrubbing = false;
                                                cx.notify();
                                            }
                                        },
                                    );
                                }
                            },
                        )
                        .size_full(),
                    ),
            )
            .child(
                div()
                    .w(px(128.))
                    .text_size(px(12.))
                    .child(SharedString::from(readout)),
            )
            .child(
                Button::new("speed", move |_, _, cx| {
                    speed_view.update(cx, |this, cx| this.shift_speed(1, cx))
                })
                .ghost()
                .label(format!("{}×", self.speed as i32)),
            )
            .into_any_element()
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

        // Create the terminal at the bounds (minus the scroll bar gutter), or
        // resize it to them, so alacritty wraps to the window; then read the rows
        // it would currently show and its scroll position.
        let cols = ((f32::from(bounds.size.width) - SCROLLBAR_WIDTH)
            / f32::from(metrics.cell_width))
        .floor()
        .max(1.0) as usize;
        let rows = (f32::from(bounds.size.height) / f32::from(metrics.line_height))
            .floor()
            .max(1.0) as usize;
        self.view.update(cx, |view, _| {
            let frame = view.current_frame;
            // Rebuild the terminal when the played frame changes, reconstructing the
            // build's output up to that point; otherwise just reflow to the bounds.
            // Before the source loads there is nothing to build and the viewport
            // read below sees `None` and renders empty.
            if view.terminal.is_none() || view.terminal_frame != Some(frame) {
                if let TermSource::Loaded(replay) = &view.source {
                    let source = replay.display_at(frame);
                    let mut terminal = Terminal::from_display(&source, cols, rows);
                    // Follow the newest output, as the build's terminal did.
                    terminal.scroll(Scroll::Bottom);
                    view.terminal = Some(terminal);
                    view.terminal_frame = Some(frame);
                }
            } else if let Some(terminal) = view.terminal.as_mut() {
                terminal.resize(cols, rows);
            }
        });
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

        // The scroll bar thumb, sized and placed by how much is off-screen and how
        // far the viewport is scrolled. Its geometry is kept on the view so mouse
        // events can hit-test and drag it.
        let scrollbar_layout = (history > 0).then(|| {
            let total = (history + rows) as f32;
            let height = f32::from(bounds.size.height);
            let thumb_height = (height * rows as f32 / total).max(MIN_THUMB);
            let thumb_top = (f32::from(bounds.top()) + height * (history - offset) as f32 / total)
                .min(f32::from(bounds.bottom()) - thumb_height);
            ScrollbarLayout {
                gutter_left: f32::from(bounds.right()) - SCROLLBAR_WIDTH,
                track_top: f32::from(bounds.top()),
                track_height: height,
                thumb_top,
                thumb_height,
            }
        });
        self.view
            .update(cx, |view, _| view.scrollbar = scrollbar_layout);
        // The geometry is kept for hit-testing even while hidden; the thumb is
        // painted only while the bar is revealed or being dragged.
        let view = self.view.read(cx);
        let show_scrollbar = view.scrollbar_visible || view.drag_grab.is_some();
        let scrollbar = scrollbar_layout.filter(|_| show_scrollbar).map(|layout| {
            fill(
                Bounds::new(
                    point(px(layout.gutter_left), px(layout.thumb_top)),
                    size(px(SCROLLBAR_WIDTH), px(layout.thumb_height)),
                ),
                bviz_ui_theme::mix(colors.text, colors.background, 0.6),
            )
        });

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

        // The selection highlight tints the selected cells; a translucent fill
        // over the cell backgrounds keeps the text on top legible.
        let selection_color = Rgba {
            a: 0.4,
            ..colors.selected
        };
        for (r, span) in selection.iter().enumerate() {
            let Some((first, last)) = span else { continue };
            let x = bounds.left() + metrics.cell_width * *first as f32;
            let y = bounds.top() + metrics.line_height * r as f32;
            let width = metrics.cell_width * (*last - *first + 1) as f32;
            backgrounds.push(fill(
                Bounds::new(point(x, y), size(width, metrics.line_height)),
                selection_color,
            ));
        }

        // Highlight search matches in yellow, the focused match more strongly.
        for (r, spans) in search.iter().enumerate() {
            let y = bounds.top() + metrics.line_height * r as f32;
            for &(first, last, focused) in spans {
                let x = bounds.left() + metrics.cell_width * first as f32;
                let width = metrics.cell_width * (last - first + 1) as f32;
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
                backgrounds.push(fill(
                    Bounds::new(point(x, y), size(width, metrics.line_height)),
                    color,
                ));
            }
        }

        // Underline the hovered URL in the accent color to mark it as a link.
        let mut overlays = Vec::new();
        if let Some(url) = self.view.read(cx).hovered_url.as_ref() {
            for (r, span) in url.spans().iter().enumerate() {
                let Some((first, last)) = span else { continue };
                let x = bounds.left() + metrics.cell_width * *first as f32;
                let y = bounds.top() + metrics.line_height * (r as f32 + 1.0) - px(2.0);
                let width = metrics.cell_width * (*last - *first + 1) as f32;
                overlays.push(fill(
                    Bounds::new(point(x, y), size(width, px(1.5))),
                    colors.selected,
                ));
            }
        }

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

        // While dragging the thumb, follow and end the drag with window-global
        // handlers so it keeps working when the cursor leaves the window.
        if self.view.read(cx).drag_grab.is_some() {
            track_drag(
                window,
                &self.view,
                |view, event, cx| view.drag_to(f32::from(event.position.y), cx),
                |view, _, cx| {
                    if view.drag_grab.take().is_some() {
                        // Keep the bar up briefly after the drag, then hide.
                        view.reveal_scrollbar(cx);
                    }
                },
            );
        }

        // Likewise follow and end a text-selection drag, so dragging past the
        // window edge keeps extending the selection and a release outside still
        // ends it.
        if self.view.read(cx).selecting {
            track_drag(
                window,
                &self.view,
                |view, event, cx| view.selection_drag_to(event.position, cx),
                |view, _, cx| {
                    if view.selecting {
                        view.selecting = false;
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

/// Cells are grouped into shaped runs by identical color and styling.
fn style_key(cell: &Cell) -> (Color, Color, Flags) {
    (cell.fg, cell.bg, cell.flags)
}

/// A cell's appearance resolved to concrete colors and font attributes.
struct CellStyle {
    foreground: Rgba,
    background: Rgba,
    weight: FontWeight,
    font_style: FontStyle,
    underline: Option<UnderlineStyle>,
    strikethrough: Option<StrikethroughStyle>,
}

impl CellStyle {
    fn resolve(cell: &Cell, fg: Rgba, bg: Rgba, ansi: &[u32; 16]) -> CellStyle {
        let mut foreground = to_rgba(cell.fg, fg, bg, ansi);
        let mut background = to_rgba(cell.bg, fg, bg, ansi);
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut foreground, &mut background);
        }
        if cell.flags.contains(Flags::DIM) {
            foreground = bviz_ui_theme::mix(foreground, background, 0.45);
        }
        if cell.flags.contains(Flags::HIDDEN) {
            foreground = background;
        }
        // gpui can render solid and wavy underlines; map undercurl to wavy and
        // the other variants (double, dotted, dashed) to a plain underline.
        let underline = cell
            .flags
            .intersects(Flags::ALL_UNDERLINES)
            .then_some(UnderlineStyle {
                color: Some(foreground.into()),
                thickness: px(1.0),
                wavy: cell.flags.contains(Flags::UNDERCURL),
            });
        let strikethrough = cell
            .flags
            .contains(Flags::STRIKEOUT)
            .then_some(StrikethroughStyle {
                color: Some(foreground.into()),
                thickness: px(1.0),
            });
        CellStyle {
            foreground,
            background,
            weight: if cell.flags.contains(Flags::BOLD) {
                FontWeight::BOLD
            } else {
                FontWeight::NORMAL
            },
            font_style: if cell.flags.contains(Flags::ITALIC) {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
            underline,
            strikethrough,
        }
    }
}

// gpui's base palette has no ANSI colors, so the 16 named colors come from an
// established terminal theme — Zed's One — chosen to stay legible on gpui's
// default dark and light backgrounds. The default foreground and background
// still follow the gpui theme (see [`to_rgba`]).
const ONE_DARK: [u32; 16] = [
    0x282c34, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xabb2bf, 0x636d83,
    0xea858b, 0xaad581, 0xffd885, 0x85c1ff, 0xd398eb, 0x6ed5de, 0xfafafa,
];
const ONE_LIGHT: [u32; 16] = [
    0x000000, 0xde3e35, 0x3f953a, 0xd2b67c, 0x2f5af3, 0x950095, 0x0997b3, 0xbbbbbb, 0x000000,
    0xde3e35, 0x3f953a, 0xd2b67c, 0x2f5af3, 0xa00095, 0x0bbcd6, 0xffffff,
];

/// The ANSI palette for the window's current light/dark appearance.
fn ansi_palette(window: &Window) -> &'static [u32; 16] {
    match window.appearance() {
        WindowAppearance::Light | WindowAppearance::VibrantLight => &ONE_LIGHT,
        WindowAppearance::Dark | WindowAppearance::VibrantDark => &ONE_DARK,
    }
}

fn rgb_to_rgba(c: Rgb) -> Rgba {
    Rgba {
        r: c.r as f32 / 255.0,
        g: c.g as f32 / 255.0,
        b: c.b as f32 / 255.0,
        a: 1.0,
    }
}

/// Resolve an alacritty [`Color`] against the terminal's default fore/background
/// and the appearance-selected ANSI palette.
fn to_rgba(color: Color, fg: Rgba, bg: Rgba, ansi: &[u32; 16]) -> Rgba {
    match color {
        Color::Spec(c) => rgb_to_rgba(c),
        Color::Indexed(i) => indexed(i, ansi),
        Color::Named(named) => match named {
            NamedColor::Foreground
            | NamedColor::BrightForeground
            | NamedColor::DimForeground
            | NamedColor::Cursor => fg,
            NamedColor::Background => bg,
            other => {
                let idx = other as usize;
                if idx < 16 {
                    rgb(ansi[idx])
                } else if (259..=266).contains(&idx) {
                    // The dim ANSI block (DimBlack..DimWhite) maps to colors 0..7.
                    rgb(ansi[idx - 259])
                } else {
                    fg
                }
            }
        },
    }
}

/// Map a 256-color palette index to RGB: 0..15 ANSI, 16..231 cube, 232..255 gray.
fn indexed(i: u8, ansi: &[u32; 16]) -> Rgba {
    match i {
        0..=15 => rgb(ansi[i as usize]),
        16..=231 => {
            let j = i - 16;
            let level = |v: u8| -> f32 {
                let step = if v == 0 { 0 } else { 55 + v as u16 * 40 };
                step as f32 / 255.0
            };
            Rgba {
                r: level(j / 36),
                g: level((j % 36) / 6),
                b: level(j % 6),
                a: 1.0,
            }
        }
        232..=255 => {
            let v = (8 + (i - 232) as u16 * 10) as f32 / 255.0;
            Rgba {
                r: v,
                g: v,
                b: v,
                a: 1.0,
            }
        }
    }
}

/// Format a number of seconds as `M:SS`.
fn fmt_time(seconds: f64) -> String {
    let whole = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", whole / 60, whole % 60)
}

/// Paint the scrubber into its bounds: a dim full-width track, a brighter played
/// portion up to `fraction`, and a knob marking the current position.
fn paint_scrubber(
    bounds: Bounds<Pixels>,
    fraction: f32,
    track: Rgba,
    played: Rgba,
    knob: Rgba,
    window: &mut Window,
) {
    let left = f32::from(bounds.left());
    let width = f32::from(bounds.size.width);
    let mid = f32::from(bounds.top()) + f32::from(bounds.size.height) / 2.0;
    let track_h = 4.0;
    let knob_size = 12.0;
    let played_w = width * fraction.clamp(0.0, 1.0);
    let knob_x = (left + played_w - knob_size / 2.0).clamp(left, left + width - knob_size);

    let bar = |x: f32, w: f32, color: Rgba| {
        fill(
            Bounds::new(
                point(px(x), px(mid - track_h / 2.0)),
                size(px(w), px(track_h)),
            ),
            color,
        )
    };
    window.paint_quad(bar(left, width, track));
    window.paint_quad(bar(left, played_w, played));
    window.paint_quad(fill(
        Bounds::new(
            point(px(knob_x), px(mid - knob_size / 2.0)),
            size(px(knob_size), px(knob_size)),
        ),
        knob,
    ));
}
