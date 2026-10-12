//! An element choosing between two layouts by the room it is given.

use crate::Axis;
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Style, Window, relative, size,
};

/// Fills its parent with `roomy` where the parent gives it at least
/// `threshold` along `axis`, and with `cramped` where it gives less. Only the
/// one chosen is laid out, so both may carry the same element ids.
pub struct ByRoom {
    axis: Axis,
    threshold: Pixels,
    roomy: Option<AnyElement>,
    cramped: Option<AnyElement>,
}

impl ByRoom {
    /// `tall` given at least `threshold` of height, else `short`.
    pub fn height(threshold: Pixels, tall: AnyElement, short: AnyElement) -> ByRoom {
        ByRoom::along(Axis::Vertical, threshold, tall, short)
    }

    /// `wide` given at least `threshold` of width, else `narrow`.
    pub fn width(threshold: Pixels, wide: AnyElement, narrow: AnyElement) -> ByRoom {
        ByRoom::along(Axis::Horizontal, threshold, wide, narrow)
    }

    fn along(axis: Axis, threshold: Pixels, roomy: AnyElement, cramped: AnyElement) -> ByRoom {
        ByRoom {
            axis,
            threshold,
            roomy: Some(roomy),
            cramped: Some(cramped),
        }
    }
}

impl IntoElement for ByRoom {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ByRoom {
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
        let room = match self.axis {
            Axis::Vertical => bounds.size.height,
            Axis::Horizontal => bounds.size.width,
        };
        let chosen = match room >= self.threshold {
            true => self.roomy.take(),
            false => self.cramped.take(),
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
