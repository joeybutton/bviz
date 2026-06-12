//! Reusable, window-agnostic widgets: a text field and a button.

mod button;
pub mod text_input;

pub use button::Button;
pub use text_input::TextInput;

use gpui::App;

/// Register the key bindings the components rely on (currently the text field's).
pub fn register_keys(cx: &mut App) {
    text_input::bind_keys(cx);
}
