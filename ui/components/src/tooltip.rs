use bviz_ui_theme::TooltipColors;
use gpui::{
    AnyView, App, AppContext, AvailableSpace, BorderStyle, Bounds, Context, Decorations, Element,
    ElementId, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, ParentElement,
    Pixels, Point, Render, ShapedLine, SharedString, Size, Style, Styled, TextAlign, Window, div,
    font, point, px, quad, size,
};
use std::rc::Rc;

const TOOLTIP_TEXT_SIZE: f32 = 12.0;
/// Inset of the text block from the panel's edges.
const TOOLTIP_PAD: f32 = 6.0;
/// Leading between the panel's lines.
const TOOLTIP_LINE_GAP: f32 = 4.0;
const TOOLTIP_LINE_HEIGHT: f32 = TOOLTIP_TEXT_SIZE + TOOLTIP_LINE_GAP;
/// Widest a line of the panel's text runs before it wraps, a measure that
/// reads in one sweep of the eye.
const TOOLTIP_MAX_WIDTH: f32 = 480.0;
/// How far from its anchor the panel is placed along each axis, clear of it.
const TOOLTIP_OFFSET: f32 = 12.0;
/// Room between a tooltip's text and the keys named beside it.
const TOOLTIP_KEYS_GAP: f32 = 12.0;
/// Corner radius of the panel.
const TOOLTIP_RADIUS: f32 = 4.0;
/// Width of the panel's border.
const TOOLTIP_BORDER: f32 = 1.0;

/// Where a box `extent` long starts along one axis for an anchor at `at`:
/// past the anchor by [`TOOLTIP_OFFSET`], or as far before it where that runs
/// past the end of `inner`; failing both, likewise within `outer`, which holds
/// `inner`; slid inside `outer` where neither side holds it there either.
fn beside(at: f32, extent: f32, inner: (f32, f32), outer: (f32, f32)) -> f32 {
    let after = at + TOOLTIP_OFFSET;
    let before = at - TOOLTIP_OFFSET - extent;
    for (start, end) in [inner, outer] {
        if after + extent <= end {
            return after;
        }
        if before >= start {
            return before;
        }
    }
    let (start, end) = outer;
    (end - extent).max(start)
}

/// The span of `bounds` along the axis `along` reads.
fn span(bounds: Bounds<Pixels>, along: fn(Point<Pixels>) -> Pixels) -> (f32, f32) {
    (
        f32::from(along(bounds.origin)),
        f32::from(along(bounds.bottom_right())),
    )
}

/// Where a box of `extent` beside an anchor at `at` goes: as
/// [`beside_anchor`] places it inside `inner`, and inside `outer`, which
/// holds `inner`, along an axis where `inner` holds it on neither side.
fn beside_within(
    at: Point<Pixels>,
    extent: Size<Pixels>,
    inner: Bounds<Pixels>,
    outer: Bounds<Pixels>,
) -> Point<Pixels> {
    let along = |at: Pixels, extent: Pixels, axis: fn(Point<Pixels>) -> Pixels| {
        let (inner, outer) = (span(inner, axis), span(outer, axis));
        px(beside(f32::from(at), f32::from(extent), inner, outer))
    };
    point(
        along(at.x, extent.width, |p| p.x),
        along(at.y, extent.height, |p| p.y),
    )
}

/// Where a box of `extent` beside an anchor at `at` goes inside `bounds`: on
/// each axis past the anchor by [`TOOLTIP_OFFSET`], or as far before it where
/// that runs past the far edge, so the box covers nothing at the anchor; slid
/// inside only where neither side holds it.
pub fn beside_anchor(
    at: Point<Pixels>,
    extent: Size<Pixels>,
    bounds: Bounds<Pixels>,
) -> Point<Pixels> {
    beside_within(at, extent, bounds, bounds)
}

/// Where a box of `extent` describing the item at `rect` goes: beside the
/// middle of the item's bottom edge, or of its top edge where it would cover
/// the item there, placed as [`beside_within`] places it.
fn beside_item(
    rect: Bounds<Pixels>,
    extent: Size<Pixels>,
    inner: Bounds<Pixels>,
    outer: Bounds<Pixels>,
) -> Point<Pixels> {
    let x = rect.center().x;
    let below = beside_within(point(x, rect.bottom()), extent, inner, outer);
    match below.y >= rect.bottom() {
        true => below,
        false => beside_within(point(x, rect.top()), extent, inner, outer),
    }
}

/// What a tooltip panel stands beside.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CardAnchor {
    /// The pointer, which the panel follows within the item it describes.
    Pointer(Point<Pixels>),
    /// The item the panel describes, which the panel stands clear of wherever
    /// the pointer is.
    Item(Bounds<Pixels>),
}

impl CardAnchor {
    /// Where a panel of `extent` goes beside the anchor: inside `panel` where
    /// that holds it clear of the anchor, and otherwise inside `frame`, which
    /// holds `panel`.
    pub(crate) fn place(
        self,
        extent: Size<Pixels>,
        panel: Bounds<Pixels>,
        frame: Bounds<Pixels>,
    ) -> Point<Pixels> {
        match self {
            CardAnchor::Pointer(at) => beside_within(at, extent, panel, frame),
            CardAnchor::Item(rect) => beside_item(rect, extent, panel, frame),
        }
    }
}

/// The largest extent [`beside_anchor`] places clear of an anchor at `at`
/// inside `bounds`, on whichever side of it holds more on each axis.
pub fn room_beside(at: Point<Pixels>, bounds: Bounds<Pixels>) -> Size<Pixels> {
    let along = |at: Pixels, start: Pixels, end: Pixels| {
        ((end - at).max(at - start) - px(TOOLTIP_OFFSET)).max(px(0.))
    };
    size(
        along(at.x, bounds.left(), bounds.right()),
        along(at.y, bounds.top(), bounds.bottom()),
    )
}

/// The part of the window its content is drawn in: inside the room a
/// client-decorated window keeps for its shadow on each edge not tiled
/// against the screen.
fn window_frame(window: &Window) -> Bounds<Pixels> {
    let whole = Bounds::new(Point::default(), window.viewport_size());
    let (Decorations::Client { tiling }, Some(inset)) =
        (window.window_decorations(), window.client_inset())
    else {
        return whole;
    };
    let edge = |tiled: bool| if tiled { px(0.) } else { inset };
    let (top, left) = (edge(tiling.top), edge(tiling.left));
    let (bottom, right) = (edge(tiling.bottom), edge(tiling.right));
    Bounds::from_corners(
        whole.origin + point(left, top),
        whole.bottom_right() - point(right, bottom),
    )
}

/// The widest a tooltip panel's text runs inside `frame`: no wider than
/// [`TOOLTIP_MAX_WIDTH`], nor than the frame holds with the panel's padding
/// and the `beside` the text stands next to.
fn text_room(frame: Bounds<Pixels>, beside: Pixels) -> Pixels {
    let held = frame.size.width - px(2.0 * (TOOLTIP_PAD + TOOLTIP_BORDER)) - beside;
    held.min(px(TOOLTIP_MAX_WIDTH)).max(px(TOOLTIP_TEXT_SIZE))
}

/// The rows of `text` as a tooltip panel reads them: in the UI face whatever
/// the face of the panel it is painted over, wrapped at `width`.
fn shape_rows(text: &str, color: Hsla, width: Pixels, window: &mut Window) -> Vec<ShapedLine> {
    bviz_ui_assets::shape_wrapped(
        window,
        text,
        font(bviz_ui_assets::UI),
        px(TOOLTIP_TEXT_SIZE),
        color,
        width,
    )
}

/// Each of `lines` shaped as [`shape_rows`] shapes it.
fn shape_lines(
    lines: &[String],
    color: Hsla,
    width: Pixels,
    window: &mut Window,
) -> Vec<ShapedLine> {
    lines
        .iter()
        .flat_map(|line| shape_rows(line, color, width, window))
        .collect()
}

/// The extent of the padded panel around `rows`.
fn panel_extent(rows: &[ShapedLine]) -> Size<Pixels> {
    let text_w = rows.iter().map(|row| row.width()).fold(px(0.), Pixels::max);
    size(
        text_w + px(2.0 * TOOLTIP_PAD),
        px(rows.len() as f32 * TOOLTIP_LINE_HEIGHT + 2.0 * TOOLTIP_PAD),
    )
}

/// The extent of a panel reading `lines` wrapped at [`TOOLTIP_MAX_WIDTH`].
pub fn tooltip_extent(lines: &[String], window: &mut Window) -> Size<Pixels> {
    let color = window.text_style().color;
    panel_extent(&shape_lines(lines, color, px(TOOLTIP_MAX_WIDTH), window))
}

/// Where a panel of the extent given goes inside the window frame given.
type Place = Box<dyn FnOnce(Size<Pixels>, Bounds<Pixels>) -> Point<Pixels>>;

/// The layer of deferred draws tooltip panels are painted in: over every
/// panel, beneath the menus.
const TOOLTIP_LAYER: usize = 0;

/// Shows `lines` in a tooltip panel painted over every panel of the window,
/// where `place` puts a panel of its extent inside the window's frame, and
/// held inside that frame. Called as the element showing it prepaints.
pub fn defer_tooltip(
    lines: impl Into<Rc<[String]>>,
    place: impl FnOnce(Size<Pixels>, Bounds<Pixels>) -> Point<Pixels> + 'static,
    colors: TooltipColors,
    window: &mut Window,
    cx: &mut App,
) {
    let floating = Floating {
        lines: lines.into(),
        place: Some(Box::new(place)),
        colors,
    };
    let mut element = floating.into_any_element();
    element.layout_as_root(AvailableSpace::min_size(), window, cx);
    window.defer_draw(element, Point::default(), TOOLTIP_LAYER, None);
}

/// The panel [`defer_tooltip`] shows, which takes no room of its own and
/// paints wherever it is placed.
struct Floating {
    lines: Rc<[String]>,
    place: Option<Place>,
    colors: TooltipColors,
}

impl IntoElement for Floating {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Floating {
    type RequestLayoutState = ();
    type PrepaintState = ();

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
        (window.request_layout(Style::default(), [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut Window,
        _: &mut App,
    ) {
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
        let Some(place) = self.place.take() else {
            return;
        };
        let frame = window_frame(window);
        let width = text_room(frame, px(0.));
        let shaped = shape_lines(&self.lines, self.colors.label.into(), width, window);
        let extent = panel_extent(&shaped);
        let origin = held_in(place(extent, frame), extent, frame);
        paint_panel(
            &shaped,
            Bounds::new(origin, extent),
            self.colors,
            window,
            cx,
        );
    }
}

/// `origin`, moved as little as holds a box of `extent` standing there inside
/// `frame`, or at the frame's top left corner where the frame is smaller.
fn held_in(origin: Point<Pixels>, extent: Size<Pixels>, frame: Bounds<Pixels>) -> Point<Pixels> {
    let along =
        |at: Pixels, extent: Pixels, start: Pixels, end: Pixels| at.min(end - extent).max(start);
    point(
        along(origin.x, extent.width, frame.left(), frame.right()),
        along(origin.y, extent.height, frame.top(), frame.bottom()),
    )
}

/// Paints the panel at `bounds` around the shaped `rows`.
fn paint_panel(
    rows: &[ShapedLine],
    bounds: Bounds<Pixels>,
    colors: TooltipColors,
    window: &mut Window,
    cx: &mut App,
) {
    window.paint_quad(quad(
        bounds,
        px(TOOLTIP_RADIUS),
        colors.panel,
        px(TOOLTIP_BORDER),
        Hsla::from(colors.border),
        BorderStyle::Solid,
    ));
    let line_height = px(TOOLTIP_LINE_HEIGHT);
    let mut top = bounds.origin + point(px(TOOLTIP_PAD), px(TOOLTIP_PAD));
    for row in rows {
        let _ = row.paint(top, line_height, TextAlign::Left, None, window, cx);
        top.y += line_height;
    }
}

/// Places each element's own tooltip as [`beside_anchor`] places a painted
/// one, [`TOOLTIP_OFFSET`] clear of the pointer.
pub fn place_tooltips(cx: &mut App) {
    cx.set_tooltip_offset(px(TOOLTIP_OFFSET));
}

/// Text in the panel [`defer_tooltip`] shows, as a view an element's own
/// tooltip can show. Each `\n` in it starts a line.
pub struct TextTooltip {
    text: SharedString,
    /// The keys that do what the text names, set beside its first line.
    keys: Option<SharedString>,
    colors: TooltipColors,
}

impl TextTooltip {
    /// Builds the tooltip showing `text`, for an element's `tooltip`.
    pub fn builder(
        text: SharedString,
        colors: TooltipColors,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        move |_, cx| Self::view(text.clone(), None, colors, cx)
    }

    /// The tooltip showing `text`, and `keys` beside it where given.
    pub fn view(
        text: SharedString,
        keys: Option<SharedString>,
        colors: TooltipColors,
        cx: &mut App,
    ) -> AnyView {
        cx.new(|_| TextTooltip { text, keys, colors }).into()
    }
}

impl Render for TextTooltip {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let keys = self.keys.as_ref().map_or(px(0.), |keys| {
            let size = px(TOOLTIP_TEXT_SIZE);
            let ui = font(bviz_ui_assets::UI);
            let shaped =
                bviz_ui_assets::shape_line(window, keys.clone(), ui, size, self.colors.keys.into());
            shaped.width() + px(TOOLTIP_KEYS_GAP)
        });
        let width = text_room(window_frame(window), keys);
        let rows = shape_rows(&self.text, self.colors.label.into(), width, window);
        // A tooltip view is drawn outside the element tree that sets the UI
        // face, so it inherits none.
        div()
            .font_family(bviz_ui_assets::UI)
            .p(px(TOOLTIP_PAD))
            .rounded(px(TOOLTIP_RADIUS))
            .border(px(TOOLTIP_BORDER))
            .border_color(self.colors.border)
            .bg(self.colors.panel)
            .text_size(px(TOOLTIP_TEXT_SIZE))
            .line_height(px(TOOLTIP_LINE_HEIGHT))
            .text_color(self.colors.label)
            .flex()
            .gap(px(TOOLTIP_KEYS_GAP))
            .whitespace_nowrap()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .children(rows.into_iter().map(|row| row.text)),
            )
            .children(
                self.keys
                    .clone()
                    .map(|keys| div().text_color(self.colors.keys).child(keys)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tooltip_flips_before_its_anchor_where_it_would_run_past_the_end() {
        let span = (0.0, 100.0);
        assert_eq!(beside(10.0, 50.0, span, span), 10.0 + TOOLTIP_OFFSET);
        assert_eq!(beside(90.0, 50.0, span, span), 90.0 - TOOLTIP_OFFSET - 50.0);
    }

    #[test]
    fn a_tooltip_neither_side_holds_is_slid_inside_the_span() {
        let span = (0.0, 100.0);
        assert_eq!(beside(50.0, 80.0, span, span), 20.0);
        assert_eq!(beside(50.0, 120.0, span, span), 0.0);
    }

    #[test]
    fn a_tooltip_leaves_the_inner_span_only_where_it_holds_it_on_neither_side() {
        let (inner, outer) = ((100.0, 200.0), (0.0, 400.0));
        assert_eq!(beside(120.0, 50.0, inner, outer), 120.0 + TOOLTIP_OFFSET);
        assert_eq!(
            beside(190.0, 50.0, inner, outer),
            190.0 - TOOLTIP_OFFSET - 50.0
        );
        assert_eq!(beside(150.0, 90.0, inner, outer), 150.0 + TOOLTIP_OFFSET);
        assert_eq!(beside(190.0, 90.0, inner, outer), 190.0 + TOOLTIP_OFFSET);
        assert_eq!(beside(200.0, 390.0, inner, outer), 10.0);
    }

    #[test]
    fn a_panel_is_held_inside_the_frame() {
        let frame = Bounds::new(point(px(10.), px(10.)), size(px(200.), px(100.)));
        let extent = size(px(80.), px(40.));
        let held = |x: f32, y: f32| held_in(point(px(x), px(y)), extent, frame);
        assert_eq!(held(50., 20.), point(px(50.), px(20.)));
        assert_eq!(held(180., 90.), point(px(130.), px(70.)));
        assert_eq!(held(-5., 0.), point(px(10.), px(10.)));
        let wide = size(px(300.), px(40.));
        assert_eq!(held_in(point(px(50.), px(20.)), wide, frame).x, px(10.));
    }

    #[test]
    fn a_tooltip_stands_below_its_item_or_above_where_there_is_no_room() {
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(400.), px(300.)));
        let extent = size(px(120.), px(60.));
        let high = Bounds::new(point(px(100.), px(20.)), size(px(80.), px(20.)));
        assert!(beside_item(high, extent, bounds, bounds).y >= high.bottom());
        let low = Bounds::new(point(px(100.), px(270.)), size(px(80.), px(20.)));
        let placed = beside_item(low, extent, bounds, bounds);
        assert!(placed.y + extent.height <= low.top(), "{placed:?}");
    }

    #[test]
    fn the_room_beside_an_anchor_is_held_clear_of_it() {
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(100.), px(60.)));
        for at in [point(px(10.), px(50.)), point(px(70.), px(20.))] {
            let room = room_beside(at, bounds);
            let placed = Bounds::new(beside_anchor(at, room, bounds), room);
            assert!(!placed.contains(&at), "{at:?} {placed:?}");
            assert!(placed.right() <= bounds.right() && placed.bottom() <= bounds.bottom());
            assert!(placed.left() >= bounds.left() && placed.top() >= bounds.top());
        }
    }
}
