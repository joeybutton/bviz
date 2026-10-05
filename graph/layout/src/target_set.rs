//! A set of targets written the way a Bazel build command line writes it:
//! target patterns separated by whitespace, each one subtracted rather than
//! added where it starts with `-`.

use std::fmt;

use crate::target_pattern::TargetPattern;

mod copies;

pub use copies::TopLevel;

/// The patterns of a target set, in the order they were written.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct TargetSet {
    patterns: Vec<Signed>,
}

/// One pattern of a target set, with the sign it was written with.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Signed {
    written: String,
    pattern: TargetPattern,
}

impl Signed {
    /// The pattern as written, sign and all, which is how an error names it.
    pub fn written(&self) -> &str {
        &self.written
    }

    /// Whether the pattern takes its targets out of those before it.
    pub fn negative(&self) -> bool {
        self.written.starts_with(NEGATIVE)
    }

    pub fn pattern(&self) -> &TargetPattern {
        &self.pattern
    }
}

/// A pattern that is no target pattern, and why.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PatternError {
    /// The pattern as written.
    pub pattern: String,
    pub message: String,
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "'{}': {}", self.pattern, self.message)
    }
}

impl std::error::Error for PatternError {}

/// Starts a comment running to the end of its line where it starts a word,
/// as a shell reads one. Anywhere else it is part of its pattern, a target
/// name being free to hold one.
const COMMENT: char = '#';

/// Marks a pattern whose targets are taken out of those before it.
const NEGATIVE: char = '-';

/// What a command line holding negative patterns writes ahead of them, for
/// Bazel to read them as patterns and not as options.
const END_OF_OPTIONS: &str = "--";

impl TargetSet {
    /// The target set `text` writes, or the first of its patterns Bazel would
    /// reject. Text holding no pattern is the empty set, and an
    /// [`END_OF_OPTIONS`] ahead of the patterns is none of them.
    pub fn parse(text: &str) -> Result<Self, PatternError> {
        let uncommented = text.lines().flat_map(|line| {
            line.split_whitespace()
                .take_while(|word| !word.starts_with(COMMENT))
        });
        let mut written = uncommented.peekable();
        written.next_if_eq(&END_OF_OPTIONS);
        let patterns = written
            .map(|written| {
                let unsigned = written.strip_prefix(NEGATIVE).unwrap_or(written);
                let pattern = TargetPattern::parse(unsigned).map_err(|message| PatternError {
                    pattern: written.to_string(),
                    message,
                })?;
                Ok(Signed {
                    written: written.to_string(),
                    pattern,
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(TargetSet { patterns })
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    pub fn patterns(&self) -> &[Signed] {
        &self.patterns
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target_pattern::Repo;

    fn written(set: &TargetSet) -> Vec<(bool, &str)> {
        let patterns = set.patterns().iter();
        patterns.map(|p| (p.negative(), p.written())).collect()
    }

    #[test]
    fn a_set_reads_its_patterns_in_order_each_signed() {
        let set = TargetSet::parse("//...  -//tools/...\n\t//dist").unwrap();
        assert_eq!(
            written(&set),
            [(false, "//..."), (true, "-//tools/..."), (false, "//dist")]
        );
        assert_eq!(
            *set.patterns()[1].pattern(),
            TargetPattern::Below {
                repo: Repo::Main,
                directory: "tools".to_string(),
                rules_only: true,
            }
        );
    }

    #[test]
    fn the_end_of_options_ahead_of_the_patterns_is_none_of_them() {
        let set = TargetSet::parse("-- //... -//tools/...").unwrap();
        assert_eq!(written(&set), [(false, "//..."), (true, "-//tools/...")]);
        assert!(TargetSet::parse("--").unwrap().is_empty());
        let set = TargetSet::parse("//a --").unwrap();
        assert_eq!(written(&set), [(false, "//a"), (true, "--")]);
    }

    #[test]
    fn a_comment_starts_a_word_and_runs_to_the_end_of_its_line() {
        let set = TargetSet::parse("//a # all of //b\n# //c\n//d #e //f").unwrap();
        assert_eq!(written(&set), [(false, "//a"), (false, "//d")]);
        assert!(TargetSet::parse("  # nothing\n").unwrap().is_empty());
    }

    #[test]
    fn a_target_name_may_hold_the_comment_mark() {
        let set = TargetSet::parse("//foo:a#b").unwrap();
        assert_eq!(
            *set.patterns()[0].pattern(),
            TargetPattern::Single {
                repo: Repo::Main,
                package: "foo".to_string(),
                name: "a#b".to_string(),
            }
        );
    }

    #[test]
    fn an_error_names_the_pattern_as_written() {
        let error = TargetSet::parse("//ok -//foo/...:bar").unwrap_err();
        assert_eq!(error.pattern, "-//foo/...:bar");
        assert!(error.to_string().starts_with("'-//foo/...:bar': "));
        assert_eq!(
            TargetSet::parse("-").unwrap_err().message,
            "invalid target name '': empty target name"
        );
    }
}
