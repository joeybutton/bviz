//! Window-agnostic canvas primitives: shaping text to fit a width, and painting a
//! filled, bordered, labeled quad.

use gpui::{App, Bounds, Hsla, Pixels, Rgba, TextAlign, Window, fill, point, px, size};

/// Shapes `text` at `fs`, truncated with an ellipsis to fit `avail` pixels.
pub fn shape_truncated(
    text: &str,
    avail: f32,
    font: gpui::Font,
    fs: Pixels,
    color: Hsla,
    window: &mut Window,
) -> gpui::ShapedLine {
    truncated(text, avail, |s| {
        bviz_ui_assets::shape_line(window, s, font.clone(), fs, color)
    })
}

const ELLIPSIS: char = '…';

/// `text` shaped by `shape`, or, if wider than `avail`, its longest prefix that
/// fits `avail` with an [`ELLIPSIS`] appended, the prefix no longer than the
/// proportional estimate of what fits and down to none at all.
///
/// The whole line's glyph positions place the cut, which a shape or two
/// confirms; finding it that way relies on an ellipsized prefix never narrowing
/// as it lengthens.
fn truncated(
    text: &str,
    avail: f32,
    mut shape: impl FnMut(String) -> gpui::ShapedLine,
) -> gpui::ShapedLine {
    let line = shape(text.to_owned());
    let width = f32::from(line.width());
    if width <= avail {
        return line;
    }
    let chars = text.chars().count();
    // The proportional estimate can overshoot in a variable-width font, leaving
    // the ellipsized line wider than the box; it only caps the prefix.
    let estimate = ((chars as f32 * avail / width).floor() as usize)
        .min(chars)
        .saturating_sub(1);
    let ellipsis = shape(ELLIPSIS.to_string()).width();
    let placed = line
        .index_for_x(px(avail) - ellipsis)
        .map_or(chars, |byte| {
            text.char_indices().take_while(|&(i, _)| i < byte).count()
        });
    let mut ellipsized = |keep: usize| {
        let cut = text.char_indices().nth(keep).map_or(text.len(), |(i, _)| i);
        let mut s = String::with_capacity(cut + ELLIPSIS.len_utf8());
        s.push_str(&text[..cut]);
        s.push(ELLIPSIS);
        shape(s)
    };
    let fits = |line: &gpui::ShapedLine| f32::from(line.width()) <= avail;
    let mut keep = placed.min(estimate);
    let mut best = ellipsized(keep);
    if fits(&best) {
        while keep < estimate {
            let longer = ellipsized(keep + 1);
            if !fits(&longer) {
                break;
            }
            best = longer;
            keep += 1;
        }
    } else {
        while keep > 0 && !fits(&best) {
            keep -= 1;
            best = ellipsized(keep);
        }
    }
    best
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

impl LabelBoxStyle {
    /// The width a label of a box filling `rect` is drawn within, and the font
    /// size it is drawn at.
    pub fn label_room(&self, rect: Bounds<Pixels>) -> (f32, Pixels) {
        let inset = 2. * self.text_pad;
        (
            f32::from(rect.size.width) - inset,
            rect.size.height - px(inset),
        )
    }
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
    let LabelBoxStyle {
        background,
        seam_mix,
        label_min_px,
        text_pad,
    } = style;
    let w = f32::from(rect.size.width);
    let h = f32::from(rect.size.height);
    // Square corners keep every column at the box's full height, so a run of thin
    // boxes reads as an even band; a box no wider than its seam is seam throughout,
    // so a run of one-pixel boxes reads as a band of edges.
    let seam_color = Hsla::from(bviz_ui_theme::mix(
        Rgba::from(fill_color),
        background,
        seam_mix,
    ));
    let seam_w = w.min(SEAM_PX);
    if w > seam_w {
        window.paint_quad(fill(rect, fill_color));
    }
    let seam = Bounds {
        origin: rect.origin,
        size: size(px(seam_w), rect.size.height),
    };
    window.paint_quad(fill(seam, seam_color));
    if w < label_min_px {
        return;
    }
    let (avail, fs) = style.label_room(rect);
    let font = bviz_ui_assets::text_metrics(window).font;
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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Font, FontWeight, SharedString, TextRun, WindowTextSystem, font};
    use std::sync::Arc;

    /// Truncation by dropping one character at a time from the proportional
    /// estimate until the ellipsized prefix fits: the definition
    /// [`truncated`] must agree with.
    fn truncated_by_scan(
        text: &str,
        avail: f32,
        mut shape: impl FnMut(String) -> gpui::ShapedLine,
    ) -> gpui::ShapedLine {
        let line = shape(text.to_owned());
        if f32::from(line.width()) <= avail {
            return line;
        }
        let chars: Vec<char> = text.chars().collect();
        let mut keep = ((chars.len() as f32 * avail / f32::from(line.width())).floor() as usize)
            .min(chars.len())
            .saturating_sub(1);
        loop {
            let mut truncated: String = chars[..keep].iter().collect();
            truncated.push(ELLIPSIS);
            let shaped = shape(truncated);
            if keep == 0 || f32::from(shaped.width()) <= avail {
                return shaped;
            }
            keep -= 1;
        }
    }

    const TEXTS: &[&str] = &[
        "//ui/graph:graph_lib",
        "@@rules_rust++rust+rust_linux_x86_64__x86_64-unknown-linux-gnu__stable_tools//:rustc_lib",
        "bazel-out/k8-opt-exec-ST-d57f47055a04/bin/external/protobuf+/protoc",
        "Compiling Rust (with build script output) bviz_ui_components v0.1.0",
        "iiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiii",
        "WWWWWWWWWWWWWWWWWWWWWWWWWWWWWW",
        "iWiWiWiW lll mmm ::// ..__ AVAVAV To Ty Wa",
        "fi fl ffi -> => != <= >= :: ...",
        "naïve café résumé façade — “quoted” ‘single’",
        "e\u{301}e\u{301}e\u{301} combining a\u{308}o\u{308}u\u{308}",
        "x",
        "ab",
    ];

    /// Width step between the room each text is truncated to, finer than any
    /// glyph's advance at the sizes tried.
    const AVAIL_STEP: f32 = 0.5;

    fn shaper(
        text_system: &WindowTextSystem,
        font: Font,
        size: Pixels,
    ) -> impl FnMut(String) -> gpui::ShapedLine {
        move |s: String| {
            let run = TextRun {
                len: s.len(),
                font: font.clone(),
                color: Hsla::black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            text_system.shape_line(SharedString::from(s), size, &[run], None)
        }
    }

    #[test]
    fn truncates_where_dropping_one_character_at_a_time_does() {
        gpui_platform::headless().run(|cx| {
            bviz_ui_assets::load_fonts(cx);
            let text_system = WindowTextSystem::new(Arc::clone(cx.text_system()));
            let fonts = [
                font(bviz_ui_assets::UI),
                font(bviz_ui_assets::UI).bold(),
                Font {
                    weight: FontWeight::LIGHT,
                    ..font(bviz_ui_assets::UI)
                },
                font(bviz_ui_assets::MONO),
            ];
            for f in fonts {
                for size in [5.0, 11.3, 24.0] {
                    for text in TEXTS {
                        let mut shape = shaper(&text_system, f.clone(), px(size));
                        let width = f32::from(shape(text.to_string()).width());
                        let mut avail = -1.0;
                        while avail <= width + 1.0 {
                            let got = truncated(text, avail, &mut shape);
                            let want = truncated_by_scan(text, avail, &mut shape);
                            assert_eq!(
                                got.text, want.text,
                                "{text:?} in {:?} at {size}px within {avail}px",
                                f.weight
                            );
                            avail += AVAIL_STEP;
                        }
                    }
                }
            }
            // A quit only lands once the event loop runs.
            cx.spawn(async |cx| cx.update(|cx| cx.quit())).detach();
        });
    }
}
