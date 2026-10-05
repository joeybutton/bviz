use bviz_invocation::{Analysing, ConfiguredTarget, Interval};

use crate::model::{CategoryId, Profile};

/// The Bazel `cat` of the span covering a Starlark thread's outermost call,
/// `ProfilerTask.STARLARK_THREAD_CONTEXT`, which is named for what the thread
/// evaluates:
/// <https://github.com/bazelbuild/bazel/blob/9.1.0/src/main/java/com/google/devtools/build/lib/packages/PackageFactory.java#L699-L703>
const THREAD_CONTEXT_CATEGORY: &str = "Starlark thread context";

/// Whether a thread context named `name` is a rule's analysis, which Bazel
/// names by the target's label:
/// <https://github.com/bazelbuild/bazel/blob/9.1.0/src/main/java/com/google/devtools/build/lib/analysis/RuleContext.java#L978-L980>
/// Every other context is named by a phrase ("MODULE.bazel file of …",
/// "repository …", "an initializer", "map_each callback"), which no label
/// spells: a label starts at its repository or package and holds no space.
fn names_a_label(name: &str) -> bool {
    (name.starts_with("//") || name.starts_with('@')) && !name.contains(char::is_whitespace)
}

impl Profile {
    /// When the build was analysing each target whose rule evaluates
    /// Starlark, on the wall clock; `None` for a profile that does not say
    /// when its clock started. Bazel profiles the analysis of a native rule
    /// not at all, and of the others only those lasting long enough unless
    /// asked to record every one. A label is all a context names, so each
    /// target is recorded under no configuration.
    pub fn analysing(&self) -> Option<Analysing> {
        let origin = self.metadata().epoch_origin?;
        let Some(category) = self
            .categories()
            .position(|category| self.str(category) == THREAD_CONTEXT_CATEGORY)
            .and_then(CategoryId::at)
        else {
            return Some(Analysing::default());
        };
        let contexts = || {
            self.threads()
                .iter()
                .flat_map(|track| self.slices(track))
                .filter(move |slice| slice.category == Some(category))
                .map(|slice| (self.str(slice.name), slice))
                .filter(|(name, _)| names_a_label(name))
        };
        let mut analysing = Analysing::with_capacity(contexts().count());
        for (label, slice) in contexts() {
            let during = Interval::new(
                origin.plus_micros(slice.start_us),
                origin.plus_micros(slice.end_us()),
            );
            if let Some(during) = during {
                analysing.record(ConfiguredTarget::new(label.to_owned(), None), during);
            }
        }
        Some(analysing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_invocation::EpochMicros;

    const ORIGIN_MS: i64 = 1_781_439_245_231;

    fn profile(contexts: &[(&str, i64, i64)], origin: bool) -> Profile {
        let events: Vec<String> = contexts
            .iter()
            .map(|(name, ts, dur)| {
                format!(
                    r#"{{"cat":"{THREAD_CONTEXT_CATEGORY}","name":"{name}","ph":"X","ts":{ts},"dur":{dur},"tid":7}}"#
                )
            })
            .collect();
        let other = match origin {
            true => format!(r#""otherData":{{"profile_start_ts":{ORIGIN_MS}}},"#),
            false => String::new(),
        };
        let json = format!(r#"{{{other}"traceEvents":[{}]}}"#, events.join(","));
        Profile::from_bytes(json.as_bytes()).expect("parse")
    }

    fn at(micros: i64) -> EpochMicros {
        EpochMicros::from_millis(ORIGIN_MS).plus_micros(micros)
    }

    #[test]
    fn a_target_is_analysing_throughout_the_contexts_its_label_names() {
        let analysing = profile(
            &[
                ("//t:passes", 100, 50),
                ("MODULE.bazel file of root module", 0, 500),
                ("@@rules_shell+//shell/runfiles:runfiles_impl", 300, 20),
                ("an initializer", 10, 900),
                ("repository @@platforms+host_platform", 10, 900),
                ("//t:passes", 400, 10),
            ],
            true,
        )
        .analysing()
        .expect("on the wall clock");
        let during = |label| analysing.during(ConfiguredTarget::new(label, None));
        assert_eq!(during("//t:passes"), Interval::new(at(100), at(410)));
        assert_eq!(
            during("@@rules_shell+//shell/runfiles:runfiles_impl"),
            Interval::new(at(300), at(320))
        );
        assert_eq!(analysing.iter().count(), 2);
    }

    #[test]
    fn a_profile_off_the_wall_clock_tells_no_analysis() {
        assert!(
            profile(&[("//t:passes", 100, 50)], false)
                .analysing()
                .is_none()
        );
        let none = profile(&[], true).analysing().expect("on the wall clock");
        assert!(none.is_empty());
    }
}
