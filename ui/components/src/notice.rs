use crate::spinner;
use bviz_ui_theme::Colors;
use gpui::{Div, SharedString, div, prelude::*, px};

/// How a load in flight is labelled on screen.
const LOADING: &str = "Loading…";

/// The load state of the document a view renders.
pub enum Source<T> {
    Loading,
    Loaded(T),
    /// Why no document is shown, worded for the reader.
    Message(String),
}

impl<T> Source<T> {
    /// What the view draws in place of the document until one is loaded, and
    /// `None` once one is.
    pub fn placeholder(&self, colors: &Colors) -> Option<Div> {
        match self {
            Self::Loading => Some(centered_spinner(LOADING, colors)),
            Self::Loaded(_) => None,
            Self::Message(msg) => Some(centered_notice(msg.clone(), colors)),
        }
    }

    /// The status line for this state: the load label while one is in flight,
    /// `loaded` describing the document once one is loaded, and `absent` naming
    /// what is missing when a message stands in the document's place.
    pub fn status(&self, absent: &str, loaded: impl FnOnce(&T) -> String) -> String {
        match self {
            Self::Loading => LOADING.into(),
            Self::Loaded(document) => loaded(document),
            Self::Message(_) => absent.into(),
        }
    }
}

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
