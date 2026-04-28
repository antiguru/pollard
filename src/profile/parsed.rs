//! Ergonomic accessors over the raw profile tables.
//!
//! This layer owns the `RawProfile` and exposes:
//! - threads + processes (with tid/pid/name)
//! - frame lookup: `Profile::frame_info(thread_handle, frame_index)` →
//!   `{function_name, module_name, file?, line?, address?}`
//! - stack walking: iterate samples and walk their stack chains
//! - duration / sample rate
//!
//! Keep this read-only and `Sync`; query functions must be able to share an
//! `&Profile` across threads.

#![allow(dead_code)]

use crate::profile::raw::{RawLib, RawProfile, RawThread};

pub struct Profile {
    raw: RawProfile,
    /// Flattened (process, thread) tuples for top-level enumeration.
    threads: Vec<ThreadHandle>,
}

#[derive(Clone, Copy, Debug)]
pub struct ProcessHandle {
    pub pid: u64,
    pub process_idx: Option<usize>, // None means "root profile is itself a process"
}

#[derive(Clone, Copy, Debug)]
pub struct ThreadHandle {
    pub process: ProcessHandle,
    pub thread_idx: usize,
}

#[derive(Debug, Clone)]
pub struct FrameInfo<'a> {
    pub function_name: &'a str,
    pub module_name: Option<&'a str>,
    pub file: Option<&'a str>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub address: Option<i64>,
    pub lib: Option<&'a RawLib>,
}

pub struct ThreadView<'a> {
    profile: &'a Profile,
    handle: ThreadHandle,
}

impl Profile {
    pub fn from_raw(raw: RawProfile) -> Self {
        let mut threads = Vec::new();
        // Top-level threads belong to the implicit "root" process.
        for (i, _) in raw.threads.iter().enumerate() {
            threads.push(ThreadHandle {
                process: ProcessHandle {
                    pid: 0,
                    process_idx: None,
                },
                thread_idx: i,
            });
        }
        // Sub-process threads.
        for (pi, p) in raw.processes.iter().enumerate() {
            for (i, t) in p.threads.iter().enumerate() {
                threads.push(ThreadHandle {
                    process: ProcessHandle {
                        pid: t.pid,
                        process_idx: Some(pi),
                    },
                    thread_idx: i,
                });
            }
        }
        Self { raw, threads }
    }

    pub fn meta(&self) -> &crate::profile::raw::RawMeta {
        &self.raw.meta
    }

    pub fn threads(&self) -> impl Iterator<Item = ThreadView<'_>> + '_ {
        self.threads.iter().map(move |&h| ThreadView {
            profile: self,
            handle: h,
        })
    }

    pub fn duration_ms(&self) -> f64 {
        self.threads()
            .filter_map(|t| {
                let times = t.raw().samples.absolute_times();
                Some(*times.last()? - *times.first()?)
            })
            .fold(0.0_f64, f64::max)
    }

    /// Resolve a thread handle back to the raw thread.
    pub(crate) fn raw_thread(&self, handle: ThreadHandle) -> &RawThread {
        match handle.process.process_idx {
            None => &self.raw.threads[handle.thread_idx],
            Some(pi) => &self.raw.processes[pi].threads[handle.thread_idx],
        }
    }

    /// Look up the lib for a `RawResourceTable.lib` index.
    pub(crate) fn lib(&self, idx: usize) -> Option<&RawLib> {
        self.raw.libs.get(idx)
    }

    /// Look up frame info for a given thread + frame index.
    pub fn frame_info(&self, handle: ThreadHandle, frame_idx: usize) -> Option<FrameInfo<'_>> {
        let thread = self.raw_thread(handle);
        let func_idx = *thread.frame_table.func.get(frame_idx)?;
        let func_name_idx = *thread.func_table.name.get(func_idx)?;
        let function_name = thread.string_array.get(func_name_idx)?.as_str();

        let resource_idx = thread
            .func_table
            .resource
            .get(func_idx)
            .copied()
            .unwrap_or(-1);
        let lib = if resource_idx >= 0 {
            thread
                .resource_table
                .lib
                .get(resource_idx as usize)
                .copied()
                .flatten()
                .and_then(|li| self.lib(li))
        } else {
            None
        };
        let module_name = lib.and_then(|l| l.name.as_deref());

        let file = thread
            .func_table
            .file_name
            .get(func_idx)
            .and_then(|opt| opt.and_then(|si| thread.string_array.get(si).map(String::as_str)));

        let line = thread.frame_table.line.get(frame_idx).copied().flatten();
        let column = thread.frame_table.column.get(frame_idx).copied().flatten();
        let address = thread.frame_table.address.get(frame_idx).copied();
        let address = address.filter(|&a| a >= 0);

        Some(FrameInfo {
            function_name,
            module_name,
            file,
            line,
            column,
            address,
            lib,
        })
    }

    /// Walk the frame indices for a stack from leaf to root.
    pub fn walk_stack(
        &self,
        handle: ThreadHandle,
        stack_idx: usize,
    ) -> impl Iterator<Item = usize> + '_ {
        let thread = self.raw_thread(handle);
        let mut current = Some(stack_idx);
        std::iter::from_fn(move || {
            let s = current?;
            let frame = *thread.stack_table.frame.get(s)?;
            current = thread.stack_table.prefix.get(s).copied().flatten();
            Some(frame)
        })
    }
}

impl<'a> ThreadView<'a> {
    pub fn handle(&self) -> ThreadHandle {
        self.handle
    }

    pub fn raw(&self) -> &'a RawThread {
        self.profile.raw_thread(self.handle)
    }

    pub fn tid(&self) -> u64 {
        self.raw().tid
    }

    pub fn pid(&self) -> u64 {
        self.raw().pid
    }

    pub fn name(&self) -> Option<&'a str> {
        self.raw().name.as_deref()
    }

    pub fn samples(&self) -> &'a crate::profile::raw::RawSampleTable {
        &self.raw().samples
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::raw::RawProfile;

    fn fixture() -> Profile {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/minimal_profile.json"))
                .unwrap();
        Profile::from_raw(raw)
    }

    #[test]
    fn enumerates_threads() {
        let p = fixture();
        let threads: Vec<_> = p.threads().collect();
        assert_eq!(threads.len(), 1);
        let t = &threads[0];
        assert_eq!(t.tid(), 1);
        assert_eq!(t.name(), Some("Main"));
    }

    #[test]
    fn duration_ms_is_zero_for_empty_profile() {
        let p = fixture();
        assert_eq!(p.duration_ms(), 0.0);
    }
}
