//! The keys that zoom a canvas about the middle of its view, shared so that
//! every canvas answers the same keys.

use crate::KeyScope;
use crate::text_input;
use gpui::{KeyBinding, actions};

actions!(
    bviz_zoom_keys,
    [
        /// Zoom in
        ZoomIn,
        /// Zoom out
        ZoomOut,
    ]
);

/// How many times closer one press of [`ZoomIn`] brings a view; a press of
/// [`ZoomOut`] takes it as many times farther out.
pub const ZOOM_KEY_STEP: f32 = 1.25;

/// The zoom keys of a canvas carrying `scope`, which stand down while a text
/// field in it holds the focus.
pub fn zoom_key_bindings(scope: KeyScope) -> [KeyBinding; 3] {
    let predicate = format!("{scope} && !{}", text_input::KEY_CONTEXT);
    let predicate = Some(predicate.as_str());
    [
        KeyBinding::new("=", ZoomIn, predicate),
        KeyBinding::new("+", ZoomIn, predicate),
        KeyBinding::new("-", ZoomOut, predicate),
    ]
}
