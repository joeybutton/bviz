//! The order the jump palette lists the nodes a query matches in.

use super::index::Candidates;
use bviz_graph_layout::Id;
use nucleo::pattern::Pattern;
use nucleo::{Matcher, Utf32Str};
use std::cmp::Reverse;

/// How many of the matcher's best-scored matches are ranked and listed. A
/// match the matcher scores lower but that a hit on the node's own name lifts
/// is found only within this window, which is wide enough to hold the near
/// ties of a short query.
const WINDOW: usize = 256;

/// The most matches ranked and listed where the score at the end of the
/// [`WINDOW`] is shared past it. The matcher breaks a tie on its score by the
/// text's length and its order, which the ranking's own tie-breaks override,
/// so every match sharing that score is ranked, up to this many.
const MAX_WINDOW: usize = 4096;

/// Where in a candidate's text a query's matched characters fall.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Hit {
    /// At least one falls in the candidate's path.
    Path,
    /// Every one falls within the node's own name.
    Name,
}

/// One row the palette lists.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Row {
    /// The candidate's index among the document's [`Candidates`].
    pub(crate) candidate: u32,
    /// The char indices of the candidate's text the query matched, ascending.
    pub(crate) matched: Vec<u32>,
}

/// The rows for the matches in `found`, best scored first, each candidate
/// given by its index and the text it was matched by, in the order the
/// palette lists them: a hit on the node's own name before one reaching
/// into its path, then by the matcher's score, then the nodes `drawn` holds,
/// then the most depended on. Only the first of `found` are ranked: the
/// [`WINDOW`] and every match tying its last score, up to [`MAX_WINDOW`].
pub(crate) fn rank<'a>(
    found: impl Iterator<Item = (u32, Utf32Str<'a>)>,
    pattern: &Pattern,
    matcher: &mut Matcher,
    candidates: &Candidates,
    drawn: impl Fn(Id) -> bool,
) -> Vec<Row> {
    let mut ranked: Vec<((Hit, u32, bool, u32), Row)> = Vec::with_capacity(WINDOW);
    let mut last = None;
    for (index, text) in found.take(MAX_WINDOW) {
        let mut matched: Vec<u32> = Vec::with_capacity(pattern.atoms.len());
        let Some(score) = pattern.indices(text, matcher, &mut matched) else {
            continue;
        };
        if ranked.len() >= WINDOW && last != Some(score) {
            break;
        }
        last = Some(score);
        matched.sort_unstable();
        matched.dedup();
        let candidate = candidates.get(index);
        let hit = match matched.iter().any(|at| candidate.path.contains(at)) {
            true => Hit::Path,
            false => Hit::Name,
        };
        let key = (hit, score, drawn(candidate.id), candidate.rdeps);
        ranked.push((
            key,
            Row {
                candidate: index,
                matched,
            },
        ));
    }
    ranked.sort_by_key(|(key, _)| Reverse(*key));
    ranked.into_iter().map(|(_, row)| row).collect()
}

#[cfg(test)]
mod tests {
    use super::super::index::{Candidate, Candidates, Scope};
    use super::*;
    use bviz_graph_layout::Kind;
    use nucleo::pattern::{CaseMatching, Normalization};
    use nucleo::{Config, Utf32String};

    fn candidate(id: u32, text: &str, name_start: u32, rdeps: u32) -> Candidate {
        Candidate {
            id: Id(id),
            kind: Kind::Target,
            text: text.into(),
            path: 0..name_start,
            rdeps,
        }
    }

    /// The texts `rank` lists for `query` over `candidates`, of which those
    /// with the ids `drawn` names are drawn.
    fn ranked(candidates: Vec<Candidate>, query: &str, drawn: &[u32]) -> Vec<String> {
        let candidates = Candidates::from(candidates);
        let texts: Vec<Utf32String> = candidates
            .in_scope(Scope::All)
            .map(|index| Utf32String::from(&*candidates.get(index).text))
            .collect();
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut config = Config::DEFAULT;
        config.set_match_paths();
        let mut matcher = Matcher::new(config);
        let found = (0u32..).zip(texts.iter().map(|text| text.slice(..)));
        rank(found, &pattern, &mut matcher, &candidates, |id| {
            drawn.contains(&id.0)
        })
        .into_iter()
        .map(|row| candidates.get(row.candidate).text.to_string())
        .collect()
    }

    #[test]
    fn a_hit_in_the_name_beats_one_in_the_package_path() {
        let listed = ranked(
            vec![
                candidate(0, "//net/http:client", 10, 0),
                candidate(1, "//util:http", 7, 0),
            ],
            "http",
            &[],
        );
        assert_eq!(listed, ["//util:http", "//net/http:client"]);
    }

    #[test]
    fn among_equal_hits_a_drawn_node_comes_first() {
        let listed = ranked(
            vec![candidate(0, "//a:lib", 4, 0), candidate(1, "//b:lib", 4, 0)],
            "lib",
            &[1],
        );
        assert_eq!(listed, ["//b:lib", "//a:lib"]);
    }

    #[test]
    fn among_equal_drawn_hits_the_most_depended_on_comes_first() {
        let listed = ranked(
            vec![
                candidate(0, "//a:lib", 4, 2),
                candidate(1, "//b:lib", 4, 9),
                candidate(2, "//c:lib", 4, 9),
            ],
            "lib",
            &[0, 1],
        );
        assert_eq!(listed, ["//b:lib", "//a:lib", "//c:lib"]);
    }

    /// An action's mnemonic is its name, so a hit on it beats one reaching
    /// into another action's output directories.
    #[test]
    fn a_hit_on_an_actions_mnemonic_is_a_hit_on_its_name() {
        let action = |id: u32, text: &str, path: std::ops::Range<u32>| Candidate {
            id: Id(id),
            kind: Kind::Action,
            text: text.into(),
            path,
            rdeps: 0,
        };
        let listed = ranked(
            vec![
                action(0, "Genrule bazel-out/k8/bin/javac/out.jar", 8..31),
                action(1, "Javac bazel-out/k8/bin/app/lib.jar", 6..27),
            ],
            "javac",
            &[],
        );
        assert_eq!(
            listed,
            [
                "Javac bazel-out/k8/bin/app/lib.jar",
                "Genrule bazel-out/k8/bin/javac/out.jar"
            ]
        );
    }

    /// Matches tying the score at the end of the window are ranked too, so a
    /// drawn one the matcher lists past it still comes first.
    #[test]
    fn the_matches_tying_the_last_in_the_window_are_ranked_too() {
        let tied = |count: usize| -> Vec<Candidate> {
            (0..count as u32)
                .map(|id| candidate(id, &format!("//p{id:04}:lib"), 8, 0))
                .collect()
        };
        let past = WINDOW as u32 + 5;
        let listed = ranked(tied(WINDOW + 10), "lib", &[past]);
        assert_eq!(listed[0], format!("//p{past:04}:lib"));

        let beyond = MAX_WINDOW as u32 + 5;
        let listed = ranked(tied(MAX_WINDOW + 10), "lib", &[beyond]);
        assert_ne!(listed[0], format!("//p{beyond:04}:lib"));
    }

    #[test]
    fn every_match_is_listed_and_a_text_the_query_misses_is_not() {
        const MATCHING: u32 = 20;
        let mut candidates: Vec<Candidate> = (0..MATCHING)
            .map(|id| candidate(id, &format!("//p{id}:lib"), 0, 0))
            .collect();
        candidates.push(candidate(99, "//x:other", 4, 0));
        let listed = ranked(candidates, "lib", &[]);
        assert_eq!(listed.len(), MATCHING as usize);
        assert!(!listed.contains(&"//x:other".to_owned()));
    }
}
