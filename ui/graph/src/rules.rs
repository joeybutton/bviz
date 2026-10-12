//! The filter rule the boxes menu reads its checkmarks from and writes.
//!
//! The control keeps no record of what it has hidden: its checkmarks are read
//! out of the panel's rule list and a toggle rewrites that list, so the control
//! and the drawing cannot come to disagree about what is shown. Everything it
//! hides is held in one rule naming box classes and nothing else.

use bviz_graph_layout::Kind;
use bviz_graph_layout::select::{Predicate, Rule, Values, Visibility};
use std::collections::BTreeSet;

/// A kind of node the drawing puts a box round what the node holds. Hiding
/// the class unwraps each such box, lifting what it holds to the nearest box
/// still drawn, and leaves a node of the kind that holds nothing as it is.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum BoxClass {
    Package,
    Target,
    /// A generated file under a label of its own, holding the configured
    /// targets it is built in.
    LabeledOutput,
    ConfiguredTarget,
}

impl BoxClass {
    /// The class of a box of `kind`; `None` for a kind that holds nothing in
    /// the graph a filter is evaluated over.
    pub(crate) fn of(kind: Kind) -> Option<Self> {
        match kind {
            Kind::Package => Some(BoxClass::Package),
            Kind::Target => Some(BoxClass::Target),
            Kind::Artifact => Some(BoxClass::LabeledOutput),
            Kind::ConfiguredTarget => Some(BoxClass::ConfiguredTarget),
            Kind::TargetGroup | Kind::MergeGroup | Kind::Action | Kind::Source => None,
        }
    }

    pub(crate) fn kind(self) -> Kind {
        match self {
            BoxClass::Package => Kind::Package,
            BoxClass::Target => Kind::Target,
            BoxClass::LabeledOutput => Kind::Artifact,
            BoxClass::ConfiguredTarget => Kind::ConfiguredTarget,
        }
    }
}

/// The box classes `rules` hide through the rule the control writes.
pub(crate) fn hidden(rules: &[Rule]) -> BTreeSet<BoxClass> {
    rules.iter().filter_map(written).flatten().collect()
}

/// `rules` with `class` hidden, or shown where `hidden` is false.
pub(crate) fn set_hidden(rules: &[Rule], class: BoxClass, hidden: bool) -> Vec<Rule> {
    let mut classes = self::hidden(rules);
    match hidden {
        true => classes.insert(class),
        false => classes.remove(&class),
    };
    let mut rules: Vec<Rule> = rules
        .iter()
        .filter(|rule| !by_control(rule))
        .cloned()
        .collect();
    if let Some(kinds) = Values::new(classes.into_iter().map(BoxClass::kind)) {
        rules.push(Rule {
            predicate: Predicate {
                kinds: Some(kinds),
                holding: true,
            },
            visibility: Visibility::Hidden,
        });
    }
    rules
}

/// Whether the boxes menu wrote `rule`.
pub(crate) fn by_control(rule: &Rule) -> bool {
    written(rule).is_some()
}

/// The box classes `rule` hides, where it is the one the control writes;
/// `None` for any other rule, which the control may not edit.
fn written(rule: &Rule) -> Option<Vec<BoxClass>> {
    let kinds = rule.predicate.kinds.as_ref()?;
    let hides_kinds_alone = rule.visibility == Visibility::Hidden
        && rule.predicate
            == Predicate {
                kinds: Some(kinds.clone()),
                holding: true,
            };
    if !hides_kinds_alone {
        return None;
    }
    kinds
        .as_slice()
        .iter()
        .map(|&kind| BoxClass::of(kind))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unchecking a class writes the rule hiding it, and checking it again takes
    /// that rule back out, so rules that hide nothing are no rules at all.
    #[test]
    fn a_class_hidden_and_shown_again_leaves_the_rules_as_they_were() {
        let hiding = set_hidden(&[], BoxClass::Target, true);

        assert_eq!(
            hiding,
            vec![Rule {
                predicate: Predicate {
                    kinds: Values::new([Kind::Target]),
                    holding: true,
                },
                visibility: Visibility::Hidden,
            }]
        );
        assert_eq!(hidden(&hiding), BTreeSet::from([BoxClass::Target]));
        assert_eq!(set_hidden(&hiding, BoxClass::Target, false), []);
    }

    /// One rule carries every class hidden, so hiding a second widens that rule
    /// rather than laying another beside it.
    #[test]
    fn classes_hidden_gather_into_one_rule() {
        let mut rules = Vec::new();
        for class in [BoxClass::Package, BoxClass::Target] {
            rules = set_hidden(&rules, class, true);
        }

        assert_eq!(rules.len(), 1);
        assert_eq!(
            hidden(&rules),
            BTreeSet::from([BoxClass::Package, BoxClass::Target])
        );
    }

    /// A rule the control did not write — one over every kind, one naming a
    /// kind of leaf, one reaching the nodes of its kinds that hold nothing, or
    /// one that shows rather than hides — is neither read as a checkmark nor
    /// rewritten by a toggle.
    #[test]
    fn a_rule_no_control_wrote_is_left_alone() {
        let untouched = vec![
            Rule {
                predicate: Predicate {
                    holding: true,
                    ..Predicate::default()
                },
                visibility: Visibility::Hidden,
            },
            Rule {
                predicate: Predicate {
                    kinds: Values::new([Kind::Package, Kind::Action]),
                    holding: true,
                },
                visibility: Visibility::Hidden,
            },
            Rule {
                predicate: Predicate {
                    kinds: Values::new([Kind::Target]),
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
        assert!(hidden(&untouched).is_empty());
        let hiding = set_hidden(&untouched, BoxClass::Target, true);
        assert_eq!(hiding[..untouched.len()], untouched[..]);
        assert_eq!(hiding.len(), untouched.len() + 1);
    }
}
