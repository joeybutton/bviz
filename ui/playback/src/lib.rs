//! A document's playback clock, which every view of the document follows, and
//! the bar that drives it.

mod bar;
mod clock;
mod note;

pub use bar::PlaybackBar;
pub use clock::{Clock, DocumentClock, OnClock, Source, SourceId, Span, Speed, Timeline};
pub use note::unaligned_note;

use gpui::{Pixels, px};

/// Size of the text the playback controls and notes are set in.
const TEXT_SIZE: Pixels = px(12.);
