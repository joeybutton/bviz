use gpui::Task;

/// Bookkeeping for a value rebuilt in the background and superseded on stale
/// inputs: the previous value stays shown while a rebuild for a newer key runs,
/// and a build whose key the inputs have moved past is discarded when it lands.
pub(crate) struct Rebuild<K> {
    /// The key the ready value reflects, if any.
    ready: Option<K>,
    /// The key a background build is running for.
    inflight: Option<K>,
    /// Bumped on every (re)spawn so a superseded build's result is discarded
    /// when it lands after the inputs have moved on.
    r#gen: u64,
    /// Holds the background task so it isn't cancelled on drop.
    task: Task<()>,
}

impl<K: Copy + PartialEq> Rebuild<K> {
    pub(crate) fn new() -> Self {
        Self { ready: None, inflight: None, r#gen: 0, task: Task::ready(()) }
    }

    pub(crate) fn is_ready(&self, key: K) -> bool {
        self.ready == Some(key)
    }

    /// The key the ready value reflects, if any.
    pub(crate) fn ready(&self) -> Option<K> {
        self.ready
    }

    pub(crate) fn is_inflight(&self, key: K) -> bool {
        self.inflight == Some(key)
    }

    /// Whether a background build is currently running for any key.
    pub(crate) fn is_building(&self) -> bool {
        self.inflight.is_some()
    }

    /// Cancel an in-flight build (bumping gen so its late result is discarded).
    pub(crate) fn cancel_inflight(&mut self) {
        if self.inflight.take().is_some() {
            self.r#gen += 1;
            self.task = Task::ready(());
        }
    }

    /// Mark `key` ready without a build (a synchronously computed value).
    pub(crate) fn mark_ready(&mut self, key: K) {
        self.ready = Some(key);
    }

    /// Begin a build for `key`; returns the generation to check on completion.
    pub(crate) fn start(&mut self, key: K) -> u64 {
        self.inflight = Some(key);
        self.r#gen += 1;
        self.r#gen
    }

    pub(crate) fn set_task(&mut self, task: Task<()>) {
        self.task = task;
    }

    /// On completion: false if superseded (discard the result); otherwise record
    /// `key` ready and clear the in-flight marker.
    pub(crate) fn accept(&mut self, generation: u64, key: K) -> bool {
        if self.r#gen != generation {
            return false;
        }
        self.ready = Some(key);
        self.inflight = None;
        true
    }

    /// Drop readiness and supersede any running build (used on full invalidation).
    pub(crate) fn invalidate(&mut self) {
        self.ready = None;
        self.inflight = None;
        self.r#gen += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::Rebuild;

    #[test]
    fn a_build_runs_from_its_start_until_its_result_is_accepted() {
        let mut r: Rebuild<u64> = Rebuild::new();
        assert!(!r.is_building());
        assert_eq!(r.ready(), None);

        let generation = r.start(1);
        assert!(r.is_building());
        assert!(r.is_inflight(1));
        // Still nothing to show: whatever was shown before stays.
        assert_eq!(r.ready(), None);

        assert!(r.accept(generation, 1));
        assert!(!r.is_building());
        assert!(r.is_ready(1));
    }

    #[test]
    fn a_superseded_result_is_discarded_and_leaves_the_newer_build_running() {
        let mut r: Rebuild<u64> = Rebuild::new();
        let stale = r.start(1);
        let current = r.start(2);
        // Key 1's result lands after the inputs moved on to key 2.
        assert!(!r.accept(stale, 1));
        assert_eq!(r.ready(), None);
        assert!(r.is_building());
        assert!(r.is_inflight(2));

        assert!(r.accept(current, 2));
        assert!(r.is_ready(2));
        assert!(!r.is_building());
    }

    #[test]
    fn cancelling_ends_the_build_and_keeps_the_ready_value() {
        let mut r: Rebuild<u64> = Rebuild::new();
        let first = r.start(1);
        assert!(r.accept(first, 1));

        let cancelled = r.start(2);
        r.cancel_inflight();
        assert!(!r.is_building());
        assert!(r.is_ready(1));
        // The cancelled build's late result cannot clobber the shown value.
        assert!(!r.accept(cancelled, 2));
        assert!(r.is_ready(1));
    }

    #[test]
    fn invalidating_drops_readiness_and_supersedes_the_running_build() {
        let mut r: Rebuild<u64> = Rebuild::new();
        let first = r.start(1);
        assert!(r.accept(first, 1));

        let running = r.start(2);
        r.invalidate();
        assert_eq!(r.ready(), None);
        assert!(!r.is_building());
        assert!(!r.accept(running, 2));
        assert_eq!(r.ready(), None);
    }
}
