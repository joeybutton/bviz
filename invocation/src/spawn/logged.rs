use std::collections::HashMap;

use super::PhaseTiming;

/// The spawns an execution log records of an invocation.
#[derive(Default)]
pub struct LoggedSpawns {
    /// Distinct runner names, which [`Record::runner`] indexes.
    runners: Vec<Box<str>>,
    /// Distinct mnemonics, which [`Record::mnemonic`] indexes.
    mnemonics: Vec<Box<str>>,
    records: Vec<Record>,
}

struct Record {
    runner: usize,
    mnemonic: usize,
    target_label: Option<Box<str>>,
    primary_output: Option<Box<str>>,
    timing: PhaseTiming,
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

    /// The exec path of the spawn's first declared output.
    pub fn primary_output(&self) -> Option<&'a str> {
        self.record.primary_output.as_deref()
    }

    pub fn timing(&self) -> &'a PhaseTiming {
        &self.record.timing
    }
}

/// One spawn as a reader of a log finds it, for [`Builder::push`].
pub struct Logged<'a> {
    pub runner: &'a str,
    pub mnemonic: &'a str,
    /// Empty where no target owns the spawn's action.
    pub target_label: &'a str,
    pub primary_output: Option<&'a str>,
    pub timing: PhaseTiming,
}

/// [`LoggedSpawns`] gathered one spawn at a time.
pub struct Builder {
    runners: Names,
    mnemonics: Names,
    records: Vec<Record>,
}

impl Builder {
    /// Room for `spawns` spawns.
    pub fn with_capacity(spawns: usize) -> Builder {
        Builder {
            runners: Names::default(),
            mnemonics: Names::default(),
            records: Vec::with_capacity(spawns),
        }
    }

    pub fn push(&mut self, spawn: Logged<'_>) {
        let record = Record {
            runner: self.runners.intern(spawn.runner),
            mnemonic: self.mnemonics.intern(spawn.mnemonic),
            target_label: (!spawn.target_label.is_empty()).then(|| spawn.target_label.into()),
            primary_output: spawn.primary_output.map(Box::from),
            timing: spawn.timing,
        };
        self.records.push(record);
    }

    pub fn finish(self) -> LoggedSpawns {
        LoggedSpawns {
            runners: self.runners.into_vec(),
            mnemonics: self.mnemonics.into_vec(),
            records: self.records,
        }
    }
}

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
            primary_output: None,
            timing: PhaseTiming::new(Duration::ZERO, [Duration::ZERO; REPORTED_PHASES]),
        }
    }

    #[test]
    fn names_are_shared_between_spawns() {
        let mut builder = Builder::with_capacity(3);
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
        let mut builder = Builder::with_capacity(2);
        builder.push(logged("remote", "A"));
        builder.push(Logged {
            target_label: "//a",
            ..logged("remote", "A")
        });
        let log = builder.finish();
        let labels: Vec<_> = log.iter().map(|s| s.target_label()).collect();
        assert_eq!(labels, [None, Some("//a")]);
    }
}
