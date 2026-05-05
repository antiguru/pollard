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
/// Cap on `top_processes`. Five fits a typical multi-process recording
/// (a binary plus its children) while keeping the summary payload narrow.
const DEFAULT_PROCESS_LIMIT: usize = 5;
/// Cap on `top_threads`. Ten covers the common "one busy worker pool"
/// shape without leaking deep into idle threads.
const DEFAULT_THREAD_LIMIT: usize = 10;

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub profile_id: String,
    pub name: String,
    /// Wall-clock span across all threads, rounded to whole milliseconds.
    /// See [`crate::query::describe::ProfileDescription::duration_ms`] for
    /// caveats around event-shaped (non-time-sampled) profiles.
    pub duration_ms: u64,
    pub interval_ms: f64,
    pub sample_rate_hz: f64,
    pub total_samples: u64,
    /// `[start_ms, end_ms]` taken from the union of per-thread sample
    /// times, **relative to profile start** so a caller can paste this
    /// straight into a `time_range` filter without an offset. The
    /// underlying clock is whatever samply records (boot-relative on
    /// Linux); add [`Self::profile_start_ms`] to recover the absolute
    /// timestamps. `[0.0, 0.0]` if the profile has no timed samples.
    pub time_range_ms: [f64; 2],
    /// Absolute timestamp (ms, in the profile's native clock) of the
    /// earliest recorded sample. Subtracted from raw sample times to
    /// produce [`Self::time_range_ms`] and the values that
    /// `time_range` filter args are matched against. `0.0` when the
    /// profile has no timed samples.
    pub profile_start_ms: f64,
    pub unsymbolicated_pct: f32,
    /// Coarse bucket for `unsymbolicated_pct` so the caller can decide
    /// "should I re-record?" without parsing the raw float.
    pub unsymbolicated_bracket: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dominant_thread: Option<DominantThread>,
    /// Top processes by sample count, descending. Capped at
    /// [`DEFAULT_PROCESS_LIMIT`]; processes beyond that are dropped silently
    /// (use `describe_profile` with a wider `top_n` to widen).
    pub top_processes: Vec<ProcessEntry>,
    /// Top threads by sample count, descending. Capped at
    /// [`DEFAULT_THREAD_LIMIT`]; threads beyond that are dropped silently.
    pub top_threads: Vec<ThreadEntry>,
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
pub struct ProcessEntry {
    /// String form to preserve samply's `.N` sub-process suffix.
    pub pid: String,
    pub name: String,
    /// Total samples across all threads of this process.
    pub samples: u64,
    /// `samples` as a percentage of profile-wide `total_samples`. Field
    /// name mirrors [`DominantThread::samples_pct`]; see issue #65 for
    /// the larger naming sweep across percentage fields.
    pub samples_pct: f32,
    /// Threads belonging to this process, before any filtering.
    pub thread_count: usize,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ThreadEntry {
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
    // `top_n=DEFAULT_PROCESS_LIMIT` lets us reuse the per-pid aggregation
    // describe already performs, so we don't walk the threads twice.
    let desc = describe(
        profile,
        profile_id,
        name,
        path,
        unsymbolicated_pct,
        DEFAULT_PROCESS_LIMIT,
    );

    let profile_start_ms = profile.start_time_ms();
    let time_range_ms = profile_time_range(profile, profile_start_ms);
    let dominant_thread = compute_dominant_thread(profile, desc.total_samples);
    let top_processes = compute_top_processes(&desc);
    let top_threads = compute_top_threads(profile, desc.total_samples, DEFAULT_THREAD_LIMIT);
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
        profile_start_ms,
        unsymbolicated_pct: desc.unsymbolicated_pct,
        unsymbolicated_bracket: bracket(desc.unsymbolicated_pct),
        dominant_thread,
        top_processes,
        top_threads,
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

fn profile_time_range(profile: &Profile, profile_start_ms: f64) -> [f64; 2] {
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
        [start - profile_start_ms, end - profile_start_ms]
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

fn compute_top_processes(desc: &crate::query::describe::ProfileDescription) -> Vec<ProcessEntry> {
    // `desc.processes` is already sorted descending by sample count and
    // capped at `DEFAULT_PROCESS_LIMIT` (passed as `top_n` above), so we
    // just translate the rows into the summary's narrower entry shape.
    let total_f = desc.total_samples.max(1) as f32;
    desc.processes
        .iter()
        .map(|p| ProcessEntry {
            pid: p.pid.clone(),
            name: p.name.clone(),
            samples: p.total_samples,
            samples_pct: 100.0 * p.total_samples as f32 / total_f,
            thread_count: p.thread_count,
        })
        .collect()
}

fn compute_top_threads(profile: &Profile, total: u64, limit: usize) -> Vec<ThreadEntry> {
    let total_f = total.max(1) as f32;
    let mut entries: Vec<ThreadEntry> = profile
        .threads()
        .filter(|t| t.raw().samples.length > 0)
        .map(|t| {
            let samples = t.raw().samples.length as u64;
            ThreadEntry {
                tid: t.tid(),
                name: t.name().unwrap_or("").to_owned(),
                pid: t.pid_full().to_string(),
                samples,
                samples_pct: 100.0 * samples as f32 / total_f,
            }
        })
        .collect();
    // Sort descending by samples; tid asc as a stable tiebreak so the
    // output doesn't shuffle across calls when counts are equal.
    entries.sort_by(|a, b| b.samples.cmp(&a.samples).then_with(|| a.tid.cmp(&b.tid)));
    entries.truncate(limit);
    entries
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
        assert_eq!(s.profile_start_ms, 0.0);
        assert!(s.dominant_thread.is_none());
        assert!(s.top_modules.is_empty());
        assert!(s.top_processes.is_empty());
        assert!(s.top_threads.is_empty());
    }

    #[test]
    fn summary_returns_top_processes_and_threads() {
        let profile = fixture("two_functions");
        let s = summary(&profile, "id", "name", "/tmp/p.json", 0.0).unwrap();

        // The fixture has at least one sampled thread, so both arrays
        // should be non-empty and capped at their respective limits.
        assert!(!s.top_processes.is_empty());
        assert!(s.top_processes.len() <= DEFAULT_PROCESS_LIMIT);
        assert!(!s.top_threads.is_empty());
        assert!(s.top_threads.len() <= DEFAULT_THREAD_LIMIT);

        // Top-process entry must carry positive sample counts and a
        // sensible percentage, and `thread_count` should be at least 1.
        let p = &s.top_processes[0];
        assert!(p.samples > 0);
        assert!(p.samples_pct > 0.0);
        assert!(p.thread_count >= 1);

        // Top-thread leader should match dominant_thread (same sort key,
        // same tiebreak), so the two surfaces stay self-consistent.
        let dom = s.dominant_thread.as_ref().unwrap();
        let leader = &s.top_threads[0];
        assert_eq!(leader.tid, dom.tid);
        assert_eq!(leader.samples, dom.samples);
        assert_eq!(leader.pid, dom.pid);
    }

    #[test]
    fn top_processes_ordered_descending_by_samples() {
        // Reuse describe's multi-process fixture pattern by constructing
        // a profile inline so we don't depend on fixture content.
        let json = r#"{
            "meta": {"interval": 1.0, "startTime": 0.0, "product": "test"},
            "libs": [],
            "threads": [
                {
                    "name": "t1", "processName": "alpha", "tid": 1, "pid": 10,
                    "registerTime": 0.0, "stringArray": [],
                    "frameTable": {"length": 0, "address": [], "func": [], "category": [], "subcategory": [], "line": [], "column": [], "nativeSymbol": []},
                    "stackTable": {"length": 0, "frame": [], "category": [], "subcategory": [], "prefix": []},
                    "samples": {"length": 2, "stack": [null, null], "time": [0.0, 1.0]},
                    "funcTable": {"length": 0, "name": [], "isJS": [], "relevantForJS": [], "resource": [], "fileName": [], "lineNumber": [], "columnNumber": []},
                    "resourceTable": {"length": 0, "lib": [], "name": [], "host": [], "type": []},
                    "nativeSymbols": {"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []}
                },
                {
                    "name": "t2", "processName": "beta", "tid": 2, "pid": 20,
                    "registerTime": 0.0, "stringArray": [],
                    "frameTable": {"length": 0, "address": [], "func": [], "category": [], "subcategory": [], "line": [], "column": [], "nativeSymbol": []},
                    "stackTable": {"length": 0, "frame": [], "category": [], "subcategory": [], "prefix": []},
                    "samples": {"length": 5, "stack": [null, null, null, null, null], "time": [0.0, 1.0, 2.0, 3.0, 4.0]},
                    "funcTable": {"length": 0, "name": [], "isJS": [], "relevantForJS": [], "resource": [], "fileName": [], "lineNumber": [], "columnNumber": []},
                    "resourceTable": {"length": 0, "lib": [], "name": [], "host": [], "type": []},
                    "nativeSymbols": {"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []}
                }
            ]
        }"#;
        let raw: RawProfile = serde_json::from_str(json).unwrap();
        let profile = Profile::from_raw(raw);
        let s = summary(&profile, "id", "name", "/tmp/p.json", 0.0).unwrap();

        assert_eq!(s.top_processes.len(), 2);
        assert_eq!(s.top_processes[0].name, "beta");
        assert_eq!(s.top_processes[0].samples, 5);
        assert_eq!(s.top_processes[1].name, "alpha");
        assert_eq!(s.top_processes[1].samples, 2);

        // Percentages should reflect 5/7 and 2/7 of total_samples.
        assert!((s.top_processes[0].samples_pct - 100.0 * 5.0 / 7.0).abs() < 0.01);
        assert!((s.top_processes[1].samples_pct - 100.0 * 2.0 / 7.0).abs() < 0.01);
    }

    /// Synthesize a profile whose sample timestamps are boot-relative
    /// (mimicking samply's actual output) and assert that
    /// `time_range_ms` is reported relative to the first sample, with
    /// `profile_start_ms` carrying the absolute anchor. Regression
    /// guard for issue #64.
    #[test]
    fn time_range_ms_is_relative_with_profile_start_offset() {
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
                "samples": {"length": 3, "stack": [null, null, null], "time": [42646349.0, 42646350.0, 42713794.0]},
                "funcTable": {"length": 0, "name": [], "isJS": [], "relevantForJS": [], "resource": [], "fileName": [], "lineNumber": [], "columnNumber": []},
                "resourceTable": {"length": 0, "lib": [], "name": [], "host": [], "type": []},
                "nativeSymbols": {"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []}
            }]
        }"#;
        let raw: RawProfile = serde_json::from_str(json).unwrap();
        let profile = Profile::from_raw(raw);
        let s = summary(&profile, "id", "name", "/tmp/p.json", 0.0).unwrap();
        assert_eq!(s.profile_start_ms, 42_646_349.0);
        assert_eq!(s.time_range_ms, [0.0, 42_713_794.0 - 42_646_349.0]);
    }
}
