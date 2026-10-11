//! Window-agnostic canvas primitives: shaping text to fit a width, and painting a
//! filled, bordered, labeled quad.

use gpui::{App, Bounds, Hsla, Pixels, Rgba, TextAlign, Window, fill, point, px, size};
use std::ops::Range;

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

/// Text set in two inks: the bytes `muted` of it in a muted one, the rest in
/// the ink it is set in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inked<'a> {
    pub text: &'a str,
    pub muted: Range<usize>,
}

impl<'a> From<&'a str> for Inked<'a> {
    fn from(text: &'a str) -> Self {
        Self { text, muted: 0..0 }
    }
}

impl Inked<'_> {
    /// The runs of colour a line of the first `len` bytes of this, followed
    /// by whatever cuts it short, is set in: `muted` over the muted bytes,
    /// `ink` over the rest.
    fn runs(&self, len: usize, ink: Hsla, muted: Hsla) -> Vec<(usize, Hsla)> {
        let (start, end) = (self.muted.start.min(len), self.muted.end.min(len));
        let runs = [(start, ink), (end - start, muted), (len - end, ink)];
        runs.into_iter().filter(|&(len, _)| len > 0).collect()
    }
}

const ELLIPSIS: char = '…';

/// `text` shaped by `shape`, or, if wider than `avail`, its longest prefix that
/// fits `avail` with an [`ELLIPSIS`] appended, down to none at all.
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
    if f32::from(line.width()) <= avail {
        return line;
    }
    let chars = text.chars().count();
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
    let mut keep = placed;
    let mut best = ellipsized(keep);
    if fits(&best) {
        while keep < chars {
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
/// flame-graph node. The muted part of the name is set in the muted ink of
/// the fill as it is seen opaque, faded as far as `label_color` is.
pub fn paint_labeled_box<'a>(
    rect: Bounds<Pixels>,
    fill_color: Hsla,
    name: impl Into<Inked<'a>>,
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
    let name = name.into();
    let opaque = Hsla {
        a: 1.0,
        ..fill_color
    };
    let muted = Hsla {
        a: label_color.a,
        ..bviz_ui_theme::muted_label_on(opaque)
    };
    let line = truncated(name.text, avail, |s| {
        let runs = name.runs(s.len(), label_color, muted);
        bviz_ui_assets::shape_runs(window, s, font.clone(), fs, &runs)
    });
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

    /// The longest prefix whose ellipsized width fits, found by shaping every
    /// prefix: the definition [`truncated`] must agree with.
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
        let mut ellipsized = |keep: usize| {
            let mut s: String = chars[..keep].iter().collect();
            s.push(ELLIPSIS);
            shape(s)
        };
        let longest = (0..=chars.len())
            .rev()
            .find(|&keep| f32::from(ellipsized(keep).width()) <= avail)
            .unwrap_or(0);
        ellipsized(longest)
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
    fn truncates_to_the_longest_fitting_prefix() {
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
