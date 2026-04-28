//! Reusable filter abstraction for thread/process/time selection.

#![allow(dead_code)]

use crate::error::{ThreadRef, ToolError};
use crate::profile::{Profile, ThreadHandle};

#[allow(unused_imports)]
pub use crate::error::ProcessRef;

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
    Pid(u64),
    Name(String),
}

impl Filter {
    /// Returns thread handles matching the filter. Empty if nothing matches.
    pub fn threads<'a>(&'a self, profile: &'a Profile) -> impl Iterator<Item = ThreadHandle> + 'a {
        profile.threads().filter_map(move |t| {
            if let Some(pf) = &self.process {
                let ok = match pf {
                    ProcessFilter::Pid(p) => t.pid() == *p,
                    ProcessFilter::Name(_) => false, // TODO: wire process names
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
        Err(ToolError::ThreadNotFound { thread, available_threads })
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
    use crate::profile::{raw::RawProfile, Profile};

    fn fixture() -> Profile {
        let raw: RawProfile = serde_json::from_str(include_str!(
            "../../tests/fixtures/minimal_profile.json"
        ))
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
}
