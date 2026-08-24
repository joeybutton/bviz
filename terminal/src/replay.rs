//! A build's output organized for playback over time.
//!
//! Each Bazel progress event is one frame — a prefix of the captured output that
//! reconstructs the terminal as it stood at that point. The challenge is timing:
//! Bazel does not timestamp progress events. When the build was captured with
//! `--show_timestamps`, each progress repaint and message carries a `(HH:MM:SS)`
//! prefix in its text, giving real wall-clock anchors that are parsed back out
//! here. Otherwise the timeline is interpolated between the sparse events that do
//! carry timestamps (build start and finish, action and test completions).

use crate::display::DisplaySource;
use crate::output::{Anchor, BuildOutput};

/// One playback frame: the output reconstructs the terminal at byte offset `end`,
/// shown `time` seconds into the build.
struct Frame {
    end: usize,
    time: f64,
}

/// A build's output laid out as a timed sequence of frames.
pub struct Replay {
    bytes: Vec<u8>,
    columns: u16,
    frames: Vec<Frame>,
    real_time: bool,
}

impl Replay {
    /// Lay out a parsed build's output for playback, choosing real timestamps when
    /// the capture carries them and interpolating otherwise.
    pub fn from_output(output: BuildOutput) -> Replay {
        let BuildOutput {
            columns,
            bytes,
            frames,
            anchors,
            show_timestamps,
        } = output;
        // A build with no progress events still plays as a single frame holding
        // whatever output there is.
        let ends = if frames.is_empty() {
            vec![bytes.len()]
        } else {
            frames
        };
        let count = ends.len();

        // Read the real `(HH:MM:SS)` markers only when the build was run with
        // --show_timestamps, so a stray time-shaped token in ordinary output never
        // masquerades as a timeline; otherwise interpolate from event timestamps.
        let markers = if show_timestamps {
            parse_markers(&bytes)
        } else {
            Vec::new()
        };
        let (points, real_time) = if markers.is_empty() {
            (event_points(&anchors, count), false)
        } else {
            (marker_points(&markers, &ends), true)
        };

        let times = interpolate(points, count);
        let frames = ends
            .into_iter()
            .zip(times)
            .map(|(end, time)| Frame { end, time })
            .collect();
        Replay {
            bytes,
            columns,
            frames,
            real_time,
        }
    }

    /// The number of frames; always at least one.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Total playback length in seconds: the time of the last frame.
    pub fn duration(&self) -> f64 {
        self.frames.last().map_or(0.0, |frame| frame.time)
    }

    /// Whether the timeline is from real `--show_timestamps` markers (`true`) or
    /// interpolated from the build's sparse event timestamps (`false`).
    pub fn real_time(&self) -> bool {
        self.real_time
    }

    /// The time in seconds at which a frame is shown.
    pub fn time_of(&self, frame: usize) -> f64 {
        self.frames[frame.min(self.frames.len() - 1)].time
    }

    /// The last frame whose time is at or before `seconds`.
    pub fn frame_at(&self, seconds: f64) -> usize {
        let next = self.frames.partition_point(|frame| frame.time <= seconds);
        next.saturating_sub(1)
    }

    /// Reconstruct the terminal as it stood at the end of `frame`.
    pub fn display_at(&self, frame: usize) -> DisplaySource {
        let end = self.frames[frame.min(self.frames.len() - 1)].end;
        DisplaySource::from_bytes(&self.bytes[..end], self.columns)
    }
}

/// A `(HH:MM:SS)` timestamp Bazel emits with `--show_timestamps`, recovered from
/// the output: its byte offset and its time of day in seconds.
struct Marker {
    offset: usize,
    secs_of_day: u32,
}

/// Find every `(HH:MM:SS)` marker in the output, in offset order.
fn parse_markers(bytes: &[u8]) -> Vec<Marker> {
    // The fixed shape `(dd:dd:dd)` is ten bytes; scan for it directly.
    let mut markers = Vec::new();
    let mut i = 0;
    while i + 10 <= bytes.len() {
        if let Some(secs) = parse_marker(&bytes[i..i + 10]) {
            markers.push(Marker {
                offset: i,
                secs_of_day: secs,
            });
            i += 10;
        } else {
            i += 1;
        }
    }
    markers
}

/// Parse the time of day from a ten-byte `(HH:MM:SS)` window, if it is one.
fn parse_marker(window: &[u8]) -> Option<u32> {
    let digit = |b: u8| (b as char).to_digit(10);
    if window[0] != b'(' || window[3] != b':' || window[6] != b':' || window[9] != b')' {
        return None;
    }
    let h = digit(window[1])? * 10 + digit(window[2])?;
    let m = digit(window[4])? * 10 + digit(window[5])?;
    let s = digit(window[7])? * 10 + digit(window[8])?;
    if h >= 24 || m >= 60 || s >= 60 {
        return None;
    }
    Some((h * 3600 + m * 60 + s) as u32)
}

const SECONDS_PER_DAY: f64 = 24.0 * 3600.0;

/// Just before midnight and just after, for telling a real day rollover from a
/// stray out-of-order marker.
const LATE_NIGHT: u32 = 23 * 3600;
const EARLY_MORNING: u32 = 3600;

/// Anchor points `(frame_index, seconds)` from `--show_timestamps` markers. Times
/// of day are unwrapped across midnight; markers that step backwards without a
/// plausible rollover are dropped as coincidental matches in the output.
fn marker_points(markers: &[Marker], ends: &[usize]) -> Vec<(usize, f64)> {
    let mut points = Vec::new();
    let mut day = 0i64;
    let mut prev_of_day: Option<u32> = None;
    let mut last_abs = f64::NEG_INFINITY;
    for marker in markers {
        if let Some(prev) = prev_of_day {
            if marker.secs_of_day < prev {
                if prev >= LATE_NIGHT && marker.secs_of_day < EARLY_MORNING {
                    day += 1;
                } else {
                    // Backwards without a midnight rollover: not a timestamp.
                    continue;
                }
            }
        }
        let abs = day as f64 * SECONDS_PER_DAY + marker.secs_of_day as f64;
        if abs < last_abs {
            continue;
        }
        prev_of_day = Some(marker.secs_of_day);
        last_abs = abs;
        points.push((frame_of(ends, marker.offset), abs));
    }
    points
}

/// The frame an output offset falls in: the first frame whose output extends past
/// it.
fn frame_of(ends: &[usize], offset: usize) -> usize {
    let frame = ends.partition_point(|&end| end <= offset);
    frame.min(ends.len() - 1)
}

/// Anchor points from the build's timestamped events, each clamped to a real frame
/// and dated relative to the first.
fn event_points(anchors: &[Anchor], count: usize) -> Vec<(usize, f64)> {
    anchors
        .iter()
        .map(|anchor| (anchor.frame.min(count - 1), anchor.time))
        .collect()
}

/// Anchor points sorted by frame, collapsed to one per frame (keeping the latest
/// time seen there), forced non-decreasing in time, and shifted so the timeline
/// starts at zero.
fn sanitize_anchors(mut points: Vec<(usize, f64)>) -> Vec<(usize, f64)> {
    points.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let mut anchors: Vec<(usize, f64)> = Vec::new();
    for (frame, time) in points {
        let time = anchors.last().map_or(time, |&(_, t)| time.max(t));
        match anchors.last_mut() {
            Some(last) if last.0 == frame => last.1 = time,
            _ => anchors.push((frame, time)),
        }
    }
    if let Some(&(_, base)) = anchors.first() {
        for anchor in &mut anchors {
            anchor.1 -= base;
        }
    }
    anchors
}

/// Assign every frame a time by linear interpolation between anchor points.
///
/// Frames before the first anchor or after the last take that anchor's time.
/// With no anchors the timeline is flat at zero.
fn interpolate(points: Vec<(usize, f64)>, count: usize) -> Vec<f64> {
    let anchors = sanitize_anchors(points);
    if anchors.is_empty() {
        return vec![0.0; count];
    }

    let mut times = Vec::with_capacity(count);
    for frame in 0..count {
        let time = match anchors.partition_point(|&(f, _)| f <= frame) {
            // Before the first anchor.
            0 => anchors[0].1,
            // At or after the last anchor.
            next if next == anchors.len() => anchors[next - 1].1,
            next => {
                let (fa, ta) = anchors[next - 1];
                let (fb, tb) = anchors[next];
                if fb == fa {
                    ta
                } else {
                    ta + (tb - ta) * (frame - fa) as f64 / (fb - fa) as f64
                }
            }
        };
        times.push(time);
    }
    times
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = include_bytes!("../testdata/sample.bep");

    #[test]
    fn parses_markers_and_rejects_non_markers() {
        let bytes = b"(14:47:41) start\nnot (99:99:99) a time\n(14:47:43) next";
        let markers = parse_markers(bytes);
        let times: Vec<u32> = markers.iter().map(|m| m.secs_of_day).collect();
        assert_eq!(
            times,
            vec![14 * 3600 + 47 * 60 + 41, 14 * 3600 + 47 * 60 + 43]
        );
    }

    #[test]
    fn marker_points_unwrap_real_midnight_but_drop_strays() {
        let ends = [10, 20, 30, 40];
        // A genuine rollover (23:59:59 → 00:00:01) advances a day; a stray earlier
        // time mid-stream is dropped, not treated as another rollover.
        let markers = vec![
            Marker {
                offset: 0,
                secs_of_day: 23 * 3600 + 59 * 60 + 59,
            },
            Marker {
                offset: 12,
                secs_of_day: 1,
            },
            Marker {
                offset: 22,
                secs_of_day: 12 * 3600,
            },
            Marker {
                offset: 32,
                secs_of_day: 2,
            },
        ];
        let points = marker_points(&markers, &ends);
        // The 12:00:00 stray after 00:00:01 is dropped; the rollover is kept.
        assert_eq!(points.len(), 3);
        assert!(points.windows(2).all(|w| w[0].1 <= w[1].1));
        // 00:00:01 the day after 23:59:59 is two seconds later.
        assert_eq!(points[1].1 - points[0].1, 2.0);
    }

    #[test]
    fn interpolation_spreads_frames_between_anchors() {
        // Anchors at frames 0 and 4, ten seconds apart; the middle frames fall
        // evenly between, and the timeline starts at zero.
        let times = interpolate(vec![(0, 100.0), (4, 110.0)], 5);
        assert_eq!(times, vec![0.0, 2.5, 5.0, 7.5, 10.0]);
    }

    #[test]
    fn interpolation_clamps_outside_anchors_and_forces_monotonic() {
        // One interior anchor: frames before it hold its time, frames after hold
        // the last; a backwards anchor time is clamped up, not allowed to dip.
        let times = interpolate(vec![(1, 10.0), (2, 5.0)], 4);
        assert!(times.windows(2).all(|w| w[0] <= w[1]), "{times:?}");
        assert_eq!(times[0], 0.0);
    }

    #[test]
    fn frame_at_finds_the_frame_for_a_time() {
        let replay = Replay {
            bytes: Vec::new(),
            columns: 80,
            frames: vec![
                Frame { end: 0, time: 0.0 },
                Frame { end: 0, time: 2.0 },
                Frame { end: 0, time: 4.0 },
            ],
            real_time: true,
        };
        assert_eq!(replay.frame_at(-1.0), 0);
        assert_eq!(replay.frame_at(0.0), 0);
        assert_eq!(replay.frame_at(1.9), 0);
        assert_eq!(replay.frame_at(2.0), 1);
        assert_eq!(replay.frame_at(100.0), 2);
        assert_eq!(replay.duration(), 4.0);
    }

    #[test]
    fn display_at_shows_only_the_frame_prefix() {
        // Two progress events: the first writes "alpha", the second "beta". Frame 0
        // shows only the first; frame 1 shows both.
        let output = BuildOutput {
            columns: 40,
            bytes: b"alpha\nbeta\n".to_vec(),
            frames: vec![6, 11],
            anchors: Vec::new(),
            show_timestamps: false,
        };
        let replay = Replay::from_output(output);
        assert_eq!(replay.frame_count(), 2);

        let at = |frame| {
            let viewport =
                crate::Terminal::from_display(&replay.display_at(frame), 40, 10).viewport();
            crate::screen::rows_to_text(&viewport)
                .trim_end()
                .to_string()
        };
        assert_eq!(at(0), "alpha");
        assert_eq!(at(1), "alpha\nbeta");
    }

    #[test]
    fn sample_without_markers_falls_back_to_event_anchors() {
        // The committed sample was captured without --show_timestamps, so the
        // timeline is interpolated from build events, spanning a real duration.
        let output = crate::parse(SAMPLE).expect("parse sample");
        let replay = Replay::from_output(output);
        assert!(!replay.real_time());
        assert!(replay.frame_count() > 1);
        assert!(replay.duration() > 0.0, "events should span real time");
        // Frame times are non-decreasing and start at zero.
        assert_eq!(replay.time_of(0), 0.0);
        for f in 1..replay.frame_count() {
            assert!(replay.time_of(f) >= replay.time_of(f - 1));
        }
    }

    #[test]
    fn real_markers_used_only_with_the_flag() {
        // Three timestamped lines, one per frame, two seconds apart.
        let bytes = b"(00:00:01) a\n(00:00:03) b\n(00:00:05) c\n".to_vec();
        let frames = vec![13, 26, bytes.len()];
        let with = Replay::from_output(BuildOutput {
            columns: 40,
            bytes: bytes.clone(),
            frames: frames.clone(),
            anchors: Vec::new(),
            show_timestamps: true,
        });
        assert!(with.real_time());
        assert_eq!(with.duration(), 4.0);

        // The same bytes without the flag: the markers are not a timeline, so it
        // falls back (here to nothing, as there are no event anchors).
        let without = Replay::from_output(BuildOutput {
            columns: 40,
            bytes,
            frames,
            anchors: Vec::new(),
            show_timestamps: false,
        });
        assert!(!without.real_time());
        assert_eq!(without.duration(), 0.0);
    }
}
