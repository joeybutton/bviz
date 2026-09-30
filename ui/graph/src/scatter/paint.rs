//! Painting the scatter plot: its axes and the marks placed along them.

use super::plot::{DataArea, LogScale, Plotted, Tick, tick_label};
use crate::Palette;
use bviz_ui_theme::mix;
use gpui::{
    App, BorderStyle, Bounds, Hsla, Pixels, Rgba, ShapedLine, TextAlign, Window, fill, point, px,
    quad, size,
};

/// Room left of the axes for the vertical tick labels.
const LEFT_MARGIN: f32 = 52.0;
/// Room right of the axes for the last horizontal tick label, centred on the
/// axes' right edge, to reach past it.
const RIGHT_MARGIN: f32 = 24.0;
/// Room above the axes for the vertical axis title.
const TOP_MARGIN: f32 = 32.0;
/// Room below the axes for the horizontal tick labels and axis title.
const BOTTOM_MARGIN: f32 = 50.0;

/// Below this the axes leave no room worth plotting in, and nothing is drawn.
const MIN_FRAME: f32 = 40.0;

const MAJOR_TICK: f32 = 6.0;
const MINOR_TICK: f32 = 3.0;
/// Between a tick's end and its label, and between the labels and a title.
const LABEL_GAP: f32 = 4.0;

const TICK_LABEL_SIZE: f32 = 11.0;
const TITLE_SIZE: f32 = 12.0;

/// How far a grid line recedes into the background: faint enough that it does
/// not compete with the marks.
const GRID_BLEND: f32 = 0.5;
/// Opacity of a mark, so a dense cluster reads darker than a lone target.
const MARK_ALPHA: f32 = 0.5;

const X_TITLE: &str = "closure size (transitive dependencies)";
const Y_TITLE: &str = "blast radius (transitive dependents)";

/// Paints `plotted` filling `bounds`.
pub(crate) fn paint_plot(
    plotted: &Plotted,
    palette: &Palette,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let axes = Bounds::from_corners(
        point(
            bounds.left() + px(LEFT_MARGIN),
            bounds.top() + px(TOP_MARGIN),
        ),
        point(
            bounds.right() - px(RIGHT_MARGIN),
            bounds.bottom() - px(BOTTOM_MARGIN),
        ),
    );
    if axes.size.width < px(MIN_FRAME) || axes.size.height < px(MIN_FRAME) {
        return;
    }
    let area = DataArea::within(axes, plotted.x, plotted.y);
    let ink = Ink::new(palette);
    paint_grid(&area, plotted, axes, &ink, window);
    paint_axes(&area, plotted, axes, &ink, window, cx);
    // One layer shares one draw order among the marks, so a mark is inserted
    // without searching what it overlaps; submission order still stacks them.
    window.paint_layer(axes, |window| {
        for mark in &plotted.marks {
            let center = area.place(&mark.target);
            let r = px(mark.radius);
            let color = Rgba {
                a: MARK_ALPHA,
                ..palette.kind_accent(mark.target.kind)
            };
            window.paint_quad(quad(
                Bounds::new(center - point(r, r), size(r * 2., r * 2.)),
                r,
                color,
                px(0.),
                Hsla::transparent_black(),
                BorderStyle::Solid,
            ));
        }
    });
}

/// The colours the plot's frame is drawn in.
struct Ink {
    axis: Rgba,
    grid: Rgba,
    label: Hsla,
    muted: Hsla,
}

impl Ink {
    fn new(palette: &Palette) -> Self {
        let muted: Rgba = palette.colors.text_muted.into();
        Self {
            axis: palette.border,
            grid: mix(palette.border, palette.background, GRID_BLEND),
            label: palette.label.into(),
            muted: muted.into(),
        }
    }
}

/// A faint line across the axes at every major tick.
fn paint_grid(
    area: &DataArea,
    plotted: &Plotted,
    axes: Bounds<Pixels>,
    ink: &Ink,
    window: &mut Window,
) {
    for tick in majors(plotted.x) {
        let x = area.x_of(tick.value);
        window.paint_quad(fill(
            Bounds::new(point(x, axes.top()), size(px(1.), axes.size.height)),
            ink.grid,
        ));
    }
    for tick in majors(plotted.y) {
        let y = area.y_of(tick.value);
        window.paint_quad(fill(
            Bounds::new(point(axes.left(), y), size(axes.size.width, px(1.))),
            ink.grid,
        ));
    }
}

fn majors(scale: LogScale) -> impl Iterator<Item = Tick> {
    scale.ticks().into_iter().filter(|tick| tick.major)
}

/// The axis lines, their ticks and tick labels, and the axis titles.
fn paint_axes(
    area: &DataArea,
    plotted: &Plotted,
    axes: Bounds<Pixels>,
    ink: &Ink,
    window: &mut Window,
    cx: &mut App,
) {
    let (left, bottom) = (axes.left(), axes.bottom());
    window.paint_quad(fill(
        Bounds::new(
            point(left - px(1.), axes.top()),
            size(px(1.), axes.size.height + px(1.)),
        ),
        ink.axis,
    ));
    window.paint_quad(fill(
        Bounds::new(
            point(left - px(1.), bottom),
            size(axes.size.width + px(1.), px(1.)),
        ),
        ink.axis,
    ));
    let length = |tick: &Tick| px(if tick.major { MAJOR_TICK } else { MINOR_TICK });

    let label_top = bottom + px(MAJOR_TICK + LABEL_GAP);
    for tick in plotted.x.ticks() {
        let x = area.x_of(tick.value);
        window.paint_quad(fill(
            Bounds::new(point(x, bottom), size(px(1.), length(&tick))),
            ink.axis,
        ));
        if tick.major {
            let text = tick_label(tick.value);
            paint_text(
                &text,
                TICK_LABEL_SIZE,
                ink.muted,
                Align::Center(x),
                label_top,
                window,
                cx,
            );
        }
    }
    let label_right = left - px(MAJOR_TICK + LABEL_GAP);
    for tick in plotted.y.ticks() {
        let y = area.y_of(tick.value);
        let tick_left = left - length(&tick);
        window.paint_quad(fill(
            Bounds::new(point(tick_left, y), size(length(&tick), px(1.))),
            ink.axis,
        ));
        if tick.major {
            let text = tick_label(tick.value);
            let top = y - px(TICK_LABEL_SIZE / 2.);
            paint_text(
                &text,
                TICK_LABEL_SIZE,
                ink.muted,
                Align::End(label_right),
                top,
                window,
                cx,
            );
        }
    }

    let x_title_top = label_top + px(TICK_LABEL_SIZE + LABEL_GAP * 2.);
    let x_center = axes.left() + axes.size.width / 2.;
    paint_text(
        X_TITLE,
        TITLE_SIZE,
        ink.label,
        Align::Center(x_center),
        x_title_top,
        window,
        cx,
    );
    let y_title_top = axes.top() - px(TITLE_SIZE + LABEL_GAP * 2.);
    paint_text(
        Y_TITLE,
        TITLE_SIZE,
        ink.label,
        Align::Start(left),
        y_title_top,
        window,
        cx,
    );
}

/// Which point of a line of text sits at a given horizontal position.
#[derive(Clone, Copy)]
enum Align {
    Start(Pixels),
    Center(Pixels),
    End(Pixels),
}

fn paint_text(
    text: &str,
    font_size: f32,
    color: Hsla,
    align: Align,
    top: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    let (line, left) = shaped(text, font_size, color, align, window);
    let _ = line.paint(
        point(left, top),
        px(font_size),
        TextAlign::Left,
        None,
        window,
        cx,
    );
}

/// `text` shaped as one line, and where its left edge falls for `align`.
fn shaped(
    text: &str,
    font_size: f32,
    color: Hsla,
    align: Align,
    window: &mut Window,
) -> (ShapedLine, Pixels) {
    let font = bviz_ui_assets::text_metrics(window).font;
    let line = bviz_ui_assets::shape_line(window, text.to_owned(), font, px(font_size), color);
    let left = match align {
        Align::Start(x) => x,
        Align::Center(x) => x - line.width() / 2.,
        Align::End(x) => x - line.width(),
    };
    (line, left)
}
