//! Reusable, window-agnostic widgets (a text field, a button, and a dropdown) and
//! gpui interaction helpers.

mod background;
mod button;
mod drag;
mod dropdown;
mod scrollbar;
pub mod text_input;

pub use background::load_in_background;
pub use button::Button;
pub use drag::track_drag;
pub use dropdown::{Dropdown, DropdownEvent};
pub use scrollbar::{Axis, Scrollbar};
pub use text_input::TextInput;

use gpui::App;

/// Register the key bindings the components rely on (currently the text field's).
pub fn register_keys(cx: &mut App) {
    text_input::bind_keys(cx);
}
