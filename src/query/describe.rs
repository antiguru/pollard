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
    pub sample_rate_hz: f64,
    pub total_samples: u64,
    pub unsymbolicated_pct: f32,
    pub processes: Vec<ProcessDescription>,
}

#[derive(Serialize, JsonSchema, Debug)]
pub struct ProcessDescription {
    pub pid: u64,
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
    let sample_rate_hz = if interval_ms > 0.0 { 1000.0 / interval_ms } else { 0.0 };

    // Group threads by pid.
    let mut by_pid: std::collections::BTreeMap<u64, Vec<ThreadDescription>> =
        std::collections::BTreeMap::new();
    let mut total_samples: u64 = 0;

    for thread in profile.threads() {
        let raw = thread.raw();
        let times = raw.samples.absolute_times();
        let dur = times.last().copied().unwrap_or(0.0) - times.first().copied().unwrap_or(0.0);
        let samples = raw.samples.length as u64;
        total_samples += samples;
        by_pid.entry(thread.pid()).or_default().push(ThreadDescription {
            tid: thread.tid(),
            name: thread.name().unwrap_or("").to_owned(),
            samples,
            duration_ms: dur,
        });
    }

    let processes = by_pid
        .into_iter()
        .map(|(pid, threads)| ProcessDescription {
            pid,
            name: String::new(), // TODO: extract from RawProfile.processes when present
            thread_count: threads.len(),
            threads,
        })
        .collect();

    ProfileDescription {
        profile_id: profile_id.to_owned(),
        name: name.to_owned(),
        path: path.to_owned(),
        duration_ms: profile.duration_ms(),
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
        let raw: RawProfile = serde_json::from_str(include_str!(
            "../../tests/fixtures/minimal_profile.json"
        ))
        .unwrap();
        let profile = Profile::from_raw(raw);
        let desc = describe(&profile, "id1", "name1", "/tmp/p.json", 0.0);
        assert_eq!(desc.profile_id, "id1");
        assert_eq!(desc.name, "name1");
        assert_eq!(desc.unsymbolicated_pct, 0.0);
        assert!(!desc.processes.is_empty() || !desc.processes.is_empty());
    }
}
