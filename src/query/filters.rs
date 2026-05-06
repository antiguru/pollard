//! Reusable filter abstraction for thread/process/time selection.

#![allow(dead_code)]

use crate::error::{ERROR_LIST_LIMIT, ProcessRef, ThreadRef, ToolError};
use crate::profile::raw::Pid;
use crate::profile::{Profile, ThreadHandle};

#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub thread: Option<ThreadFilter>,
    pub process: Option<ProcessFilter>,
    /// Inclusive `[start_ms, end_ms]` window, anchored at profile start
    /// (i.e. the earliest sample timestamp; see
    /// [`Profile::start_time_ms`]). Sample times are offset by that
    /// anchor before the comparison, so this matches `summary`'s
    /// `time_range_ms` field one-for-one regardless of whether the
    /// underlying clock is boot-relative.
    pub time_range: Option<[f64; 2]>,
}

#[derive(Debug, Clone)]
pub enum ThreadFilter {
    Tid(u64),
    Name(String),
}

#[derive(Debug, Clone)]
pub enum ProcessFilter {
    /// Numeric pid match. A [`Pid`] without `suffix` matches every thread
    /// sharing the OS pid (i.e. ignores samply's `.N` sub-process suffix);
    /// a [`Pid`] with `suffix` set matches only the exact sub-process.
    Pid(Pid),
    /// Match against [`ThreadView::process_name`] (Firefox-style schema
    /// stores the process name on the thread, not on a separate record).
    Name(String),
}

impl Filter {
    /// Returns thread handles matching the filter. Empty if nothing matches.
    pub fn threads<'a>(&'a self, profile: &'a Profile) -> impl Iterator<Item = ThreadHandle> + 'a {
        profile.threads().filter_map(move |t| {
            if let Some(pf) = &self.process {
                let ok = match pf {
                    ProcessFilter::Pid(p) => match p.suffix {
                        // Bare pid matches every thread sharing that OS pid,
                        // regardless of the `.N` sub-process suffix.
                        None => t.pid() == p.value,
                        // Suffixed pid pins to the exact sub-process.
                        Some(_) => t.pid_full() == *p,
                    },
                    ProcessFilter::Name(n) => t.process_name().is_some_and(|name| name == n),
                };
                if !ok {
                    return None;
                }
            }
            if let Some(tf) = &self.thread {
                let ok = match tf {
                    ThreadFilter::Tid(tid) => t.tid() == *tid,
                    ThreadFilter::Name(n) => t.name().is_some_and(|name| name == n),
                };
                if !ok {
                    return None;
                }
            }
            Some(t.handle())
        })
    }

    /// Validate thread filter; if it matches no threads, return a structured error.
    pub fn validate_thread(&self, profile: &Profile) -> Result<(), ToolError> {
        if self.thread.is_none() {
            return Ok(());
        }
        if self.threads(profile).next().is_some() {
            return Ok(());
        }
        // Rank by sample count, descending, so the busiest threads stay
        // visible after truncation. Equal counts tiebreak by tid asc so
        // the output is stable across calls.
        let mut all: Vec<ThreadRef> = profile
            .threads()
            .map(|t| ThreadRef {
                tid: t.tid(),
                name: t.name().unwrap_or("").to_owned(),
                samples: t.raw().samples.length as u64,
            })
            .collect();
        all.sort_by(|a, b| b.samples.cmp(&a.samples).then_with(|| a.tid.cmp(&b.tid)));
        let total = all.len();
        let available_threads: Vec<ThreadRef> = all.into_iter().take(ERROR_LIST_LIMIT).collect();
        let omitted_thread_count = total.saturating_sub(available_threads.len());
        let thread = match self.thread.as_ref().unwrap() {
            ThreadFilter::Tid(t) => t.to_string(),
            ThreadFilter::Name(n) => n.clone(),
        };
        Err(ToolError::ThreadNotFound {
            thread,
            available_threads,
            omitted_thread_count,
        })
    }

    /// Validate process filter; if it matches no threads, return a
    /// `process_not_found` error listing the busiest distinct
    /// `(pid, name)` pairs (capped at [`ERROR_LIST_LIMIT`]) so the
    /// caller can pick a real one without drowning in 196-process
    /// listings. Mirrors [`Self::validate_thread`].
    pub fn validate_process(&self, profile: &Profile) -> Result<(), ToolError> {
        let Some(pf) = self.process.as_ref() else {
            return Ok(());
        };
        // Only the process predicate participates in this check — a
        // thread-filter miss must surface as `thread_not_found`, not a
        // misleading `process_not_found`.
        let process_only = Filter {
            process: Some(pf.clone()),
            ..Default::default()
        };
        if process_only.threads(profile).next().is_some() {
            return Ok(());
        }
        // Aggregate per-pid: pick the first non-empty process name we
        // see, sum samples across all threads of the pid.
        let mut seen: std::collections::BTreeMap<Pid, (String, u64)> =
            std::collections::BTreeMap::new();
        for t in profile.threads() {
            let entry = seen.entry(t.pid_full()).or_default();
            if entry.0.is_empty()
                && let Some(name) = t.process_name().filter(|s| !s.is_empty())
            {
                entry.0 = name.to_owned();
            }
            entry.1 += t.raw().samples.length as u64;
        }
        let mut all: Vec<ProcessRef> = seen
            .into_iter()
            .map(|(pid, (name, samples))| ProcessRef {
                pid: pid.to_string(),
                name,
                samples,
            })
            .collect();
        // Rank by sample count desc; pid asc tiebreak for stability.
        all.sort_by(|a, b| b.samples.cmp(&a.samples).then_with(|| a.pid.cmp(&b.pid)));
        let total = all.len();
        let available_processes: Vec<ProcessRef> = all.into_iter().take(ERROR_LIST_LIMIT).collect();
        let omitted_process_count = total.saturating_sub(available_processes.len());
        let process = match pf {
            ProcessFilter::Pid(p) => p.to_string(),
            ProcessFilter::Name(n) => n.clone(),
        };
        Err(ToolError::ProcessNotFound {
            process,
            available_processes,
            omitted_process_count,
        })
    }

    /// When the process filter is a bare name (`ProcessFilter::Name`) that
    /// matches more than one distinct pid, return the matched
    /// `(pid, name)` pairs so callers can surface the over-aggregation in
    /// their response. Returns `None` when the filter is unset, when it's
    /// a `Pid` filter (already pid-precise), or when the bare-name match
    /// resolves to a single pid.
    ///
    /// Distinct-pid count uses [`ThreadView::pid_full`] so samply's
    /// `.N` sub-process suffix counts as a separate process — passing
    /// `pid.suffix` syntax is exactly the disambiguation we tell the
    /// caller about.
    pub fn bare_name_multi_match(&self, profile: &Profile) -> Option<Vec<ProcessRef>> {
        let ProcessFilter::Name(needle) = self.process.as_ref()? else {
            return None;
        };
        let mut seen: std::collections::BTreeMap<Pid, (String, u64)> =
            std::collections::BTreeMap::new();
        for t in profile.threads() {
            if t.process_name().is_none_or(|name| name != needle) {
                continue;
            }
            let entry = seen.entry(t.pid_full()).or_default();
            if entry.0.is_empty() {
                entry.0 = t
                    .process_name()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("")
                    .to_owned();
            }
            entry.1 += t.raw().samples.length as u64;
        }
        if seen.len() <= 1 {
            return None;
        }
        Some(
            seen.into_iter()
                .map(|(pid, (name, samples))| ProcessRef {
                    pid: pid.to_string(),
                    name,
                    samples,
                })
                .collect(),
        )
    }

    /// Clamp a time range to the profile's actual duration; emit no error.
    /// Returns the clamped range and the original-range diagnostic if anything changed.
    pub fn clamped_time_range(
        &self,
        profile_duration: f64,
    ) -> Option<([f64; 2], Option<[f64; 2]>)> {
        let r = self.time_range?;
        let clamped = [r[0].max(0.0), r[1].min(profile_duration)];
        let changed = if (clamped[0] - r[0]).abs() > f64::EPSILON
            || (clamped[1] - r[1]).abs() > f64::EPSILON
        {
            Some(r)
        } else {
            None
        };
        Some((clamped, changed))
    }

    /// Validate the time-range filter. Per the design spec, partial
    /// overlap clamps silently — only a fully-disjoint range (or an
    /// inverted `[start > end]`) is a hard error so the LLM can correct
    /// it. Profile bounds come from [`Profile::duration_ms`], anchored
    /// at zero.
    pub fn validate_time_range(&self, profile: &Profile) -> Result<(), ToolError> {
        let Some([start, end]) = self.time_range else {
            return Ok(());
        };
        let duration = profile.duration_ms();
        let clamped = [start.max(0.0), end.min(duration)];
        let inverted = start > end;
        let disjoint = clamped[0] > clamped[1];
        if inverted || disjoint {
            return Err(ToolError::OutOfBounds {
                original_range: [start, end],
                clamped_range: clamped,
            });
        }
        Ok(())
    }

    /// True when `time_ms` falls within the time-range filter, or when
    /// no time-range is set. Used by per-sample iterators to gate
    /// inclusion.
    pub fn in_time_range(&self, time_ms: f64) -> bool {
        match self.time_range {
            None => true,
            Some([s, e]) => time_ms >= s && time_ms <= e,
        }
    }

    /// True when no scope predicate is set — caller can skip composition
    /// entirely. Loaded (non-view) profiles always carry this default.
    pub fn is_unset(&self) -> bool {
        self.thread.is_none() && self.process.is_none() && self.time_range.is_none()
    }

    /// Compose a request-time filter (`self`) under a view's pre-filter
    /// (`scope`). Per the design notes on issue #90, per-call filters
    /// must be a sub-slice of the view's scope: a scoped view pins a
    /// process/thread/window once, and the per-call filter can only
    /// further narrow within it.
    ///
    /// Field-by-field rules:
    /// - **thread**: when both are set they must select the same
    ///   thread. We accept literal equality (same `Tid` or same `Name`)
    ///   only — name↔tid pairs would require resolving against the
    ///   profile to be sound, and silent acceptance of a mismatch is
    ///   exactly the confusing failure mode the design rejects.
    /// - **process**: same equality rule, with one exception — a bare
    ///   `Pid` view scope accepts a per-call `Pid` with the matching
    ///   `value` and an additional `suffix`, since a suffixed pid is a
    ///   strict sub-slice of the bare pid that covers it.
    /// - **time_range**: per-call `[s', e']` must satisfy
    ///   `s' >= s && e' <= e` against the view's `[s, e]`.
    ///
    /// On conflict, returns [`ToolError::InvalidValue`] with `field`
    /// pointing at the offending dimension and a hint that names the
    /// scope so the caller can correct in one retry.
    pub fn compose_under_scope(self, scope: &Filter) -> Result<Filter, ToolError> {
        if scope.is_unset() {
            return Ok(self);
        }
        let thread = match (scope.thread.as_ref(), self.thread) {
            (None, t) => t,
            (Some(s), None) => Some(s.clone()),
            (Some(s), Some(t)) => {
                if !thread_filter_eq(s, &t) {
                    return Err(scope_conflict_error(
                        "thread",
                        thread_filter_label(s),
                        thread_filter_label(&t),
                    ));
                }
                Some(t)
            }
        };
        let process = match (scope.process.as_ref(), self.process) {
            (None, p) => p,
            (Some(s), None) => Some(s.clone()),
            (Some(s), Some(p)) => {
                let kept = process_filter_subslice(s, &p).map_err(|()| {
                    scope_conflict_error(
                        "process",
                        process_filter_label(s),
                        process_filter_label(&p),
                    )
                })?;
                Some(kept)
            }
        };
        let time_range = match (scope.time_range, self.time_range) {
            (None, t) => t,
            (Some(s), None) => Some(s),
            (Some([ss, se]), Some([rs, re])) => {
                if rs < ss || re > se {
                    return Err(scope_conflict_error(
                        "time_range",
                        format!("[{ss}, {se}]"),
                        format!("[{rs}, {re}]"),
                    ));
                }
                Some([rs, re])
            }
        };
        Ok(Filter {
            thread,
            process,
            time_range,
        })
    }
}

fn thread_filter_eq(a: &ThreadFilter, b: &ThreadFilter) -> bool {
    match (a, b) {
        (ThreadFilter::Tid(x), ThreadFilter::Tid(y)) => x == y,
        (ThreadFilter::Name(x), ThreadFilter::Name(y)) => x == y,
        _ => false,
    }
}

fn thread_filter_label(t: &ThreadFilter) -> String {
    match t {
        ThreadFilter::Tid(n) => format!("tid:{n}"),
        ThreadFilter::Name(n) => n.clone(),
    }
}

fn process_filter_label(p: &ProcessFilter) -> String {
    match p {
        ProcessFilter::Pid(p) => format!("pid:{p}"),
        ProcessFilter::Name(n) => n.clone(),
    }
}

/// Returns the more specific of (`scope`, `request`) when `request` is a
/// sub-slice of `scope`; `Err(())` otherwise. A bare-pid scope accepts a
/// suffixed pid with the matching `value` (samply's `.N` sub-process is
/// a strict sub-slice of the OS pid bucket); everything else must match
/// literally.
fn process_filter_subslice(
    scope: &ProcessFilter,
    request: &ProcessFilter,
) -> Result<ProcessFilter, ()> {
    use crate::profile::raw::Pid;
    match (scope, request) {
        (ProcessFilter::Pid(s), ProcessFilter::Pid(r)) => {
            if s.value != r.value {
                return Err(());
            }
            match (s.suffix, r.suffix) {
                (None, _) => Ok(ProcessFilter::Pid(*r)), // bare scope, request more specific (or equal)
                (Some(a), Some(b)) if a == b => Ok(ProcessFilter::Pid(Pid {
                    value: s.value,
                    suffix: Some(a),
                })),
                _ => Err(()),
            }
        }
        (ProcessFilter::Name(s), ProcessFilter::Name(r)) if s == r => {
            Ok(ProcessFilter::Name(r.clone()))
        }
        _ => Err(()),
    }
}

fn scope_conflict_error(field: &'static str, scope: String, request: String) -> ToolError {
    ToolError::InvalidValue {
        field: field.to_owned(),
        value: request,
        accepted: vec!["<unset>".to_owned(), scope.clone()],
        hint: Some(format!(
            "view scope pins {field}={scope}; per-call {field} must be unset or a sub-slice"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Profile, raw::RawProfile};

    fn fixture() -> Profile {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/minimal_profile.json"))
                .unwrap();
        Profile::from_raw(raw)
    }

    #[test]
    fn no_filter_keeps_everything() {
        let p = fixture();
        let filter = Filter::default();
        let kept: Vec<_> = filter.threads(&p).collect();
        assert!(!kept.is_empty());
    }

    #[test]
    fn thread_name_filter_matches() {
        let p = fixture();
        let filter = Filter {
            thread: Some(ThreadFilter::Name("Main".into())),
            ..Default::default()
        };
        let kept: Vec<_> = filter.threads(&p).collect();
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn unmatched_thread_returns_empty() {
        let p = fixture();
        let filter = Filter {
            thread: Some(ThreadFilter::Name("Nope".into())),
            ..Default::default()
        };
        let kept: Vec<_> = filter.threads(&p).collect();
        assert!(kept.is_empty());
    }

    /// Two threads on the same OS pid with different `.N` suffixes (samply
    /// emits this for parent recorder vs. forked targets) plus distinct
    /// process names. Lets us exercise pid/sub-pid/name selection in one
    /// fixture.
    fn multi_process_fixture() -> Profile {
        let json = r#"{
            "meta": {"interval": 1.0, "startTime": 0.0, "product": "test"},
            "libs": [],
            "threads": [
                {
                    "name": "Main",
                    "processName": "samply",
                    "tid": 1,
                    "pid": 100,
                    "registerTime": 0.0,
                    "stringArray": [],
                    "frameTable": {"length": 0, "address": [], "func": [], "category": [], "subcategory": [], "line": [], "column": [], "nativeSymbol": []},
                    "stackTable": {"length": 0, "frame": [], "category": [], "subcategory": [], "prefix": []},
                    "samples": {"length": 0, "stack": [], "time": []},
                    "funcTable": {"length": 0, "name": [], "isJS": [], "relevantForJS": [], "resource": [], "fileName": [], "lineNumber": [], "columnNumber": []},
                    "resourceTable": {"length": 0, "lib": [], "name": [], "host": [], "type": []},
                    "nativeSymbols": {"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []}
                },
                {
                    "name": "Worker",
                    "processName": "pager-bench",
                    "tid": 2,
                    "pid": "100.1",
                    "registerTime": 0.0,
                    "stringArray": [],
                    "frameTable": {"length": 0, "address": [], "func": [], "category": [], "subcategory": [], "line": [], "column": [], "nativeSymbol": []},
                    "stackTable": {"length": 0, "frame": [], "category": [], "subcategory": [], "prefix": []},
                    "samples": {"length": 0, "stack": [], "time": []},
                    "funcTable": {"length": 0, "name": [], "isJS": [], "relevantForJS": [], "resource": [], "fileName": [], "lineNumber": [], "columnNumber": []},
                    "resourceTable": {"length": 0, "lib": [], "name": [], "host": [], "type": []},
                    "nativeSymbols": {"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []}
                }
            ]
        }"#;
        let raw: RawProfile = serde_json::from_str(json).unwrap();
        Profile::from_raw(raw)
    }

    #[test]
    fn process_name_filter_matches_thread_processname() {
        let p = multi_process_fixture();
        let filter = Filter {
            process: Some(ProcessFilter::Name("pager-bench".into())),
            ..Default::default()
        };
        let kept: Vec<_> = filter.threads(&p).collect();
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn process_pid_with_suffix_distinguishes_subprocesses() {
        let p = multi_process_fixture();
        // Bare 100 → matches both threads (samply sub-pid suffix is informational).
        let bare = Filter {
            process: Some(ProcessFilter::Pid(crate::profile::raw::Pid {
                value: 100,
                suffix: None,
            })),
            ..Default::default()
        };
        assert_eq!(bare.threads(&p).count(), 2);

        // 100.1 → matches only the Worker thread.
        let suffixed = Filter {
            process: Some(ProcessFilter::Pid(crate::profile::raw::Pid {
                value: 100,
                suffix: Some(1),
            })),
            ..Default::default()
        };
        let kept: Vec<_> = suffixed.threads(&p).collect();
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn process_not_found_caps_available_at_error_list_limit() {
        // Build a fixture with `ERROR_LIST_LIMIT + 5` distinct procs so
        // we can assert the cap kicks in and `omitted_process_count`
        // reports the rest.
        let mut threads = String::new();
        let target = ERROR_LIST_LIMIT + 5;
        for i in 0..target {
            if i > 0 {
                threads.push(',');
            }
            // Each process has 1 thread. Sample counts ascend by `i`
            // so the highest-pid process is also the busiest — that
            // tells us the rank-by-samples sort is what's actually
            // selecting which entries survive truncation.
            let samples = (i + 1) * 10;
            let times: Vec<String> = (0..samples).map(|t| t.to_string()).collect();
            let stacks: Vec<&str> = (0..samples).map(|_| "null").collect();
            threads.push_str(&format!(
                r#"{{
                    "name": "t{i}",
                    "processName": "proc-{i}",
                    "tid": {i},
                    "pid": {i},
                    "registerTime": 0.0,
                    "stringArray": [],
                    "frameTable": {{"length": 0, "address": [], "func": [], "category": [], "subcategory": [], "line": [], "column": [], "nativeSymbol": []}},
                    "stackTable": {{"length": 0, "frame": [], "category": [], "subcategory": [], "prefix": []}},
                    "samples": {{"length": {samples}, "stack": [{stacks}], "time": [{times}]}},
                    "funcTable": {{"length": 0, "name": [], "isJS": [], "relevantForJS": [], "resource": [], "fileName": [], "lineNumber": [], "columnNumber": []}},
                    "resourceTable": {{"length": 0, "lib": [], "name": [], "host": [], "type": []}},
                    "nativeSymbols": {{"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []}}
                }}"#,
                stacks = stacks.join(","),
                times = times.join(","),
            ));
        }
        let json = format!(
            r#"{{
                "meta": {{"interval": 1.0, "startTime": 0.0, "product": "test"}},
                "libs": [],
                "threads": [{threads}]
            }}"#
        );
        let raw: RawProfile = serde_json::from_str(&json).unwrap();
        let profile = Profile::from_raw(raw);
        let filter = Filter {
            process: Some(ProcessFilter::Name("nope".into())),
            ..Default::default()
        };
        let err = filter.validate_process(&profile).unwrap_err();
        match err {
            ToolError::ProcessNotFound {
                available_processes,
                omitted_process_count,
                ..
            } => {
                assert_eq!(available_processes.len(), ERROR_LIST_LIMIT);
                assert_eq!(omitted_process_count, target - ERROR_LIST_LIMIT);
                // Busiest first — proc with pid `target-1` has the most
                // samples by construction.
                assert_eq!(available_processes[0].name, format!("proc-{}", target - 1));
                assert!(available_processes[0].samples > available_processes[1].samples);
            }
            other => panic!("expected ProcessNotFound, got {other:?}"),
        }
    }

    #[test]
    fn thread_not_found_caps_available_at_error_list_limit() {
        // Same construction as above but the filter rejects every
        // thread — exercises `available_threads` truncation.
        let mut threads = String::new();
        let target = ERROR_LIST_LIMIT + 3;
        for i in 0..target {
            if i > 0 {
                threads.push(',');
            }
            let samples = (i + 1) * 10;
            let times: Vec<String> = (0..samples).map(|t| t.to_string()).collect();
            let stacks: Vec<&str> = (0..samples).map(|_| "null").collect();
            threads.push_str(&format!(
                r#"{{
                    "name": "t{i}",
                    "processName": "p",
                    "tid": {i},
                    "pid": 1,
                    "registerTime": 0.0,
                    "stringArray": [],
                    "frameTable": {{"length": 0, "address": [], "func": [], "category": [], "subcategory": [], "line": [], "column": [], "nativeSymbol": []}},
                    "stackTable": {{"length": 0, "frame": [], "category": [], "subcategory": [], "prefix": []}},
                    "samples": {{"length": {samples}, "stack": [{stacks}], "time": [{times}]}},
                    "funcTable": {{"length": 0, "name": [], "isJS": [], "relevantForJS": [], "resource": [], "fileName": [], "lineNumber": [], "columnNumber": []}},
                    "resourceTable": {{"length": 0, "lib": [], "name": [], "host": [], "type": []}},
                    "nativeSymbols": {{"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []}}
                }}"#,
                stacks = stacks.join(","),
                times = times.join(","),
            ));
        }
        let json = format!(
            r#"{{
                "meta": {{"interval": 1.0, "startTime": 0.0, "product": "test"}},
                "libs": [],
                "threads": [{threads}]
            }}"#
        );
        let raw: RawProfile = serde_json::from_str(&json).unwrap();
        let profile = Profile::from_raw(raw);
        let filter = Filter {
            thread: Some(ThreadFilter::Name("nope".into())),
            ..Default::default()
        };
        let err = filter.validate_thread(&profile).unwrap_err();
        match err {
            ToolError::ThreadNotFound {
                available_threads,
                omitted_thread_count,
                ..
            } => {
                assert_eq!(available_threads.len(), ERROR_LIST_LIMIT);
                assert_eq!(omitted_thread_count, target - ERROR_LIST_LIMIT);
                // Highest-tid thread has the most samples in this
                // fixture, so it's the leader after rank-by-samples.
                assert_eq!(available_threads[0].tid, (target - 1) as u64);
            }
            other => panic!("expected ThreadNotFound, got {other:?}"),
        }
    }

    #[test]
    fn unmatched_process_returns_empty_and_validates_with_error() {
        let p = multi_process_fixture();
        let filter = Filter {
            process: Some(ProcessFilter::Name("nope".into())),
            ..Default::default()
        };
        assert!(filter.threads(&p).next().is_none());
        let err = filter.validate_process(&p).unwrap_err();
        match err {
            ToolError::ProcessNotFound {
                process,
                available_processes,
                ..
            } => {
                assert_eq!(process, "nope");
                let names: Vec<_> = available_processes.iter().map(|r| &r.name).collect();
                assert!(names.iter().any(|n| n.as_str() == "samply"));
                assert!(names.iter().any(|n| n.as_str() == "pager-bench"));
                let pids: Vec<_> = available_processes.iter().map(|r| &r.pid).collect();
                assert!(pids.iter().any(|s| s.as_str() == "100"));
                assert!(pids.iter().any(|s| s.as_str() == "100.1"));
            }
            other => panic!("expected ProcessNotFound, got {other:?}"),
        }
    }

    /// Two threads sharing the same `processName` (`clusterd`) on different
    /// pids — the over-aggregation case bare-name `process=` filtering is
    /// supposed to warn about.
    fn shared_name_fixture() -> Profile {
        let json = r#"{
            "meta": {"interval": 1.0, "startTime": 0.0, "product": "test"},
            "libs": [],
            "threads": [
                {
                    "name": "main",
                    "processName": "clusterd",
                    "tid": 1,
                    "pid": 100,
                    "registerTime": 0.0,
                    "stringArray": [],
                    "frameTable": {"length": 0, "address": [], "func": [], "category": [], "subcategory": [], "line": [], "column": [], "nativeSymbol": []},
                    "stackTable": {"length": 0, "frame": [], "category": [], "subcategory": [], "prefix": []},
                    "samples": {"length": 0, "stack": [], "time": []},
                    "funcTable": {"length": 0, "name": [], "isJS": [], "relevantForJS": [], "resource": [], "fileName": [], "lineNumber": [], "columnNumber": []},
                    "resourceTable": {"length": 0, "lib": [], "name": [], "host": [], "type": []},
                    "nativeSymbols": {"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []}
                },
                {
                    "name": "main",
                    "processName": "clusterd",
                    "tid": 2,
                    "pid": 200,
                    "registerTime": 0.0,
                    "stringArray": [],
                    "frameTable": {"length": 0, "address": [], "func": [], "category": [], "subcategory": [], "line": [], "column": [], "nativeSymbol": []},
                    "stackTable": {"length": 0, "frame": [], "category": [], "subcategory": [], "prefix": []},
                    "samples": {"length": 0, "stack": [], "time": []},
                    "funcTable": {"length": 0, "name": [], "isJS": [], "relevantForJS": [], "resource": [], "fileName": [], "lineNumber": [], "columnNumber": []},
                    "resourceTable": {"length": 0, "lib": [], "name": [], "host": [], "type": []},
                    "nativeSymbols": {"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []}
                }
            ]
        }"#;
        let raw: RawProfile = serde_json::from_str(json).unwrap();
        Profile::from_raw(raw)
    }

    #[test]
    fn bare_name_multi_match_returns_pids_when_more_than_one() {
        let p = shared_name_fixture();
        let filter = Filter {
            process: Some(ProcessFilter::Name("clusterd".into())),
            ..Default::default()
        };
        let matched = filter
            .bare_name_multi_match(&p)
            .expect("expected Some when bare name resolves to >1 pid");
        assert_eq!(matched.len(), 2);
        let pids: Vec<_> = matched.iter().map(|r| r.pid.as_str()).collect();
        assert!(pids.contains(&"100"), "missing pid 100 in {pids:?}");
        assert!(pids.contains(&"200"), "missing pid 200 in {pids:?}");
        assert!(matched.iter().all(|r| r.name == "clusterd"));
    }

    #[test]
    fn bare_name_multi_match_returns_none_for_single_pid() {
        let p = multi_process_fixture(); // each name maps to one pid here
        let filter = Filter {
            process: Some(ProcessFilter::Name("samply".into())),
            ..Default::default()
        };
        assert!(filter.bare_name_multi_match(&p).is_none());
    }

    #[test]
    fn bare_name_multi_match_returns_none_for_pid_filter() {
        // Pid filtering is already pid-precise, so we must not warn even
        // when a bare pid (suffix=None) covers multiple sub-pids.
        let p = shared_name_fixture();
        let filter = Filter {
            process: Some(ProcessFilter::Pid(crate::profile::raw::Pid {
                value: 100,
                suffix: None,
            })),
            ..Default::default()
        };
        assert!(filter.bare_name_multi_match(&p).is_none());
    }

    #[test]
    fn bare_name_multi_match_returns_none_when_no_filter() {
        let p = shared_name_fixture();
        let filter = Filter::default();
        assert!(filter.bare_name_multi_match(&p).is_none());
    }

    #[test]
    fn validate_time_range_passes_when_unset_or_overlapping() {
        // linear_chain.json has 100 samples at 1ms cadence, so end ≈ 99ms.
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/linear_chain.json")).unwrap();
        let p = Profile::from_raw(raw);

        Filter::default().validate_time_range(&p).unwrap();
        Filter {
            time_range: Some([10.0, 20.0]),
            ..Default::default()
        }
        .validate_time_range(&p)
        .unwrap();
        // Partial overlap (extends past end) is OK — clamp_silently handles it.
        Filter {
            time_range: Some([90.0, 9_999_999.0]),
            ..Default::default()
        }
        .validate_time_range(&p)
        .unwrap();
    }

    #[test]
    fn validate_time_range_errors_when_no_overlap() {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/linear_chain.json")).unwrap();
        let p = Profile::from_raw(raw);

        let err = Filter {
            time_range: Some([5_000.0, 6_000.0]),
            ..Default::default()
        }
        .validate_time_range(&p)
        .unwrap_err();
        match err {
            ToolError::OutOfBounds {
                original_range,
                clamped_range,
            } => {
                assert_eq!(original_range, [5_000.0, 6_000.0]);
                // No overlap → clamped is reported as the empty range
                // [start, start] anchored at the requested start, clamped
                // into the profile's [0, duration].
                assert!(clamped_range[0] >= 0.0);
                assert!(clamped_range[1] <= 100.0);
            }
            other => panic!("expected OutOfBounds, got {other:?}"),
        }
    }

    #[test]
    fn compose_under_scope_passthrough_when_scope_unset() {
        let scope = Filter::default();
        let request = Filter {
            thread: Some(ThreadFilter::Tid(7)),
            ..Default::default()
        };
        let merged = request.clone().compose_under_scope(&scope).unwrap();
        // Field-by-field equality: clone-and-compare avoids needing PartialEq on Filter.
        assert!(matches!(merged.thread, Some(ThreadFilter::Tid(7))));
    }

    #[test]
    fn compose_under_scope_inherits_when_request_unset() {
        let scope = Filter {
            thread: Some(ThreadFilter::Tid(7)),
            time_range: Some([10.0, 100.0]),
            ..Default::default()
        };
        let merged = Filter::default().compose_under_scope(&scope).unwrap();
        assert!(matches!(merged.thread, Some(ThreadFilter::Tid(7))));
        assert_eq!(merged.time_range, Some([10.0, 100.0]));
    }

    #[test]
    fn compose_under_scope_accepts_equal_thread() {
        let scope = Filter {
            thread: Some(ThreadFilter::Tid(7)),
            ..Default::default()
        };
        let req = Filter {
            thread: Some(ThreadFilter::Tid(7)),
            ..Default::default()
        };
        req.compose_under_scope(&scope).unwrap();
    }

    #[test]
    fn compose_under_scope_rejects_thread_conflict() {
        let scope = Filter {
            thread: Some(ThreadFilter::Tid(7)),
            ..Default::default()
        };
        let req = Filter {
            thread: Some(ThreadFilter::Tid(8)),
            ..Default::default()
        };
        let err = req.compose_under_scope(&scope).unwrap_err();
        match err {
            ToolError::InvalidValue { field, hint, .. } => {
                assert_eq!(field, "thread");
                assert!(hint.unwrap().contains("scope pins"));
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn compose_under_scope_rejects_thread_kind_mismatch() {
        let scope = Filter {
            thread: Some(ThreadFilter::Name("worker".into())),
            ..Default::default()
        };
        let req = Filter {
            thread: Some(ThreadFilter::Tid(7)),
            ..Default::default()
        };
        let err = req.compose_under_scope(&scope).unwrap_err();
        assert!(matches!(err, ToolError::InvalidValue { .. }));
    }

    #[test]
    fn compose_under_scope_accepts_suffixed_pid_under_bare_scope() {
        let scope = Filter {
            process: Some(ProcessFilter::Pid(crate::profile::raw::Pid {
                value: 100,
                suffix: None,
            })),
            ..Default::default()
        };
        let req = Filter {
            process: Some(ProcessFilter::Pid(crate::profile::raw::Pid {
                value: 100,
                suffix: Some(1),
            })),
            ..Default::default()
        };
        let merged = req.compose_under_scope(&scope).unwrap();
        match merged.process {
            Some(ProcessFilter::Pid(p)) => {
                assert_eq!(p.value, 100);
                assert_eq!(p.suffix, Some(1));
            }
            other => panic!("expected pinned suffixed pid, got {other:?}"),
        }
    }

    #[test]
    fn compose_under_scope_rejects_widening_pid() {
        let scope = Filter {
            process: Some(ProcessFilter::Pid(crate::profile::raw::Pid {
                value: 100,
                suffix: Some(1),
            })),
            ..Default::default()
        };
        // Bare pid is wider than a suffixed scope pid → rejected.
        let req = Filter {
            process: Some(ProcessFilter::Pid(crate::profile::raw::Pid {
                value: 100,
                suffix: None,
            })),
            ..Default::default()
        };
        let err = req.compose_under_scope(&scope).unwrap_err();
        assert!(matches!(err, ToolError::InvalidValue { .. }));
    }

    #[test]
    fn compose_under_scope_accepts_inner_time_range() {
        let scope = Filter {
            time_range: Some([10.0, 100.0]),
            ..Default::default()
        };
        let req = Filter {
            time_range: Some([20.0, 50.0]),
            ..Default::default()
        };
        let merged = req.compose_under_scope(&scope).unwrap();
        assert_eq!(merged.time_range, Some([20.0, 50.0]));
    }

    #[test]
    fn compose_under_scope_rejects_widening_time_range() {
        let scope = Filter {
            time_range: Some([10.0, 100.0]),
            ..Default::default()
        };
        let req = Filter {
            time_range: Some([5.0, 200.0]),
            ..Default::default()
        };
        let err = req.compose_under_scope(&scope).unwrap_err();
        match err {
            ToolError::InvalidValue { field, .. } => assert_eq!(field, "time_range"),
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn validate_time_range_errors_when_start_after_end() {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/linear_chain.json")).unwrap();
        let p = Profile::from_raw(raw);

        let err = Filter {
            time_range: Some([50.0, 10.0]),
            ..Default::default()
        }
        .validate_time_range(&p)
        .unwrap_err();
        assert!(matches!(err, ToolError::OutOfBounds { .. }));
    }
}
