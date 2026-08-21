//! Test-only builders that assemble a [`Profile`] through the real Chrome-trace
//! parser, so tests exercise the same model the UI reads.

use bviz_trace_profile::Profile;

/// Parses a profile of threads named `(thread name, slice count)` through the
/// real Chrome-trace parser, which derives each thread's lane class from its
/// name. A thread needs a slice to reach the model, so a count of zero yields
/// no track.
pub(crate) fn profile_of(threads: &[(String, usize)]) -> Profile {
    let mut events = Vec::new();
    for (i, (name, slices)) in threads.iter().enumerate() {
        // tid 0 is Bazel's synthetic critical-path thread, whatever its name.
        let tid = i + 1;
        events.push(format!(
            r#"{{"name":"thread_name","ph":"M","tid":{tid},"args":{{"name":"{name}"}}}}"#
        ));
        for ts in 0..*slices {
            events.push(format!(
                r#"{{"cat":"general information","name":"s","ph":"X","ts":{ts},"dur":1,"tid":{tid}}}"#
            ));
        }
    }
    let json = format!(r#"{{"traceEvents":[{}]}}"#, events.join(","));
    Profile::from_bytes(json.as_bytes()).expect("parse test profile")
}
