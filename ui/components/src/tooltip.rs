use bviz_ui_theme::ThemeColors;
use gpui::{
    AnyView, App, AppContext, BorderStyle, Bounds, Context, Hsla, IntoElement, ParentElement,
    Pixels, Point, Render, Rgba, SharedString, Styled, TextAlign, Window, div, point, px, quad,
    size,
};

const TOOLTIP_TEXT_SIZE: f32 = 12.0;
/// Inset of the text block from the panel's edges.
const TOOLTIP_PAD: f32 = 6.0;
/// Leading added below each line.
const TOOLTIP_LINE_GAP: f32 = 4.0;
/// How far down and right of the cursor the panel is placed, clear of it.
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

/// Measures `lines`, sizes a padded box, clamps it inside `bounds`, and paints the
/// panel background, border, and each line near `at`.
pub fn paint_tooltip_box(
    lines: &[String],
    at: Point<Pixels>,
    bounds: Bounds<Pixels>,
    colors: TooltipColors,
    window: &mut Window,
    cx: &mut App,
) {
    let font = bviz_ui_assets::text_metrics(window).font;
    let fs = px(TOOLTIP_TEXT_SIZE);
    let shaped: Vec<_> = lines
        .iter()
        .map(|line| {
            bviz_ui_assets::shape_line(window, line.clone(), font.clone(), fs, colors.label.into())
        })
        .collect();
    let text_w = shaped
        .iter()
        .map(|l| f32::from(l.width()))
        .fold(0.0_f32, f32::max);
    let line_h = TOOLTIP_TEXT_SIZE + TOOLTIP_LINE_GAP;
    let box_w = text_w + 2.0 * TOOLTIP_PAD;
    let box_h = shaped.len() as f32 * line_h + 2.0 * TOOLTIP_PAD;

    let left = f32::from(bounds.origin.x);
    let top = f32::from(bounds.origin.y);
    let right = left + f32::from(bounds.size.width);
    let bottom = top + f32::from(bounds.size.height);
    let mut x = f32::from(at.x) + TOOLTIP_OFFSET;
    let mut y = f32::from(at.y) + TOOLTIP_OFFSET;
    if x + box_w > right {
        x = (right - box_w).max(left);
    }
    if y + box_h > bottom {
        y = (bottom - box_h).max(top);
    }

    window.paint_quad(quad(
        Bounds::new(point(px(x), px(y)), size(px(box_w), px(box_h))),
        px(TOOLTIP_RADIUS),
        colors.panel,
        px(TOOLTIP_BORDER),
        Hsla::from(colors.border),
        BorderStyle::Solid,
    ));
    for (i, line) in shaped.into_iter().enumerate() {
        let ly = y + TOOLTIP_PAD + i as f32 * line_h;
        let _ = line.paint(
            point(px(x + TOOLTIP_PAD), px(ly)),
            fs,
            TextAlign::Left,
            None,
            window,
            cx,
        );
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
