//! Implementation of the `describe_profile` MCP tool.

#![allow(dead_code)]

use crate::profile::Profile;
use schemars::JsonSchema;
use serde::Serialize;

#[derive(Serialize, JsonSchema, Debug)]
pub struct ProfileDescription {
    pub profile_id: String,
    pub name: String,
    pub path: String,
    pub duration_ms: f64,
    /// Sampling interval in milliseconds, as recorded in `meta.interval`.
    pub interval_ms: f64,
    /// Derived sampling rate: `1000.0 / interval_ms`, or 0 if interval is 0.
    pub sample_rate_hz: f64,
    pub total_samples: u64,
    pub unsymbolicated_pct: f32,
    pub processes: Vec<ProcessDescription>,
}

#[derive(Serialize, JsonSchema, Debug)]
pub struct ProcessDescription {
    /// String form to preserve the `.N` sub-process suffix samply emits for
    /// distinct processes that share the same OS pid (e.g. `"1969186.1"`).
    pub pid: String,
    pub name: String,
    pub thread_count: usize,
    pub threads: Vec<ThreadDescription>,
}

#[derive(Serialize, JsonSchema, Debug)]
pub struct ThreadDescription {
    pub tid: u64,
    pub name: String,
    pub samples: u64,
    pub duration_ms: f64,
}

pub fn describe(
    profile: &Profile,
    profile_id: &str,
    name: &str,
    path: &str,
    unsymbolicated_pct: f32,
) -> ProfileDescription {
    let interval_ms = profile.meta().interval;
    let sample_rate_hz = if interval_ms > 0.0 {
        1000.0 / interval_ms
    } else {
        0.0
    };

    // Group threads by pid. The Firefox processed-profile schema attaches
    // the process name to each thread (`processName`), not to a separate
    // process record — so we recover it here by taking the first non-empty
    // value seen for each pid. We bucket on the full [`Pid`] so the `.N`
    // sub-process suffix samply uses (parent recorder vs. forked targets
    // sharing one OS pid) doesn't collapse them into a single entry.
    let mut by_pid: std::collections::BTreeMap<
        crate::profile::raw::Pid,
        (Option<String>, Vec<ThreadDescription>),
    > = std::collections::BTreeMap::new();
    let mut total_samples: u64 = 0;

    for thread in profile.threads() {
        let raw = thread.raw();
        let times = raw.samples.absolute_times();
        let dur = times.last().copied().unwrap_or(0.0) - times.first().copied().unwrap_or(0.0);
        let samples = raw.samples.length as u64;
        total_samples += samples;
        let entry = by_pid.entry(thread.pid_full()).or_default();
        if entry.0.is_none()
            && let Some(pname) = thread.process_name().filter(|s| !s.is_empty())
        {
            entry.0 = Some(pname.to_owned());
        }
        entry.1.push(ThreadDescription {
            tid: thread.tid(),
            name: thread.name().unwrap_or("").to_owned(),
            samples,
            duration_ms: dur,
        });
    }

    let processes = by_pid
        .into_iter()
        .map(|(pid, (proc_name, threads))| ProcessDescription {
            pid: pid.to_string(),
            name: proc_name.unwrap_or_default(),
            thread_count: threads.len(),
            threads,
        })
        .collect();

    ProfileDescription {
        profile_id: profile_id.to_owned(),
        name: name.to_owned(),
        path: path.to_owned(),
        duration_ms: profile.duration_ms(),
        interval_ms,
        sample_rate_hz,
        total_samples,
        unsymbolicated_pct,
        processes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Profile, raw::RawProfile};

    #[test]
    fn describes_minimal_profile() {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/minimal_profile.json"))
                .unwrap();
        let profile = Profile::from_raw(raw);
        let desc = describe(&profile, "id1", "name1", "/tmp/p.json", 0.0);
        assert_eq!(desc.profile_id, "id1");
        assert_eq!(desc.name, "name1");
        assert_eq!(desc.unsymbolicated_pct, 0.0);
        assert!(!desc.processes.is_empty() || !desc.processes.is_empty());
    }

    #[test]
    fn process_name_recovered_from_thread_field() {
        // Firefox-style schema places the process name on each thread, not
        // on a separate process record. Confirm describe() picks it up.
        let json = r#"{
            "meta": {"interval": 1.0, "startTime": 0.0, "product": "test"},
            "libs": [],
            "threads": [{
                "name": "Main",
                "processName": "rustfmt",
                "tid": 1,
                "pid": 42,
                "registerTime": 0.0,
                "stringArray": [],
                "frameTable": {"length": 0, "address": [], "func": [], "category": [], "subcategory": [], "line": [], "column": [], "nativeSymbol": []},
                "stackTable": {"length": 0, "frame": [], "category": [], "subcategory": [], "prefix": []},
                "samples": {"length": 0, "stack": [], "time": []},
                "funcTable": {"length": 0, "name": [], "isJS": [], "relevantForJS": [], "resource": [], "fileName": [], "lineNumber": [], "columnNumber": []},
                "resourceTable": {"length": 0, "lib": [], "name": [], "host": [], "type": []},
                "nativeSymbols": {"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []}
            }]
        }"#;
        let raw: RawProfile = serde_json::from_str(json).unwrap();
        let profile = Profile::from_raw(raw);
        let desc = describe(&profile, "id", "n", "/tmp/p", 0.0);
        assert_eq!(desc.processes.len(), 1);
        assert_eq!(desc.processes[0].pid, "42");
        assert_eq!(desc.processes[0].name, "rustfmt");
    }

    #[test]
    fn surfaces_interval_and_sample_rate() {
        // 1ms interval → 1 kHz sampling rate.
        let json = r#"{
            "meta": {"interval": 1.0, "startTime": 0.0, "product": "test"},
            "libs": [],
            "threads": []
        }"#;
        let raw: RawProfile = serde_json::from_str(json).unwrap();
        let profile = Profile::from_raw(raw);
        let desc = describe(&profile, "id", "n", "/tmp/p", 0.0);
        assert_eq!(desc.interval_ms, 1.0);
        assert_eq!(desc.sample_rate_hz, 1000.0);
    }

    #[test]
    fn zero_interval_yields_zero_rate() {
        // Defensive: synthetic profile with interval=0 must not divide by zero.
        let json = r#"{
            "meta": {"interval": 0.0, "startTime": 0.0, "product": "test"},
            "libs": [],
            "threads": []
        }"#;
        let raw: RawProfile = serde_json::from_str(json).unwrap();
        let profile = Profile::from_raw(raw);
        let desc = describe(&profile, "id", "n", "/tmp/p", 0.0);
        assert_eq!(desc.interval_ms, 0.0);
        assert_eq!(desc.sample_rate_hz, 0.0);
    }
}
