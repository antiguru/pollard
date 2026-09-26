//! Event-source resolution: maps a user-facing `event` string to an
//! [`EventSource`]. The iterator implementation lives on [`Profile::stack_indices`];
//! this module owns the string-to-source mapping and the "did you mean"
//! suggestions so that path can stay profile-aware.

pub use crate::profile::EventSource;

use crate::error::ToolError;
use crate::profile::Profile;
use schemars::JsonSchema;
use serde::Serialize;

/// Resolve a user-facing event string to an [`EventSource`].
///
/// * `None` or empty string → [`EventSource::Samples`] (the default).
/// * A name that matches a marker carrying a `cause.stack` payload →
///   [`EventSource::Marker`].
/// * A name that matches a marker but the marker has no `cause.stack`
///   (e.g. text-only `mmap` annotations) → [`ToolError::Internal`]
///   explaining that the marker is present but isn't aggregatable.
/// * No matching marker → [`ToolError::Internal`] with the unknown-event
///   diagnostic and the list of stack-bearing names as suggestions.
///
/// The stackless-vs-unknown split lets the LLM tell "I asked for the
/// wrong name" (try a suggestion) apart from "this marker exists but
/// isn't aggregatable" (try a different marker entirely).
///
/// We do not treat `"cycles"` or `"samples"` as aliases — samply does
/// not emit a marker by either name, so the user must omit the arg to
/// get the samples track.
pub fn resolve(profile: &Profile, event: Option<&str>) -> Result<EventSource, ToolError> {
    let raw = match event {
        None | Some("") => return Ok(EventSource::Samples),
        Some(s) => s,
    };
    match marker_lookup(profile, raw) {
        MarkerLookup::StackBearing => Ok(EventSource::Marker(raw.to_owned())),
        MarkerLookup::Stackless => Err(ToolError::InvalidValue {
            field: "event".to_owned(),
            value: raw.to_owned(),
            accepted: known_marker_events(profile),
            hint: Some(
                "marker is present but carries no `cause.stack` payload, so it can't be \
                 aggregated as an event. Pick a stack-bearing marker from `accepted`, or \
                 omit `event` for the default samples track."
                    .to_owned(),
            ),
        }),
        MarkerLookup::Unknown => Err(ToolError::InvalidValue {
            field: "event".to_owned(),
            value: raw.to_owned(),
            accepted: known_marker_events(profile),
            hint: Some("omit `event` for the default samples track".to_owned()),
        }),
    }
}

/// Outcome of looking a marker name up across every thread.
///
/// `StackBearing` wins over `Stackless` if both are present for the
/// same name (a stack-bearing entry is enough to aggregate). `Unknown`
/// fires only when no entry uses the name at all.
#[derive(Debug, PartialEq, Eq)]
enum MarkerLookup {
    StackBearing,
    Stackless,
    Unknown,
}

fn marker_lookup(profile: &Profile, target: &str) -> MarkerLookup {
    let Some(target_idx) = profile.shared().strings.position(target) else {
        return MarkerLookup::Unknown;
    };
    let mut seen_stackless = false;
    for thread in profile.threads() {
        let raw = thread.raw();
        for (i, &str_idx) in raw.markers.name.iter().enumerate() {
            if str_idx != target_idx {
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
                // Any stack-bearing entry is enough; we can short-circuit.
                return MarkerLookup::StackBearing;
            }
            seen_stackless = true;
        }
    }
    if seen_stackless {
        MarkerLookup::Stackless
    } else {
        MarkerLookup::Unknown
    }
}

/// Marker schema samply uses for secondary perf events.
const OTHER_EVENT_TYPE: &str = "Other event";

/// One event a profile can aggregate by.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EventInfo {
    /// `"samples"` for the samples track, else the marker name, e.g.
    /// `"cache-misses"`. The samples track's real event name (such as
    /// cycles) is in `event` when the profile records it.
    pub name: String,
    /// `"samples"` or `"marker"`.
    pub source: &'static str,
    /// Samples or markers across all threads.
    pub count: u64,
    /// For markers: true when no marker of this name carries a stack,
    /// so it cannot be aggregated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stackless: Option<bool>,
    /// For the samples track: the main perf event's name as samply
    /// recorded it, e.g. `"cycles:u"`. Absent without samply's
    /// `Perf events` section.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    /// How perf sampled this event, `"frequency N Hz"` or `"period N"`.
    /// Absent when the profile does not say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling: Option<String>,
}

/// The samples track first, then `Other event` markers by name.
pub fn list_events(profile: &Profile) -> Vec<EventInfo> {
    let mut samples = 0u64;
    let mut markers: std::collections::BTreeMap<usize, (u64, bool)> = Default::default();
    for thread in profile.threads() {
        let raw = thread.raw();
        samples += raw.samples.length as u64;
        for (i, &str_idx) in raw.markers.name.iter().enumerate() {
            let Some(data) = raw.markers.data.get(i).and_then(|d| d.as_ref()) else {
                continue;
            };
            if data.type_.as_deref() != Some(OTHER_EVENT_TYPE) {
                continue;
            }
            let entry = markers.entry(str_idx).or_default();
            entry.0 += 1;
            entry.1 |= data.cause.is_some();
        }
    }
    let strings = &profile.shared().strings;
    let perf = profile.perf_events();
    // Equally named events share the first entry's sampling.
    let sampling_of = |label: &str| {
        perf.and_then(|p| p.event(label))
            .and_then(|e| e.sampling)
            .map(|s| s.describe())
    };
    let mut marker_events: Vec<EventInfo> = markers
        .into_iter()
        .filter_map(|(idx, (count, has_stack))| {
            let name = strings.get(idx)?.to_owned();
            Some(EventInfo {
                sampling: sampling_of(&name),
                name,
                source: "marker",
                count,
                stackless: Some(!has_stack),
                event: None,
            })
        })
        .collect();
    marker_events.sort_by(|a, b| a.name.cmp(&b.name));
    let main = perf.and_then(|p| p.main_event());
    let mut events = vec![EventInfo {
        name: "samples".to_owned(),
        source: "samples",
        count: samples,
        stackless: None,
        event: main.map(|e| e.label.clone()),
        sampling: main.and_then(|e| e.sampling).map(|s| s.describe()),
    }];
    events.extend(marker_events);
    events
}

/// Sorted list of distinct `Other event` marker names that have at
/// least one stack-bearing entry. Used to populate "did you mean?"
/// suggestions when `resolve` rejects an unknown event.
///
/// These suggestions list perf events only: `resolve` itself is not
/// limited to `Other event` markers, it also accepts any other
/// stack-bearing marker by its exact name.
fn known_marker_events(profile: &Profile) -> Vec<String> {
    list_events(profile)
        .into_iter()
        .filter(|e| e.stackless == Some(false))
        .map(|e| e.name)
        .collect()
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
    fn list_events_reads_the_perf_events_section() {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/weighted_events.json"))
                .unwrap();
        let p = Profile::from_raw(raw);
        let events = list_events(&p);
        assert_eq!(events[0].name, "samples");
        assert_eq!(events[0].event.as_deref(), Some("cycles"));
        assert_eq!(events[0].sampling.as_deref(), Some("frequency 1000 Hz"));
        let cm = events.iter().find(|e| e.name == "cache-misses").unwrap();
        assert_eq!(cm.event, None);
        assert_eq!(cm.sampling.as_deref(), Some("frequency 1000 Hz"));
        assert_eq!(cm.count, 4);
        let ins = events.iter().find(|e| e.name == "instructions").unwrap();
        assert_eq!(ins.sampling.as_deref(), Some("period 1000"));
    }

    #[test]
    fn list_events_without_section_omits_event_and_sampling() {
        let events = list_events(&fixture());
        assert_eq!(events[0].event, None);
        assert_eq!(events[0].sampling, None);
        let json = serde_json::to_value(&events[0]).unwrap();
        assert!(json.get("event").is_none(), "{json}");
        assert!(json.get("sampling").is_none(), "{json}");
    }

    #[test]
    fn list_events_reports_samples_and_other_event_markers() {
        let p = fixture();
        let events = list_events(&p);
        assert_eq!(events[0].name, "samples");
        assert_eq!(events[0].source, "samples");
        assert_eq!(events[0].stackless, None);
        let cm = events.iter().find(|e| e.name == "cache-misses").unwrap();
        assert_eq!(cm.source, "marker");
        assert_eq!(cm.count, 2);
        assert_eq!(cm.stackless, Some(false));
        // `mmap` markers are bookkeeping, not events.
        assert!(events.iter().all(|e| e.name != "mmap"));
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
        match err {
            ToolError::InvalidValue {
                field,
                value,
                accepted,
                hint,
            } => {
                assert_eq!(field, "event");
                assert_eq!(value, "not-a-real-event");
                assert!(
                    accepted.iter().any(|s| s == "cache-misses"),
                    "accepted={accepted:?}"
                );
                // The "no such marker" path should not pretend the marker
                // is stackless — that's a different and more actionable
                // error.
                assert!(
                    hint.as_deref()
                        .is_some_and(|s| !s.contains("no `cause.stack`")),
                    "hint should not mention stackless: {hint:?}"
                );
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn stackless_marker_errors_distinctly_from_unknown() {
        // two_events.json carries a stackless `mmap` marker (data == null).
        // Resolving event="mmap" must surface a *different* error from the
        // "no such marker" path so the LLM can tell "I asked for the wrong
        // name" apart from "this name exists but isn't aggregatable".
        let p = fixture();
        let err = resolve(&p, Some("mmap")).unwrap_err();
        match err {
            ToolError::InvalidValue {
                field,
                value,
                accepted,
                hint,
            } => {
                assert_eq!(field, "event");
                assert_eq!(value, "mmap");
                // Suggestions still come from the stack-bearing set.
                assert!(
                    accepted.iter().any(|s| s == "cache-misses"),
                    "accepted={accepted:?}"
                );
                // The hint must mention the stackless reason so the
                // caller doesn't mistake this for a typo.
                let hint = hint.expect("expected hint for stackless marker");
                assert!(
                    hint.contains("cause.stack")
                        || hint.contains("no stack")
                        || hint.contains("stackless"),
                    "expected stackless explanation in hint: {hint}"
                );
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
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

    /// Regression for issue #64: a profile with boot-relative sample
    /// timestamps (samply's actual output) must still accept a
    /// profile-relative `time_range`.
    #[test]
    fn samples_iter_time_range_is_relative_to_profile_start() {
        let json = r#"{
            "meta": {"interval": 1.0, "startTime": 0.0, "product": "test"},
            "libs": [],
            "threads": [{
                "name": "Main",
                "tid": 1,
                "pid": 1,
                "registerTime": 0.0,
                "stringArray": [],
                "frameTable": {"length": 0, "address": [], "func": [], "category": [], "subcategory": [], "line": [], "column": [], "nativeSymbol": []},
                "stackTable": {"length": 0, "frame": [], "category": [], "subcategory": [], "prefix": []},
                "samples": {"length": 4, "stack": [null, null, null, null], "time": [42646349.0, 42646359.0, 42646369.0, 42646379.0]},
                "funcTable": {"length": 0, "name": [], "isJS": [], "relevantForJS": [], "resource": [], "fileName": [], "lineNumber": [], "columnNumber": []},
                "resourceTable": {"length": 0, "lib": [], "name": [], "host": [], "type": []},
                "nativeSymbols": {"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []}
            }]
        }"#;
        let raw: RawProfile = serde_json::from_str(json).unwrap();
        let p = Profile::from_raw(raw);
        let handle = p.threads().next().unwrap().handle();
        // [0, 20] relative covers the first three samples (0, 10, 20 ms
        // after the first one). Pasting the absolute boot timestamp
        // would have selected zero.
        let kept: Vec<_> = p
            .stack_indices(handle, &EventSource::Samples, Some([0.0, 20.0]))
            .collect();
        assert_eq!(kept.len(), 3);
    }
}
