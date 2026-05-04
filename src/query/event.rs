//! Event-source resolution: maps a user-facing `event` string to an
//! [`EventSource`]. The iterator implementation lives on [`Profile::stack_indices`];
//! this module owns the string-to-source mapping and the "did you mean"
//! suggestions so that path can stay profile-aware.

#![allow(dead_code)]

pub use crate::profile::EventSource;

use crate::error::ToolError;
use crate::profile::Profile;

/// Resolve a user-facing event string to an [`EventSource`].
///
/// * `None` or empty string → [`EventSource::Samples`] (the default).
/// * Any non-empty string → looks up a stack-bearing marker with that
///   name in any thread of the profile; returns [`ToolError::Internal`]
///   with the list of available marker names if the lookup fails.
///
/// We do not treat `"cycles"` or `"samples"` as aliases — samply does
/// not emit a marker by either name, so the user must omit the arg to
/// get the samples track. The error message lists known events, so
/// even an unaware caller gets pointed at the right vocabulary on the
/// first miss.
pub fn resolve(profile: &Profile, event: Option<&str>) -> Result<EventSource, ToolError> {
    let raw = match event {
        None | Some("") => return Ok(EventSource::Samples),
        Some(s) => s,
    };
    if marker_event_exists(profile, raw) {
        Ok(EventSource::Marker(raw.to_owned()))
    } else {
        Err(ToolError::Internal {
            message: format!(
                "unknown event {raw:?}; known marker events: {known:?} (omit `event` for the default samples track)",
                known = known_marker_events(profile),
            ),
        })
    }
}

/// True if at least one marker entry has `name == target` AND a
/// `cause.stack` payload. The stack-bearing requirement filters out
/// text-only markers (e.g. `mmap` annotations) that have nothing to
/// aggregate.
fn marker_event_exists(profile: &Profile, target: &str) -> bool {
    for thread in profile.threads() {
        let raw = thread.raw();
        for (i, &str_idx) in raw.markers.name.iter().enumerate() {
            if raw.string_array.get(str_idx).map(String::as_str) != Some(target) {
                continue;
            }
            let has_stack = raw
                .markers
                .data
                .get(i)
                .and_then(|d| d.as_ref())
                .and_then(|d| d.cause.as_ref())
                .is_some();
            if has_stack {
                return true;
            }
        }
    }
    false
}

/// Sorted list of distinct marker names that have at least one
/// stack-bearing entry. Used to populate "did you mean?" suggestions
/// when `resolve` rejects an unknown event.
fn known_marker_events(profile: &Profile) -> Vec<String> {
    let mut names: std::collections::BTreeSet<String> = Default::default();
    for thread in profile.threads() {
        let raw = thread.raw();
        for (i, &str_idx) in raw.markers.name.iter().enumerate() {
            let has_stack = raw
                .markers
                .data
                .get(i)
                .and_then(|d| d.as_ref())
                .and_then(|d| d.cause.as_ref())
                .is_some();
            if !has_stack {
                continue;
            }
            if let Some(s) = raw.string_array.get(str_idx) {
                names.insert(s.clone());
            }
        }
    }
    names.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::raw::RawProfile;

    fn fixture() -> Profile {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_events.json")).unwrap();
        Profile::from_raw(raw)
    }

    #[test]
    fn default_resolves_to_samples() {
        let p = fixture();
        assert_eq!(resolve(&p, None).unwrap(), EventSource::Samples);
        assert_eq!(resolve(&p, Some("")).unwrap(), EventSource::Samples);
    }

    #[test]
    fn known_marker_resolves() {
        let p = fixture();
        let s = resolve(&p, Some("cache-misses")).unwrap();
        assert_eq!(s, EventSource::Marker("cache-misses".into()));
        assert!(!s.is_time_shaped());
    }

    #[test]
    fn unknown_event_errors_with_suggestions() {
        let p = fixture();
        let err = resolve(&p, Some("not-a-real-event")).unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("not-a-real-event"), "{msg}");
        assert!(msg.contains("cache-misses"), "{msg}");
    }

    #[test]
    fn samples_iter_yields_every_sample_stack() {
        let p = fixture();
        let handle = p.threads().next().unwrap().handle();
        let n: usize = p.stack_indices(handle, &EventSource::Samples, None).count();
        assert_eq!(n, p.raw_thread(handle).samples.stack.len());
    }

    #[test]
    fn marker_iter_filters_to_named_event() {
        let p = fixture();
        let handle = p.threads().next().unwrap().handle();
        let stacks: Vec<_> = p
            .stack_indices(handle, &EventSource::Marker("cache-misses".into()), None)
            .collect();
        assert_eq!(stacks.len(), 2);
        assert!(stacks.iter().all(|s| s.is_some()));
    }

    #[test]
    fn samples_iter_gates_by_time_range() {
        // linear_chain.json has 100 samples with 1ms cadence (t = 0..99).
        // A [10, 19] window must yield exactly 10 stacks; an out-of-range
        // window must yield 0.
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/linear_chain.json")).unwrap();
        let p = Profile::from_raw(raw);
        let handle = p.threads().next().unwrap().handle();
        let in_window: Vec<_> = p
            .stack_indices(handle, &EventSource::Samples, Some([10.0, 19.0]))
            .collect();
        assert_eq!(in_window.len(), 10);
        let outside: Vec<_> = p
            .stack_indices(handle, &EventSource::Samples, Some([5_000.0, 6_000.0]))
            .collect();
        assert!(outside.is_empty());
    }
}
