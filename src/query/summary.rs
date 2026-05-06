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
use crate::profile::symbolicate::LibSymbolicationOutcome;
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
    /// Discoverability nudges toward `create_view` when a heuristic fires
    /// against the summary as it stands. Each entry is a one-line human-
    /// readable hint naming the transform argument that would help.
    /// Triggers (cheap, one heuristic per hint, computed during summary):
    /// dominant function looks recursive (`collapse_recursion`), a single
    /// module dominates the stacks (`hide_modules`), and frame names
    /// carry generic / type parameters (`strip_type_params`). Empty when
    /// nothing fires. See issue #93.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub hints: Vec<String>,
    /// Per-lib symbolication outcomes worth flagging — non-`Loaded`
    /// libs, plus loaded libs whose every address lookup missed (a
    /// stale-binary signature; see issue #80). Empty when every lib
    /// loaded and resolved at least one frame.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub lib_diagnostics: Vec<LibSymbolicationOutcome>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DominantThread {
    pub tid: u64,
    pub name: String,
    /// String form to preserve samply's `.N` sub-process suffix.
    pub pid: String,
    pub samples: u64,
    pub self_pct: f32,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ProcessEntry {
    /// String form to preserve samply's `.N` sub-process suffix.
    pub pid: String,
    pub name: String,
    /// Total samples across all threads of this process.
    pub samples: u64,
    /// `samples` as a percentage of profile-wide `total_samples`. Same
    /// `{kind}_pct` convention as `top_functions[].self_pct` —
    /// `self` here means "samples attributed to this process",
    /// which has no `total` counterpart since processes don't compose
    /// the way function self/total does.
    pub self_pct: f32,
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
    pub self_pct: f32,
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
    filter_args: Filter,
) -> Result<Output, ToolError> {
    filter_args.validate_process(profile)?;
    filter_args.validate_thread(profile)?;
    filter_args.validate_time_range(profile)?;

    // Profile-wide describe still drives the *recording-level* fields
    // (interval, sample rate, unsymbolicated pct) — those don't shift
    // with a filter context. Sample-count fields below are recomputed
    // against the filter so the rest of the response is the filtered
    // view.
    let desc = describe(
        profile,
        profile_id,
        name,
        path,
        unsymbolicated_pct,
        DEFAULT_PROCESS_LIMIT,
    );

    let profile_start_ms = profile.start_time_ms();
    let (total_samples, time_range_ms) =
        compute_filtered_shape(profile, &filter_args, profile_start_ms);
    // Filtered duration: span of the surviving samples. Falls back to
    // describe's profile-wide duration when no samples land inside the
    // filter (the time_range_ms collapses to [0, 0]) so an empty slice
    // doesn't masquerade as a zero-length recording.
    let duration_ms = if time_range_ms == [0.0, 0.0] {
        desc.duration_ms
    } else {
        (time_range_ms[1] - time_range_ms[0]).max(0.0).round() as u64
    };
    let dominant_thread = compute_dominant_thread(profile, &filter_args, total_samples);
    let top_processes =
        compute_top_processes(profile, &filter_args, total_samples, DEFAULT_PROCESS_LIMIT);
    let top_threads =
        compute_top_threads(profile, &filter_args, total_samples, DEFAULT_THREAD_LIMIT);
    let top_modules = compute_top_modules(profile, &filter_args, DEFAULT_MODULE_LIMIT);

    let by_self = top_functions::top_functions(
        profile,
        &top_functions::Args {
            limit: DEFAULT_FUNCTION_LIMIT,
            sort_by: SortBy::SelfTime,
            filter_args: filter_args.clone(),
            ..Default::default()
        },
    )?;
    let by_total = top_functions::top_functions(
        profile,
        &top_functions::Args {
            limit: DEFAULT_FUNCTION_LIMIT,
            sort_by: SortBy::TotalTime,
            filter_args: filter_args.clone(),
            ..Default::default()
        },
    )?;

    let hints = compute_hints(
        profile,
        &filter_args,
        &by_total.functions,
        &by_self.functions,
        &top_modules,
    );

    Ok(Output {
        profile_id: desc.profile_id,
        name: desc.name,
        duration_ms,
        interval_ms: desc.interval_ms,
        sample_rate_hz: desc.sample_rate_hz,
        total_samples,
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
        hints,
        // Filled in by the lifecycle tool wrapper, which has access to
        // [`crate::session::ProfileSession::lib_outcomes`]. Default
        // empty so non-tool callers (tests, ad-hoc diagnostics) don't
        // need to think about it.
        lib_diagnostics: Vec::new(),
    })
}

/// Threshold (% of stacks) above which the dominant function triggers a
/// recursion check against the stack walk.
const RECURSION_DOMINANCE_PCT: f32 = 30.0;
/// Threshold (% of stacks) above which the leading module triggers the
/// `hide_modules` hint.
const MODULE_DOMINANCE_PCT: f32 = 40.0;

/// Build the `hints` payload from already-computed summary slices.
///
/// One heuristic per hint, no extra full traversal — only the recursion
/// probe touches the stack table, and only when the dominant function
/// already crosses [`RECURSION_DOMINANCE_PCT`]. Hints are skipped
/// silently when the relevant slice is empty (e.g. an unsymbolicated
/// recording with no top function), so the field stays absent in
/// degenerate cases instead of emitting noise.
fn compute_hints(
    profile: &Profile,
    filter: &Filter,
    by_total: &[FunctionEntry],
    by_self: &[FunctionEntry],
    top_modules: &[ModuleEntry],
) -> Vec<String> {
    let mut hints = Vec::new();

    if let Some(top) = by_total.first()
        && top.total_pct > RECURSION_DOMINANCE_PCT
        && function_recurs_in_any_stack(profile, filter, &top.function)
    {
        hints.push(format!(
            "{} looks recursive — try create_view(collapse_recursion=true)",
            top.function
        ));
    }

    if let Some(m) = top_modules.first()
        && m.total_pct > MODULE_DOMINANCE_PCT
    {
        hints.push(format!(
            "module {} dominates ({:.0}% of stacks) — try create_view(hide_modules=[\"{}\"]) to focus on application code",
            m.module, m.total_pct, m.module
        ));
    }

    if by_self
        .iter()
        .chain(by_total.iter())
        .any(|f| has_type_params(&f.function))
    {
        hints.push(
            "frame names carry type parameters — try create_view(strip_type_params=true) to normalize".into(),
        );
    }

    hints
}

/// True iff `name` contains a balanced or unbalanced `<…>` segment —
/// a `<` followed (eventually) by a `>` in the same string. Cheap
/// substring-only check; the actual `strip_type_params` transform
/// uses a depth counter on apply.
fn has_type_params(name: &str) -> bool {
    match name.find('<') {
        Some(i) => name[i + 1..].contains('>'),
        None => false,
    }
}

/// Return true if `name` appears at two or more frame positions in any
/// single stack under the filter. Walks `stack_indices` lazily and
/// short-circuits on the first stack that satisfies the predicate, so
/// the cost is bounded by "stacks visited until a recurrence is found"
/// rather than the full profile.
fn function_recurs_in_any_stack(profile: &Profile, filter: &Filter, name: &str) -> bool {
    use crate::query::event::EventSource;
    for handle in filter.threads(profile) {
        for stack_opt in profile.stack_indices(handle, &EventSource::Samples, filter.time_range) {
            let Some(stack_idx) = stack_opt else { continue };
            let mut hits = 0u32;
            for frame_idx in profile.walk_stack(handle, stack_idx) {
                let Some(info) = profile.frame_info(handle, frame_idx) else {
                    continue;
                };
                if info.function_name == name {
                    hits += 1;
                    if hits >= 2 {
                        return true;
                    }
                }
            }
        }
    }
    false
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

/// Filtered total-sample count plus the `[start, end]` span of the
/// surviving timestamps, both relative to `profile_start_ms`.
///
/// Threads outside the process/thread filter are skipped; per-sample
/// timestamps that fall outside the time-range filter are dropped. The
/// time-range comparison happens against the *raw* (boot-relative)
/// sample timestamps so the same range that gates `stack_indices`
/// gates this aggregation too.
///
/// When a thread carries no `time` / `timeDeltas` we fall back to its
/// `samples.length` (only possible if no time-range filter is set —
/// otherwise we have nothing to compare against). This matches the
/// semantics describe.rs uses for unstamped recordings.
fn compute_filtered_shape(
    profile: &Profile,
    filter: &Filter,
    profile_start_ms: f64,
) -> (u64, [f64; 2]) {
    let mut total: u64 = 0;
    let mut start = f64::INFINITY;
    let mut end = f64::NEG_INFINITY;
    for handle in filter.threads(profile) {
        let raw = profile.raw_thread(handle);
        let times = raw.samples.absolute_times();
        if times.is_empty() {
            // No timestamps — count every sample only when the filter
            // doesn't gate by time (otherwise we'd have to guess which
            // window they belong in).
            if filter.time_range.is_none() {
                total += raw.samples.length as u64;
            }
            continue;
        }
        for &abs_t in times.iter() {
            let rel = abs_t - profile_start_ms;
            if !filter.in_time_range(rel) {
                continue;
            }
            total += 1;
            if abs_t < start {
                start = abs_t;
            }
            if abs_t > end {
                end = abs_t;
            }
        }
    }
    let time_range_ms = if start.is_finite() && end.is_finite() {
        [start - profile_start_ms, end - profile_start_ms]
    } else {
        [0.0, 0.0]
    };
    (total, time_range_ms)
}

/// Per-thread sample count under the filter, keyed by [`ThreadHandle`].
/// Centralizes the "samples count under filter" rule so dominant_thread,
/// top_threads, and top_processes can't disagree about which samples
/// they each see.
fn per_thread_filtered_counts(
    profile: &Profile,
    filter: &Filter,
    profile_start_ms: f64,
) -> Vec<(crate::profile::ThreadHandle, u64)> {
    let mut out = Vec::new();
    for handle in filter.threads(profile) {
        let raw = profile.raw_thread(handle);
        let times = raw.samples.absolute_times();
        let count = if times.is_empty() {
            if filter.time_range.is_none() {
                raw.samples.length as u64
            } else {
                0
            }
        } else {
            times
                .iter()
                .filter(|&&abs_t| filter.in_time_range(abs_t - profile_start_ms))
                .count() as u64
        };
        out.push((handle, count));
    }
    out
}

fn compute_dominant_thread(
    profile: &Profile,
    filter: &Filter,
    total: u64,
) -> Option<DominantThread> {
    let total_f = total.max(1) as f32;
    let counts = per_thread_filtered_counts(profile, filter, profile.start_time_ms());
    counts
        .into_iter()
        .filter(|&(_, c)| c > 0)
        .max_by_key(|&(_, c)| c)
        .map(|(handle, samples)| {
            let raw = profile.raw_thread(handle);
            DominantThread {
                tid: raw.tid,
                name: raw.name.clone().unwrap_or_default(),
                pid: raw.pid.to_string(),
                samples,
                self_pct: 100.0 * samples as f32 / total_f,
            }
        })
}

fn compute_top_processes(
    profile: &Profile,
    filter: &Filter,
    total: u64,
    limit: usize,
) -> Vec<ProcessEntry> {
    let total_f = total.max(1) as f32;
    let counts = per_thread_filtered_counts(profile, filter, profile.start_time_ms());
    // Aggregate by full pid (preserving the `.N` sub-process suffix so
    // samply's parent recorder vs. forked targets stay distinct).
    let mut by_pid: HashMap<crate::profile::raw::Pid, (String, u64, usize)> = HashMap::new();
    for (handle, samples) in counts {
        let raw = profile.raw_thread(handle);
        let entry = by_pid
            .entry(raw.pid)
            .or_insert_with(|| (String::new(), 0, 0));
        if entry.0.is_empty()
            && let Some(name) = raw.process_name.as_deref().filter(|s| !s.is_empty())
        {
            entry.0 = name.to_owned();
        }
        entry.1 += samples;
        entry.2 += 1;
    }
    let mut entries: Vec<ProcessEntry> = by_pid
        .into_iter()
        .filter(|(_, (_, samples, _))| *samples > 0)
        .map(|(pid, (name, samples, thread_count))| ProcessEntry {
            pid: pid.to_string(),
            name,
            samples,
            self_pct: 100.0 * samples as f32 / total_f,
            thread_count,
        })
        .collect();
    entries.sort_by(|a, b| b.samples.cmp(&a.samples).then_with(|| a.pid.cmp(&b.pid)));
    entries.truncate(limit);
    entries
}

fn compute_top_threads(
    profile: &Profile,
    filter: &Filter,
    total: u64,
    limit: usize,
) -> Vec<ThreadEntry> {
    let total_f = total.max(1) as f32;
    let counts = per_thread_filtered_counts(profile, filter, profile.start_time_ms());
    let mut entries: Vec<ThreadEntry> = counts
        .into_iter()
        .filter(|&(_, c)| c > 0)
        .map(|(handle, samples)| {
            let raw = profile.raw_thread(handle);
            ThreadEntry {
                tid: raw.tid,
                name: raw.name.clone().unwrap_or_default(),
                pid: raw.pid.to_string(),
                samples,
                self_pct: 100.0 * samples as f32 / total_f,
            }
        })
        .collect();
    entries.sort_by(|a, b| b.samples.cmp(&a.samples).then_with(|| a.tid.cmp(&b.tid)));
    entries.truncate(limit);
    entries
}

fn compute_top_modules(profile: &Profile, filter: &Filter, limit: usize) -> Vec<ModuleEntry> {
    use crate::query::event::EventSource;
    let mut counts: HashMap<String, u64> = HashMap::new();
    let mut total_samples: u64 = 0;

    for handle in filter.threads(profile) {
        for stack_opt in profile.stack_indices(handle, &EventSource::Samples, filter.time_range) {
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
        let s = summary(
            &profile,
            "id",
            "name",
            "/tmp/p.json",
            0.0,
            Filter::default(),
        )
        .unwrap();
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
        let s = summary(
            &profile,
            "id",
            "name",
            "/tmp/p.json",
            0.0,
            Filter::default(),
        )
        .unwrap();
        let dom = s
            .dominant_thread
            .expect("fixture has at least one sampled thread");
        assert!(dom.samples > 0);
        assert!(dom.self_pct > 0.0);
    }

    #[test]
    fn summary_handles_empty_profile() {
        let profile = fixture("minimal");
        let s = summary(
            &profile,
            "id",
            "name",
            "/tmp/p.json",
            0.0,
            Filter::default(),
        )
        .unwrap();
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
        let s = summary(
            &profile,
            "id",
            "name",
            "/tmp/p.json",
            0.0,
            Filter::default(),
        )
        .unwrap();

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
        assert!(p.self_pct > 0.0);
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
        let s = summary(
            &profile,
            "id",
            "name",
            "/tmp/p.json",
            0.0,
            Filter::default(),
        )
        .unwrap();

        assert_eq!(s.top_processes.len(), 2);
        assert_eq!(s.top_processes[0].name, "beta");
        assert_eq!(s.top_processes[0].samples, 5);
        assert_eq!(s.top_processes[1].name, "alpha");
        assert_eq!(s.top_processes[1].samples, 2);

        // Percentages should reflect 5/7 and 2/7 of total_samples.
        assert!((s.top_processes[0].self_pct - 100.0 * 5.0 / 7.0).abs() < 0.01);
        assert!((s.top_processes[1].self_pct - 100.0 * 2.0 / 7.0).abs() < 0.01);
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
        let s = summary(
            &profile,
            "id",
            "name",
            "/tmp/p.json",
            0.0,
            Filter::default(),
        )
        .unwrap();
        assert_eq!(s.profile_start_ms, 42_646_349.0);
        assert_eq!(s.time_range_ms, [0.0, 42_713_794.0 - 42_646_349.0]);
    }

    /// Two processes with different sample volumes; a `process=` filter
    /// should restrict every sample-count surface to the named process —
    /// `total_samples`, `top_processes`, `top_threads`, and the
    /// dominant-thread slot all reflect the slice. Regression guard for
    /// issue #63 (per-process orientation via filter).
    #[test]
    fn filter_by_process_restricts_sample_counts_to_that_process() {
        use crate::query::filters::ProcessFilter;
        let json = r#"{
            "meta": {"interval": 1.0, "startTime": 0.0, "product": "test"},
            "libs": [],
            "threads": [
                {"name": "t1", "processName": "alpha", "tid": 1, "pid": 10, "registerTime": 0.0,
                 "stringArray": [],
                 "frameTable": {"length": 0, "address": [], "func": [], "category": [], "subcategory": [], "line": [], "column": [], "nativeSymbol": []},
                 "stackTable": {"length": 0, "frame": [], "category": [], "subcategory": [], "prefix": []},
                 "samples": {"length": 2, "stack": [null, null], "time": [0.0, 1.0]},
                 "funcTable": {"length": 0, "name": [], "isJS": [], "relevantForJS": [], "resource": [], "fileName": [], "lineNumber": [], "columnNumber": []},
                 "resourceTable": {"length": 0, "lib": [], "name": [], "host": [], "type": []},
                 "nativeSymbols": {"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []}},
                {"name": "t2", "processName": "beta", "tid": 2, "pid": 20, "registerTime": 0.0,
                 "stringArray": [],
                 "frameTable": {"length": 0, "address": [], "func": [], "category": [], "subcategory": [], "line": [], "column": [], "nativeSymbol": []},
                 "stackTable": {"length": 0, "frame": [], "category": [], "subcategory": [], "prefix": []},
                 "samples": {"length": 5, "stack": [null, null, null, null, null], "time": [0.0, 1.0, 2.0, 3.0, 4.0]},
                 "funcTable": {"length": 0, "name": [], "isJS": [], "relevantForJS": [], "resource": [], "fileName": [], "lineNumber": [], "columnNumber": []},
                 "resourceTable": {"length": 0, "lib": [], "name": [], "host": [], "type": []},
                 "nativeSymbols": {"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []}}
            ]
        }"#;
        let raw: RawProfile = serde_json::from_str(json).unwrap();
        let profile = Profile::from_raw(raw);
        let s = summary(
            &profile,
            "id",
            "name",
            "/tmp/p.json",
            0.0,
            Filter {
                process: Some(ProcessFilter::Name("beta".into())),
                ..Default::default()
            },
        )
        .unwrap();

        // Filtered scope = beta's 5 samples only.
        assert_eq!(s.total_samples, 5);
        // Only beta should appear in top_processes / top_threads.
        assert_eq!(s.top_processes.len(), 1);
        assert_eq!(s.top_processes[0].name, "beta");
        assert_eq!(s.top_processes[0].samples, 5);
        // beta's sample share within its own filtered scope is 100%.
        assert!((s.top_processes[0].self_pct - 100.0).abs() < 0.01);
        assert_eq!(s.top_threads.len(), 1);
        assert_eq!(s.top_threads[0].tid, 2);
        let dom = s.dominant_thread.expect("filtered profile has samples");
        assert_eq!(dom.tid, 2);
        assert_eq!(dom.samples, 5);
    }

    /// `time_range` filter must trim `total_samples` and `time_range_ms`
    /// on top of the process / thread slice.
    #[test]
    fn filter_by_time_range_trims_sample_counts() {
        let json = r#"{
            "meta": {"interval": 1.0, "startTime": 0.0, "product": "test"},
            "libs": [],
            "threads": [{
                "name": "Main", "tid": 1, "pid": 1, "registerTime": 0.0,
                "stringArray": [],
                "frameTable": {"length": 0, "address": [], "func": [], "category": [], "subcategory": [], "line": [], "column": [], "nativeSymbol": []},
                "stackTable": {"length": 0, "frame": [], "category": [], "subcategory": [], "prefix": []},
                "samples": {"length": 5, "stack": [null, null, null, null, null], "time": [0.0, 10.0, 20.0, 30.0, 40.0]},
                "funcTable": {"length": 0, "name": [], "isJS": [], "relevantForJS": [], "resource": [], "fileName": [], "lineNumber": [], "columnNumber": []},
                "resourceTable": {"length": 0, "lib": [], "name": [], "host": [], "type": []},
                "nativeSymbols": {"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []}
            }]
        }"#;
        let raw: RawProfile = serde_json::from_str(json).unwrap();
        let profile = Profile::from_raw(raw);
        // Time range covers the middle three samples (10, 20, 30 ms).
        let s = summary(
            &profile,
            "id",
            "name",
            "/tmp/p.json",
            0.0,
            Filter {
                time_range: Some([10.0, 30.0]),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(s.total_samples, 3);
        assert_eq!(s.time_range_ms, [10.0, 30.0]);
        // Filtered duration is the span of the surviving samples, not
        // the full recording.
        assert_eq!(s.duration_ms, 20);
    }

    #[test]
    fn has_type_params_detects_brackets() {
        assert!(has_type_params("Vec<T>"));
        assert!(has_type_params(
            "OrdValBatch<RowRowLayout<((Row, Row), Ts, i64)>>"
        ));
        assert!(!has_type_params("plain_name"));
        assert!(!has_type_params("a>b"));
        assert!(!has_type_params(""));
    }

    #[test]
    fn no_hints_when_top_function_is_not_recursive() {
        // two_functions: `hot` carries 90% of samples but each stack is
        // a single frame, so the recursion probe must say no — and with
        // no modules / no `<>` in names, hints should be empty.
        let profile = fixture("two_functions");
        let s = summary(
            &profile,
            "id",
            "name",
            "/tmp/p.json",
            0.0,
            Filter::default(),
        )
        .unwrap();
        assert!(s.hints.is_empty(), "expected no hints, got {:?}", s.hints);
    }

    #[test]
    fn recursion_hint_fires_on_recursive_dominant_function() {
        // Three-deep self-call on `rec`: every stack contains `rec` at
        // three frame positions. `rec`'s total_pct is 100%, well above
        // the 30% gate, and the recursion probe trips on the first
        // stack it visits.
        let json = r#"{
            "meta": {"interval": 1.0, "startTime": 0.0, "product": "test"},
            "libs": [],
            "threads": [{
                "name": "Main", "tid": 1, "pid": 1, "registerTime": 0.0,
                "stringArray": ["rec"],
                "frameTable": {"length": 1, "address": [-1], "func": [0], "category": [0], "subcategory": [0], "line": [null], "column": [null], "nativeSymbol": [null]},
                "stackTable": {"length": 3, "frame": [0, 0, 0], "category": [0, 0, 0], "subcategory": [0, 0, 0], "prefix": [null, 0, 1]},
                "samples": {"length": 4, "stack": [2, 2, 2, 2], "time": [0.0, 1.0, 2.0, 3.0]},
                "funcTable": {"length": 1, "name": [0], "isJS": [false], "relevantForJS": [false], "resource": [-1], "fileName": [null], "lineNumber": [null], "columnNumber": [null]},
                "resourceTable": {"length": 0, "lib": [], "name": [], "host": [], "type": []},
                "nativeSymbols": {"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []}
            }]
        }"#;
        let raw: RawProfile = serde_json::from_str(json).unwrap();
        let profile = Profile::from_raw(raw);
        let s = summary(
            &profile,
            "id",
            "name",
            "/tmp/p.json",
            0.0,
            Filter::default(),
        )
        .unwrap();
        assert!(
            s.hints.iter().any(|h| h.contains("collapse_recursion")),
            "expected a collapse_recursion hint, got {:?}",
            s.hints
        );
    }

    #[test]
    fn type_params_hint_fires_when_frame_names_carry_brackets() {
        // Single frame named `foo<T>` so a type-param hint trips off
        // the top-functions list without needing a recursion or module
        // signal.
        let json = r#"{
            "meta": {"interval": 1.0, "startTime": 0.0, "product": "test"},
            "libs": [],
            "threads": [{
                "name": "Main", "tid": 1, "pid": 1, "registerTime": 0.0,
                "stringArray": ["foo<T>"],
                "frameTable": {"length": 1, "address": [-1], "func": [0], "category": [0], "subcategory": [0], "line": [null], "column": [null], "nativeSymbol": [null]},
                "stackTable": {"length": 1, "frame": [0], "category": [0], "subcategory": [0], "prefix": [null]},
                "samples": {"length": 2, "stack": [0, 0], "time": [0.0, 1.0]},
                "funcTable": {"length": 1, "name": [0], "isJS": [false], "relevantForJS": [false], "resource": [-1], "fileName": [null], "lineNumber": [null], "columnNumber": [null]},
                "resourceTable": {"length": 0, "lib": [], "name": [], "host": [], "type": []},
                "nativeSymbols": {"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []}
            }]
        }"#;
        let raw: RawProfile = serde_json::from_str(json).unwrap();
        let profile = Profile::from_raw(raw);
        let s = summary(
            &profile,
            "id",
            "name",
            "/tmp/p.json",
            0.0,
            Filter::default(),
        )
        .unwrap();
        assert!(
            s.hints.iter().any(|h| h.contains("strip_type_params")),
            "expected a strip_type_params hint, got {:?}",
            s.hints
        );
    }
}
