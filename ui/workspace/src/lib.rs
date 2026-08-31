//! The window's root: a [`Workspace`] framing one [`Panel`] in the application
//! chrome, and the erased handle ([`AnyPanel`]) it holds the panel through.

mod panel;
mod workspace;

pub use panel::{AnyPanel, Panel, PanelEvent, PanelKind};
pub use workspace::Workspace;
