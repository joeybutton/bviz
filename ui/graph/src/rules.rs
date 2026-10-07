//! The filter rule the class checklist reads its checkmarks from and writes.
//!
//! The control keeps no record of what it has hidden: its checkmarks are read
//! out of the filter's rule list and a toggle rewrites that list, so the control
//! and the drawing cannot come to disagree about what is shown. Everything it
//! hides is held in one rule naming node kinds and nothing else.

use bviz_graph_layout::Kind;
use bviz_graph_layout::select::{Filter, Predicate, Rule, Values, Visibility};
use std::collections::BTreeSet;

/// The node classes `filter` hides through the rule the control writes.
pub(crate) fn hidden(filter: &Filter) -> BTreeSet<Kind> {
    filter
        .rules
        .iter()
        .filter_map(written)
        .flat_map(|kinds| kinds.as_slice().iter().copied())
        .collect()
}

/// `filter` with `kind` hidden, or shown where `hidden` is false.
pub(crate) fn set_hidden(filter: &Filter, kind: Kind, hidden: bool) -> Filter {
    let mut kinds = self::hidden(filter);
    match hidden {
        true => kinds.insert(kind),
        false => kinds.remove(&kind),
    };
    let mut rules: Vec<Rule> = filter
        .rules
        .iter()
        .filter(|rule| !by_control(rule))
        .cloned()
        .collect();
    if let Some(kinds) = Values::new(kinds) {
        rules.push(Rule {
            predicate: Predicate {
                kinds: Some(kinds),
                ..Predicate::default()
            },
            visibility: Visibility::Hidden,
        });
    }
    Filter {
        rules,
        ..filter.clone()
    }
}

/// Whether the class checklist wrote `rule`.
pub(crate) fn by_control(rule: &Rule) -> bool {
    written(rule).is_some()
}

/// The kinds `rule` hides, where it is the one the control writes; `None` for
/// any other rule, which the control may not edit.
fn written(rule: &Rule) -> Option<&Values<Kind>> {
    let kinds = rule.predicate.kinds.as_ref()?;
    let hides_kinds_alone = rule.visibility == Visibility::Hidden
        && rule.predicate
            == Predicate {
                kinds: Some(kinds.clone()),
                ..Predicate::default()
            };
    hides_kinds_alone.then_some(kinds)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unchecking a class writes the rule hiding it, and checking it again takes
    /// that rule back out, so a filter that hides nothing holds no rules at all.
    #[test]
    fn a_class_hidden_and_shown_again_leaves_the_filter_as_it_was() {
        let hiding = set_hidden(&Filter::default(), Kind::Action, true);

        assert_eq!(
            hiding.rules,
            vec![Rule {
                predicate: Predicate {
                    kinds: Values::new([Kind::Action]),
                    ..Predicate::default()
                },
                visibility: Visibility::Hidden,
            }]
        );
        assert_eq!(hidden(&hiding), BTreeSet::from([Kind::Action]));
        assert_eq!(set_hidden(&hiding, Kind::Action, false), Filter::default());
    }

    /// One rule carries every class hidden, so hiding a second widens that rule
    /// rather than laying another beside it.
    #[test]
    fn classes_hidden_gather_into_one_rule() {
        let mut filter = Filter::default();
        for kind in [Kind::Action, Kind::Artifact] {
            filter = set_hidden(&filter, kind, true);
        }

        assert_eq!(filter.rules.len(), 1);
        assert_eq!(
            hidden(&filter),
            BTreeSet::from([Kind::Action, Kind::Artifact])
        );
    }

    /// A rule the control did not write — one over a term it does not offer,
    /// one constraining a second term beside the kind, or one that shows rather
    /// than hides — is neither read as a checkmark nor rewritten by a toggle.
    #[test]
    fn a_rule_no_control_wrote_is_left_alone() {
        let untouched = vec![
            Rule {
                predicate: Predicate {
                    tags: Values::new(["flaky".to_string()]),
                    ..Predicate::default()
                },
                visibility: Visibility::Hidden,
            },
            Rule {
                predicate: Predicate {
                    kinds: Values::new([Kind::Action]),
                    testonly: Some(true),
                    ..Predicate::default()
                },
                visibility: Visibility::Hidden,
            },
            Rule {
                predicate: Predicate {
                    kinds: Values::new([Kind::Source]),
                    ..Predicate::default()
                },
                visibility: Visibility::Faded,
            },
        ];
        let filter = Filter {
            rules: untouched.clone(),
            ..Filter::default()
        };

        assert!(hidden(&filter).is_empty());
        let hiding = set_hidden(&filter, Kind::Package, true);
        assert_eq!(hiding.rules[..untouched.len()], untouched[..]);
        assert_eq!(hiding.rules.len(), untouched.len() + 1);
    }
}
