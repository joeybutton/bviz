//! A document's playback clock, which every view of the document follows.

mod clock;

pub use clock::{Clock, Source, SourceId, Span, Speed, Timeline};
