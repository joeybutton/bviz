//! The filter rules the toolbar's controls read their state from and write.
//!
//! A control keeps no record of what it has hidden: its checkmarks are read out
//! of the filter's rule list and a toggle rewrites that list, so a control and
//! the drawing cannot come to disagree about what is shown.
//!
//! Every rule written here hides, and constrains exactly one [`Term`]. A rule
//! naming two terms would hide only the nodes answering both, which no control
//! offers; and rules that only ever hide compose whatever order they are read
//! in, so one control's rule can never widen what another's hides.

use bviz_graph_layout::Kind;
use bviz_graph_layout::facets::RuleCategory;
use bviz_graph_layout::repo::{RepoCategory, RepoName};
use bviz_graph_layout::select::{Filter, Predicate, Rule, Values, Visibility};
use std::collections::BTreeSet;

/// A predicate term the toolbar hides values on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Term {
    Kind,
    RepoCategory,
    Repo,
    RuleCategory,
    Mnemonic,
    ToolConfiguration,
}

impl Term {
    const ALL: [Term; 6] = [
        Term::Kind,
        Term::RepoCategory,
        Term::Repo,
        Term::RuleCategory,
        Term::Mnemonic,
        Term::ToolConfiguration,
    ];
}

/// One value of a [`Term`], as a control offers it.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum Value {
    Kind(Kind),
    RepoCategory(RepoCategory),
    /// A repository by the name identifying it.
    Repo(RepoName<String>),
    RuleCategory(RuleCategory),
    /// An action by its mnemonic.
    Mnemonic(String),
    /// Every configuration built for a tool the build runs rather than for an
    /// artifact it produces.
    ToolConfiguration,
}

impl Value {
    pub(crate) fn term(&self) -> Term {
        match self {
            Value::Kind(_) => Term::Kind,
            Value::RepoCategory(_) => Term::RepoCategory,
            Value::Repo(_) => Term::Repo,
            Value::RuleCategory(_) => Term::RuleCategory,
            Value::Mnemonic(_) => Term::Mnemonic,
            Value::ToolConfiguration => Term::ToolConfiguration,
        }
    }
}

/// The values `filter` hides on `term`.
pub(crate) fn hidden(filter: &Filter, term: Term) -> BTreeSet<Value> {
    filter
        .rules
        .iter()
        .filter_map(written)
        .filter(|(written, _)| *written == term)
        .flat_map(|(_, values)| values)
        .collect()
}

/// Whether `filter` hides `value`.
pub(crate) fn hides(filter: &Filter, value: &Value) -> bool {
    hidden(filter, value.term()).contains(value)
}

/// `filter` with `value` hidden, or shown where `hidden` is false.
pub(crate) fn set_hidden(filter: &Filter, value: &Value, hidden: bool) -> Filter {
    let term = value.term();
    let mut values = self::hidden(filter, term);
    match hidden {
        true => values.insert(value.clone()),
        false => values.remove(value),
    };
    hiding(filter, term, values)
}

/// `filter` with everything it hid on `term` replaced by `values`, which is what
/// holds a term to one rule however many changes it has been through.
pub(crate) fn hiding(filter: &Filter, term: Term, values: BTreeSet<Value>) -> Filter {
    let values: Vec<Value> = values.into_iter().collect();
    let mut rules: Vec<Rule> = filter
        .rules
        .iter()
        .filter(|rule| written(rule).is_none_or(|(written, _)| written != term))
        .cloned()
        .collect();
    if let Some(predicate) = predicate(term, &values) {
        rules.push(Rule {
            predicate,
            visibility: Visibility::Hidden,
        });
    }
    Filter {
        rules,
        ..filter.clone()
    }
}

/// The term whose control wrote `rule`; `None` for a rule no control wrote.
pub(crate) fn control(rule: &Rule) -> Option<Term> {
    written(rule).map(|(term, _)| term)
}

/// The term and values `rule` hides, where it is one a control wrote; `None` for
/// any other rule, which no control may edit.
fn written(rule: &Rule) -> Option<(Term, Vec<Value>)> {
    if rule.visibility != Visibility::Hidden {
        return None;
    }
    let mut read = Term::ALL
        .into_iter()
        .filter_map(|term| named(&rule.predicate, term).map(|values| (term, values)));
    let (term, values) = read.next()?;
    if read.next().is_some() {
        return None;
    }
    // A predicate constraining anything besides the one term differs from the
    // one that term's values alone build.
    (predicate(term, &values).as_ref() == Some(&rule.predicate)).then_some((term, values))
}

/// The values `predicate` names on `term`; `None` where it leaves the term unset,
/// or sets it to something no control writes.
fn named(predicate: &Predicate, term: Term) -> Option<Vec<Value>> {
    fn listed<T: Clone>(values: &Values<T>, of: fn(T) -> Value) -> Vec<Value> {
        values.as_slice().iter().cloned().map(of).collect()
    }
    Some(match term {
        Term::Kind => listed(predicate.kinds.as_ref()?, Value::Kind),
        Term::RepoCategory => listed(predicate.repo_categories.as_ref()?, Value::RepoCategory),
        Term::Repo => listed(predicate.repos.as_ref()?, Value::Repo),
        Term::RuleCategory => listed(predicate.rule_categories.as_ref()?, Value::RuleCategory),
        Term::Mnemonic => listed(predicate.mnemonics.as_ref()?, Value::Mnemonic),
        // A term over the configurations that are not a tool's names none of
        // the values a control offers.
        Term::ToolConfiguration => match predicate.is_tool? {
            true => vec![Value::ToolConfiguration],
            false => return None,
        },
    })
}

/// The predicate matching the nodes carrying any of `values`, all of which must
/// be of `term`; `None` where they are empty, which constrains nothing.
fn predicate(term: Term, values: &[Value]) -> Option<Predicate> {
    fn listed<T: Ord>(values: &[Value], of: impl FnMut(&Value) -> Option<T>) -> Option<Values<T>> {
        Values::new(values.iter().filter_map(of))
    }
    let predicate = match term {
        Term::Kind => Predicate {
            kinds: listed(values, |value| match value {
                Value::Kind(kind) => Some(*kind),
                _ => None,
            }),
            ..Predicate::default()
        },
        Term::RepoCategory => Predicate {
            repo_categories: listed(values, |value| match value {
                Value::RepoCategory(category) => Some(*category),
                _ => None,
            }),
            ..Predicate::default()
        },
        Term::Repo => Predicate {
            repos: listed(values, |value| match value {
                Value::Repo(name) => Some(name.clone()),
                _ => None,
            }),
            ..Predicate::default()
        },
        Term::RuleCategory => Predicate {
            rule_categories: listed(values, |value| match value {
                Value::RuleCategory(category) => Some(*category),
                _ => None,
            }),
            ..Predicate::default()
        },
        Term::Mnemonic => Predicate {
            mnemonics: listed(values, |value| match value {
                Value::Mnemonic(name) => Some(name.clone()),
                _ => None,
            }),
            ..Predicate::default()
        },
        Term::ToolConfiguration => Predicate {
            is_tool: values.contains(&Value::ToolConfiguration).then_some(true),
            ..Predicate::default()
        },
    };
    (predicate != Predicate::default()).then_some(predicate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(filter: &Filter, term: Term) -> Vec<Value> {
        hidden(filter, term).into_iter().collect()
    }

    /// Unchecking a value writes the rule hiding it, and checking it again takes
    /// that rule back out, so a filter that hides nothing holds no rules at all.
    #[test]
    fn a_value_hidden_and_shown_again_leaves_the_filter_as_it_was() {
        let action = Value::Kind(Kind::Action);
        let hiding = set_hidden(&Filter::default(), &action, true);

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
        assert!(hides(&hiding, &action));
        assert_eq!(set_hidden(&hiding, &action, false), Filter::default());
    }

    /// One rule carries everything hidden on a term, so hiding a second value
    /// widens that rule rather than laying another beside it.
    #[test]
    fn values_hidden_on_one_term_gather_into_one_rule() {
        let mut filter = Filter::default();
        for kind in [Kind::Action, Kind::Artifact] {
            filter = set_hidden(&filter, &Value::Kind(kind), true);
        }

        assert_eq!(filter.rules.len(), 1);
        assert_eq!(
            values(&filter, Term::Kind),
            [Value::Kind(Kind::Action), Value::Kind(Kind::Artifact)]
        );
    }

    /// Each term keeps its own rule: a rule naming two would hide only what
    /// answers both terms.
    #[test]
    fn a_rule_is_written_over_one_term_alone() {
        let filter = set_hidden(&Filter::default(), &Value::Kind(Kind::Action), true);
        let filter = set_hidden(
            &filter,
            &Value::Repo(RepoName::Canonical("rules_foo+".into())),
            true,
        );

        assert_eq!(filter.rules.len(), 2);
        assert_eq!(values(&filter, Term::Kind), [Value::Kind(Kind::Action)]);
        assert_eq!(
            values(&filter, Term::Repo),
            [Value::Repo(RepoName::Canonical("rules_foo+".into()))]
        );
    }

    /// Every term round-trips through the predicate it writes, so a control
    /// reads back exactly the values the last toggle left.
    #[test]
    fn every_term_reads_back_the_values_it_was_written_with() {
        let cases = [
            Value::Kind(Kind::Package),
            Value::RepoCategory(RepoCategory::Module),
            Value::Repo(RepoName::Canonical("rules_foo+".into())),
            Value::RuleCategory(RuleCategory::Plumbing),
            Value::Mnemonic("SymlinkTree".into()),
            Value::ToolConfiguration,
        ];

        for value in cases {
            let filter = set_hidden(&Filter::default(), &value, true);
            assert_eq!(
                values(&filter, value.term()),
                std::slice::from_ref(&value),
                "{value:?}"
            );
            assert_eq!(
                set_hidden(&filter, &value, false),
                Filter::default(),
                "{value:?}"
            );
        }
    }

    /// A rule the controls did not write — one over a term none of them offers,
    /// one constraining two terms at once, or one that shows rather than hides —
    /// is neither read as a checkmark nor rewritten by a toggle.
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

        assert!(hidden(&filter, Term::Kind).is_empty());
        let hiding = set_hidden(&filter, &Value::Kind(Kind::Package), true);
        assert_eq!(hiding.rules[..untouched.len()], untouched[..]);
        assert_eq!(hiding.rules.len(), untouched.len() + 1);
    }
}
