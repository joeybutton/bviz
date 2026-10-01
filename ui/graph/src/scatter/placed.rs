//! An element standing where the bounds of the layer it overlays put it, which
//! are known only once that layer is laid out.

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Edges, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Position, Style, Window, px, relative, size,
};

/// Where a placed element goes within the bounds of the layer.
type Place = Box<dyn Fn(Bounds<Pixels>) -> Bounds<Pixels>>;

/// What is painted over a placed element, given where it went and the layer's
/// bounds.
type Over = Box<dyn FnOnce(Bounds<Pixels>, Bounds<Pixels>, &mut Window, &mut App)>;

/// `child` covering the layer it overlays at the rect `place` gives for the
/// layer's bounds, with `over` painted on top of it.
pub(super) struct Placed {
    child: AnyElement,
    place: Place,
    over: Option<Over>,
}

impl Placed {
    pub(super) fn new(
        child: impl IntoElement,
        place: impl Fn(Bounds<Pixels>) -> Bounds<Pixels> + 'static,
    ) -> Self {
        Self {
            child: child.into_any_element(),
            place: Box::new(place),
            over: None,
        }
    }

    pub(super) fn over(
        mut self,
        paint: impl FnOnce(Bounds<Pixels>, Bounds<Pixels>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.over = Some(Box::new(paint));
        self
    }
}

impl IntoElement for Placed {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Placed {
    type RequestLayoutState = ();
    type PrepaintState = Bounds<Pixels>;

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
            position: Position::Absolute,
            inset: Edges::all(px(0.).into()),
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
    ) -> Bounds<Pixels> {
        let rect = (self.place)(bounds);
        let space = size(
            AvailableSpace::Definite(rect.size.width),
            AvailableSpace::Definite(rect.size.height),
        );
        self.child.prepaint_as_root(rect.origin, space, window, cx);
        rect
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        rect: &mut Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
        if let Some(over) = self.over.take() {
            over(*rect, bounds, window, cx);
        }
    }
}
