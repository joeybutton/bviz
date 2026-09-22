//! Writes the third-party notices of a binary from the licence data
//! //tools/notices:notices.bzl collects over its dependencies.

mod corpus;
mod expression;
mod purl;
mod version;

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::str::FromStr;

use clap::Parser;
use packageurl::PackageUrl;
use serde::Deserialize;

use corpus::Corpus;
use version::Version;

#[derive(Parser)]
struct Args {
    /// The JSON manifest the third_party_notices rule writes.
    #[arg(long)]
    manifest: PathBuf,
    /// The in-repo table of licence facts a component's own metadata lacks.
    #[arg(long)]
    components: PathBuf,
    /// Where the notices are written.
    #[arg(long)]
    output: PathBuf,
}

#[derive(Deserialize)]
struct Manifest {
    crates: Vec<CrateEntry>,
    package_metadata: Vec<PathBuf>,
    denied_licences: Vec<String>,
    preferred_licences: Vec<String>,
    spdx_texts: Vec<PathBuf>,
}

#[derive(Deserialize)]
struct CrateEntry {
    /// rules_rs's file of the crate's Cargo manifest fields, at the crate root.
    env: PathBuf,
    metadata: PathBuf,
    licence_files: Vec<PathBuf>,
}

/// The licence facts a component's own metadata does not carry, recorded in the
/// repository against the component's package name. Every entry states the
/// reason it is there.
#[derive(Deserialize)]
struct Table {
    declared_licences: BTreeMap<String, DeclaredLicence>,
    copyright_holders: BTreeMap<String, CopyrightHolders>,
    accepted_texts: BTreeMap<String, BTreeMap<String, AcceptedText>>,
}

#[derive(Deserialize)]
struct DeclaredLicence {
    licence: String,
    #[allow(dead_code, reason = "the reason is written for a reader of the table")]
    reason: String,
}

#[derive(Deserialize)]
struct CopyrightHolders {
    holders: Vec<String>,
    #[allow(dead_code, reason = "the reason is written for a reader of the table")]
    reason: String,
}

/// A licence text a component vendors whose terms the component's own licence
/// declaration does not name, and which bviz may keep regardless.
#[derive(Deserialize)]
struct AcceptedText {
    terms: Terms,
    #[allow(dead_code, reason = "the reason is written for a reader of the table")]
    reason: String,
}

/// The terms bviz takes an accepted text under. Nothing that does not parse as
/// an SPDX expression becomes one, so an entry always has terms to state.
#[derive(Deserialize)]
#[serde(try_from = "String")]
struct Terms(String);

impl TryFrom<String> for Terms {
    type Error = spdx::ParseError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        expression::parse(&text).map(|parsed| Self(expression::canonical(&parsed)))
    }
}

impl Terms {
    fn as_str(&self) -> &str {
        &self.0
    }
}

/// The JSON a package_metadata target writes.
#[derive(Deserialize)]
struct PackageMetadata {
    label: String,
    purl: String,
    attributes: BTreeMap<String, PathBuf>,
}

/// The JSON a license target writes.
#[derive(Deserialize)]
struct LicenceAttribute {
    kind: LicenceKind,
    text: Option<PathBuf>,
}

#[derive(Deserialize)]
struct LicenceKind {
    identifier: String,
}

const LICENCE_ATTRIBUTE: &str = "build.bazel.attribute.license";

/// The licences whose notice requirement is discharged by naming the copyright
/// holder, so that their standard text alone says nothing.
const NOTICE_LICENCE_PREFIXES: [&str; 4] = ["0BSD", "BSD-", "ISC", "MIT"];

struct Component {
    name: String,
    version: Option<Version>,
    /// The expression the component is available under.
    declared: String,
    /// The alternative of `declared` bviz takes the component under.
    elected: String,
    /// The terms of the texts the component vendors that `declared` does not
    /// name and bviz accepts anyway.
    accepted: Vec<String>,
    source: String,
    copyright: Vec<String>,
    texts: Vec<Text>,
}

struct Text {
    body: String,
    /// The SPDX License List's text, standing in for a licence file the
    /// component does not ship.
    standard: bool,
}

/// How an entry and every message about a component names it.
fn title(name: &str, version: Option<&Version>) -> String {
    match version {
        Some(version) => format!("{name} {version}"),
        None => name.to_owned(),
    }
}

impl Component {
    fn title(&self) -> String {
        title(&self.name, self.version.as_ref())
    }
}

/// What a component's own metadata says, before the in-repo table fills gaps.
struct Declaration {
    name: String,
    version: Option<Version>,
    source: String,
    licence: String,
    copyright: Vec<String>,
    /// The licence texts the component ships, by the path each sits at below
    /// the component's own root.
    texts: Vec<(String, String)>,
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// A licence text as a reader should see it: UTF-8 (a file that is not is read
/// as Latin-1), LF line ends, no trailing blanks.
pub fn read_text(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let bytes = bytes.strip_prefix("\u{feff}".as_bytes()).unwrap_or(&bytes);
    let decoded = match std::str::from_utf8(bytes) {
        Ok(text) => Cow::Borrowed(text),
        Err(_) => Cow::Owned(bytes.iter().map(|&b| char::from(b)).collect::<String>()),
    };
    let mut text = String::with_capacity(decoded.len());
    for line in decoded.lines() {
        let line = line.trim_end();
        if line.is_empty() && text.is_empty() {
            continue;
        }
        text.push_str(line);
        text.push('\n');
    }
    while text.ends_with('\n') {
        text.pop();
    }
    Ok(text)
}

/// The fields of a rules_rs crate environment file, which holds one
/// `CARGO_PKG_<FIELD>=<value>` line per Cargo manifest field.
fn cargo_fields(path: &Path) -> Result<HashMap<String, String>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut fields = HashMap::with_capacity(text.lines().count());
    for line in text.lines() {
        if let Some((key, value)) = line
            .strip_prefix("CARGO_PKG_")
            .and_then(|l| l.split_once('='))
        {
            fields.insert(key.to_owned(), value.trim().to_owned());
        }
    }
    Ok(fields)
}

/// Builds the components of the notices, drawing on the SPDX License List for
/// the texts a component does not ship and on the in-repo table for the facts
/// its own metadata does not carry.
struct Catalogue<'a> {
    corpus: &'a Corpus,
    denied: &'a [String],
    preferred: &'a [String],
    table: &'a Table,
    table_path: &'a Path,
}

fn is_denied(denied: &[String], id: &str) -> bool {
    denied.iter().any(|prefix| id.starts_with(prefix.as_str()))
}

impl Catalogue<'_> {
    fn crate_component(&self, entry: &CrateEntry) -> Result<Component, Vec<String>> {
        let one = |e: String| vec![e];
        let fields = cargo_fields(&entry.env).map_err(one)?;
        let field = |key: &str| fields.get(key).map(String::as_str).unwrap_or_default();
        let name = field("NAME").to_owned();
        let version = Version::parse(field("VERSION"));
        let title = title(&name, version.as_ref());

        let metadata: PackageMetadata = read_json(&entry.metadata).map_err(one)?;
        let purl = PackageUrl::from_str(&metadata.purl)
            .map_err(|e| one(format!("crate {title}: purl {:?}: {e}", metadata.purl)))?;
        let source = purl::source(&purl).ok_or_else(|| {
            one(format!(
                "crate {title}: no known source location for {:?}",
                metadata.purl
            ))
        })?;

        let mut paths: Vec<&PathBuf> = entry.licence_files.iter().collect();
        paths.sort();
        let root = entry.env.parent().unwrap_or(Path::new(""));
        let mut texts = Vec::with_capacity(paths.len());
        for path in paths {
            let body = read_text(path).map_err(|e| one(format!("crate {title}: {e}")))?;
            let relative = path.strip_prefix(root).unwrap_or(path);
            texts.push((relative.display().to_string(), body));
        }

        self.component(Declaration {
            copyright: field("AUTHORS")
                .split(':')
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .map(str::to_owned)
                .collect(),
            licence: field("LICENSE").to_owned(),
            name,
            version,
            source,
            texts,
        })
    }

    fn package_component(&self, path: &Path) -> Result<Component, Vec<String>> {
        let one = |e: String| vec![e];
        let metadata: PackageMetadata = read_json(path).map_err(one)?;
        let label = &metadata.label;
        let purl = PackageUrl::from_str(&metadata.purl)
            .map_err(|e| one(format!("{label}: purl {:?}: {e}", metadata.purl)))?;
        let source = purl::source(&purl).ok_or_else(|| {
            one(format!(
                "{label}: no known source location for {:?}",
                metadata.purl
            ))
        })?;
        let attribute_path = metadata
            .attributes
            .get(LICENCE_ATTRIBUTE)
            .ok_or_else(|| one(format!("{label} declares no licence")))?;
        let attribute: LicenceAttribute = read_json(attribute_path).map_err(one)?;

        let texts = match &attribute.text {
            Some(path) => {
                let body = read_text(path).map_err(|e| one(format!("{label}: {e}")))?;
                let name = path.file_name().unwrap_or(path.as_os_str());
                vec![(name.to_string_lossy().into_owned(), body)]
            }
            None => Vec::new(),
        };

        self.component(Declaration {
            name: purl::display_name(&purl),
            version: purl.version().and_then(Version::parse),
            source,
            licence: attribute.kind.identifier,
            copyright: Vec::new(),
            texts,
        })
    }

    fn component(&self, declaration: Declaration) -> Result<Component, Vec<String>> {
        let Declaration {
            name,
            version,
            source,
            licence,
            copyright,
            texts,
        } = declaration;
        let title = title(&name, version.as_ref());
        let table = self.table_path.display();

        let licence = if !licence.is_empty() {
            Cow::Owned(licence)
        } else {
            let entry = self.table.declared_licences.get(&name).ok_or_else(|| {
                vec![format!(
                    "{title} declares no licence; record the one it is available under in {table}"
                )]
            })?;
            Cow::Borrowed(entry.licence.as_str())
        };
        let parsed = expression::parse(&licence)
            .map_err(|e| vec![format!("{title}: licence {licence:?}: {e}")])?;
        let declared = expression::canonical(&parsed);
        let elected_terms = expression::elect(
            &parsed,
            |id| is_denied(self.denied, id),
            self.preferred,
        )
        .ok_or_else(|| {
            vec![format!(
                "{title} is only available under {declared}, which includes a denied licence"
            )]
        })?;
        let elected = expression::conjunction(&elected_terms);
        let declared_ids = expression::identifiers(parsed.requirements().map(|r| &r.req));
        let elected_ids = expression::identifiers(elected_terms);

        let accepted = self.table.accepted_texts.get(&name);
        let mut accepted_terms: Vec<String> = Vec::new();
        let mut errors = Vec::new();
        let mut covered: BTreeSet<String> = BTreeSet::new();
        let mut own_wording = false;
        let mut kept = Vec::with_capacity(texts.len() + elected_ids.len());
        for (path, body) in texts {
            let ids = self.corpus.identify(&body);
            let covers: Vec<&str> = ids
                .iter()
                .copied()
                .filter(|id| elected_ids.iter().any(|e| e == id))
                .collect();
            // A denied text is the copy of an alternative bviz does not take
            // when the declaration names its text, and is then dropped unless
            // it shares its file with the elected terms.
            let denied = if ids.iter().any(|id| is_denied(self.denied, id)) {
                ids.iter().copied().find(|id| {
                    is_denied(self.denied, id)
                        && (!covers.is_empty()
                            || !declared_ids.iter().any(|d| self.corpus.same_text(d, id)))
                })
            } else {
                self.corpus.resembled_denied(&body)
            };
            if let Some(denied) = denied {
                match accepted.and_then(|files| files.get(&path)) {
                    Some(entry) => {
                        let terms = entry.terms.as_str();
                        if !accepted_terms.iter().any(|t| t == terms) {
                            accepted_terms.push(terms.to_owned());
                        }
                    }
                    None => errors.push(format!(
                        "{title} ships {path}, which carries the {denied} text that {elected} does not cover; \
                         drop the dependency, or record in {table} why the text does not reach what bviz \
                         links, or on what terms bviz accepts it"
                    )),
                }
            } else if covers.is_empty()
                && ids.iter().any(|id| {
                    is_denied(self.denied, id)
                        || (declared_ids.iter().any(|d| d == id) && names_licence(&path, id))
                })
            {
                // The text of an alternative the component is not taken under;
                // a denied one never ships, whatever file holds it.
                continue;
            }
            own_wording |= ids.is_empty();
            covered.extend(covers.into_iter().map(str::to_owned));
            kept.push(Text {
                body,
                standard: false,
            });
        }

        let ships_a_text = !kept.is_empty();

        // A term the component ships no text for is discharged by the SPDX
        // License List's standard text. Where no shipped text covers a term and
        // one is identified as no licence at all, that text is taken to be the
        // component's own wording of its terms rather than evidence that one
        // is missing.
        if !own_wording || !covered.is_empty() {
            for id in &elected_ids {
                if covered.contains(id) {
                    continue;
                }
                match self.corpus.standard_text(id) {
                    Ok(text) => kept.push(Text {
                        body: text.to_owned(),
                        standard: true,
                    }),
                    Err(e) => errors.push(format!("{title} ships no text for {id}, and {e}")),
                }
            }
        }

        let copyright = if !copyright.is_empty() {
            copyright
        } else if let Some(entry) = self.table.copyright_holders.get(&name) {
            entry.holders.clone()
        } else {
            Vec::new()
        };
        // A shipped licence text carries the component's own copyright line;
        // the SPDX License List's leaves a placeholder in its place, so the
        // entry has to name the holders instead.
        if copyright.is_empty() && !ships_a_text {
            for id in elected_ids
                .iter()
                .filter(|id| NOTICE_LICENCE_PREFIXES.iter().any(|p| id.starts_with(p)))
            {
                errors.push(format!(
                    "{title} is taken under {id} and ships no licence text, so nothing names its \
                     copyright holders; record them in {table}"
                ));
            }
        }

        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(Component {
            name,
            version,
            declared,
            elected,
            accepted: accepted_terms,
            source,
            copyright,
            texts: kept,
        })
    }
}

/// The words that name a file as a licence file, whatever licence it holds.
const LICENCE_FILE_WORDS: [&str; 3] = ["copying", "licence", "license"];

/// The extensions a licence file's name may end in without naming a licence.
const TEXT_EXTENSIONS: [&str; 4] = ["markdown", "md", "rst", "txt"];

/// A name's runs of letters and of digits, in one case.
fn name_words(name: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut last: Option<bool> = None;
    for c in name.to_lowercase().chars() {
        let kind = c.is_alphanumeric().then(|| c.is_ascii_digit());
        match (kind, words.last_mut()) {
            (Some(digit), Some(word)) if last == Some(digit) => word.push(c),
            (Some(_), _) => words.push(c.to_string()),
            (None, _) => {}
        }
        last = kind;
    }
    words
}

/// Whether a licence file's name marks it as the text of the given licence:
/// with the licence-file word and a text extension removed, what remains is
/// that licence's identifier or a leading part of it, as in `LICENSE-MIT`,
/// `LICENSE-APACHE` and `LICENSE-ZLIB.md`. A file named for anything else,
/// such as `LICENSE-THIRD-PARTY`, may carry the terms of work the component
/// includes under a licence it also offers itself.
fn names_licence(path: &str, id: &str) -> bool {
    let name = Path::new(path)
        .file_name()
        .map_or(Cow::Borrowed(path), |name| name.to_string_lossy());
    let mut words = name_words(&name);
    if words
        .last()
        .is_some_and(|w| TEXT_EXTENSIONS.contains(&w.as_str()))
    {
        words.pop();
    }
    words.retain(|w| !LICENCE_FILE_WORDS.contains(&w.as_str()));
    !words.is_empty() && name_words(id).starts_with(&words)
}

/// Texts that differ only in whitespace are one text.
pub fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

const RULE_WIDTH: usize = 80;

/// The column an entry's values start at: one past the widest field name,
/// "Taken under:".
const LABEL_WIDTH: usize = 13;

const ENTRY_BYTES: usize = 256;

fn push_wrapped(out: &mut String, lead: &str, words: &[String]) {
    let indent = " ".repeat(lead.len());
    let mut line = lead.to_owned();
    for (i, word) in words.iter().enumerate() {
        let piece = if i + 1 < words.len() {
            format!("{word},")
        } else {
            word.clone()
        };
        if line.len() > indent.len() && line.len() + 1 + piece.len() > RULE_WIDTH {
            out.push_str(line.trim_end());
            out.push('\n');
            line = indent.clone();
        } else if line.len() > indent.len() {
            line.push(' ');
        }
        line.push_str(&piece);
    }
    out.push_str(&line);
    out.push('\n');
}

fn push_field(out: &mut String, label: &str, value: &str) {
    let _ = writeln!(out, "    {label:<LABEL_WIDTH$}{value}");
}

const HEADER: &str = "\
THIRD-PARTY NOTICES

This program includes the third-party components listed below. Each entry names
the licence the component is available under, the terms it is taken under where
those are narrower, the further terms of work it carries that its own licence
does not name, its copyright holders, where its source code is available, and
the licence texts that apply to it, which follow the list under the numbers
given. A component marked \"standard text\" ships no licence file of its own for
at least one of the terms it is taken under; that licence's text from the SPDX
License List stands in, and the copyright holders the entry names are the ones
its notice refers to. The NOTICE file accompanying this program states what
bviz accepts on an \"also under\" line and the obligations that follow for anyone
who redistributes the program.
";

fn render(components: &[Component]) -> String {
    let mut index: HashMap<String, usize> = HashMap::with_capacity(components.len());
    let mut texts: Vec<(&str, Vec<String>)> = Vec::with_capacity(components.len());
    let mut component_texts: Vec<Vec<usize>> = Vec::with_capacity(components.len());
    for component in components {
        let title = component.title();
        let mut numbers = Vec::with_capacity(component.texts.len());
        for text in &component.texts {
            let number = *index.entry(normalized(&text.body)).or_insert_with(|| {
                texts.push((&text.body, Vec::new()));
                texts.len()
            });
            let users = &mut texts[number - 1].1;
            if users.last() != Some(&title) {
                users.push(title.clone());
            }
            if !numbers.contains(&number) {
                numbers.push(number);
            }
        }
        numbers.sort_unstable();
        component_texts.push(numbers);
    }

    let rule = "=".repeat(RULE_WIDTH);
    let thin_rule = "-".repeat(RULE_WIDTH);
    let text_bytes: usize = texts.iter().map(|(t, _)| t.len()).sum();
    // Each component entry and each text's heading run to a few lines.
    let mut out = String::with_capacity(
        HEADER.len() + text_bytes + (components.len() + texts.len()) * ENTRY_BYTES,
    );
    out.push_str(HEADER);
    let _ = write!(out, "\n{rule}\nCOMPONENTS\n{rule}\n");
    for (component, numbers) in components.iter().zip(&component_texts) {
        let _ = write!(out, "\n{}\n", component.title());
        push_field(&mut out, "Licence:", &component.declared);
        if component.elected != component.declared {
            push_field(&mut out, "Taken under:", &component.elected);
        }
        if !component.accepted.is_empty() {
            let lead = format!("    {:<LABEL_WIDTH$}", "Also under:");
            push_wrapped(&mut out, &lead, &component.accepted);
        }
        if !component.copyright.is_empty() {
            let lead = format!("    {:<LABEL_WIDTH$}", "Copyright:");
            push_wrapped(&mut out, &lead, &component.copyright);
        }
        push_field(&mut out, "Source:", &component.source);
        let refs: Vec<String> = numbers.iter().map(|n| format!("[{n}]")).collect();
        let marker = if component.texts.iter().any(|t| t.standard) {
            " (standard text)"
        } else {
            ""
        };
        push_field(&mut out, "Texts:", &format!("{}{marker}", refs.join(" ")));
    }

    let _ = write!(out, "\n\n{rule}\nLICENCE TEXTS\n{rule}\n");
    for (i, (text, users)) in texts.iter().enumerate() {
        let _ = write!(out, "\n{thin_rule}\n");
        push_wrapped(&mut out, &format!("[{}] Applies to: ", i + 1), users);
        let _ = write!(out, "{thin_rule}\n\n{text}\n");
    }
    out
}

fn run(args: &Args) -> Result<(), Vec<String>> {
    let manifest: Manifest = read_json(&args.manifest).map_err(|e| vec![e])?;
    let table: Table = read_json(&args.components).map_err(|e| vec![e])?;
    let denied = |id: &str| is_denied(&manifest.denied_licences, id);
    let corpus = Corpus::read(&manifest.spdx_texts, denied).map_err(|e| vec![e])?;
    let catalogue = Catalogue {
        corpus: &corpus,
        denied: &manifest.denied_licences,
        preferred: &manifest.preferred_licences,
        table: &table,
        table_path: &args.components,
    };

    // Every failure is gathered, so one run reports each component that needs
    // attention.
    let mut errors = Vec::new();
    let mut components =
        Vec::with_capacity(manifest.crates.len() + manifest.package_metadata.len());
    for entry in &manifest.crates {
        match catalogue.crate_component(entry) {
            Ok(component) => components.push(component),
            Err(e) => errors.extend(e),
        }
    }
    for path in &manifest.package_metadata {
        match catalogue.package_component(path) {
            Ok(component) => components.push(component),
            Err(e) => errors.extend(e),
        }
    }
    if !errors.is_empty() {
        errors.sort();
        errors.dedup();
        return Err(errors);
    }

    components.sort_by(|a, b| {
        (a.name.to_lowercase(), &a.version, &a.source).cmp(&(
            b.name.to_lowercase(),
            &b.version,
            &b.source,
        ))
    });
    components.dedup_by(|later, kept| {
        if (&later.name, &later.version, &later.source, &later.elected)
            != (&kept.name, &kept.version, &kept.source, &kept.elected)
        {
            return false;
        }
        for text in later.texts.drain(..) {
            if !kept.texts.iter().any(|t| t.body == text.body) {
                kept.texts.push(text);
            }
        }
        for terms in later.accepted.drain(..) {
            if !kept.accepted.contains(&terms) {
                kept.accepted.push(terms);
            }
        }
        true
    });

    std::fs::write(&args.output, render(&components))
        .map_err(|e| vec![format!("{}: {e}", args.output.display())])
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(errors) => {
            for error in errors {
                eprintln!("error: {error}");
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use super::*;

    fn component(name: &str, texts: &[&str]) -> Component {
        Component {
            name: name.into(),
            version: Version::parse("1.0.0"),
            declared: "MIT".into(),
            elected: "MIT".into(),
            accepted: Vec::new(),
            source: format!("https://crates.io/crates/{name}/1.0.0"),
            copyright: Vec::new(),
            texts: texts
                .iter()
                .map(|t| Text {
                    body: (*t).to_owned(),
                    standard: false,
                })
                .collect(),
        }
    }

    #[test]
    fn texts_differing_in_whitespace_are_shared() {
        let out = render(&[
            component("a", &["Permission is  granted."]),
            component("b", &["Permission is\ngranted."]),
            component("c", &["Another text."]),
        ]);
        assert!(out.contains("[1] Applies to: a 1.0.0, b 1.0.0\n"));
        assert!(out.contains("[2] Applies to: c 1.0.0\n"));
        assert!(!out.contains("[3]"));
    }

    #[test]
    fn an_entry_names_the_elected_alternative_only_when_it_narrows() {
        let mut dual = component("dual", &[]);
        assert!(!render(std::slice::from_ref(&dual)).contains("Taken under:"));
        dual.declared = "MIT OR GPL-2.0-only".into();
        let out = render(&[dual]);
        assert!(out.contains("    Licence:     MIT OR GPL-2.0-only\n"));
        assert!(out.contains("    Taken under: MIT\n"));
    }

    #[test]
    fn an_entry_names_the_terms_of_an_accepted_text() {
        let mut vendoring = component("vendoring", &[]);
        vendoring.accepted = vec!["LGPL-3.0".into()];
        assert!(render(&[vendoring]).contains("    Also under:  LGPL-3.0\n"));
    }

    /// A text long enough to identify.
    /// The SPDX License List, as the test's data carries it.
    fn spdx() -> &'static Corpus {
        static CORPUS: OnceLock<Corpus> = OnceLock::new();
        CORPUS.get_or_init(|| {
            let runfiles = runfiles::Runfiles::create().unwrap();
            let paths: Vec<PathBuf> = std::env::var("SPDX_TEXTS")
                .unwrap()
                .split_whitespace()
                .map(|path| runfiles::rlocation!(runfiles, path).unwrap())
                .collect();
            Corpus::read(&paths, |id| is_denied(&denied(), id)).unwrap()
        })
    }

    fn denied() -> Vec<String> {
        ["AGPL-", "GPL-", "LGPL-"].map(String::from).into()
    }

    /// One file holding the standard texts of the given licences.
    fn text(ids: &[&str]) -> String {
        let texts: Vec<&str> = ids
            .iter()
            .map(|id| spdx().standard_text(id).unwrap())
            .collect();
        texts.join("\n\n")
    }

    fn build(licence: &str, texts: Vec<(&str, String)>) -> Result<Component, Vec<String>> {
        let table = r#"{"declared_licences": {}, "copyright_holders": {}, "accepted_texts": {}}"#;
        Catalogue {
            corpus: spdx(),
            denied: &denied(),
            preferred: &[],
            table: &serde_json::from_str(table).unwrap(),
            table_path: Path::new(""),
        }
        .component(Declaration {
            name: "c".into(),
            version: None,
            source: String::new(),
            licence: licence.into(),
            copyright: Vec::new(),
            texts: texts.into_iter().map(|(p, t)| (p.to_owned(), t)).collect(),
        })
    }

    #[test]
    fn a_denied_text_beside_the_elected_one_fails() {
        for (licence, ids) in [
            ("MIT", &["MIT", "GPL-2.0-only"][..]),
            ("MIT OR GPL-2.0-only", &["MIT", "GPL-2.0-only"]),
            (
                "MIT OR GPL-2.0-only",
                &["MIT", "GPL-2.0-only", "GPL-3.0-only"],
            ),
        ] {
            let errors = build(licence, vec![("LICENSE", text(ids))]).err().unwrap();
            assert!(errors[0].contains("GPL-"), "{errors:?}");
        }
    }

    #[test]
    fn a_denied_text_the_declared_one_merely_contains_fails() {
        let texts = vec![
            ("LICENSE-MIT", text(&["MIT"])),
            ("COPYING", text(&["GPL-3.0-only"])),
        ];
        let errors = build("MIT OR LGPL-3.0-or-later", texts).err().unwrap();
        assert!(errors[0].contains("ships COPYING"), "{errors:?}");
    }

    /// The List's LGPL-2.1 text with curly quotes, the British spelling and a
    /// typo, which its signature no longer finds.
    fn edited_lgpl() -> String {
        let text = text(&["LGPL-2.1-only"]);
        let middle = text.len() / 2;
        let edited = format!(
            "{}{}",
            &text[..middle],
            text[middle..].replacen("the ", "teh ", 1)
        );
        edited.replace('"', "\u{201c}").replace("icense", "icence")
    }

    #[test]
    fn an_edited_denied_text_fails() {
        assert!(spdx().identify(&edited_lgpl()).is_empty());
        let texts = vec![("LICENSE", text(&["MIT"])), ("COPYING.LIB", edited_lgpl())];
        let errors = build("MIT", texts).err().unwrap();
        assert!(errors[0].contains("ships COPYING.LIB"), "{errors:?}");
    }

    #[test]
    fn an_edited_denied_text_is_named_by_the_licence_it_most_resembles() {
        let gpl = text(&["GPL-3.0-only"]).replace("icense", "icence");
        assert!(spdx().identify(&gpl).iter().all(|id| !id.contains("GPL")));
        let texts = vec![("LICENSE", text(&["MIT"])), ("COPYING", gpl)];
        let errors = build("MIT", texts).err().unwrap();
        assert!(errors[0].contains("carries the GPL-3.0"), "{errors:?}");
    }

    #[test]
    fn permissive_texts_resemble_no_denied_licence() {
        for id in ["MIT", "Apache-2.0", "BSD-3-Clause"] {
            let text = text(&[id]);
            let (denied, share) = spdx()
                .resemblances(&text)
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap();
            assert!(share < 0.5, "{id} resembles {denied} at {share}");
        }
    }

    #[test]
    fn an_unelected_alternatives_text_is_kept_unless_its_file_is_named_for_it() {
        let texts = vec![
            ("LICENSE-APACHE", text(&["Apache-2.0"])),
            ("LICENSE-MIT", text(&["MIT"])),
            ("LICENSE-THIRD-PARTY", text(&["MIT"])),
        ];
        let component = build("Apache-2.0 OR MIT", texts).unwrap();
        assert_eq!(component.texts.len(), 2);
    }

    #[test]
    fn an_unelected_alternatives_text_leaves_the_elected_standard_text() {
        let component = build("Apache-2.0 OR MIT", vec![("LICENSE", text(&["MIT"]))]).unwrap();
        assert_eq!(component.texts.len(), 2);
        assert!(component.texts.iter().any(|t| t.standard));
    }

    #[test]
    fn an_identified_vendored_text_leaves_the_elected_standard_text() {
        let texts = vec![
            ("LICENSE-MIT", text(&["MIT"])),
            ("third_party/x/LICENSE", text(&["BSD-3-Clause"])),
        ];
        let component = build("Apache-2.0 OR MIT", texts).unwrap();
        assert!(component.texts.iter().any(|t| t.standard));
    }

    #[test]
    fn a_denied_alternatives_text_is_dropped_whatever_its_file_name() {
        let texts = vec![("COPYING", text(&["GPL-2.0-only"]))];
        let component = build("Apache-2.0 OR GPL-2.0-only", texts).unwrap();
        assert!(component.texts.iter().all(|t| t.standard));
    }

    #[test]
    fn licence_files_are_named_for_the_whole_licence_name() {
        assert!(names_licence("LICENSE-MIT", "MIT"));
        assert!(names_licence("sub/LICENSE.MIT", "MIT"));
        assert!(names_licence("LICENSE-MIT.md", "MIT"));
        assert!(names_licence("LICENSE-APACHE", "Apache-2.0"));
        assert!(names_licence("LICENSE-ZLIB.md", "Zlib"));
        assert!(names_licence("LICENSE-CC0", "CC0-1.0"));
        assert!(!names_licence("LICENSE-MIT-THIRD-PARTY", "MIT"));
        assert!(!names_licence("LICENSE-3RD-PARTY-MITM", "MIT"));
        assert!(!names_licence("LICENSE-THIRD-PARTY", "MIT"));
        assert!(!names_licence("LICENSE", "MIT"));
        assert!(!names_licence("LICENSE-MIT", "Apache-2.0"));
    }

    #[test]
    fn a_denied_alternatives_own_text_is_dropped() {
        let texts = vec![
            ("LICENSE-MIT", text(&["MIT"])),
            ("LICENSE-GPL", text(&["GPL-2.0-only"])),
        ];
        let component = build("MIT OR GPL-2.0-only", texts).unwrap();
        assert_eq!(component.texts.len(), 1);
    }

    #[test]
    fn accepted_terms_naming_no_licence_are_rejected() {
        let table = r#"{
            "declared_licences": {},
            "copyright_holders": {},
            "accepted_texts": {
                "c": {"COPYING": {"terms": " ", "reason": "why"}}
            }
        }"#;
        assert!(serde_json::from_str::<Table>(table).is_err());
    }
}
