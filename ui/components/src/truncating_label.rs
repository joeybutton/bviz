use crate::Hint;
use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, HighlightStyle,
    InspectorElementId, InteractiveElement, IntoElement, LayoutId, ParentElement, Pixels,
    Refineable, RenderOnce, SharedString, StatefulInteractiveElement, StyleRefinement, Styled,
    StyledText, TextLayout, Window, div,
};
use std::cell::Cell;
use std::ops::Range;
use std::panic::Location;
use std::rc::Rc;

/// A line of text cut short with an ellipsis where it runs wider than the room
/// it is laid out in, which reads in full in a hint only while it is cut short.
///
/// Its style refines the box the line is laid out in; that cuts the end of the
/// text unless it is given `text_ellipsis_start`.
#[derive(IntoElement)]
pub struct TruncatingLabel {
    id: ElementId,
    text: SharedString,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    style: StyleRefinement,
}

impl TruncatingLabel {
    pub fn new(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            highlights: Vec::new(),
            style: StyleRefinement::default(),
        }
    }

    /// Draws each byte range of the text in its style over the label's own.
    pub fn highlights(mut self, highlights: Vec<(Range<usize>, HighlightStyle)>) -> Self {
        self.highlights = highlights;
        self
    }
}

impl Styled for TruncatingLabel {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for TruncatingLabel {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = bviz_ui_theme::theme(window, cx).colors.tooltip();
        let line = StyledText::new(self.text.clone()).with_highlights(self.highlights);
        let shown = line.layout().clone();
        let clipped = Rc::new(Cell::new(false));
        // Named in full, as Element's own `id` is in scope too.
        let mut held = InteractiveElement::id(div(), self.id)
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis();
        if let Some(hint) = Hint::default().text(self.text.clone()).tooltip(colors) {
            let (shown, clipped, text) = (shown.clone(), Rc::clone(&clipped), self.text);
            // gpui ellipsizes a line by laying out a cut copy in its place.
            held = held.optional_tooltip(move |window, cx| {
                let cut = clipped.get() || shown.text() != *text;
                cut.then(|| hint(window, cx)).flatten()
            });
        }
        held.style().refine(&self.style);
        held.child(Measured {
            text: line.into_any_element(),
            shown,
            clipped,
        })
    }
}

/// A label's text, which records as it is laid out whether it runs past the
/// right edge of what its ancestors show.
struct Measured {
    text: AnyElement,
    shown: TextLayout,
    /// Read by the label's hint, which is built after the frame is laid out.
    clipped: Rc<Cell<bool>>,
}

impl Measured {
    fn is_clipped(&self, window: &Window) -> bool {
        let shown = window.content_mask().bounds.right();
        let end = self.shown.position_for_index(self.shown.len());
        // Floored as gpui's own ellipsis judges a line too wide for its room.
        end.is_some_and(|end| end.x.floor() > shown)
    }
}

impl IntoElement for Measured {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Measured {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.text.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.text.prepaint(window, cx);
        self.clipped.set(self.is_clipped(window));
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.text.paint(window, cx);
    }
}
