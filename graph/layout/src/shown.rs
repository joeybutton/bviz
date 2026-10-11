//! How a label or package path a reader writes is shown on screen, which may
//! read shorter than it is written.

use std::ops::Range;

/// Text as it is shown, and where each char of it as written stands in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    text: String,
    /// The chars of the written text the shown text leaves out of its
    /// repository part.
    dropped: Range<u32>,
    /// Where the shown text leaves out a target's name, written by Bazel's
    /// shorthand for a target named after its package (`//pkg` for
    /// `//pkg:pkg`) or its repository (`@repo` for `@repo//:repo`).
    named: Option<NamedByPlace>,
}

/// A target's `:name` left out of a label as shown, the last component of
/// the place it lives in reading it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NamedByPlace {
    /// The char of the written text holding the `:` that starts the name.
    colon: u32,
    /// The char of the shown text starting the component that reads the
    /// name.
    component: u32,
}

impl Shown {
    /// `written` shown as it is written.
    pub(crate) fn whole(written: &str) -> Self {
        Self {
            text: written.to_owned(),
            dropped: 0..0,
            named: None,
        }
    }

    /// `written` with the chars `dropped` left out.
    pub(crate) fn dropping(written: &str, dropped: Range<u32>) -> Self {
        let mut text = String::with_capacity(written.len());
        let kept = (0u32..).zip(written.chars());
        text.extend(kept.filter(|(at, _)| !dropped.contains(at)).map(|(_, c)| c));
        Self {
            text,
            dropped,
            named: None,
        }
    }

    /// This, with the name of a target named after its package or its
    /// repository left out, as Bazel's shorthands leave it out.
    pub(crate) fn named_by_place(mut self) -> Self {
        let Some(Shorthand {
            place,
            kept,
            component,
        }) = shorthand(&self.text)
        else {
            return self;
        };
        let chars = |text: &str| text.chars().count() as u32;
        let unshown = self.dropped.end - self.dropped.start;
        self.named = Some(NamedByPlace {
            colon: chars(place) + unshown,
            component: chars(kept) - chars(component),
        });
        self.text.truncate(kept.len());
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn into_text(self) -> String {
        self.text
    }

    /// The char of the shown text standing for the char `written` of the text
    /// as written, where one does.
    pub fn at(&self, written: u32) -> Option<u32> {
        if let Some(NamedByPlace { colon, component }) = self.named
            && written >= colon
        {
            return (written > colon).then(|| component + (written - colon - 1));
        }
        let Range { start, end } = self.dropped;
        match written {
            _ if written < start => Some(written),
            _ if written < end => None,
            _ => Some(written - (end - start)),
        }
    }
}

/// A label as Bazel's shorthand for a target named after its package or
/// its repository writes it, by that place alone.
struct Shorthand<'a> {
    /// The label up to the `:` starting the name.
    place: &'a str,
    /// The label as the shorthand writes it.
    kept: &'a str,
    /// The last component of `kept`, which reads the name.
    component: &'a str,
}

/// `label` as Bazel's shorthand writes it, where it names a target named
/// after its package (`//pkg` for `//pkg:pkg`) or its repository (`@repo`
/// for `@repo//:repo`).
fn shorthand(label: &str) -> Option<Shorthand<'_>> {
    let (place, name) = label.split_once(':')?;
    let (repo, package) = place.split_once("//")?;
    let (kept, component) = match package.rsplit_once('/') {
        _ if package.is_empty() => (repo, repo.trim_start_matches('@')),
        Some((_, last)) => (place, last),
        None => (place, package),
    };
    (!component.is_empty() && component == name).then_some(Shorthand {
        place,
        kept,
        component,
    })
}

/// Whether Bazel's shorthand writes `label` by its package or its
/// repository alone, the target being named after that place.
pub fn written_by_place(label: &str) -> bool {
    shorthand(label).is_some()
}

/// Where the name of the target `shown` labels starts, `shown` being a label
/// as it is shown: at the `:` before the name, or for a target written by its
/// package or repository alone, where the last component of that reads it.
/// What leads up to it is where the target lives. `None` for text that is no
/// label.
pub fn name_start(shown: &str) -> Option<usize> {
    let Some((_, path)) = shown.split_once("//") else {
        let repo = shown.trim_start_matches('@');
        return (repo.len() < shown.len()).then_some(shown.len() - repo.len());
    };
    let path_start = shown.len() - path.len();
    let start = match path.find(':') {
        Some(colon) => colon,
        None => path.rfind('/').map_or(0, |slash| slash + 1),
    };
    Some(path_start + start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_char_left_out_stands_nowhere_and_those_after_it_move_up() {
        let shown = Shown::dropping("@@a+b//c:d", 1..4);
        assert_eq!(shown.text(), "@b//c:d");
        assert_eq!(shown.at(0), Some(0));
        assert_eq!(shown.at(3), None);
        assert_eq!(shown.at(4), Some(1));
        assert_eq!(shown.at(9), Some(6));
    }

    #[test]
    fn a_target_named_after_its_package_is_shown_by_the_package_alone() {
        let read = |written: &str| Shown::whole(written).named_by_place();
        assert_eq!(read("//foo/bar:bar").text(), "//foo/bar");
        assert_eq!(read("@r//bar:bar").text(), "@r//bar");
        assert_eq!(read("//foo/bar:baz").text(), "//foo/bar:baz");
        assert_eq!(read("//foo:foo_test").text(), "//foo:foo_test");
        assert_eq!(read("//:bar").text(), "//:bar");
        assert_eq!(read("//foo/bar").text(), "//foo/bar");
    }

    #[test]
    fn a_target_named_after_its_repository_is_shown_by_the_repository_alone() {
        let read = |written: &str| Shown::whole(written).named_by_place();
        assert_eq!(read("@crates//:crates").text(), "@crates");
        assert_eq!(read("@@crates+//:crates+").text(), "@@crates+");
        assert_eq!(read("@crates//:other").text(), "@crates//:other");
        let shortened = Shown::dropping("@@x+e+tools//:tools", 1..6).named_by_place();
        assert_eq!(shortened.text(), "@tools");
        assert_eq!(shortened.at(14), Some(1));
        assert_eq!(shortened.at(18), Some(5));
    }

    #[test]
    fn a_label_s_name_starts_at_its_colon_or_its_package_s_last_component() {
        fn place(shown: &str) -> Option<&str> {
            name_start(shown).map(|start| &shown[..start])
        }
        assert_eq!(place("@r//foo/bar:baz"), Some("@r//foo/bar"));
        assert_eq!(place("//:baz"), Some("//"));
        assert_eq!(place("//foo/bar"), Some("//foo/"));
        assert_eq!(place("@r//bar"), Some("@r//"));
        assert_eq!(place("@@crates+"), Some("@@"));
        assert_eq!(place("action7"), None);
    }

    /// The chars of the name left out stand for the last component of the
    /// package, which reads alike; the colon stands for nothing.
    #[test]
    fn the_name_left_out_stands_where_the_package_reads_it() {
        let shown = Shown::dropping("@@a+b//c/d:d", 1..4).named_by_place();
        assert_eq!(shown.text(), "@b//c/d");
        assert_eq!(shown.at(8), Some(5));
        assert_eq!(shown.at(10), None);
        assert_eq!(shown.at(11), Some(6));
    }
}
