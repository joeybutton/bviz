//! Fuzzy matching of a query against candidates a list offers, run off the UI
//! thread, and the rows it ranks drawn with the matched characters picked out.

mod rank;

use crate::GraphPanel;
use gpui::{
    Context, HighlightStyle, Hsla, Pixels, Rems, SharedString, StyledText, Task, UnderlineStyle,
    px, rems,
};
use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config, Injector, Matcher, Nucleo};
pub(crate) use rank::{Row, Standing};
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How often a running match is polled for what it has found, often enough
/// for the rows to keep pace with typing.
const POLL: Duration = Duration::from_millis(16);

/// How long one poll waits on a match pass to finish, in milliseconds: a
/// small share of a frame, so the UI thread is not held on it.
const POLL_WAIT_MS: u64 = 1;

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
pub(crate) type Locate = fn(&mut GraphPanel) -> Option<&mut Matching>;

/// Takes up a result of a matching that changed since the last poll.
pub(crate) type TakeUp = fn(&mut GraphPanel, &mut Context<GraphPanel>);

/// A matcher over candidates handed to it by index, with the rows its latest
/// result ranks.
pub(crate) struct Matching {
    nucleo: Nucleo<u32>,
    /// Finds the characters each row's query matched, for ranking the rows
    /// and highlighting them.
    matcher: Matcher,
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
    /// Whether the last pass the matcher was polled on is still running.
    running: bool,
    /// Whether candidates are still being handed to the matcher.
    streaming: bool,
    _injecting: Injecting,
    _polling: Task<()>,
}

/// The matcher worker pool a panel keeps while no list of its holds it, so
/// that a list opened takes it up rather than starting a pool of its own.
#[derive(Default)]
pub(crate) struct Pool(Option<Nucleo<u32>>);

impl Pool {
    /// A matching with no query, on the parked pool where one is parked.
    pub(crate) fn take(&mut self) -> Matching {
        Matching::new(self.0.take())
    }

    /// Parks the pool of `matching`, which stops matching; a pool parked
    /// already stays, and the other is dropped.
    pub(crate) fn park(&mut self, matching: Matching) {
        self.0.get_or_insert(matching.nucleo);
    }
}

impl Matching {
    fn new(pool: Option<Nucleo<u32>>) -> Self {
        let mut nucleo = pool.unwrap_or_else(|| {
            let workers = Some(bviz_parallel::worker_count());
            Nucleo::new(config(), Arc::new(|| {}), workers, 1)
        });
        nucleo
            .pattern
            .reparse(0, "", CaseMatching::Smart, Normalization::Smart, false);
        Matching {
            nucleo,
            matcher: Matcher::new(config()),
            query: SharedString::default(),
            ranked: SharedString::default(),
            rows: Vec::new(),
            matched: 0,
            running: false,
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
        let narrowing = query.starts_with(self.query.as_str());
        self.nucleo.pattern.reparse(
            0,
            &query,
            CaseMatching::Smart,
            Normalization::Smart,
            narrowing,
        );
        self.query = query;
        true
    }

    /// Waits briefly on the matcher, and reports whether its result changed
    /// since the last poll.
    fn tick(&mut self) -> bool {
        let status = self.nucleo.tick(POLL_WAIT_MS);
        self.running = status.running;
        status.changed
    }

    /// Whether the matcher runs or is still handed candidates.
    fn at_work(&self) -> bool {
        self.running || self.streaming
    }

    /// Lists the rows the matcher's latest result ranks first, as `standing`
    /// tells of each candidate's index; it is asked only of an index handed
    /// to the matcher.
    pub(crate) fn rerank(&mut self, standing: impl Fn(u32) -> Standing) {
        self.ranked = self.query.clone();
        let snapshot = self.nucleo.snapshot();
        self.matched = snapshot.matched_item_count();
        if self.query.is_empty() || snapshot.item_count() == 0 {
            self.rows.clear();
            return;
        }
        let found = snapshot
            .matched_items(..)
            .map(|item| (*item.data, item.matcher_columns[0].slice(..)));
        self.rows = rank::rank(
            found,
            &self.query,
            snapshot.pattern().column_pattern(0),
            &mut self.matcher,
            standing,
        );
    }
}

/// Hands candidates to a matcher off the UI thread.
pub(crate) struct Feeder {
    injector: Injector<u32>,
    stopped: Arc<AtomicBool>,
}

impl Feeder {
    /// Hands the matcher the candidate `index` reading `text`; reports whether
    /// the matcher still takes candidates from this feeder.
    pub(crate) fn push(&self, index: u32, text: &str) -> bool {
        if self.stopped.load(Ordering::Relaxed) {
            return false;
        }
        self.injector
            .push(index, |_, columns| columns[0] = text.into());
        true
    }
}

/// Candidates being handed to the matcher off the UI thread, stopped once
/// this is dropped: a restarted matcher reads nothing handed to it through an
/// injector taken before.
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
pub(crate) fn inject(
    view: &mut GraphPanel,
    locate: Locate,
    take_up: TakeUp,
    feed: impl FnOnce(&Feeder) + Send + 'static,
    cx: &mut Context<GraphPanel>,
) {
    let Some(matching) = locate(view) else {
        return;
    };
    matching.nucleo.restart(true);
    matching.rows.clear();
    matching.matched = 0;
    matching.streaming = true;
    let feeder = Feeder {
        injector: matching.nucleo.injector(),
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
pub(crate) fn poll(
    view: &mut GraphPanel,
    locate: Locate,
    take_up: TakeUp,
    cx: &mut Context<GraphPanel>,
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

/// `text` with its `path` muted and the `matched` chars picked out over it.
pub(crate) fn highlighted(
    text: SharedString,
    path: &Range<u32>,
    matched: &[u32],
    muted: Hsla,
    picked: Hsla,
) -> StyledText {
    let highlights = styled_runs(&text, path, matched)
        .into_iter()
        .map(|(range, style)| {
            let highlight = match style {
                Style::Path => HighlightStyle {
                    color: Some(muted),
                    ..HighlightStyle::default()
                },
                // Picked out by colour and an underline but in the row's own
                // weight: a row is cut to fit as measured in that weight, so a
                // wider one would run past the cut and be clipped unmarked.
                Style::Matched => HighlightStyle {
                    color: Some(picked),
                    underline: Some(UnderlineStyle {
                        thickness: MATCH_UNDERLINE,
                        color: Some(picked),
                        wavy: false,
                    }),
                    ..HighlightStyle::default()
                },
            };
            (range, highlight)
        });
    let highlights: Vec<_> = highlights.collect();
    StyledText::new(text).with_highlights(highlights)
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
