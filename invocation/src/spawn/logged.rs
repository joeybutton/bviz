use std::collections::HashMap;
use std::ops::Range;

use super::PhaseTiming;
use crate::EpochMicros;

/// The spawns an execution log records of an invocation.
#[derive(Default)]
pub struct LoggedSpawns {
    /// Distinct runner names, which [`Record::runner`] indexes.
    runners: Vec<Box<str>>,
    /// Distinct mnemonics, which [`Record::mnemonic`] indexes.
    mnemonics: Vec<Box<str>>,
    /// Distinct failure statuses, which [`Record::status`] indexes.
    statuses: Vec<Box<str>>,
    records: Vec<Record>,
    /// Every spawn's outputs, each spawn's a run [`Record::outputs`] names.
    outputs: Paths,
    /// Every digest hash end to end, each spawn's where [`Record::digest`]
    /// names.
    digests: String,
}

struct Record {
    runner: usize,
    mnemonic: usize,
    target_label: Option<Box<str>>,
    outputs: Range<usize>,
    cache_hit: bool,
    timing: PhaseTiming,
    start: Option<EpochMicros>,
    exit_code: i32,
    status: Option<usize>,
    digest: Option<Range<usize>>,
}

/// Exec paths end to end, each named by its place in the order added.
#[derive(Default)]
struct Paths {
    text: String,
    /// Where each path ends in `text`, the next starting there.
    ends: Vec<usize>,
}

impl Paths {
    fn with_capacity(paths: usize, text_bytes: usize) -> Paths {
        Paths {
            text: String::with_capacity(text_bytes),
            ends: Vec::with_capacity(paths),
        }
    }

    fn len(&self) -> usize {
        self.ends.len()
    }

    fn push(&mut self, path: &str) {
        self.text.push_str(path);
        self.ends.push(self.text.len());
    }

    /// Keeps the first `paths` paths.
    fn truncate(&mut self, paths: usize) {
        self.ends.truncate(paths);
        self.text.truncate(self.ends.last().copied().unwrap_or(0));
    }

    fn get(&self, index: usize) -> &str {
        let start = index.checked_sub(1).map_or(0, |before| self.ends[before]);
        &self.text[start..self.ends[index]]
    }
}

impl LoggedSpawns {
    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Distinct runner names, each at the place [`LoggedSpawn::runner_index`]
    /// names.
    pub fn runners(&self) -> &[Box<str>] {
        &self.runners
    }

    /// Distinct mnemonics, each at the place [`LoggedSpawn::mnemonic_index`]
    /// names.
    pub fn mnemonics(&self) -> &[Box<str>] {
        &self.mnemonics
    }

    /// The spawns in the order the log lists them.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = LoggedSpawn<'_>> {
        self.records
            .iter()
            .map(|record| LoggedSpawn { log: self, record })
    }
}

/// One spawn of [`LoggedSpawns`].
#[derive(Clone, Copy)]
pub struct LoggedSpawn<'a> {
    log: &'a LoggedSpawns,
    record: &'a Record,
}

impl<'a> LoggedSpawn<'a> {
    /// What answered the spawn: a strategy that executed it, such as `remote`
    /// or `linux-sandbox`, or a cache, such as `disk cache hit`.
    pub fn runner(&self) -> &'a str {
        &self.log.runners[self.record.runner]
    }

    pub fn runner_index(&self) -> usize {
        self.record.runner
    }

    pub fn mnemonic(&self) -> &'a str {
        &self.log.mnemonics[self.record.mnemonic]
    }

    pub fn mnemonic_index(&self) -> usize {
        self.record.mnemonic
    }

    /// The label of the target owning the spawn's action, `None` for an
    /// action no target owns.
    pub fn target_label(&self) -> Option<&'a str> {
        self.record.target_label.as_deref()
    }

    /// The exec paths of the spawn's outputs the log names, in the order it
    /// declares them.
    pub fn outputs(&self) -> impl ExactSizeIterator<Item = &'a str> + use<'a> {
        let outputs = &self.log.outputs;
        self.record.outputs.clone().map(|index| outputs.get(index))
    }

    /// The exec path of the first of [`outputs`](Self::outputs).
    pub fn primary_output(&self) -> Option<&'a str> {
        self.outputs().next()
    }

    /// Whether a cache answered the spawn rather than an execution of it.
    pub fn cache_hit(&self) -> bool {
        self.record.cache_hit
    }

    pub fn timing(&self) -> &'a PhaseTiming {
        &self.record.timing
    }

    /// When the spawn started to execute, on the clock of whatever executed
    /// it: a remote executor's for a remote execution, and for a cache hit
    /// the execution that wrote the entry, however long before the build.
    /// `None` where the log names no instant.
    pub fn start(&self) -> Option<EpochMicros> {
        self.record.start
    }

    /// The spawn's exit code, whose meaning depends on its
    /// [`status`](Self::status).
    pub fn exit_code(&self) -> i32 {
        self.record.exit_code
    }

    /// Why the spawn failed, as Bazel names its `SpawnResult.Status`, such as
    /// `NON_ZERO_EXIT` or `TIMEOUT`; `None` for a spawn that succeeded.
    pub fn status(&self) -> Option<&'a str> {
        self.record.status.map(|status| &*self.log.statuses[status])
    }

    /// Whether the spawn failed: it has a status, or a non-zero exit code.
    pub fn failed(&self) -> bool {
        self.record.status.is_some() || self.record.exit_code != 0
    }

    /// The hash of the spawn's action digest, the key a remote executor and
    /// the remote and disk caches know it by; `None` where Bazel computed
    /// none, as it does only for a spawn one of those could answer.
    pub fn digest(&self) -> Option<&'a str> {
        let range = self.record.digest.clone()?;
        Some(&self.log.digests[range])
    }
}

/// One spawn as a reader of a log finds it, for [`Builder::push`].
pub struct Logged<'a> {
    pub runner: &'a str,
    pub mnemonic: &'a str,
    /// Empty where no target owns the spawn's action.
    pub target_label: &'a str,
    pub cache_hit: bool,
    pub timing: PhaseTiming,
}

/// [`LoggedSpawns`] gathered one spawn at a time.
pub struct Builder {
    runners: Names,
    mnemonics: Names,
    statuses: Names,
    records: Vec<Record>,
    outputs: Paths,
    /// Where the outputs of the spawn pushed next start in `outputs`.
    next_outputs: usize,
    /// What [`started`](Self::started) gave the spawn pushed next.
    next_start: Option<EpochMicros>,
    /// What [`exited`](Self::exited) gave the spawn pushed next, its status
    /// interned.
    next_exit: (i32, Option<usize>),
    digests: String,
    /// Where the digest of the spawn pushed next lies in `digests`.
    next_digest: Option<Range<usize>>,
}

impl Builder {
    /// Room for `spawns` spawns with `outputs` outputs between them, their
    /// paths `output_bytes` long together.
    pub fn with_capacity(spawns: usize, outputs: usize, output_bytes: usize) -> Builder {
        Builder {
            runners: Names::default(),
            mnemonics: Names::default(),
            statuses: Names::default(),
            records: Vec::with_capacity(spawns),
            outputs: Paths::with_capacity(outputs, output_bytes),
            next_outputs: 0,
            next_start: None,
            next_exit: (0, None),
            digests: String::new(),
            next_digest: None,
        }
    }

    /// Adds an output of the spawn [`push`](Self::push) adds next.
    pub fn output(&mut self, path: &str) {
        self.outputs.push(path);
    }

    /// Gives the spawn [`push`](Self::push) adds next the
    /// [start](LoggedSpawn::start) `at`.
    pub fn started(&mut self, at: EpochMicros) {
        self.next_start = Some(at);
    }

    /// Gives the spawn [`push`](Self::push) adds next its
    /// [exit code](LoggedSpawn::exit_code) and [status](LoggedSpawn::status);
    /// one given none of either succeeded.
    pub fn exited(&mut self, exit_code: i32, status: Option<&str>) {
        self.next_exit = (exit_code, status.map(|status| self.statuses.intern(status)));
    }

    /// Gives the spawn [`push`](Self::push) adds next the digest `hash`.
    pub fn digest(&mut self, hash: &str) {
        if self.digests.capacity() == 0 {
            self.digests
                .reserve(self.records.capacity().saturating_mul(SHA256_HEX_CHARS));
        }
        let start = self.digests.len();
        self.digests.push_str(hash);
        self.next_digest = Some(start..self.digests.len());
    }

    /// Adds `spawn`, with the outputs added, and the start, exit and digest
    /// given, since the spawn before it.
    pub fn push(&mut self, spawn: Logged<'_>) {
        let outputs = self.next_outputs..self.outputs.len();
        self.next_outputs = outputs.end;
        let (exit_code, status) = std::mem::take(&mut self.next_exit);
        let record = Record {
            runner: self.runners.intern(spawn.runner),
            mnemonic: self.mnemonics.intern(spawn.mnemonic),
            target_label: (!spawn.target_label.is_empty()).then(|| spawn.target_label.into()),
            outputs,
            cache_hit: spawn.cache_hit,
            timing: spawn.timing,
            start: self.next_start.take(),
            exit_code,
            status,
            digest: self.next_digest.take(),
        };
        self.records.push(record);
    }

    /// The spawns pushed; outputs added after the last of them are dropped.
    pub fn finish(mut self) -> LoggedSpawns {
        self.outputs.truncate(self.next_outputs);
        LoggedSpawns {
            runners: self.runners.into_vec(),
            mnemonics: self.mnemonics.into_vec(),
            statuses: self.statuses.into_vec(),
            records: self.records,
            outputs: self.outputs,
            digests: self.digests,
        }
    }
}

/// The length of a digest hash in the hash function Bazel defaults to, which a
/// log of a build keying spawns by digest is reserved room for per spawn.
const SHA256_HEX_CHARS: usize = 64;

/// Distinct strings, each named by its place in the order first seen.
#[derive(Default)]
struct Names {
    index: HashMap<Box<str>, usize>,
}

impl Names {
    fn intern(&mut self, name: &str) -> usize {
        let next = self.index.len();
        match self.index.get(name) {
            Some(&index) => index,
            None => {
                self.index.insert(name.into(), next);
                next
            }
        }
    }

    fn into_vec(self) -> Vec<Box<str>> {
        let mut names = vec![Box::<str>::default(); self.index.len()];
        for (name, index) in self.index {
            names[index] = name;
        }
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spawn::REPORTED_PHASES;
    use std::time::Duration;

    fn logged<'a>(runner: &'a str, mnemonic: &'a str) -> Logged<'a> {
        Logged {
            runner,
            mnemonic,
            target_label: "",
            cache_hit: false,
            timing: PhaseTiming::new(Duration::ZERO, [Duration::ZERO; REPORTED_PHASES]),
        }
    }

    #[test]
    fn names_are_shared_between_spawns() {
        let mut builder = Builder::with_capacity(3, 0, 0);
        builder.push(logged("remote", "A"));
        builder.push(logged("local", "B"));
        builder.push(logged("remote", "B"));
        let log = builder.finish();
        assert_eq!(*log.runners(), ["remote".into(), "local".into()]);
        assert_eq!(*log.mnemonics(), ["A".into(), "B".into()]);
        let runners: Vec<&str> = log.iter().map(|spawn| spawn.runner()).collect();
        assert_eq!(runners, ["remote", "local", "remote"]);
    }

    #[test]
    fn an_empty_target_label_names_no_target() {
        let mut builder = Builder::with_capacity(2, 0, 0);
        builder.push(logged("remote", "A"));
        builder.push(Logged {
            target_label: "//a",
            ..logged("remote", "A")
        });
        let log = builder.finish();
        let labels: Vec<_> = log.iter().map(|s| s.target_label()).collect();
        assert_eq!(labels, [None, Some("//a")]);
    }

    #[test]
    fn each_spawn_keeps_the_outputs_added_before_it() {
        let mut builder = Builder::with_capacity(3, 3, 0);
        builder.output("bazel-out/a.o");
        builder.output("bazel-out/a.d");
        builder.push(logged("remote", "A"));
        builder.push(logged("remote", "B"));
        builder.output("bazel-out/c");
        builder.push(Logged {
            cache_hit: true,
            ..logged("disk cache hit", "C")
        });
        builder.output("bazel-out/dropped");
        let log = builder.finish();
        let outputs: Vec<Vec<&str>> = log.iter().map(|s| s.outputs().collect()).collect();
        assert_eq!(
            outputs,
            [
                vec!["bazel-out/a.o", "bazel-out/a.d"],
                vec![],
                vec!["bazel-out/c"]
            ]
        );
        let primary: Vec<_> = log.iter().map(|s| s.primary_output()).collect();
        assert_eq!(primary, [Some("bazel-out/a.o"), None, Some("bazel-out/c")]);
        let hits: Vec<bool> = log.iter().map(|s| s.cache_hit()).collect();
        assert_eq!(hits, [false, false, true]);
    }

    #[test]
    fn each_spawn_keeps_the_digest_given_since_the_spawn_before_it() {
        let mut builder = Builder::with_capacity(3, 0, 0);
        builder.digest("9f86d081");
        builder.push(logged("remote", "A"));
        builder.push(logged("linux-sandbox", "B"));
        builder.digest("60303ae2");
        builder.push(logged("disk cache hit", "C"));
        let log = builder.finish();
        let digests: Vec<_> = log.iter().map(|s| s.digest()).collect();
        assert_eq!(digests, [Some("9f86d081"), None, Some("60303ae2")]);
    }

    #[test]
    fn a_spawn_fails_by_its_status_or_its_exit_code() {
        let started = EpochMicros::from_micros(1_781_439_245_231_000);
        let mut builder = Builder::with_capacity(4, 0, 0);
        builder.started(started);
        builder.push(logged("linux-sandbox", "A"));
        builder.exited(1, Some("NON_ZERO_EXIT"));
        builder.push(logged("linux-sandbox", "A"));
        builder.exited(0, Some("TIMEOUT"));
        builder.push(logged("linux-sandbox", "A"));
        builder.exited(2, None);
        builder.push(logged("linux-sandbox", "A"));
        let log = builder.finish();
        let starts: Vec<_> = log.iter().map(|s| s.start()).collect();
        assert_eq!(starts, [Some(started), None, None, None]);
        let statuses: Vec<_> = log.iter().map(|s| s.status()).collect();
        assert_eq!(
            statuses,
            [None, Some("NON_ZERO_EXIT"), Some("TIMEOUT"), None]
        );
        let exits: Vec<_> = log.iter().map(|s| s.exit_code()).collect();
        assert_eq!(exits, [0, 1, 0, 2]);
        let failed: Vec<_> = log.iter().map(|s| s.failed()).collect();
        assert_eq!(failed, [false, true, true, true]);
    }
}
