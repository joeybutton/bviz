//! The spawns an invocation ran, as its execution log records them.

mod dated;
mod logged;
mod timing;

pub use dated::{DatedSpawns, Placement};
pub use logged::{Builder, Logged, LoggedSpawn, LoggedSpawns, Names};
pub use timing::{Phase, PhaseTiming, REPORTED_PHASES};
