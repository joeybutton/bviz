//! A menu of labelled rows standing over the window at the point it was asked
//! for, some of its rows opening a submenu beside them.

use crate::dropdown_chrome::{self, MENU_ICON, ROW_INSET, SHEET_BORDER};
use bviz_ui_theme::ThemeColors;
use gpui::{
    AnyWindowHandle, App, Bounds, Context, Div, EventEmitter, FocusHandle, IntoElement, Keystroke,
    MouseMoveEvent, Pixels, Point, Render, ScrollHandle, SharedString, Size, Stateful,
    Subscription, TextStyleRefinement, Window, div, point, prelude::*, px, size,
};
use std::rc::Rc;

/// Room above the first row of a list and below its last.
const LIST_PAD: Pixels = px(4.);

/// Room between a row's label and the mark at the end of it.
const LABEL_GAP: Pixels = px(16.);

/// The tallest a list stands before it scrolls, short enough that a list of
/// many rows leaves the window's own edges in view.
const MAX_LIST_HEIGHT: Pixels = px(420.);

/// What choosing a row does, run on the window the menu stands over.
type Chosen = Rc<dyn Fn(&mut Window, &mut App)>;

/// One row of a [`Menu`].
pub struct MenuItem {
    label: SharedString,
    opens: Opens,
}

enum Opens {
    Nothing(Chosen),
    Submenu(Vec<SubmenuItem>),
}

impl MenuItem {
    /// A row reading `label` that runs `chosen` and closes the menu.
    pub fn new(
        label: impl Into<SharedString>,
        chosen: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            opens: Opens::Nothing(Rc::new(chosen)),
        }
    }

    /// A row reading `label` that opens `items` beside itself.
    pub fn submenu(label: impl Into<SharedString>, items: Vec<SubmenuItem>) -> Self {
        Self {
            label: label.into(),
            opens: Opens::Submenu(items),
        }
    }
}

/// One row of the submenu a [`MenuItem::submenu`] row opens. A menu stands one
/// list deep beside its own, so a row of a submenu opens nothing further:
/// a third list would have neither a place to stand nor a way to be aimed at.
pub struct SubmenuItem {
    label: SharedString,
    chosen: Chosen,
}

impl SubmenuItem {
    /// A row reading `label` that runs `chosen` and closes the menu.
    pub fn new(
        label: impl Into<SharedString>,
        chosen: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            chosen: Rc::new(chosen),
        }
    }
}

/// Which row of a menu the cursor or the keyboard is on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Aim {
    /// No row: the menu has opened and nothing has been reached for yet.
    Away,
    /// A row of the menu itself. Where that row opens a submenu, the submenu
    /// stands open beside it with none of its own rows aimed at.
    Row(usize),
    /// A row of the submenu that the row at `under` opens.
    Under { under: usize, row: usize },
}

impl Aim {
    /// The row of the menu itself the aim is on, or the one it is under.
    fn on_menu(self) -> Option<usize> {
        match self {
            Aim::Away => None,
            Aim::Row(row) => Some(row),
            Aim::Under { under, .. } => Some(under),
        }
    }

    /// The row of the open submenu the aim is on.
    fn in_submenu(self) -> Option<usize> {
        match self {
            Aim::Under { row, .. } => Some(row),
            _ => None,
        }
    }
}

/// What a [`Menu`] tells the view that put it up.
pub enum MenuEvent {
    /// The menu has closed: it draws nothing further and is to be let go.
    /// `holding_focus` says whether focus was still the menu's own as it
    /// closed, in which case the holder is to place it somewhere that stands —
    /// a menu names rows that close panels and move them to other windows, so
    /// what the menu was opened over may be gone by the time it closes, and
    /// only the holder knows what is left. Where a chosen row took focus
    /// somewhere of its own it is `false`, and focus stays there.
    Dismissed { holding_focus: bool },
}

/// A menu standing over the whole window, drawn above everything else in it. It
/// takes the window's keystrokes while it is up, so the arrow keys walk its
/// rows and `escape` leaves them without anything under it answering; a choice,
/// `escape`, a press outside it, or the window going inactive closes the menu,
/// which then reports itself [`MenuEvent::Dismissed`].
pub struct Menu {
    items: Vec<MenuItem>,
    /// Where in the window the menu was asked for, which it opens from.
    at: Point<Pixels>,
    aim: Aim,
    /// Where the cursor was when it last moved over a row, which the next move
    /// is read against to tell a run at an open submenu from a change of aim.
    cursor: Option<Point<Pixels>>,
    /// Where the last frame laid the open submenu out, and `None` where that
    /// frame drew none.
    submenu_frame: Option<Bounds<Pixels>>,
    list_scroll: ScrollHandle,
    submenu_scroll: ScrollHandle,
    /// Whether a choice or a dismissal has been taken already. The menu goes on
    /// drawing and answering until the view holding it lets it go, which is a
    /// frame away, and every keystroke and click landing in between finds it
    /// still up.
    closed: bool,
    focus_handle: FocusHandle,
    /// What the menu is listening to while it stands, let go with it.
    _while_up: [Subscription; 3],
}

impl EventEmitter<MenuEvent> for Menu {}

impl Menu {
    /// A menu of `items` opening at `at` in the window, which it takes focus
    /// and keystrokes from while it is up.
    pub fn new(
        items: Vec<MenuItem>,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let dismiss = cx.on_focus_out(&focus_handle, window, |this, _, window, cx| {
            this.close(window, cx)
        });
        let deactivated = cx.observe_window_activation(window, |this, window, cx| {
            if !window.is_window_active() {
                this.close(window, cx);
            }
        });
        focus_handle.focus(window, cx);
        Self {
            items,
            at,
            aim: Aim::Away,
            cursor: None,
            submenu_frame: None,
            list_scroll: ScrollHandle::new(),
            submenu_scroll: ScrollHandle::new(),
            closed: false,
            focus_handle,
            _while_up: [dismiss, deactivated, intercept(window.window_handle(), cx)],
        }
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.closed = true;
        cx.emit(MenuEvent::Dismissed {
            holding_focus: self.focus_handle.is_focused(window),
        });
    }

    /// Aims at `aim`, which puts up the submenu of a row that opens one, takes
    /// down the one a row left behind had open, and brings the row aimed at
    /// into view where its list has scrolled away from it.
    fn aim_at(&mut self, aim: Aim, cx: &mut Context<Self>) {
        if self.aim == aim {
            return;
        }
        self.aim = aim;
        match aim {
            Aim::Away => {}
            Aim::Row(row) => self.list_scroll.scroll_to_item(row),
            Aim::Under { row, .. } => self.submenu_scroll.scroll_to_item(row),
        }
        cx.notify();
    }

    /// Aims at `aim` for a cursor that has reached `at`. A cursor travelling
    /// into an open submenu leaves the aim where it is: the run at the submenu
    /// crosses the rows lying between it and the row that opened it, and taking
    /// each row crossed would shut the submenu before the cursor arrived.
    fn aim_at_cursor(&mut self, aim: Aim, at: Point<Pixels>, cx: &mut Context<Self>) {
        let from = self.cursor.replace(at);
        if aim.on_menu() != self.aim.on_menu()
            && let (Some(from), Some(submenu)) = (from, self.submenu_frame)
            && travelling_into(from, at, submenu)
        {
            return;
        }
        self.aim_at(aim, cx);
    }

    /// Takes the row `aim` names: a row opening a submenu opens it and aims at
    /// its first row, and any other runs what it stands for and closes the menu.
    fn take(&mut self, aim: Aim, window: &mut Window, cx: &mut Context<Self>) {
        self.act(taken(aim, &self.shape()), window, cx);
    }

    /// Carries out what a keystroke or a click asked of the menu.
    fn act(&mut self, asked: Asked, window: &mut Window, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        match asked {
            Asked::At(aim) => self.aim_at(aim, cx),
            Asked::Close => self.close(window, cx),
            Asked::Run(aim) => {
                if let Some(chosen) = self.chosen(aim) {
                    chosen(window, cx);
                    self.close(window, cx);
                }
            }
        }
    }

    fn on_key(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        if keystroke.modifiers.modified() {
            return;
        }
        if let Some(asked) = asked_by(&keystroke.key, self.aim, &self.shape()) {
            self.act(asked, window, cx);
        }
    }

    /// How many rows each row of the menu opens beside itself, `None` where it
    /// opens none.
    fn shape(&self) -> Vec<Option<usize>> {
        self.items
            .iter()
            .map(|item| match &item.opens {
                Opens::Nothing(_) => None,
                Opens::Submenu(items) => Some(items.len()),
            })
            .collect()
    }

    /// What the row `aim` names runs, where it runs anything rather than
    /// opening a submenu.
    fn chosen(&self, aim: Aim) -> Option<Chosen> {
        match aim {
            Aim::Away => None,
            Aim::Row(row) => match &self.items.get(row)?.opens {
                Opens::Nothing(chosen) => Some(chosen.clone()),
                Opens::Submenu(_) => None,
            },
            Aim::Under { under, row } => Some(self.submenu(under)?.get(row)?.chosen.clone()),
        }
    }

    /// The rows the row at `under` opens beside itself, if it opens any.
    fn submenu(&self, under: usize) -> Option<&Vec<SubmenuItem>> {
        match &self.items.get(under)?.opens {
            Opens::Submenu(items) => Some(items),
            Opens::Nothing(_) => None,
        }
    }

    /// One of the menu's two lists, its rows laid out in `frame`.
    fn list(
        &self,
        rows: &[Laid<'_>],
        frame: Bounds<Pixels>,
        which: Which,
        colors: &ThemeColors,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let (id, scroll, aimed) = match which {
            Which::Menu => ("menu-list", &self.list_scroll, self.aim.on_menu()),
            Which::Submenu => ("menu-submenu", &self.submenu_scroll, self.aim.in_submenu()),
        };
        let laid = rows
            .iter()
            .enumerate()
            .map(|(row, laid)| self.row(laid, row, aimed == Some(row), colors, cx));
        let list = div()
            .id(id)
            .absolute()
            .left(frame.left())
            .top(frame.top())
            .w(frame.size.width)
            .h(frame.size.height)
            .flex()
            .flex_col()
            .py(LIST_PAD)
            .overflow_y_scroll()
            .track_scroll(scroll)
            .shadow_lg()
            // A press on a row is not also the press outside the menu that
            // closes it, which would take the row away before its release.
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .children(laid);
        dropdown_chrome::surface(list, colors)
    }

    /// The `row`th row of a list.
    fn row(
        &self,
        laid: &Laid<'_>,
        row: usize,
        aimed: bool,
        colors: &ThemeColors,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let aim = laid.aim;
        dropdown_chrome::aimed_row(("menu-row", row).into(), colors, aimed)
            .justify_between()
            .flex_none()
            .gap(LABEL_GAP)
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                this.aim_at_cursor(aim, event.position, cx);
            }))
            .on_click(cx.listener(move |this, _, window, cx| this.take(aim, window, cx)))
            .child(div().whitespace_nowrap().child(laid.label.clone()))
            .child(dropdown_chrome::submenu_mark(colors, laid.opens_submenu))
    }
}

/// One row of a list as a frame lays it out: how it reads, whether it opens a
/// submenu, and the aim that names it.
struct Laid<'a> {
    label: &'a SharedString,
    opens_submenu: bool,
    aim: Aim,
}

/// Which of a menu's two lists is being laid out.
#[derive(Clone, Copy)]
enum Which {
    /// The menu's own rows.
    Menu,
    /// The rows the row aimed at opens beside itself.
    Submenu,
}

/// Takes the keystrokes of the window `here` for the menu being built, so that
/// nothing under the menu answers one while the menu stands over it. The
/// keystrokes of every window of the application come through here, and a menu
/// takes only those of the window it stands over.
fn intercept(here: AnyWindowHandle, cx: &mut Context<Menu>) -> Subscription {
    let menu = cx.weak_entity();
    cx.intercept_keystrokes(move |event, window, cx| {
        if window.window_handle() != here {
            return;
        }
        cx.stop_propagation();
        let keystroke = event.keystroke.clone();
        menu.update(cx, |menu, cx| menu.on_key(&keystroke, window, cx))
            .ok();
    })
}

/// What a keystroke or a click asks of a menu.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Asked {
    /// Aim there.
    At(Aim),
    /// Run what the row aimed at stands for, and close over it.
    Run(Aim),
    Close,
}

/// What taking the row `aim` names comes to, `shape` giving how many rows each
/// row of the menu opens beside itself: a row opening a submenu opens it at its
/// first row, and any other runs what it stands for.
fn taken(aim: Aim, shape: &[Option<usize>]) -> Asked {
    match aim {
        Aim::Away => Asked::At(aim),
        Aim::Row(under) if opens(shape, under).is_some() => Asked::At(Aim::Under { under, row: 0 }),
        _ => Asked::Run(aim),
    }
}

/// What `key` asks of a menu aimed at `aim`, out of a menu of `shape`. `None`
/// for a key a menu makes nothing of.
fn asked_by(key: &str, aim: Aim, shape: &[Option<usize>]) -> Option<Asked> {
    Some(match key {
        "down" => Asked::At(walked(aim, 1, shape)),
        "up" => Asked::At(walked(aim, -1, shape)),
        "right" => match aim {
            Aim::Row(under) if opens(shape, under).is_some() => {
                Asked::At(Aim::Under { under, row: 0 })
            }
            _ => Asked::At(aim),
        },
        "left" => Asked::At(match aim {
            Aim::Under { under, .. } => Aim::Row(under),
            _ => aim,
        }),
        "enter" | "space" => taken(aim, shape),
        // A submenu is left for the row that opens it, and the menu itself is
        // closed, so the key that backs out of an open submenu is not also the
        // one that takes the whole menu down.
        "escape" => match aim {
            Aim::Under { under, .. } => Asked::At(Aim::Row(under)),
            _ => Asked::Close,
        },
        _ => return None,
    })
}

/// How many rows the row at `under` opens beside itself, where it opens any at
/// all. A row opening an empty list opens nothing to aim at.
fn opens(shape: &[Option<usize>], under: usize) -> Option<usize> {
    shape.get(under).copied().flatten().filter(|rows| *rows > 0)
}

/// The aim `by` steps from `aim` through the list it stands in, out of a menu
/// of `shape`. An aim with nowhere to step stays where it is.
fn walked(aim: Aim, by: isize, shape: &[Option<usize>]) -> Aim {
    match aim {
        Aim::Under { under, row } => {
            let rows = opens(shape, under).unwrap_or(0);
            stepped(Some(row), by, rows).map_or(aim, |row| Aim::Under { under, row })
        }
        Aim::Row(row) => stepped(Some(row), by, shape.len()).map_or(aim, Aim::Row),
        Aim::Away => stepped(None, by, shape.len()).map_or(aim, Aim::Row),
    }
}

/// The row `by` steps from `row` of a list of `rows` rows, wrapping at either
/// end; from no row at all, the first row stepping forward and the last
/// stepping back. `None` for a list with no rows to stand on.
fn stepped(row: Option<usize>, by: isize, rows: usize) -> Option<usize> {
    if rows == 0 {
        return None;
    }
    let from = match row {
        Some(row) => row as isize,
        // From no row at all the aim stands just off whichever end it is
        // stepping away from, so the first step lands on the first row going
        // forward and on the last going back.
        None => match by >= 0 {
            true => -1,
            false => rows as isize,
        },
    };
    Some((from + by).rem_euclid(rows as isize) as usize)
}

/// Whether a cursor that has moved from `from` to `at` is travelling into the
/// submenu standing in `submenu`: `at` falls inside the wedge that `from` sees
/// the submenu's near edge across.
fn travelling_into(from: Point<Pixels>, at: Point<Pixels>, submenu: Bounds<Pixels>) -> bool {
    let near = match submenu.left() >= from.x {
        true => submenu.left(),
        false => submenu.right(),
    };
    within(
        at,
        from,
        point(near, submenu.top()),
        point(near, submenu.bottom()),
    )
}

/// Whether `at` falls within the triangle `a`, `b`, `c`, an edge counting as
/// within it.
fn within(at: Point<Pixels>, a: Point<Pixels>, b: Point<Pixels>, c: Point<Pixels>) -> bool {
    let side = |from: Point<Pixels>, to: Point<Pixels>| {
        f32::from(to.x - from.x) * f32::from(at.y - from.y)
            - f32::from(to.y - from.y) * f32::from(at.x - from.x)
    };
    let sides = [side(a, b), side(b, c), side(c, a)];
    sides.iter().all(|side| *side >= 0.0) || sides.iter().all(|side| *side <= 0.0)
}

/// How wide the rows reading `labels` are laid out: room for the widest of
/// them in the menu's own face, for what a row keeps at its ends, and for the
/// mark a row opening a submenu carries.
fn list_width<'a>(
    labels: impl IntoIterator<Item = &'a SharedString>,
    window: &mut Window,
) -> Pixels {
    let widest = in_face(window, |window| {
        dropdown_chrome::widest(window, labels.into_iter().cloned())
    });
    widest + ROW_INSET * 2. + LABEL_GAP + MENU_ICON + SHEET_BORDER * 2.
}

/// How tall a list of `rows` rows each `row` tall is laid out in a window of
/// `viewport`: as tall as its rows, and no taller than it can stand and still
/// be reached whole, past which it scrolls and ends on a whole row.
fn list_height(rows: usize, row: Pixels, viewport: Size<Pixels>) -> Pixels {
    let frame = LIST_PAD * 2. + SHEET_BORDER * 2.;
    let room = MAX_LIST_HEIGHT.min(viewport.height) - frame;
    frame + (row * rows as f32).min(dropdown_chrome::whole_rows(row, room))
}

/// The face a menu's rows read in, set by the menu itself so they read in the
/// UI face whatever the menu is mounted under.
fn face() -> TextStyleRefinement {
    TextStyleRefinement {
        font_family: Some(bviz_ui_assets::UI.into()),
        ..Default::default()
    }
}

/// Runs `measure` under the face the menu's rows read in, so what it measures
/// is what they render at.
fn in_face<R>(window: &mut Window, measure: impl FnOnce(&mut Window) -> R) -> R {
    window.with_text_style(Some(face()), measure)
}

/// Where a list of `extent` opens for a menu asked at `from`: down and to the
/// right of it, flipped back across `back` on an axis where it would otherwise
/// hang over the window of `viewport`, and held against the window's near edge
/// where it fits neither way.
fn opened(
    from: Point<Pixels>,
    back: Point<Pixels>,
    extent: Size<Pixels>,
    viewport: Size<Pixels>,
) -> Bounds<Pixels> {
    let along = |from: Pixels, back: Pixels, extent: Pixels, room: Pixels| {
        let placed = match from + extent <= room {
            true => from,
            false => back - extent,
        };
        placed.clamp(px(0.), (room - extent).max(px(0.)))
    };
    Bounds::new(
        point(
            along(from.x, back.x, extent.width, viewport.width),
            along(from.y, back.y, extent.height, viewport.height),
        ),
        extent,
    )
}

/// Where the submenu the row at `row` opens is laid out: beside the list
/// `frame` lays that row out in, level with the row itself, each row of that
/// list standing `row_height` tall and `scrolled` giving how far the list has
/// carried its rows from where it starts.
fn beside(
    frame: Bounds<Pixels>,
    row: usize,
    row_height: Pixels,
    scrolled: Pixels,
    extent: Size<Pixels>,
    viewport: Size<Pixels>,
) -> Bounds<Pixels> {
    let top = frame.top() + SHEET_BORDER + LIST_PAD + row_height * row as f32 + scrolled;
    opened(
        point(frame.right(), top),
        point(frame.left(), top + row_height),
        extent,
        viewport,
    )
}

impl Render for Menu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = bviz_ui_theme::theme(window, cx).colors;
        let viewport = window.viewport_size();
        let row_height = in_face(window, |window| dropdown_chrome::row_height(window));
        let extent = size(
            list_width(self.items.iter().map(|item| &item.label), window),
            list_height(self.items.len(), row_height, viewport),
        );
        let frame = opened(self.at, self.at, extent, viewport);
        let open = self.aim.on_menu().and_then(|under| {
            let items = self.submenu(under)?;
            let extent = size(
                list_width(items.iter().map(|item| &item.label), window),
                list_height(items.len(), row_height, viewport),
            );
            let scrolled = self.list_scroll.offset().y;
            Some((
                under,
                beside(frame, under, row_height, scrolled, extent, viewport),
            ))
        });
        self.submenu_frame = open.map(|(_, beside)| beside);
        let rows: Vec<Laid<'_>> = self
            .items
            .iter()
            .enumerate()
            .map(|(row, item)| Laid {
                label: &item.label,
                opens_submenu: matches!(item.opens, Opens::Submenu(_)),
                aim: Aim::Row(row),
            })
            .collect();
        let submenu = open.and_then(|(under, beside)| {
            let rows: Vec<Laid<'_>> = self
                .submenu(under)?
                .iter()
                .enumerate()
                .map(|(row, item)| Laid {
                    label: &item.label,
                    opens_submenu: false,
                    aim: Aim::Under { under, row },
                })
                .collect();
            Some(self.list(&rows, beside, Which::Submenu, &colors, cx))
        });
        let mut menu = div();
        *menu.text_style() = face();
        menu.absolute()
            .inset_0()
            .occlude()
            .track_focus(&self.focus_handle)
            .on_any_mouse_down(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.close(window, cx);
            }))
            .child(self.list(&rows, frame, Which::Menu, &colors, cx))
            .children(submenu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How tall a row of the lists laid out here stands.
    const ROW: Pixels = px(24.);

    /// A row height that neither height a list is held to divides evenly.
    const UNEVEN_ROW: Pixels = px(34.);

    const WINDOW: Size<Pixels> = Size {
        width: px(1000.),
        height: px(800.),
    };

    const LIST: Size<Pixels> = Size {
        width: px(200.),
        height: px(300.),
    };

    /// A menu of three rows, the second of which opens a submenu of two.
    const SHAPE: [Option<usize>; 3] = [None, Some(2), None];

    /// Where a menu asked for at (`x`, `y`) of `WINDOW` opens a `LIST`.
    fn at(x: f32, y: f32) -> Bounds<Pixels> {
        let asked = point(px(x), px(y));
        opened(asked, asked, LIST, WINDOW)
    }

    /// A menu opens down and to the right of the point it was asked for, so it
    /// falls where the eye already is rather than over what was pressed.
    #[test]
    fn a_menu_opens_from_the_point_it_was_asked_for() {
        assert_eq!(at(120.0, 60.0).origin, point(px(120.), px(60.)));
    }

    /// A menu asked for near an edge opens back across the point instead of
    /// hanging over that edge, where the rows it holds could not be read or
    /// reached at all.
    #[test]
    fn a_menu_with_no_room_for_it_opens_back_across_the_point() {
        assert_eq!(at(900.0, 60.0).origin, point(px(700.), px(60.)));
        assert_eq!(at(120.0, 700.0).origin, point(px(120.), px(400.)));
        assert_eq!(at(900.0, 700.0).origin, point(px(700.), px(400.)));
    }

    /// A list ending exactly at the window's edge is inside it, so it opens
    /// where it was asked for rather than flipping back across the point for
    /// want of a pixel.
    #[test]
    fn a_list_ending_exactly_at_the_edge_opens_where_it_was_asked_for() {
        let exact = point(WINDOW.width - LIST.width, WINDOW.height - LIST.height);
        let opened = opened(exact, exact, LIST, WINDOW);
        assert_eq!(opened.origin, exact);
        assert_eq!(opened.right(), WINDOW.width);
        assert_eq!(opened.bottom(), WINDOW.height);
    }

    /// A list of many rows is held inside the window and scrolls, so every row
    /// it lists can be reached rather than standing past the window's edge.
    #[test]
    fn a_list_of_many_rows_is_held_inside_the_window() {
        let rows = 200;
        for viewport in [WINDOW, size(px(400.), px(160.))] {
            let extent = size(px(200.), list_height(rows, ROW, viewport));
            assert!(extent.height < ROW * rows as f32, "{viewport:?}");
            assert!(extent.height <= viewport.height, "{viewport:?}");
            let asked = point(px(10.), px(10.));
            let opened = opened(asked, asked, extent, viewport);
            assert!(opened.top() >= px(0.), "{viewport:?}");
            assert!(opened.bottom() <= viewport.height, "{viewport:?}");
        }
    }

    /// A list too long to stand whole scrolls within a height of whole rows,
    /// so its last row in view is shown entire rather than cut through.
    #[test]
    fn a_scrolling_list_ends_on_a_whole_row() {
        let frame = LIST_PAD * 2. + SHEET_BORDER * 2.;
        for row in [ROW, UNEVEN_ROW] {
            for viewport in [WINDOW, size(px(400.), px(160.))] {
                let height = list_height(200, row, viewport);
                let rows = f32::from(height - frame) / f32::from(row);
                assert_eq!(rows, rows.floor(), "{row:?} {viewport:?}");
                assert!(height <= viewport.height, "{row:?} {viewport:?}");
            }
        }
    }

    /// A submenu opens beside the row that opens it, level with that row, so
    /// the two read as one gesture rather than as a second menu somewhere else.
    #[test]
    fn a_submenu_opens_beside_the_row_that_opens_it() {
        let frame = at(100.0, 100.0);
        let beside = beside(frame, 2, ROW, px(0.), size(px(120.), px(80.)), WINDOW);
        assert_eq!(beside.left(), frame.right());
        assert_eq!(
            beside.top(),
            frame.top() + SHEET_BORDER + LIST_PAD + ROW * 2.
        );
    }

    /// A submenu with no room beside its list opens on the other side of it,
    /// rather than over the rows it was opened from.
    #[test]
    fn a_submenu_with_no_room_beside_its_list_opens_the_other_way() {
        let frame = at(780.0, 100.0);
        let beside = beside(frame, 0, ROW, px(0.), size(px(120.), px(80.)), WINDOW);
        assert_eq!(beside.right(), frame.left());
    }

    /// A submenu opened from a row near the window's bottom edge stands up from
    /// that row rather than down past the edge, so its last row is on screen.
    #[test]
    fn a_submenu_with_no_room_below_its_row_opens_upward() {
        let frame = at(100.0, 400.0);
        let extent = size(px(120.), px(300.));
        let beside = beside(frame, 5, ROW, px(0.), extent, WINDOW);
        let row = frame.top() + SHEET_BORDER + LIST_PAD + ROW * 5.;
        assert_eq!(beside.bottom(), row + ROW);
        assert!(beside.bottom() <= WINDOW.height);
    }

    /// A submenu too wide for the room on either side of its list is held
    /// inside the window, where the rows it holds can still be read.
    #[test]
    fn a_submenu_that_fits_on_neither_side_is_held_inside_the_window() {
        let frame = at(400.0, 100.0);
        let extent = size(WINDOW.width + px(200.), px(80.));
        let beside = beside(frame, 0, ROW, px(0.), extent, WINDOW);
        assert_eq!(beside.left(), px(0.));
    }

    /// A list scrolled away from its first row lays the submenu out beside the
    /// row as the frame draws it, not as an unscrolled list would.
    #[test]
    fn a_submenu_follows_the_row_its_scrolled_list_draws() {
        let frame = at(100.0, 100.0);
        let extent = size(px(120.), px(80.));
        let scrolled = px(-60.);
        assert_eq!(
            beside(frame, 4, ROW, scrolled, extent, WINDOW).top(),
            beside(frame, 4, ROW, px(0.), extent, WINDOW).top() + scrolled
        );
    }

    /// The arrow keys walk the rows of a list and wrap round its ends, and the
    /// first keystroke takes up a row from having none aimed at.
    #[test]
    fn walking_a_list_wraps_at_either_end() {
        assert_eq!(stepped(None, 1, 3), Some(0));
        assert_eq!(stepped(None, -1, 3), Some(2));
        assert_eq!(stepped(Some(1), 1, 3), Some(2));
        assert_eq!(stepped(Some(2), 1, 3), Some(0));
        assert_eq!(stepped(Some(0), -1, 3), Some(2));
    }

    /// A list with no rows has none to aim at, so the keys leave the aim where
    /// it is rather than landing it anywhere.
    #[test]
    fn a_list_with_no_rows_has_nothing_to_walk() {
        assert_eq!(stepped(None, 1, 0), None);
        assert_eq!(stepped(Some(0), 1, 0), None);
    }

    /// The arrow towards an open submenu opens the submenu of the row aimed at
    /// and aims at its first row, and leaves a row opening none where it is.
    #[test]
    fn the_arrow_into_a_submenu_opens_it() {
        assert_eq!(
            asked_by("right", Aim::Row(1), &SHAPE),
            Some(Asked::At(Aim::Under { under: 1, row: 0 }))
        );
        assert_eq!(
            asked_by("right", Aim::Row(0), &SHAPE),
            Some(Asked::At(Aim::Row(0)))
        );
    }

    /// Escape backs out of an open submenu to the row that opens it, and closes
    /// the menu only from a menu with no submenu open.
    #[test]
    fn escape_leaves_an_open_submenu_before_it_closes_the_menu() {
        assert_eq!(
            asked_by("escape", Aim::Under { under: 1, row: 1 }, &SHAPE),
            Some(Asked::At(Aim::Row(1)))
        );
        assert_eq!(asked_by("escape", Aim::Row(1), &SHAPE), Some(Asked::Close));
        assert_eq!(asked_by("escape", Aim::Away, &SHAPE), Some(Asked::Close));
    }

    /// Taking a row that opens a submenu opens it rather than choosing it: the
    /// row stands for the list it opens and for nothing of its own.
    #[test]
    fn taking_a_row_that_opens_a_submenu_opens_it() {
        for key in ["enter", "space"] {
            assert_eq!(
                asked_by(key, Aim::Row(1), &SHAPE),
                Some(Asked::At(Aim::Under { under: 1, row: 0 }))
            );
            assert_eq!(
                asked_by(key, Aim::Row(0), &SHAPE),
                Some(Asked::Run(Aim::Row(0)))
            );
            assert_eq!(
                asked_by(key, Aim::Under { under: 1, row: 1 }, &SHAPE),
                Some(Asked::Run(Aim::Under { under: 1, row: 1 }))
            );
            assert_eq!(asked_by(key, Aim::Away, &SHAPE), Some(Asked::At(Aim::Away)));
        }
    }

    /// The keys walk the list the aim stands in, which is the open submenu
    /// where the aim is in one rather than the menu it stands beside.
    #[test]
    fn the_keys_walk_the_list_the_aim_stands_in() {
        assert_eq!(
            asked_by("down", Aim::Under { under: 1, row: 1 }, &SHAPE),
            Some(Asked::At(Aim::Under { under: 1, row: 0 }))
        );
        assert_eq!(
            asked_by("down", Aim::Row(2), &SHAPE),
            Some(Asked::At(Aim::Row(0)))
        );
        assert_eq!(
            asked_by("up", Aim::Away, &SHAPE),
            Some(Asked::At(Aim::Row(2)))
        );
    }

    /// A key a menu makes nothing of leaves the menu as it stands; it is taken
    /// from the window all the same, nothing under an open menu answering.
    #[test]
    fn a_key_the_menu_makes_nothing_of_leaves_it_alone() {
        assert_eq!(asked_by("w", Aim::Row(1), &SHAPE), None);
        assert_eq!(asked_by("tab", Aim::Away, &SHAPE), None);
    }

    /// A row opening an empty list opens nothing to aim at, so taking it is
    /// neither a choice nor a list to walk.
    #[test]
    fn a_row_opening_an_empty_list_opens_nothing() {
        let shape = [Some(0)];
        assert_eq!(
            asked_by("right", Aim::Row(0), &shape),
            Some(Asked::At(Aim::Row(0)))
        );
        assert_eq!(
            asked_by("down", Aim::Under { under: 0, row: 0 }, &shape),
            Some(Asked::At(Aim::Under { under: 0, row: 0 }))
        );
    }

    /// A cursor run diagonally at an open submenu crosses the rows lying
    /// between it and the row that opened it; the aim holds while it travels,
    /// so the submenu is still there when the cursor arrives.
    #[test]
    fn a_run_at_an_open_submenu_holds_the_aim_it_crosses() {
        let submenu = Bounds::new(point(px(200.), px(100.)), size(px(120.), px(80.)));
        let from = point(px(150.), px(110.));
        assert!(travelling_into(from, point(px(180.), px(140.)), submenu));
        assert!(travelling_into(from, point(px(199.), px(175.)), submenu));
    }

    /// A cursor leaving a row for another without making for the open submenu
    /// takes the row it lands on, submenu or none.
    #[test]
    fn a_move_away_from_an_open_submenu_takes_the_row_it_lands_on() {
        let submenu = Bounds::new(point(px(200.), px(100.)), size(px(120.), px(80.)));
        let from = point(px(150.), px(110.));
        assert!(!travelling_into(from, point(px(150.), px(260.)), submenu));
        assert!(!travelling_into(from, point(px(150.), px(40.)), submenu));
        assert!(!travelling_into(from, point(px(120.), px(130.)), submenu));
    }

    /// A submenu opened on the far side of its list is travelled into the same
    /// way, the wedge reaching for whichever edge of it faces the cursor.
    #[test]
    fn a_run_at_a_submenu_on_the_other_side_holds_the_aim_too() {
        let submenu = Bounds::new(point(px(30.), px(100.)), size(px(120.), px(80.)));
        let from = point(px(200.), px(110.));
        assert!(travelling_into(from, point(px(170.), px(140.)), submenu));
        assert!(!travelling_into(from, point(px(240.), px(140.)), submenu));
    }
}
