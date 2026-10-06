use crate::{RUNFILES_MNEMONIC, RecordedStep};
use bviz_trace_profile::{Action, Profile};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

/// The action spans a critical path's steps may have run as, each claimable
/// by one step only.
pub(crate) struct Spans<'a> {
    actions: &'a [Action],
    /// Positions in `actions`, in the order the spans started, by the text a
    /// step can be joined on.
    by_primary_output: HashMap<&'a str, Vec<usize>>,
    by_runfiles_owner: HashMap<&'a str, Vec<usize>>,
    by_description: HashMap<&'a str, Vec<usize>>,
    claimed: Vec<bool>,
}

impl<'a> Spans<'a> {
    /// The spans of `actions`, in the order they started, that one of
    /// `steps` can be joined to.
    pub(crate) fn new(profile: &'a Profile, actions: &'a [Action], steps: &[RecordedStep]) -> Self {
        let mut primary_outputs = HashSet::with_capacity(steps.len());
        let mut runfiles_owners = HashSet::with_capacity(steps.len());
        let mut descriptions: HashSet<Cow<str>> = HashSet::with_capacity(steps.len());
        for step in steps {
            primary_outputs.extend(step.primary_output);
            runfiles_owners.extend(step.runfiles_label());
            descriptions.extend(step.span_description());
        }
        let mut by_primary_output: HashMap<&str, Vec<usize>> =
            HashMap::with_capacity(primary_outputs.len());
        let mut by_runfiles_owner: HashMap<&str, Vec<usize>> =
            HashMap::with_capacity(runfiles_owners.len());
        let mut by_description: HashMap<&str, Vec<usize>> =
            HashMap::with_capacity(descriptions.len());
        for (i, action) in actions.iter().enumerate() {
            if let Some(out) = profile.opt_str(action.out)
                && primary_outputs.contains(out)
            {
                by_primary_output.entry(out).or_default().push(i);
            }
            if profile.opt_str(action.mnemonic) == Some(RUNFILES_MNEMONIC)
                && let Some(owner) = profile.opt_str(action.target)
                && runfiles_owners.contains(owner)
            {
                by_runfiles_owner.entry(owner).or_default().push(i);
            }
            let description = profile.str(action.description);
            if descriptions.contains(description) {
                by_description.entry(description).or_default().push(i);
            }
        }
        Spans {
            actions,
            by_primary_output,
            by_runfiles_owner,
            by_description,
            claimed: vec![false; actions.len()],
        }
    }

    /// Claims the span of `step`: the first unclaimed one starting within
    /// the step that writes its primary output, or failing that, builds the
    /// runfiles tree of its target, or failing that, carries its description.
    /// A span starts a little after its step does, so the start tells apart
    /// actions of one description that ran at different times.
    pub(crate) fn claim(&mut self, step: &RecordedStep) -> Option<&'a Action> {
        let description = step.span_description();
        let found = [
            step.primary_output
                .and_then(|out| self.by_primary_output.get(out)),
            step.runfiles_label()
                .and_then(|owner| self.by_runfiles_owner.get(owner)),
            description
                .as_deref()
                .and_then(|description| self.by_description.get(description)),
        ]
        .into_iter()
        .flatten()
        .find_map(|positions| {
            positions.iter().copied().find(|&i| {
                !self.claimed[i]
                    && (step.start_us..=step.end_us).contains(&self.actions[i].span.start_us)
            })
        })?;
        self.claimed[found] = true;
        Some(&self.actions[found])
    }
}
