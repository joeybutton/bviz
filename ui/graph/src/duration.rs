//! How long something took, in words.

use std::time::Duration;

/// How long a run took, to the precision a reader compares runs at.
pub(crate) fn lasted(run: Duration) -> String {
    const MILLIS: u64 = 1_000;
    const TENTHS: u64 = 10;
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    let millis = (run.as_secs_f64() * MILLIS as f64).round() as u64;
    let tenths = (run.as_secs_f64() * TENTHS as f64).round() as u64;
    let seconds = run.as_secs_f64().round() as u64;
    if millis == 0 {
        "under 1ms".to_owned()
    } else if millis < MILLIS / TENTHS {
        format!("{millis}ms")
    } else if tenths < MINUTE * TENTHS {
        format!("{}.{}s", tenths / TENTHS, tenths % TENTHS)
    } else if seconds < HOUR {
        format!("{}m {:02}s", seconds / MINUTE, seconds % MINUTE)
    } else {
        const MINUTES_PER_HOUR: u64 = HOUR / MINUTE;
        let minutes = (run.as_secs_f64() / MINUTE as f64).round() as u64;
        format!(
            "{}h {:02}m",
            minutes / MINUTES_PER_HOUR,
            minutes % MINUTES_PER_HOUR
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run is timed as it rounds, so one rounding up to a minute reads as
    /// the minute, and one rounding to no millisecond as under one.
    #[test]
    fn a_run_is_timed_to_the_precision_it_is_compared_at() {
        let millis = |millis| lasted(Duration::from_millis(millis));
        let micros = |micros| lasted(Duration::from_micros(micros));
        assert_eq!(millis(0), "under 1ms");
        assert_eq!(micros(499), "under 1ms");
        assert_eq!(micros(500), "1ms");
        assert_eq!(millis(49), "49ms");
        assert_eq!(micros(99_499), "99ms");
        assert_eq!(micros(99_500), "0.1s");
        assert_eq!(millis(3_038), "3.0s");
        assert_eq!(millis(59_949), "59.9s");
        assert_eq!(millis(59_950), "1m 00s");
        assert_eq!(millis(59_999), "1m 00s");
        assert_eq!(millis(125_400), "2m 05s");
        assert_eq!(millis(3_599_600), "1h 00m");
        assert_eq!(millis((3 * 3600 + 7 * 60 + 29) * 1000), "3h 07m");
        assert_eq!(millis((3 * 3600 + 7 * 60 + 30) * 1000), "3h 08m");
        assert_eq!(millis((3 * 3600 + 59 * 60 + 30) * 1000), "4h 00m");
    }
}
