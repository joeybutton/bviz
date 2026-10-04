//! SPDX licence expressions, as Cargo manifests and licence declarations state
//! them.

use spdx::expression::{ExprNode, Operator};
use spdx::{Expression, LicenseReq, ParseError, ParseMode};

/// Accepts Cargo's deprecated `MIT/Apache-2.0` form and GNU's `+`, but no
/// identifier the SPDX License List lacks, not even one that names a listed
/// licence loosely.
pub fn parse(text: &str) -> Result<Expression, ParseError> {
    let mode = ParseMode {
        allow_imprecise_license_names: false,
        ..ParseMode::LAX
    };
    Expression::parse_mode(text, mode)
}

/// Combines the values of an expression's terms the way its operators join
/// them, each operator seeing its operands in the order the expression states.
fn fold<'e, T>(
    expression: &'e Expression,
    term: impl Fn(&'e LicenseReq) -> T,
    join: impl Fn(Operator, T, T) -> T,
) -> T {
    const BALANCED: &str = "a parsed expression gives every operator two operands";
    let mut stack = Vec::with_capacity(expression.requirements().count());
    for node in expression.iter() {
        let value = match node {
            ExprNode::Req(r) => term(&r.req),
            ExprNode::Op(op) => {
                let right = stack.pop().expect(BALANCED);
                let left = stack.pop().expect(BALANCED);
                join(*op, left, right)
            }
        };
        stack.push(value);
    }
    stack.pop().expect(BALANCED)
}

/// The expression with canonical identifiers and operators, bracketed only
/// where precedence requires.
pub fn canonical(expression: &Expression) -> String {
    alternatives(expression).join(" OR ")
}

/// The expression's top-level `OR` operands, each in canonical form.
pub fn alternatives(expression: &Expression) -> Vec<String> {
    fold(
        expression,
        |req| vec![req.to_string()],
        |op, mut left, right| match op {
            Operator::Or => {
                left.extend(right);
                left
            }
            Operator::And => {
                let operand = |alternatives: Vec<String>| match alternatives.len() {
                    1 => alternatives.join(""),
                    _ => format!("({})", alternatives.join(" OR ")),
                };
                vec![format!("{} AND {}", operand(left), operand(right))]
            }
        },
    )
}

/// The terms the work is taken under, among the alternatives that name no
/// licence `denied` matches: the one taken wholly under licences `preferred`
/// lists whose least preferred licence is listed earliest. Ties, among them
/// the alternatives taken under licences `preferred` does not list, go to the
/// one with fewer terms and then to the one stated first. A licence with an
/// exception or a `+` counts as the licence, since either only grants further
/// permissions. `None` when every alternative names a denied licence.
pub fn elect<'e>(
    expression: &'e Expression,
    denied: impl Fn(&str) -> bool,
    preferred: &[String],
) -> Option<Vec<&'e LicenseReq>> {
    let rank = |req: &LicenseReq| {
        req.license
            .id()
            .and_then(|id| preferred.iter().position(|p| p == id.name))
            .unwrap_or(preferred.len())
    };
    let elected = fold(
        expression,
        |req| (!denied(&req.license.to_string())).then(|| (rank(req), vec![req])),
        |op, left, right| match op {
            Operator::Or => left
                .into_iter()
                .chain(right)
                .min_by_key(|(rank, terms)| (*rank, terms.len())),
            Operator::And => {
                let (left_rank, mut terms) = left?;
                let (right_rank, right_terms) = right?;
                terms.extend(right_terms);
                Some((left_rank.max(right_rank), terms))
            }
        },
    );
    elected.map(|(_, terms)| terms)
}

/// Every licence and exception identifier the terms name, each once, in order
/// of appearance.
pub fn identifiers<'r>(terms: impl IntoIterator<Item = &'r LicenseReq>) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for req in terms {
        let addition = req.addition.as_ref().map(ToString::to_string);
        for id in std::iter::once(req.license.to_string()).chain(addition) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids
}

/// Elected terms as the expression that states them.
pub fn conjunction(terms: &[&LicenseReq]) -> String {
    let texts: Vec<String> = terms.iter().map(ToString::to_string).collect();
    texts.join(" AND ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_form_parses_back_to_itself() {
        for (text, expected) in [
            ("MIT/Apache-2.0", "MIT OR Apache-2.0"),
            ("MIT or Apache-2.0 and Zlib", "MIT OR Apache-2.0 AND Zlib"),
            (
                "(Apache-2.0 WITH LLVM-exception) OR MIT",
                "Apache-2.0 WITH LLVM-exception OR MIT",
            ),
            (
                "MIT AND (Apache-2.0 OR Zlib)",
                "MIT AND (Apache-2.0 OR Zlib)",
            ),
            (
                "(MIT OR Apache-2.0) AND Unicode-3.0",
                "(MIT OR Apache-2.0) AND Unicode-3.0",
            ),
        ] {
            let printed = canonical(&parse(text).unwrap());
            assert_eq!(printed, expected);
            assert_eq!(canonical(&parse(&printed).unwrap()), printed);
        }
    }

    #[test]
    fn alternatives_split_only_the_top_level_disjunction() {
        let split = |text: &str| alternatives(&parse(text).unwrap());
        assert_eq!(split("MIT"), ["MIT"]);
        assert_eq!(
            split("MIT OR Apache-2.0 AND Zlib"),
            ["MIT", "Apache-2.0 AND Zlib"]
        );
        assert_eq!(
            split("(MIT OR Apache-2.0) AND Zlib"),
            ["(MIT OR Apache-2.0) AND Zlib"]
        );
    }

    #[test]
    fn unlisted_identifiers_are_rejected() {
        assert!(parse("MIT OR Nonesuch-1.0").is_err());
        assert!(parse("BSD").is_err());
        assert!(parse("Apache").is_err());
        assert!(parse("(MIT OR Apache-2.0").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn election_takes_the_first_alternative_that_avoids_denial() {
        let gpl = |id: &str| id.starts_with("GPL-");
        let elected = |text: &str| {
            let expression = parse(text).unwrap();
            elect(&expression, gpl, &[]).map(|terms| conjunction(&terms))
        };
        assert_eq!(
            elected("Apache-2.0 OR GPL-2.0-only").as_deref(),
            Some("Apache-2.0")
        );
        assert_eq!(
            elected("GPL-2.0-only OR MIT OR Zlib").as_deref(),
            Some("MIT")
        );
        assert_eq!(
            elected("MIT AND (GPL-2.0-only OR BSD-3-Clause)").as_deref(),
            Some("MIT AND BSD-3-Clause")
        );
        assert_eq!(elected("MIT AND GPL-3.0-or-later"), None);
    }

    #[test]
    fn election_prefers_alternatives_wholly_under_preferred_licences() {
        let gpl = |id: &str| id.starts_with("GPL-");
        let preferred = ["Apache-2.0".to_owned(), "MIT".to_owned()];
        let elected = |text: &str| {
            let expression = parse(text).unwrap();
            elect(&expression, gpl, &preferred).map(|terms| conjunction(&terms))
        };
        assert_eq!(elected("MIT OR Apache-2.0").as_deref(), Some("Apache-2.0"));
        assert_eq!(elected("Zlib OR MIT").as_deref(), Some("MIT"));
        assert_eq!(
            elected("MIT OR Apache-2.0 WITH LLVM-exception").as_deref(),
            Some("Apache-2.0 WITH LLVM-exception")
        );
        assert_eq!(
            elected("MIT OR Apache-2.0 AND Zlib").as_deref(),
            Some("MIT")
        );
        assert_eq!(elected("Zlib OR BSD-3-Clause").as_deref(), Some("Zlib"));
        assert_eq!(
            elected("GPL-2.0-only OR Apache-2.0 OR MIT").as_deref(),
            Some("Apache-2.0")
        );
        assert_eq!(
            elected("(MIT OR Apache-2.0) AND Unicode-3.0").as_deref(),
            Some("Apache-2.0 AND Unicode-3.0")
        );
        assert_eq!(elected("Apache-2.0 AND MIT OR MIT").as_deref(), Some("MIT"));
        assert_eq!(
            elected("MIT OR Apache-2.0+").as_deref(),
            Some("Apache-2.0+")
        );
        assert_eq!(
            elected("Apache-2.0 AND GPL-3.0-or-later OR Zlib").as_deref(),
            Some("Zlib")
        );
    }

    #[test]
    fn identifiers_name_exceptions_once() {
        let expression = parse("(Apache-2.0 WITH LLVM-exception) OR Apache-2.0 OR MIT").unwrap();
        let terms = expression.requirements().map(|r| &r.req);
        assert_eq!(identifiers(terms), ["Apache-2.0", "LLVM-exception", "MIT"]);
    }
}
