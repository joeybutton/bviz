use bviz_trace_profile::{CounterPoint, TimeSpan};

/// The smallest 1/2/5 ×10ⁿ value that is at least `min`.
pub(crate) fn nice_step(min: f64) -> f64 {
    if min <= 0.0 {
        return 1.0;
    }
    let pow = 10f64.powf(min.log10().floor());
    [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .map(|m| m * pow)
        .find(|&s| s >= min)
        .unwrap_or(pow * 10.0)
}

/// Formats a profile-relative timestamp (microseconds) in the unit `step`
/// warrants, with just enough decimals to tell adjacent ticks apart.
pub(crate) fn format_time(us: f64, step: f64) -> String {
    let (unit, scale) = if step < 1_000.0 {
        ("µs", 1.0)
    } else if step < 1_000_000.0 {
        ("ms", 1_000.0)
    } else {
        ("s", 1_000_000.0)
    };
    let step_in_unit = step / scale;
    let decimals = if step_in_unit >= 1.0 {
        0
    } else if step_in_unit >= 0.1 {
        1
    } else {
        2
    };
    // Collapse a negative zero (from rounding a tick just below the origin) to 0.
    let value = us / scale;
    let value = if value == 0.0 { 0.0 } else { value };
    format!("{:.*}{}", decimals, value, unit)
}

/// A compact duration label (e.g. `4.20ms`) for a microsecond span.
pub(crate) fn format_duration(us: i64) -> String {
    let a = (us as f64).abs();
    if a < 1_000.0 {
        format!("{us}µs")
    } else if a < 1_000_000.0 {
        format!("{:.2}ms", us as f64 / 1_000.0)
    } else {
        format!("{:.2}s", us as f64 / 1_000_000.0)
    }
}

/// The y of the segment `(x0, y0)`–`(x1, y1)` at `x`; `x0` must differ from `x1`.
pub(crate) fn lerp_y(x0: f32, y0: f32, x1: f32, y1: f32, x: f32) -> f32 {
    y0 + (y1 - y0) * (x - x0) / (x1 - x0)
}

/// Clips a polyline whose x is non-decreasing to the vertical strip `[lo, hi]`,
/// interpolating y where a segment crosses an edge so the on-screen shape is
/// exact. At high zoom a series' off-screen vertices otherwise carry pixel x far
/// outside the window, where the renderer loses precision and the filled area
/// bleeds past its content mask.
pub(crate) fn clip_polyline_x(verts: &[(f32, f32)], lo: f32, hi: f32) -> Vec<(f32, f32)> {
    let mut out: Vec<(f32, f32)> = Vec::new();
    let mut push = |p: (f32, f32)| {
        if out.last() != Some(&p) {
            out.push(p);
        }
    };
    for seg in verts.windows(2) {
        let (x0, y0) = seg[0];
        let (x1, y1) = seg[1];
        if x1 < lo || x0 > hi {
            continue;
        }
        if x0 >= x1 {
            if (lo..=hi).contains(&x0) {
                push((x0, y0));
                push((x1, y1));
            }
            continue;
        }
        let a = if x0 < lo {
            (lo, lerp_y(x0, y0, x1, y1, lo))
        } else {
            (x0, y0)
        };
        let b = if x1 > hi {
            (hi, lerp_y(x0, y0, x1, y1, hi))
        } else {
            (x1, y1)
        };
        push(a);
        push(b);
    }
    out
}

/// Expands a sequence of projected samples (x non-decreasing) into a staircase
/// boundary: each sample's value holds until the next sample's x, so the plot
/// asserts nothing about the unmeasured interval between two samples. The last
/// sample ends the boundary at its own x, having no successor to hold until.
pub(crate) fn step_vertices(
    samples: impl IntoIterator<Item = (f32, f32)>,
) -> impl Iterator<Item = (f32, f32)> {
    let mut held_y: Option<f32> = None;
    samples.into_iter().flat_map(move |(x, y)| {
        let tread = held_y.replace(y).map(|prev| (x, prev));
        tread.into_iter().chain(std::iter::once((x, y)))
    })
}

/// Collapses a boundary polyline (x non-decreasing) to at most two vertices per
/// whole-pixel column: a column crowded with points keeps only its extreme-y
/// pair, in encounter order, preserving the filled area's per-pixel envelope; a
/// column with at most two points passes through unchanged, so sparse runs keep
/// their exact shape at high zoom. Streams its input, so the caller need not
/// materialize the projected vertices.
pub(crate) fn downsample_columns(verts: impl IntoIterator<Item = (f32, f32)>) -> Vec<(f32, f32)> {
    struct Col {
        x: f32,
        n: usize,
        head: [(f32, f32); 2],
        lo_i: usize,
        lo: (f32, f32),
        hi_i: usize,
        hi: (f32, f32),
    }
    fn flush(c: &Col, out: &mut Vec<(f32, f32)>) {
        if c.n <= 2 {
            out.extend_from_slice(&c.head[..c.n]);
        } else if c.lo_i == c.hi_i {
            out.push(c.lo);
        } else if c.lo_i < c.hi_i {
            out.extend_from_slice(&[c.lo, c.hi]);
        } else {
            out.extend_from_slice(&[c.hi, c.lo]);
        }
    }
    let mut out: Vec<(f32, f32)> = Vec::new();
    let mut cur: Option<Col> = None;
    for v in verts {
        let x = v.0.floor();
        match &mut cur {
            Some(c) if c.x == x => {
                if c.n < 2 {
                    c.head[c.n] = v;
                }
                if v.1 < c.lo.1 {
                    (c.lo_i, c.lo) = (c.n, v);
                }
                if v.1 > c.hi.1 {
                    (c.hi_i, c.hi) = (c.n, v);
                }
                c.n += 1;
            }
            _ => {
                if let Some(c) = &cur {
                    flush(c, &mut out);
                }
                cur = Some(Col {
                    x,
                    n: 1,
                    head: [v, v],
                    lo_i: 0,
                    lo: v,
                    hi_i: 0,
                    hi: v,
                });
            }
        }
    }
    if let Some(c) = &cur {
        flush(c, &mut out);
    }
    out
}

/// The series' value in effect at time `t` (microseconds): the value of the last
/// point at or before `t`, held until the next point, as [`step_vertices`] plots
/// it. `None` when `t` lies outside the series' covered `[first.ts, last.ts]`
/// range. Points are assumed sorted by `ts_us` ascending.
pub(crate) fn sample_at(points: &[CounterPoint], t: f64) -> Option<f64> {
    let first = points.first()?;
    let last = points.last()?;
    if t < first.ts_us as f64 || t > last.ts_us as f64 {
        return None;
    }
    let after = points.partition_point(|p| (p.ts_us as f64) <= t);
    Some(points[after - 1].value)
}

/// The unit a counter lane's values carry, inferred from its name since the
/// trace format stores none. Empty for unitless counts (action counts,
/// SkyFunctions, RPCs, load averages).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CounterUnit {
    Cores,
    Megabytes,
    Megabits,
    Percent,
    Plain,
}

pub(crate) fn unit_for(lane: &str) -> CounterUnit {
    let lane = lane.to_lowercase();
    if lane.contains("cpu usage") {
        CounterUnit::Cores
    } else if lane.contains("memory") {
        CounterUnit::Megabytes
    } else if lane.contains("network") {
        CounterUnit::Megabits
    } else if lane.contains("pressure") {
        CounterUnit::Percent
    } else {
        CounterUnit::Plain
    }
}

/// A compact `"<value> <unit>"` label, rescaling memory past 1 GB.
pub(crate) fn format_counter(value: f64, unit: CounterUnit) -> String {
    match unit {
        CounterUnit::Cores => format!("{value:.2} cores"),
        CounterUnit::Megabytes => {
            if value >= 1024.0 {
                format!("{:.2} GB", value / 1024.0)
            } else {
                format!("{value:.0} MB")
            }
        }
        CounterUnit::Megabits => format!("{value:.2} Mbps"),
        CounterUnit::Percent => format!("{value:.0}%"),
        CounterUnit::Plain => format!("{value:.1}"),
    }
}

/// The pixels-per-microsecond at which `span` fits `width_px` less `margin_px` of
/// empty space at each end; the zoom-out floor, matching [`TimeView::fit`].
pub(crate) fn fit_px_per_us(span: TimeSpan, width_px: f32, margin_px: f32) -> f64 {
    let span_us = (span.end_us - span.start_us).max(1) as f64;
    (width_px - 2.0 * margin_px).max(1.0) as f64 / span_us
}

pub(crate) fn span_seconds(span: TimeSpan) -> f64 {
    (span.end_us - span.start_us) as f64 / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_px_per_us_maps_span_to_width() {
        let span = TimeSpan {
            start_us: 0,
            end_us: 1000,
        };
        assert_eq!(fit_px_per_us(span, 100.0, 0.0), 0.1);
        // A 10px margin each side leaves 80px for the span.
        assert_eq!(fit_px_per_us(span, 100.0, 10.0), 0.08);
        // An empty span clamps the divisor to 1 rather than dividing by zero.
        let empty = TimeSpan {
            start_us: 5,
            end_us: 5,
        };
        assert_eq!(fit_px_per_us(empty, 100.0, 0.0), 100.0);
    }

    #[test]
    fn nice_step_rounds_to_1_2_5() {
        assert_eq!(nice_step(1.0), 1.0);
        assert_eq!(nice_step(3.0), 5.0);
        assert_eq!(nice_step(7.0), 10.0);
        assert_eq!(nice_step(150.0), 200.0);
    }

    #[test]
    fn format_time_picks_unit_and_decimals() {
        assert_eq!(format_time(500.0, 100.0), "500µs");
        assert_eq!(format_time(200_000.0, 100_000.0), "200ms");
        assert_eq!(format_time(5_000_000.0, 5_000_000.0), "5s");
        assert_eq!(format_time(-2_000_000.0, 1_000_000.0), "-2s");
        assert_eq!(format_time(-0.0, 5_000_000.0), "0s");
    }

    #[test]
    fn format_duration_is_compact() {
        assert_eq!(format_duration(500), "500µs");
        assert_eq!(format_duration(4_200), "4.20ms");
        assert_eq!(format_duration(2_500_000), "2.50s");
    }

    #[test]
    fn sample_at_holds_each_value_until_the_next_point() {
        let points = vec![
            CounterPoint {
                ts_us: 100,
                value: 10.0,
            },
            CounterPoint {
                ts_us: 200,
                value: 30.0,
            },
        ];
        // Exact hit on a point.
        assert_eq!(sample_at(&points, 100.0), Some(10.0));
        assert_eq!(sample_at(&points, 200.0), Some(30.0));
        // Between points the earlier value still stands, right up to the next.
        assert_eq!(sample_at(&points, 150.0), Some(10.0));
        assert_eq!(sample_at(&points, 199.9), Some(10.0));
        // Out of range on either side yields no value.
        assert_eq!(sample_at(&points, 50.0), None);
        assert_eq!(sample_at(&points, 250.0), None);
        assert_eq!(sample_at(&[], 100.0), None);
        // Of coincident timestamps the last is the one in effect.
        let dup = vec![
            CounterPoint {
                ts_us: 100,
                value: 10.0,
            },
            CounterPoint {
                ts_us: 100,
                value: 30.0,
            },
        ];
        assert_eq!(sample_at(&dup, 100.0), Some(30.0));
    }

    #[test]
    fn step_vertices_holds_each_value_to_the_next_sample() {
        let samples = vec![(0.0, 5.0), (10.0, 2.0), (20.0, 7.0)];
        assert_eq!(
            step_vertices(samples).collect::<Vec<_>>(),
            vec![
                (0.0, 5.0),
                (10.0, 5.0),
                (10.0, 2.0),
                (20.0, 2.0),
                (20.0, 7.0),
            ]
        );
        // A lone sample has no interval to span, so it stands alone.
        assert_eq!(
            step_vertices([(3.0, 1.0)]).collect::<Vec<_>>(),
            vec![(3.0, 1.0)]
        );
        assert_eq!(step_vertices([]).collect::<Vec<_>>(), vec![]);
    }

    #[test]
    fn downsample_columns_keeps_sparse_runs_and_column_extremes() {
        // At most two points per whole-pixel column: passthrough.
        let sparse = vec![(0.2, 5.0), (0.8, 7.0), (3.0, 1.0), (10.5, 4.0)];
        assert_eq!(downsample_columns(sparse.iter().copied()), sparse);

        // A dense oscillation collapses to each column's extremes, in encounter
        // order, with x monotone and the global extremes preserved.
        let dense: Vec<(f32, f32)> = (0..1000)
            .map(|i| {
                let x = i as f32 / 100.0; // 100 points per column
                (x, (i as f32 * 0.71).sin() * 50.0)
            })
            .collect();
        let out = downsample_columns(dense.iter().copied());
        let columns = 10.0_f32;
        assert!(out.len() as f32 <= 2.0 * columns);
        let max = |v: &[(f32, f32)]| v.iter().map(|p| p.1).fold(f32::MIN, f32::max);
        let min = |v: &[(f32, f32)]| v.iter().map(|p| p.1).fold(f32::MAX, f32::min);
        assert_eq!(max(&out), max(&dense));
        assert_eq!(min(&out), min(&dense));
        assert!(out.windows(2).all(|w| w[0].0 <= w[1].0));
        // Per column both extremes survive.
        for col in 0..10 {
            let in_col = |v: &[(f32, f32)]| -> Vec<(f32, f32)> {
                v.iter().filter(|p| p.0.floor() == col as f32).copied().collect()
            };
            let (all, kept) = (in_col(&dense), in_col(&out));
            assert_eq!(max(&kept), max(&all));
            assert_eq!(min(&kept), min(&all));
        }

        // A flat dense column (min == max) collapses to a single vertex.
        let flat = (0..10).map(|i| (i as f32 / 100.0, 3.0));
        assert_eq!(downsample_columns(flat), vec![(0.0, 3.0)]);
    }

    #[test]
    fn unit_for_infers_from_lane_name() {
        assert_eq!(unit_for("CPU usage (Bazel)"), CounterUnit::Cores);
        assert_eq!(unit_for("System memory usage"), CounterUnit::Megabytes);
        assert_eq!(unit_for("Network Up (system)"), CounterUnit::Megabits);
        // "memory" wins over "pressure" by the inference order, so a pressure
        // lane reads as "%" only when it does not also mention memory.
        assert_eq!(unit_for("CPU pressure"), CounterUnit::Percent);
        assert_eq!(unit_for("Action count"), CounterUnit::Plain);
        assert_eq!(unit_for("System load average"), CounterUnit::Plain);
    }

    #[test]
    fn format_counter_uses_unit_and_thresholds() {
        assert_eq!(format_counter(3.5, CounterUnit::Cores), "3.50 cores");
        assert_eq!(format_counter(512.0, CounterUnit::Megabytes), "512 MB");
        // At and past 1024 MB, rescale to GB.
        assert_eq!(format_counter(1024.0, CounterUnit::Megabytes), "1.00 GB");
        assert_eq!(format_counter(2048.0, CounterUnit::Megabytes), "2.00 GB");
        assert_eq!(format_counter(12.5, CounterUnit::Megabits), "12.50 Mbps");
        assert_eq!(format_counter(80.0, CounterUnit::Percent), "80%");
        assert_eq!(format_counter(42.0, CounterUnit::Plain), "42.0");
    }
}
