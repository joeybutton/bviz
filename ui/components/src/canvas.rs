//! Window-agnostic canvas primitives: shaping text to fit a width, and painting a
//! filled, bordered, labeled quad.

use gpui::{App, Bounds, Hsla, Pixels, Rgba, TextAlign, Window, fill, point, px, size};

/// Shapes `text` at `fs`, truncating with an ellipsis to fit `avail` pixels by
/// the same proportional estimate slices and gutter labels share.
pub fn shape_truncated(
    text: &str,
    avail: f32,
    font: gpui::Font,
    fs: Pixels,
    color: Hsla,
    window: &mut Window,
) -> gpui::ShapedLine {
    let line = bviz_ui_assets::shape_line(window, text.to_string(), font.clone(), fs, color);
    if f32::from(line.width()) <= avail {
        return line;
    }
    // The proportional estimate can overshoot in a variable-width font, leaving
    // the ellipsized line wider than the box, so drop characters until it fits.
    let chars: Vec<char> = text.chars().collect();
    let mut keep = ((chars.len() as f32 * avail / f32::from(line.width())).floor() as usize)
        .min(chars.len())
        .saturating_sub(1);
    loop {
        let mut truncated: String = chars[..keep].iter().collect();
        truncated.push('…');
        let shaped = bviz_ui_assets::shape_line(window, truncated, font.clone(), fs, color);
        if keep == 0 || f32::from(shaped.width()) <= avail {
            return shaped;
        }
        keep -= 1;
    }
}

/// Width of the muted leading column that divides a box from its neighbor.
const SEAM_PX: f32 = 1.0;

/// Box styling held constant across a chart, so every box in it seams and labels
/// alike whatever color or width it is drawn at.
#[derive(Clone, Copy)]
pub struct LabelBoxStyle {
    pub background: Rgba,
    pub seam_mix: f32,
    pub label_min_px: f32,
    pub text_pad: f32,
}

/// Paints a filled box and, if it is wide enough, its name truncated to fit and
/// centered vertically — the shared shape behind a flame-chart slice and a
/// flame-graph node.
pub fn paint_labeled_box(
    rect: Bounds<Pixels>,
    fill_color: Hsla,
    name: &str,
    label_color: Hsla,
    style: LabelBoxStyle,
    window: &mut Window,
    cx: &mut App,
) {
    let LabelBoxStyle { background, seam_mix, label_min_px, text_pad } = style;
    let w = f32::from(rect.size.width);
    let h = f32::from(rect.size.height);
    // Square corners keep every column at the box's full height, so a run of thin
    // boxes reads as an even band; a box no wider than its seam is seam throughout,
    // so a run of one-pixel boxes reads as a band of edges.
    let seam_color = Hsla::from(bviz_ui_theme::mix(Rgba::from(fill_color), background, seam_mix));
    let seam_w = w.min(SEAM_PX);
    if w > seam_w {
        window.paint_quad(fill(rect, fill_color));
    }
    let seam = Bounds { origin: rect.origin, size: size(px(seam_w), rect.size.height) };
    window.paint_quad(fill(seam, seam_color));
    if w < label_min_px {
        return;
    }
    let avail = w - 2. * text_pad;
    let font = bviz_ui_assets::text_metrics(window).font;
    let fs = px(h - 2. * text_pad);
    let line = shape_truncated(name, avail, font, fs, label_color, window);
    let ty = rect.origin.y + px((h - f32::from(fs)) / 2.);
    let _ = line.paint(
        point(rect.origin.x + px(text_pad), ty),
        fs,
        TextAlign::Left,
        None,
        window,
        cx,
    );
}
