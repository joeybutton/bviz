//! Reusable, window-agnostic widgets (a text field, a button, and a dropdown) and
//! gpui interaction helpers.

mod background;
mod button;
mod canvas;
mod drag;
mod dropdown;
mod scrollbar;
mod scrubber;
mod spinner;
pub mod text_input;
mod toggle;

pub use background::load_in_background;
pub use button::Button;
pub use canvas::{paint_labeled_box, shape_truncated};
pub use drag::track_drag;
pub use dropdown::{Dropdown, DropdownEvent};
pub use scrollbar::{Axis, Scrollbar};
pub use scrubber::{ScrubberLayout, scrubber};
pub use spinner::spinner;
pub use text_input::TextInput;
pub use toggle::toggle_pill;

use gpui::App;

/// Register the key bindings the components rely on (currently the text field's).
pub fn register_keys(cx: &mut App) {
    text_input::bind_keys(cx);
}
