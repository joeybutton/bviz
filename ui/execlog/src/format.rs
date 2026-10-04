//! How phases, durations and shares read on screen.

use bviz_execlog::Phase;
use std::time::Duration;

pub(crate) fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Parse => "parse",
        Phase::Upload => "upload",
        Phase::Network => "network",
        Phase::Queue => "queue",
        Phase::Setup => "setup",
        Phase::Execute => "execute",
        Phase::ProcessOutputs => "process outputs",
        Phase::Fetch => "fetch",
        Phase::Retry => "retry",
        Phase::Other => "other",
    }
}

const SECS_PER_MINUTE: f64 = 60.0;
const SECS_PER_HOUR: f64 = 60.0 * SECS_PER_MINUTE;

/// `d` in the largest unit that leaves it at least one.
pub(crate) fn duration(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if d.is_zero() {
        "0 ms".into()
    } else if d < Duration::from_millis(1) {
        "<1 ms".into()
    } else if secs < 1.0 {
        format!("{} ms", d.as_millis())
    } else if secs < SECS_PER_MINUTE {
        format!("{secs:.1} s")
    } else if secs < SECS_PER_HOUR {
        format!("{:.1} min", secs / SECS_PER_MINUTE)
    } else {
        format!("{:.1} h", secs / SECS_PER_HOUR)
    }
}

/// `d` in seconds to the millisecond, for where the rounding [`duration`]
/// does would hide a difference.
pub(crate) fn exact_duration(d: Duration) -> String {
    format!("{}.{:03} s", d.as_secs(), d.subsec_millis())
}

/// `part` as a percentage of `whole`.
pub(crate) fn share(part: Duration, whole: Duration) -> String {
    if whole.is_zero() {
        return "0%".into();
    }
    format!("{:.1}%", 100.0 * part.as_secs_f64() / whole.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_duration_reads_in_the_largest_unit_it_fills() {
        assert_eq!(duration(Duration::ZERO), "0 ms");
        assert_eq!(duration(Duration::from_micros(400)), "<1 ms");
        assert_eq!(duration(Duration::from_millis(850)), "850 ms");
        assert_eq!(duration(Duration::from_millis(12_340)), "12.3 s");
        assert_eq!(duration(Duration::from_secs(150)), "2.5 min");
        assert_eq!(duration(Duration::from_secs(5400)), "1.5 h");
    }

    #[test]
    fn an_exact_duration_reads_to_the_millisecond() {
        assert_eq!(exact_duration(Duration::from_micros(400)), "0.000 s");
        assert_eq!(exact_duration(Duration::from_millis(12_345)), "12.345 s");
        assert_eq!(exact_duration(Duration::from_secs(5400)), "5400.000 s");
    }

    #[test]
    fn a_share_is_a_percentage_and_of_nothing_zero() {
        assert_eq!(
            share(Duration::from_secs(1), Duration::from_secs(4)),
            "25.0%"
        );
        assert_eq!(share(Duration::ZERO, Duration::ZERO), "0%");
    }
}
