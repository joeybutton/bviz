//! Reader for `--build_event_binary_file` streams.
//!
//! The file is a sequence of length-delimited `BuildEvent` messages. This pulls
//! out the console output and the terminal width Bazel rendered it for, which is
//! everything needed to replay the build into a terminal emulator.

use prost::Message;

use crate::bep::build_event_stream::{BuildEvent, build_event::Payload};
use crate::bep::command_line::{CommandLine, command_line_section::SectionType};

/// Console output captured from a build, with the terminal width Bazel used.
pub struct BuildOutput {
    /// Terminal width in columns Bazel rendered for (`--terminal_columns`).
    pub columns: u16,
    /// stdout and stderr concatenated in stream order; within one progress event
    /// stderr precedes stdout, matching Bazel's emission order, so the bytes replay
    /// into a terminal as the build wrote them.
    pub bytes: Vec<u8>,
}

/// Width Bazel falls back to when no terminal is detected; see `--terminal_columns`.
const DEFAULT_COLUMNS: u16 = 80;

/// Parse the console output and terminal width from a build event binary stream.
pub fn parse(mut stream: &[u8]) -> Result<BuildOutput, prost::DecodeError> {
    let mut bytes = Vec::new();
    let mut columns = None;
    while !stream.is_empty() {
        let event = BuildEvent::decode_length_delimited(&mut stream)?;
        match event.payload {
            Some(Payload::Progress(progress)) => {
                bytes.extend_from_slice(progress.stderr.as_bytes());
                bytes.extend_from_slice(progress.stdout.as_bytes());
            }
            Some(Payload::StructuredCommandLine(command_line)) => {
                columns = columns.or_else(|| terminal_columns(&command_line));
            }
            _ => {}
        }
    }
    Ok(BuildOutput {
        columns: columns.unwrap_or(DEFAULT_COLUMNS),
        bytes,
    })
}

/// Find the `terminal_columns` option in a structured command line.
fn terminal_columns(command_line: &CommandLine) -> Option<u16> {
    command_line
        .sections
        .iter()
        .filter_map(|section| match &section.section_type {
            Some(SectionType::OptionList(options)) => Some(options),
            _ => None,
        })
        .flat_map(|options| &options.option)
        .find(|option| option.option_name == "terminal_columns")
        .and_then(|option| option.option_value.parse().ok())
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
}
