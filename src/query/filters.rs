//! Reusable filter abstraction for thread/process/time selection.

#![allow(dead_code)]

use crate::error::{ProcessRef, ThreadRef, ToolError};
use crate::profile::raw::Pid;
use crate::profile::{Profile, ThreadHandle};

#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub thread: Option<ThreadFilter>,
    pub process: Option<ProcessFilter>,
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
        let available_threads = profile
            .threads()
            .map(|t| ThreadRef {
                tid: t.tid(),
                name: t.name().unwrap_or("").to_owned(),
            })
            .collect();
        let thread = match self.thread.as_ref().unwrap() {
            ThreadFilter::Tid(t) => t.to_string(),
            ThreadFilter::Name(n) => n.clone(),
        };
        Err(ToolError::ThreadNotFound {
            thread,
            available_threads,
        })
    }

    /// Validate process filter; if it matches no threads, return a
    /// `process_not_found` error listing every distinct `(pid, name)` in
    /// the profile so the caller can pick a real one. Mirrors
    /// [`Self::validate_thread`].
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
        let mut seen: std::collections::BTreeMap<Pid, String> = std::collections::BTreeMap::new();
        for t in profile.threads() {
            let entry = seen.entry(t.pid_full()).or_default();
            if entry.is_empty()
                && let Some(name) = t.process_name().filter(|s| !s.is_empty())
            {
                *entry = name.to_owned();
            }
        }
        let available_processes = seen
            .into_iter()
            .map(|(pid, name)| ProcessRef {
                pid: pid.to_string(),
                name,
            })
            .collect();
        let process = match pf {
            ProcessFilter::Pid(p) => p.to_string(),
            ProcessFilter::Name(n) => n.clone(),
        };
        Err(ToolError::ProcessNotFound {
            process,
            available_processes,
        })
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
}
