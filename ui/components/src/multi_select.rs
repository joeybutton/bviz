//! A multi-select: a trigger reading how many of its values are checked, and a
//! menu of rows that stays open as they are toggled.

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

/// How tall the menu's row list grows before it scrolls. A facet can carry
/// hundreds of values, so the list is bounded and the rest reached by scrolling.
const MENU_MAX_HEIGHT: Pixels = px(320.);

/// The share of the window's width a row's label runs to before it is cut short.
/// A canonical repository name can run far wider than anything else a menu
/// holds, and the menu is a transient sheet over the content it acts on.
const MENU_LABEL_MAX_SHARE: f32 = 0.5;

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
}

impl MultiOption {
    /// A value the control offers.
    pub fn new(label: impl Into<SharedString>, checked: bool) -> Self {
        MultiOption {
            label: label.into(),
            checked,
            available: true,
        }
    }
}

/// Emitted when the user clicks an available row: the value at `index` is asked
/// to become `checked`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MultiSelectEvent {
    pub index: usize,
    pub checked: bool,
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
        self.options = options;
        self.measured = None;
        true
    }

    /// What a click on the row at `index` asks of whoever gave the options: the
    /// value to flip. The menu stays open, since a multi-select is visited to
    /// check several values in one go, and the options stand as they were given.
    fn clicked(&mut self, index: usize) -> Option<MultiSelectEvent> {
        let option = self.options.get(index).filter(|option| option.available)?;
        Some(MultiSelectEvent {
            index,
            checked: !option.checked,
        })
    }

    /// How many values are checked, of how many are offered.
    fn tally(&self) -> (usize, usize) {
        let checked = self.options.iter().filter(|option| option.checked).count();
        (checked, self.options.len())
    }
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
            .child(self.list(colors, label_width, cx));
        // Defer the menu so it paints over the content below the trigger.
        deferred(sheet)
    }

    /// The scrollable rows with a scroll bar over their right edge, which the
    /// list keeps clear of its labels.
    fn list(
        &self,
        colors: &ThemeColors,
        label_width: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let rows = self
            .rows
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| self.row(index, option, colors, label_width, cx));
        let list = div()
            .id("multi-select-list")
            .flex()
            .flex_col()
            .w_full()
            .pr(px(SCROLLBAR_THICKNESS))
            .max_h(MENU_MAX_HEIGHT)
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

    fn row(
        &self,
        index: usize,
        option: &MultiOption,
        colors: &ThemeColors,
        label_width: Pixels,
        cx: &Context<Self>,
    ) -> impl IntoElement + use<> {
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
        .child(menu::check(colors, option.checked))
        .child(menu::reading(label_width, option.label.clone()))
    }
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
            true => Some(self.menu(&colors, label_width, cx)),
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
    cx: &mut Context<MultiSelect>,
) {
    control.thumb_grab = None;
    cx.notify();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(checked: [bool; 3]) -> Rows {
        Rows {
            options: checked
                .into_iter()
                .enumerate()
                .map(|(i, checked)| MultiOption::new(format!("value {i}"), checked))
                .collect(),
            open: true,
            measured: None,
        }
    }

    /// The control reports the flip and changes nothing: the checked state it
    /// draws is the one it was given, so a toggle that also flipped the row
    /// would leave a second record of what is shown.
    #[test]
    fn a_click_reports_the_value_to_flip_and_leaves_the_row_as_given() {
        let mut rows = rows([false, true, false]);

        assert_eq!(
            rows.clicked(1),
            Some(MultiSelectEvent {
                index: 1,
                checked: false
            })
        );
        assert_eq!(
            rows.clicked(2),
            Some(MultiSelectEvent {
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
        let mut rows = Rows {
            options: vec![MultiOption {
                label: "needs --transitions=lite".into(),
                checked: false,
                available: false,
            }],
            open: true,
            measured: None,
        };

        assert_eq!(rows.clicked(0), None);
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
