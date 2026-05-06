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

use crate::profile::event_source::EventSource;
use crate::profile::raw::{InlineFrame, Pid, RawLib, RawProfile, RawThread};

pub struct Profile {
    raw: std::sync::Arc<RawProfile>,
    /// Frame-chain transforms applied lazily by `resolved_chain`.
    /// Default = identity; views construct with non-default transforms
    /// via [`Self::view`].
    transforms: crate::profile::transforms::Transforms,
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

/// One frame in a transformed root-to-leaf chain produced by
/// [`Profile::resolved_chain`]. Owns its strings so callers can hold it
/// across `&Profile` borrows; the chain has already had hide / rename /
/// collapse transforms applied, so aggregators can consume it directly
/// without re-doing per-frame transform checks.
#[derive(Debug, Clone)]
pub struct ResolvedFrame {
    pub function: String,
    pub module: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub address: Option<i64>,
}

pub struct ThreadView<'a> {
    profile: &'a Profile,
    handle: ThreadHandle,
}

impl Profile {
    pub fn from_raw(raw: RawProfile) -> Self {
        Self::new_inner(
            std::sync::Arc::new(raw),
            crate::profile::transforms::Transforms::default(),
        )
    }

    /// Build a view profile that shares the base's raw tables but
    /// applies its own transforms. The thread enumeration is identical
    /// to the base — views never add or remove threads.
    pub fn view(base: &Self, transforms: crate::profile::transforms::Transforms) -> Self {
        Self::new_inner(std::sync::Arc::clone(&base.raw), transforms)
    }

    fn new_inner(
        raw: std::sync::Arc<RawProfile>,
        transforms: crate::profile::transforms::Transforms,
    ) -> Self {
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
                        pid: t.pid.value,
                        process_idx: Some(pi),
                    },
                    thread_idx: i,
                });
            }
        }
        Self {
            raw,
            transforms,
            threads,
        }
    }

    /// Returns the transform set applied by `resolved_chain`. Identity
    /// for base profiles.
    pub fn transforms(&self) -> &crate::profile::transforms::Transforms {
        &self.transforms
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

    /// Wrap a previously-issued [`ThreadHandle`] back into a
    /// [`ThreadView`]. Used when an aggregator iterates handles via
    /// [`crate::query::filters::Filter::threads`] but still needs
    /// per-thread metadata (pid, name, …) without re-walking and
    /// re-filtering [`Self::threads`].
    pub fn thread_view(&self, handle: ThreadHandle) -> ThreadView<'_> {
        ThreadView {
            profile: self,
            handle,
        }
    }

    pub fn duration_ms(&self) -> f64 {
        self.threads()
            .filter_map(|t| {
                let times = t.raw().samples.absolute_times();
                Some(*times.last()? - *times.first()?)
            })
            .fold(0.0_f64, f64::max)
    }

    /// Earliest sample timestamp across all threads — the anchor that
    /// defines "profile-relative time zero".
    ///
    /// Sample timestamps in samply's processed-profile schema are
    /// boot-relative (e.g. tens of millions of ms), so `time_range`
    /// filter args (documented as profile-relative) must be offset by
    /// this value before they can be compared with raw sample times.
    /// `summary.profile_start_ms` exposes the same value so callers
    /// can convert in either direction.
    ///
    /// Returns `0.0` when the profile carries no timed samples — keeps
    /// the gating loop in [`Self::stack_indices`] arithmetic-clean
    /// without a special case at the call site.
    pub fn start_time_ms(&self) -> f64 {
        let min = self
            .threads()
            .filter_map(|t| t.raw().samples.absolute_times().first().copied())
            .fold(f64::INFINITY, f64::min);
        if min.is_finite() { min } else { 0.0 }
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

    /// All libraries across the root profile and any sub-processes.
    /// Order is root libs first, then per-process libs in declaration order.
    pub fn all_libs(&self) -> impl Iterator<Item = &RawLib> + '_ {
        self.raw
            .libs
            .iter()
            .chain(self.raw.processes.iter().flat_map(|p| p.libs.iter()))
    }

    /// Distinct, sorted, non-empty `lib.name` values across every
    /// library in the profile. Populates
    /// `module_not_found.available_modules` so callers see real
    /// candidates after a `module=` typo instead of falling through
    /// to a misleading `function_not_found`.
    pub fn module_names(&self) -> Vec<String> {
        let mut names: std::collections::BTreeSet<String> = Default::default();
        for lib in self.all_libs() {
            if let Some(n) = lib.name.as_deref().filter(|s| !s.is_empty()) {
                names.insert(n.to_owned());
            }
        }
        names.into_iter().collect()
    }

    /// Inline-call chain attached to a native frame (innermost-first).
    /// Empty when the frame has no DWARF inline records or symbolication
    /// hasn't run yet.
    pub fn inline_chain(&self, handle: ThreadHandle, frame_idx: usize) -> &[InlineFrame] {
        self.raw_thread(handle)
            .inline_chains
            .get(frame_idx)
            .map(Vec::as_slice)
            .unwrap_or(&[])
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

    /// Resolve `stack_idx` into a frame chain (root-to-leaf), applying
    /// the profile's transforms. When `expand_inlines` is true, native
    /// frames fan out into their DWARF inline chain (outer-to-inner)
    /// before transforms apply, so hide/rename rules see inline frames
    /// as first-class entries.
    pub fn resolved_chain(
        &self,
        handle: ThreadHandle,
        stack_idx: usize,
        expand_inlines: bool,
    ) -> Vec<ResolvedFrame> {
        // walk_stack iterates leaf-to-root; collect-then-reverse keeps
        // order explicit (root-to-leaf) so transforms downstream see
        // chains in execution order.
        let leaf_first: Vec<usize> = self.walk_stack(handle, stack_idx).collect();
        let mut chain: Vec<ResolvedFrame> = Vec::with_capacity(leaf_first.len());
        for &fi in leaf_first.iter().rev() {
            let Some(info) = self.frame_info(handle, fi) else {
                continue;
            };
            chain.push(ResolvedFrame {
                function: info.function_name.to_owned(),
                module: info.module_name.map(str::to_owned),
                file: info.file.map(str::to_owned),
                line: info.line,
                column: info.column,
                address: info.address,
            });
            if expand_inlines {
                // Inline chain is innermost-first; emit outer-to-inner
                // so the deepest inline becomes the leaf.
                let module = info.module_name.map(str::to_owned);
                for inl in self.inline_chain(handle, fi).iter().rev() {
                    chain.push(ResolvedFrame {
                        function: inl.function.clone(),
                        module: module.clone(),
                        file: inl.file.clone(),
                        line: inl.line,
                        column: None,
                        address: None,
                    });
                }
            }
        }
        self.apply_transforms(&mut chain);
        chain
    }

    fn apply_transforms(&self, chain: &mut Vec<ResolvedFrame>) {
        let t = &self.transforms;
        if t.is_identity() {
            return;
        }
        // Hide first so rename and collapse don't see frames the user
        // told us to drop.
        if !t.hide_frames.is_empty() || !t.hide_modules.is_empty() {
            chain.retain(|f| {
                if t.hide_frames.iter().any(|m| m.matches(&f.function)) {
                    return false;
                }
                if let Some(m) = f.module.as_deref()
                    && t.hide_modules.iter().any(|mm| mm.matches(m))
                {
                    return false;
                }
                true
            });
        }
        if !t.rename.is_empty() {
            for f in chain.iter_mut() {
                // Sequential apply: each rule sees the result of prior
                // renames, so users can write a normalize-then-merge
                // pipeline (`foo<.*> => foo`, then `foo => bar`) in one
                // rename list. View composition relies on this — when a
                // child view stacks on a parent, the two layers'
                // rename rules concatenate in parent-first order and
                // the child's rules fire on top of the parent's
                // renamed frames.
                for rule in &t.rename {
                    if rule.matcher.matches(&f.function) {
                        f.function = rule.matcher.replace(&f.function, &rule.replacement);
                    }
                }
            }
        }
        if t.collapse_recursion {
            collapse_cycles(chain, MAX_CYCLE_LEN);
        }
    }
}

/// Maximum cycle length `collapse_recursion` will fold. Covers
/// depth-3 interleaved recursion (e.g. timely's `Subgraph::schedule
/// → PerOperatorState::schedule → Subgraph::schedule …`) with
/// headroom; longer cycles are left alone so unrelated work isn't
/// folded into a phantom recurrence.
pub(crate) const MAX_CYCLE_LEN: usize = 8;

/// Collapse repeating adjacent cycles in `chain`, in place.
///
/// For each cycle length `k` from `max_len` down to `1`, scans
/// left-to-right and drains a duplicate adjacent run of length `k`
/// when found, staying at the same position so a tripled or
/// quadrupled cycle collapses fully in one pass. Long-to-short
/// ordering means a length-3 cycle wins over a coincidentally-equal
/// length-1 sub-pattern when both apply. Equality is by
/// `(function, module)` only; per-frame metadata (file/line/address)
/// on the surviving copy is whichever the first occurrence carried.
pub(crate) fn collapse_cycles(chain: &mut Vec<ResolvedFrame>, max_len: usize) {
    fn frames_eq(a: &ResolvedFrame, b: &ResolvedFrame) -> bool {
        a.function == b.function && a.module == b.module
    }
    fn slice_eq(a: &[ResolvedFrame], b: &[ResolvedFrame]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| frames_eq(x, y))
    }
    for k in (1..=max_len).rev() {
        let mut i = 0;
        while i + 2 * k <= chain.len() {
            if slice_eq(&chain[i..i + k], &chain[i + k..i + 2 * k]) {
                chain.drain(i + k..i + 2 * k);
                // Don't advance — same window may match again.
            } else {
                i += 1;
            }
        }
    }
}

impl Profile {
    /// Iterate the stack-table indices that this thread contributes for
    /// the given event source. `Some(idx)` per sample/marker, `None` to
    /// skip (matching the existing `samples.stack: Vec<Option<usize>>`
    /// shape so callers can stay in their per-stack loop).
    ///
    /// `time_range`, when set, gates each yielded item by its per-sample
    /// timestamp ([`crate::profile::raw::RawSampleTable::absolute_times`]
    /// for [`EventSource::Samples`], `markers.start_time` for
    /// [`EventSource::Marker`]). The range is interpreted relative to
    /// [`Self::start_time_ms`] — i.e. profile-zero — to match the
    /// public filter contract; raw sample timestamps are offset before
    /// the comparison so callers never have to know whether the
    /// profile uses boot-relative or zero-anchored timestamps. Items
    /// outside the inclusive range are dropped entirely. Pass `None`
    /// for the unfiltered behavior.
    pub fn stack_indices<'a>(
        &'a self,
        handle: ThreadHandle,
        source: &'a EventSource,
        time_range: Option<[f64; 2]>,
    ) -> Box<dyn Iterator<Item = Option<usize>> + 'a> {
        let raw = self.raw_thread(handle);
        let start = self.start_time_ms();
        // Closure copies the range so each branch's iterator can move
        // it freely without borrowing `time_range` itself. Sample times
        // are offset by `start` so a [s, e] filter behaves the same way
        // regardless of whether the profile's clock is boot-relative
        // (samply) or already zero-anchored (synthetic fixtures).
        let in_range = move |t: f64| match time_range {
            None => true,
            Some([s, e]) => {
                let rel = t - start;
                rel >= s && rel <= e
            }
        };
        match source {
            EventSource::Samples => {
                // Materialize absolute times once per thread; samply
                // emits either `time` directly or `timeDeltas`, and
                // `absolute_times` unifies the two.
                let times = raw.samples.absolute_times();
                Box::new(
                    raw.samples
                        .stack
                        .iter()
                        .copied()
                        .enumerate()
                        .filter_map(move |(i, s)| {
                            // A sample with no recorded timestamp can't
                            // be gated; with a time-range filter set we
                            // conservatively drop unstamped samples —
                            // there's no way to tell whether they belong
                            // in the slice.
                            let t = *times.get(i)?;
                            if !in_range(t) {
                                return None;
                            }
                            Some(s)
                        }),
                )
            }
            EventSource::Marker(name) => {
                // Resolve the marker name to its string-array index
                // *once* per thread; markers without a `cause.stack`
                // payload are yielded as `None` so the caller's
                // "skip None" branch handles them uniformly with samples
                // that have no stack.
                let str_idx = raw.string_array.iter().position(|s| s == name);
                match str_idx {
                    None => Box::new(std::iter::empty()),
                    Some(target) => Box::new(raw.markers.name.iter().enumerate().filter_map(
                        move |(i, &n)| {
                            // Skip non-matching markers entirely so we
                            // yield exactly one item per *matching*
                            // marker. Text-only matches still appear,
                            // as `None`, so the aggregator can tell
                            // "no stack to attribute to" apart from
                            // "marker isn't ours".
                            if n != target {
                                return None;
                            }
                            // Gate by the marker's start_time when a
                            // range is set; missing entries are
                            // conservatively dropped (same rationale as
                            // the unstamped-sample branch).
                            let t = raw.markers.start_time.get(i).copied()?;
                            if !in_range(t) {
                                return None;
                            }
                            Some(
                                raw.markers
                                    .data
                                    .get(i)
                                    .and_then(|d| d.as_ref())
                                    .and_then(|d| d.cause.as_ref())
                                    .map(|c| c.stack),
                            )
                        },
                    )),
                }
            }
        }
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
        self.raw().pid.value
    }

    /// Full pid including the `.N` sub-process suffix when present.
    /// Use this for grouping; use [`Self::pid`] for filter-by-integer matches.
    pub fn pid_full(&self) -> Pid {
        self.raw().pid
    }

    pub fn name(&self) -> Option<&'a str> {
        self.raw().name.as_deref()
    }

    pub fn process_name(&self) -> Option<&'a str> {
        self.raw().process_name.as_deref()
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

    #[test]
    fn view_shares_raw_tables_and_threads() {
        let base = fixture();
        let view = Profile::view(&base, crate::profile::transforms::Transforms::default());
        assert_eq!(base.threads().count(), view.threads().count());
        assert!(view.transforms().is_identity());
        // Same Arc backing → same raw pointer.
        assert!(std::sync::Arc::ptr_eq(&base.raw, &view.raw));
    }

    #[test]
    fn resolved_chain_with_identity_matches_walk_stack() {
        use crate::profile::raw::RawProfile;
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_functions.json")).unwrap();
        let p = Profile::from_raw(raw);
        let handle = p.threads().next().unwrap().handle();
        let stack_idx = p
            .stack_indices(
                handle,
                &crate::profile::event_source::EventSource::Samples,
                None,
            )
            .find_map(|s| s)
            .unwrap();

        let manual: Vec<String> = p
            .walk_stack(handle, stack_idx)
            .filter_map(|fi| p.frame_info(handle, fi).map(|i| i.function_name.to_owned()))
            .collect::<Vec<_>>()
            .into_iter()
            .rev() // walk_stack is leaf-to-root; resolved_chain is root-to-leaf.
            .collect();
        let resolved: Vec<String> = p
            .resolved_chain(handle, stack_idx, false)
            .into_iter()
            .map(|f| f.function)
            .collect();
        assert_eq!(manual, resolved);
    }

    #[test]
    fn resolved_chain_hides_matching_function() {
        use crate::matching::FunctionMatcher;
        use crate::profile::raw::RawProfile;
        use crate::profile::transforms::Transforms;
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_functions.json")).unwrap();
        let base = Profile::from_raw(raw);
        let mut t = Transforms::default();
        // Substring matcher (default form for plain patterns without `re:` prefix).
        t.hide_frames.push(FunctionMatcher::new("hot").unwrap());
        let view = Profile::view(&base, t);
        let handle = view.threads().next().unwrap().handle();
        for stack_opt in view.stack_indices(
            handle,
            &crate::profile::event_source::EventSource::Samples,
            None,
        ) {
            let Some(stack_idx) = stack_opt else { continue };
            let names: Vec<String> = view
                .resolved_chain(handle, stack_idx, false)
                .into_iter()
                .map(|f| f.function)
                .collect();
            assert!(
                !names.iter().any(|n| n == "hot"),
                "found hidden frame: {names:?}"
            );
        }
    }

    #[test]
    fn resolved_chain_collapses_consecutive_recursion() {
        use crate::profile::raw::RawProfile;
        use crate::profile::transforms::Transforms;
        let json = r#"{
          "meta": {"interval": 1.0, "startTime": 0.0, "product": "test"},
          "libs": [],
          "threads": [{
            "name": "Main", "tid": 1, "pid": 1, "registerTime": 0.0,
            "stringArray": ["main", "recurse"],
            "frameTable": {"length": 2, "address": [-1, -1], "func": [0, 1], "category": [0, 0], "subcategory": [0, 0], "line": [null, null], "column": [null, null], "nativeSymbol": [null, null]},
            "stackTable": {"length": 4, "frame": [0, 1, 1, 1], "category": [0, 0, 0, 0], "subcategory": [0, 0, 0, 0], "prefix": [null, 0, 1, 2]},
            "samples": {"length": 1, "stack": [3], "time": [0.0]},
            "funcTable": {"length": 2, "name": [0, 1], "isJS": [false, false], "relevantForJS": [false, false], "resource": [-1, -1], "fileName": [null, null], "lineNumber": [null, null], "columnNumber": [null, null]},
            "resourceTable": {"length": 0, "lib": [], "name": [], "host": [], "type": []},
            "nativeSymbols": {"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []}
          }]
        }"#;
        let raw: RawProfile = serde_json::from_str(json).unwrap();
        let base = Profile::from_raw(raw);
        let view = Profile::view(
            &base,
            Transforms {
                collapse_recursion: true,
                ..Default::default()
            },
        );
        let handle = view.threads().next().unwrap().handle();
        let stack_idx = view
            .stack_indices(
                handle,
                &crate::profile::event_source::EventSource::Samples,
                None,
            )
            .find_map(|s| s)
            .unwrap();
        let names: Vec<String> = view
            .resolved_chain(handle, stack_idx, false)
            .into_iter()
            .map(|f| f.function)
            .collect();
        assert_eq!(names, vec!["main".to_owned(), "recurse".to_owned()]);
    }

    #[test]
    fn resolved_chain_renames_apply_sequentially() {
        use crate::matching::FunctionMatcher;
        use crate::profile::raw::RawProfile;
        use crate::profile::transforms::{RenameRule, Transforms};
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_functions.json")).unwrap();
        let base = Profile::from_raw(raw);
        // First rule: `hot` -> `mid`. Second rule: `mid` -> `final`.
        // Sequential apply means a frame named `hot` ends up `final` —
        // i.e. the second rule sees the result of the first. This is
        // also what view stacking depends on: each layer's renames see
        // the result of the layers below.
        let view = Profile::view(
            &base,
            Transforms {
                rename: vec![
                    RenameRule {
                        matcher: FunctionMatcher::new("hot").unwrap(),
                        replacement: "mid".to_owned(),
                    },
                    RenameRule {
                        matcher: FunctionMatcher::new("mid").unwrap(),
                        replacement: "final".to_owned(),
                    },
                ],
                ..Default::default()
            },
        );
        let handle = view.threads().next().unwrap().handle();
        let mut saw_final = false;
        for stack_opt in view.stack_indices(
            handle,
            &crate::profile::event_source::EventSource::Samples,
            None,
        ) {
            let Some(stack_idx) = stack_opt else { continue };
            let names: Vec<String> = view
                .resolved_chain(handle, stack_idx, false)
                .into_iter()
                .map(|f| f.function)
                .collect();
            assert!(
                !names.iter().any(|n| n == "hot" || n == "mid"),
                "intermediate names should not survive: {names:?}"
            );
            if names.iter().any(|n| n == "final") {
                saw_final = true;
            }
        }
        assert!(
            saw_final,
            "at least one stack should rename through to `final`"
        );
    }

    #[test]
    fn resolved_chain_rename_supports_regex_backreferences() {
        use crate::matching::FunctionMatcher;
        use crate::profile::raw::RawProfile;
        use crate::profile::transforms::{RenameRule, Transforms};
        // Issue #87: regex captures must interpolate in the
        // replacement so a single rule can fold structurally similar
        // names (e.g. trait-vs-inherent monomorphisations) instead of
        // requiring one rule per concrete name.
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_functions.json")).unwrap();
        let base = Profile::from_raw(raw);
        let view = Profile::view(
            &base,
            Transforms {
                rename: vec![RenameRule {
                    matcher: FunctionMatcher::new("re:^(.)(.*)$").unwrap(),
                    replacement: "${2}_${1}".to_owned(),
                }],
                ..Default::default()
            },
        );
        let handle = view.threads().next().unwrap().handle();
        let mut renamed: Vec<String> = Vec::new();
        for stack_opt in view.stack_indices(
            handle,
            &crate::profile::event_source::EventSource::Samples,
            None,
        ) {
            let Some(stack_idx) = stack_opt else { continue };
            for f in view.resolved_chain(handle, stack_idx, false) {
                if !renamed.contains(&f.function) {
                    renamed.push(f.function);
                }
            }
        }
        renamed.sort();
        assert_eq!(renamed, vec!["old_c".to_owned(), "ot_h".to_owned()]);
    }

    #[test]
    fn resolved_chain_collapses_multi_function_cycle() {
        use crate::profile::raw::RawProfile;
        use crate::profile::transforms::Transforms;
        // Stack chain (root-to-leaf): A → B → C → A → B → C → X.
        // The (A,B,C) cycle repeats once, so collapse_recursion should
        // reduce the chain to [A, B, C, X]. This is the timely case:
        // alternating multi-frame cycles consecutive `dedup_by` could
        // never fold.
        let json = r#"{
          "meta": {"interval": 1.0, "startTime": 0.0, "product": "test"},
          "libs": [],
          "threads": [{
            "name": "Main", "tid": 1, "pid": 1, "registerTime": 0.0,
            "stringArray": ["A", "B", "C", "X"],
            "frameTable": {"length": 4, "address": [-1, -1, -1, -1], "func": [0, 1, 2, 3], "category": [0, 0, 0, 0], "subcategory": [0, 0, 0, 0], "line": [null, null, null, null], "column": [null, null, null, null], "nativeSymbol": [null, null, null, null]},
            "stackTable": {"length": 7, "frame": [0, 1, 2, 0, 1, 2, 3], "category": [0, 0, 0, 0, 0, 0, 0], "subcategory": [0, 0, 0, 0, 0, 0, 0], "prefix": [null, 0, 1, 2, 3, 4, 5]},
            "samples": {"length": 1, "stack": [6], "time": [0.0]},
            "funcTable": {"length": 4, "name": [0, 1, 2, 3], "isJS": [false, false, false, false], "relevantForJS": [false, false, false, false], "resource": [-1, -1, -1, -1], "fileName": [null, null, null, null], "lineNumber": [null, null, null, null], "columnNumber": [null, null, null, null]},
            "resourceTable": {"length": 0, "lib": [], "name": [], "host": [], "type": []},
            "nativeSymbols": {"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []}
          }]
        }"#;
        let raw: RawProfile = serde_json::from_str(json).unwrap();
        let base = Profile::from_raw(raw);
        let view = Profile::view(
            &base,
            Transforms {
                collapse_recursion: true,
                ..Default::default()
            },
        );
        let handle = view.threads().next().unwrap().handle();
        let stack_idx = view
            .stack_indices(
                handle,
                &crate::profile::event_source::EventSource::Samples,
                None,
            )
            .find_map(|s| s)
            .unwrap();
        let names: Vec<String> = view
            .resolved_chain(handle, stack_idx, false)
            .into_iter()
            .map(|f| f.function)
            .collect();
        assert_eq!(
            names,
            vec![
                "A".to_owned(),
                "B".to_owned(),
                "C".to_owned(),
                "X".to_owned()
            ]
        );
    }
}
