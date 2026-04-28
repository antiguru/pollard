//! Event-source resolution: maps a user-facing `event` string to either
//! the per-thread samples track (default — what samply uses for
//! cycles-as-samples) or to a marker-name slice. Provides a unified
//! stack-index iterator so the aggregators stay generic.

#![allow(dead_code)]

use crate::error::ToolError;
use crate::profile::{Profile, ThreadHandle};

/// Where a per-sample stack index comes from. Matches the two layouts
/// samply emits: cycles in `samples`, all other hardware events in
/// `markers` keyed by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum EventSource {
    /// The default samples track. samply records the first perf event
    /// (typically `cycles`) here; pct columns mean "% of cycles" or
    /// equivalently "% of CPU time".
    #[default]
    Samples,
    /// Markers whose `name[i]` resolves to this string. samply uses this
    /// for secondary perf events (cache-misses, branch-misses,
    /// instructions, etc.).
    Marker(String),
}

impl EventSource {
    /// True iff the source's per-event count multiplied by
    /// `meta.interval` produces a meaningful wall-time-ish duration.
    /// `Samples` is, all marker-backed events are not.
    pub fn is_time_shaped(&self) -> bool {
        matches!(self, EventSource::Samples)
    }

    /// Stable lowercase label for output payloads ("samples" or the
    /// marker name verbatim).
    pub fn label(&self) -> &str {
        match self {
            EventSource::Samples => "samples",
            EventSource::Marker(name) => name.as_str(),
        }
    }
}

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

/// Iterate the stack-table indices that this thread contributes for the
/// given event source. `Some(idx)` per sample/marker, `None` to skip
/// (matching the existing `samples.stack: Vec<Option<usize>>` shape so
/// callers can stay in their current per-stack loop).
pub fn stack_indices<'a>(
    profile: &'a Profile,
    handle: ThreadHandle,
    source: &'a EventSource,
) -> Box<dyn Iterator<Item = Option<usize>> + 'a> {
    let raw = profile.raw_thread(handle);
    match source {
        EventSource::Samples => Box::new(raw.samples.stack.iter().copied()),
        EventSource::Marker(name) => {
            // Resolve the marker name to its string-array index *once*
            // per thread; markers without a `cause.stack` payload are
            // yielded as `None` so the caller's "skip None" branch
            // handles them uniformly with samples that have no stack.
            let str_idx = raw.string_array.iter().position(|s| s == name);
            match str_idx {
                None => Box::new(std::iter::empty()),
                Some(target) => Box::new(raw.markers.name.iter().enumerate().filter_map(
                    move |(i, &n)| {
                        // Skip non-matching markers entirely so we yield
                        // exactly one item per *matching* marker. Text-only
                        // matches still appear, as `None`, so the aggregator
                        // can tell "no stack to attribute to" apart from
                        // "marker isn't ours".
                        if n != target {
                            return None;
                        }
                        Some(
                            raw.markers
                                .data
                                .get(i)
                                .and_then(|d| d.as_ref())
                                .and_then(|d| d.cause.as_ref())
                                .map(|c| c.stack),
                        )
                    },
                )),
            }
        }
    }
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
        let n: usize = stack_indices(&p, handle, &EventSource::Samples).count();
        assert_eq!(n, p.raw_thread(handle).samples.stack.len());
    }

    #[test]
    fn marker_iter_filters_to_named_event() {
        let p = fixture();
        let handle = p.threads().next().unwrap().handle();
        let stacks: Vec<_> =
            stack_indices(&p, handle, &EventSource::Marker("cache-misses".into())).collect();
        assert_eq!(stacks.len(), 2);
        assert!(stacks.iter().all(|s| s.is_some()));
    }
}
