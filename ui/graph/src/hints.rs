//! What tells apart rows whose labels read alike, wherever several points
//! are named side by side.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

/// For each row, which of its `readings` it shows beside a label that the
/// row's `key` stands for, the readings most compact first: among rows whose
/// keys are alike, the first reading that tells every one of them apart, else
/// the last; a row whose key no other shares shows its first.
pub(crate) fn hints<K: Eq + Hash>(
    keys: &[K],
    readings: Vec<Vec<Option<String>>>,
) -> Vec<Option<String>> {
    let mut alike: HashMap<&K, Vec<usize>> = HashMap::with_capacity(keys.len());
    for (row, key) in keys.iter().enumerate() {
        alike.entry(key).or_default().push(row);
    }
    let mut chosen = vec![0; keys.len()];
    for group in alike.values().filter(|group| group.len() > 1) {
        let count = readings[group[0]].len();
        let tells_apart = |reading: usize| {
            let mut seen = HashSet::with_capacity(group.len());
            group
                .iter()
                .all(|&row| seen.insert(readings[row].get(reading).and_then(Option::as_deref)))
        };
        let reading = (0..count)
            .find(|&reading| tells_apart(reading))
            .unwrap_or(count.saturating_sub(1));
        for &row in group {
            chosen[row] = reading;
        }
    }
    readings
        .into_iter()
        .zip(chosen)
        .map(|(readings, reading)| readings.into_iter().nth(reading).flatten())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn readings(rows: &[&[&str]]) -> Vec<Vec<Option<String>>> {
        rows.iter()
            .map(|row| row.iter().map(|r| Some(r.to_string())).collect())
            .collect()
    }

    /// The four writes of one target's metadata file name the same file in as
    /// many configurations, so its name tells them apart no better than their
    /// label does, and each reads its configuration.
    #[test]
    fn rows_alike_read_the_first_reading_telling_them_apart() {
        let keys = ["w", "w", "w", "w", "x"];
        let chosen = hints(
            &keys,
            readings(&[
                &["m.json", "k8-fastbuild", "p/m.json"],
                &["m.json", "k8-opt-exec-ST-1", "p/m.json"],
                &["m.json", "k8-opt-exec-ST-2", "p/m.json"],
                &["m.json", "k8-fastbuild-ST-3", "p/m.json"],
                &["x.o", "k8-fastbuild", "p/x.o"],
            ]),
        );
        let configs = ["k8-fastbuild", "k8-opt-exec-ST-1", "k8-opt-exec-ST-2"];
        assert_eq!(&chosen[..3], configs.map(|c| Some(c.to_owned())));
        assert_eq!(chosen[4].as_deref(), Some("x.o"));
    }

    #[test]
    fn rows_alike_whose_files_differ_read_the_file() {
        let chosen = hints(
            &["w", "w"],
            readings(&[&["a.o", "k8", "p/a.o"], &["b.o", "k8", "p/b.o"]]),
        );
        assert_eq!(chosen, [Some("a.o".to_owned()), Some("b.o".to_owned())]);
    }

    /// Where neither the file's name nor the configuration tells rows apart
    /// alone, the most specific reading does.
    #[test]
    fn rows_no_single_reading_parts_read_the_last() {
        let chosen = hints(
            &["w", "w", "w"],
            readings(&[
                &["m.h", "k8", "a/m.h", "k8/a/m.h"],
                &["m.h", "k8", "b/m.h", "k8/b/m.h"],
                &["m.h", "arm", "a/m.h", "arm/a/m.h"],
            ]),
        );
        assert_eq!(chosen[2].as_deref(), Some("arm/a/m.h"));
    }

    #[test]
    fn a_row_with_nothing_to_read_reads_no_hint() {
        assert_eq!(hints(&["a", "a"], vec![vec![], vec![]]), [None, None]);
        assert_eq!(hints(&["a"], vec![vec![None]]), [None]);
    }
}
