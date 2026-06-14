//! A gpui view that renders a captured terminal [`Screen`] as styled monospace
//! text: one shaped line per row, with per-cell colors and background fills.

use bviz_terminal::{Cell, Color, Flags, NamedColor, Rgb, Screen};
use bviz_ui_shell::Chrome;
use gpui::{
    App, Bounds, Context, Element, FocusHandle, Font, FontStyle, FontWeight, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, PaintQuad, Pixels, Render, Rgba, ShapedLine,
    SharedString, Style, TextRun, UnderlineStyle, Window, div, fill, point, prelude::*, px, rgb,
    size,
};
use std::rc::Rc;

/// The root view: the rendered build terminal inside the window chrome.
pub struct TerminalView {
    screen: Rc<Screen>,
    focus_handle: FocusHandle,
}

impl TerminalView {
    pub fn new(screen: Screen, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            screen: Rc::new(screen),
            focus_handle: cx.focus_handle(),
        }
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let colors = bviz_ui_theme::colors(window);
        Chrome::new(&self.focus_handle).title("bazel_term").child(
            div()
                .size_full()
                .overflow_hidden()
                .bg(colors.background)
                .font_family(bviz_ui_assets::MONO)
                .text_size(px(13.))
                .text_color(colors.text)
                .child(TerminalElement {
                    screen: self.screen.clone(),
                }),
        )
    }
}

/// Custom element that measures the monospace cell, shapes each grid row, and
/// paints background fills and glyphs.
struct TerminalElement {
    screen: Rc<Screen>,
}

/// Font metrics resolved once per layout from the inherited monospace text style.
struct Metrics {
    cell_width: Pixels,
    line_height: Pixels,
    font_size: Pixels,
    font: Font,
}

struct Prepaint {
    lines: Vec<ShapedLine>,
    backgrounds: Vec<PaintQuad>,
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

        let cols = self.screen.columns.max(1) as f32;
        let rows = self.screen.rows.len().max(1) as f32;
        let mut style = Style::default();
        style.size.width = (cell_width * cols).into();
        style.size.height = (line_height * rows).into();

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
        _: &mut App,
    ) -> Prepaint {
        let colors = bviz_ui_theme::colors(window);
        let fg = colors.text;
        let bg = colors.background;

        let mut lines = Vec::with_capacity(self.screen.rows.len());
        let mut backgrounds = Vec::new();

        for (r, row) in self.screen.rows.iter().enumerate() {
            let y = bounds.top() + metrics.line_height * r as f32;
            let text: SharedString = row.iter().map(|cell| cell.c).collect::<String>().into();
            let mut runs = Vec::new();

            let mut col = 0;
            while col < row.len() {
                let start = col;
                let key = style_key(&row[col]);
                let mut byte_len = 0;
                while col < row.len() && style_key(&row[col]) == key {
                    byte_len += row[col].c.len_utf8();
                    col += 1;
                }
                let style = CellStyle::resolve(&row[start], fg, bg);

                if style.background != bg {
                    let x = bounds.left() + metrics.cell_width * start as f32;
                    let width = metrics.cell_width * (col - start) as f32;
                    backgrounds.push(fill(
                        Bounds::new(point(x, y), size(width, metrics.line_height)),
                        style.background,
                    ));
                }

                let mut font = metrics.font.clone();
                font.weight = style.weight;
                font.style = style.font_style;
                runs.push(TextRun {
                    len: byte_len,
                    font,
                    color: style.foreground.into(),
                    background_color: None,
                    underline: style.underline,
                    strikethrough: None,
                });
            }

            lines.push(
                window
                    .text_system()
                    .shape_line(text, metrics.font_size, &runs, None),
            );
        }

        Prepaint { lines, backgrounds }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        metrics: &mut Metrics,
        prepaint: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        for quad in prepaint.backgrounds.drain(..) {
            window.paint_quad(quad);
        }
        for (i, line) in prepaint.lines.iter().enumerate() {
            let origin = point(bounds.left(), bounds.top() + metrics.line_height * i as f32);
            line.paint(
                origin,
                metrics.line_height,
                gpui::TextAlign::Left,
                None,
                window,
                cx,
            )
            .unwrap();
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
}

impl CellStyle {
    fn resolve(cell: &Cell, fg: Rgba, bg: Rgba) -> CellStyle {
        let mut foreground = to_rgba(cell.fg, fg, bg);
        let mut background = to_rgba(cell.bg, fg, bg);
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut foreground, &mut background);
        }
        if cell.flags.contains(Flags::DIM) {
            foreground = bviz_ui_theme::mix(foreground, background, 0.45);
        }
        if cell.flags.contains(Flags::HIDDEN) {
            foreground = background;
        }
        let underline = cell
            .flags
            .intersects(Flags::ALL_UNDERLINES)
            .then_some(UnderlineStyle {
                color: Some(foreground.into()),
                thickness: px(1.0),
                wavy: false,
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
        }
    }
}

/// The 16 standard ANSI colors. The default foreground and background follow the
/// theme's light/dark appearance (see [`to_rgba`]), but gpui's base palette has no
/// ANSI colors, so these absolute values stand in for the named palette.
const ANSI: [u32; 16] = [
    0x000000, 0xcd0000, 0x00cd00, 0xcdcd00, 0x0000ee, 0xcd00cd, 0x00cdcd, 0xe5e5e5, 0x7f7f7f,
    0xff0000, 0x00ff00, 0xffff00, 0x5c5cff, 0xff00ff, 0x00ffff, 0xffffff,
];

fn rgb_to_rgba(c: Rgb) -> Rgba {
    Rgba {
        r: c.r as f32 / 255.0,
        g: c.g as f32 / 255.0,
        b: c.b as f32 / 255.0,
        a: 1.0,
    }
}

/// Resolve an alacritty [`Color`] against the terminal's default fore/background.
fn to_rgba(color: Color, fg: Rgba, bg: Rgba) -> Rgba {
    match color {
        Color::Spec(c) => rgb_to_rgba(c),
        Color::Indexed(i) => indexed(i),
        Color::Named(named) => match named {
            NamedColor::Foreground
            | NamedColor::BrightForeground
            | NamedColor::DimForeground
            | NamedColor::Cursor => fg,
            NamedColor::Background => bg,
            other => {
                let idx = other as usize;
                if idx < 16 {
                    rgb(ANSI[idx])
                } else if (259..=266).contains(&idx) {
                    // The dim ANSI block (DimBlack..DimWhite) maps to colors 0..7.
                    rgb(ANSI[idx - 259])
                } else {
                    fg
                }
            }
        },
    }
}

/// Map a 256-color palette index to RGB: 0..15 ANSI, 16..231 cube, 232..255 gray.
fn indexed(i: u8) -> Rgba {
    match i {
        0..=15 => rgb(ANSI[i as usize]),
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
