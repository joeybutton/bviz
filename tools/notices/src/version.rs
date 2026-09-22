//! Package version strings, as Cargo manifests and purls state them.

use std::fmt;

/// A semantic version where the text, less a leading `v`, is one, ordered by
/// precedence ahead of every version that is not, which orders as text. Either
/// is shown as written.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Version {
    Semantic(semver::Version, String),
    Other(String),
}

impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let unprefixed = text.strip_prefix('v').unwrap_or(text);
        (!text.is_empty()).then(|| match semver::Version::parse(unprefixed) {
            Ok(version) => Version::Semantic(version, text.to_owned()),
            Err(_) => Version::Other(text.to_owned()),
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Version::Semantic(_, text) | Version::Other(text) => f.write_str(text),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn blank_versions_are_not_versions() {
        assert_eq!(Version::parse(""), None);
        assert_eq!(Version::parse("  "), None);
    }

    #[test]
    fn semantic_versions_order_by_precedence() {
        assert!(version("0.9.0") < version("0.10.0"));
        assert!(version("1.0.0-rc1") < version("1.0.0"));
        assert!(version("v0.11.0+rs.2") < version("v0.11.0+rs.10"));
        assert!(version("v0.11.0+rs.2") < version("v0.12.0"));
        assert_eq!(version("v0.11.0+rs.2").to_string(), "v0.11.0+rs.2");
        assert!(version("1.0.0") < version("v4.1"));
    }
}
