//! Reader for `--build_event_binary_file` streams.
//!
//! The file is a sequence of length-delimited `BuildEvent` messages. This pulls
//! out the console output and the terminal width Bazel rendered it for, which is
//! everything needed to replay the build into a terminal emulator.

use prost::Message;

use crate::bep::build_event_stream::{BuildEvent, build_event::Payload};
use crate::bep::command_line::{CommandLine, command_line_section::SectionType};
use crate::bep::timestamp_proto::google::protobuf::Timestamp;

/// Console output captured from a build, with the terminal width Bazel used and
/// the structure needed to replay it over time.
pub struct BuildOutput {
    /// Terminal width in columns Bazel rendered for (`--terminal_columns`).
    pub columns: u16,
    /// stdout and stderr concatenated in stream order. `bazel build` writes
    /// everything — progress, messages, forwarded action output — to stderr and
    /// leaves stdout empty; the query-family commands (`query`/`cquery`/`aquery`/
    /// `info`) emit their results on stdout instead. Within one progress event
    /// stderr is appended before stdout, keeping progress ahead of any such results.
    pub bytes: Vec<u8>,
    /// The end offset into [`bytes`](Self::bytes) of each progress event, in stream
    /// order: replaying `bytes[..frames[i]]` reproduces the terminal as it stood
    /// after the i-th progress event. One entry per progress event; the last is
    /// `bytes.len()`. This is the unit of playback — each progress event is a frame.
    pub(crate) frames: Vec<usize>,
    /// Wall-clock anchors recovered from the timestamp-bearing events, each tying a
    /// time to the point in the frame sequence where the event occurred. In stream
    /// order. Bazel does not timestamp progress events themselves, so these anchor
    /// an interpolated timeline when the output carries no `--show_timestamps`
    /// markers; see [`crate::Replay`].
    pub(crate) anchors: Vec<Anchor>,
    /// Whether the build was run with `--show_timestamps`, read from its structured
    /// command line. When set, the output carries `(HH:MM:SS)` markers that give a
    /// real timeline; otherwise the timeline is interpolated from [`anchors`].
    pub(crate) show_timestamps: bool,
}

/// A wall-clock time tied to a position in the progress-frame sequence.
pub(crate) struct Anchor {
    /// The number of progress frames emitted before the anchoring event, i.e. an
    /// index into [`BuildOutput::frames`].
    pub(crate) frame: usize,
    /// The event's wall-clock time, in seconds since the Unix epoch.
    pub(crate) time: f64,
}

#[cfg(test)]
impl BuildOutput {
    /// A single-frame output that carries no clock: the base whose fields a
    /// test overrides.
    pub(crate) fn from_bytes(columns: u16, bytes: Vec<u8>) -> BuildOutput {
        BuildOutput {
            columns,
            frames: vec![bytes.len()],
            bytes,
            anchors: Vec::new(),
            show_timestamps: false,
        }
    }
}

/// Width Bazel falls back to when no terminal is detected; see `--terminal_columns`.
const DEFAULT_COLUMNS: u16 = 80;

/// Parse the console output, terminal width, frame boundaries, and timestamp
/// anchors from a build event binary stream.
pub fn parse(mut stream: &[u8]) -> Result<BuildOutput, prost::DecodeError> {
    let mut bytes = Vec::new();
    let mut frames = Vec::new();
    let mut anchors = Vec::new();
    let mut columns = None;
    let mut show_timestamps = false;
    while !stream.is_empty() {
        let event = BuildEvent::decode_length_delimited(&mut stream)?;
        // An anchoring event ties its time to the frame boundary it falls on: the
        // number of progress frames already emitted.
        let frame = frames.len();
        match event.payload {
            Some(Payload::Progress(progress)) => {
                bytes.extend_from_slice(progress.stderr.as_bytes());
                bytes.extend_from_slice(progress.stdout.as_bytes());
                frames.push(bytes.len());
            }
            Some(Payload::Started(s)) => record(&mut anchors, frame, epoch_opt(s.start_time)),
            Some(Payload::Finished(f)) => record(&mut anchors, frame, epoch_opt(f.finish_time)),
            Some(Payload::Action(a)) => record(&mut anchors, frame, epoch_opt(a.end_time)),
            Some(Payload::TestSummary(s)) => {
                record(&mut anchors, frame, epoch_opt(s.last_stop_time))
            }
            Some(Payload::StructuredCommandLine(command_line)) => {
                columns = columns.or_else(|| terminal_columns(&command_line));
                show_timestamps |= show_timestamps_set(&command_line);
            }
            _ => {}
        }
    }
    Ok(BuildOutput {
        columns: columns.unwrap_or(DEFAULT_COLUMNS),
        bytes,
        frames,
        anchors,
        show_timestamps,
    })
}

/// Record a wall-clock anchor at a frame boundary, skipping events whose
/// timestamp is absent.
fn record(anchors: &mut Vec<Anchor>, frame: usize, time: Option<f64>) {
    if let Some(time) = time {
        anchors.push(Anchor { frame, time });
    }
}

/// Seconds since the Unix epoch for a `google.protobuf.Timestamp`'s components.
fn epoch(seconds: i64, nanos: i32) -> f64 {
    seconds as f64 + nanos as f64 / 1e9
}

fn epoch_opt(ts: Option<Timestamp>) -> Option<f64> {
    ts.map(|t| epoch(t.seconds, t.nanos))
}

/// The value of a named option in a structured command line, if present.
fn option_value<'a>(command_line: &'a CommandLine, name: &str) -> Option<&'a str> {
    command_line
        .sections
        .iter()
        .filter_map(|section| match &section.section_type {
            Some(SectionType::OptionList(options)) => Some(options),
            _ => None,
        })
        .flat_map(|options| &options.option)
        .find(|option| option.option_name == name)
        .map(|option| option.option_value.as_str())
}

fn terminal_columns(command_line: &CommandLine) -> Option<u16> {
    option_value(command_line, "terminal_columns").and_then(|value| value.parse().ok())
}

/// Whether `--show_timestamps` is enabled, as a boolean option Bazel canonicalizes
/// to `1`/`true`.
fn show_timestamps_set(command_line: &CommandLine) -> bool {
    option_value(command_line, "show_timestamps")
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A real `--build_event_binary_file` from `bazel build … --terminal_columns=100`.
    const SAMPLE: &[u8] = include_bytes!("../testdata/sample.bep");

    #[test]
    fn reads_columns_and_output() {
        let output = parse(SAMPLE).expect("parse sample stream");
        assert_eq!(output.columns, 100);
        // Bazel's curses progress display emits cursor-control escape sequences.
        assert!(
            output.bytes.contains(&0x1b),
            "expected ANSI escapes in output"
        );
    }

    #[test]
    fn frames_partition_the_output() {
        let output = parse(SAMPLE).expect("parse sample stream");
        // Every progress event yields a frame, and frame ends are non-decreasing
        // offsets running up to the full output.
        assert!(!output.frames.is_empty());
        assert!(output.frames.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(output.frames.last().copied(), Some(output.bytes.len()));
    }

    #[test]
    fn detects_show_timestamps_off() {
        // The sample was captured without --show_timestamps.
        let output = parse(SAMPLE).expect("parse sample stream");
        assert!(!output.show_timestamps);
    }

    #[test]
    fn anchors_bracket_the_build() {
        let output = parse(SAMPLE).expect("parse sample stream");
        // A completed build carries at least its start and finish times, in
        // non-decreasing wall-clock order, each tied to a real frame boundary.
        assert!(
            output.anchors.len() >= 2,
            "expected start and finish anchors"
        );
        assert!(output.anchors.windows(2).all(|w| w[0].time <= w[1].time));
        assert!(
            output
                .anchors
                .iter()
                .all(|a| a.frame <= output.frames.len())
        );
    }
}
