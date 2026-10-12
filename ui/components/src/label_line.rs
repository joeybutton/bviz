//! A label set on one line of whatever width it is given.

use crate::Hint;
use bviz_label::shown::{Fitted, Parts, fit};
use bviz_ui_assets::{shape_line, text_metrics};
use gpui::{
    App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId, HighlightStyle, Hsla,
    InspectorElementId, IntoElement, LayoutId, Pixels, RenderOnce, ShapedLine, SharedString,
    StatefulInteractiveElement, Style, TextAlign, TextRun, Window, div, prelude::*, px, size,
};
use std::cell::Cell;
use std::rc::Rc;

/// `text`, a label as it is shown or text parted as a label is, on one line
/// in the ink it inherits, where it lives set in `muted`. It stands as wide
/// as it reads whole and narrows to whatever its line gives it, cut short as
/// [`fit`] cuts it.
pub struct LabelLine {
    text: SharedString,
    parts: Parts,
    muted: Hsla,
    /// Chars of `text` picked out, and how.
    picked: Option<(Vec<u32>, HighlightStyle)>,
    /// Set as the line is laid out to whether it reads less than `text`.
    cut: Option<Rc<Cell<bool>>>,
}

impl LabelLine {
    /// `label`, a label as it is shown; text that is no label is cut short at
    /// its end.
    pub fn new(label: impl Into<SharedString>, muted: Hsla) -> Self {
        let text = label.into();
        Self {
            parts: Parts::of_label(&text).unwrap_or_default(),
            text,
            muted,
            picked: None,
            cut: None,
        }
    }

    /// `text` parted as `parts` parts it.
    pub fn parted(text: impl Into<SharedString>, parts: Parts, muted: Hsla) -> Self {
        Self {
            text: text.into(),
            parts,
            muted,
            picked: None,
            cut: None,
        }
    }

    /// Picks out the chars `chars` of the text, ascending, in `style`,
    /// wherever they are kept.
    pub fn picked(mut self, chars: Vec<u32>, style: HighlightStyle) -> Self {
        self.picked = Some((chars, style));
        self
    }

    /// The line in a box keyed by `id`, whose hint reads `written`, the text
    /// as a reader writes it, wherever the line reads otherwise: cut short to
    /// fit, or shown shorter than it is written.
    pub fn hinted(
        self,
        id: impl Into<ElementId>,
        written: impl Into<SharedString>,
    ) -> HintedLabelLine {
        HintedLabelLine {
            id: id.into(),
            line: self,
            written: written.into(),
        }
    }

    /// `fitted` as runs of the font `font` in `ink`, its muted bytes in the
    /// muted ink, the chars picked out as they are picked.
    fn runs(&self, fitted: &Fitted, font: &gpui::Font, ink: Hsla) -> Vec<TextRun> {
        let picked: Vec<u32> = match &self.picked {
            Some((chars, _)) => chars.iter().filter_map(|&at| fitted.at(at)).collect(),
            None => Vec::new(),
        };
        let style = self.picked.as_ref().map(|(_, style)| *style);
        let muted = fitted.muted();
        let mut runs: Vec<TextRun> = Vec::with_capacity(picked.len() * 2 + INKS);
        for (at, (byte, c)) in (0u32..).zip(fitted.text().char_indices()) {
            let mut run = TextRun {
                len: c.len_utf8(),
                font: font.clone(),
                color: if muted.contains(&byte) {
                    self.muted
                } else {
                    ink
                },
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            if let Some(style) = style.filter(|_| picked.binary_search(&at).is_ok()) {
                run.color = style.color.unwrap_or(run.color);
                run.underline = style.underline;
            }
            match runs.last_mut() {
                Some(last) if last.color == run.color && last.underline == run.underline => {
                    last.len += run.len;
                }
                _ => runs.push(run),
            }
        }
        runs
    }
}

/// The inks a line is set in short of what is picked out: the muted and the
/// inherited.
const INKS: usize = 2;

impl IntoElement for LabelLine {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for LabelLine {
    type RequestLayoutState = ();
    type PrepaintState = ShapedLine;

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
        _: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.min_size.width = px(0.).into();
        // The measure runs once layout is computed, outside the text style
        // this element inherits, so it measures in the style taken here.
        let (text, metrics) = (self.text.clone(), text_metrics(window));
        let (ink, line_height) = (window.text_style().color, window.line_height());
        let layout = window.request_measured_layout(style, move |known, available, window, _| {
            let (font, font_size) = (metrics.font.clone(), metrics.font_size);
            let whole = shape_line(window, text.clone(), font, font_size, ink);
            let width = match available.width {
                AvailableSpace::Definite(room) => whole.width().min(room),
                AvailableSpace::MinContent => px(0.),
                AvailableSpace::MaxContent => whole.width(),
            };
            size(known.width.unwrap_or(width), line_height)
        });
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) -> ShapedLine {
        let metrics = text_metrics(window);
        let (font, font_size) = (metrics.font, metrics.font_size);
        let ink = window.text_style().color;
        let width = |text: &str| {
            let line = shape_line(window, text.to_owned(), font.clone(), font_size, ink);
            f32::from(line.width())
        };
        let fitted = fit(&self.text, self.parts, f32::from(bounds.size.width), width);
        if let Some(cut) = &self.cut {
            cut.set(fitted.text() != self.text.as_ref());
        }
        let runs = self.runs(&fitted, &font, ink);
        let text = SharedString::from(fitted.text().to_owned());
        window
            .text_system()
            .shape_line(text, font_size, &runs, None)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        line: &mut ShapedLine,
        window: &mut Window,
        cx: &mut App,
    ) {
        let line_height = window.line_height();
        line.paint(
            bounds.origin,
            line_height,
            TextAlign::Left,
            None,
            window,
            cx,
        )
        .ok();
    }
}

/// A [`LabelLine`] reading in full in a hint, made by [`LabelLine::hinted`].
#[derive(IntoElement)]
pub struct HintedLabelLine {
    id: ElementId,
    line: LabelLine,
    written: SharedString,
}

impl RenderOnce for HintedLabelLine {
    fn render(mut self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = bviz_ui_theme::theme(window, cx).colors.tooltip();
        let cut = Rc::new(Cell::new(false));
        self.line.cut = Some(Rc::clone(&cut));
        let reshown = self.written != self.line.text;
        let held = div().id(self.id).min_w_0();
        let held = match Hint::default().text(self.written).tooltip(colors) {
            // Read as the hint is built, after the frame laying the line out.
            Some(hint) => held.optional_tooltip(move |window, cx| {
                (reshown || cut.get()).then(|| hint(window, cx)).flatten()
            }),
            None => held,
        };
        held.child(self.line)
    }
}
