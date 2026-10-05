//! An element shown whole or not at all.

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, Window,
};

/// `child`, laid out as it would be, but painted only where the region its
/// ancestors clip to holds all of its height: a line a short pane has no
/// room for is left out rather than cut through.
pub(crate) fn whole_or_none(child: impl IntoElement) -> WholeOrNone {
    WholeOrNone {
        child: child.into_any_element(),
    }
}

/// See [`whole_or_none`].
pub(crate) struct WholeOrNone {
    child: AnyElement,
}

impl IntoElement for WholeOrNone {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for WholeOrNone {
    type RequestLayoutState = ();
    /// Whether the child fits whole, and so is painted.
    type PrepaintState = bool;

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
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let shown = window.content_mask().bounds;
        let whole = bounds.top() >= shown.top() && bounds.bottom() <= shown.bottom();
        if whole {
            self.child.prepaint(window, cx);
        }
        whole
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        whole: &mut bool,
        window: &mut Window,
        cx: &mut App,
    ) {
        if *whole {
            self.child.paint(window, cx);
        }
    }
}
