//! Reusable, window-agnostic widgets and gpui interaction helpers.

mod background;
mod button;
mod canvas;
mod divider;
mod drag;
mod dropdown;
pub mod gesture;
mod key_scope;
mod keys;
mod keys_help;
mod layer;
mod list_scroll;
mod menu;
pub mod menu_chrome;
mod multi_select;
mod notice;
mod query_field;
mod scrollbar;
mod scrubber;
mod sort;
mod spinner;
pub mod text_input;
mod toolbar;
mod tooltip;
mod zoom_keys;

pub use background::{Pending, load_in_background};
pub use button::Button;
pub use canvas::{LabelBoxStyle, paint_labeled_box, shape_truncated};
pub use divider::{Divider, DividerSide};
pub use drag::{DragEnd, track_drag};
pub use dropdown::{Dropdown, DropdownEvent};
pub use key_scope::{KeyScope, Nesting, UNSCOPED_LABEL, declare_scopes, declared_scopes};
pub use keys::{KeyStyle, action_label, binding_text, key_text, key_text_in, keystroke_text};
pub use keys_help::{
    GestureGroup, KEYS_HELP_NAME, KeyGroup, KeyRow, KeysHelp, KeysHelpEvent, keys_in_effect,
};
pub use layer::{cached_layer, overlay_canvas};
pub use list_scroll::{ListScroll, list_scrollbar};
pub use menu::{Menu, MenuEvent, MenuItem, SubmenuItem};
pub use multi_select::{MultiOption, MultiSelect, MultiSelectEvent};
pub use notice::{Source, centered_notice, centered_spinner};
pub use query_field::{QueryField, QueryFieldEvent};
pub use scrollbar::{Axis, DEFAULT_MIN_THUMB, SCROLLBAR_THICKNESS, Scrollbar, ScrollbarColors};
pub use scrubber::{ScrubberColors, ScrubberState, scrubber};
pub use sort::{Sort, sort_gesture, sort_header};
pub use spinner::spinner;
pub use text_input::TextInput;
pub use toolbar::Toolbar;
pub use tooltip::{
    TextTooltip, TooltipColors, beside_anchor, paint_tooltip, paint_tooltip_box, room_beside,
    tooltip_extent,
};
pub use zoom_keys::{ZOOM_KEY_STEP, ZoomIn, ZoomOut, zoom_key_bindings};

use gpui::{App, Pixels, px};

/// Height every control renders at, so controls sharing a row line up.
pub(crate) const CONTROL_HEIGHT: Pixels = px(28.);

/// Room added past a measured text width, so that sub-pixel rounding between
/// measuring a line and laying the glyph run out cannot push it onto a second
/// line.
const TEXT_SLACK: Pixels = px(6.);

/// The width to lay a line of text out at, from the width it measured.
pub(crate) fn laid_out_width(measured: Pixels) -> Pixels {
    px(f32::from(measured).ceil()) + TEXT_SLACK
}

/// Register the key bindings the components rely on.
pub fn register_keys(cx: &mut App) {
    declare_scopes(
        cx,
        [
            text_input::KEY_CONTEXT,
            multi_select::KEY_CONTEXT,
            keys_help::KEY_CONTEXT,
        ],
    );
    keys_help::bind_keys(cx);
    text_input::bind_keys(cx);
    multi_select::bind_keys(cx);
}
