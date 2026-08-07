use bviz_trace_profile::TimeSpan;

/// The region between two instants, whichever order they are given in.
pub fn span_between(a_us: i64, b_us: i64) -> TimeSpan {
    TimeSpan {
        start_us: a_us.min(b_us),
        end_us: a_us.max(b_us),
    }
}

/// A set of profile-time regions an aggregation is restricted to. The regions
/// are sorted ascending by start, pairwise disjoint and non-touching, and each
/// has non-zero width, so an instant lies in at most one and no microsecond is
/// counted twice. Consumers read the empty set as unrestricted: the whole
/// timeline contributes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimeSelection {
    spans: Vec<TimeSpan>,
}

impl TimeSelection {
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    pub fn spans(&self) -> &[TimeSpan] {
        &self.spans
    }

    pub fn len(&self) -> usize {
        self.spans.len()
    }

    pub fn total_us(&self) -> i64 {
        self.spans.iter().map(|s| s.end_us - s.start_us).sum()
    }

    /// First start to last end, for bracketing a scan before testing regions
    /// individually.
    pub fn bounds(&self) -> Option<TimeSpan> {
        Some(TimeSpan {
            start_us: self.spans.first()?.start_us,
            end_us: self.spans.last()?.end_us,
        })
    }

    pub fn clear(&mut self) {
        self.spans.clear();
    }

    /// Restricts to `span` alone, reporting its index.
    pub fn set(&mut self, span: TimeSpan) -> Option<usize> {
        if span.start_us >= span.end_us {
            return None;
        }
        self.spans.clear();
        self.spans.push(span);
        Some(0)
    }

    /// Unions `span` into the set, reporting the index of the region it became
    /// part of.
    pub fn insert(&mut self, span: TimeSpan) -> Option<usize> {
        if span.start_us >= span.end_us {
            return None;
        }
        Some(self.merge_in(span))
    }

    /// Moves or resizes the region at `index` to `span`, reporting the index it
    /// lands at once it has absorbed whatever it now reaches.
    pub fn replace(&mut self, index: usize, span: TimeSpan) -> Option<usize> {
        if span.start_us >= span.end_us || index >= self.spans.len() {
            return None;
        }
        self.spans.remove(index);
        Some(self.merge_in(span))
    }

    pub fn region_at(&self, t_us: i64) -> Option<usize> {
        let i = self.spans.partition_point(|s| s.end_us <= t_us);
        self.spans.get(i).filter(|s| s.start_us <= t_us).map(|_| i)
    }

    /// Microseconds of `[start_us, end_us)` the selection covers. Aggregation
    /// calls this once per profile span, so it costs a binary search plus one
    /// step per region actually covered.
    pub fn overlap_us(&self, start_us: i64, end_us: i64) -> i64 {
        if start_us >= end_us {
            return 0;
        }
        let first = self.spans.partition_point(|s| s.end_us <= start_us);
        self.spans[first..]
            .iter()
            .take_while(|s| s.start_us < end_us)
            .map(|s| s.end_us.min(end_us) - s.start_us.max(start_us))
            .sum()
    }

    /// Absorbs into `span` every region it overlaps or touches and puts the
    /// result in their place, keeping the set sorted, disjoint and
    /// non-touching. `span` must have non-zero width.
    fn merge_in(&mut self, span: TimeSpan) -> usize {
        let first = self.spans.partition_point(|s| s.end_us < span.start_us);
        let last = self.spans.partition_point(|s| s.start_us <= span.end_us);
        let absorbed = &self.spans[first..last];
        let merged = TimeSpan {
            start_us: absorbed
                .first()
                .map_or(span.start_us, |s| s.start_us.min(span.start_us)),
            end_us: absorbed
                .last()
                .map_or(span.end_us, |s| s.end_us.max(span.end_us)),
        };
        self.spans.splice(first..last, [merged]);
        first
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selection(regions: &[(i64, i64)]) -> TimeSelection {
        let mut sel = TimeSelection::default();
        for &(a, b) in regions {
            assert!(sel.insert(span_between(a, b)).is_some(), "({a}, {b})");
        }
        sel
    }

    /// A span built without the ordering `span_between` imposes, so a test can
    /// hand a mutator an inverted or zero-width region.
    fn raw(start_us: i64, end_us: i64) -> TimeSpan {
        TimeSpan { start_us, end_us }
    }

    fn regions(sel: &TimeSelection) -> Vec<(i64, i64)> {
        sel.spans().iter().map(|s| (s.start_us, s.end_us)).collect()
    }

    /// Asserts every region is non-empty and ends strictly before the next one
    /// starts.
    fn assert_disjoint(sel: &TimeSelection) {
        for s in sel.spans() {
            assert!(s.start_us < s.end_us, "empty region {s:?}");
        }
        for pair in sel.spans().windows(2) {
            assert!(
                pair[0].end_us < pair[1].start_us,
                "{:?} touches or overlaps {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn span_between_orders_its_ends() {
        assert_eq!(span_between(90, 10), span_between(10, 90));
        assert_eq!(span_between(10, 90).start_us, 10);
    }

    #[test]
    fn set_restricts_to_one_region() {
        let mut sel = selection(&[(0, 10), (20, 30)]);
        assert_eq!(sel.set(span_between(100, 140)), Some(0));
        assert_eq!(regions(&sel), [(100, 140)]);
    }

    #[test]
    fn inserting_an_overlapping_region_merges_it() {
        let mut sel = selection(&[(0, 100)]);
        assert_eq!(sel.insert(span_between(50, 150)), Some(0));
        assert_eq!(regions(&sel), [(0, 150)]);
        assert_disjoint(&sel);
    }

    #[test]
    fn inserting_a_touching_region_merges_it() {
        let mut sel = selection(&[(0, 100)]);
        assert_eq!(sel.insert(span_between(100, 150)), Some(0));
        assert_eq!(regions(&sel), [(0, 150)]);
        assert_disjoint(&sel);
    }

    #[test]
    fn inserting_across_several_regions_collapses_them_into_one() {
        let mut sel = selection(&[(0, 10), (20, 30), (40, 50), (200, 300)]);
        assert_eq!(sel.insert(span_between(5, 45)), Some(0));
        assert_eq!(regions(&sel), [(0, 50), (200, 300)]);
        assert_disjoint(&sel);
    }

    #[test]
    fn insert_returns_the_index_of_the_region_it_joined() {
        let mut sel = selection(&[(0, 10), (100, 110)]);
        assert_eq!(sel.insert(span_between(50, 60)), Some(1));
        assert_eq!(sel.insert(span_between(105, 130)), Some(2));
        assert_eq!(regions(&sel), [(0, 10), (50, 60), (100, 130)]);
    }

    #[test]
    fn replace_reports_the_index_the_region_moved_to() {
        let mut sel = selection(&[(0, 10), (100, 110), (200, 210)]);
        // The first region jumps past the second.
        assert_eq!(sel.replace(0, span_between(120, 130)), Some(1));
        assert_eq!(regions(&sel), [(100, 110), (120, 130), (200, 210)]);
        assert_disjoint(&sel);
    }

    #[test]
    fn replace_growing_a_region_into_its_neighbour_merges_them() {
        let mut sel = selection(&[(0, 10), (100, 110), (200, 210)]);
        assert_eq!(sel.replace(1, span_between(100, 205)), Some(1));
        assert_eq!(regions(&sel), [(0, 10), (100, 210)]);
        assert_eq!(sel.len(), 2);
        assert_disjoint(&sel);
    }

    #[test]
    fn degenerate_and_inverted_spans_are_rejected_unchanged() {
        let mut sel = selection(&[(0, 10)]);
        let before = sel.clone();
        assert_eq!(sel.insert(span_between(50, 50)), None);
        assert_eq!(sel.set(raw(50, 50)), None);
        assert_eq!(sel.insert(raw(90, 20)), None);
        assert_eq!(sel.replace(0, raw(90, 20)), None);
        assert_eq!(sel, before);
    }

    #[test]
    fn an_out_of_range_index_leaves_the_selection_unchanged() {
        let mut sel = selection(&[(0, 10)]);
        let before = sel.clone();
        assert_eq!(sel.replace(1, span_between(20, 30)), None);
        assert_eq!(sel, before);
    }

    #[test]
    fn total_us_and_bounds_measure_the_regions() {
        assert_eq!(TimeSelection::default().bounds(), None);
        assert_eq!(TimeSelection::default().total_us(), 0);
        let sel = selection(&[(0, 10), (100, 130)]);
        assert_eq!(sel.total_us(), 40);
        assert_eq!(sel.bounds(), Some(span_between(0, 130)));
    }

    #[test]
    fn region_at_finds_the_region_holding_an_instant() {
        let sel = selection(&[(0, 10), (100, 130)]);
        assert_eq!(sel.region_at(0), Some(0));
        assert_eq!(sel.region_at(9), Some(0));
        // A region's end is exclusive, so it holds neither its own end nor the
        // gap beyond it.
        assert_eq!(sel.region_at(10), None);
        assert_eq!(sel.region_at(129), Some(1));
        assert_eq!(sel.region_at(130), None);
    }

    #[test]
    fn overlap_us_counts_a_span_inside_one_region_in_full() {
        let sel = selection(&[(0, 100), (200, 300)]);
        assert_eq!(sel.overlap_us(210, 250), 40);
    }

    #[test]
    fn overlap_us_ignores_a_span_outside_every_region() {
        let sel = selection(&[(0, 100), (200, 300)]);
        assert_eq!(sel.overlap_us(120, 180), 0);
        assert_eq!(sel.overlap_us(400, 500), 0);
        assert_eq!(TimeSelection::default().overlap_us(0, 100), 0);
    }

    #[test]
    fn overlap_us_clips_a_span_straddling_one_boundary() {
        let sel = selection(&[(0, 100), (200, 300)]);
        assert_eq!(sel.overlap_us(80, 150), 20);
        assert_eq!(sel.overlap_us(150, 220), 20);
    }

    #[test]
    fn overlap_us_clips_a_span_covering_a_whole_region() {
        let sel = selection(&[(200, 300)]);
        assert_eq!(sel.overlap_us(150, 400), 100);
    }

    #[test]
    fn overlap_us_sums_the_pieces_across_several_regions() {
        let sel = selection(&[(0, 100), (200, 300), (400, 410)]);
        assert_eq!(sel.overlap_us(50, 405), 50 + 100 + 5);
    }

    #[test]
    fn spans_stay_sorted_and_disjoint_across_a_sequence_of_inserts() {
        let mut sel = TimeSelection::default();
        let inserts = [
            (500, 600),
            (100, 200),
            (150, 250),
            (900, 1000),
            (0, 50),
            (250, 900),
            (620, 640),
            (1000, 1001),
        ];
        // Dragged right-to-left, so the ends arrive inverted.
        for (a, b) in inserts {
            assert!(sel.insert(span_between(b, a)).is_some(), "({a}, {b})");
            assert_disjoint(&sel);
        }
        assert_eq!(regions(&sel), [(0, 50), (100, 1001)]);
    }
}
