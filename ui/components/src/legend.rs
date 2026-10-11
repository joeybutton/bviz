//! A view's colour key, held to one line of its toolbar so it never covers
//! what it explains. Entries that do not fit fold into a chip opening the
//! whole key.

use crate::gesture::{Button as PointerButton, Entry, GestureRow, Gestures, Input, Place, Pointer};
use crate::{
    Button, CONTROL_HEIGHT, KeyScope, TextTooltip, action_label, key_text_in, menu_chrome,
};
use bviz_ui_theme::ThemeColors;
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, ClickEvent, Context, Div, Element, ElementId, Entity,
    FocusHandle, Font, GlobalElementId, Hsla, InspectorElementId, IntoElement, KeyBinding,
    LayoutId, Length, MouseButton, Pixels, SharedString, Style, Subscription, Window, div,
    prelude::*, px, relative, size,
};
use std::panic::Location;
use std::rc::Rc;

gpui::actions!(
    bviz_legend,
    [
        /// Show legend
        ShowLegend,
        /// Close legend
        CloseLegend,
    ]
);

/// The key context the open popover carries, which [`CloseLegend`] is scoped
/// to.
pub(crate) const KEY_CONTEXT: KeyScope = KeyScope::new("Legend", "Legend");

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "escape",
        CloseLegend,
        Some(KEY_CONTEXT.id()),
    )]);
}

/// The key showing the legend of a view carrying `scope`, which stands down
/// while a text field in it holds the focus.
pub fn legend_key_bindings(scope: KeyScope) -> [KeyBinding; 1] {
    let predicate = crate::text_input::unless_typing(scope);
    [KeyBinding::new("l", ShowLegend, Some(predicate.as_str()))]
}

#[derive(Clone, Copy)]
enum LegendIntent {
    Toggle,
}

const GESTURES: Gestures<LegendIntent> = Gestures::new(&[Entry::new(
    Input::click(PointerButton::Left),
    Place::On("a legend chip"),
    "Show or hide legend",
    LegendIntent::Toggle,
)]);

/// The gestures a [`Legend`] answers to, for the view holding one to list.
pub fn legend_gestures() -> impl Iterator<Item = GestureRow> {
    GESTURES.rows()
}

/// What a [`LegendToggle`] reads, and the icon it carries.
const TOGGLE_LABEL: &str = "legend";
const TOGGLE_ICON: &str = "swatches";

/// Every swatch's box, so a swatch of one kind lines up with one of another.
const SWATCH_WIDTH: Pixels = px(16.);
const SWATCH_HEIGHT: Pixels = px(10.);

/// Stroke of a line swatch, and the dash and gap of a dashed one.
const LINE_WIDTH: Pixels = px(2.);
const DASH: Pixels = px(4.);
const DASH_GAP: Pixels = px(2.);

/// Diameter of the small dot of a [`Swatch::Sizes`]; its large dot fills the
/// swatch's height.
const SMALL_DOT: Pixels = px(4.);

/// Text size of everything a legend reads, in the strip and the popover alike.
const TEXT_SIZE: Pixels = px(12.);

/// Room between a swatch and its label, and between entries.
const LABEL_GAP: Pixels = px(4.);
const ENTRY_GAP: Pixels = px(12.);

/// Most columns of entries the popover lays its key out in.
const POPOVER_COLUMNS: u16 = 2;

/// Room the popover leaves on either side of its key.
const POPOVER_INSET: Pixels = px(12.);

/// Widest a note in the popover runs before it wraps.
const NOTE_WIDTH: Pixels = px(320.);

/// Paints a mark into the bounds of a swatch.
pub type PaintSwatch = Rc<dyn Fn(Bounds<Pixels>, &mut Window, &mut App)>;

/// How an entry's swatch is drawn, to match the marks it names.
#[derive(Clone, IntoElement)]
pub enum Swatch {
    Fill(Hsla),
    /// A fill edged in another colour.
    Outlined {
        fill: Hsla,
        edge: Hsla,
    },
    /// A fill wearing a heavy ring, as a halo marks a box.
    Ring {
        fill: Hsla,
        ring: Hsla,
    },
    Line(Hsla),
    DashedLine(Hsla),
    Dot(Hsla),
    /// A small and a large dot, keying a mark's size rather than its colour.
    Sizes(Hsla),
    /// Fills from the least end of a scale to the most.
    Ramp(Rc<[Hsla]>),
    /// A mark drawn by the view itself into the swatch's box.
    Painted(PaintSwatch),
}

impl Swatch {
    fn element(&self) -> Div {
        let frame = div()
            .flex()
            .flex_none()
            .items_center()
            .w(SWATCH_WIDTH)
            .h(SWATCH_HEIGHT);
        let dot = |diameter: Pixels, color: Hsla| {
            div().flex_none().size(diameter).rounded_full().bg(color)
        };
        match self {
            Swatch::Fill(fill) => frame.rounded_sm().bg(*fill),
            Swatch::Outlined { fill, edge } => {
                frame.rounded_sm().bg(*fill).border_1().border_color(*edge)
            }
            Swatch::Ring { fill, ring } => {
                frame.rounded_sm().bg(*fill).border_2().border_color(*ring)
            }
            Swatch::Line(color) => frame.child(div().w_full().h(LINE_WIDTH).bg(*color)),
            Swatch::DashedLine(color) => {
                let dashes = ((SWATCH_WIDTH + DASH_GAP) / (DASH + DASH_GAP)) as usize;
                frame
                    .gap(DASH_GAP)
                    .children((0..dashes).map(|_| div().w(DASH).h(LINE_WIDTH).bg(*color)))
            }
            Swatch::Dot(color) => frame.justify_center().child(dot(SWATCH_HEIGHT, *color)),
            Swatch::Sizes(color) => frame
                .justify_between()
                .child(dot(SMALL_DOT, *color))
                .child(dot(SWATCH_HEIGHT, *color)),
            Swatch::Ramp(fills) => frame
                .rounded_sm()
                .overflow_hidden()
                .children(fills.iter().map(|&fill| div().flex_1().h_full().bg(fill))),
            Swatch::Painted(paint) => {
                let paint = paint.clone();
                frame
                    .relative()
                    .child(crate::overlay_canvas(move |bounds, window, cx| {
                        paint(bounds, window, cx)
                    }))
            }
        }
    }
}

impl RenderOnce for Swatch {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        self.element()
    }
}

/// One mark a legend names: its swatch beside its label.
#[derive(Clone, IntoElement)]
pub struct LegendEntry {
    swatch: Swatch,
    label: SharedString,
    tooltip: Option<SharedString>,
}

impl LegendEntry {
    pub fn new(swatch: Swatch, label: impl Into<SharedString>) -> Self {
        Self {
            swatch,
            label: label.into(),
            tooltip: None,
        }
    }

    /// The entry explaining itself by `tooltip` under the pointer.
    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    fn body(&self) -> Div {
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(LABEL_GAP)
            .text_size(TEXT_SIZE)
            .whitespace_nowrap()
            .child(self.swatch.element())
            .child(self.label.clone())
    }
}

/// An entry's tooltip is keyed by its label, so the entries sharing a parent
/// carry distinct labels.
impl RenderOnce for LegendEntry {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let body = self.body();
        match self.tooltip {
            Some(tooltip) => {
                let colors = bviz_ui_theme::theme(window, cx).colors.tooltip();
                body.id(ElementId::Name(self.label))
                    .tooltip(TextTooltip::builder(tooltip, colors))
                    .into_any_element()
            }
            None => body.into_any_element(),
        }
    }
}

/// The popover a [`LegendState`] has open.
struct Opened {
    /// Where focus stood when the popover opened, which closing it from
    /// within hands focus back to.
    from: FocusHandle,
    /// Closes the popover as focus leaves it.
    _dismiss: Subscription,
}

/// Whether a legend's popover is open. The view showing the legend holds it,
/// so that its [`ShowLegend`] handler can open the popover.
pub struct LegendState {
    /// Focus of the view the legend belongs to: what the chip's tooltip looks
    /// [`ShowLegend`]'s keys up in, and where closing the popover hands focus
    /// when nothing held it as the popover opened.
    view_focus: FocusHandle,
    popover_focus: FocusHandle,
    drawn: OnScreen,
    open: Option<Opened>,
    /// The widths the legend last laid out was measured at, which a frame
    /// laying out the same key in the same font reuses.
    measured: Option<Rc<Measured>>,
}

impl LegendState {
    pub fn new(view_focus: FocusHandle, cx: &mut Context<Self>) -> Self {
        Self {
            view_focus,
            popover_focus: cx.focus_handle(),
            drawn: OnScreen::new(cx),
            open: None,
            measured: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Opens the popover, taking focus into it so that an outside click or
    /// Escape closes it. A legend the last frame did not draw, or that keys
    /// nothing, has no popover to open.
    pub fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let keys_something = self.measured.as_ref().is_some_and(|m| !m.keys_nothing());
        if self.open.is_some() || !keys_something || !self.drawn.in_last_frame(window) {
            return;
        }
        let dismiss = cx.on_focus_out(&self.popover_focus, window, |this, _, _, cx| {
            if this.open.take().is_some() {
                cx.notify();
            }
        });
        self.open = Some(Opened {
            from: window
                .focused(cx)
                .unwrap_or_else(|| self.view_focus.clone()),
            _dismiss: dismiss,
        });
        self.popover_focus.focus(window, cx);
        cx.notify();
    }

    /// Closes the popover from within, handing focus back where it stood when
    /// the popover opened.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Opened { from, .. }) = self.open.take() {
            window.focus(&from, cx);
            cx.notify();
        }
    }

    fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.is_open() {
            true => self.close(window, cx),
            false => self.show(window, cx),
        }
    }
}

/// What a strip of a given width shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fold {
    /// Every entry, and no chip.
    All,
    /// The first `shown` entries, then a chip opening the popover.
    Leading { shown: usize },
    /// No entry: one chip standing for the whole legend.
    Whole,
}

/// What a strip `room` wide shows of entries `widths` wide, kept `ENTRY_GAP`
/// apart, where a chip for `held` entries left out is `chip(held)` wide. Where
/// the legend has `notes`, which only the popover shows, a chip always stands.
fn fold(
    widths: &[Pixels],
    room: Pixels,
    notes: bool,
    mut chip: impl FnMut(usize) -> Pixels,
) -> Fold {
    let mut ends = Vec::with_capacity(widths.len() + 1);
    ends.push(px(0.));
    for (shown, &width) in widths.iter().enumerate() {
        let gap = if shown == 0 { px(0.) } else { ENTRY_GAP };
        ends.push(ends[shown] + gap + width);
    }
    let all = widths.len();
    if !notes && ends[all] <= room {
        return Fold::All;
    }
    (1..=all)
        .rev()
        .find(|&shown| ends[shown] + ENTRY_GAP + chip(all - shown) <= room)
        .map_or(Fold::Whole, |shown| Fold::Leading { shown })
}

/// What a chip reads: the number of entries it holds back, or, where it holds
/// back none, what it does hold.
fn chip_label(fold: Fold, entries: usize) -> SharedString {
    match fold {
        Fold::Leading { shown } if shown < entries => format!("+{}", entries - shown).into(),
        Fold::Leading { .. } => "Notes".into(),
        Fold::All | Fold::Whole => "Legend".into(),
    }
}

/// A view's legend, held to one line of `CONTROL_HEIGHT` that takes the room
/// left on its row. Entries are given in priority order and fold from the end
/// into a chip opening the popover, which shows every entry and the notes.
pub struct Legend {
    id: ElementId,
    state: Entity<LegendState>,
    entries: Vec<LegendEntry>,
    notes: Vec<SharedString>,
}

impl Legend {
    pub fn new(id: impl Into<ElementId>, state: &Entity<LegendState>) -> Self {
        Self {
            id: id.into(),
            state: state.clone(),
            entries: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn entry(mut self, entry: LegendEntry) -> Self {
        self.entries.push(entry);
        self
    }

    pub fn entries(mut self, entries: impl IntoIterator<Item = LegendEntry>) -> Self {
        self.entries.extend(entries);
        self
    }

    /// A line on how to read the key, shown only in the popover.
    pub fn note(mut self, note: impl Into<SharedString>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// The legend standing as a toggle that opens the whole key under itself,
    /// to sit among a toolbar's toggles putting panels up and away.
    pub fn toggle(self) -> LegendToggle {
        LegendToggle { legend: self }
    }

    /// What this frame reads of the legend's state.
    fn seen(&self, cx: &App) -> Seen {
        let state = self.state.read(cx);
        Seen {
            view_focus: state.view_focus.clone(),
            popover_focus: state.popover_focus.clone(),
            drawn: state.drawn.clone(),
            open: state.is_open(),
        }
    }

    /// The widths this legend lays out at: those `state` holds where they were
    /// measured for the same key in the font of this frame, or else measured
    /// afresh and kept there.
    fn widths(&self, window: &mut Window, cx: &mut App) -> Rc<Measured> {
        let font = window.text_style().font();
        let held = self.state.read(cx).measured.clone();
        if let Some(held) = held.filter(|held| held.is_of(self, &font)) {
            return held;
        }
        let measured = Rc::new(Measured::new(self, font, window, cx));
        let kept = measured.clone();
        self.state
            .update(cx, |state, _| state.measured = Some(kept));
        measured
    }

    /// A chip reading `label` that opens the popover under itself, the
    /// popover's holder laid out to the chip's bounds.
    fn chip(&self, label: SharedString, seen: &Seen, colors: &ThemeColors) -> Div {
        let state = self.state.clone();
        let view_focus = seen.view_focus.clone();
        let tooltip_colors = colors.tooltip();
        // A tooltip would cover the popover the chip holds open.
        let chip = menu_chrome::trigger("chip".into(), colors)
            .when(!seen.open, |chip| {
                chip.tooltip(move |window, cx| {
                    let label = action_label(&ShowLegend, cx).unwrap_or_default();
                    let keys = key_text_in(&ShowLegend, &view_focus, window);
                    TextTooltip::view(label.into(), keys, tooltip_colors, cx)
                })
            })
            .on_click(move |event: &ClickEvent, window, cx| {
                GESTURES.dispatch(Pointer::click(event), |intent| match intent {
                    LegendIntent::Toggle => {
                        state.update(cx, |state, cx| state.toggle(window, cx));
                        true
                    }
                });
            })
            .child(label);
        chip_holder(chip)
    }

    /// Every entry and note, opened under the element it is a child of, the
    /// key laid out in `columns`.
    fn popover(
        &self,
        seen: &Seen,
        columns: u16,
        measured: &Measured,
        colors: &ThemeColors,
    ) -> impl IntoElement + use<> {
        let state = self.state.clone();
        let key = div()
            .id("popover-key")
            .grid()
            .grid_cols(columns)
            .gap_x(ENTRY_GAP)
            .gap_y_1()
            .children(self.entries.iter().cloned());
        let notes = self
            .notes
            .iter()
            .zip(&measured.note_widths)
            .map(|(text, &width)| note(text).w(width).text_color(colors.text_muted));
        let sheet = menu_chrome::sheet(colors)
            .key_context(KEY_CONTEXT)
            .track_focus(&seen.popover_focus)
            .on_action(move |_: &CloseLegend, window, cx| {
                state.update(cx, |state, cx| state.close(window, cx));
            })
            .px(POPOVER_INSET)
            .gap_2()
            .text_size(TEXT_SIZE)
            .text_color(colors.text)
            .child(key)
            .children(notes);
        menu_chrome::under_trigger(sheet)
    }

    /// The strip as `fold` lays it out, with the popover in `columns` where it
    /// is open: under the chip, or under the strip where no chip stands.
    fn strip(
        &mut self,
        fold: Fold,
        seen: &Seen,
        popover: Option<u16>,
        measured: &Measured,
        colors: &ThemeColors,
    ) -> AnyElement {
        let all = self.entries.len();
        let shown = match fold {
            Fold::All => all,
            Fold::Leading { shown } => shown,
            Fold::Whole => 0,
        };
        let popover = popover.map(|columns| self.popover(seen, columns, measured, colors));
        let (chip, unchipped) = match fold {
            Fold::All => (None, popover),
            _ => (
                Some(
                    self.chip(chip_label(fold, all), seen, colors)
                        .children(popover),
                ),
                None,
            ),
        };
        div()
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap(ENTRY_GAP)
            .size_full()
            .text_size(TEXT_SIZE)
            .text_color(colors.text)
            .children(self.entries.drain(..shown))
            .children(chip)
            .children(unchipped)
            .child(seen.drawn.mark())
            .into_any_element()
    }
}

/// A [`Legend`] standing as a toggle, made by [`Legend::toggle`].
#[derive(IntoElement)]
pub struct LegendToggle {
    legend: Legend,
}

impl RenderOnce for LegendToggle {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let legend = self.legend;
        let colors = bviz_ui_theme::theme(window, cx).colors;
        let measured = legend.widths(window, cx);
        let seen = legend.seen(cx);
        let popover = seen.open.then(|| {
            let inset = window.client_inset().unwrap_or_default();
            let room = window.viewport_size().width - inset * 2.;
            let columns = popover_columns(&measured.entry_widths, room);
            legend.popover(&seen, columns, &measured, &colors)
        });
        let state = legend.state.clone();
        let toggle = Button::new(legend.id.clone(), move |event, window, cx| {
            GESTURES.dispatch(Pointer::click(event), |intent| match intent {
                LegendIntent::Toggle => {
                    state.update(cx, |state, cx| state.toggle(window, cx));
                    true
                }
            });
        })
        .label(TOGGLE_LABEL)
        .icon(TOGGLE_ICON)
        .selected(seen.open)
        .disabled(measured.keys_nothing())
        // A tooltip would cover the popover the toggle holds open.
        .when(!seen.open, |toggle| {
            toggle.keys_for(&ShowLegend, &seen.view_focus)
        });
        div()
            .relative()
            .flex_none()
            // Keeps the press from an ancestor that focuses on it, which would
            // close the popover just before the click toggles it open again.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(toggle)
            .children(popover)
            .child(seen.drawn.mark())
    }
}

/// Marks the frames a legend is drawn in, for [`LegendState::show`] to tell
/// the legend is on screen by. Layout runs neither for a frame leaving the
/// legend out nor for one replaying it from a cache, so only what the frame
/// carries tells the two apart. gpui tells whether a frame carries a focus
/// handle only by whether one handle contains another, so the mark carries
/// two.
#[derive(Clone)]
struct OnScreen {
    outer: FocusHandle,
    inner: FocusHandle,
}

impl OnScreen {
    fn new(cx: &mut App) -> Self {
        Self {
            outer: cx.focus_handle(),
            inner: cx.focus_handle(),
        }
    }

    /// The mark a frame carries, of no size so that no click focuses it.
    fn mark(&self) -> Div {
        div()
            .absolute()
            .track_focus(&self.outer)
            .child(div().track_focus(&self.inner))
    }

    fn in_last_frame(&self, window: &Window) -> bool {
        self.outer.contains(&self.inner, window)
    }
}

/// What a frame of a [`Legend`] reads of its [`LegendState`].
struct Seen {
    view_focus: FocusHandle,
    popover_focus: FocusHandle,
    drawn: OnScreen,
    open: bool,
}

/// The widths a legend's key lays out at in one font. An entry's width
/// follows from its label alone, as every swatch is one size.
pub struct Measured {
    font: Font,
    labels: Vec<SharedString>,
    note_texts: Vec<SharedString>,
    entry_widths: Vec<Pixels>,
    /// Each note's unwrapped width, up to `NOTE_WIDTH`, which it is held to:
    /// the sheet takes its width from what it holds, so a note sizing itself
    /// would wrap as narrow as the sheet's holder.
    note_widths: Vec<Pixels>,
    /// A chip's width by the number of entries it holds back.
    chip_widths: Vec<Pixels>,
    /// The chip standing for the whole legend, which the strip is never
    /// narrower than; none where the legend keys nothing.
    whole_chip: Pixels,
}

impl Measured {
    fn new(legend: &Legend, font: Font, window: &mut Window, cx: &mut App) -> Self {
        let colors = bviz_ui_theme::theme(window, cx).colors;
        let chip = |label: SharedString, window: &mut Window, cx: &mut App| {
            let face = menu_chrome::trigger("measured-chip".into(), &colors).child(label);
            measured(chip_holder(face), window, cx)
        };
        let all = legend.entries.len();
        let chip_widths = (0..=all)
            .map(|held| {
                chip(
                    chip_label(Fold::Leading { shown: all - held }, all),
                    window,
                    cx,
                )
            })
            .collect();
        let keys_nothing = all == 0 && legend.notes.is_empty();
        let whole_chip = match keys_nothing {
            true => px(0.),
            false => chip(chip_label(Fold::Whole, all), window, cx),
        };
        let entry_widths = legend
            .entries
            .iter()
            .map(|entry| measured(entry.body(), window, cx))
            .collect();
        let note_widths = legend
            .notes
            .iter()
            .map(|text| measured(note(text), window, cx).min(NOTE_WIDTH))
            .collect();
        Self {
            font,
            labels: legend
                .entries
                .iter()
                .map(|entry| entry.label.clone())
                .collect(),
            note_texts: legend.notes.clone(),
            entry_widths,
            note_widths,
            chip_widths,
            whole_chip,
        }
    }

    /// Whether these are the widths of `legend`'s key in `font`.
    fn is_of(&self, legend: &Legend, font: &Font) -> bool {
        self.font == *font
            && self.note_texts == legend.notes
            && self
                .labels
                .iter()
                .eq(legend.entries.iter().map(|entry| &entry.label))
    }

    fn keys_nothing(&self) -> bool {
        self.labels.is_empty() && self.note_texts.is_empty()
    }
}

/// The most columns, up to `POPOVER_COLUMNS` and one for each entry, that
/// entries `widths` wide lay out in within `room` across.
fn popover_columns(widths: &[Pixels], room: Pixels) -> u16 {
    let widest = widths.iter().copied().fold(px(0.), Pixels::max);
    let most = POPOVER_COLUMNS.min(widths.len().max(1) as u16);
    let edges = (POPOVER_INSET + menu_chrome::SHEET_BORDER) * 2.;
    (1..=most)
        .rev()
        .find(|&columns| {
            let columns = f32::from(columns);
            widest * columns + ENTRY_GAP * (columns - 1.) + edges <= room
        })
        .unwrap_or(1)
}

/// The box a chip stands in on the strip.
fn chip_holder(chip: impl IntoElement) -> Div {
    div()
        .relative()
        .flex_none()
        .text_size(TEXT_SIZE)
        .child(chip)
}

/// A note of the popover.
fn note(text: &SharedString) -> Div {
    div().text_size(TEXT_SIZE).child(text.clone())
}

/// How wide `element` lays out given all the room it asks for.
fn measured(element: impl IntoElement, window: &mut Window, cx: &mut App) -> Pixels {
    let content = size(AvailableSpace::MaxContent, AvailableSpace::MaxContent);
    element
        .into_any_element()
        .layout_as_root(content, window, cx)
        .width
}

impl IntoElement for Legend {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

/// Lays the strip out once its width is known: growing into the room its row
/// leaves, which a sibling that also grows shares with it, and never narrower
/// than the chip standing for the whole legend. A legend keying nothing takes
/// no room, so a growing sibling has the row's room to itself.
impl Element for Legend {
    type RequestLayoutState = Rc<Measured>;
    type PrepaintState = AnyElement;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Rc<Measured>) {
        let measured = self.widths(window, cx);
        let style = Style {
            flex_grow: if measured.keys_nothing() { 0. } else { 1. },
            flex_shrink: 1.,
            flex_basis: relative(0.).into(),
            min_size: size(measured.whole_chip.into(), Length::Auto),
            size: size(Length::Auto, CONTROL_HEIGHT.into()),
            ..Style::default()
        };
        (window.request_layout(style, [], cx), measured)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        measured: &mut Rc<Measured>,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let colors = bviz_ui_theme::theme(window, cx).colors;
        let seen = self.seen(cx);
        let fold = fold(
            &measured.entry_widths,
            bounds.size.width,
            !self.notes.is_empty(),
            |held| measured.chip_widths[held],
        );
        let popover = seen.open.then(|| {
            let inset = window.client_inset().unwrap_or_default();
            let room = window.viewport_size().width - inset * 2.;
            popover_columns(&measured.entry_widths, room)
        });
        let mut strip = self.strip(fold, &seen, popover, measured, &colors);
        let room = size(
            AvailableSpace::Definite(bounds.size.width),
            AvailableSpace::Definite(bounds.size.height),
        );
        strip.prepaint_as_root(bounds.origin, room, window, cx);
        strip
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Rc<Measured>,
        strip: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) {
        strip.paint(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A chip as wide as its count has digits, so a test can tell which
    /// count it was measured for.
    fn chip(held: usize) -> Pixels {
        px(10. * (1 + held.to_string().len()) as f32)
    }

    fn widths(each: &[f32]) -> Vec<Pixels> {
        each.iter().copied().map(px).collect()
    }

    #[test]
    fn a_strip_with_room_for_every_entry_shows_them_all_and_no_chip() {
        let widths = widths(&[50., 50., 50.]);
        let room = px(50. * 3.) + ENTRY_GAP * 2.;
        assert_eq!(fold(&widths, room, false, chip), Fold::All);
    }

    #[test]
    fn entries_past_the_room_fold_from_the_end_into_the_chip() {
        let widths = widths(&[50., 50., 50., 50.]);
        let two_and_chip = px(50. * 2.) + ENTRY_GAP * 2. + chip(2);
        assert_eq!(
            fold(&widths, two_and_chip, false, chip),
            Fold::Leading { shown: 2 }
        );
        assert_eq!(
            fold(&widths, two_and_chip - px(1.), false, chip),
            Fold::Leading { shown: 1 }
        );
    }

    #[test]
    fn the_room_the_chip_takes_is_kept_for_it() {
        let widths = widths(&[50., 50.]);
        let both = px(100.) + ENTRY_GAP;
        assert_eq!(
            fold(&widths, both - px(1.), false, chip),
            Fold::Leading { shown: 1 }
        );
    }

    #[test]
    fn the_chip_is_measured_for_the_count_it_reads() {
        let eleven = widths(&[50.; 11]);
        let one_and_short_chip = px(50.) + ENTRY_GAP + chip(9);
        assert_eq!(fold(&eleven, one_and_short_chip, false, chip), Fold::Whole);
        let one_and_long_chip = px(50.) + ENTRY_GAP + chip(10);
        assert_eq!(
            fold(&eleven, one_and_long_chip, false, chip),
            Fold::Leading { shown: 1 }
        );
    }

    #[test]
    fn a_legend_keying_nothing_shows_nothing_in_no_room() {
        assert_eq!(fold(&[], px(0.), false, chip), Fold::All);
    }

    #[test]
    fn a_strip_with_no_room_for_its_first_entry_stands_as_one_chip() {
        let widths = widths(&[200., 50.]);
        assert_eq!(fold(&widths, px(150.), false, chip), Fold::Whole);
        assert_eq!(chip_label(Fold::Whole, 2), "Legend");
    }

    #[test]
    fn notes_keep_a_chip_standing_where_every_entry_fits() {
        let widths = widths(&[50., 50.]);
        let fold = fold(&widths, px(1000.), true, chip);
        assert_eq!(fold, Fold::Leading { shown: 2 });
        assert_eq!(chip_label(fold, 2), "Notes");
        assert_eq!(chip_label(Fold::Leading { shown: 1 }, 3), "+2");
    }

    #[test]
    fn a_popover_takes_one_column_where_two_overflow_its_room() {
        let widths = widths(&[300., 100., 100.]);
        let edges = (POPOVER_INSET + menu_chrome::SHEET_BORDER) * 2.;
        let two = px(600.) + ENTRY_GAP + edges;
        assert_eq!(popover_columns(&widths, two), 2);
        assert_eq!(popover_columns(&widths, two - px(1.)), 1);
        assert_eq!(popover_columns(&widths, px(10.)), 1);
    }

    #[test]
    fn a_popover_takes_no_more_columns_than_entries() {
        assert_eq!(popover_columns(&widths(&[50.]), px(1000.)), 1);
        assert_eq!(popover_columns(&[], px(1000.)), 1);
    }

    #[test]
    fn the_chip_click_is_reachable_and_fits_the_keys_help() {
        assert_eq!(GESTURES.unreachable(), []);
        assert_eq!(GESTURES.overlong(), []);
    }
}
