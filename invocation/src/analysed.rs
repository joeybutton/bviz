//! When a build analysed its targets.

use crate::outcomes::PerTarget;
use crate::{ConfiguredTarget, EpochMicros, Interval};

/// The instant a build finished analysing each top-level configured target,
/// where it says. A build tells this only of the targets it was asked for, so
/// of most of a build graph it tells nothing.
#[derive(Default)]
pub struct Analysed(PerTarget<EpochMicros>);

impl Analysed {
    /// Records that `target` was analysed at `at`; of a target recorded
    /// again, the earliest instant is kept.
    pub fn record(&mut self, target: ConfiguredTarget, at: EpochMicros) {
        self.0
            .report(target, at, |held, at| *held = (*held).min(at));
    }

    /// When `target` was analysed; `None` for a target the build does not
    /// say it analysed.
    pub fn at(&self, target: ConfiguredTarget<&str>) -> Option<EpochMicros> {
        self.0.get(target).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (ConfiguredTarget<&str>, EpochMicros)> {
        self.0.iter().map(|(target, &at)| (target, at))
    }
}

/// When a build was analysing each target: from the start of the first stretch
/// of analysis recorded of it to the end of the last.
#[derive(Default)]
pub struct Analysing(PerTarget<Interval>);

impl Analysing {
    pub fn with_capacity(labels: usize) -> Self {
        Self(PerTarget::with_capacity(labels))
    }

    /// Records that `target` was being analysed throughout `during`.
    pub fn record(&mut self, target: ConfiguredTarget, during: Interval) {
        self.0
            .report(target, during, |held, during| *held = held.hull(during));
    }

    /// When `target` was being analysed; `None` for a target no analysis is
    /// recorded of.
    pub fn during(&self, target: ConfiguredTarget<&str>) -> Option<Interval> {
        self.0.get(target).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (ConfiguredTarget<&str>, Interval)> {
        self.0.iter().map(|(target, &during)| (target, during))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(millis: i64) -> EpochMicros {
        EpochMicros::from_millis(1_781_439_245_000 + millis)
    }

    #[test]
    fn a_target_is_analysed_from_the_first_stretch_recorded_to_the_last() {
        let lib = || ConfiguredTarget::new("//lib".to_owned(), None);
        let mut analysing = Analysing::with_capacity(1);
        assert!(analysing.is_empty());
        analysing.record(lib(), Interval::new(at(20), at(30)).unwrap());
        analysing.record(lib(), Interval::new(at(10), at(15)).unwrap());
        assert_eq!(
            analysing.during(lib().borrowed()),
            Interval::new(at(10), at(30))
        );
        assert_eq!(analysing.during(ConfiguredTarget::new("//app", None)), None);
        assert_eq!(analysing.iter().count(), 1);
    }

    #[test]
    fn a_target_is_analysed_at_the_earliest_instant_recorded() {
        let app = |configuration: &str| {
            ConfiguredTarget::new("//app".to_owned(), Some(configuration.to_owned()))
        };
        let mut analysed = Analysed::default();
        assert!(analysed.is_empty());
        analysed.record(app("a7a71f"), at(20));
        analysed.record(app("a7a71f"), at(10));
        analysed.record(app("960ec4"), at(30));
        assert_eq!(analysed.at(app("a7a71f").borrowed()), Some(at(10)));
        assert_eq!(analysed.at(app("960ec4").borrowed()), Some(at(30)));
        assert_eq!(analysed.at(ConfiguredTarget::new("//app", None)), None);
        assert_eq!(analysed.iter().count(), 2);
    }
}
