use crate::spinner;
use bviz_ui_theme::Colors;
use gpui::{Div, SharedString, div, prelude::*, px};

/// Shown while work is in flight.
pub fn centered_spinner(label: impl Into<SharedString>, colors: &Colors) -> Div {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .justify_center()
        .items_center()
        .gap_3()
        .child(spinner("spinner", px(36.), colors.text))
        .child(div().text_sm().text_color(colors.disabled).child(label.into()))
}

/// Shown in place of content that is absent or failed to load.
pub fn centered_notice(message: impl Into<SharedString>, colors: &Colors) -> Div {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .justify_center()
        .items_center()
        .gap_4()
        .p_4()
        .child(
            div()
                .max_w(px(400.))
                .text_sm()
                .text_color(colors.disabled)
                .child(message.into()),
        )
}
