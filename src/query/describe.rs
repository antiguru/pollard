//! Implementation of the `describe_profile` MCP tool.

#![allow(dead_code)]

use crate::profile::Profile;
use crate::profile::symbolicate::LibSymbolicationOutcome;
use crate::serde_util::{is_zero_u64, is_zero_usize};
use schemars::JsonSchema;
use serde::Serialize;

/// Default cap on processes (and threads per process) returned by
/// [`describe`]. Sized so the JSON payload of a multi-hundred-process
/// profile stays well under MCP tool-result token limits while keeping
/// the busiest entries visible. Callers can override via `top_n`.
pub const DEFAULT_TOP_N: usize = 20;

#[derive(Serialize, JsonSchema, Debug)]
pub struct ProfileDescription {
    pub profile_id: String,
    pub name: String,
    pub path: String,
    /// Wall-clock span across all threads, rounded to whole milliseconds.
    /// For event-shaped (non-time-sampled) profiles this is still the
    /// extent between the earliest and latest event timestamp; the
    /// magnitude signal there is `total_samples`, not duration.
    pub duration_ms: u64,
    /// Sampling interval in milliseconds, as recorded in `meta.interval`.
    /// Meaningful only for time-sampled profiles.
    pub interval_ms: f64,
    /// Derived sampling rate: `1000.0 / interval_ms`, or 0 if interval is 0.
    pub sample_rate_hz: f64,
    pub total_samples: u64,
    /// Number of distinct processes with at least one thread, before any
    /// truncation. The `processes` array may hold fewer entries.
    pub total_processes: usize,
    /// Number of distinct threads across all processes, before any
    /// 0-sample drop or `top_n` truncation. The per-process `threads`
    /// arrays may hold fewer entries.
    pub total_threads: usize,
    pub unsymbolicated_pct: f32,
    /// Processes sorted descending by sample count. 0-sample processes
    /// and any beyond `top_n` are omitted; see `omitted_process_*`.
    pub processes: Vec<ProcessDescription>,
    #[serde(skip_serializing_if = "is_zero_usize")]
    pub omitted_process_count: usize,
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub omitted_process_samples: u64,
    /// Per-lib symbolication outcomes worth flagging — non-`Loaded`
    /// libs, plus loaded libs whose every address lookup missed (a
    /// stale-binary signature; see issue #80). Empty when every lib
    /// loaded and resolved at least one frame.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub lib_diagnostics: Vec<LibSymbolicationOutcome>,
}

#[derive(Serialize, JsonSchema, Debug)]
pub struct ProcessDescription {
    /// String form to preserve the `.N` sub-process suffix samply emits for
    /// distinct processes that share the same OS pid (e.g. `"1969186.1"`).
    pub pid: String,
    pub name: String,
    /// Total threads belonging to this process, before any drop or
    /// truncation. The `threads` array may hold fewer entries.
    pub thread_count: usize,
    /// Sum of samples across all threads of this process, before any
    /// truncation.
    pub total_samples: u64,
    /// Threads sorted descending by sample count. 0-sample threads and
    /// any beyond `top_n` are omitted; see `omitted_thread_*`.
    pub threads: Vec<ThreadDescription>,
    #[serde(skip_serializing_if = "is_zero_usize")]
    pub omitted_thread_count: usize,
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub omitted_thread_samples: u64,
}

#[derive(Serialize, JsonSchema, Debug)]
pub struct ThreadDescription {
    pub tid: u64,
    pub name: String,
    pub samples: u64,
    /// Wall-clock span between first and last sample, rounded to whole
    /// milliseconds. May be 0 for event-shaped profiles where every
    /// event lands at the same timestamp; sort by `samples` instead.
    pub duration_ms: u64,
}

/// Build a [`ProfileDescription`] capped at `top_n` processes (and `top_n`
/// threads per process). Pass `0` to suppress per-process detail entirely
/// while still computing the scalar totals.
///
/// Threads with zero samples are dropped before truncation and counted
/// into `omitted_thread_*` (and, when an entire process collapses to
/// zero samples, into `omitted_process_*`).
pub fn describe(
    profile: &Profile,
    profile_id: &str,
    name: &str,
    path: &str,
    unsymbolicated_pct: f32,
    top_n: usize,
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
    let mut total_threads: usize = 0;

    for thread in profile.threads() {
        let raw = thread.raw();
        let times = raw.samples.absolute_times();
        let dur_f = times.last().copied().unwrap_or(0.0) - times.first().copied().unwrap_or(0.0);
        let duration_ms = dur_f.max(0.0).round() as u64;
        let samples = raw.samples.length as u64;
        total_samples += samples;
        total_threads += 1;
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
            duration_ms,
        });
    }

    let total_processes = by_pid.len();

    let mut processes: Vec<ProcessDescription> = by_pid
        .into_iter()
        .map(|(pid, (proc_name, mut threads))| {
            let thread_count = threads.len();
            let proc_total: u64 = threads.iter().map(|t| t.samples).sum();
            // Sort descending by samples, with tid asc as a stable tiebreak
            // so output doesn't shuffle between calls on equal counts.
            threads.sort_by(|a, b| b.samples.cmp(&a.samples).then_with(|| a.tid.cmp(&b.tid)));
            // Drop trailing 0-sample threads. After the desc-sort these are
            // contiguous at the tail, so a single truncate suffices.
            let nonzero_end = threads
                .iter()
                .rposition(|t| t.samples > 0)
                .map(|i| i + 1)
                .unwrap_or(0);
            threads.truncate(nonzero_end);
            if threads.len() > top_n {
                threads.truncate(top_n);
            }
            let kept_samples: u64 = threads.iter().map(|t| t.samples).sum();
            let omitted_thread_count = thread_count - threads.len();
            let omitted_thread_samples = proc_total - kept_samples;
            ProcessDescription {
                pid: pid.to_string(),
                name: proc_name.unwrap_or_default(),
                thread_count,
                total_samples: proc_total,
                threads,
                omitted_thread_count,
                omitted_thread_samples,
            }
        })
        .collect();

    // Sort processes descending by sample count, pid asc as tiebreak.
    processes.sort_by(|a, b| {
        b.total_samples
            .cmp(&a.total_samples)
            .then_with(|| a.pid.cmp(&b.pid))
    });
    let proc_total_full: u64 = processes.iter().map(|p| p.total_samples).sum();
    // Drop trailing 0-sample processes (mirrors the per-thread rule).
    let nonzero_end = processes
        .iter()
        .rposition(|p| p.total_samples > 0)
        .map(|i| i + 1)
        .unwrap_or(0);
    processes.truncate(nonzero_end);
    if processes.len() > top_n {
        processes.truncate(top_n);
    }
    let kept_proc_samples: u64 = processes.iter().map(|p| p.total_samples).sum();
    let omitted_process_count = total_processes - processes.len();
    let omitted_process_samples = proc_total_full - kept_proc_samples;

    let duration_ms = profile.duration_ms().max(0.0).round() as u64;

    ProfileDescription {
        profile_id: profile_id.to_owned(),
        name: name.to_owned(),
        path: path.to_owned(),
        duration_ms,
        interval_ms,
        sample_rate_hz,
        total_samples,
        total_processes,
        total_threads,
        unsymbolicated_pct,
        processes,
        omitted_process_count,
        omitted_process_samples,
        // Filled in by the lifecycle tool wrappers, which have access to
        // the [`crate::session::ProfileSession`] and its lib outcomes.
        // Default empty so callers that build descriptions outside that
        // path (tests, ad-hoc diagnostics) don't need to think about it.
        lib_diagnostics: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Profile, raw::RawProfile};

    /// Boilerplate-free thread literal for the inline fixtures below.
    /// Generates a `samples` table with `n` samples spaced 1ms apart so
    /// the per-thread `duration_ms` is non-trivial.
    fn thread_json(name: &str, proc_name: &str, tid: u64, pid: &str, samples: usize) -> String {
        let times: Vec<String> = (0..samples).map(|i| (i as f64).to_string()).collect();
        let stacks: Vec<&str> = (0..samples).map(|_| "null").collect();
        format!(
            r#"{{
                "name": "{name}",
                "processName": "{proc_name}",
                "tid": {tid},
                "pid": {pid_lit},
                "registerTime": 0.0,
                "stringArray": [],
                "frameTable": {{"length": 0, "address": [], "func": [], "category": [], "subcategory": [], "line": [], "column": [], "nativeSymbol": []}},
                "stackTable": {{"length": 0, "frame": [], "category": [], "subcategory": [], "prefix": []}},
                "samples": {{"length": {samples}, "stack": [{stack}], "time": [{time}]}},
                "funcTable": {{"length": 0, "name": [], "isJS": [], "relevantForJS": [], "resource": [], "fileName": [], "lineNumber": [], "columnNumber": []}},
                "resourceTable": {{"length": 0, "lib": [], "name": [], "host": [], "type": []}},
                "nativeSymbols": {{"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []}}
            }}"#,
            pid_lit = if pid.parse::<u64>().is_ok() {
                pid.to_string()
            } else {
                format!("\"{pid}\"")
            },
            stack = stacks.join(","),
            time = times.join(","),
        )
    }

    fn build_profile(threads: &[(&str, &str, u64, &str, usize)]) -> Profile {
        let body = threads
            .iter()
            .map(|(n, pn, tid, pid, s)| thread_json(n, pn, *tid, pid, *s))
            .collect::<Vec<_>>()
            .join(",");
        let json = format!(
            r#"{{
                "meta": {{"interval": 1.0, "startTime": 0.0, "product": "test"}},
                "libs": [],
                "threads": [{body}]
            }}"#
        );
        let raw: RawProfile = serde_json::from_str(&json).unwrap();
        Profile::from_raw(raw)
    }

    #[test]
    fn describes_minimal_profile() {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/minimal_profile.json"))
                .unwrap();
        let profile = Profile::from_raw(raw);
        let desc = describe(&profile, "id1", "name1", "/tmp/p.json", 0.0, DEFAULT_TOP_N);
        assert_eq!(desc.profile_id, "id1");
        assert_eq!(desc.name, "name1");
        assert_eq!(desc.unsymbolicated_pct, 0.0);
    }

    #[test]
    fn process_name_recovered_from_thread_field() {
        // Firefox-style schema places the process name on each thread, not
        // on a separate process record. Confirm describe() picks it up.
        let p = build_profile(&[("Main", "rustfmt", 1, "42", 3)]);
        let desc = describe(&p, "id", "n", "/tmp/p", 0.0, DEFAULT_TOP_N);
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
        let desc = describe(&profile, "id", "n", "/tmp/p", 0.0, DEFAULT_TOP_N);
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
        let desc = describe(&profile, "id", "n", "/tmp/p", 0.0, DEFAULT_TOP_N);
        assert_eq!(desc.interval_ms, 0.0);
        assert_eq!(desc.sample_rate_hz, 0.0);
    }

    #[test]
    fn drops_zero_sample_threads_and_reports_full_thread_count() {
        // One process, two threads — only one has samples. The 0-sample
        // thread must not appear in `threads`, but `thread_count` and
        // `total_threads` must still reflect both, and the omission is
        // counted in `omitted_thread_count`.
        let p = build_profile(&[("Main", "app", 1, "42", 5), ("Idle", "app", 2, "42", 0)]);
        let desc = describe(&p, "id", "n", "/tmp/p", 0.0, DEFAULT_TOP_N);
        assert_eq!(desc.total_threads, 2);
        assert_eq!(desc.processes.len(), 1);
        let proc = &desc.processes[0];
        assert_eq!(proc.thread_count, 2);
        assert_eq!(proc.threads.len(), 1);
        assert_eq!(proc.threads[0].name, "Main");
        assert_eq!(proc.omitted_thread_count, 1);
        assert_eq!(proc.omitted_thread_samples, 0);
    }

    #[test]
    fn drops_zero_sample_processes() {
        // Two processes; one has only 0-sample threads. It must collapse
        // out of `processes` but still be counted in totals/omitted.
        let p = build_profile(&[("Main", "busy", 1, "10", 4), ("Idle", "ghost", 2, "20", 0)]);
        let desc = describe(&p, "id", "n", "/tmp/p", 0.0, DEFAULT_TOP_N);
        assert_eq!(desc.total_processes, 2);
        assert_eq!(desc.total_threads, 2);
        assert_eq!(desc.processes.len(), 1);
        assert_eq!(desc.processes[0].name, "busy");
        assert_eq!(desc.omitted_process_count, 1);
        assert_eq!(desc.omitted_process_samples, 0);
    }

    #[test]
    fn truncates_threads_to_top_n_with_omitted_counts() {
        // Three threads with 5/3/1 samples; top_n=2 keeps the busiest
        // two and reports 1 omitted thread carrying 1 sample.
        let p = build_profile(&[
            ("a", "p", 1, "42", 5),
            ("b", "p", 2, "42", 3),
            ("c", "p", 3, "42", 1),
        ]);
        let desc = describe(&p, "id", "n", "/tmp/p", 0.0, 2);
        let proc = &desc.processes[0];
        assert_eq!(proc.thread_count, 3);
        assert_eq!(proc.threads.len(), 2);
        assert_eq!(proc.threads[0].name, "a");
        assert_eq!(proc.threads[1].name, "b");
        assert_eq!(proc.omitted_thread_count, 1);
        assert_eq!(proc.omitted_thread_samples, 1);
    }

    #[test]
    fn truncates_processes_to_top_n_with_omitted_counts() {
        // Three single-thread processes with 5/3/1 samples; top_n=2.
        let p = build_profile(&[
            ("t1", "alpha", 1, "10", 5),
            ("t2", "beta", 2, "20", 3),
            ("t3", "gamma", 3, "30", 1),
        ]);
        let desc = describe(&p, "id", "n", "/tmp/p", 0.0, 2);
        assert_eq!(desc.total_processes, 3);
        assert_eq!(desc.processes.len(), 2);
        assert_eq!(desc.processes[0].name, "alpha");
        assert_eq!(desc.processes[1].name, "beta");
        assert_eq!(desc.omitted_process_count, 1);
        assert_eq!(desc.omitted_process_samples, 1);
    }

    #[test]
    fn top_n_zero_returns_no_processes_but_keeps_totals() {
        // `summary` calls describe with top_n=0 because it only needs
        // the scalars. Verify totals stay populated and omissions cover
        // the full set.
        let p = build_profile(&[("a", "p", 1, "42", 5), ("b", "p", 2, "42", 3)]);
        let desc = describe(&p, "id", "n", "/tmp/p", 0.0, 0);
        assert_eq!(desc.total_samples, 8);
        assert_eq!(desc.total_processes, 1);
        assert_eq!(desc.total_threads, 2);
        assert!(desc.processes.is_empty());
        assert_eq!(desc.omitted_process_count, 1);
        assert_eq!(desc.omitted_process_samples, 8);
    }

    #[test]
    fn duration_ms_rounded_to_whole_milliseconds() {
        // 5 samples spaced 1ms → span = 4ms exactly. Rounding logic is
        // exercised through the `as u64` cast on `dur.round()`.
        let p = build_profile(&[("Main", "app", 1, "42", 5)]);
        let desc = describe(&p, "id", "n", "/tmp/p", 0.0, DEFAULT_TOP_N);
        assert_eq!(desc.processes[0].threads[0].duration_ms, 4);
    }
}
