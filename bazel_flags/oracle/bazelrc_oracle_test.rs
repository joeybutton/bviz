//! Checks the bazelrc bviz writes against Bazel's own option parser, run as
//! `bazelrc_oracle`: each config must parse, flag names and values alike, for
//! every command whose section passes it and every command inheriting that
//! section.

use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};

use bviz_bazel_flags::{CaptureDir, ConfigName, settings};

/// The commands a capture runs, `test` standing in for `build`.
const COMMANDS: &[&str] = &["build", "test", "query", "cquery", "aquery"];

/// Bazel's verdict on one config for one command.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// The config parses, resolving each flag naming a file the command
    /// writes to the value given, empty where unset.
    Parsed(BTreeMap<String, String>),
    Rejected(String),
    /// The section is for a command the oracle does not model.
    Unchecked,
}

#[derive(Debug)]
struct Answer {
    command: String,
    config: String,
    verdict: Verdict,
}

impl Answer {
    fn parse(line: &str) -> Self {
        let mut fields = line.split('\t');
        let mut field = || fields.next().unwrap_or_else(|| panic!("{line}"));
        let (command, config, kind) = (field().to_owned(), field().to_owned(), field());
        let verdict = match kind {
            "ok" => Verdict::Parsed(
                fields
                    .map(|capture| {
                        let (flag, value) = capture.split_once('=').expect("a flag=value");
                        (flag.to_owned(), value.to_owned())
                    })
                    .collect(),
            ),
            "rejected" => Verdict::Rejected(fields.collect::<Vec<_>>().join("\t")),
            "unchecked command" => Verdict::Unchecked,
            kind => panic!("no verdict {kind}: {line}"),
        };
        Self {
            command,
            config,
            verdict,
        }
    }
}

/// Bazel's verdict on each config of `rc` for each of `COMMANDS` that takes
/// one.
fn ask_bazel(rc: &str) -> Vec<Answer> {
    let runfiles = runfiles::Runfiles::create().unwrap();
    let oracle = runfiles::rlocation!(runfiles, std::env::var("BAZELRC_ORACLE").unwrap()).unwrap();
    let mut child = Command::new(oracle)
        .args(COMMANDS)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(rc.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "the oracle failed");
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(Answer::parse)
        .collect()
}

/// Checks every one of `COMMANDS` parses `config`, and gives what each resolves
/// the flags naming a file to.
fn every_command_takes<'a>(
    config: &str,
    answers: &'a [Answer],
) -> BTreeMap<&'a str, &'a BTreeMap<String, String>> {
    let mut resolved = BTreeMap::new();
    let mut checked: Vec<&str> = Vec::with_capacity(COMMANDS.len());
    for Answer {
        command,
        config: named,
        verdict,
    } in answers
    {
        assert_eq!(named, config, "{command}");
        let Verdict::Parsed(captures) = verdict else {
            panic!("{command}:{named}: {verdict:?}");
        };
        checked.push(command);
        resolved.insert(command.as_str(), captures);
    }
    assert_eq!(checked, COMMANDS);
    resolved
}

fn captures(profile: &str, build_events: &str, exec_log: &str) -> BTreeMap<String, String> {
    [
        ("profile", profile),
        ("build_event_binary_file", build_events),
        ("execution_log_compact_file", exec_log),
    ]
    .into_iter()
    .map(|(flag, value)| (flag.to_owned(), value.to_owned()))
    .collect()
}

/// A query writes none of a build's files: each gets a profile and execution
/// log of its own and no build event stream, where build and test write
/// build's.
#[test]
fn the_default_bazelrc_parses_for_every_command() {
    let rc = settings(&ConfigName::default(), &CaptureDir::default());
    let answers = ask_bazel(&rc);
    let resolved = every_command_takes(&ConfigName::default().to_string(), &answers);
    let build = captures(
        ".bviz/build.profile.gz",
        ".bviz/build.bep",
        "%workspace%/.bviz/build.execlog.zst",
    );
    assert_eq!(resolved["build"], &build);
    assert_eq!(resolved["test"], &build);
    for query in ["cquery", "aquery"] {
        assert_eq!(
            resolved[query],
            &captures(
                &format!(".bviz/{query}.profile.gz"),
                "",
                &format!("%workspace%/.bviz/{query}.execlog.zst"),
            ),
            "{query}"
        );
    }
}

#[test]
fn a_bazelrc_for_a_named_config_and_directory_parses_for_every_command() {
    let rc = settings(&"ci".parse().unwrap(), &"out/capture".parse().unwrap());
    every_command_takes("ci", &ask_bazel(&rc));
}

/// The oracle refuses what Bazel refuses, a name or a value alike, and holds a
/// command to the sections it inherits.
#[test]
fn the_oracle_rejects_a_flag_bazel_rejects() {
    let answers = ask_bazel(
        "build:name --no_such_flag\n\
         cquery:value --transitions=some\n\
         build:inherited --output=streamed_proto\n\
         run:command --foo\n",
    );
    let verdict = |command: &str, config: &str| {
        answers
            .iter()
            .find(|answer| answer.command == command && answer.config == config)
            .map(|answer| &answer.verdict)
    };
    let rejected = |command, config| matches!(verdict(command, config), Some(Verdict::Rejected(_)));
    for command in ["build", "test", "cquery", "aquery"] {
        assert!(rejected(command, "name"), "{command}");
    }
    assert_eq!(verdict("query", "name"), None);
    assert!(rejected("cquery", "value"));
    assert!(rejected("build", "inherited"));
    assert!(matches!(
        verdict("cquery", "inherited"),
        Some(Verdict::Parsed(_))
    ));
    assert_eq!(verdict("run", "command"), Some(&Verdict::Unchecked));
}
