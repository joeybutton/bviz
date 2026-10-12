//! Reader for `--build_event_binary_file` streams.
//!
//! The file is a sequence of length-delimited `BuildEvent` messages.

use std::io::{BufReader, Read};
use std::path::Path;

use bviz_invocation::{Configurations, InvocationId, Outcomes, Recorded};
use prost::Message;

use crate::bep::build_event_stream::{BuildEvent, build_event::Payload, build_event_id::Id};
use crate::bep::command_line::{CommandLine, command_line_section::SectionType};
use crate::bep::timestamp_proto::google::protobuf::Timestamp;
use crate::columns::Columns;
use crate::outcomes::{self, checksum};

/// What a build event stream tells of the build that wrote it.
pub struct BuildOutput {
    /// The configurations the stream describes (its `Configuration` events).
    pub configurations: Configurations,
    pub outcomes: Outcomes,
    /// Terminal width Bazel rendered for (`--terminal_columns`), or Bazel's
    /// fallback where the stream names none the emulator can run at.
    pub(crate) columns: Columns,
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
            configurations: Configurations::default(),
            outcomes: Outcomes::default(),
            columns: Columns::new(columns).expect("a width the emulator runs at"),
            frames: vec![bytes.len()],
            bytes,
            anchors: Vec::new(),
            show_timestamps: false,
        }
    }
}

/// Why a build event binary stream cannot be read.
#[derive(Debug)]
pub enum ParseError {
    Decode(prost::DecodeError),
    /// The stream holds no whole event, as no build event stream does.
    NoEvent,
}

impl From<prost::DecodeError> for ParseError {
    fn from(e: prost::DecodeError) -> Self {
        ParseError::Decode(e)
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Decode(e) => write!(f, "cannot parse build event stream: {e}"),
            ParseError::NoEvent => write!(f, "not a build event stream"),
        }
    }
}

impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ParseError::Decode(e) => Some(e),
            ParseError::NoEvent => None,
        }
    }
}

/// Read a build event binary stream: whole, where it runs to the event Bazel
/// marks its last, and up to where it ends otherwise. An event that does not
/// decode fails the read, as does a stream that holds no whole event.
pub fn parse(mut stream: &[u8]) -> Result<Recorded<BuildOutput>, ParseError> {
    let mut bytes = Vec::new();
    let mut frames = Vec::new();
    let mut anchors = Vec::new();
    let mut configurations = Configurations::default();
    let mut outcomes = Outcomes::default();
    let mut columns = None;
    let mut show_timestamps = false;
    let mut last_read = false;
    let mut any_event = false;
    let ended_whole = loop {
        let event = match next_event(&mut stream)? {
            Next::Event(event) => event,
            Next::End => break true,
            Next::CutOff => break false,
        };
        let BuildEvent {
            id,
            children,
            payload,
            last_message,
            ..
        } = BuildEvent::decode(event)?;
        any_event = true;
        last_read |= last_message;
        let id = id.and_then(|id| id.id);
        // An anchoring event ties its time to the frame boundary it falls on: the
        // number of progress frames already emitted.
        let frame = frames.len();
        match payload {
            Some(Payload::Progress(progress)) => {
                bytes.extend_from_slice(progress.stderr.as_bytes());
                bytes.extend_from_slice(progress.stdout.as_bytes());
                frames.push(bytes.len());
            }
            Some(Payload::Started(s)) => record(&mut anchors, frame, epoch_opt(s.start_time)),
            Some(Payload::Finished(f)) => record(&mut anchors, frame, epoch_opt(f.finish_time)),
            Some(Payload::Action(a)) => {
                record(&mut anchors, frame, epoch_opt(a.end_time));
                outcomes::action(&mut outcomes, id, a);
            }
            Some(Payload::TestSummary(s)) => {
                record(&mut anchors, frame, epoch_opt(s.last_stop_time));
                outcomes::test_summary(&mut outcomes, id, s);
            }
            Some(Payload::Completed(c)) => {
                outcomes::completed(&mut outcomes, id, children, c.success)
            }
            Some(Payload::Aborted(a)) => outcomes::aborted(&mut outcomes, id, children, a),
            Some(Payload::Configuration(c)) => {
                if let Some(Id::Configuration(id)) = id
                    && let Some(checksum) = checksum(Some(id))
                {
                    configurations.insert(checksum, c.mnemonic);
                }
            }
            Some(Payload::StructuredCommandLine(command_line)) => {
                columns = columns.or_else(|| terminal_columns(&command_line));
                show_timestamps |= show_timestamps_set(&command_line);
            }
            _ => {}
        }
    };
    if !any_event {
        return Err(ParseError::NoEvent);
    }
    let output = BuildOutput {
        configurations,
        outcomes,
        columns: columns.unwrap_or(Columns::DEFAULT),
        bytes,
        frames,
        anchors,
        show_timestamps,
    };
    Ok(match ended_whole && last_read {
        true => Recorded::Complete(output),
        false => Recorded::CutOff(output),
    })
}

/// How the head of a stream holds its next event.
enum Next<'a> {
    Event(&'a [u8]),
    End,
    /// The stream ends inside the event.
    CutOff,
}

/// The event at the head of `stream`, which it advances past the event.
fn next_event<'a>(stream: &mut &'a [u8]) -> Result<Next<'a>, prost::DecodeError> {
    if stream.is_empty() {
        return Ok(Next::End);
    }
    let delimiter_ends = stream
        .iter()
        .take(MAX_DELIMITER_BYTES)
        .any(|&b| b & VARINT_CONTINUES == 0);
    if !delimiter_ends && stream.len() < MAX_DELIMITER_BYTES {
        return Ok(Next::CutOff);
    }
    let length = prost::decode_length_delimiter(&mut *stream)?;
    if length > stream.len() {
        return Ok(Next::CutOff);
    }
    let (event, rest) = stream.split_at(length);
    *stream = rest;
    Ok(Next::Event(event))
}

/// Bytes [`probe_invocation_id`] reads from the head of a stream, many times
/// the `BuildStarted` event a stream opens with.
const PROBE_PREFIX_BYTES: u64 = 256 << 10;

/// The invocation that wrote the build event binary stream at `path`: the first
/// `BuildStarted.uuid` that is not empty, reading the file event by event up
/// to the one that names it, and no more than a bounded prefix. `None` when
/// that prefix holds no such event whole, the file cannot be read, or `path`
/// is not a regular file: reading a pipe could block, and would take from it
/// bytes it yields only once.
pub fn probe_invocation_id(path: &Path) -> Option<InvocationId> {
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    invocation_id_in_prefix(BufReader::new(file), PROBE_PREFIX_BYTES)
}

/// [`probe_invocation_id`] over the first `prefix_bytes` bytes of a stream.
fn invocation_id_in_prefix(stream: impl Read, prefix_bytes: u64) -> Option<InvocationId> {
    let mut prefix = stream.take(prefix_bytes);
    let mut event = Vec::new();
    loop {
        let length = read_length_delimiter(&mut prefix)?;
        if length as u64 > prefix.limit() {
            return None;
        }
        event.resize(length, 0);
        prefix.read_exact(&mut event).ok()?;
        if let Some(Payload::Started(started)) = BuildEvent::decode(event.as_slice()).ok()?.payload
            && let Some(id) = InvocationId::new(&started.uuid)
        {
            return Some(id);
        }
    }
}

/// The longest encoding of a length delimiter, a 64-bit varint.
const MAX_DELIMITER_BYTES: usize = 10;

/// Set on every byte of a varint but its last.
const VARINT_CONTINUES: u8 = 0x80;

/// The length of the next length-delimited message in `stream`, consuming its
/// delimiter. `None` when the stream ends first.
fn read_length_delimiter(stream: &mut impl Read) -> Option<usize> {
    let mut delimiter = [0u8; MAX_DELIMITER_BYTES];
    for end in 1..=MAX_DELIMITER_BYTES {
        stream.read_exact(&mut delimiter[end - 1..end]).ok()?;
        if delimiter[end - 1] & VARINT_CONTINUES == 0 {
            return prost::decode_length_delimiter(&delimiter[..end]).ok();
        }
    }
    None
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

fn terminal_columns(command_line: &CommandLine) -> Option<Columns> {
    option_value(command_line, "terminal_columns")
        .and_then(|value| value.parse().ok())
        .and_then(Columns::new)
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
    use crate::bep::build_event_stream::{self as bes, BuildEventId, BuildStarted, build_event_id};
    use crate::outcomes::NULL_CONFIGURATION;

    // A real `--build_event_binary_file` from `bazel build … --terminal_columns=100`.
    const SAMPLE: &[u8] = include_bytes!("../testdata/sample.bep");

    #[test]
    fn reads_columns_and_output() {
        let output = parse(SAMPLE).expect("parse sample stream").into_inner();
        assert_eq!(output.columns.get(), 100);
        // Bazel's curses progress display emits cursor-control escape sequences.
        assert!(
            output.bytes.contains(&0x1b),
            "expected ANSI escapes in output"
        );
    }

    /// A length-delimited `Configuration` event introducing `id`.
    fn configuration_event(id: &str, mnemonic: &str) -> Vec<u8> {
        BuildEvent {
            id: Some(BuildEventId {
                id: Some(Id::Configuration(build_event_id::ConfigurationId {
                    id: id.into(),
                })),
            }),
            payload: Some(Payload::Configuration(bes::Configuration {
                mnemonic: mnemonic.into(),
                ..Default::default()
            })),
            ..Default::default()
        }
        .encode_length_delimited_to_vec()
    }

    #[test]
    fn reads_configurations_by_checksum() {
        let output = parse(SAMPLE).expect("parse sample stream").into_inner();
        // The sample's two configurations share a mnemonic.
        let target = "55817e2200e2f24b506d68c96928ed7b9fa185d1fb462c4f511796269d8d06d4";
        let other = "91e24984663c795f7d9f904758d4b84ba1954389b2ab756f70a6f32db2a79020";
        let known = &output.configurations;
        assert_eq!(known.name(target).as_deref(), Some("k8-fastbuild 55817e"));
        assert_eq!(known.name(other).as_deref(), Some("k8-fastbuild 91e249"));
    }

    #[test]
    fn skips_a_configuration_event_that_names_no_configuration() {
        let stream = [
            configuration_event(NULL_CONFIGURATION, "null"),
            configuration_event("", "unnamed"),
            configuration_event("0c3d9a77", "k8-opt-exec"),
        ]
        .concat();
        let known = parse(&stream)
            .expect("parse stream")
            .into_inner()
            .configurations;
        assert_eq!(known.name(NULL_CONFIGURATION), None);
        assert_eq!(known.name(""), None);
        assert_eq!(known.name("0c3d9a77").as_deref(), Some("k8-opt-exec"));
    }

    /// A length-delimited `StructuredCommandLine` event naming
    /// `--terminal_columns=value`.
    fn terminal_columns_event(value: &str) -> Vec<u8> {
        use crate::bep::command_line::{self as cl, CommandLineSection};
        BuildEvent {
            payload: Some(Payload::StructuredCommandLine(CommandLine {
                sections: vec![CommandLineSection {
                    section_type: Some(SectionType::OptionList(cl::OptionList {
                        option: vec![cl::Option {
                            option_name: "terminal_columns".into(),
                            option_value: value.into(),
                            ..Default::default()
                        }],
                    })),
                    ..Default::default()
                }],
                ..Default::default()
            })),
            ..Default::default()
        }
        .encode_length_delimited_to_vec()
    }

    /// A length-delimited `Progress` event writing `stderr`.
    fn progress_event(stderr: &str) -> Vec<u8> {
        BuildEvent {
            payload: Some(Payload::Progress(bes::Progress {
                stderr: stderr.into(),
                ..Default::default()
            })),
            ..Default::default()
        }
        .encode_length_delimited_to_vec()
    }

    #[test]
    fn falls_back_from_a_width_the_emulator_cannot_hold() {
        for width in ["0", "1", "65535", "-3", "wide"] {
            let stream = [terminal_columns_event(width), progress_event("built\n")].concat();
            let output = parse(&stream).expect("parse stream").into_inner();
            assert_eq!(
                output.columns,
                Columns::DEFAULT,
                "--terminal_columns={width}"
            );
            let screen = crate::screen::Screen::from_output(&output);
            assert_eq!(screen.text(), "built", "--terminal_columns={width}");
        }
    }

    #[test]
    fn takes_the_first_width_the_emulator_can_hold() {
        let stream = [terminal_columns_event("0"), terminal_columns_event("120")].concat();
        assert_eq!(
            parse(&stream)
                .expect("parse stream")
                .into_inner()
                .columns
                .get(),
            120
        );
    }

    /// The offset each of the sample's events ends at, in stream order.
    fn sample_event_ends() -> Vec<usize> {
        let mut rest = SAMPLE;
        let mut ends = Vec::new();
        while !rest.is_empty() {
            BuildEvent::decode_length_delimited(&mut rest).expect("sample event");
            ends.push(SAMPLE.len() - rest.len());
        }
        ends
    }

    #[test]
    fn reads_a_stream_ending_in_its_last_event_whole() {
        assert!(matches!(parse(SAMPLE), Ok(Recorded::Complete(_))));
    }

    #[test]
    fn a_stream_cut_off_keeps_the_events_ahead_of_the_cut() {
        let cut_off = |cut: usize| match parse(&SAMPLE[..cut]) {
            Ok(Recorded::CutOff(output)) => output,
            _ => panic!("a stream cut at {cut} is cut off"),
        };
        let ends = sample_event_ends();
        // Each event after the first but the last, which Bazel marks the last,
        // cut before, inside, and right after its start.
        for (&start, &end) in ends.iter().zip(&ends[1..ends.len() - 1]) {
            let kept = cut_off(start);
            for cut in [start + 1, (start + end) / 2, end - 1] {
                let output = cut_off(cut);
                assert_eq!(output.bytes, kept.bytes, "cut at {cut}");
                assert_eq!(output.frames, kept.frames, "cut at {cut}");
            }
        }
    }

    #[test]
    fn a_file_that_holds_no_whole_event_is_no_stream() {
        const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
        let first_end = sample_event_ends()[0];
        for stream in [
            &[][..],
            &SAMPLE[..1],
            &SAMPLE[..first_end - 1],
            PNG_SIGNATURE,
        ] {
            assert!(
                matches!(parse(stream), Err(ParseError::NoEvent)),
                "{stream:?}"
            );
        }
    }

    #[test]
    fn an_event_that_does_not_decode_fails_the_read() {
        let mut stream = progress_event("built\n");
        let garbled = [0xff; 3];
        prost::encode_length_delimiter(garbled.len(), &mut stream).expect("delimiter");
        stream.extend(garbled);
        assert!(parse(&stream).is_err());
    }

    #[test]
    fn a_delimiter_running_past_its_longest_encoding_fails_the_read() {
        let stream = [VARINT_CONTINUES; MAX_DELIMITER_BYTES + 1];
        assert!(parse(&stream).is_err());
    }

    #[test]
    fn probe_reads_the_id_within_its_prefix() {
        let id = InvocationId::new("a5f7f3e6-4fda-4675-bc16-8ede5420b210");
        let whole = SAMPLE.len() as u64;
        assert_eq!(invocation_id_in_prefix(SAMPLE, whole), id);

        // `BuildStarted` is the sample's first event; a prefix ending inside
        // it holds no id, and one ending right after it does.
        let mut rest = SAMPLE;
        BuildEvent::decode_length_delimited(&mut rest).expect("first event");
        let first_event = (SAMPLE.len() - rest.len()) as u64;
        assert_eq!(invocation_id_in_prefix(SAMPLE, first_event - 1), None);
        assert_eq!(invocation_id_in_prefix(SAMPLE, first_event), id);

        // Events ahead of `BuildStarted` are passed over.
        let mut late = configuration_event("0c3d9a77", "k8-opt-exec");
        let events_ahead = late.len() as u64;
        late.extend(SAMPLE);
        assert_eq!(invocation_id_in_prefix(late.as_slice(), whole), id);
        assert_eq!(invocation_id_in_prefix(late.as_slice(), events_ahead), None);
        assert_eq!(invocation_id_in_prefix(&[][..], PROBE_PREFIX_BYTES), None);
    }

    /// A length-delimited `BuildStarted` event of the invocation `uuid`.
    fn started_event(uuid: &str) -> Vec<u8> {
        BuildEvent {
            payload: Some(Payload::Started(BuildStarted {
                uuid: uuid.into(),
                ..Default::default()
            })),
            ..Default::default()
        }
        .encode_length_delimited_to_vec()
    }

    #[test]
    fn probe_reads_the_first_id_that_is_not_empty() {
        let stream = ["", "first", "second"].map(started_event).concat();
        assert_eq!(
            invocation_id_in_prefix(stream.as_slice(), stream.len() as u64),
            InvocationId::new("first")
        );
    }

    #[test]
    fn probe_stops_at_an_event_longer_than_its_prefix() {
        let mut stream = Vec::new();
        prost::encode_length_delimiter(usize::MAX, &mut stream).expect("delimiter");
        stream.extend(started_event("late"));
        assert_eq!(
            invocation_id_in_prefix(stream.as_slice(), PROBE_PREFIX_BYTES),
            None
        );
    }

    #[test]
    fn probe_reads_a_stream_from_disk() {
        let dir = std::env::var("TEST_TMPDIR").unwrap_or_else(|_| "/tmp".into());
        let path = Path::new(&dir).join("probe.bep");
        std::fs::write(&path, SAMPLE).unwrap();
        assert_eq!(
            probe_invocation_id(&path),
            InvocationId::new("a5f7f3e6-4fda-4675-bc16-8ede5420b210")
        );
        assert_eq!(probe_invocation_id(&path.with_extension("missing")), None);
        assert_eq!(probe_invocation_id(Path::new(&dir)), None);
    }

    #[test]
    fn frames_partition_the_output() {
        let output = parse(SAMPLE).expect("parse sample stream").into_inner();
        // Every progress event yields a frame, and frame ends are non-decreasing
        // offsets running up to the full output.
        assert!(!output.frames.is_empty());
        assert!(output.frames.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(output.frames.last().copied(), Some(output.bytes.len()));
    }

    #[test]
    fn detects_show_timestamps_off() {
        // The sample was captured without --show_timestamps.
        let output = parse(SAMPLE).expect("parse sample stream").into_inner();
        assert!(!output.show_timestamps);
    }

    #[test]
    fn anchors_bracket_the_build() {
        let output = parse(SAMPLE).expect("parse sample stream").into_inner();
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
