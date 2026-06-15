//! A gpui view that renders a build's display [`Terminal`] as styled monospace
//! text: the visible viewport, reflowed to the window, one shaped line per row
//! with per-cell colors and background fills.

use std::time::Duration;

use bviz_terminal::{
    BuildOutput, Cell, Color, Flags, NamedColor, Rgb, Scroll, SelectionType, Side, Terminal, Url,
};
use bviz_ui_shell::Chrome;
use gpui::{
    App, Bounds, ClipboardItem, Context, DispatchPhase, Element, Entity, FocusHandle, Font,
    FontStyle, FontWeight,
    GlobalElementId, InspectorElementId, IntoElement, KeyBinding, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, Render, Rgba,
    ScrollWheelEvent, ShapedLine, SharedString, StrikethroughStyle, Style, TextRun, UnderlineStyle,
    Window,
    WindowAppearance, actions, div, fill, point, prelude::*, px, relative, rgb, size,
};

actions!(
    bazel_terminal,
    [
        ScrollLineUp,
        ScrollLineDown,
        ScrollPageUp,
        ScrollPageDown,
        ScrollToTop,
        ScrollToBottom,
        Copy
    ]
);

/// Lines moved per arrow-key press.
const SCROLL_STEP: i32 = 3;

/// Width of the scroll bar gutter reserved on the right edge.
const SCROLLBAR_WIDTH: f32 = 8.0;

/// How long the scroll bar stays visible after the last scroll interaction.
const SCROLLBAR_HIDE: Duration = Duration::from_millis(1200);

/// Smallest the scroll bar thumb may shrink to.
const MIN_THUMB: f32 = 24.0;

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

/// The root view: the rendered build terminal inside the window chrome.
pub struct TerminalView {
    output: BuildOutput,
    /// The display terminal, created at the window size on first layout.
    terminal: Option<Terminal>,
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
}

impl TerminalView {
    pub fn new(output: BuildOutput, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.bind_keys([
            KeyBinding::new("up", ScrollLineUp, Some("Terminal")),
            KeyBinding::new("down", ScrollLineDown, Some("Terminal")),
            KeyBinding::new("pageup", ScrollPageUp, Some("Terminal")),
            KeyBinding::new("pagedown", ScrollPageDown, Some("Terminal")),
            KeyBinding::new("home", ScrollToTop, Some("Terminal")),
            KeyBinding::new("end", ScrollToBottom, Some("Terminal")),
            KeyBinding::new("cmd-c", Copy, Some("Terminal")),
            KeyBinding::new("ctrl-c", Copy, Some("Terminal")),
        ]);
        let body_focus = cx.focus_handle();
        body_focus.focus(window, cx);
        let mut view = Self {
            output,
            terminal: None,
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
        };
        // Show the bar briefly on launch so the scrollable extent is visible,
        // then let it auto-hide.
        view.reveal_scrollbar(cx);
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

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
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

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.terminal.as_ref().and_then(Terminal::selection_text) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
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
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = bviz_ui_theme::colors(window);
        Chrome::new(&self.focus_handle).title("bazel_term").child(
            div()
                .key_context("Terminal")
                .track_focus(&self.body_focus)
                .on_action(cx.listener(Self::scroll_line_up))
                .on_action(cx.listener(Self::scroll_line_down))
                .on_action(cx.listener(Self::scroll_page_up))
                .on_action(cx.listener(Self::scroll_page_down))
                .on_action(cx.listener(Self::scroll_to_top))
                .on_action(cx.listener(Self::scroll_to_bottom))
                .on_action(cx.listener(Self::copy))
                .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
                .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                .on_mouse_move(cx.listener(Self::on_mouse_move))
                .when(self.hovered_url.is_some(), |body| body.cursor_pointer())
                .size_full()
                .overflow_hidden()
                .bg(colors.background)
                .font_family(bviz_ui_assets::MONO)
                .text_size(px(13.))
                .text_color(colors.text)
                .child(TerminalElement { view: cx.entity() }),
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
        let text_style = window.text_style();
        let font = text_style.font();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();

        // A monospace advance width, measured from a single glyph.
        let probe = TextRun {
            len: 1,
            font: font.clone(),
            color: text_style.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let cell_width = window
            .text_system()
            .shape_line(SharedString::from("M"), font_size, &[probe], None)
            .width;

        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();

        (
            window.request_layout(style, [], cx),
            Metrics {
                cell_width,
                line_height,
                font_size,
                font,
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
        self.view
            .update(cx, |view, _| match view.terminal.as_mut() {
                Some(terminal) => terminal.resize(cols, rows),
                None => view.terminal = Some(Terminal::new(&view.output, cols, rows)),
            });
        let (viewport, history, offset, selection) = match self.view.read(cx).terminal.as_ref() {
            Some(terminal) => (
                terminal.viewport(),
                terminal.history_size(),
                terminal.display_offset(),
                terminal.selection_spans(),
            ),
            None => (Vec::new(), 0, 0, Vec::new()),
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
        // handlers so it keeps working when the cursor leaves the window (Wayland
        // grabs the pointer for the held button and still delivers move/up here).
        if self.view.read(cx).drag_grab.is_some() {
            let view = self.view.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble {
                    view.update(cx, |view, cx| view.drag_to(f32::from(event.position.y), cx));
                }
            });
            let view = self.view.clone();
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble {
                    view.update(cx, |view, cx| {
                        if view.drag_grab.take().is_some() {
                            // Keep the bar up briefly after the drag, then hide.
                            view.reveal_scrollbar(cx);
                        }
                    });
                }
            });
        }

        // Likewise follow and end a text-selection drag with window-global
        // handlers, so dragging past the window edge keeps extending the
        // selection and a release outside still ends it.
        if self.view.read(cx).selecting {
            let view = self.view.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble {
                    view.update(cx, |view, cx| view.selection_drag_to(event.position, cx));
                }
            });
            let view = self.view.clone();
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble {
                    view.update(cx, |view, cx| {
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
                    });
                }
            });
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
        let strikethrough =
            cell.flags
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
