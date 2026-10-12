//! The names a set of repositories are written and shown by.

use crate::repo::{self, RepoName};
use crate::shown::Shown;
use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;

/// The repository part a reader writes each of a set of repositories by,
/// sigil and all, which is the name a pattern reaches it by, and the part it
/// is shown by, which may be shorter.
///
/// A repository is written by the name of the module whose repository it is,
/// since `@rules_foo` is how that repository is written before a repository
/// mapping resolves the name. A name several repositories would be written by
/// says which of them a reader means no more than a dump says which one an
/// apparent name reaches, so each of those holding a canonical name is written
/// by that instead. The main repository, whose labels carry no repository part,
/// is written by nothing at all.
#[derive(Default, Clone)]
pub struct RepoNames {
    /// The name each repository is written by, in the order the set lists
    /// them.
    written: Vec<String>,
    /// The canonical name an apparent name written or shown here stands for,
    /// which is the name labels carry.
    canonical: HashMap<String, String>,
    /// The name written for the repository part a label carries, sigil and all,
    /// where the two read differently.
    rewritten: HashMap<String, String>,
    /// How many chars after its leading `@` the repository part written as
    /// the key leaves out where it is shown, which only one written by its
    /// canonical name does, and only where what is left tells it apart from
    /// every other repository.
    shortened: HashMap<String, u32>,
}

impl RepoNames {
    /// The names of the repositories `repos` identifies, each by the name its
    /// labels carry.
    pub fn of(repos: &[RepoName<&str>]) -> Self {
        fn written_apparently(name: RepoName<&str>) -> Option<&str> {
            match name {
                RepoName::Canonical(canonical) => repo::module_name(canonical),
                RepoName::Apparent(apparent) => Some(apparent),
            }
        }
        let mut claims: HashMap<&str, usize> = HashMap::with_capacity(repos.len());
        for name in repos.iter().copied().filter_map(written_apparently) {
            *claims.entry(name).or_default() += 1;
        }
        let mut names = RepoNames {
            written: Vec::with_capacity(repos.len()),
            ..RepoNames::default()
        };
        for &name in repos {
            let unshared = written_apparently(name).filter(|name| claims[name] == 1);
            names.written.push(match (name, unshared) {
                // The name identifying a repository is the only one its labels
                // carry, so it is written by that even where another repository
                // is written alike.
                (RepoName::Apparent(name), _) => format!("@{name}"),
                (RepoName::Canonical(canonical), Some(apparent)) => {
                    let (canonical, apparent) = (canonical.to_owned(), apparent.to_owned());
                    names
                        .rewritten
                        .insert(format!("@@{canonical}"), format!("@{apparent}"));
                    names.canonical.insert(apparent.clone(), canonical);
                    format!("@{apparent}")
                }
                (RepoName::Canonical(""), None) => String::new(),
                (RepoName::Canonical(canonical), None) => format!("@@{canonical}"),
            });
        }
        names.shortened = shortened(&names.written);
        // A name shown for a repository reaches it as its written name does,
        // the name being one no other repository reads alike.
        for (written, &dropped) in &names.shortened {
            let shown = Shown::dropping(written, 1..1 + dropped).into_text();
            let canonical = written.trim_start_matches('@').to_owned();
            names.canonical.insert(shown[1..].to_owned(), canonical);
        }
        names
    }

    /// The names of the repositories `labels` name, as their repository
    /// parts identify them, in the order they are first named.
    pub fn of_labels<'a>(labels: impl Iterator<Item = &'a str>) -> Self {
        let mut seen: Vec<RepoName<&str>> = Vec::new();
        for label in labels {
            let Some((repo, _)) = label.split_once("//") else {
                continue;
            };
            let name = match repo.strip_prefix("@@") {
                Some(canonical) => RepoName::Canonical(canonical),
                None => match repo.strip_prefix('@') {
                    Some(apparent) => RepoName::Apparent(apparent),
                    None => RepoName::Canonical(repo),
                },
            };
            if !seen.contains(&name) {
                seen.push(name);
            }
        }
        Self::of(&seen)
    }

    /// The name the repository at `index` of the set is written by.
    pub fn written(&self, index: usize) -> &str {
        &self.written[index]
    }

    /// The canonical name the repository written `written` carries in its
    /// labels, or `None` where that name is what its labels already carry.
    pub fn canonical(&self, written: &str) -> Option<&str> {
        self.canonical.get(written).map(String::as_str)
    }

    /// The canonical label `label` as a reader writes it, which is by the
    /// repository name the same set's boxes read.
    pub fn written_label<'a>(&self, label: &'a str) -> Cow<'a, str> {
        let Some((repo, rest)) = label.split_once("//") else {
            return Cow::Borrowed(label);
        };
        match self.rewritten.get(repo) {
            Some(written) => Cow::Owned(format!("{written}//{rest}")),
            None => Cow::Borrowed(label),
        }
    }

    /// The name the repository at `index` is shown by: the name it is
    /// written by, or where that is a canonical name, the shorter one
    /// [`shortened`] finds for it.
    pub fn shown(&self, index: usize) -> Cow<'_, str> {
        let written = self.written(index);
        match self.dropped(written) {
            Some(dropped) => Cow::Owned(Shown::dropping(written, dropped).into_text()),
            None => Cow::Borrowed(written),
        }
    }

    /// The canonical label `label` as it is shown. What is shown is for
    /// reading only; the written label is what a pattern or a command line
    /// takes.
    pub fn shown_label(&self, label: &str) -> String {
        self.show(&self.written_label(label)).into_text()
    }

    /// `written`, a label or a package path as a reader writes it, as it is
    /// shown: by the name its repository is shown by, and where it names a
    /// target named after its package or repository, by that alone.
    pub fn show(&self, written: &str) -> Shown {
        let repo = written.split_once("//").map(|(repo, _)| repo);
        let shown = match repo.and_then(|repo| self.dropped(repo)) {
            Some(dropped) => Shown::dropping(written, dropped),
            None => Shown::whole(written),
        };
        shown.named_by_place()
    }

    /// The chars the repository part written `repo` leaves out where it is
    /// shown, where it leaves any out.
    fn dropped(&self, repo: &str) -> Option<Range<u32>> {
        let &dropped = self.shortened.get(repo)?;
        Some(1..1 + dropped)
    }
}

/// How many chars after its leading `@` each repository written by its
/// canonical name in `written` leaves out to be shown as `@` and the name the
/// module extension that generated it gave it, where neither another such
/// repository nor any repository written otherwise would read alike. Keyed by
/// the written name.
fn shortened(written: &[String]) -> HashMap<String, u32> {
    let given = |written: &str| {
        let canonical = written.strip_prefix("@@")?;
        let given = repo::extension_given_name(canonical)?;
        Some(format!("@{given}"))
    };
    let mut claims: HashMap<String, usize> = HashMap::with_capacity(written.len());
    for name in written {
        let reads = given(name).unwrap_or_else(|| name.clone());
        *claims.entry(reads).or_default() += 1;
    }
    let chars = |name: &str| name.chars().count() as u32;
    let shortened = written.iter().filter_map(|name| {
        let shown = given(name).filter(|shown| claims[shown] == 1)?;
        Some((name.clone(), chars(name) - chars(&shown)))
    });
    shortened.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Labels as a build's events or profile print them carry canonical
    /// names, which are shown as the same graph's labels would be.
    #[test]
    fn the_repositories_a_set_of_labels_names_are_shown_as_a_graph_shows_them() {
        let labels = [
            "@@llvm++llvm+llvm-project//llvm:Support",
            "@@rules_rust+//rust:toolchain",
            "//ui/graph:graph",
            "@@llvm++llvm+llvm-project//llvm:llvm",
            "@crates//:crates",
        ];
        let names = RepoNames::of_labels(labels.into_iter());
        let shown: Vec<String> = labels
            .iter()
            .map(|label| names.shown_label(label))
            .collect();
        assert_eq!(
            shown,
            [
                "@llvm-project//llvm:Support",
                "@rules_rust//rust:toolchain",
                "//ui/graph",
                "@llvm-project//llvm",
                "@crates",
            ]
        );
        assert_eq!(
            names.written_label("@@rules_rust+//rust:toolchain"),
            "@rules_rust//rust:toolchain"
        );
    }
}
