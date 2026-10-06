//! The spawns an invocation ran, as its execution log records them.

mod logged;
mod timing;

pub use logged::{Builder, Logged, LoggedSpawn, LoggedSpawns, SpawnRunner};
pub use timing::{Phase, PhaseTiming, REPORTED_PHASES};
