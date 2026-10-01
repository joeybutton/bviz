use bviz_ui_theme::ThemeColors;
use gpui::{
    AnyView, App, AppContext, BorderStyle, Bounds, Context, Hsla, IntoElement, ParentElement,
    Pixels, Point, Render, Rgba, ShapedLine, SharedString, Size, Styled, TextAlign, Window, div,
    point, px, quad, size,
};

const TOOLTIP_TEXT_SIZE: f32 = 12.0;
/// Inset of the text block from the panel's edges.
const TOOLTIP_PAD: f32 = 6.0;
/// Leading added below each line.
const TOOLTIP_LINE_GAP: f32 = 4.0;
const TOOLTIP_LINE_HEIGHT: f32 = TOOLTIP_TEXT_SIZE + TOOLTIP_LINE_GAP;
/// How far from its anchor the panel is placed along each axis, clear of it.
const TOOLTIP_OFFSET: f32 = 12.0;
/// Corner radius of the panel.
const TOOLTIP_RADIUS: f32 = 4.0;
/// Width of the panel's border.
const TOOLTIP_BORDER: f32 = 1.0;

/// A tooltip's colors. The panel fill is opaque, so text under the tooltip does
/// not read through it.
#[derive(Clone, Copy)]
pub struct TooltipColors {
    pub panel: Rgba,
    pub border: Rgba,
    pub label: Rgba,
}

impl TooltipColors {
    pub fn from_colors(colors: &ThemeColors) -> TooltipColors {
        TooltipColors {
            panel: colors.panel.into(),
            border: colors.border.into(),
            label: colors.text.into(),
        }
    }
}

/// Where a box `extent` long starts along one axis spanning `start..end` for an
/// anchor at `at`, as [`beside_anchor`] places it.
fn beside(at: f32, extent: f32, start: f32, end: f32) -> f32 {
    let after = at + TOOLTIP_OFFSET;
    let before = at - TOOLTIP_OFFSET - extent;
    if after + extent <= end {
        after
    } else if before >= start {
        before
    } else {
        (end - extent).max(start)
    }
}

/// Where a box of `extent` beside an anchor at `at` goes inside `bounds`: on
/// each axis past the anchor by [`TOOLTIP_OFFSET`], or as far before it where
/// that runs past the far edge, so the box covers nothing at the anchor; slid
/// inside only where neither side holds it.
pub fn beside_anchor(
    at: Point<Pixels>,
    extent: Size<Pixels>,
    bounds: Bounds<Pixels>,
) -> Point<Pixels> {
    let along = |at: Pixels, extent: Pixels, start: Pixels, end: Pixels| {
        px(beside(
            f32::from(at),
            f32::from(extent),
            f32::from(start),
            f32::from(end),
        ))
    };
    point(
        along(at.x, extent.width, bounds.left(), bounds.right()),
        along(at.y, extent.height, bounds.top(), bounds.bottom()),
    )
}

/// The largest extent [`beside_anchor`] places clear of an anchor at `at`
/// inside `bounds`, on whichever side of it holds more on each axis.
pub fn room_beside(at: Point<Pixels>, bounds: Bounds<Pixels>) -> Size<Pixels> {
    let along = |at: Pixels, start: Pixels, end: Pixels| {
        ((end - at).max(at - start) - px(TOOLTIP_OFFSET)).max(px(0.))
    };
    size(
        along(at.x, bounds.left(), bounds.right()),
        along(at.y, bounds.top(), bounds.bottom()),
    )
}

/// Paints `lines` in a tooltip panel beside `at` inside `bounds`.
pub fn paint_tooltip_box(
    lines: &[String],
    at: Point<Pixels>,
    bounds: Bounds<Pixels>,
    colors: TooltipColors,
    window: &mut Window,
    cx: &mut App,
) {
    paint_tooltip(
        lines,
        |extent| beside_anchor(at, extent, bounds),
        colors,
        window,
        cx,
    );
}

/// Each of `lines` shaped as a tooltip panel reads it.
fn shape_lines(lines: &[String], color: Hsla, window: &mut Window) -> Vec<ShapedLine> {
    let font = bviz_ui_assets::text_metrics(window).font;
    let fs = px(TOOLTIP_TEXT_SIZE);
    lines
        .iter()
        .map(|line| bviz_ui_assets::shape_line(window, line.clone(), font.clone(), fs, color))
        .collect()
}

/// The extent of the padded panel around `shaped`.
fn panel_extent(shaped: &[ShapedLine]) -> Size<Pixels> {
    let text_w = shaped
        .iter()
        .map(|l| f32::from(l.width()))
        .fold(0.0_f32, f32::max);
    size(
        px(text_w + 2.0 * TOOLTIP_PAD),
        px(shaped.len() as f32 * TOOLTIP_LINE_HEIGHT + 2.0 * TOOLTIP_PAD),
    )
}

/// The extent of the panel [`paint_tooltip`] paints reading `lines`.
pub fn tooltip_extent(lines: &[String], window: &mut Window) -> Size<Pixels> {
    let color = window.text_style().color;
    panel_extent(&shape_lines(lines, color, window))
}

/// Measures `lines`, sizes a padded panel around them, and paints it with its
/// top-left corner where `place` puts a panel of that extent.
pub fn paint_tooltip(
    lines: &[String],
    place: impl FnOnce(Size<Pixels>) -> Point<Pixels>,
    colors: TooltipColors,
    window: &mut Window,
    cx: &mut App,
) {
    let shaped = shape_lines(lines, colors.label.into(), window);
    let extent = panel_extent(&shaped);
    let origin = place(extent);

    window.paint_quad(quad(
        Bounds::new(origin, extent),
        px(TOOLTIP_RADIUS),
        colors.panel,
        px(TOOLTIP_BORDER),
        Hsla::from(colors.border),
        BorderStyle::Solid,
    ));
    let fs = px(TOOLTIP_TEXT_SIZE);
    for (i, line) in shaped.into_iter().enumerate() {
        let offset = point(
            px(TOOLTIP_PAD),
            px(TOOLTIP_PAD + i as f32 * TOOLTIP_LINE_HEIGHT),
        );
        let _ = line.paint(origin + offset, fs, TextAlign::Left, None, window, cx);
    }
}

/// A line of text in the panel [`paint_tooltip_box`] paints, as a view an
/// element's own tooltip can show.
pub struct TextTooltip {
    text: SharedString,
    colors: TooltipColors,
}

impl TextTooltip {
    /// Builds the tooltip showing `text`, for an element's `tooltip`.
    pub fn builder(
        text: SharedString,
        colors: TooltipColors,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        move |_, cx| {
            let text = text.clone();
            cx.new(|_| TextTooltip { text, colors }).into()
        }
    }
}

impl Render for TextTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // A tooltip view is drawn outside the element tree that sets the UI
        // face, so it inherits none.
        div()
            .font_family(bviz_ui_assets::UI)
            .p(px(TOOLTIP_PAD))
            .rounded(px(TOOLTIP_RADIUS))
            .border(px(TOOLTIP_BORDER))
            .border_color(self.colors.border)
            .bg(self.colors.panel)
            .text_size(px(TOOLTIP_TEXT_SIZE))
            .text_color(self.colors.label)
            .child(self.text.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tooltip_flips_before_its_anchor_where_it_would_run_past_the_end() {
        assert_eq!(beside(10.0, 50.0, 0.0, 100.0), 10.0 + TOOLTIP_OFFSET);
        assert_eq!(beside(90.0, 50.0, 0.0, 100.0), 90.0 - TOOLTIP_OFFSET - 50.0);
    }

    #[test]
    fn a_tooltip_neither_side_holds_is_slid_inside_the_span() {
        assert_eq!(beside(50.0, 80.0, 0.0, 100.0), 20.0);
        assert_eq!(beside(50.0, 120.0, 0.0, 100.0), 0.0);
    }

    #[test]
    fn the_room_beside_an_anchor_is_held_clear_of_it() {
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(100.), px(60.)));
        for at in [point(px(10.), px(50.)), point(px(70.), px(20.))] {
            let room = room_beside(at, bounds);
            let placed = Bounds::new(beside_anchor(at, room, bounds), room);
            assert!(!placed.contains(&at), "{at:?} {placed:?}");
            assert!(placed.right() <= bounds.right() && placed.bottom() <= bounds.bottom());
            assert!(placed.left() >= bounds.left() && placed.top() >= bounds.top());
        }
    }
}
