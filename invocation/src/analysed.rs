//! When a build finished analysing the targets it was asked for.

use crate::outcomes::PerTarget;
use crate::{ConfiguredTarget, EpochMicros};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_is_analysed_at_the_earliest_instant_recorded() {
        let at = |millis: i64| EpochMicros::from_millis(1_781_439_245_000 + millis);
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
