//! The words the card reads what a build reported in.

use super::{Action, BuildTally, Listed, Mismatches, Name, Row, Share, Shown, Unbuilt};
use bviz_invocation::Configurations;

/// `count` of a thing named `one`, or `many` where there is not one of it.
fn counted(count: usize, one: &str, many: &str) -> String {
    match count {
        1 => format!("1 {one}"),
        count => format!("{count} {many}"),
    }
}

/// `total` things of which each of `parts` counts some: the total, and after
/// it each count that is not zero.
fn tallied(total: String, parts: &[(usize, &str)]) -> String {
    let counts: Vec<String> = parts
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, name)| format!("{count} {name}"))
        .collect();
    match counts.is_empty() {
        true => total,
        false => format!("{total}: {}", counts.join(", ")),
    }
}

impl BuildTally {
    pub(super) fn line(&self) -> String {
        tallied(
            counted(self.total(), "target", "targets"),
            &[
                (self.built, "built"),
                (self.failed, "failed"),
                (self.not_built, "not built"),
                (self.skipped, "skipped"),
            ],
        )
    }
}

/// What a row adds to its account of a thing the graph holds no node for.
const OFF_GRAPH: &str = "not in this graph";

impl Share {
    /// That `count` of the things reported, which `one` names one of and
    /// `many` several, stand as `how` says; `None` where none does.
    fn line(&self, count: usize, one: &str, many: &str, how: &str) -> Option<String> {
        let verb = match count {
            0 => return None,
            1 => "is",
            _ => "are",
        };
        let reported = counted(self.reported, one, many);
        Some(format!("{count} of {reported} {verb} {how}"))
    }

    /// That some of the things reported are not in the graph; `None` where
    /// all are.
    fn off(&self, one: &str, many: &str) -> Option<String> {
        self.line(self.off, one, many, OFF_GRAPH)
    }
}

impl Mismatches {
    /// A line for each way the graph does not hold what the build reports
    /// on.
    pub(super) fn lines(&self) -> impl Iterator<Item = String> {
        let (target, many) = ("reported target", "reported targets");
        let reconfigured = "in this graph under other configurations only";
        [
            self.targets.off(target, many),
            self.targets
                .line(self.reconfigured, target, many, reconfigured),
            self.failed_actions.off("failed action", "failed actions"),
        ]
        .into_iter()
        .flatten()
    }
}

/// Columns a row's hover wraps what the build said at: a tooltip wider than
/// its window is cut off by it.
const HOVER_COLUMNS: usize = 96;

/// Lines of what the build said a row's hover shows before it counts the
/// rest: a tooltip taller than its window is cut off by it.
const HOVER_LINES: usize = 32;

/// `line` broken wherever it runs longer than `columns`: at the last space
/// within them, or at `columns` where it holds none.
fn broken(mut line: &str, columns: usize) -> impl Iterator<Item = &str> {
    let mut rest = Some(());
    std::iter::from_fn(move || {
        rest?;
        let Some((cut, beyond)) = line.char_indices().nth(columns) else {
            rest = None;
            return Some(line);
        };
        let (head, tail) = match line[..cut + beyond.len_utf8()].rfind(' ') {
            Some(space) if space > 0 => (&line[..space], &line[space + 1..]),
            _ => line.split_at(cut),
        };
        line = tail;
        Some(head)
    })
}

/// The lines of `text`, each [`broken`] at `columns`.
fn wrapped(text: &str, columns: usize) -> impl Iterator<Item = &str> {
    text.lines().flat_map(move |line| broken(line, columns))
}

/// What a row's hover reads: its two lines whole, over what the build said of
/// the failure.
fn hover(name: &str, detail: &str, message: &str) -> String {
    let mut said = wrapped(message.trim(), HOVER_COLUMNS).peekable();
    let mut text = format!("{name}\n{detail}");
    if said.peek().is_some() {
        text.push('\n');
    }
    for line in said.by_ref().take(HOVER_LINES) {
        text.push('\n');
        text.push_str(line);
    }
    let beyond = said.count();
    if beyond > 0 {
        text.push_str(&format!(
            "\n… and {}",
            counted(beyond, "more line", "more lines")
        ));
    }
    text
}

/// What kind of action `mnemonic` says one is, where it says any.
fn kind(mnemonic: &str) -> &str {
    match mnemonic.is_empty() {
        true => "action",
        false => mnemonic,
    }
}

impl<'a> Name<'a> {
    /// What kind of thing [`subject`](Self::subject) names, where the
    /// subject alone does not say.
    fn lead(self) -> Option<String> {
        match self {
            Name::Owned { mnemonic, .. } => Some(kind(mnemonic).to_owned()),
            Name::Writing { mnemonic, .. } => Some(format!("{} writing", kind(mnemonic))),
            Name::Label(_) => None,
        }
    }

    /// The label or path naming the thing.
    fn subject(self) -> &'a str {
        match self {
            Name::Owned { owner: subject, .. }
            | Name::Writing {
                primary_output: subject,
                ..
            }
            | Name::Label(subject) => subject,
        }
    }
}

/// How `action` failed: by the code its process exited with, where the build
/// states one.
fn exited(action: &Action) -> String {
    match action.exit_code {
        Some(code) => format!("exited {code}"),
        None => "failed".to_owned(),
    }
}

/// That a root cause did `what`, and failed `failing` targets where it
/// failed any.
fn blamed(what: &str, failing: usize) -> String {
    match failing {
        0 => what.to_owned(),
        failing => format!("{what}, failing {}", counted(failing, "target", "targets")),
    }
}

impl Listed {
    /// How the thing listed came to be listed.
    fn account(&self) -> String {
        match self {
            Listed::Action {
                action, failing, ..
            } => blamed(&exited(action), *failing),
            Listed::Target { came, failing, .. } => {
                let what = match came {
                    Unbuilt::Unanalysed => "could not be analysed",
                    Unbuilt::Uncaused => "failed, and the build names no cause",
                    Unbuilt::Unreported => "could not be loaded or analysed",
                };
                blamed(what, *failing)
            }
        }
    }

    /// What the build said of the thing listed.
    fn message(&self) -> &str {
        match self {
            Listed::Action { message, .. } => message,
            Listed::Target { error, .. } => error,
        }
    }
}

impl Row {
    pub(super) fn lead(&self) -> Option<String> {
        self.listed.name().lead()
    }

    /// The row's subject, followed where another row names its thing alike
    /// by the configuration of its own, as `configurations` names it.
    pub(super) fn subject(&self, configurations: &Configurations) -> String {
        let subject = self.listed.name().subject();
        match self.listed.configuration().filter(|_| self.alike) {
            Some(checksum) => {
                let configuration = configurations.name_or_checksum(checksum);
                format!("{subject} ({configuration})")
            }
            None => subject.to_owned(),
        }
    }

    /// The row's account of the thing listed, which says so where the graph
    /// does not hold it.
    fn account(&self) -> String {
        let account = self.listed.account();
        match self.shown {
            Shown::Itself(_) => account,
            Shown::Failed(_) | Shown::Nothing => format!("{account} · {OFF_GRAPH}"),
        }
    }

    /// The row's second line: its account, and the first line of what the
    /// build said.
    pub(super) fn detail(&self) -> String {
        let account = self.account();
        match self.listed.message().trim().lines().next() {
            Some(first) => format!("{account} — {first}"),
            None => account,
        }
    }

    pub(super) fn hover(&self, configurations: &Configurations) -> String {
        let subject = self.subject(configurations);
        let name = match self.lead() {
            Some(lead) => format!("{lead} {subject}"),
            None => subject,
        };
        hover(&name, &self.account(), self.listed.message())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_line_of_what_the_build_said_is_broken_at_a_space() {
        let wrapped = |text, columns| wrapped(text, columns).collect::<Vec<_>>();
        assert_eq!(wrapped("one two three", 7), ["one two", "three"]);
        assert_eq!(wrapped("one two three", 5), ["one", "two", "three"]);
        assert_eq!(wrapped("onetwothree", 4), ["onet", "woth", "ree"]);
        assert_eq!(wrapped("a\n\nb", 4), ["a", "", "b"]);
        assert_eq!(wrapped("", 4), Vec::<&str>::new());
    }

    /// A line is measured and cut in characters, of which a compiler's
    /// message holds some longer than a byte.
    #[test]
    fn a_line_is_broken_between_characters_however_many_bytes_each_takes() {
        let wrapped = |text, columns| wrapped(text, columns).collect::<Vec<_>>();
        assert_eq!(wrapped("ééééééé", 3), ["ééé", "ééé", "é"]);
        assert_eq!(wrapped("naïve café crème", 6), ["naïve", "café", "crème"]);
        assert_eq!(wrapped("‘a’ → ‘b’", 5), ["‘a’ →", "‘b’"]);

        let said = "é".repeat(HOVER_COLUMNS + 1);
        let text = hover("name", "detail", &said);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[3].chars().count(), HOVER_COLUMNS);
        assert_eq!(lines[4], "é");
    }

    #[test]
    fn a_hover_counts_the_lines_it_leaves_out() {
        let lines = |count: usize| vec!["line"; count].join("\n");
        let within = hover("name", "detail", &lines(HOVER_LINES));
        assert_eq!(within.lines().count(), 3 + HOVER_LINES);
        let beyond = hover("name", "detail", &lines(HOVER_LINES + 2));
        assert_eq!(beyond.lines().count(), 3 + HOVER_LINES + 1);
        assert!(beyond.ends_with("\n… and 2 more lines"), "{beyond}");
        assert_eq!(hover("name", "detail", " \n"), "name\ndetail");
    }
}
