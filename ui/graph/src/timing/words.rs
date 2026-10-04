//! The words the graph's views say the action times placed on it in.

use super::{Coverage, Ran};
use bviz_invocation::Runner;

impl Coverage {
    pub(crate) fn line(self) -> String {
        format!(
            "{} of {} profiled actions placed on this graph",
            self.placed, self.profiled
        )
    }
}

impl Ran {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Ran::On(Runner::RemoteExecution) => "remote execution",
            Ran::On(Runner::Worker) => "persistent worker",
            Ran::On(Runner::Sandboxed) => "sandboxed",
            Ran::On(Runner::Local) => "local, unsandboxed",
            Ran::On(Runner::RemoteCacheHit) => "remote cache hit",
            Ran::On(Runner::DiskCacheHit) => "disk cache hit",
            Ran::On(Runner::NoSpawnRecorded) => "no spawn recorded",
            Ran::Mixed => "mixed runners",
        }
    }
}
