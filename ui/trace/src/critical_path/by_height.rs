//! An element choosing between two layouts by the height it is given.

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Style, Window, relative, size,
};

/// Fills its parent with `tall` where the parent gives it at least `threshold`
/// of height, and with `short` where it gives less. Only the one chosen is
/// laid out, so both may carry the same element ids.
pub(crate) struct ByHeight {
    threshold: Pixels,
    tall: Option<AnyElement>,
    short: Option<AnyElement>,
}

impl ByHeight {
    pub(crate) fn new(threshold: Pixels, tall: AnyElement, short: AnyElement) -> ByHeight {
        ByHeight {
            threshold,
            tall: Some(tall),
            short: Some(short),
        }
    }
}

impl IntoElement for ByHeight {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ByHeight {
    type RequestLayoutState = ();
    type PrepaintState = Option<AnyElement>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let style = Style {
            size: size(relative(1.).into(), relative(1.).into()),
            ..Default::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let chosen = match bounds.size.height >= self.threshold {
            true => self.tall.take(),
            false => self.short.take(),
        };
        let mut child = chosen?;
        let space = size(
            AvailableSpace::Definite(bounds.size.width),
            AvailableSpace::Definite(bounds.size.height),
        );
        child.prepaint_as_root(bounds.origin, space, window, cx);
        Some(child)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        child: &mut Option<AnyElement>,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(child) = child {
            child.paint(window, cx);
        }
    }
}
