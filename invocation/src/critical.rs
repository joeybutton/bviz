use crate::ConfiguredTarget;

/// One step of an invocation's critical path, by what its other outputs know
/// the step's action by: its primary output, and the target owning it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CriticalStep {
    primary_output: Option<String>,
    owner: Option<ConfiguredTarget>,
}

impl CriticalStep {
    pub fn new(primary_output: Option<&str>, owner: Option<ConfiguredTarget<&str>>) -> Self {
        Self {
            primary_output: primary_output.map(str::to_owned),
            owner: owner.map(ConfiguredTarget::owned),
        }
    }

    /// The exec path of the action's primary output.
    pub fn primary_output(&self) -> Option<&str> {
        self.primary_output.as_deref()
    }

    pub fn owner(&self) -> Option<ConfiguredTarget<&str>> {
        self.owner.as_ref().map(ConfiguredTarget::borrowed)
    }
}
