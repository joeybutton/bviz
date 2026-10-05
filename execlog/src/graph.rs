//! Reader for Bazel's execution graph log
//! (`--experimental_enable_execution_graph_log`): one node for each spawn a
//! spawn runner executed, and one for each action that ended without running
//! a spawn, each with when it started and how long it took.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use bviz_invocation::EpochMicros;
use bviz_invocation::spawn::{self, Logged, LoggedSpawn, Names, PhaseTiming, REPORTED_PHASES};
use bviz_parallel::worker_count;
use prost::DecodeError;
use rustc_hash::{FxBuildHasher, FxHashSet};

use crate::frame::{for_each_message, for_each_message_pipelined};
use crate::graph_fields::{metrics, node};
use crate::read::READ_CHUNK_BYTES;
use crate::wire::{Value, int32, next_field, text};
use crate::{ExecLog, LoadError};

mod dated;

pub use dated::DatedNodes;

/// Compressed log bytes per node, rounded down from the densest logs measured
/// so the node lists are reserved at or above their final length.
const COMPRESSED_BYTES_PER_NODE: u64 = 24;

/// What a cache answering a spawn appends to the name of the runner that
/// asked it, as in `remote cache hit`:
/// <https://github.com/bazelbuild/bazel/blob/9.2.0/src/main/java/com/google/devtools/build/lib/remote/util/Utils.java#L159>
const CACHE_HIT_SUFFIX: &str = " cache hit";

/// The nodes of one execution graph log.
///
/// A node with a runner is a spawn, and one without is of an action. Where a
/// spawn node of the same action, by owner, mnemonic and description, stands
/// in the log, the action ran that spawn; the others are
/// [`spawnless`](Self::spawnless).
pub struct GraphLog {
    spawns: ExecLog,
    /// By spawn, in the order of [`spawns`](Self::spawns), when it started on
    /// the local clock: when its action began discovering inputs, or when its
    /// runner was handed it where it discovered none.
    spawn_starts: Vec<EpochMicros>,
    /// Distinct target labels, which [`Spawnless::owner`] indexes.
    labels: Vec<Box<str>>,
    spawnless: Vec<Spawnless>,
}

/// An action node no spawn node of the same action accompanies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Spawnless {
    /// `None` for an action no target owns.
    owner: Option<usize>,
    start: EpochMicros,
    took: Duration,
}

/// An action of a [`GraphLog`] that ran no spawn: the action cache found it up
/// to date, Bazel pruned it as unchanged, it ran within Bazel, or a remote or
/// disk cache answered its spawns, which the log does not tell apart: Bazel
/// writes such a node of every action that completed, whether it ran spawns
/// or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnlessAction<'a> {
    /// The label of the target owning the action; `None` for one no target
    /// owns.
    pub target_label: Option<&'a str>,
    /// On the local clock.
    pub start: EpochMicros,
    pub took: Duration,
}

impl GraphLog {
    pub fn from_path(path: &Path) -> Result<GraphLog, LoadError> {
        let file = File::open(path).map_err(LoadError::Io)?;
        let compressed = file.metadata().map_err(LoadError::Io)?.len();
        read_compressed(file, compressed)
    }

    /// The spawn nodes, as an execution log of the spawns they record, which
    /// names no outputs; [`spawns_started`](Self::spawns_started) gives when
    /// each started.
    pub fn spawns(&self) -> &ExecLog {
        &self.spawns
    }

    /// Each spawn node's spawn, with when it started.
    pub fn spawns_started(&self) -> impl ExactSizeIterator<Item = (LoggedSpawn<'_>, EpochMicros)> {
        self.spawns
            .spawns
            .iter()
            .zip(self.spawn_starts.iter().copied())
    }

    /// The actions that ran no spawn, in the order the log lists them.
    pub fn spawnless(&self) -> impl ExactSizeIterator<Item = SpawnlessAction<'_>> {
        self.spawnless.iter().map(|action| SpawnlessAction {
            target_label: action.owner.map(|owner| &*self.labels[owner]),
            start: action.start,
            took: action.took,
        })
    }
}

/// Reads a log of `compressed` bytes from `log`.
fn read_compressed(log: impl Read + Send, compressed: u64) -> Result<GraphLog, LoadError> {
    let decoder = zstd::stream::read::Decoder::new(log).map_err(LoadError::Io)?;
    let nodes = usize::try_from(compressed / COMPRESSED_BYTES_PER_NODE).unwrap_or(usize::MAX);
    let mut builder = Builder::with_capacity(nodes);
    let add = |node: &[u8]| Ok(builder.add(node)?);
    match worker_count() > 1 {
        true => for_each_message_pipelined(decoder, READ_CHUNK_BYTES, add)?,
        false => for_each_message(decoder, READ_CHUNK_BYTES, add)?,
    }
    Ok(builder.finish())
}

/// An action as its nodes name it: by the label of its owner, its mnemonic and
/// its description, each interned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ActionKey {
    owner: usize,
    mnemonic: usize,
    description: usize,
}

struct Builder {
    spawns: spawn::Builder,
    spawn_starts: Vec<EpochMicros>,
    labels: Names<FxBuildHasher>,
    /// Mnemonics and descriptions alike.
    words: Names<FxBuildHasher>,
    /// The actions spawn nodes ran spawns of.
    ran: FxHashSet<ActionKey>,
    /// The action nodes, in log order, each with its action, start and
    /// duration.
    actions: Vec<(ActionKey, EpochMicros, Duration)>,
}

/// One node's metrics, in milliseconds.
#[derive(Default)]
struct Metrics {
    start: i64,
    duration: u64,
    discover_inputs: u64,
    /// In the order of [`spawn::Phase::ALL`].
    reported: [u64; REPORTED_PHASES],
}

impl Builder {
    fn with_capacity(nodes: usize) -> Builder {
        Builder {
            spawns: spawn::Builder::with_capacity(nodes, 0, 0),
            spawn_starts: Vec::with_capacity(nodes),
            labels: Names::default(),
            words: Names::with_capacity(nodes),
            ran: FxHashSet::with_capacity_and_hasher(nodes, Default::default()),
            actions: Vec::with_capacity(nodes),
        }
    }

    fn add(&mut self, mut bytes: &[u8]) -> Result<(), DecodeError> {
        let mut description = "";
        let mut target_label = "";
        let mut mnemonic = "";
        let mut runner = "";
        let mut identifier = "";
        let mut metrics = Metrics::default();
        while !bytes.is_empty() {
            match next_field(&mut bytes)? {
                (node::DESCRIPTION, Value::Bytes(value)) => description = text(value)?,
                (node::TARGET_LABEL, Value::Bytes(value)) => target_label = text(value)?,
                (node::MNEMONIC, Value::Bytes(value)) => mnemonic = text(value)?,
                (node::RUNNER, Value::Bytes(value)) => runner = text(value)?,
                (node::IDENTIFIER, Value::Bytes(value)) => identifier = text(value)?,
                (node::METRICS, Value::Bytes(value)) => metrics = read_metrics(value)?,
                _ => {}
            }
        }
        let action = ActionKey {
            owner: self.labels.intern(target_label),
            mnemonic: self.words.intern(mnemonic),
            description: self.words.intern(description),
        };
        let start = EpochMicros::from_millis(metrics.start);
        if runner.is_empty() {
            let took = Duration::from_millis(metrics.duration);
            self.actions.push((action, start, took));
            return Ok(());
        }
        self.ran.insert(action);
        if !identifier.is_empty() {
            self.spawns.digest(identifier);
        }
        let discovering = Duration::from_millis(metrics.discover_inputs);
        self.spawn_starts.push(start.before(discovering));
        self.spawns.push(Logged {
            runner,
            mnemonic,
            target_label,
            cache_hit: runner.ends_with(CACHE_HIT_SUFFIX),
            timing: PhaseTiming::new(
                Duration::from_millis(metrics.duration),
                metrics.reported.map(Duration::from_millis),
            ),
        });
        Ok(())
    }

    fn finish(self) -> GraphLog {
        let labels = self.labels.into_vec();
        let ran = self.ran;
        let spawnless = self
            .actions
            .into_iter()
            .filter(|(action, _, _)| !ran.contains(action))
            .map(|(action, start, took)| Spawnless {
                owner: (!labels[action.owner].is_empty()).then_some(action.owner),
                start,
                took,
            })
            .collect();
        GraphLog {
            spawns: ExecLog {
                spawns: self.spawns.finish(),
            },
            spawn_starts: self.spawn_starts,
            labels,
            spawnless,
        }
    }
}

/// The metrics encoded in `bytes`; a negative duration reads as none.
fn read_metrics(mut bytes: &[u8]) -> Result<Metrics, DecodeError> {
    use spawn::Phase;
    let mut read = Metrics::default();
    while !bytes.is_empty() {
        let (field, Value::Varint(value)) = next_field(&mut bytes)? else {
            continue;
        };
        let millis = u64::try_from(int32(value)).unwrap_or(0);
        let phase = match field {
            metrics::START_TIMESTAMP_MILLIS => {
                read.start = value as i64;
                continue;
            }
            metrics::DURATION_MILLIS => {
                read.duration = millis;
                continue;
            }
            metrics::DISCOVER_INPUTS_MILLIS => {
                read.discover_inputs = millis;
                continue;
            }
            metrics::PARSE_MILLIS => Phase::Parse,
            metrics::UPLOAD_MILLIS => Phase::Upload,
            metrics::NETWORK_MILLIS => Phase::Network,
            metrics::QUEUE_MILLIS => Phase::Queue,
            metrics::SETUP_MILLIS => Phase::Setup,
            metrics::PROCESS_MILLIS => Phase::Execute,
            metrics::PROCESS_OUTPUTS_MILLIS => Phase::ProcessOutputs,
            metrics::FETCH_MILLIS => Phase::Fetch,
            metrics::RETRY_MILLIS => Phase::Retry,
            _ => continue,
        };
        read.reported[phase.index()] = millis;
    }
    Ok(read)
}

#[cfg(test)]
mod tests {
    use bazel_spawn_proto::execution_graph::{Metrics as ProtoMetrics, Node};
    use prost::Message;

    use super::*;
    use crate::Phase;

    const ORIGIN_MS: i64 = 1_781_439_245_231;

    fn encode(nodes: &[Node]) -> Vec<u8> {
        let mut decoded = Vec::new();
        for node in nodes {
            node.encode_length_delimited(&mut decoded)
                .expect("a Vec grows to fit");
        }
        zstd::encode_all(decoded.as_slice(), 0).expect("compress in memory")
    }

    pub(super) fn load(nodes: &[Node]) -> GraphLog {
        let log = encode(nodes);
        read_compressed(log.as_slice(), log.len() as u64).expect("read the log")
    }

    /// A node of the action `description` of `//p:t`, started `start`
    /// milliseconds into the build and lasting `duration`.
    pub(super) fn node(runner: &str, description: &str, start: i64, duration: i32) -> Node {
        Node {
            description: description.into(),
            target_label: "//p:t".into(),
            mnemonic: "Genrule".into(),
            runner: runner.into(),
            metrics: Some(ProtoMetrics {
                start_timestamp_millis: ORIGIN_MS + start,
                duration_millis: duration,
                process_millis: duration,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    pub(super) fn at(millis: i64) -> EpochMicros {
        EpochMicros::from_millis(ORIGIN_MS + millis)
    }

    #[test]
    fn a_node_with_a_runner_is_a_spawn() {
        let mut remote = node("remote", "action 'Compiling a.cc'", 100, 50);
        remote.identifier = "9f86d081".into();
        remote.metrics = Some(ProtoMetrics {
            start_timestamp_millis: ORIGIN_MS + 100,
            duration_millis: 50,
            queue_millis: 10,
            process_millis: 30,
            fetch_millis: 5,
            discover_inputs_millis: 4,
            ..Default::default()
        });
        let log = load(&[
            remote,
            node("remote cache hit", "action 'Compiling b.cc'", 200, 3),
        ]);
        let spawns: Vec<_> = log.spawns().spawns().collect();
        assert_eq!(spawns.len(), 2);
        assert_eq!(spawns[0].runner(), "remote");
        assert_eq!(spawns[0].mnemonic(), "Genrule");
        assert_eq!(spawns[0].target_label(), Some("//p:t"));
        assert_eq!(spawns[0].digest(), Some("9f86d081"));
        let starts: Vec<_> = log.spawns_started().map(|(_, start)| start).collect();
        assert_eq!(starts, [at(96), at(200)]);
        assert!(!spawns[0].cache_hit());
        let timing = spawns[0].timing();
        assert_eq!(timing.total(), Duration::from_millis(50));
        assert_eq!(timing.phase(Phase::Queue), Duration::from_millis(10));
        assert_eq!(timing.phase(Phase::Execute), Duration::from_millis(30));
        assert_eq!(timing.phase(Phase::Fetch), Duration::from_millis(5));
        assert_eq!(timing.phase(Phase::Other), Duration::from_millis(5));
        assert_eq!(spawns[1].digest(), None);
        assert!(spawns[1].cache_hit());
        assert_eq!(log.spawnless().len(), 0);
    }

    #[test]
    fn an_action_node_beside_a_spawn_node_of_its_action_ran_that_spawn() {
        let log = load(&[
            node("linux-sandbox", "action 'Executing genrule //p:t'", 0, 40),
            node("", "action 'Executing genrule //p:t'", 0, 45),
            node("", "action 'Expanding template p/t.sh'", 50, 2),
        ]);
        assert_eq!(log.spawns().spawn_count(), 1);
        assert_eq!(
            log.spawnless().collect::<Vec<_>>(),
            [SpawnlessAction {
                target_label: Some("//p:t"),
                start: at(50),
                took: Duration::from_millis(2),
            }]
        );
    }

    #[test]
    fn an_action_node_ahead_of_its_spawn_node_still_ran_it() {
        let log = load(&[
            node("", "action 'Executing genrule //p:t'", 0, 45),
            node("linux-sandbox", "action 'Executing genrule //p:t'", 0, 40),
        ]);
        assert_eq!(log.spawnless().len(), 0);
    }

    #[test]
    fn an_action_is_told_apart_by_owner_mnemonic_and_description() {
        let description = "action 'Executing genrule //p:t'";
        let mut owned_elsewhere = node("", description, 0, 1);
        owned_elsewhere.target_label = "//q:t".into();
        let mut other_mnemonic = node("", description, 0, 1);
        other_mnemonic.mnemonic = "Javac".into();
        let mut unowned = node("", description, 0, 0);
        unowned.target_label = String::new();
        let log = load(&[
            node("local", description, 0, 1),
            owned_elsewhere,
            other_mnemonic,
            unowned,
        ]);
        let owners: Vec<_> = log.spawnless().map(|a| a.target_label).collect();
        assert_eq!(owners, [Some("//q:t"), Some("//p:t"), None]);
    }

    #[test]
    fn a_negative_duration_reads_as_none() {
        let log = load(&[node("local", "action 'a'", 0, -5)]);
        let spawn = log.spawns().spawns().next().expect("a spawn");
        assert_eq!(spawn.timing().total(), Duration::ZERO);
    }

    #[test]
    fn a_cut_off_log_fails() {
        let log = encode(&[node("local", "action 'a'", 0, 1)]);
        let cut = &log[..log.len() - 1];
        assert!(read_compressed(cut, cut.len() as u64).is_err());
    }
}
