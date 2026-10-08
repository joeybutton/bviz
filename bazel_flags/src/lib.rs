//! The Bazel flags bviz reads a build through, each spelled once, and the
//! bazelrc that passes them all under one `--config`.

mod rc;

use std::fmt;

pub use rc::{CaptureDir, ConfigName, InvalidCaptureDir, InvalidConfigName, header, settings};

/// A flag as Bazel's command line spells it, value included.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Flag(&'static str);

impl Flag {
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for Flag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

const STREAMED_PROTO: Flag = Flag("--output=streamed_proto");
/// Spells external repositories' labels canonically, as a profile does.
pub const CONSISTENT_LABELS: Flag = Flag("--consistent_labels");
/// Makes each cquery dependency edge state the configuration its dependency is
/// built in.
pub const TRANSITIONS: Flag = Flag("--transitions=lite");
const INCLUDE_CONFIGURATIONS: Flag = Flag("--proto:include_configurations");
pub const OUTPUT_RULE_ATTRS: Flag = Flag("--proto:output_rule_attrs=all");
pub const INCLUDE_ARTIFACTS: Flag = Flag("--include_artifacts");

const NOSLIM_PROFILE: Flag = Flag("--noslim_profile");
const RECORD_FULL_PROFILER_DATA: Flag = Flag("--record_full_profiler_data");
pub const PROFILE_PRIMARY_OUTPUT: Flag = Flag("--experimental_profile_include_primary_output");
pub const PROFILE_TARGET_LABEL: Flag = Flag("--experimental_profile_include_target_label");
const PROFILE_TARGET_CONFIGURATION: Flag =
    Flag("--experimental_profile_include_target_configuration");
const PRESSURE_STALL_INDICATORS: Flag = Flag("--experimental_collect_pressure_stall_indicators");
const RESOURCE_ESTIMATION: Flag = Flag("--experimental_collect_resource_estimation");
const SHOW_TIMESTAMPS: Flag = Flag("--show_timestamps");
const NO_BUILD_EVENT_FILE: Flag = Flag("--build_event_binary_file=");

/// The earliest Bazel that knows every flag here: 8.0.0 is the first with
/// `--experimental_profile_include_target_configuration`.
const MIN_BAZEL: &str = "8.0";

/// A Bazel command a bazelrc section applies to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Command {
    Build,
    Query(QueryCommand),
}

impl Command {
    const fn name(self) -> &'static str {
        match self {
            Command::Build => "build",
            Command::Query(query) => query.name(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum QueryCommand {
    Query,
    Cquery,
    Aquery,
}

impl QueryCommand {
    const fn name(self) -> &'static str {
        match self {
            QueryCommand::Query => "query",
            QueryCommand::Cquery => "cquery",
            QueryCommand::Aquery => "aquery",
        }
    }
}

/// What Bazel resolves a relative path a flag names against.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Root {
    Workspace,
    /// The directory Bazel runs in, so the path starts at `%workspace%`, which
    /// the flag's own module expands.
    WorkingDirectory,
}

impl Root {
    /// What a path relative to the workspace root is prefixed with to resolve
    /// against this root.
    const fn prefix(self) -> &'static str {
        match self {
            Root::Workspace => "",
            Root::WorkingDirectory => "%workspace%/",
        }
    }
}

/// A flag naming a file Bazel writes into the capture directory, which Bazel
/// does not create.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Capture {
    name: &'static str,
    file: &'static str,
    root: Root,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Setting {
    Flag(Flag),
    Capture(Capture),
}

const BUILD_PROFILE: Capture = Capture {
    name: "--profile",
    file: "build.profile.gz",
    root: Root::Workspace,
};
const BUILD_EVENTS: Capture = Capture {
    name: "--build_event_binary_file",
    file: "build.bep",
    root: Root::Workspace,
};
const BUILD_EXEC_LOG: Capture = Capture {
    name: "--execution_log_compact_file",
    file: "build.execlog.zst",
    root: Root::WorkingDirectory,
};

/// The file each query's output is shell-redirected to: the queries'
/// `--output_file` first appears in Bazel 8.2, after [`MIN_BAZEL`].
const QUERY_OUTPUTS: [(QueryCommand, &str); 3] = [
    (QueryCommand::Query, "query.pb"),
    (QueryCommand::Cquery, "cquery.pb"),
    (QueryCommand::Aquery, "aquery.pb"),
];

/// A bazelrc section: the settings `command:<config>` lines pass.
struct Section {
    command: Command,
    settings: &'static [Setting],
}

/// The settings bviz needs, each under the command it belongs to and never
/// `common`, where Bazel ignores a flag the command does not know, which would
/// hide a misspelled or renamed flag, and where `--output` would reach
/// `bazel mod` and `bazel config` too.
///
/// Some settings restate Bazel's default: `--config` expands where it stands,
/// so they override an earlier rc line turning bviz's data off.
///
/// cquery inherits `build`'s settings through `test`, and aquery inherits them
/// directly. Without building anything, either still truncates the execution
/// log and overwrites the profile and build event stream those settings name,
/// so each gives them files of its own or turns them off. The execution log
/// always gets a file: Bazel 8 and 9 turn it off only with an empty path, which
/// Bazel 10.0.0-pre.20260421.2 and later reject.
const SECTIONS: [Section; 4] = [
    Section {
        command: Command::Build,
        settings: &[
            Setting::Capture(BUILD_PROFILE),
            Setting::Flag(NOSLIM_PROFILE),
            Setting::Flag(RECORD_FULL_PROFILER_DATA),
            Setting::Flag(PROFILE_PRIMARY_OUTPUT),
            Setting::Flag(PROFILE_TARGET_LABEL),
            Setting::Flag(PROFILE_TARGET_CONFIGURATION),
            Setting::Flag(PRESSURE_STALL_INDICATORS),
            Setting::Flag(RESOURCE_ESTIMATION),
            Setting::Capture(BUILD_EVENTS),
            Setting::Flag(SHOW_TIMESTAMPS),
            Setting::Capture(BUILD_EXEC_LOG),
        ],
    },
    Section {
        command: Command::Query(QueryCommand::Query),
        settings: &[
            Setting::Flag(STREAMED_PROTO),
            Setting::Flag(CONSISTENT_LABELS),
            Setting::Flag(OUTPUT_RULE_ATTRS),
        ],
    },
    Section {
        command: Command::Query(QueryCommand::Cquery),
        settings: &[
            Setting::Flag(STREAMED_PROTO),
            Setting::Flag(CONSISTENT_LABELS),
            Setting::Flag(TRANSITIONS),
            Setting::Flag(INCLUDE_CONFIGURATIONS),
            Setting::Flag(OUTPUT_RULE_ATTRS),
            Setting::Capture(Capture {
                file: "cquery.profile.gz",
                ..BUILD_PROFILE
            }),
            Setting::Flag(NO_BUILD_EVENT_FILE),
            Setting::Capture(Capture {
                file: "cquery.execlog.zst",
                ..BUILD_EXEC_LOG
            }),
        ],
    },
    Section {
        command: Command::Query(QueryCommand::Aquery),
        settings: &[
            Setting::Flag(STREAMED_PROTO),
            Setting::Flag(INCLUDE_ARTIFACTS),
            Setting::Capture(Capture {
                file: "aquery.profile.gz",
                ..BUILD_PROFILE
            }),
            Setting::Flag(NO_BUILD_EVENT_FILE),
            Setting::Capture(Capture {
                file: "aquery.execlog.zst",
                ..BUILD_EXEC_LOG
            }),
        ],
    },
];
