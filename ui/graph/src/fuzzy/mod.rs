//! Fuzzy matching of a query against candidates a list offers, in time slices
//! on the thread that polls it, and the rows it ranks drawn with the matched
//! characters picked out.

mod rank;

use bviz_label::shown::Shown;
use gpui::{
    Context, HighlightStyle, Hsla, Pixels, Rems, SharedString, StyledText, Task, UnderlineStyle,
    px, rems,
};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str, Utf32String};
pub(crate) use rank::{Row, Standing};
use std::cell::RefCell;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use web_time::Instant;

/// How often a running match is polled for what it has found, often enough
/// for the rows to keep pace with typing.
const POLL: Duration = Duration::from_millis(16);

/// How long one poll spends matching: a small share of a frame, so a long
/// list is matched over several frames rather than holding the UI thread.
const MATCH_SLICE: Duration = Duration::from_millis(4);

/// Candidates scored between checks of the time a poll has spent.
const SCORED_PER_CHECK: usize = 256;

/// The weight of the line under a row's matched characters.
const MATCH_UNDERLINE: Pixels = px(1.);

/// The size a list of matches is set in, which its rows are measured at.
pub(crate) const TEXT_SIZE: Rems = rems(0.875);

/// The matcher's settings: every candidate's text reads as a path.
fn config() -> Config {
    let mut config = Config::DEFAULT;
    config.set_match_paths();
    config
}

/// Reaches the matching `view` holds, while it holds one.
pub(crate) type Locate<V> = fn(&mut V) -> Option<&mut Matching>;

/// Takes up a result of a matching that changed since the last poll.
pub(crate) type TakeUp<V> = fn(&mut V, &mut Context<V>);

/// The candidates handed to a matching so far, each with the index it was
/// handed with, in the order they came.
type Candidates = Arc<Mutex<Vec<(u32, Utf32String)>>>;

/// A matcher over candidates handed to it by index, with the rows its latest
/// result ranks.
///
/// It scores candidates on the thread that polls it, a time slice per poll,
/// with no thread of its own, so it matches the same on a desktop as in a
/// browser. A query extending the last one rescores only what that matched,
/// as the narrowing it is.
pub(crate) struct Matching {
    candidates: Candidates,
    /// Scores candidates for the query.
    scorer: Matcher,
    /// The query's pattern the candidates are scored by.
    scoring: Pattern,
    /// What the query matched among the first `covered` candidates: each
    /// match's score and where its candidate stands among them.
    found: Vec<(u32, u32)>,
    covered: usize,
    /// The matches of the query this one narrows, still to be rescored.
    narrowing: Vec<(u32, u32)>,
    /// Whether `found` changed since the last poll.
    changed: bool,
    /// Finds the characters each row's query matched, for ranking the rows
    /// and highlighting them, on a row's text as written and as shown.
    matcher: RefCell<Matcher>,
    /// The pattern `rows` were ranked by.
    pattern: Pattern,
    /// The query last handed to the matcher, which an edit extending it
    /// narrows rather than matches afresh.
    query: SharedString,
    /// The query `rows` were ranked for, which trails `query` until the
    /// matcher is next polled.
    ranked: SharedString,
    rows: Vec<Row>,
    /// How many candidates the query matches, of which `rows` lists the
    /// first.
    matched: u32,
    /// Whether candidates are still being handed to the matcher.
    streaming: bool,
    _injecting: Injecting,
    _polling: Task<()>,
}

impl Default for Matching {
    fn default() -> Self {
        Self::new()
    }
}

impl Matching {
    /// A matching with no query and no candidates.
    pub(crate) fn new() -> Self {
        Matching {
            candidates: Candidates::default(),
            scorer: Matcher::new(config()),
            scoring: Pattern::default(),
            found: Vec::new(),
            covered: 0,
            narrowing: Vec::new(),
            changed: false,
            matcher: RefCell::new(Matcher::new(config())),
            pattern: Pattern::default(),
            query: SharedString::default(),
            ranked: SharedString::default(),
            rows: Vec::new(),
            matched: 0,
            streaming: false,
            _injecting: Injecting::none(),
            _polling: Task::ready(()),
        }
    }

    pub(crate) fn query(&self) -> &SharedString {
        &self.query
    }

    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub(crate) fn matched(&self) -> u32 {
        self.matched
    }

    pub(crate) fn streaming(&self) -> bool {
        self.streaming
    }

    /// Whether `rows` were ranked for the query last handed to the matcher.
    pub(crate) fn current(&self) -> bool {
        self.ranked == self.query
    }

    /// Hands the matcher `query`; reports whether it differs from the last.
    pub(crate) fn set_query(&mut self, query: SharedString) -> bool {
        if query == self.query {
            return false;
        }
        let narrowing = !self.query.is_empty() && query.starts_with(self.query.as_str());
        self.scoring
            .reparse(&query, CaseMatching::Smart, Normalization::Smart);
        if narrowing {
            // Only what the shorter query matched can match the longer one;
            // the candidates past those it covered are scored as they come.
            let mut previous = std::mem::take(&mut self.found);
            previous.append(&mut self.narrowing);
            self.narrowing = previous;
        } else {
            self.restart();
        }
        self.changed = true;
        self.query = query;
        true
    }

    /// Scores every candidate afresh.
    fn restart(&mut self) {
        self.found.clear();
        self.narrowing.clear();
        self.covered = 0;
    }

    /// Scores candidates for one time slice, and reports whether the result
    /// changed since the last poll.
    fn tick(&mut self) -> bool {
        let candidates = Arc::clone(&self.candidates);
        let candidates = candidates.lock().unwrap_or_else(PoisonError::into_inner);
        if !self.query.is_empty() {
            let started = Instant::now();
            while self.has_work(candidates.len()) && started.elapsed() < MATCH_SLICE {
                for _ in 0..SCORED_PER_CHECK {
                    let at = match self.narrowing.pop() {
                        Some((_, at)) => at,
                        None if self.covered < candidates.len() => {
                            self.covered += 1;
                            (self.covered - 1) as u32
                        }
                        None => break,
                    };
                    let text = candidates[at as usize].1.slice(..);
                    if let Some(score) = self.scoring.score(text, &mut self.scorer) {
                        self.found.push((score, at));
                    }
                    self.changed = true;
                }
            }
        } else if self.covered != candidates.len() {
            // An empty query matches every candidate, and ranks none.
            self.covered = candidates.len();
            self.changed = true;
        }
        std::mem::take(&mut self.changed)
    }

    fn has_work(&self, candidates: usize) -> bool {
        !self.narrowing.is_empty() || self.covered < candidates
    }

    /// Whether the matcher runs or is still handed candidates.
    fn at_work(&self) -> bool {
        self.streaming
            || self.has_work(
                self.candidates
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .len(),
            )
    }

    /// Lists the rows the matcher's latest result ranks first, as `standing`
    /// tells of each candidate's index; it is asked only of an index handed
    /// to the matcher.
    pub(crate) fn rerank(&mut self, standing: impl Fn(u32) -> Standing) {
        self.ranked = self.query.clone();
        let candidates = Arc::clone(&self.candidates);
        let candidates = candidates.lock().unwrap_or_else(PoisonError::into_inner);
        if self.query.is_empty() {
            self.matched = candidates.len() as u32;
            self.rows.clear();
            return;
        }
        self.matched = self.found.len() as u32;
        if candidates.is_empty() {
            self.rows.clear();
            return;
        }
        // The best score first, a tie in the order the candidates came.
        self.found
            .sort_unstable_by_key(|&(score, at)| (std::cmp::Reverse(score), at));
        let found = self.found.iter().map(|&(_, at)| {
            let (index, text) = &candidates[at as usize];
            (*index, text.slice(..))
        });
        self.pattern = self.scoring.clone();
        self.rows = rank::rank(
            found,
            &self.query,
            &self.pattern,
            self.matcher.get_mut(),
            standing,
        );
    }
}

/// Hands candidates to a matcher off the UI thread.
pub(crate) struct Feeder {
    candidates: Candidates,
    stopped: Arc<AtomicBool>,
}

impl Feeder {
    /// Hands the matcher the candidate `index` reading `text`; reports whether
    /// the matcher still takes candidates from this feeder.
    pub(crate) fn push(&self, index: u32, text: &str) -> bool {
        if self.stopped.load(Ordering::Relaxed) {
            return false;
        }
        self.candidates
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((index, text.into()));
        true
    }
}

/// Candidates being handed to the matcher off the UI thread, stopped once
/// this is dropped: a restarted matcher reads nothing handed to it through a
/// feeder taken before.
struct Injecting {
    stopped: Arc<AtomicBool>,
    _task: Task<()>,
}

impl Injecting {
    fn none() -> Self {
        Injecting {
            stopped: Arc::default(),
            _task: Task::ready(()),
        }
    }
}

impl Drop for Injecting {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
    }
}

/// Hands the matching `locate` reaches the candidates `feed` pushes, in place
/// of any it held, off the UI thread, polling it through `take_up`.
pub(crate) fn inject<V: 'static>(
    view: &mut V,
    locate: Locate<V>,
    take_up: TakeUp<V>,
    feed: impl FnOnce(&Feeder) + Send + 'static,
    cx: &mut Context<V>,
) {
    let Some(matching) = locate(view) else {
        return;
    };
    matching.candidates = Candidates::default();
    matching.restart();
    matching.rows.clear();
    matching.matched = 0;
    matching.streaming = true;
    let feeder = Feeder {
        candidates: Arc::clone(&matching.candidates),
        stopped: Arc::default(),
    };
    let stopped = Arc::clone(&feeder.stopped);
    let task = cx.spawn(async move |view, cx| {
        cx.background_executor()
            .spawn(async move { feed(&feeder) })
            .await;
        let _ = view.update(cx, |view, cx| {
            if let Some(matching) = locate(view) {
                matching.streaming = false;
            }
            poll(view, locate, take_up, cx);
        });
    });
    matching._injecting = Injecting {
        stopped,
        _task: task,
    };
    poll(view, locate, take_up, cx);
}

/// Polls the matching `locate` reaches every [`POLL`] through `take_up`, for
/// as long as it runs or is handed candidates.
pub(crate) fn poll<V: 'static>(
    view: &mut V,
    locate: Locate<V>,
    take_up: TakeUp<V>,
    cx: &mut Context<V>,
) {
    let Some(matching) = locate(view) else {
        return;
    };
    matching._polling = cx.spawn(async move |view, cx| {
        loop {
            let running = view.update(cx, |view, cx| {
                let Some(matching) = locate(view) else {
                    return false;
                };
                if matching.tick() || !matching.current() {
                    take_up(view, cx);
                }
                locate(view).is_some_and(|matching| matching.at_work())
            });
            if !running.unwrap_or(false) {
                break;
            }
            cx.background_executor().timer(POLL).await;
        }
    });
}

/// The rows for `query` over the few `candidates`, each given by its index
/// and its text, ranked as a [`Matching`] ranks, on the calling thread: for a
/// list short enough that matching it costs less than handing it off.
pub(crate) fn rank_few<'a>(
    candidates: impl Iterator<Item = (u32, &'a str)>,
    query: &str,
    standing: impl Fn(u32) -> Standing,
) -> Vec<Row> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let texts: Vec<(u32, Utf32String)> = candidates
        .map(|(index, text)| (index, Utf32String::from(text)))
        .collect();
    let mut matcher = Matcher::new(config());
    let mut found: Vec<(u32, u32, Utf32Str<'_>)> = Vec::with_capacity(texts.len());
    found.extend(texts.iter().filter_map(|(index, text)| {
        let score = pattern.score(text.slice(..), &mut matcher)?;
        Some((score, *index, text.slice(..)))
    }));
    found.sort_by_key(|&(score, index, _)| (std::cmp::Reverse(score), index));
    let found = found.into_iter().map(|(_, index, text)| (index, text));
    rank::rank(found, query, &pattern, &mut matcher, standing)
}

impl Matching {
    /// The chars of `shown`, a row's text as it is shown, that pick out what
    /// the query matched of `row`'s text as written, ascending: where each
    /// matched char stands in `shown` while every one of them shows there, and
    /// otherwise the chars of `shown` the query matches, where it matches.
    pub(crate) fn picked(&self, shown: &Shown, row: &Row) -> Vec<u32> {
        picked(
            shown,
            &row.matched,
            &self.pattern,
            &mut self.matcher.borrow_mut(),
        )
    }
}

/// [`Matching::picked`] for `matched`, the chars of the text as written that
/// `pattern` matched, as `matcher` matches.
fn picked(shown: &Shown, matched: &[u32], pattern: &Pattern, matcher: &mut Matcher) -> Vec<u32> {
    if matched.iter().all(|&at| shown.at(at).is_some()) {
        return shown_matches(shown, matched);
    }
    let mut chars = Vec::new();
    let text = Utf32Str::new(shown.text(), &mut chars);
    let mut rematched: Vec<u32> = Vec::with_capacity(matched.len());
    match pattern.indices(text, matcher, &mut rematched) {
        Some(_) => {
            rematched.sort_unstable();
            rematched.dedup();
            rematched
        }
        None => shown_matches(shown, matched),
    }
}

/// The chars of `shown` standing for the chars `matched` of the text as it
/// is written, ascending. A shorthand shows a name's chars where its place
/// reads them, ahead of the chars of the place matched before them.
fn shown_matches(shown: &Shown, matched: &[u32]) -> Vec<u32> {
    let mut shown_matched: Vec<u32> = matched.iter().filter_map(|&at| shown.at(at)).collect();
    shown_matched.sort_unstable();
    shown_matched.dedup();
    shown_matched
}

/// `text` with its `path` muted and the `matched` chars picked out over it.
pub(crate) fn highlighted(
    text: SharedString,
    path: &Range<u32>,
    matched: &[u32],
    muted: Hsla,
    picked: Hsla,
) -> StyledText {
    let highlights = highlights(&text, path, matched, muted, picked);
    StyledText::new(text).with_highlights(highlights)
}

/// The byte ranges of `text` that [`highlighted`] draws apart, each with how.
pub(crate) fn highlights(
    text: &str,
    path: &Range<u32>,
    matched: &[u32],
    muted: Hsla,
    picked: Hsla,
) -> Vec<(Range<usize>, HighlightStyle)> {
    styled_runs(text, path, matched)
        .into_iter()
        .map(|(range, style)| {
            let highlight = match style {
                Style::Path => HighlightStyle {
                    color: Some(muted),
                    ..HighlightStyle::default()
                },
                Style::Matched => matched_style(picked),
            };
            (range, highlight)
        })
        .collect()
}

/// How the chars a query matched are picked out, in `picked`: by colour and
/// an underline but in the row's own weight, since a row is cut to fit as
/// measured in that weight, so a wider one would run past the cut and be
/// clipped unmarked.
pub(crate) fn matched_style(picked: Hsla) -> HighlightStyle {
    HighlightStyle {
        color: Some(picked),
        underline: Some(UnderlineStyle {
            thickness: MATCH_UNDERLINE,
            color: Some(picked),
            wavy: false,
        }),
        ..HighlightStyle::default()
    }
}

/// How a run of a row's text is drawn apart from the candidate's name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Style {
    Path,
    Matched,
}

/// The byte ranges of `text` drawn apart from the name: the chars in `path` as
/// path and the `matched` chars as matched, a matched char in the path drawn
/// matched.
fn styled_runs(text: &str, path: &Range<u32>, matched: &[u32]) -> Vec<(Range<usize>, Style)> {
    let mut runs: Vec<(Range<usize>, Style)> = Vec::with_capacity(matched.len() + 1);
    for (index, (at, c)) in (0u32..).zip(text.char_indices()) {
        let style = if matched.binary_search(&index).is_ok() {
            Some(Style::Matched)
        } else if path.contains(&index) {
            Some(Style::Path)
        } else {
            None
        };
        let Some(style) = style else {
            continue;
        };
        let end = at + c.len_utf8();
        match runs.last_mut() {
            Some((range, last)) if *last == style && range.end == at => range.end = end,
            _ => runs.push((at..end, style)),
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_label::names::RepoNames;

    fn unweighted(_: u32) -> Standing {
        Standing {
            path: 0..0,
            drawn: false,
            weight: 0,
        }
    }

    /// A matching over `texts` handed over in order.
    fn matching_over(texts: &[String]) -> Matching {
        let matching = Matching::new();
        let feeder = Feeder {
            candidates: Arc::clone(&matching.candidates),
            stopped: Arc::default(),
        };
        for (index, text) in (0u32..).zip(texts) {
            assert!(feeder.push(index, text));
        }
        matching
    }

    /// Polls `matching` until it has scored every candidate, and ranks them;
    /// reports how many polls that took.
    fn settle(matching: &mut Matching) -> usize {
        let mut polls = 0;
        while matching.has_work(matching.candidates.lock().unwrap().len()) || polls == 0 {
            matching.tick();
            polls += 1;
        }
        matching.rerank(unweighted);
        polls
    }

    fn candidates(rows: &[Row]) -> Vec<u32> {
        rows.iter().map(|row| row.candidate).collect()
    }

    /// Rows found by narrowing a query are those of matching the narrower
    /// query afresh, in the same order.
    #[test]
    fn narrowing_finds_what_matching_afresh_finds() {
        let texts: Vec<String> = (0..5_000)
            .map(|i| format!("//pkg{}/lib{}:target_{i}", i % 97, i % 13))
            .collect();
        let mut narrowed = matching_over(&texts);
        for query in ["t", "ta", "tar", "targ", "target_1"] {
            narrowed.set_query(query.into());
            settle(&mut narrowed);
        }
        let mut fresh = matching_over(&texts);
        fresh.set_query("target_1".into());
        settle(&mut fresh);
        assert!(fresh.matched() > 0);
        assert_eq!(narrowed.matched(), fresh.matched());
        assert_eq!(candidates(narrowed.rows()), candidates(fresh.rows()));
        assert_eq!(candidates(&fresh.rows()[..1]), [1]);
    }

    /// An empty query matches every candidate and ranks none; widening the
    /// query matches afresh.
    #[test]
    fn an_empty_query_matches_all_and_widening_rescores() {
        let texts: Vec<String> = ["//a:alpha", "//b:beta", "//c:gamma"]
            .map(String::from)
            .into();
        let mut matching = matching_over(&texts);
        settle(&mut matching);
        assert_eq!((matching.matched(), matching.rows().len()), (3, 0));
        matching.set_query("beta".into());
        settle(&mut matching);
        assert_eq!(candidates(matching.rows()), [1]);
        matching.set_query("a".into());
        settle(&mut matching);
        assert_eq!(matching.matched(), 3);
    }

    /// A long list is matched over several polls, each a time slice.
    #[test]
    fn a_long_list_is_matched_a_slice_per_poll() {
        let texts: Vec<String> = (0..400_000)
            .map(|i| format!("//very/long/package/path/number/{i}:a_target_named_{i}"))
            .collect();
        let mut matching = matching_over(&texts);
        matching.set_query("named_399999".into());
        let polls = settle(&mut matching);
        assert!(polls > 1, "{polls} polls");
        assert_eq!(candidates(&matching.rows()[..1]), [399_999]);
    }

    /// The chars of `written` as `names` shows it that [`picked`] picks out
    /// for `query`, matched against `written` as a list matches it.
    fn picked_for(names: &RepoNames, written: &str, query: &str) -> Vec<u32> {
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut matcher = Matcher::new(config());
        let mut chars = Vec::new();
        let mut matched = Vec::new();
        let text = Utf32Str::new(written, &mut chars);
        pattern.indices(text, &mut matcher, &mut matched).unwrap();
        matched.sort_unstable();
        matched.dedup();
        picked(&names.show(written), &matched, &pattern, &mut matcher)
    }

    /// A query matching the text as written in the part of a repository's
    /// name its shown name leaves out picks out what it matches of the text
    /// shown, its `@` the `@` shown.
    #[test]
    fn a_match_in_a_part_left_out_is_found_in_the_text_shown() {
        let written = "@@llvm++glibc+glibc//:glibc_crt1.object";
        let names = RepoNames::of_labels([written].into_iter());
        assert_eq!(names.show(written).text(), "@glibc//:glibc_crt1.object");
        assert_eq!(picked_for(&names, written, "@glibc"), [0, 1, 2, 3, 4, 5]);
        let picked = picked_for(&names, written, "glibc");
        assert_eq!(picked.len(), "glibc".len());
        assert!(picked.iter().all(|&at| at > 0));
    }

    /// Where the query matches nothing of the text shown, as one naming the
    /// part a shortening leaves out does, the chars that show of those it
    /// matched are picked out, and none in their stead.
    #[test]
    fn a_match_the_text_shown_lacks_keeps_what_shows_of_it() {
        let written = "@@llvm++glibc+glibc//:crt1";
        let names = RepoNames::of_labels([written].into_iter());
        assert_eq!(names.show(written).text(), "@glibc//:crt1");
        assert_eq!(picked_for(&names, written, "@@llvm"), [0]);
    }

    /// A match whose every char shows keeps where it fell, even where the
    /// text shown reads it elsewhere too.
    #[test]
    fn a_match_that_shows_is_picked_out_where_it_fell() {
        let written = "//a/lib:lib";
        let names = RepoNames::of_labels([written].into_iter());
        assert_eq!(names.show(written).text(), "//a/lib");
        assert_eq!(picked_for(&names, written, "a"), [2]);
    }

    #[test]
    fn the_path_is_muted_and_the_matched_chars_picked_out_over_it() {
        assert_eq!(
            styled_runs("//a:lib", &(0..4), &[1, 4, 5]),
            [
                (0..1, Style::Path),
                (1..2, Style::Matched),
                (2..4, Style::Path),
                (4..6, Style::Matched),
            ]
        );
    }

    #[test]
    fn a_path_inside_the_text_is_muted_alone() {
        assert_eq!(
            styled_runs("Javac out/lib.jar", &(6..10), &[0]),
            [(0..1, Style::Matched), (6..10, Style::Path)]
        );
    }

    #[test]
    fn a_run_is_measured_in_bytes_where_its_chars_are_counted() {
        assert_eq!(
            styled_runs("é:é", &(0..2), &[2]),
            [(0..3, Style::Path), (3..5, Style::Matched)]
        );
    }
}
