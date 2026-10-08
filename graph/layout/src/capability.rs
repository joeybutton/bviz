//! Which filter axes a set of inputs actually supports.
//!
//! The Bazel queries run outside bviz and their output arrives as files, so an
//! axis may be missing because a flag was not passed rather than because the
//! build has nothing to say. Each axis is therefore read off the bytes: every
//! node the axis applies to is counted, and so is every one whose description
//! states the field the axis reads. An axis no description states is reported
//! unavailable only where a Bazel flag governs whether that field is written.
//! A field written and empty everywhere — an action list of aspect descriptors
//! for a build that runs no aspect action — is support with no values to offer.

use bviz_bazel_flags::{
    CONSISTENT_LABELS, Flag, INCLUDE_ARTIFACTS, OUTPUT_RULE_ATTRS, TRANSITIONS,
};

/// A property a query selects nodes by.
///
/// The discriminants index [`Axis::ALL`], which lists them in that same order.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Axis {
    Repo,
    Package,
    Label,
    RuleClass,
    RuleCategory,
    /// The configuration a node is built in.
    Configuration,
    /// Whether a dependency is consumed in an exec configuration, which a
    /// dependency edge carries only where it names the configuration that
    /// dependency is built in. The configuration a node is itself built in is
    /// [`Axis::Configuration`].
    ExecConfiguration,
    Mnemonic,
    Aspect,
    Tags,
    Testonly,
    /// The files an action reads and writes.
    Artifacts,
}

impl Axis {
    pub const ALL: [Axis; 12] = [
        Axis::Repo,
        Axis::Package,
        Axis::Label,
        Axis::RuleClass,
        Axis::RuleCategory,
        Axis::Configuration,
        Axis::ExecConfiguration,
        Axis::Mnemonic,
        Axis::Aspect,
        Axis::Tags,
        Axis::Testonly,
        Axis::Artifacts,
    ];

    /// The Bazel query flag that makes an input carry this axis, where one
    /// governs it. An axis every output states regardless names none, so it is
    /// never reported as something a flag could restore.
    pub fn flag(self) -> Option<Flag> {
        match self {
            // query and cquery print apparent repository names unless asked for
            // canonical ones, and only a canonical name states where its
            // repository comes from.
            Axis::Repo => Some(CONSISTENT_LABELS),
            Axis::Package | Axis::Label | Axis::Mnemonic | Axis::Aspect => None,
            // Every query writes the class a rule was declared by whatever else
            // it is asked for.
            Axis::RuleClass | Axis::RuleCategory => None,
            // A cquery asked for no configurations writes query's message type
            // instead, which is no longer cquery output at all.
            Axis::Configuration => None,
            Axis::ExecConfiguration => Some(TRANSITIONS),
            Axis::Tags | Axis::Testonly => Some(OUTPUT_RULE_ATTRS),
            Axis::Artifacts => Some(INCLUDE_ARTIFACTS),
        }
    }

    fn support(self, counts: Counts) -> Support {
        let Counts { covered, relevant } = counts;
        if covered == relevant {
            return Support::Available;
        }
        match (covered, self.flag()) {
            (0, Some(flag)) => Support::Unavailable { flag },
            _ => Support::Partial {
                covered,
                relevant,
                flag: self.flag(),
            },
        }
    }
}

/// What the inputs let a filter do on one axis.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Support {
    /// Every description of a node the axis applies to states the field it
    /// reads, of which there may be no values to read.
    Available,
    Partial {
        covered: usize,
        relevant: usize,
        flag: Option<Flag>,
    },
    Unavailable {
        flag: Flag,
    },
}

/// How far each axis reaches across one set of inputs.
#[derive(Clone)]
pub struct Capabilities {
    support: [Support; Axis::ALL.len()],
}

impl Capabilities {
    pub fn support(&self, axis: Axis) -> Support {
        self.support[axis as usize]
    }

    pub fn axes(&self) -> impl Iterator<Item = (Axis, Support)> {
        Axis::ALL.into_iter().map(|axis| (axis, self.support(axis)))
    }
}

/// Counts, while the inputs are read, how far each axis reaches.
#[derive(Default)]
pub struct CapabilitiesBuilder {
    counts: [Counts; Axis::ALL.len()],
}

/// One axis's tally. Both count the descriptions the inputs make rather than
/// the nodes they describe, so a node several inputs describe is counted once
/// per input; the share they form is what an axis's support is read from.
#[derive(Clone, Copy, Default)]
struct Counts {
    covered: usize,
    relevant: usize,
}

impl CapabilitiesBuilder {
    /// Records one description of a node `axis` applies to. `carried` says
    /// whether that description stated the field the axis reads, which for a
    /// repeated field is whether the message holding it was written at all and
    /// not whether the list holds a value.
    pub fn observe(&mut self, axis: Axis, carried: bool) {
        let counts = &mut self.counts[axis as usize];
        counts.relevant += 1;
        counts.covered += usize::from(carried);
    }

    pub fn seal(self) -> Capabilities {
        Capabilities {
            support: std::array::from_fn(|axis| Axis::ALL[axis].support(self.counts[axis])),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The support `covered` of `relevant` descriptions yields on `axis`.
    fn support(axis: Axis, covered: usize, relevant: usize) -> Support {
        let mut builder = CapabilitiesBuilder::default();
        for i in 0..relevant {
            builder.observe(axis, i < covered);
        }
        builder.seal().support(axis)
    }

    #[test]
    fn each_axis_sits_at_the_position_its_discriminant_names() {
        for (position, axis) in Axis::ALL.into_iter().enumerate() {
            assert_eq!(axis as usize, position, "{axis:?}");
        }
    }

    #[test]
    fn an_axis_every_description_states_is_available() {
        assert_eq!(support(Axis::Tags, 4, 4), Support::Available);
    }

    #[test]
    fn an_axis_no_description_applies_to_is_available_with_nothing_to_offer() {
        // No input named an action, so nothing is missing from the mnemonics.
        assert_eq!(support(Axis::Mnemonic, 0, 0), Support::Available);
        assert_eq!(support(Axis::Artifacts, 0, 0), Support::Available);
    }

    #[test]
    fn an_axis_a_flag_governs_and_nothing_states_is_unavailable() {
        assert_eq!(
            support(Axis::Repo, 0, 7),
            Support::Unavailable {
                flag: CONSISTENT_LABELS
            }
        );
    }

    #[test]
    fn an_axis_every_output_states_offers_no_flag() {
        for axis in [Axis::RuleClass, Axis::RuleCategory, Axis::Configuration] {
            assert_eq!(axis.flag(), None, "{axis:?}");
        }
    }

    #[test]
    fn an_axis_no_flag_governs_is_never_reported_unavailable() {
        // Nothing a flag could add is missing, so the gap is reported as the
        // partial coverage it is rather than as a flag to pass.
        assert_eq!(
            support(Axis::Package, 0, 3),
            Support::Partial {
                covered: 0,
                relevant: 3,
                flag: None
            }
        );
    }

    #[test]
    fn a_partly_stated_axis_counts_the_descriptions_that_state_it() {
        assert_eq!(
            support(Axis::Testonly, 2, 5),
            Support::Partial {
                covered: 2,
                relevant: 5,
                flag: Some(OUTPUT_RULE_ATTRS)
            }
        );
    }

    #[test]
    fn every_axis_is_summarized() {
        let summary = CapabilitiesBuilder::default().seal();

        let axes: Vec<Axis> = summary.axes().map(|(axis, _)| axis).collect();
        assert_eq!(axes, Axis::ALL);
    }
}
