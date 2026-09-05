//! The flags naming the build event streams a console replay is read from, and
//! the documents they resolve to. Nothing here opens a window or draws, so a
//! parse can be exercised without a display.

use clap::Args;
use std::path::PathBuf;

/// The build event streams a console replay is read from.
#[derive(Args, Debug)]
#[command(next_help_heading = "Console replays")]
pub struct ConsoleArgs {
    /// `bazel build --build_event_binary_file=FILE` output.
    #[arg(long, value_name = "FILE")]
    pub bep: Vec<PathBuf>,
}

impl ConsoleArgs {
    pub fn sources(self) -> impl Iterator<Item = ConsoleSource> {
        self.bep.into_iter().map(|path| ConsoleSource { path })
    }
}

/// One console replay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleSource {
    pub path: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        console: ConsoleArgs,
    }

    fn sources(args: &[&str]) -> Vec<ConsoleSource> {
        Cli::try_parse_from(std::iter::once("terminal").chain(args.iter().copied()))
            .expect("the flags parse")
            .console
            .sources()
            .collect()
    }

    #[test]
    fn a_repeated_bep_names_a_document_each() {
        assert_eq!(
            sources(&["--bep", "one.bep", "--bep", "two.bep"]),
            [
                ConsoleSource {
                    path: "one.bep".into()
                },
                ConsoleSource {
                    path: "two.bep".into()
                },
            ]
        );
    }

    #[test]
    fn naming_no_stream_names_no_document() {
        assert_eq!(sources(&[]), []);
    }
}
