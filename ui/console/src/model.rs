use bviz_terminal::Replay;
use bviz_ui_components::{Pending, load_in_background};
use gpui::{Context, EventEmitter, Task};

pub(crate) type Source = bviz_ui_components::Source<Replay>;

/// The read has landed, loaded or failed. Carried as an event because what it
/// calls for is view-local — putting the playhead on the build that landed — and
/// owed once, on the landing.
pub(crate) struct SourceReplaced;

/// A build's captured console output, held apart from the panels replaying it so
/// a view of it can close and be shown again without the build event stream
/// being read and reconstructed afresh.
pub(crate) struct ConsoleModel {
    source: Source,
    /// The read that fills `source`, held so that it is not cancelled.
    _load: Task<()>,
}

impl EventEmitter<SourceReplaced> for ConsoleModel {}

impl ConsoleModel {
    /// The build `replay` is reading, playable once the read finishes and failed
    /// where it does not.
    pub(crate) fn reading(replay: Pending<Replay>, cx: &mut Context<Self>) -> Self {
        Self::new(load_in_background(
            cx,
            move || replay.join(),
            |model, result, cx| {
                model.source = match result {
                    Ok(replay) => Source::Loaded(replay),
                    Err(err) => Source::Failed(err),
                };
                cx.emit(SourceReplaced);
            },
        ))
    }

    fn new(load: Task<()>) -> Self {
        Self {
            source: Source::Loading,
            _load: load,
        }
    }

    pub(crate) fn source(&self) -> &Source {
        &self.source
    }

    /// The timeline a view plays, and `None` while the read has landed none, for
    /// which a view draws [`Source::placeholder`] in its stead.
    pub(crate) fn replay(&self) -> Option<&Replay> {
        match &self.source {
            Source::Loaded(replay) => Some(replay),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A build whose read has not finished, for exercising what a view reads
    /// off it before one has.
    fn reading() -> ConsoleModel {
        ConsoleModel::new(Task::ready(()))
    }

    /// One `BuildEvent` carrying a `Progress` payload whose `stderr` is `text`,
    /// length-delimited as a `--build_event_binary_file` carries it. Bazel emits
    /// the build's console output as a run of these, one per progress event, and
    /// each is one frame of the replay.
    fn progress_event(text: &str) -> Vec<u8> {
        const PROGRESS_IN_EVENT: u8 = 3;
        const STDERR_IN_PROGRESS: u8 = 2;
        // Field tag byte: the field number, then the wire type of a
        // length-delimited field.
        let tag = |field: u8| (field << 3) | 2;
        let progress = [
            &[tag(STDERR_IN_PROGRESS), text.len() as u8],
            text.as_bytes(),
        ]
        .concat();
        let event = [
            &[tag(PROGRESS_IN_EVENT), progress.len() as u8],
            &progress[..],
        ]
        .concat();
        [&[event.len() as u8], &event[..]].concat()
    }

    /// A two-frame build, read the way one named on the command line is read.
    fn loaded() -> ConsoleModel {
        let stream = [progress_event("Loading: \n"), progress_event("Analyzing\n")].concat();
        let output = bviz_terminal::parse(&stream).expect("the stream parses");
        let mut model = reading();
        model.source = Source::Loaded(Replay::from_output(output));
        model
    }

    /// What a view plays is read off the build alone, so it answers the same
    /// whether or not a panel is showing one.
    #[test]
    fn a_build_still_being_read_has_nothing_to_play() {
        assert!(reading().replay().is_none());
    }

    /// A read that failed leaves nothing to play and holds on to why, which is
    /// what a view shows in the timeline's place.
    #[test]
    fn a_build_that_could_not_be_read_says_why() {
        let mut model = reading();
        model.source = Source::Failed("build.bep: no such file".into());
        assert!(model.replay().is_none());
        let Source::Failed(why) = model.source() else {
            panic!("the read failed")
        };
        assert_eq!(why, "build.bep: no such file");
    }

    /// Every progress event of the stream read is a frame the reader can seek to.
    #[test]
    fn a_loaded_build_plays_what_was_read() {
        let model = loaded();
        let replay = model.replay().expect("the read landed a build");
        assert_eq!(replay.frame_count(), 2);
    }
}
