use std::fmt::{self, Write};
use std::str::FromStr;

use crate::{Capture, MIN_BAZEL, QUERY_OUTPUTS, SECTIONS, Setting};

const DEFAULT_CONFIG: &str = "bviz";
const DEFAULT_DIR: &str = ".bviz";

/// Whether `c` reads as itself both in a bazelrc line, which is tokenized with
/// `#` comments, quotes and `\` escapes, and in an unquoted shell word.
fn is_literal(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-')
}

/// A `--config` name that a bazelrc and a shell both read as written.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ConfigName(String);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InvalidConfigName {
    Empty,
    /// It would read as a flag.
    LeadingDash,
    Character(char),
}

impl fmt::Display for InvalidConfigName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InvalidConfigName::Empty => f.write_str("a config name cannot be empty"),
            InvalidConfigName::LeadingDash => f.write_str("a config name cannot start with '-'"),
            InvalidConfigName::Character(c) => write!(
                f,
                "a config name holds only ASCII letters, digits, '.', '_', '+' and '-', not {c:?}"
            ),
        }
    }
}

impl std::error::Error for InvalidConfigName {}

impl FromStr for ConfigName {
    type Err = InvalidConfigName;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        if name.is_empty() {
            Err(InvalidConfigName::Empty)
        } else if name.starts_with('-') {
            Err(InvalidConfigName::LeadingDash)
        } else if let Some(c) = name.chars().find(|&c| !is_literal(c)) {
            Err(InvalidConfigName::Character(c))
        } else {
            Ok(Self(name.to_owned()))
        }
    }
}

impl Default for ConfigName {
    fn default() -> Self {
        Self(DEFAULT_CONFIG.to_owned())
    }
}

impl fmt::Display for ConfigName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A directory below the workspace root, by a relative path that a bazelrc and
/// a shell both read as written, with no trailing `/`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CaptureDir(String);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InvalidCaptureDir {
    Empty,
    Absolute,
    /// It would read as a flag.
    LeadingDash,
    Character(char),
    /// An empty, `.` or `..` component.
    Component,
}

impl fmt::Display for InvalidCaptureDir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InvalidCaptureDir::Empty => f.write_str("a capture directory cannot be empty"),
            InvalidCaptureDir::Absolute => {
                f.write_str("a capture directory is relative to the workspace root, not absolute")
            }
            InvalidCaptureDir::LeadingDash => {
                f.write_str("a capture directory cannot start with '-'")
            }
            InvalidCaptureDir::Character(c) => write!(
                f,
                "a capture directory holds only ASCII letters, digits, '.', '_', '+', '-', '@' \
                 and '/', not {c:?}"
            ),
            InvalidCaptureDir::Component => f.write_str(
                "a capture directory names a directory below the workspace root, without an \
                 empty, '.' or '..' component",
            ),
        }
    }
}

impl std::error::Error for InvalidCaptureDir {}

impl FromStr for CaptureDir {
    type Err = InvalidCaptureDir;

    fn from_str(dir: &str) -> Result<Self, Self::Err> {
        if dir.is_empty() {
            return Err(InvalidCaptureDir::Empty);
        }
        if dir.starts_with('/') {
            return Err(InvalidCaptureDir::Absolute);
        }
        if dir.starts_with('-') {
            return Err(InvalidCaptureDir::LeadingDash);
        }
        if let Some(c) = dir
            .chars()
            .find(|&c| !(is_literal(c) || matches!(c, '@' | '/')))
        {
            return Err(InvalidCaptureDir::Character(c));
        }
        let dir = dir.trim_end_matches('/');
        if dir
            .split('/')
            .any(|component| matches!(component, "" | "." | ".."))
        {
            return Err(InvalidCaptureDir::Component);
        }
        Ok(Self(dir.to_owned()))
    }
}

impl Default for CaptureDir {
    fn default() -> Self {
        Self(DEFAULT_DIR.to_owned())
    }
}

impl fmt::Display for CaptureDir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The bazelrc lines passing every setting bviz needs under
/// `--config=<config>`, its files written into `dir`, the workspace root's
/// subdirectory.
pub fn settings(config: &ConfigName, dir: &CaptureDir) -> String {
    let mut rc = String::new();
    for (index, section) in SECTIONS.iter().enumerate() {
        if index > 0 {
            rc.push('\n');
        }
        let command = section.command.name();
        for setting in section.settings {
            let _ = match setting {
                Setting::Flag(flag) => writeln!(rc, "{command}:{config} {flag}"),
                Setting::Capture(Capture { name, file, root }) => writeln!(
                    rc,
                    "{command}:{config} {name}={}{dir}/{file}",
                    root.prefix()
                ),
            };
        }
    }
    rc
}

/// The comment heading the [`settings`] for `config` and `dir`: how to set
/// them up and capture a build with them.
pub fn header(config: &ConfigName, dir: &CaptureDir) -> String {
    let mut header = format!(
        "\
# The Bazel flags bviz reads a build through, passed with --config={config}.
# Requires Bazel {MIN_BAZEL} or later.
#
# Once, from the workspace root:
#   mkdir -p {dir} && {{ [ -e {dir}/.gitignore ] || echo '*' > {dir}/.gitignore; }}
# and in the workspace's .bazelrc, or a bazelrc it imports:
#   try-import %workspace%/{dir}/bazelrc
#
# Pass --config={config} to build, test, query, cquery and aquery only: any
# other command inheriting build's flags would overwrite the capture. From the
# workspace root, build (or test), then query everything that went into the
# same targets, with the same flags:
#   bazel build --config={config} <targets>
"
    );
    for (query, file) in QUERY_OUTPUTS {
        let _ = writeln!(
            header,
            "#   bazel {} --config={config} 'deps(<targets>)' > {dir}/{file}",
            query.name()
        );
    }
    header
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT_BAZELRC: &str = r#"# The Bazel flags bviz reads a build through, passed with --config=bviz.
# Requires Bazel 8.0 or later.
#
# Once, from the workspace root:
#   mkdir -p .bviz && { [ -e .bviz/.gitignore ] || echo '*' > .bviz/.gitignore; }
# and in the workspace's .bazelrc, or a bazelrc it imports:
#   try-import %workspace%/.bviz/bazelrc
#
# Pass --config=bviz to build, test, query, cquery and aquery only: any
# other command inheriting build's flags would overwrite the capture. From the
# workspace root, build (or test), then query everything that went into the
# same targets, with the same flags:
#   bazel build --config=bviz <targets>
#   bazel query --config=bviz 'deps(<targets>)' > .bviz/query.pb
#   bazel cquery --config=bviz 'deps(<targets>)' > .bviz/cquery.pb
#   bazel aquery --config=bviz 'deps(<targets>)' > .bviz/aquery.pb

build:bviz --profile=.bviz/build.profile.gz
build:bviz --noslim_profile
build:bviz --record_full_profiler_data
build:bviz --experimental_profile_include_primary_output
build:bviz --experimental_profile_include_target_label
build:bviz --experimental_profile_include_target_configuration
build:bviz --experimental_collect_pressure_stall_indicators
build:bviz --experimental_collect_resource_estimation
build:bviz --build_event_binary_file=.bviz/build.bep
build:bviz --show_timestamps
build:bviz --execution_log_compact_file=%workspace%/.bviz/build.execlog.zst

query:bviz --output=streamed_proto
query:bviz --consistent_labels
query:bviz --proto:output_rule_attrs=all

cquery:bviz --output=streamed_proto
cquery:bviz --consistent_labels
cquery:bviz --transitions=lite
cquery:bviz --proto:include_configurations
cquery:bviz --proto:output_rule_attrs=all
cquery:bviz --profile=.bviz/cquery.profile.gz
cquery:bviz --build_event_binary_file=
cquery:bviz --execution_log_compact_file=%workspace%/.bviz/cquery.execlog.zst

aquery:bviz --output=streamed_proto
aquery:bviz --include_artifacts
aquery:bviz --profile=.bviz/aquery.profile.gz
aquery:bviz --build_event_binary_file=
aquery:bviz --execution_log_compact_file=%workspace%/.bviz/aquery.execlog.zst
"#;

    fn config(name: &str) -> ConfigName {
        name.parse().unwrap()
    }

    fn dir(path: &str) -> CaptureDir {
        path.parse().unwrap()
    }

    fn bazelrc(config: &ConfigName, dir: &CaptureDir) -> String {
        format!("{}\n{}", header(config, dir), settings(config, dir))
    }

    #[test]
    fn the_default_bazelrc_is_pinned() {
        assert_eq!(
            bazelrc(&ConfigName::default(), &CaptureDir::default()),
            DEFAULT_BAZELRC
        );
    }

    #[test]
    fn a_named_config_and_directory_replace_the_defaults_throughout() {
        let rc = bazelrc(&config("trace"), &dir("out/capture/"));
        let expected = DEFAULT_BAZELRC
            .replace("=bviz", "=trace")
            .replace(":bviz ", ":trace ")
            .replace(".bviz", "out/capture");
        assert_eq!(rc, expected);
    }

    #[test]
    fn a_config_name_is_a_word_a_bazelrc_and_a_shell_read_as_written() {
        assert_eq!(config("bviz-ci.2+x_y"), ConfigName("bviz-ci.2+x_y".into()));
        assert_eq!("".parse::<ConfigName>(), Err(InvalidConfigName::Empty));
        assert_eq!(
            "-x".parse::<ConfigName>(),
            Err(InvalidConfigName::LeadingDash)
        );
        for (name, c) in [
            ("my config", ' '),
            ("build:x", ':'),
            ("a#b", '#'),
            ("a'b", '\''),
            ("a\"b", '"'),
            ("a\\b", '\\'),
            ("$X", '$'),
        ] {
            assert_eq!(
                name.parse::<ConfigName>(),
                Err(InvalidConfigName::Character(c)),
                "{name}"
            );
        }
    }

    #[test]
    fn a_capture_directory_is_a_relative_path_a_bazelrc_and_a_shell_read_as_written() {
        assert_eq!(dir("out/x//"), CaptureDir("out/x".into()));
        assert_eq!(dir(".bviz/@x/a-b"), CaptureDir(".bviz/@x/a-b".into()));
        assert_eq!("".parse::<CaptureDir>(), Err(InvalidCaptureDir::Empty));
        for path in ["/", "/tmp/x"] {
            assert_eq!(
                path.parse::<CaptureDir>(),
                Err(InvalidCaptureDir::Absolute),
                "{path}"
            );
        }
        assert_eq!(
            "-x".parse::<CaptureDir>(),
            Err(InvalidCaptureDir::LeadingDash)
        );
        for (path, c) in [
            ("my dir", ' '),
            ("a#b", '#'),
            ("~/x", '~'),
            ("a'b", '\''),
            ("a\"b", '"'),
            ("$X", '$'),
        ] {
            assert_eq!(
                path.parse::<CaptureDir>(),
                Err(InvalidCaptureDir::Character(c)),
                "{path}"
            );
        }
        for path in [".", "./", "..", "a/../b", "a/./b", "a//b"] {
            assert_eq!(
                path.parse::<CaptureDir>(),
                Err(InvalidCaptureDir::Component),
                "{path}"
            );
        }
    }
}
