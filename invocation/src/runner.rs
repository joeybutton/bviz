use std::time::Duration;

/// Where an action's spawn ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Runner {
    /// Sent to a remote executor, whose own cache may have answered it.
    RemoteExecution,
    /// Handed to a persistent worker.
    Worker,
    /// Run as a local subprocess in a sandbox.
    Sandboxed,
    /// Run as a local subprocess outside any sandbox.
    Local,
    /// Answered by a remote cache.
    RemoteCacheHit,
    /// Answered by a cache on the local disk.
    DiskCacheHit,
    /// No spawn is recorded on the thread running the action: work Bazel does
    /// in its own process, such as writing a file or a symlink, or an action
    /// whose spawns ran on other threads, as both branches of a dynamic
    /// execution (`--strategy=dynamic`) do.
    NoSpawnRecorded,
}

impl Runner {
    /// Every runner, in the order a reader is shown them.
    pub const ALL: [Runner; 7] = [
        Runner::RemoteExecution,
        Runner::Worker,
        Runner::Sandboxed,
        Runner::Local,
        Runner::RemoteCacheHit,
        Runner::DiskCacheHit,
        Runner::NoSpawnRecorded,
    ];

    /// The runner's place in [`ALL`](Self::ALL).
    pub const fn index(self) -> usize {
        self as usize
    }

    fn grade(self) -> Grade {
        match self {
            Runner::NoSpawnRecorded => Grade::NoSpawn,
            Runner::RemoteCacheHit | Runner::DiskCacheHit => Grade::CacheHit,
            Runner::RemoteExecution | Runner::Worker | Runner::Sandboxed | Runner::Local => {
                Grade::Execution
            }
        }
    }
}

/// How firmly a runner says where an action ran, weakest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Grade {
    NoSpawn,
    CacheHit,
    Execution,
}

/// How long the spawns of an action held each runner a profile saw them on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Spawns([Option<Duration>; Runner::ALL.len()]);

impl Spawns {
    /// A spawn that held `runner` for `time`.
    pub fn on(runner: Runner, time: Duration) -> Self {
        let mut spawns = Spawns::default();
        spawns.ran(runner, time);
        spawns
    }

    /// Counts a spawn that held `runner` for `time`.
    pub fn ran(&mut self, runner: Runner, time: Duration) {
        let held = &mut self.0[runner.index()];
        *held = Some(held.unwrap_or_default().saturating_add(time));
    }

    /// Counts the spawns of `other` among these.
    pub fn add(&mut self, other: &Spawns) {
        for runner in Runner::ALL {
            if let Some(time) = other.0[runner.index()] {
                self.ran(runner, time);
            }
        }
    }

    /// The runner said to have run the action: of those that executed it, the
    /// one its spawns held longest, so that neither a remote attempt failing
    /// fast before a local fallback nor a short secondary spawn decides;
    /// failing any execution, the cache that answered it; failing that, none
    /// recorded.
    pub fn runner(&self) -> Runner {
        Runner::ALL
            .into_iter()
            .filter_map(|runner| Some(((runner.grade(), self.0[runner.index()]?), runner)))
            .max_by_key(|&(rank, _)| rank)
            .map_or(Runner::NoSpawnRecorded, |(_, runner)| runner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_lists_every_runner_at_its_index() {
        for (i, runner) in Runner::ALL.into_iter().enumerate() {
            assert_eq!(runner.index(), i);
        }
    }

    fn spawns(ran: &[(Runner, u64)]) -> Spawns {
        let mut spawns = Spawns::default();
        for &(runner, millis) in ran {
            spawns.ran(runner, Duration::from_millis(millis));
        }
        spawns
    }

    #[test]
    fn the_execution_holding_the_most_time_decides() {
        use Runner::*;
        assert_eq!(spawns(&[(RemoteExecution, 2), (Local, 30)]).runner(), Local);
        assert_eq!(
            spawns(&[(RemoteExecution, 30), (Local, 2)]).runner(),
            RemoteExecution
        );
        assert_eq!(
            spawns(&[
                (Sandboxed, 20),
                (RemoteExecution, 15),
                (RemoteExecution, 10)
            ])
            .runner(),
            RemoteExecution
        );
    }

    #[test]
    fn any_execution_decides_over_a_cache_hit_and_a_hit_over_no_spawn() {
        use Runner::*;
        assert_eq!(spawns(&[(DiskCacheHit, 50), (Worker, 1)]).runner(), Worker);
        assert_eq!(spawns(&[(RemoteCacheHit, 0)]).runner(), RemoteCacheHit);
        assert_eq!(spawns(&[]).runner(), NoSpawnRecorded);
    }

    #[test]
    fn added_spawns_hold_their_runners_together() {
        use Runner::*;
        let mut first = spawns(&[(Sandboxed, 40), (RemoteExecution, 10)]);
        assert_eq!(first.runner(), Sandboxed);
        first.add(&spawns(&[(RemoteExecution, 35)]));
        assert_eq!(first.runner(), RemoteExecution);
    }
}
