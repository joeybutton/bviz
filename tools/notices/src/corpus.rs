//! The SPDX License List's licence and exception texts, as license-list-data
//! publishes them, and the identification of a harvested text against them.

use std::collections::{BTreeMap, HashSet};
use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
use std::path::PathBuf;

use crate::{normalized, read_text};

/// A text is identified by a signature taken from its middle: the fraction cut
/// from each end covers the copyright line a real licence file fills in and the
/// "how to apply these terms" appendix it leaves out.
const VARIABLE_FRACTION: usize = 4;

/// A signature shorter than this says too little to tell one licence from
/// another, so the shortest texts identify nothing.
const MIN_SIGNATURE_BYTES: usize = 200;

/// The prefix license-list-data gives the files of identifiers the SPDX License
/// List has withdrawn; the identifier itself follows it.
const DEPRECATED_PREFIX: &str = "deprecated_";

/// The List's copies of one text under several identifiers differ in at most
/// one word in this many, such as an updated URL.
const WORDS_PER_DIFFERENCE: usize = 100;

/// A text holding at least this share of a denied licence's word pairs is
/// taken to carry that licence, however it was edited. The List's own copy of
/// the licence holds nearly all of them, but so can a text of a neighbouring
/// family, such as LGPL-2.1 of GPL-2.0's, SSPL-1.0 of GPL-3.0's, or an
/// exception of the withdrawn GPL-with-exception identifier that embeds it; a
/// resemblance therefore only ever fails the build.
const DENIED_RESEMBLANCE: f64 = 0.8;

pub struct Corpus {
    texts: BTreeMap<String, String>,
    signatures: Vec<(String, String)>,
    denied_pairs: Vec<(String, HashSet<u64>)>,
}

/// Text as identification compares it: normalized, in one case.
fn folded(text: &str) -> String {
    normalized(text).to_lowercase()
}

/// A text's words, in one case.
fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The hashes of a text's adjacent word pairs, blind to case, punctuation and
/// the British spelling of "licence". The hasher has fixed keys, so the pairs
/// of different texts are comparable.
fn word_pairs(text: &str) -> HashSet<u64> {
    let hasher = BuildHasherDefault::<DefaultHasher>::default();
    let words = words(&text.to_lowercase().replace("licenc", "licens"));
    words.windows(2).map(|pair| hasher.hash_one(pair)).collect()
}

fn signature(folded: &str) -> Option<&str> {
    let margin = folded.len() / VARIABLE_FRACTION;
    let mut start = margin;
    while !folded.is_char_boundary(start) {
        start += 1;
    }
    let mut end = folded.len() - margin;
    while !folded.is_char_boundary(end) {
        end -= 1;
    }
    folded
        .get(start..end)
        .filter(|s| s.len() >= MIN_SIGNATURE_BYTES)
}

impl Corpus {
    /// Reads the given files of license-list-data's `text` tree, each named
    /// `<identifier>.txt`.
    pub fn read(paths: &[PathBuf], denied: impl Fn(&str) -> bool) -> Result<Self, String> {
        let mut texts = BTreeMap::new();
        for path in paths {
            let id = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| format!("{}: names no licence identifier", path.display()))?;
            let id = id.strip_prefix(DEPRECATED_PREFIX).unwrap_or(id);
            texts.insert(id.to_owned(), read_text(path)?);
        }
        let mut signatures = Vec::with_capacity(texts.len());
        for (id, text) in &texts {
            let folded = folded(text);
            if let Some(signature) = signature(&folded) {
                signatures.push((id.clone(), signature.to_owned()));
            }
        }
        let denied_pairs = texts
            .iter()
            .filter(|(id, _)| denied(id))
            .map(|(id, text)| (id.clone(), word_pairs(text)))
            .collect();
        Ok(Corpus {
            texts,
            signatures,
            denied_pairs,
        })
    }

    pub fn standard_text(&self, id: &str) -> Result<&str, String> {
        self.texts
            .get(id)
            .map(String::as_str)
            .ok_or_else(|| format!("the SPDX License List has no text for {id}"))
    }

    /// Whether the List's texts of two identifiers are one text, as those of
    /// GPL-2.0-only, GPL-2.0-or-later and the withdrawn GPL-2.0+ are.
    pub fn same_text(&self, a: &str, b: &str) -> bool {
        let (Some(a), Some(b)) = (self.texts.get(a), self.texts.get(b)) else {
            return false;
        };
        let (a, b) = (words(a), words(b));
        let differing = a.iter().zip(&b).filter(|(a, b)| a != b).count();
        a.len() == b.len() && differing * WORDS_PER_DIFFERENCE <= a.len()
    }

    /// The identifiers whose standard text the given text carries. Empty for a
    /// text the List does not hold, such as a library's own terms.
    pub fn identify(&self, text: &str) -> Vec<&str> {
        let folded = folded(text);
        self.signatures
            .iter()
            .filter(|(_, signature)| folded.contains(signature.as_str()))
            .map(|(id, _)| id.as_str())
            .collect()
    }

    /// The denied licence the text most resembles, if it carries one, whether
    /// or not it identifies as one.
    pub fn resembled_denied(&self, text: &str) -> Option<&str> {
        let pairs = word_pairs(text);
        self.denied_pairs
            .iter()
            .filter(|(_, denied)| pairs.len() as f64 >= DENIED_RESEMBLANCE * denied.len() as f64)
            .map(|(id, denied)| (id.as_str(), share(&pairs, denied)))
            .filter(|(_, share)| *share >= DENIED_RESEMBLANCE)
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id)
    }

    /// Each denied licence with the share of its word pairs the text holds.
    #[cfg(test)]
    pub fn resemblances(&self, text: &str) -> impl Iterator<Item = (&str, f64)> {
        let pairs = word_pairs(text);
        self.denied_pairs
            .iter()
            .map(move |(id, denied)| (id.as_str(), share(&pairs, denied)))
    }
}

/// The share of `of`'s word pairs that `pairs` holds.
fn share(pairs: &HashSet<u64>, of: &HashSet<u64>) -> f64 {
    pairs.iter().filter(|pair| of.contains(pair)).count() as f64 / of.len() as f64
}
