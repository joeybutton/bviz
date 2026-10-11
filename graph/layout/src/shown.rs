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
    Parts::of_label(shown).map(|parts| parts.name_start)
}

/// How text cut short to fit is parted: a lead kept as long as anything of
/// what follows it is, a place that gives way from its middle out, and a
/// name cut short only once the place and the lead are gone. Each part ends
/// on a char boundary of the text it parts, in order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Parts {
    /// The byte where the place starts, the lead ending there.
    place_start: usize,
    /// The byte where the name starts, the place ending there.
    name_start: usize,
    /// Whether the lead reads in the muted ink of the place; else in the ink
    /// of the name.
    lead_muted: bool,
}

impl Parts {
    /// The parts of `shown`, a label as it is shown: its repository, sigils
    /// and `//`, leading the package it names its target in. `None` for
    /// text that is no label.
    pub fn of_label(shown: &str) -> Option<Self> {
        let Some((repo, path)) = shown.split_once("//") else {
            let repo = shown.trim_start_matches('@');
            let sigils = shown.len() - repo.len();
            return (sigils > 0).then_some(Self {
                place_start: sigils,
                name_start: sigils,
                lead_muted: true,
            });
        };
        let place_start = repo.len() + "//".len();
        let in_path = match path.find(':') {
            Some(colon) => colon,
            None => path.rfind('/').map_or(0, |slash| slash + 1),
        };
        Some(Self {
            place_start,
            name_start: place_start + in_path,
            lead_muted: true,
        })
    }

    /// The parts of `text` with its lead in the ink of its name, ending at
    /// the byte `place_start`, and its name starting at the byte
    /// `name_start`; `None` where those are out of order or not char
    /// boundaries of `text`.
    pub fn led(text: &str, place_start: usize, name_start: usize) -> Option<Self> {
        let parts = Self {
            place_start,
            name_start,
            lead_muted: false,
        };
        let bounds = place_start <= name_start
            && text.is_char_boundary(place_start)
            && text.is_char_boundary(name_start);
        bounds.then_some(parts)
    }
}

/// Text cut short to fit, or the whole of it where it fits, with where each
/// char of the text it was cut from stands in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fitted {
    text: String,
    /// The bytes of `text` set in the muted ink.
    muted: Range<usize>,
    /// Each run of chars of the text cut from that `text` keeps, and the
    /// char of `text` it starts at.
    kept: Vec<(Range<u32>, u32)>,
}

impl Fitted {
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The bytes of the text set in the muted ink, which read where the
    /// name stands.
    pub fn muted(&self) -> Range<usize> {
        self.muted.clone()
    }

    /// The char of the text standing for the char `from` of the text it was
    /// cut from, where it was kept.
    pub fn at(&self, from: u32) -> Option<u32> {
        let (run, start) = self.kept.iter().find(|(run, _)| run.contains(&from))?;
        Some(start + (from - run.start))
    }
}

/// Ends or stands in for the part of text cut away.
pub const ELLIPSIS: &str = "…";

/// What a fit lays out: runs of chars of the text cut from, and ellipses.
#[derive(Clone, Copy)]
enum Piece {
    Kept(usize, usize),
    Ellipsis,
}

/// `text`, parted as `parts` parts it, within `room` as `width` measures
/// text: whole where it fits; else with its place giving way from the middle
/// out, keeping its first and last chars as long as it can; then with an
/// ellipsis in place of the lead and the place; then with the name cut short
/// at its end; and where not even that fits, nothing.
pub fn fit(text: &str, parts: Parts, room: f32, mut width: impl FnMut(&str) -> f32) -> Fitted {
    let chars: Vec<char> = text.chars().collect();
    let char_at = |byte: usize| text[..byte].chars().count();
    let (place_start, name_start) = (char_at(parts.place_start), char_at(parts.name_start));
    // A place ending in `/` keeps it, parting the place from the name.
    let joint = usize::from(name_start > place_start && chars[name_start - 1] == '/');
    let place_end = name_start - joint;
    let lay = |pieces: &[Piece]| lay_out(&chars, pieces, parts, place_start, name_start);
    let mut fitted = |pieces: &[Piece]| {
        let laid = lay(pieces);
        (width(&laid.text) <= room).then_some(laid)
    };
    let whole = [Piece::Kept(0, chars.len())];
    if let Some(fit) = fitted(&whole) {
        return fit;
    }
    let place = place_end - place_start;
    if place > 0 {
        let kept = |count: usize| {
            let (front, back) = (count.div_ceil(2), count / 2);
            [
                Piece::Kept(0, place_start + front),
                Piece::Ellipsis,
                Piece::Kept(place_end - back, chars.len()),
            ]
        };
        if let Some(fit) = longest(place - 1, |count| fitted(&kept(count))) {
            return fit;
        }
    }
    // A lead of no more than a char stands as it is, which an ellipsis in its
    // place would not shorten.
    let lead: &[Piece] = match place_end {
        0 => &[],
        1 => &[Piece::Kept(0, 1)],
        _ => &[Piece::Ellipsis],
    };
    let with_lead = |rest: &[Piece]| -> Vec<Piece> {
        let joined = [Piece::Kept(place_end, name_start)];
        lead.iter().chain(&joined).chain(rest).copied().collect()
    };
    if !lead.is_empty()
        && let Some(fit) = fitted(&with_lead(&[Piece::Kept(name_start, chars.len())]))
    {
        return fit;
    }
    let name = chars.len() - name_start;
    // Keeps `count` chars of the name past its first, so a cut keeps one.
    let cut = |count: usize| {
        with_lead(&[
            Piece::Kept(name_start, name_start + count + 1),
            Piece::Ellipsis,
        ])
    };
    let fit = match name {
        0 => None,
        _ => longest(name - 1, |count| fitted(&cut(count))),
    };
    fit.unwrap_or_else(|| lay(&[]))
}

/// `shown`, a label as it is shown, or text that is no label, within `room`
/// as [`fit`] fits it: a label parted by [`Parts::of_label`], other text cut
/// short at its end.
pub fn fit_label(shown: &str, room: f32, width: impl FnMut(&str) -> f32) -> Fitted {
    fit(
        shown,
        Parts::of_label(shown).unwrap_or_default(),
        room,
        width,
    )
}

/// `pieces` of `chars` laid out one after another, muted from the place,
/// which starts at the char `place_start`, or from the start where the lead
/// is muted too, up to the name, which starts at the char `name_start`.
fn lay_out(
    chars: &[char],
    pieces: &[Piece],
    parts: Parts,
    place_start: usize,
    name_start: usize,
) -> Fitted {
    let mut text = String::with_capacity(chars.len() + ELLIPSIS.len());
    let mut kept = Vec::with_capacity(pieces.len());
    let mut at = 0u32;
    for &piece in pieces {
        match piece {
            Piece::Kept(start, end) => {
                kept.push((start as u32..end as u32, at));
                text.extend(&chars[start..end]);
                at += (end - start) as u32;
            }
            Piece::Ellipsis => {
                text.push_str(ELLIPSIS);
                at += 1;
            }
        }
    }
    let mut fitted = Fitted {
        text,
        muted: 0..0,
        kept,
    };
    let laid = at;
    let end = match name_start < chars.len() {
        true => fitted.at(name_start as u32).unwrap_or(laid),
        false => laid,
    };
    let start = match parts.lead_muted {
        true => 0,
        false => fitted.at(place_start as u32).unwrap_or(0),
    };
    let byte = |char: u32| {
        let text = &fitted.text;
        text.char_indices()
            .nth(char as usize)
            .map_or(text.len(), |(byte, _)| byte)
    };
    fitted.muted = byte(start.min(end))..byte(end);
    fitted
}

/// What `fit` makes of the most, up to `most`, it fits, where it fits any,
/// fitting fewer wherever it fits more.
fn longest<T>(most: usize, mut fit: impl FnMut(usize) -> Option<T>) -> Option<T> {
    let mut best = fit(0)?;
    let (mut fits, mut over) = (0, most + 1);
    while over - fits > 1 {
        let mid = (fits + over) / 2;
        match fit(mid) {
            Some(fitted) => (best, fits) = (fitted, mid),
            None => over = mid,
        }
    }
    Some(best)
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

    /// Text as wide as it has chars.
    fn chars(text: &str) -> f32 {
        text.chars().count() as f32
    }

    fn fit(shown: &str, room: f32) -> String {
        fit_label(shown, room, chars).text().to_owned()
    }

    #[test]
    fn a_label_is_cut_short_in_its_package_before_its_name() {
        let label = "@llvm-project//llvm/lib/CodeGen:CodeGen";
        assert_eq!(fit(label, 40.0), label);
        assert_eq!(fit(label, 34.0), "@llvm-project//llvm/…deGen:CodeGen");
        assert_eq!(fit(label, 31.0), "@llvm-project//llvm…Gen:CodeGen");
        assert_eq!(fit(label, 24.0), "@llvm-project//…:CodeGen");
        assert_eq!(fit(label, 9.0), "…:CodeGen");
        assert_eq!(fit(label, 6.0), "…:Cod…");
        assert_eq!(fit(label, 2.0), "");
    }

    #[test]
    fn a_label_written_by_its_package_keeps_the_package_s_last_component() {
        let label = "@llvm-project//llvm/lib/CodeGen";
        assert_eq!(fit(label, 25.0), "@llvm-project//l…/CodeGen");
        assert_eq!(fit(label, 9.0), "…/CodeGen");
    }

    #[test]
    fn a_label_cut_short_mutes_what_stands_for_where_it_lives() {
        let read = |fitted: Fitted| (fitted.text().to_owned(), fitted.muted());
        assert_eq!(
            read(fit_label("//a/b/c:d", 6.0, chars)),
            ("//a…:d".into(), 0..6)
        );
        assert_eq!(
            read(fit_label("//a/b/c:defgh", 5.0, chars)),
            ("…:de…".into(), 0..3)
        );
        assert_eq!(
            read(fit_label("@crates", 4.0, chars)),
            ("@cr…".into(), 0..1)
        );
    }

    #[test]
    fn text_that_is_no_label_is_cut_short_at_its_end() {
        assert_eq!(fit("action7", 5.0), "acti…");
        assert_eq!(fit("action7", 1.0), "");
    }

    /// A label whose name is empty, as a root package's path is, fits as
    /// much of its place as it can and nothing where none fits.
    #[test]
    fn a_label_with_an_empty_name_fits_without_one() {
        assert_eq!(fit("@crates//", 9.0), "@crates//");
        assert_eq!(fit("@crates//", 0.0), "");
    }

    /// A lead in the name's ink, an action's mnemonic say, stays unmuted, and
    /// each char kept says where it stands.
    #[test]
    fn a_led_text_keeps_its_lead_and_maps_the_chars_it_keeps() {
        let text = "CppCompile out/k8/bin/foo.o";
        let parts = Parts::led(text, 11, 22).unwrap();
        let fitted = super::fit(text, parts, 20.0, chars);
        assert_eq!(fitted.text(), "CppCompile o…n/foo.o");
        assert_eq!(fitted.muted(), 11..17);
        assert_eq!(fitted.at(0), Some(0));
        assert_eq!(fitted.at(13), None);
        assert_eq!(fitted.at(20), Some(13));
        assert_eq!(fitted.at(22), Some(15));
        assert_eq!(Parts::led(text, 22, 11), None);
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
