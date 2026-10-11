mod fit;

use crate::button::ICON_SIZE;
use crate::menu::{Menu, MenuEvent, MenuItem, over_window};
use crate::{Button, CONTROL_HEIGHT, spinner};
use fit::{Fitted, Kind, Measure, Room, Side, Spot};
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, ContentMask, Div, ElementId, Empty, Entity,
    FocusHandle, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, RenderOnce,
    SharedString, Size, Style, Subscription, Window, div, point, prelude::*, px, relative, size,
};
use std::sync::Arc;

pub use fit::Priority;

/// Space the strip leaves above and below its controls.
const PADDING: Pixels = px(4.);

/// Room between neighbouring controls of a strip.
const GAP: Pixels = px(8.);

/// The rule drawn along the strip's foot.
const RULE: Pixels = px(1.);

/// How much of a control's height a [`ToolbarSeparator`] spans, short of the
/// whole so it parts the groups beside it without boxing them in.
const SEPARATOR_SPAN: f32 = 0.6;

/// The icon of a [`OneRow`]'s overflow menu trigger, the one every menu of
/// further controls is opened by.
const OVERFLOW_ICON: &str = "dots-three-vertical";

/// Size of the spinner cueing work in flight.
const BUSY_SPINNER: Pixels = px(14.);

/// Diameter of the dot marking an overflow menu trigger whose menu holds a
/// control standing other than at rest.
const BADGE: Pixels = px(6.);

/// How far the dot stands in from the trigger's corner, centring it on the
/// corner of the trigger's icon so that it reads as marking the icon.
fn badge_inset() -> Pixels {
    (CONTROL_HEIGHT - ICON_SIZE - BADGE) / 2.
}

/// A horizontal strip of controls acting on the content below it, wrapping
/// onto as many rows as it takes to hold them.
#[derive(IntoElement, Default)]
pub struct Toolbar {
    leading: Vec<AnyElement>,
    trailing: Vec<AnyElement>,
}

impl Toolbar {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add to the group held against the left edge, which wraps so that a
    /// control is never cut off at the window edge however narrow the window.
    pub fn child(mut self, child: impl IntoElement) -> Self {
        self.leading.push(child.into_any_element());
        self
    }

    /// Add to the group held against the right edge, which keeps its room
    /// whatever the leading group holds.
    pub fn trailing(mut self, child: impl IntoElement) -> Self {
        self.trailing.push(child.into_any_element());
        self
    }
}

impl RenderOnce for Toolbar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let leading = group(self.leading).flex_wrap().flex_1().min_w_0();
        strip(window, cx)
            .items_start()
            .child(leading)
            .child(group(self.trailing).flex_none())
    }
}

/// A group of a [`OneRow`]'s controls, standing in the narrowest of its
/// presentations the row's width calls for.
pub struct ToolbarGroup {
    priority: Priority,
    widest: AnyElement,
    narrower: Vec<AnyElement>,
    overflow: Option<Overflow>,
    parted: bool,
}

/// How a [`ToolbarGroup`] stands in its row's overflow menu.
struct Overflow {
    item: MenuItem,
    /// Whether the group stands other than at rest.
    set: bool,
}

impl ToolbarGroup {
    /// A group standing as `widest` while the row has room for it, holding
    /// out against the row narrowing by `priority`.
    pub fn new(priority: Priority, widest: impl IntoElement) -> Self {
        Self {
            priority,
            widest: widest.into_any_element(),
            narrower: Vec::new(),
            overflow: None,
            parted: false,
        }
    }

    /// A group standing as what `build` builds beside the word `name`, then
    /// without it, holding out by `priority`.
    pub fn named<E: IntoElement>(
        priority: Priority,
        name: impl Into<SharedString>,
        mut build: impl FnMut() -> E,
    ) -> Self {
        Self::new(priority, beside().child(name.into()).child(build())).or(beside().child(build()))
    }

    /// Offers `narrower` for the group to stand as once the row has no room
    /// for the presentations offered before it.
    pub fn or(mut self, narrower: impl IntoElement) -> Self {
        self.narrower.push(narrower.into_any_element());
        self
    }

    /// Lets the group leave the row for its overflow menu once it stands at
    /// its narrowest, as the menu row `item`; `set` says the group stands
    /// other than at rest, which marks the menu's trigger while it is there.
    pub fn overflow(mut self, item: MenuItem, set: bool) -> Self {
        self.overflow = Some(Overflow { item, set });
        self
    }

    /// Parts the group by a [`ToolbarSeparator`] from the controls standing
    /// before it, in whichever presentation it stands.
    pub fn parted(mut self) -> Self {
        self.parted = true;
        self
    }
}

/// A strip of controls held to one row, [`height`](Self::height) tall, so that
/// what it holds never moves the content below it.
///
/// Where the row is too narrow for its controls, its groups give way by
/// [`Priority`], narrowing through the presentations they offer and then
/// moving into a menu behind a trigger ending the left-hand controls.
#[derive(IntoElement)]
pub struct OneRow {
    id: ElementId,
    leading: Vec<Item>,
    trailing: Vec<Item>,
    busy: Option<Busy>,
    text_size: Option<Pixels>,
}

/// Work in flight a [`OneRow`] cues.
struct Busy {
    /// Keys the cue's spinner.
    id: ElementId,
    doing: SharedString,
}

/// What a [`OneRow`] lays out side by side.
enum Item {
    Group(ToolbarGroup),
    Filling(Filling),
}

/// The element a [`OneRow`] fills its room with, built twice: once to measure
/// its least width, once to stand. Text cut short to fit keeps the cut of the
/// last layout to measure it, and a second layout of one element in a frame
/// may reuse a size the first measured without measuring again, so one
/// element measured and then laid out could stand cut to its least width.
struct Filling {
    measured: AnyElement,
    standing: AnyElement,
}

impl OneRow {
    /// A row keyed by `id`, which keeps its overflow menu across frames.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            leading: Vec::new(),
            trailing: Vec::new(),
            busy: None,
            text_size: None,
        }
    }

    /// Sets everything the row reads in `size`, for a view whose text runs
    /// apart from the window's.
    pub fn text_size(mut self, size: Pixels) -> Self {
        self.text_size = Some(size);
        self
    }

    /// The strip's height, so a layout can reserve its room without building it.
    pub fn height() -> Pixels {
        CONTROL_HEIGHT + PADDING * 2. + RULE
    }

    /// Adds `group` to those held against the left edge.
    pub fn group(mut self, group: ToolbarGroup) -> Self {
        self.leading.push(Item::Group(group));
        self
    }

    /// Adds the element `build` builds to the left-hand controls, laid out
    /// across the room the row's groups leave. It holds its min-content width
    /// while any group stands in the row, every group giving way first, and
    /// never leaves the row: narrower still, it takes the whole of it.
    pub fn filling<E: IntoElement>(mut self, mut build: impl FnMut() -> E) -> Self {
        self.leading.push(Item::Filling(Filling {
            measured: build().into_any_element(),
            standing: build().into_any_element(),
        }));
        self
    }

    /// Adds `group` to those held against the right edge.
    pub fn trailing(mut self, group: ToolbarGroup) -> Self {
        self.trailing.push(Item::Group(group));
        self
    }

    /// A cue that the work `doing` names is in flight, its spinner keyed by
    /// `id`: the spinner beside the words, or alone where the room the row's
    /// controls leave is too narrow for them, against the right-hand groups,
    /// so that it never moves a control as it comes and goes.
    pub fn busy(mut self, id: impl Into<ElementId>, doing: impl Into<SharedString>) -> Self {
        self.busy = Some(Busy {
            id: id.into(),
            doing: doing.into(),
        });
        self
    }
}

impl RenderOnce for OneRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let menu = window.use_keyed_state(
            ElementId::NamedChild(Arc::new(self.id.clone()), "overflow".into()),
            cx,
            |_, _| OverflowMenu::default(),
        );
        let open = (menu.read(cx).open.as_ref()).map(|open| over_window(&open.menu, window));
        let colors = bviz_ui_theme::theme(window, cx).colors;
        let trigger = |badged: bool| {
            let menu = menu.clone();
            div()
                .relative()
                .child(
                    Button::new("overflow-trigger", move |event, window, cx| {
                        let from = window.focused(cx);
                        let items = menu.update(cx, |menu, _| std::mem::take(&mut menu.items));
                        let opened = cx.new(|cx| Menu::new(items, event.position(), window, cx));
                        menu.update(cx, |menu, cx| menu.put_up(opened, from, window, cx));
                    })
                    .ghost()
                    .icon(OVERFLOW_ICON)
                    .tooltip("More controls"),
                )
                .when(badged, |trigger| {
                    trigger.child(
                        div()
                            .absolute()
                            .top(badge_inset())
                            .right(badge_inset())
                            .size(BADGE)
                            .rounded_full()
                            .bg(colors.accent),
                    )
                })
                .into_any_element()
        };
        let status = match self.busy {
            Some(Busy { id, doing }) => {
                let spinner = || spinner(id.clone(), BUSY_SPINNER, colors.text);
                vec![
                    beside().child(spinner()).child(doing).into_any_element(),
                    spinner().into_any_element(),
                ]
            }
            None => Vec::new(),
        };
        let fitting = Fitting {
            id: self.id,
            leading: self.leading,
            trailing: self.trailing,
            status,
            trigger: Trigger {
                unmarked: trigger(false),
                marked: trigger(true),
            },
            menu,
        };
        strip(window, cx)
            .when_some(self.text_size, Div::text_size)
            .items_center()
            .h(Self::height())
            .child(fitting)
            .children(open)
    }
}

/// The overflow menu of a [`OneRow`], kept across the frames the row is
/// rebuilt in.
#[derive(Default)]
struct OverflowMenu {
    /// The rows of the groups the last laid out frame put in the menu, which
    /// its trigger opens on.
    items: Vec<MenuItem>,
    open: Option<OpenMenu>,
}

struct OpenMenu {
    menu: Entity<Menu>,
    _dismissed: Subscription,
}

impl OverflowMenu {
    /// Holds `menu` up until it is dismissed, handing focus back to `from`
    /// where the menu still holds it then.
    fn put_up(
        &mut self,
        menu: Entity<Menu>,
        from: Option<FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dismissed = cx.subscribe_in(&menu, window, move |this, _, event, window, cx| {
            let MenuEvent::Dismissed { holding_focus } = event;
            this.open = None;
            if let Some(from) = from.as_ref().filter(|_| *holding_focus) {
                window.focus(from, cx);
            }
            cx.notify();
        });
        self.open = Some(OpenMenu {
            menu,
            _dismissed: dismissed,
        });
        cx.notify();
    }
}

/// The controls of a [`OneRow`], laid out once the row's width is known.
struct Fitting {
    id: ElementId,
    leading: Vec<Item>,
    trailing: Vec<Item>,
    status: Vec<AnyElement>,
    trigger: Trigger,
    menu: Entity<OverflowMenu>,
}

/// The overflow menu's trigger, in both the looks it can stand in.
struct Trigger {
    unmarked: AnyElement,
    /// Marked as holding a group standing other than at rest.
    marked: AnyElement,
}

impl IntoElement for Fitting {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

/// An item as the fit reads it: the presentations it may stand in, measured.
struct Measured {
    widest: (AnyElement, Size<Pixels>),
    narrower: Vec<(AnyElement, Size<Pixels>)>,
    overflow: Option<Overflow>,
    fills: bool,
}

/// The space a control is measured in: as wide as it asks, as tall as a
/// control.
fn natural() -> Size<AvailableSpace> {
    size(
        AvailableSpace::MaxContent,
        AvailableSpace::Definite(CONTROL_HEIGHT),
    )
}

impl Measured {
    fn of(item: Item, side: Side, window: &mut Window, cx: &mut App) -> (Self, Measure) {
        match item {
            Item::Group(group) => {
                let mut measured = |mut element: AnyElement| {
                    let extent = element.layout_as_root(natural(), window, cx);
                    (element, extent)
                };
                let widest = measured(group.widest);
                let narrower: Vec<_> = group.narrower.into_iter().map(measured).collect();
                let measure = Measure {
                    kind: Kind::Group {
                        widths: (std::iter::once(&widest).chain(&narrower))
                            .map(|(_, extent)| extent.width)
                            .collect(),
                        priority: group.priority,
                        overflows: group.overflow.is_some(),
                        parted: group.parted,
                    },
                    side,
                };
                let measured = Measured {
                    widest,
                    narrower,
                    overflow: group.overflow,
                    fills: false,
                };
                (measured, measure)
            }
            Item::Filling(Filling { measured, standing }) => {
                let held = |element: AnyElement| {
                    div()
                        .w_full()
                        .h(CONTROL_HEIGHT)
                        .flex()
                        .items_center()
                        .child(element)
                        .into_any_element()
                };
                let least = size(
                    AvailableSpace::MinContent,
                    AvailableSpace::Definite(CONTROL_HEIGHT),
                );
                let extent = held(measured).layout_as_root(least, window, cx);
                let measure = Measure {
                    kind: Kind::Filling {
                        least: extent.width,
                    },
                    side,
                };
                let measured = Measured {
                    widest: (held(standing), extent),
                    narrower: Vec::new(),
                    overflow: None,
                    fills: true,
                };
                (measured, measure)
            }
        }
    }

    /// Lays the item out in its presentation at `stage`, or the narrowest short
    /// of it, its left edge at `x`, spanning `width` where it fills, centred
    /// across the height of `bounds`.
    fn prepaint(
        self,
        stage: usize,
        x: Pixels,
        width: Pixels,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let mut presentation = self.widest;
        for narrower in self.narrower.into_iter().take(stage) {
            presentation = narrower;
        }
        let (mut element, extent) = presentation;
        let space = match self.fills {
            true => size(
                AvailableSpace::Definite(width),
                AvailableSpace::Definite(CONTROL_HEIGHT),
            ),
            false => natural(),
        };
        element.prepaint_as_root(point(x, centred(bounds, extent.height)), space, window, cx);
        element
    }
}

/// The top of an element `height` tall centred across the height of `bounds`.
fn centred(bounds: Bounds<Pixels>, height: Pixels) -> Pixels {
    bounds.top() + (bounds.size.height - height) / 2.
}

/// Lays `element` out as wide as it asks, its left edge at `x`, centred across
/// the height of `bounds`.
fn prepaint_natural(
    mut element: AnyElement,
    x: Pixels,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let extent = element.layout_as_root(natural(), window, cx);
    element.prepaint_as_root(
        point(x, centred(bounds, extent.height)),
        natural(),
        window,
        cx,
    );
    element
}

impl Element for Fitting {
    type RequestLayoutState = ();
    type PrepaintState = Vec<AnyElement>;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
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
            flex_grow: 1.,
            flex_shrink: 1.,
            flex_basis: relative(0.).into(),
            size: size(relative(1.).into(), CONTROL_HEIGHT.into()),
            ..Style::default()
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
    ) -> Vec<AnyElement> {
        let leading = std::mem::take(&mut self.leading).into_iter();
        let trailing = std::mem::take(&mut self.trailing).into_iter();
        let items = (leading.map(|item| (item, Side::Leading)))
            .chain(trailing.map(|item| (item, Side::Trailing)));
        let (measured, measures): (Vec<Measured>, Vec<Measure>) = items
            .map(|(item, side)| Measured::of(item, side, window, cx))
            .unzip();
        let room = Room {
            width: bounds.size.width,
            gap: GAP,
            overflow: (self.trigger.unmarked)
                .layout_as_root(natural(), window, cx)
                .width,
            rule: RULE + GAP,
        };
        let Fitted {
            spots,
            trigger,
            free,
        } = fit::fit(&measures, room);
        let mut overflowed = Vec::new();
        let mut set = false;
        let statuses = std::mem::take(&mut self.status);
        let mask = ContentMask { bounds };
        let laid = window.with_content_mask(Some(mask), |window| {
            let mut laid = Vec::with_capacity(measured.len() + 2);
            for (item, spot) in measured.into_iter().zip(spots) {
                match spot {
                    Spot::Row {
                        stage,
                        x,
                        width,
                        ruled,
                    } => {
                        let mut x = bounds.left() + x;
                        let mut width = width;
                        if ruled {
                            let rule = ToolbarSeparator.into_any_element();
                            laid.push(prepaint_natural(rule, x, bounds, window, cx));
                            x += room.rule;
                            width -= room.rule;
                        }
                        laid.push(item.prepaint(stage, x, width, bounds, window, cx));
                    }
                    Spot::Menu => {
                        if let Some(overflow) = item.overflow {
                            set |= overflow.set;
                            overflowed.push(overflow.item);
                        }
                    }
                    Spot::Out => {}
                }
            }
            if let Some(x) = trigger {
                let chosen = match set {
                    true => &mut self.trigger.marked,
                    false => &mut self.trigger.unmarked,
                };
                let element = std::mem::replace(chosen, Empty.into_any_element());
                laid.push(prepaint_natural(
                    element,
                    bounds.left() + x,
                    bounds,
                    window,
                    cx,
                ));
            }
            let (free_start, free_end) = free;
            for mut status in statuses {
                let extent = status.layout_as_root(natural(), window, cx);
                if free_start + extent.width <= free_end {
                    let x = bounds.left() + free_end - extent.width;
                    laid.push(prepaint_natural(status, x, bounds, window, cx));
                    break;
                }
            }
            laid
        });
        self.menu.update(cx, |menu, _| menu.items = overflowed);
        laid
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        laid: &mut Vec<AnyElement>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let mask = ContentMask { bounds };
        window.with_content_mask(Some(mask), |window| {
            for element in laid {
                element.paint(window, cx);
            }
        });
    }
}

/// A rule parting one group of a strip's controls from the next.
#[derive(IntoElement)]
pub struct ToolbarSeparator;

impl RenderOnce for ToolbarSeparator {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = bviz_ui_theme::theme(window, cx).colors;
        div()
            .flex_none()
            .w(RULE)
            .h(CONTROL_HEIGHT * SEPARATOR_SPAN)
            .bg(colors.border)
    }
}

/// Controls laid side by side within one group.
fn beside() -> Div {
    div().flex().items_center().gap(GAP)
}

/// The strip its controls stand in, all set in one text size so that every
/// control of a row reads alike.
fn strip(window: &mut Window, cx: &mut App) -> Div {
    let colors = bviz_ui_theme::theme(window, cx).colors;
    div()
        .text_sm()
        .flex()
        .flex_row()
        .gap(GAP)
        // Hold the strip's height in a flex column, so a growing sibling
        // (the content it acts on) cannot squeeze the controls.
        .flex_none()
        .py(PADDING)
        .px_2()
        .bg(colors.surface)
        .text_color(colors.text)
        .border_b(RULE)
        .border_color(colors.border)
}

/// One group's controls, each row of it as tall as a control so the two groups
/// line up on the strip's first row.
fn group(items: Vec<AnyElement>) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(GAP)
        .min_h(CONTROL_HEIGHT)
        .children(items)
}
