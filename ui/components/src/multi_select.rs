//! A multi-select: a trigger reading how many of its values are checked, and a
//! menu of rows that stays open as they are toggled.

use crate::KeyScope;
use crate::button::Button;
use crate::list_scroll::{ListScroll, list_scrollbar};
use crate::menu_chrome;
use crate::scrollbar::ScrollbarColors;
use bviz_ui_assets::TextMetrics;
use bviz_ui_theme::ThemeColors;
use gpui::{
    App, ClickEvent, Context, ElementId, EventEmitter, FocusHandle, Font, IntoElement, KeyBinding,
    MouseButton, MouseDownEvent, Pixels, Render, SharedString, Subscription, Window, div,
    prelude::*, px,
};

gpui::actions!(
    bviz_multi_select,
    [
        /// Open or close list
        Toggle,
        /// Close list
        Dismiss,
    ]
);

/// The key context scoping the control's own bindings, so a key pressed while
/// it has focus works the control rather than an enclosing binding.
pub(crate) const KEY_CONTEXT: KeyScope = KeyScope::new("MultiSelect", "Checklist");

/// Register the multi-select's key bindings.
pub(crate) fn bind_keys(cx: &mut App) {
    let ctx = Some(KEY_CONTEXT.id());
    cx.bind_keys([
        KeyBinding::new("enter", Toggle, ctx),
        KeyBinding::new("space", Toggle, ctx),
        KeyBinding::new("escape", Dismiss, ctx),
    ]);
}

/// How tall the menu's row list grows before it scrolls.
const MENU_MAX_HEIGHT: Pixels = px(320.);

/// The share of the window's width a row's label runs to before it is cut
/// short, the menu being a transient sheet over the content it acts on.
const MENU_LABEL_MAX_SHARE: f32 = 0.5;

/// One value a [`MultiSelect`] offers.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MultiOption {
    pub label: SharedString,
    /// Whether the state behind the control has this value checked, as the owner
    /// of that state last stated it.
    pub checked: bool,
}

impl MultiOption {
    /// A value the control offers.
    pub fn new(label: impl Into<SharedString>, checked: bool) -> Self {
        MultiOption {
            label: label.into(),
            checked,
        }
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
        if !labels(&options).eq(labels(&self.options)) {
            self.measured = None;
        }
        self.options = options;
        true
    }

    /// What a click on the row at `index` asks of whoever gave the options: the
    /// value to flip. The menu stays open, since a multi-select is visited to
    /// check several values in one go, and the options stand as they were given.
    fn clicked(&mut self, index: usize) -> Option<MultiSelectEvent> {
        let option = self.options.get(index)?;
        Some(MultiSelectEvent::Toggled {
            index,
            checked: !option.checked,
        })
    }

    /// What a click on the menu-wide action asks: every value, so it leaves
    /// what a click on each row in turn would.
    fn set_all(&self, checked: bool) -> MultiSelectEvent {
        MultiSelectEvent::SetAll {
            rows: (0..self.options.len()).collect(),
            checked,
        }
    }

    /// How many values are checked, of how many are offered.
    fn tally(&self) -> (usize, usize) {
        let checked = self.options.iter().filter(|option| option.checked).count();
        (checked, self.options.len())
    }
}

/// Every value's label, in the order they are stated.
fn labels(options: &[MultiOption]) -> impl Iterator<Item = &SharedString> {
    options.iter().map(|option| &option.label)
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
    scroll: ListScroll,
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
            scroll: ListScroll::new(),
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

    fn toggle(&mut self, _: &Toggle, window: &mut Window, cx: &mut Context<Self>) {
        self.set_open(!self.rows.open, window, cx);
    }

    fn dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<Self>) {
        if self.rows.open {
            self.set_open(false, window, cx);
        } else {
            cx.propagate();
        }
    }

    /// Shapes the labels the menu's chrome is sized from.
    fn measure(&self, window: &mut Window) -> Widths {
        let total = self.rows.options.len();
        Widths {
            trigger: menu_chrome::widest(
                window,
                std::iter::once(format!("{} {total}/{total}", self.label).into()),
            ),
            label: menu_chrome::widest(window, self.rows.options.iter().map(|o| o.label.clone())),
        }
    }

    fn trigger(
        &self,
        colors: &ThemeColors,
        reading_width: Pixels,
        cx: &Context<Self>,
    ) -> impl IntoElement + use<> {
        let (checked, total) = self.rows.tally();
        menu_chrome::trigger(self.id.clone(), colors)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                let open = !this.rows.open;
                this.set_open(open, window, cx);
            }))
            .child(menu_chrome::reading(
                reading_width,
                format!("{} {checked}/{total}", self.label).into(),
            ))
            .child(menu_chrome::caret(colors))
    }

    fn menu(
        &self,
        colors: &ThemeColors,
        label_width: Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let sheet = menu_chrome::sheet(colors)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    this.scroll.press(e.position);
                    // Keep the press off an ancestor that focuses on mouse-down,
                    // whose blur would dismiss the menu mid-gesture.
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(self.header(colors, cx))
            .child(self.list(colors, label_width, window, cx));
        menu_chrome::under_trigger(sheet)
    }

    /// The actions over every value the menu holds, kept as clear of the
    /// scroll bar as the rows are.
    fn header(&self, colors: &ThemeColors, cx: &Context<Self>) -> impl IntoElement + use<> {
        menu_chrome::clear_of_scrollbar(menu_chrome::header(colors)).child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .child(set_all("multi-select-all", "all", true, cx))
                .child(set_all("multi-select-none", "none", false, cx)),
        )
    }

    /// The scrollable rows with a scroll bar over their right edge, which each
    /// row keeps clear of its label.
    fn list(
        &self,
        colors: &ThemeColors,
        label_width: Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let rows = (self.rows.options.iter())
            .enumerate()
            .map(|(index, option)| self.row(index, option, colors, label_width, cx));
        let list = div()
            .id("multi-select-list")
            .flex()
            .flex_col()
            .w_full()
            .max_h(menu_chrome::whole_rows(
                menu_chrome::row_height(window),
                MENU_MAX_HEIGHT,
            ))
            .overflow_y_scroll()
            .track_scroll(self.scroll.handle())
            .children(rows);
        let bar = list_scrollbar(
            cx.entity(),
            |control: &Self| &control.scroll,
            |control: &mut Self| &mut control.scroll,
            ScrollbarColors::from_colors(colors),
        );
        div()
            .relative()
            .w_full()
            .flex()
            .min_h_0()
            .child(list)
            .child(bar)
    }

    /// One row of the menu.
    fn row(
        &self,
        index: usize,
        option: &MultiOption,
        colors: &ThemeColors,
        label_width: Pixels,
        cx: &Context<Self>,
    ) -> impl IntoElement + use<> {
        menu_chrome::clear_of_scrollbar(menu_chrome::row(
            ("multi-select-option", index).into(),
            colors,
            option.checked,
        ))
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            if let Some(event) = this.rows.clicked(index) {
                cx.emit(event);
            }
            cx.notify();
        }))
        .child(menu_chrome::check(colors, option.checked))
        .child(menu_chrome::reading(label_width, option.label.clone()))
    }
}

/// The button asking for every value to become `checked`.
fn set_all(
    id: &'static str,
    label: &'static str,
    checked: bool,
    cx: &Context<MultiSelect>,
) -> Button {
    Button::new(
        id,
        cx.listener(move |this, _: &ClickEvent, _, cx| {
            cx.emit(this.rows.set_all(checked));
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
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::toggle))
            .on_action(cx.listener(Self::dismiss))
            .child(trigger)
            .children(menu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The open menu a control states `options` over.
    fn listing(options: Vec<MultiOption>) -> Rows {
        Rows {
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

    /// A menu-wide action leaves what clicking each row in turn would.
    #[test]
    fn a_menu_wide_action_names_every_row() {
        assert_eq!(
            rows([false, true, false]).set_all(true),
            MultiSelectEvent::SetAll {
                rows: vec![0, 1, 2],
                checked: true,
            }
        );
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
