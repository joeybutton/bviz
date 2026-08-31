//! The flags naming the profiles an action timeline is read from, and the
//! documents they resolve to. Nothing here opens a window or draws, so a parse
//! can be exercised without a display.

use clap::Args;
use std::path::PathBuf;

/// The profiles an action timeline is read from.
#[derive(Args, Debug)]
#[command(next_help_heading = "Timelines (one panel per file)")]
pub struct TraceArgs {
    /// `bazel build --profile=FILE` output (`.profile.gz` or `.json`).
    #[arg(long, value_name = "FILE")]
    pub profile: Vec<PathBuf>,
}

impl TraceArgs {
    pub fn sources(self) -> impl Iterator<Item = TraceSource> {
        self.profile.into_iter().map(|path| TraceSource { path })
    }
}

/// One action timeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceSource {
    pub path: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        trace: TraceArgs,
    }

    fn sources(args: &[&str]) -> Vec<TraceSource> {
        Cli::try_parse_from(std::iter::once("trace").chain(args.iter().copied()))
            .expect("the flags parse")
            .trace
            .sources()
            .collect()
    }

    #[test]
    fn a_repeated_profile_names_a_document_each() {
        assert_eq!(
            sources(&["--profile", "one.gz", "--profile", "two.gz"]),
            [
                TraceSource {
                    path: "one.gz".into()
                },
                TraceSource {
                    path: "two.gz".into()
                },
            ]
        );
    }

    #[test]
    fn naming_no_profile_names_no_document() {
        assert_eq!(sources(&[]), []);
    }
}
