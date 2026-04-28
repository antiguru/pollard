//! `summary`: one-call orientation across the whole profile.
//!
//! Composes `describe` (for shape: duration, sample rate, unsymbolicated%),
//! `top_functions` twice (sorted by self and by total), and a small
//! per-module aggregation. The tool exists so an LLM can spend one
//! round-trip understanding "what is this profile" instead of three.
//! See issue #16.

#![allow(dead_code)]

use crate::error::ToolError;
use crate::profile::Profile;
use crate::query::describe::describe;
use crate::query::filters::Filter;
use crate::query::top_functions::{self, FunctionEntry, SortBy};
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

const DEFAULT_FUNCTION_LIMIT: usize = 10;
const DEFAULT_MODULE_LIMIT: usize = 5;

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub profile_id: String,
    pub name: String,
    pub duration_ms: f64,
    pub interval_ms: f64,
    pub sample_rate_hz: f64,
    pub total_samples: u64,
    /// `[start_ms, end_ms]` taken from the union of per-thread sample
    /// times. `[0.0, 0.0]` if the profile has no timed samples.
    pub time_range_ms: [f64; 2],
    pub unsymbolicated_pct: f32,
    /// Coarse bucket for `unsymbolicated_pct` so the caller can decide
    /// "should I re-record?" without parsing the raw float.
    pub unsymbolicated_bracket: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dominant_thread: Option<DominantThread>,
    pub top_modules: Vec<ModuleEntry>,
    pub top_self_functions: Vec<FunctionEntry>,
    pub top_total_functions: Vec<FunctionEntry>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DominantThread {
    pub tid: u64,
    pub name: String,
    /// String form to preserve samply's `.N` sub-process suffix.
    pub pid: String,
    pub samples: u64,
    pub samples_pct: f32,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ModuleEntry {
    pub rank: usize,
    pub module: String,
    pub total_samples: u64,
    pub total_pct: f32,
}

pub fn summary(
    profile: &Profile,
    profile_id: &str,
    name: &str,
    path: &str,
    unsymbolicated_pct: f32,
) -> Result<Output, ToolError> {
    // Reuse `describe` rather than re-deriving sample-rate / total-samples
    // so the two tools cannot disagree about how those values are counted.
    let desc = describe(profile, profile_id, name, path, unsymbolicated_pct);

    let time_range_ms = profile_time_range(profile);
    let dominant_thread = compute_dominant_thread(profile, desc.total_samples);
    let top_modules = compute_top_modules(profile, DEFAULT_MODULE_LIMIT);

    let by_self = top_functions::top_functions(
        profile,
        &top_functions::Args {
            limit: DEFAULT_FUNCTION_LIMIT,
            sort_by: SortBy::SelfTime,
            filter_args: Filter::default(),
            ..Default::default()
        },
    )?;
    let by_total = top_functions::top_functions(
        profile,
        &top_functions::Args {
            limit: DEFAULT_FUNCTION_LIMIT,
            sort_by: SortBy::TotalTime,
            filter_args: Filter::default(),
            ..Default::default()
        },
    )?;

    Ok(Output {
        profile_id: desc.profile_id,
        name: desc.name,
        duration_ms: desc.duration_ms,
        interval_ms: desc.interval_ms,
        sample_rate_hz: desc.sample_rate_hz,
        total_samples: desc.total_samples,
        time_range_ms,
        unsymbolicated_pct: desc.unsymbolicated_pct,
        unsymbolicated_bracket: bracket(desc.unsymbolicated_pct),
        dominant_thread,
        top_modules,
        top_self_functions: by_self.functions,
        top_total_functions: by_total.functions,
    })
}

fn bracket(pct: f32) -> &'static str {
    if pct <= 0.0 {
        "0%"
    } else if pct < 1.0 {
        "<1%"
    } else if pct < 5.0 {
        "1-5%"
    } else if pct < 25.0 {
        "5-25%"
    } else if pct < 50.0 {
        "25-50%"
    } else {
        ">50%"
    }
}

fn profile_time_range(profile: &Profile) -> [f64; 2] {
    let mut start = f64::INFINITY;
    let mut end = f64::NEG_INFINITY;
    for t in profile.threads() {
        let times = t.raw().samples.absolute_times();
        if let Some(&first) = times.first() {
            start = start.min(first);
        }
        if let Some(&last) = times.last() {
            end = end.max(last);
        }
    }
    if start.is_finite() && end.is_finite() {
        [start, end]
    } else {
        [0.0, 0.0]
    }
}

fn compute_dominant_thread(profile: &Profile, total: u64) -> Option<DominantThread> {
    let total_f = total.max(1) as f32;
    profile
        .threads()
        .filter(|t| t.raw().samples.length > 0)
        .max_by_key(|t| t.raw().samples.length)
        .map(|t| {
            let samples = t.raw().samples.length as u64;
            DominantThread {
                tid: t.tid(),
                name: t.name().unwrap_or("").to_owned(),
                pid: t.pid_full().to_string(),
                samples,
                samples_pct: 100.0 * samples as f32 / total_f,
            }
        })
}

fn compute_top_modules(profile: &Profile, limit: usize) -> Vec<ModuleEntry> {
    let mut counts: HashMap<String, u64> = HashMap::new();
    let mut total_samples: u64 = 0;

    for view in profile.threads() {
        let handle = view.handle();
        for &stack_opt in &view.raw().samples.stack {
            let Some(stack_idx) = stack_opt else { continue };
            total_samples += 1;
            // Each sample contributes 1 to every module appearing at least
            // once on its stack — same semantics as `total_samples` in
            // `top_functions`.
            let mut seen: HashSet<String> = HashSet::new();
            for frame_idx in profile.walk_stack(handle, stack_idx) {
                let Some(info) = profile.frame_info(handle, frame_idx) else {
                    continue;
                };
                let Some(module) = info.module_name else {
                    continue;
                };
                if seen.insert(module.to_owned()) {
                    *counts.entry(module.to_owned()).or_default() += 1;
                }
            }
        }
    }

    let mut entries: Vec<(String, u64)> = counts.into_iter().collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let total_f = total_samples.max(1) as f32;
    entries
        .into_iter()
        .take(limit)
        .enumerate()
        .map(|(i, (module, total))| ModuleEntry {
            rank: i + 1,
            module,
            total_samples: total,
            total_pct: 100.0 * total as f32 / total_f,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::raw::RawProfile;

    fn fixture(path: &str) -> Profile {
        let json = match path {
            "two_functions" => include_str!("../../tests/fixtures/two_functions.json"),
            "minimal" => include_str!("../../tests/fixtures/minimal_profile.json"),
            other => panic!("unknown fixture: {other}"),
        };
        let raw: RawProfile = serde_json::from_str(json).unwrap();
        Profile::from_raw(raw)
    }

    #[test]
    fn bracket_buckets() {
        assert_eq!(bracket(0.0), "0%");
        assert_eq!(bracket(0.4), "<1%");
        assert_eq!(bracket(1.0), "1-5%");
        assert_eq!(bracket(4.99), "1-5%");
        assert_eq!(bracket(5.0), "5-25%");
        assert_eq!(bracket(24.99), "5-25%");
        assert_eq!(bracket(25.0), "25-50%");
        assert_eq!(bracket(50.0), ">50%");
        assert_eq!(bracket(99.0), ">50%");
    }

    #[test]
    fn summary_returns_top_self_and_total_with_limit() {
        let profile = fixture("two_functions");
        let s = summary(&profile, "id", "name", "/tmp/p.json", 0.0).unwrap();
        assert!(s.total_samples > 0);
        assert_eq!(
            s.top_self_functions.len().min(DEFAULT_FUNCTION_LIMIT),
            s.top_self_functions.len()
        );
        assert_eq!(
            s.top_self_functions[0].function, "hot",
            "self-time leader should be `hot`, got {:?}",
            s.top_self_functions
        );
        assert!(!s.top_total_functions.is_empty());
        assert_eq!(s.unsymbolicated_bracket, "0%");
    }

    #[test]
    fn summary_picks_dominant_thread() {
        let profile = fixture("two_functions");
        let s = summary(&profile, "id", "name", "/tmp/p.json", 0.0).unwrap();
        let dom = s
            .dominant_thread
            .expect("fixture has at least one sampled thread");
        assert!(dom.samples > 0);
        assert!(dom.samples_pct > 0.0);
    }

    #[test]
    fn summary_handles_empty_profile() {
        let profile = fixture("minimal");
        let s = summary(&profile, "id", "name", "/tmp/p.json", 0.0).unwrap();
        // Minimal fixture has zero samples; time_range collapses to [0,0].
        assert_eq!(s.time_range_ms, [0.0, 0.0]);
        assert!(s.dominant_thread.is_none());
        assert!(s.top_modules.is_empty());
    }
}
