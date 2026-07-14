//! Window-agnostic canvas primitives: shaping text to fit a width, and painting a
//! filled, bordered, labeled quad.

use gpui::{App, BorderStyle, Bounds, Edges, Hsla, Pixels, Rgba, TextAlign, Window, point, px, quad};

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

/// Paints a filled box and, if it is wide enough, its name truncated to fit and
/// centered vertically — the shared shape behind a flame-chart slice and a
/// flame-graph node.
#[allow(clippy::too_many_arguments)]
pub fn paint_labeled_box(
    rect: Bounds<Pixels>,
    fill: Hsla,
    name: &str,
    label_color: Hsla,
    background: Rgba,
    seam_min_px: f32,
    seam_mix: f32,
    label_min_px: f32,
    text_pad: f32,
    window: &mut Window,
    cx: &mut App,
) {
    let w = f32::from(rect.size.width);
    let h = f32::from(rect.size.height);
    // Square corners: every column paints the box's full height, so a run of
    // thin boxes reads as an even band rather than rounded humps that dip in
    // height between them. A box wide enough to spare its first column carries a
    // muted left-edge seam to divide it from its neighbor; a narrower one keeps
    // its whole fill so it still reads as its color rather than an edge line.
    let (border, seam) = if w > seam_min_px {
        let seam = Hsla::from(bviz_ui_theme::mix(Rgba::from(fill), background, seam_mix));
        (Edges { left: px(1.), ..Default::default() }, seam)
    } else {
        (Edges::default(), fill)
    };
    window.paint_quad(quad(rect, px(0.), fill, border, seam, BorderStyle::Solid));
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
