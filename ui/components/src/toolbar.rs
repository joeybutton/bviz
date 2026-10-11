use crate::CONTROL_HEIGHT;
use gpui::{
    AnyElement, App, Div, ElementId, IntoElement, Pixels, RenderOnce, Window, div, prelude::*, px,
};

/// Space the strip leaves above and below its controls.
const PADDING: Pixels = px(4.);

/// The rule drawn along the strip's foot.
const RULE: Pixels = px(1.);

/// How much of a control's height a [`ToolbarSeparator`] spans, short of the
/// whole so it parts the groups beside it without boxing them in.
const SEPARATOR_SPAN: f32 = 0.6;

/// A horizontal strip of controls acting on the content below it.
#[derive(IntoElement, Default)]
pub struct Toolbar {
    leading: Vec<AnyElement>,
    trailing: Vec<AnyElement>,
}

impl Toolbar {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add to the group held against the left edge. The group wraps onto as many
    /// rows as it takes to hold what it is given, so a control is never cut off
    /// at the window edge however narrow the window, unless the strip is held
    /// to [`one_row`](Self::one_row).
    pub fn child(mut self, child: impl IntoElement) -> Self {
        self.leading.push(child.into_any_element());
        self
    }

    /// Holds the strip to one row. `id` keeps the leading group's scroll.
    pub fn one_row(self, id: impl Into<ElementId>) -> OneRow {
        OneRow {
            strip: self,
            scroll: id.into(),
        }
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
        strip(leading, self.trailing, window, cx)
    }
}

/// A [`Toolbar`] held to one row, [`height`](Self::height) tall, its leading
/// group scrolling sideways where it outgrows the strip, so that what the strip
/// holds never moves the content below it.
#[derive(IntoElement)]
pub struct OneRow {
    strip: Toolbar,
    scroll: ElementId,
}

impl OneRow {
    /// The strip's height, so a layout can reserve its room without building it.
    pub fn height() -> Pixels {
        CONTROL_HEIGHT + PADDING * 2. + RULE
    }

    /// As [`Toolbar::child`], the group held to one row.
    pub fn child(mut self, child: impl IntoElement) -> Self {
        self.strip = self.strip.child(child);
        self
    }

    /// As [`Toolbar::trailing`].
    pub fn trailing(mut self, child: impl IntoElement) -> Self {
        self.strip = self.strip.trailing(child);
        self
    }
}

impl RenderOnce for OneRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let leading = group(self.strip.leading)
            .id(self.scroll)
            .h(CONTROL_HEIGHT)
            .flex_1()
            .min_w_0()
            .overflow_x_scroll();
        strip(leading, self.strip.trailing, window, cx).h(Self::height())
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

/// The strip around its `leading` group and its `trailing` controls.
fn strip(
    leading: impl IntoElement,
    trailing: Vec<AnyElement>,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let colors = bviz_ui_theme::theme(window, cx).colors;
    div()
        .flex()
        .flex_row()
        .items_start()
        .gap_2()
        // Hold the strip's height in a flex column, so a growing sibling
        // (the content it acts on) cannot squeeze the controls.
        .flex_none()
        .py(PADDING)
        .px_2()
        .bg(colors.surface)
        .border_b(RULE)
        .border_color(colors.border)
        .child(leading)
        .child(group(trailing).flex_none())
}

/// One group's controls, each row of it as tall as a control so the two groups
/// line up on the strip's first row.
fn group(items: Vec<AnyElement>) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .min_h(CONTROL_HEIGHT)
        .children(items)
}
