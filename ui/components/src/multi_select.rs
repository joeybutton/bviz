//! A multi-select: a trigger reading how many of its values are checked, and a
//! menu of rows that stays open as they are toggled.

use crate::button::Button;
use crate::drag::{DragEnd, track_drag};
use crate::layer::overlay_canvas;
use crate::menu;
use crate::scrollbar::{Axis, DEFAULT_MIN_THUMB, SCROLLBAR_THICKNESS, Scrollbar};
use bviz_ui_assets::TextMetrics;
use bviz_ui_theme::ThemeColors;
use gpui::{
    App, Bounds, ClickEvent, Context, ElementId, Entity, EventEmitter, FocusHandle, Font,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, Pixels, Point, Render, Rgba,
    ScrollHandle, SharedString, Subscription, Window, deferred, div, point, prelude::*, px, size,
};
use std::collections::HashSet;
use std::ops::Range;

/// How tall the menu's row list grows before it scrolls. A facet can carry
/// hundreds of values, so the list is bounded and the rest reached by scrolling.
const MENU_MAX_HEIGHT: Pixels = px(320.);

/// The share of the window's width a row's label runs to before it is cut short.
/// A canonical repository name can run far wider than anything else a menu
/// holds, and the menu is a transient sheet over the content it acts on.
const MENU_LABEL_MAX_SHARE: f32 = 0.5;

/// How many rows a group may run to — its own row and the values under it — and
/// still be listed when the menu is first stated over it, which is about as many
/// as the bounded list shows at once. A longer group has to be scrolled past
/// before the group after it can be read, so it opens folded.
const GROUP_LISTED_ROWS: usize = 12;

/// One value a [`MultiSelect`] offers.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MultiOption {
    pub label: SharedString,
    /// Whether the state behind the control has this value checked, as the owner
    /// of that state last stated it.
    pub checked: bool,
    /// Whether a click on the value's row reports anything. A value the loaded
    /// data cannot answer for is drawn greyed and reports nothing, so that it
    /// says it can do nothing rather than silently doing nothing.
    pub available: bool,
    /// Whether the value heads the values stated after it, up to the next that
    /// does: the menu offers such a run as a group, and acts on it as one.
    pub heads_group: bool,
}

impl MultiOption {
    /// A value the control offers.
    pub fn new(label: impl Into<SharedString>, checked: bool) -> Self {
        MultiOption {
            label: label.into(),
            checked,
            available: true,
            heads_group: false,
        }
    }

    /// The value as the head of the group of values stated after it.
    pub fn heading(mut self) -> Self {
        self.heads_group = true;
        self
    }
}

/// What a click on the menu asks of the state behind the values.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MultiSelectEvent {
    /// The value at `index` is asked to become `checked`.
    Toggled { index: usize, checked: bool },
    /// Every value `rows` names is asked to become `checked`.
    SetAll { rows: Vec<usize>, checked: bool },
}

/// What a [`MultiSelect`] draws its menu from, held apart from the gpui handles
/// so that what a click leaves can be read without a window.
#[derive(Default)]
struct Rows {
    options: Vec<MultiOption>,
    open: bool,
    /// The last measurement over `options`, absent until one is taken and
    /// whenever the options replaced leave it describing labels no row holds.
    measured: Option<Measured>,
    /// The groups whose values the menu leaves unlisted, by the position of
    /// the value heading each. Which groups are folded is the menu's own doing
    /// and says nothing of what is checked, so it outlives the options being
    /// restated over the same groups.
    folded: HashSet<usize>,
}

/// The widths a menu's chrome is laid out to.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Widths {
    /// The widest tally the trigger can read, so what it reads does not change
    /// how wide it is drawn.
    trigger: Pixels,
    /// The widest label the rows hold, so the menu fits its content without a
    /// row wrapping and without the sheet shrinking one to nothing.
    label: Pixels,
}

/// [`Widths`] and the text style they were shaped in. Measuring them shapes
/// every label, which is a text layout apiece, and a child view re-renders
/// whenever its parent notifies — so a measurement is reused for as long as
/// both the labels and the style it was taken in stand.
struct Measured {
    font: Font,
    font_size: Pixels,
    widths: Widths,
}

impl Rows {
    /// The widths already measured in `metrics`' text style; `None` where they
    /// have to be measured.
    fn measured(&self, metrics: &TextMetrics) -> Option<Widths> {
        let measured = self.measured.as_ref()?;
        (measured.font == metrics.font && measured.font_size == metrics.font_size)
            .then_some(measured.widths)
    }

    fn remember(&mut self, metrics: &TextMetrics, widths: Widths) {
        self.measured = Some(Measured {
            font: metrics.font.clone(),
            font_size: metrics.font_size,
            widths,
        });
    }

    /// Restates the values offered; `false` where they are the ones already
    /// held.
    fn set_options(&mut self, options: Vec<MultiOption>) -> bool {
        if self.options == options {
            return false;
        }
        // Values restated over the groups already listed are the same menu seen
        // again — a checkmark has moved — and it is left folded as the reader
        // has it. Other groups are another menu, which opens at its own folds.
        if !heads(&options).eq(heads(&self.options)) {
            self.folded = folded_at_rest(&options);
        }
        if !labels(&options).eq(labels(&self.options)) {
            self.measured = None;
        }
        self.options = options;
        true
    }

    /// Whether the group headed by the value at `head` is folded away.
    fn folds(&self, head: usize) -> bool {
        self.folded.contains(&head)
    }

    /// Folds the group headed by the value at `head` away, or lists it again.
    fn fold(&mut self, head: usize) {
        if !self.folded.remove(&head) {
            self.folded.insert(head);
        }
    }

    /// Whether any of the values heads a group.
    fn grouped(&self) -> bool {
        self.options.iter().any(|option| option.heads_group)
    }

    /// The rows the menu lists: every value, less those of a folded group.
    fn listed(&self) -> impl Iterator<Item = (usize, &MultiOption)> {
        let mut folded = false;
        self.options
            .iter()
            .enumerate()
            .filter(move |(index, option)| {
                if option.heads_group {
                    folded = self.folds(*index);
                }
                option.heads_group || !folded
            })
    }

    /// What a click on the row at `index` asks of whoever gave the options: the
    /// value to flip. The menu stays open, since a multi-select is visited to
    /// check several values in one go, and the options stand as they were given.
    fn clicked(&mut self, index: usize) -> Option<MultiSelectEvent> {
        let option = self.options.get(index).filter(|_| self.reports(index))?;
        Some(MultiSelectEvent::Toggled {
            index,
            checked: !option.checked,
        })
    }

    /// Whether a click on the row at `index` reports anything, which a row
    /// naming a value the loaded data cannot answer for does not.
    fn reports(&self, index: usize) -> bool {
        self.options
            .get(index)
            .is_some_and(|option| option.available)
    }

    /// What a click on a menu-wide action over `range` asks: the rows it covers
    /// that report, so such an action leaves what a click on each row in turn
    /// would.
    fn set_all(&self, range: Range<usize>, checked: bool) -> MultiSelectEvent {
        MultiSelectEvent::SetAll {
            rows: range.filter(|&index| self.reports(index)).collect(),
            checked,
        }
    }

    /// Every value the menu offers.
    fn all(&self) -> Range<usize> {
        0..self.options.len()
    }

    /// The values the row at `head` heads: that row and the rows after it, up to
    /// the next row heading a group.
    fn group(&self, head: usize) -> Range<usize> {
        group(&self.options, head)
    }

    /// How many values are checked, of how many are offered.
    fn tally(&self) -> (usize, usize) {
        let checked = self.options.iter().filter(|option| option.checked).count();
        (checked, self.options.len())
    }
}

/// The values the row at `head` heads: that row and the rows after it, up to the
/// next row heading a group.
fn group(options: &[MultiOption], head: usize) -> Range<usize> {
    let under = &options[head + 1..];
    let len = under
        .iter()
        .position(|option| option.heads_group)
        .unwrap_or(under.len());
    head..head + 1 + len
}

/// The values heading a group, each by where it stands and what it reads, which
/// together tell two groups a menu offers under one label apart.
fn heads(options: &[MultiOption]) -> impl Iterator<Item = (usize, &SharedString)> {
    options
        .iter()
        .enumerate()
        .filter(|(_, option)| option.heads_group)
        .map(|(index, option)| (index, &option.label))
}

/// Every value's label, in the order they are stated.
fn labels(options: &[MultiOption]) -> impl Iterator<Item = &SharedString> {
    options.iter().map(|option| &option.label)
}

/// The groups a menu stated over `options` folds away until a reader says
/// otherwise: those holding more values than the list shows at once.
fn folded_at_rest(options: &[MultiOption]) -> HashSet<usize> {
    options
        .iter()
        .enumerate()
        .filter(|(head, option)| {
            option.heads_group && group(options, *head).len() > GROUP_LISTED_ROWS
        })
        .map(|(head, _)| head)
        .collect()
}

/// A checklist over a list of values, as a trigger that opens a menu.
///
/// The control keeps no record of what is checked: it draws the [`MultiOption`]s
/// it was last given, and a click reports a [`MultiSelectEvent`] rather than
/// flipping the row. Whoever owns the state behind the values changes it and
/// states the options afresh with [`set_options`](Self::set_options), so what the
/// control shows cannot drift from what that state says.
pub struct MultiSelect {
    id: ElementId,
    /// Names what the values belong to; read on the trigger before the tally.
    label: SharedString,
    rows: Rows,
    focus_handle: FocusHandle,
    /// Closes the menu when focus leaves it (an outside click); installed lazily
    /// on the first render, where the window is in hand.
    dismiss: Option<Subscription>,
    scroll: ScrollHandle,
    /// The grab offset from the thumb's leading edge while the bar is dragged.
    thumb_grab: Option<f32>,
}

impl MultiSelect {
    pub fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        options: Vec<MultiOption>,
        cx: &mut Context<Self>,
    ) -> Self {
        MultiSelect {
            id: id.into(),
            label: label.into(),
            rows: Rows {
                folded: folded_at_rest(&options),
                options,
                open: false,
                measured: None,
            },
            focus_handle: cx.focus_handle(),
            dismiss: None,
            scroll: ScrollHandle::new(),
            thumb_grab: None,
        }
    }

    /// Restates the values the control offers and which of them are checked.
    pub fn set_options(&mut self, options: Vec<MultiOption>, cx: &mut Context<Self>) {
        if self.rows.set_options(options) {
            cx.notify();
        }
    }

    fn set_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.rows.open = open;
        // Hold focus while open so an outside click dismisses the menu.
        if open {
            self.focus_handle.focus(window, cx);
        }
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" if self.rows.open => self.set_open(false, window, cx),
            "enter" | "space" => self.set_open(!self.rows.open, window, cx),
            _ => {}
        }
    }

    /// The bar for the row list's content height against its visible height;
    /// `None` while the list fits.
    fn vbar(&self) -> Option<Scrollbar> {
        let list = self.scroll.bounds();
        let visible = f32::from(list.size.height);
        Scrollbar::new(
            Axis::Vertical,
            Bounds::new(
                point(list.right() - px(SCROLLBAR_THICKNESS), list.origin.y),
                size(px(SCROLLBAR_THICKNESS), list.size.height),
            ),
            visible + f32::from(self.scroll.max_offset().y),
            visible,
            -f32::from(self.scroll.offset().y),
            DEFAULT_MIN_THUMB,
        )
    }

    /// Begins a scroll drag if `pos` hit the bar; otherwise leaves the press to
    /// fall through to the row under it.
    fn press(&mut self, pos: Point<Pixels>) {
        if let Some(bar) = self.vbar()
            && bar.track_hit(pos)
        {
            let grab = bar.grab(pos);
            self.thumb_grab = Some(grab);
            self.thumb_to(pos, grab);
        }
    }

    /// Maps a thumb drag back to the list's scroll offset.
    fn thumb_to(&mut self, pos: Point<Pixels>, grab: f32) {
        if let Some(bar) = self.vbar() {
            let mut offset = self.scroll.offset();
            offset.y = px(-bar.offset_at(pos, grab));
            self.scroll.set_offset(offset);
        }
    }

    /// Shapes the labels the menu's chrome is sized from.
    fn measure(&self, window: &mut Window) -> Widths {
        let total = self.rows.options.len();
        Widths {
            trigger: menu::widest(
                window,
                std::iter::once(format!("{} {total}/{total}", self.label).into()),
            ),
            label: menu::widest(window, self.rows.options.iter().map(|o| o.label.clone())),
        }
    }

    fn trigger(
        &self,
        colors: &ThemeColors,
        reading_width: Pixels,
        cx: &Context<Self>,
    ) -> impl IntoElement + use<> {
        let (checked, total) = self.rows.tally();
        menu::trigger(self.id.clone(), colors)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                let open = !this.rows.open;
                this.set_open(open, window, cx);
            }))
            .child(menu::reading(
                reading_width,
                format!("{} {checked}/{total}", self.label).into(),
            ))
            .child(menu::caret(colors))
    }

    fn menu(
        &self,
        colors: &ThemeColors,
        label_width: Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let sheet = menu::sheet(colors)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    this.press(e.position);
                    // Keep the press off an ancestor that focuses on mouse-down,
                    // whose blur would dismiss the menu mid-gesture.
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(self.header(colors, cx))
            .child(self.list(colors, label_width, window, cx));
        // Defer the menu so it paints over the content below the trigger.
        deferred(sheet)
    }

    /// The actions over every value the menu holds, lined up with the actions a
    /// group's own row carries by leaving the same room for the scroll bar the
    /// list does.
    fn header(&self, colors: &ThemeColors, cx: &Context<Self>) -> impl IntoElement + use<> {
        menu::header(colors).child(div().pr(px(SCROLLBAR_THICKNESS)).child(self.bulk(
            "multi-select-all".into(),
            "multi-select-none".into(),
            self.rows.all(),
            cx,
        )))
    }

    /// The mark folding the group headed by the value at `head` away, or
    /// listing it again.
    fn fold_mark(
        &self,
        head: usize,
        colors: &ThemeColors,
        cx: &Context<Self>,
    ) -> impl IntoElement + use<> {
        menu::fold(
            ("multi-select-fold", head).into(),
            colors,
            self.rows.folds(head),
        )
        // Keep the press off the row behind it, whose own click would toggle
        // the value that row names.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.rows.fold(head);
            cx.notify();
        }))
    }

    /// The pair checking and clearing every value `range` covers, by which a
    /// menu of hundreds is narrowed to the few being looked for.
    fn bulk(
        &self,
        check: ElementId,
        clear: ElementId,
        range: Range<usize>,
        cx: &Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            // Keep the press off the row behind it, whose own click would
            // toggle the value that row names.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(set_all(check, "all", range.clone(), true, cx))
            .child(set_all(clear, "none", range, false, cx))
    }

    /// The scrollable rows with a scroll bar over their right edge, which the
    /// list keeps clear of its labels.
    fn list(
        &self,
        colors: &ThemeColors,
        label_width: Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let grouped = self.rows.grouped();
        let rows = self
            .rows
            .listed()
            .map(|(index, option)| self.row(index, option, grouped, colors, label_width, cx));
        let list = div()
            .id("multi-select-list")
            .flex()
            .flex_col()
            .w_full()
            .pr(px(SCROLLBAR_THICKNESS))
            .max_h(menu::whole_rows(window, MENU_MAX_HEIGHT))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .children(rows);
        let bar = list_scrollbar(
            cx.entity(),
            self.thumb_grab.is_some(),
            colors.element_hover.into(),
            colors.element_active.into(),
        );
        div()
            .relative()
            .w_full()
            .flex()
            .min_h_0()
            .child(list)
            .child(bar)
    }

    /// One row of the menu. `grouped` says whether any of the menu's values
    /// heads a group, which is what the rows under one are indented past.
    fn row(
        &self,
        index: usize,
        option: &MultiOption,
        grouped: bool,
        colors: &ThemeColors,
        label_width: Pixels,
        cx: &Context<Self>,
    ) -> impl IntoElement + use<> {
        // A row heading a group holding nothing under it has nothing to fold
        // and nothing its own checkmark does not already act on.
        let group = option
            .heads_group
            .then(|| self.rows.group(index))
            .filter(|group| group.len() > 1);
        let heads_values = group.is_some();
        menu::row(
            ("multi-select-option", index).into(),
            colors,
            option.checked,
        )
        .when(!option.available, |row| {
            row.cursor_default().text_color(colors.text_disabled)
        })
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            if let Some(event) = this.rows.clicked(index) {
                cx.emit(event);
            }
            cx.notify();
        }))
        .children(grouped.then(|| match heads_values {
            true => self.fold_mark(index, colors, cx).into_any_element(),
            false => menu::gutter().into_any_element(),
        }))
        .child(menu::check(colors, option.checked))
        .child(menu::reading(label_width, option.label.clone()))
        .children(group.map(|group| {
            self.bulk(
                ("multi-select-group-all", index).into(),
                ("multi-select-group-none", index).into(),
                group,
                cx,
            )
        }))
    }
}

/// The button asking for every value `range` covers to become `checked`.
fn set_all(
    id: ElementId,
    label: &'static str,
    range: Range<usize>,
    checked: bool,
    cx: &Context<MultiSelect>,
) -> Button {
    Button::new(
        id,
        cx.listener(move |this, _: &ClickEvent, _, cx| {
            cx.emit(this.rows.set_all(range.clone(), checked));
            cx.notify();
        }),
    )
    .ghost()
    .compact()
    .label(label)
}

impl EventEmitter<MultiSelectEvent> for MultiSelect {}

impl Render for MultiSelect {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Close on an outside click: the menu holds focus while open (see
        // `set_open`), so losing it means a click landed elsewhere.
        if self.dismiss.is_none() {
            self.dismiss = Some(
                cx.on_focus_out(&self.focus_handle, window, |this, _, _, cx| {
                    if this.rows.open {
                        this.rows.open = false;
                        cx.notify();
                    }
                }),
            );
        }

        let colors = bviz_ui_theme::theme(window, cx).colors;
        let metrics = bviz_ui_assets::text_metrics(window);
        let widths = match self.rows.measured(&metrics) {
            Some(widths) => widths,
            None => {
                let widths = self.measure(window);
                self.rows.remember(&metrics, widths);
                widths
            }
        };
        let trigger = self.trigger(&colors, widths.trigger, cx);
        let label_width = widths
            .label
            .min(window.viewport_size().width * MENU_LABEL_MAX_SHARE);
        let menu = match self.rows.open {
            true => Some(self.menu(&colors, label_width, window, cx)),
            false => None,
        };

        div()
            .relative()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key))
            .child(trigger)
            .children(menu)
    }
}

/// The menu's scroll bar, drawn over the row list once it has been laid out;
/// keeps the drag handlers installed while `dragging`.
fn list_scrollbar(
    control: Entity<MultiSelect>,
    dragging: bool,
    track: Rgba,
    thumb: Rgba,
) -> impl IntoElement {
    overlay_canvas(move |_, window: &mut Window, cx: &mut App| {
        if let Some(bar) = control.read(cx).vbar() {
            bar.paint(track, thumb, window);
        }
        if dragging {
            track_drag(window, &control, on_drag, end_drag);
        }
    })
}

fn on_drag(control: &mut MultiSelect, position: Point<Pixels>, cx: &mut Context<MultiSelect>) {
    if let Some(grab) = control.thumb_grab {
        control.thumb_to(position, grab);
    }
    cx.notify();
}

fn end_drag(
    control: &mut MultiSelect,
    _: Point<Pixels>,
    _: DragEnd,
    _: &mut Window,
    cx: &mut Context<MultiSelect>,
) {
    control.thumb_grab = None;
    cx.notify();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The open menu a control states `options` over.
    fn listing(options: Vec<MultiOption>) -> Rows {
        Rows {
            folded: folded_at_rest(&options),
            options,
            open: true,
            measured: None,
        }
    }

    fn rows(checked: [bool; 3]) -> Rows {
        listing(
            checked
                .into_iter()
                .enumerate()
                .map(|(i, checked)| MultiOption::new(format!("value {i}"), checked))
                .collect(),
        )
    }

    /// The control reports the flip and changes nothing: the checked state it
    /// draws is the one it was given, so a toggle that also flipped the row
    /// would leave a second record of what is shown.
    #[test]
    fn a_click_reports_the_value_to_flip_and_leaves_the_row_as_given() {
        let mut rows = rows([false, true, false]);

        assert_eq!(
            rows.clicked(1),
            Some(MultiSelectEvent::Toggled {
                index: 1,
                checked: false
            })
        );
        assert_eq!(
            rows.clicked(2),
            Some(MultiSelectEvent::Toggled {
                index: 2,
                checked: true
            })
        );

        assert_eq!(rows.tally(), (1, 3));
        assert!(rows.options[1].checked);
    }

    /// Several values are checked in one visit, so the menu outlives a toggle.
    #[test]
    fn a_click_leaves_the_menu_open() {
        let mut rows = rows([false, false, false]);

        rows.clicked(0);
        rows.clicked(2);

        assert!(rows.open);
    }

    #[test]
    fn a_click_off_the_end_of_the_values_reports_nothing() {
        assert_eq!(rows([false, false, false]).clicked(3), None);
    }

    /// A value the loaded data cannot answer for is listed so the reader can see
    /// it is not on offer, and reports nothing when clicked.
    #[test]
    fn a_click_on_an_unavailable_value_reports_nothing() {
        let mut rows = listing(vec![MultiOption {
            available: false,
            ..MultiOption::new("needs --transitions=lite", false)
        }]);

        assert_eq!(rows.clicked(0), None);
    }

    /// A menu-wide action leaves what clicking each row in turn would, so it
    /// passes over the values a click reports nothing for.
    #[test]
    fn a_menu_wide_action_names_the_rows_a_click_would_report() {
        let rows = listing(vec![
            MultiOption::new("answerable", false),
            MultiOption {
                available: false,
                ..MultiOption::new("needs --transitions=lite", false)
            },
            MultiOption::new("answerable too", false),
        ]);

        assert_eq!(
            rows.set_all(rows.all(), true),
            MultiSelectEvent::SetAll {
                rows: vec![0, 2],
                checked: true,
            }
        );
    }

    /// A group reaches from the value heading it to the one heading the next,
    /// so acting on it leaves the groups around it alone; a menu no value heads
    /// a group of holds one run, which the whole-menu action covers.
    #[test]
    fn a_group_runs_from_its_head_to_the_next() {
        let grouped = listing(vec![
            MultiOption::new("loose", true),
            MultiOption::new("first group", true).heading(),
            MultiOption::new("under the first", true),
            MultiOption::new("also under the first", true),
            MultiOption::new("last group", true).heading(),
            MultiOption::new("under the last", true),
        ]);

        assert_eq!(grouped.group(1), 1..4);
        assert_eq!(grouped.group(4), 4..6);
        assert_eq!(grouped.all(), 0..6);

        let flat = rows([true, true, true]);
        assert_eq!(flat.all(), 0..3);
        assert_eq!(flat.group(0), 0..3);
    }

    /// A group of `under` values headed by a row named `head`.
    fn group_of(head: &str, under: usize) -> Vec<MultiOption> {
        let mut options = vec![MultiOption::new(head, true).heading()];
        options.extend((0..under).map(|i| MultiOption::new(format!("{head} {i}"), true)));
        options
    }

    fn labels(rows: &Rows) -> Vec<&str> {
        rows.listed()
            .map(|(_, option)| option.label.as_ref())
            .collect()
    }

    /// A long group buries every group after it, so it opens folded; a short one
    /// is read at a glance and opens listed.
    #[test]
    fn a_group_longer_than_the_list_opens_folded() {
        let mut options = group_of("short", GROUP_LISTED_ROWS - 1);
        options.extend(group_of("long", GROUP_LISTED_ROWS));
        let rows = listing(options);

        assert_eq!(labels(&rows)[..2], ["short", "short 0"]);
        assert_eq!(*labels(&rows).last().expect("the long group's row"), "long");
        assert_eq!(labels(&rows).len(), GROUP_LISTED_ROWS + 1);
    }

    /// Folding a group takes away the values under it and nothing else: the row
    /// heading it stays, as do the groups around it.
    #[test]
    fn folding_a_group_leaves_every_other_group_listed() {
        let mut options = group_of("first", 2);
        options.extend(group_of("second", 2));
        let mut rows = listing(options);
        assert_eq!(
            labels(&rows),
            [
                "first", "first 0", "first 1", "second", "second 0", "second 1"
            ]
        );

        rows.fold(0);
        assert_eq!(labels(&rows), ["first", "second", "second 0", "second 1"]);

        rows.fold(0);
        assert_eq!(labels(&rows).len(), 6);
    }

    /// Two groups a menu offers under one label are still two groups, so
    /// folding one leaves the other listed.
    #[test]
    fn two_groups_reading_alike_fold_apart() {
        let mut options = group_of("group", 2);
        options.extend(group_of("group", 2));
        let mut rows = listing(options);

        rows.fold(3);

        assert_eq!(labels(&rows), ["group", "group 0", "group 1", "group"]);
    }

    /// A menu restated as a checkmark moves is the menu the reader is looking
    /// at, so what they folded stays folded; one stated over other groups is
    /// another menu, which opens at its own folds.
    #[test]
    fn a_fold_outlives_the_values_being_restated_over_the_same_groups() {
        let mut rows = listing(group_of("group", 2));
        rows.fold(0);

        assert!(
            rows.set_options(
                group_of("group", 2)
                    .into_iter()
                    .map(|option| MultiOption {
                        checked: false,
                        ..option
                    })
                    .collect()
            )
        );
        assert_eq!(labels(&rows), ["group"]);

        assert!(rows.set_options(group_of("another group", GROUP_LISTED_ROWS)));
        assert_eq!(labels(&rows), ["another group"]);
        assert!(rows.set_options(group_of("a third group", 1)));
        assert_eq!(labels(&rows), ["a third group", "a third group 0"]);
    }

    fn style(family: &str, size: f32) -> TextMetrics {
        TextMetrics {
            font: gpui::font(family),
            font_size: px(size),
            line_height: px(size * 1.5),
        }
    }

    /// A measurement stands until what it measured moves, so a control
    /// re-rendered — which a parent's notify does, open or closed — shapes no
    /// label again.
    #[test]
    fn a_measurement_is_reused_until_the_options_or_the_text_style_move() {
        let ui = style("Inter", 14.);
        let widths = Widths {
            trigger: px(80.),
            label: px(120.),
        };
        let mut rows = rows([false, true, false]);
        assert_eq!(rows.measured(&ui), None);

        rows.remember(&ui, widths);
        assert_eq!(rows.measured(&ui), Some(widths));
        assert_eq!(rows.measured(&style("Inter", 16.)), None);
        assert_eq!(rows.measured(&style("JetBrains Mono", 14.)), None);

        rows.remember(&ui, widths);
        assert!(rows.set_options(vec![MultiOption::new("another value", true)]));
        assert_eq!(rows.measured(&ui), None);
    }

    /// A measurement describes the labels it shaped, so the values being
    /// restated with a checkmark moved — which is what every toggle does —
    /// leaves it standing rather than shaping every label again.
    #[test]
    fn a_measurement_outlives_a_checkmark_moving() {
        let ui = style("Inter", 14.);
        let widths = Widths {
            trigger: px(80.),
            label: px(120.),
        };
        let mut rows = rows([false, true, false]);
        rows.remember(&ui, widths);

        let flipped: Vec<MultiOption> = rows
            .options
            .iter()
            .map(|option| MultiOption {
                checked: !option.checked,
                ..option.clone()
            })
            .collect();
        assert!(rows.set_options(flipped));

        assert_eq!(rows.measured(&ui), Some(widths));
    }

    /// Restating the values the control already holds leaves the measurement
    /// over them standing, since it describes those very labels.
    #[test]
    fn restating_the_values_already_held_keeps_the_measurement() {
        let ui = style("Inter", 14.);
        let widths = Widths {
            trigger: px(80.),
            label: px(120.),
        };
        let mut rows = rows([false, true, false]);
        rows.remember(&ui, widths);

        assert!(!rows.set_options(rows.options.clone()));
        assert_eq!(rows.measured(&ui), Some(widths));
    }

    /// The trigger reads the values it was given, so restating them is the only
    /// thing that moves it.
    #[test]
    fn the_tally_counts_the_values_the_control_was_given() {
        assert_eq!(rows([true, true, false]).tally(), (2, 3));
        assert_eq!(Rows::default().tally(), (0, 0));
    }
}
