# Event-parameter for `top_functions`, `call_tree`, `compare_profiles` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Surface samply's per-event hardware counter data (cache-misses, branch-misses, instructions, …) — which currently lives in `threads[].markers` and is silently dropped by pollard — through the existing `top_functions`, `call_tree`, and `compare_profiles` tools via a new optional `event` argument.

**Architecture:** Extend the raw deserializer with a `RawMarkerTable` carrying enough of `data.cause.stack` to recover per-marker call chains. Generalize the existing `aggregate_grouped` loop in `top_functions.rs` to walk an arbitrary sequence of stack indices instead of being hard-wired to `samples.stack`, and add an `EventSource` helper that resolves the user-facing `event` string to either the samples track (default) or a name-filtered slice of `markers`. Wire the new arg through the three tool entry points; on `compare_profiles`, suppress the `*_ms` columns when the chosen event is not time-shaped (i.e. anything other than the samples track) since `count × interval_ms` only means "wall-time-ish" for cycles.

**Tech Stack:** Rust 2024, `serde` for marker deserialization, `rmcp` for MCP wiring. No new crate dependencies.

**Spec:** Closes the gap raised in https://github.com/antiguru/pollard/issues/38 — "samply preserves all events, pollard ignores 3 of 4."

---

## File Structure

Files created or modified:

```
src/
├── profile/
│   └── raw.rs                  # ADD: RawMarkerTable, RawMarkerData, MarkerCause; wire markers into RawThread
├── query/
│   ├── event.rs                # NEW: EventSource enum + resolver; per-source stack-index iterator
│   ├── top_functions.rs        # MODIFY: accept EventSource; drive aggregate_grouped from it; surface event in Output
│   ├── call_tree.rs            # MODIFY: accept EventSource; drive accumulate_with_root from it
│   ├── compare.rs              # MODIFY: accept EventSource; null *_ms columns when event != samples
│   └── mod.rs                  # MODIFY: re-export event::EventSource
├── tools/
│   └── query.rs                # MODIFY: add `event` arg on TopFunctionsArgs/CallTreeArgs/CompareProfilesArgs; map to EventSource
└── tests/
    └── fixtures/
        └── two_events.json     # NEW: tiny profile with both samples + a marker stream for unit tests
```

`src/query/event.rs` is the central new abstraction. It owns:
* The `EventSource` enum (`Samples` | `Marker(String)`).
* A `resolve` function that takes a `Profile` + `Option<&str>` and produces an `EventSource`, validating the marker exists when one is named.
* A `stack_indices(profile, handle, source) -> impl Iterator<Item = Option<usize>>` adapter so callers stay in their existing per-thread loop.

Decomposition rationale:
* The raw-layer change is independent and small — separate task, separate commit.
* Each query tool gets its own task because the existing test surface differs (sort modes, pruning, ms columns) and we want focused commits.
* The MCP wiring is one task: three string args, three matches, easy to review together.

---

## Task 1: Deserialize markers

**Why:** Today `RawThread` has no `markers` field, so the markers array is silently dropped during `serde_json::from_str`. Without this, every later task is unreachable.

**Files:**
* Modify: `src/profile/raw.rs:144-174` (RawThread), append new types after RawSampleTable.
* Test: `src/profile/raw.rs` (existing `tests` module).

* [ ] **Step 1: Write the failing test**

Append to the existing `mod tests` in `src/profile/raw.rs`:

```rust
#[test]
fn deserializes_marker_table() {
    let json = r#"{
        "meta": {"interval": 1.0, "startTime": 0.0, "product": "test"},
        "libs": [],
        "threads": [{
            "name": "Main",
            "tid": 1,
            "pid": 1,
            "registerTime": 0.0,
            "stringArray": ["foo", "cache-misses"],
            "frameTable": {"length": 0, "address": [], "func": [], "category": [], "subcategory": [], "innerWindowID": [], "implementation": [], "line": [], "column": [], "nativeSymbol": []},
            "stackTable": {"length": 0, "frame": [], "category": [], "subcategory": [], "prefix": []},
            "samples": {"length": 0, "stack": [], "time": [], "weight": null, "weightType": "samples"},
            "funcTable": {"length": 0, "name": [], "isJS": [], "relevantForJS": [], "resource": [], "fileName": [], "lineNumber": [], "columnNumber": []},
            "resourceTable": {"length": 0, "lib": [], "name": [], "host": [], "type": []},
            "markers": {
                "length": 2,
                "data": [{"type": "Other event", "cause": {"stack": 7}}, null],
                "name": [1, 1],
                "startTime": [0.0, 1.0],
                "endTime": [0.0, 1.0],
                "phase": [0, 0],
                "category": [0, 0]
            },
            "nativeSymbols": {"length": 0, "libIndex": [], "address": [], "name": [], "functionSize": []},
            "processType": "default",
            "processStartupTime": 0.0
        }]
    }"#;
    let p: RawProfile = serde_json::from_str(json).unwrap();
    let m = &p.threads[0].markers;
    assert_eq!(m.length, 2);
    assert_eq!(m.name, vec![1, 1]);
    // First marker has a cause.stack; second is null.
    assert_eq!(m.data[0].as_ref().and_then(|d| d.cause.as_ref()).map(|c| c.stack), Some(7));
    assert!(m.data[1].is_none());
}
```

* [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib profile::raw::tests::deserializes_marker_table`
Expected: FAIL — `markers` not a field, or test fails to compile because `markers` accessor missing.

* [ ] **Step 3: Add the marker types and field**

Append after `RawSampleTable`'s `impl` block (after line 253) in `src/profile/raw.rs`:

```rust
/// Per-thread marker stream. Firefox's processed-profile schema attaches
/// arbitrary point-or-range events here; samply uses it to land
/// non-cycles hardware counter samples (cache-misses, branch-misses,
/// instructions) when a single record contains more than one event.
///
/// We model only what the query layer needs:
///   * `name[i]` is a string-array index — resolved to the marker's
///     event name (e.g. "cache-misses").
///   * `data[i]` is the per-marker payload. samply emits
///     `{type: "Other event", cause: {stack: <idx>}}` for hardware
///     counters; we extract just the stack pointer and ignore the rest.
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RawMarkerTable {
    pub length: usize,
    /// Parallel to `length`. May contain `null` entries for markers
    /// without structured data.
    pub data: Vec<Option<RawMarkerData>>,
    /// String-array indices. Resolve via `thread.string_array[name[i]]`.
    pub name: Vec<usize>,
    pub start_time: Vec<f64>,
    pub end_time: Vec<f64>,
    pub phase: Vec<u8>,
    pub category: Vec<usize>,
}

/// Marker-payload subset. We only deserialize `cause.stack`; everything
/// else (`type`, timestamps, text fields) is dropped because the query
/// layer does not consume it. Using a struct rather than a typed enum
/// keeps us forward-compatible: if samply later ships markers with no
/// `cause` (e.g. text-only annotations) we just skip them.
#[derive(Debug, Deserialize, Default)]
pub struct RawMarkerData {
    #[serde(default)]
    pub cause: Option<MarkerCause>,
}

#[derive(Debug, Deserialize, Default)]
pub struct MarkerCause {
    /// Stack-table index. Same shape as `samples.stack[i]`.
    pub stack: usize,
}
```

In `RawThread` (around line 162), add the field as the **last** field before `inline_chains`:

```rust
    pub samples: RawSampleTable,
    pub resource_table: RawResourceTable,
    #[serde(default)]
    pub native_symbols: Option<RawNativeSymbols>,
    /// Per-thread marker stream. `default` so older fixtures without
    /// markers still parse.
    #[serde(default)]
    pub markers: RawMarkerTable,
    /// Per-frame inline-call chain (innermost-first), populated by
    /// [`crate::profile::symbolicate`]. Index parallel to [`RawFrameTable`];
    /// empty Vec when no inline records exist or symbolication wasn't run.
    /// Not part of the Firefox processed-profile schema — pollard-internal,
    /// hence skipped during deserialization.
    #[serde(skip)]
    pub inline_chains: Vec<Vec<InlineFrame>>,
```

* [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib profile::raw::tests::deserializes_marker_table`
Expected: PASS.

* [ ] **Step 5: Run full lib tests — no regressions**

Run: `cargo test --lib`
Expected: PASS for every existing test (the `#[serde(default)]` keeps older fixtures parseable).

* [ ] **Step 6: Commit**

```bash
git add src/profile/raw.rs
git commit -m "feat(raw): deserialize markers with cause.stack"
```

---

## Task 2: `EventSource` resolution + per-source stack iterator

**Why:** Every aggregator currently iterates `raw.samples.stack`. Hard-coding three different code paths (samples vs each marker name) would diverge fast. We need a single helper that resolves a user string to a source and yields stack indices.

**Files:**
* Create: `src/query/event.rs`.
* Modify: `src/query/mod.rs` (add module declaration + re-export).
* Test: `src/query/event.rs` (own `mod tests`).

* [ ] **Step 1: Wire the module**

In `src/query/mod.rs`, add at the top alongside the other `pub mod` lines:

```rust
pub mod event;
```

* [ ] **Step 2: Write the failing test**

Create `src/query/event.rs` with this content (will fail to compile until Step 3):

```rust
//! Event-source resolution: maps a user-facing `event` string to either
//! the per-thread samples track (default — what samply uses for
//! cycles-as-samples) or to a marker-name slice. Provides a unified
//! stack-index iterator so the aggregators stay generic.

#![allow(dead_code)]

use crate::error::ToolError;
use crate::profile::{Profile, ThreadHandle};

/// Where a per-sample stack index comes from. Matches the two layouts
/// samply emits: cycles in `samples`, all other hardware events in
/// `markers` keyed by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventSource {
    /// The default samples track. samply records the first perf event
    /// (typically `cycles`) here; pct columns mean "% of cycles" or
    /// equivalently "% of CPU time".
    Samples,
    /// Markers whose `name[i]` resolves to this string. samply uses this
    /// for secondary perf events (cache-misses, branch-misses,
    /// instructions, etc.).
    Marker(String),
}

impl EventSource {
    /// True iff the source's per-event count multiplied by
    /// `meta.interval` produces a meaningful wall-time-ish duration.
    /// `Samples` is, all marker-backed events are not.
    pub fn is_time_shaped(&self) -> bool {
        matches!(self, EventSource::Samples)
    }

    /// Stable lowercase label for output payloads ("samples" or the
    /// marker name verbatim).
    pub fn label(&self) -> &str {
        match self {
            EventSource::Samples => "samples",
            EventSource::Marker(name) => name.as_str(),
        }
    }
}

/// Resolve a user-facing event string to an [`EventSource`].
///
/// * `None` or empty string → [`EventSource::Samples`] (the default).
/// * Any non-empty string → looks up a marker with that name in *any*
///   thread of the profile; returns [`ToolError::Internal`] with a
///   list of available marker names if the lookup fails.
///
/// We do not treat `"cycles"` or `"samples"` as aliases — samply does
/// not emit a marker by either name, so the user must omit the arg to
/// get the samples track. The error message lists known events, so
/// even an unaware caller gets pointed at the right vocabulary on the
/// first miss.
pub fn resolve(profile: &Profile, event: Option<&str>) -> Result<EventSource, ToolError> {
    let raw = match event {
        None => return Ok(EventSource::Samples),
        Some(s) if s.is_empty() => return Ok(EventSource::Samples),
        Some(s) => s,
    };
    if marker_name_exists(profile, raw) {
        Ok(EventSource::Marker(raw.to_owned()))
    } else {
        Err(ToolError::Internal {
            message: format!(
                "unknown event {raw:?}; known marker events: {known:?} (omit `event` for the default samples track)",
                known = known_marker_names(profile),
            ),
        })
    }
}

fn marker_name_exists(profile: &Profile, name: &str) -> bool {
    for handle in profile.thread_handles() {
        let raw = profile.raw_thread(handle);
        for &str_idx in &raw.markers.name {
            if raw.string_array.get(str_idx).map(String::as_str) == Some(name) {
                return true;
            }
        }
    }
    false
}

fn known_marker_names(profile: &Profile) -> Vec<String> {
    let mut names: std::collections::BTreeSet<String> = Default::default();
    for handle in profile.thread_handles() {
        let raw = profile.raw_thread(handle);
        for &str_idx in &raw.markers.name {
            if let Some(s) = raw.string_array.get(str_idx) {
                names.insert(s.clone());
            }
        }
    }
    names.into_iter().collect()
}

/// Iterate the stack-table indices that this thread contributes for the
/// given event source. `Some(idx)` per sample/marker, `None` to skip
/// (matching the existing `samples.stack: Vec<Option<usize>>` shape so
/// callers can stay in their current per-stack loop).
pub fn stack_indices<'a>(
    profile: &'a Profile,
    handle: ThreadHandle,
    source: &'a EventSource,
) -> Box<dyn Iterator<Item = Option<usize>> + 'a> {
    let raw = profile.raw_thread(handle);
    match source {
        EventSource::Samples => Box::new(raw.samples.stack.iter().copied()),
        EventSource::Marker(name) => {
            // Resolve the marker name to its string-array index *once*
            // per thread; markers without a `cause.stack` payload are
            // skipped (yielded as `None`).
            let str_idx = raw
                .string_array
                .iter()
                .position(|s| s == name);
            match str_idx {
                None => Box::new(std::iter::empty()),
                Some(target) => Box::new(raw.markers.name.iter().enumerate().map(
                    move |(i, &n)| {
                        if n != target {
                            return None;
                        }
                        raw.markers
                            .data
                            .get(i)
                            .and_then(|d| d.as_ref())
                            .and_then(|d| d.cause.as_ref())
                            .map(|c| c.stack)
                    },
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::raw::RawProfile;

    fn fixture() -> Profile {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_events.json")).unwrap();
        Profile::from_raw(raw)
    }

    #[test]
    fn default_resolves_to_samples() {
        let p = fixture();
        assert_eq!(resolve(&p, None).unwrap(), EventSource::Samples);
        assert_eq!(resolve(&p, Some("")).unwrap(), EventSource::Samples);
    }

    #[test]
    fn known_marker_resolves() {
        let p = fixture();
        let s = resolve(&p, Some("cache-misses")).unwrap();
        assert_eq!(s, EventSource::Marker("cache-misses".into()));
        assert!(!s.is_time_shaped());
    }

    #[test]
    fn unknown_event_errors_with_suggestions() {
        let p = fixture();
        let err = resolve(&p, Some("not-a-real-event")).unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("not-a-real-event"), "{msg}");
        // Suggestion list should mention the marker name from the
        // fixture so the caller knows what's available.
        assert!(msg.contains("cache-misses"), "{msg}");
    }

    #[test]
    fn samples_iter_yields_every_sample_stack() {
        let p = fixture();
        let handle = p.thread_handles().next().unwrap();
        let n: usize = stack_indices(&p, handle, &EventSource::Samples).count();
        assert_eq!(n, p.raw_thread(handle).samples.stack.len());
    }

    #[test]
    fn marker_iter_filters_to_named_event() {
        let p = fixture();
        let handle = p.thread_handles().next().unwrap();
        let stacks: Vec<_> =
            stack_indices(&p, handle, &EventSource::Marker("cache-misses".into())).collect();
        // Fixture has 2 cache-miss markers, both with cause.stack set.
        assert_eq!(stacks.len(), 2);
        assert!(stacks.iter().all(|s| s.is_some()));
    }
}
```

* [ ] **Step 3: Verify test failure (unblocked by Task 3 fixture)**

The test depends on a fixture that doesn't exist yet. Run:

```bash
cargo test --lib query::event 2>&1 | head -20
```

Expected: compilation error referencing `two_events.json` (file missing). This is OK; we'll create the fixture in Task 3 before running.

We also need a `Profile::thread_handles` accessor — confirm by:

```bash
grep -n "fn thread_handles\|fn threads" src/profile/parsed.rs
```

Expected: returns `pub fn threads`. If `thread_handles` doesn't exist as such, swap the test/iterator to use `profile.threads().map(|t| t.handle())` or whatever is exposed. Document the actual call site in step 4 below.

* [ ] **Step 4: Adapt `event.rs` to the actual handle-iteration API**

Read `src/profile/parsed.rs` and find the canonical thread-iterator. Two likely shapes:
* `pub fn threads(&self) -> impl Iterator<Item = Thread<'_>>` — adapt with `.map(|t| t.handle())` if `Thread::handle` exists.
* `pub fn thread_handles(&self) -> impl Iterator<Item = ThreadHandle>` — use directly.

If neither matches, add a thin `pub fn thread_handles(&self) -> impl Iterator<Item = ThreadHandle> + '_` to `parsed.rs` that wraps the existing iteration. Keep the change minimal.

* [ ] **Step 5: Commit (deferred until fixture exists in Task 3)**

Skip the commit at this step. Tasks 2 + 3 will land together once the fixture makes the tests runnable.

---

## Task 3: `two_events.json` fixture + run Task 2 tests

**Why:** Task 2's tests need a profile that has both a samples track and at least one marker-backed event. Crafting it once here lets every subsequent task lean on the same shape.

**Files:**
* Create: `tests/fixtures/two_events.json`.
* Test: re-run the `query::event::tests` from Task 2.

* [ ] **Step 1: Build the fixture**

The fixture is a single-thread profile with two functions ("hot", "cold"), 4 samples (3 hot, 1 cold) recording cycles, and 2 cache-miss markers with `cause.stack` pointing into the same stack table. interval = 1.0.

Create `tests/fixtures/two_events.json`:

```json
{
  "meta": {"interval": 1.0, "startTime": 0.0, "product": "two_events", "categories": [{"name": "Other", "color": "grey", "subcategories": ["Other"]}], "preprocessedProfileVersion": 55, "version": 24, "sampleUnits": {"eventDelay": "ms", "threadCPUDelta": "µs", "time": "ms"}, "markerSchema": []},
  "libs": [],
  "threads": [{
    "name": "Main", "tid": 1, "pid": 1, "isMainThread": true, "registerTime": 0.0, "processStartupTime": 0.0, "processType": "default", "processName": "Main",
    "stringArray": ["hot", "cold", "cache-misses"],
    "frameTable": {"length": 2, "address": [-1, -1], "func": [0, 1], "category": [0, 0], "subcategory": [0, 0], "innerWindowID": [0, 0], "line": [null, null], "column": [null, null], "nativeSymbol": [null, null], "inlineDepth": [0, 0]},
    "funcTable": {"length": 2, "name": [0, 1], "isJS": [false, false], "relevantForJS": [false, false], "resource": [-1, -1], "fileName": [null, null], "lineNumber": [null, null], "columnNumber": [null, null]},
    "stackTable": {"length": 2, "frame": [0, 1], "category": [0, 0], "subcategory": [0, 0], "prefix": [null, null]},
    "samples": {"length": 4, "stack": [0, 0, 0, 1], "timeDeltas": [0.0, 1.0, 1.0, 1.0], "weight": [1, 1, 1, 1], "weightType": "samples", "threadCPUDelta": [0, 0, 0, 0]},
    "resourceTable": {"length": 0, "lib": [], "name": [], "host": [], "type": []},
    "nativeSymbols": {"length": 0, "address": [], "functionSize": [], "libIndex": [], "name": []},
    "markers": {
      "length": 2,
      "data": [{"type": "Other event", "cause": {"stack": 0}}, {"type": "Other event", "cause": {"stack": 1}}],
      "name": [2, 2],
      "startTime": [0.0, 1.0],
      "endTime": [0.0, 1.0],
      "phase": [0, 0],
      "category": [0, 0]
    },
    "pausedRanges": [],
    "showMarkersInTimeline": false,
    "tid": "1"
  }],
  "pages": [],
  "profilerOverhead": [],
  "counters": []
}
```

(The `"tid"` and `"pid"` fields are duplicated as both numeric and string forms in real samply output — keep the number form on the outer object since `RawThread` uses our `deserialize_id_as_u64` shim.)

* [ ] **Step 2: Run the Task 2 tests**

Run: `cargo test --lib query::event`
Expected: all four tests pass.

* [ ] **Step 3: Run the full lib suite**

Run: `cargo test --lib`
Expected: PASS.

* [ ] **Step 4: Commit**

```bash
git add src/query/mod.rs src/query/event.rs tests/fixtures/two_events.json
git commit -m "feat(query): EventSource resolver + per-source stack iterator"
```

---

## Task 4: Plumb `event` through `top_functions` AND `compare_profiles`

**Why:** Combined into one task per code-review feedback — landing them separately means a mid-stack commit where `compare_profiles` ignores marker events even though `aggregate_functions` already supports them. One bigger commit, one shippable state.

**Files:**
* Modify: `src/query/top_functions.rs` (Args, aggregate_grouped, top_functions, Output).
* Test: `src/query/top_functions.rs` (existing `mod tests`).

* [ ] **Step 1: Write the failing test**

Append in `src/query/top_functions.rs`'s `mod tests`:

```rust
#[test]
fn aggregates_marker_event_by_name() {
    use crate::query::event::EventSource;
    let raw: RawProfile =
        serde_json::from_str(include_str!("../../tests/fixtures/two_events.json")).unwrap();
    let profile = Profile::from_raw(raw);
    let result = top_functions(
        &profile,
        &Args {
            event: EventSource::Marker("cache-misses".into()),
            ..Default::default()
        },
    )
    .unwrap();
    // Two cache-miss markers, one on `hot`'s stack, one on `cold`'s.
    assert_eq!(result.event, "cache-misses");
    assert_eq!(result.total_samples, 2);
    let hot = result.functions.iter().find(|f| f.function == "hot").unwrap();
    let cold = result.functions.iter().find(|f| f.function == "cold").unwrap();
    assert_eq!(hot.self_samples, 1);
    assert_eq!(cold.self_samples, 1);
}

#[test]
fn defaults_to_samples_event() {
    let raw: RawProfile =
        serde_json::from_str(include_str!("../../tests/fixtures/two_events.json")).unwrap();
    let profile = Profile::from_raw(raw);
    let result = top_functions(&profile, &Args::default()).unwrap();
    assert_eq!(result.event, "samples");
    assert_eq!(result.total_samples, 4);
}
```

* [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib query::top_functions::tests::aggregates_marker_event_by_name`
Expected: compile error — `event` not on `Args`, `Output` missing `event`.

* [ ] **Step 3: Add `event` to `Args` + thread it through**

In `src/query/top_functions.rs`:

Add after the other `use` lines:

```rust
use crate::query::event::{self, EventSource};
```

Modify `Args`:

```rust
#[derive(Debug, Clone, Default)]
pub struct Args {
    pub filter: Option<String>,
    pub limit: usize,
    pub sort_by: SortBy,
    pub filter_args: Filter,
    /// When true, fan each native frame out into its DWARF inline chain
    /// (innermost-first when walking leaf-to-root), so self-time attributes
    /// to the deepest inlined callee instead of the enclosing function.
    pub expand_inlines: bool,
    /// Which per-sample event drives the aggregation. Defaults to the
    /// samples track (samply puts cycles there); pass [`EventSource::Marker`]
    /// to drill into hardware-counter markers like `cache-misses`.
    pub event: EventSource,
}
```

Note: `EventSource` needs `Default`. In `src/query/event.rs`, add to the enum:

```rust
impl Default for EventSource {
    fn default() -> Self {
        EventSource::Samples
    }
}
```

Modify `Output`:

```rust
#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub thread: Option<String>,
    pub process: Option<String>,
    pub total_samples: u64,
    pub filter: Option<String>,
    pub sort_by: &'static str,
    /// Echo of the resolved event source ("samples" or the marker name).
    /// Lets the caller verify which counter the pct columns are
    /// percentages of.
    pub event: String,
    pub functions: Vec<FunctionEntry>,
}
```

Modify `aggregate_functions` to accept `event` and `aggregate_grouped` to drive iteration from it:

```rust
pub(crate) fn aggregate_functions(
    profile: &Profile,
    filter: Option<&str>,
    filter_args: &Filter,
    expand_inlines: bool,
    event: &EventSource,
) -> Result<(HashMap<(String, Option<String>), Counts>, u64), ToolError> {
    aggregate_grouped(profile, filter, filter_args, expand_inlines, event, |f, m, _| {
        Some((f.to_owned(), m.map(str::to_owned)))
    })
}

pub(crate) fn aggregate_grouped<K, F>(
    profile: &Profile,
    filter: Option<&str>,
    filter_args: &Filter,
    expand_inlines: bool,
    event: &EventSource,
    mut key_fn: F,
) -> Result<(HashMap<K, Counts>, u64), ToolError>
where
    K: std::hash::Hash + Eq + Clone,
    F: FnMut(&str, Option<&str>, Option<&str>) -> Option<K>,
{
    filter_args.validate_thread(profile)?;
    let matcher = match filter {
        Some(p) => Some(FunctionMatcher::new(p).map_err(|e| ToolError::Internal {
            message: e.to_string(),
        })?),
        None => None,
    };

    let mut counts: HashMap<K, Counts> = HashMap::new();
    let mut total_samples: u64 = 0;

    for handle in filter_args.threads(profile) {
        for stack_opt in event::stack_indices(profile, handle, event) {
            let Some(stack_idx) = stack_opt else { continue };
            total_samples += 1;
            // ... (rest of the existing loop body unchanged) ...
        }
    }

    Ok((counts, total_samples))
}
```

Replace the body verbatim from the existing implementation (keep entries, seen_in_stack, etc. — only the outer iteration source changes).

Modify `top_functions` to fill `event` in the output, and to pass through:

```rust
pub fn top_functions(profile: &Profile, args: &Args) -> Result<Output, ToolError> {
    let (counts, total_samples) = aggregate_functions(
        profile,
        args.filter.as_deref(),
        &args.filter_args,
        args.expand_inlines,
        &args.event,
    )?;
    // ... existing sort/limit/build code unchanged ...
    Ok(Output {
        thread: None,
        process: None,
        total_samples,
        filter: args.filter.clone(),
        sort_by: match args.sort_by {
            SortBy::SelfTime => "self",
            SortBy::TotalTime => "total",
            SortBy::Descendants => "descendants",
        },
        event: args.event.label().to_owned(),
        functions,
    })
}
```

* [ ] **Step 4: Update the existing call site in `compare.rs` to pass `EventSource::Samples`**

`compare.rs` calls `aggregate_functions`. Until Task 6 we hold it on the default by passing `&EventSource::Samples` (Task 6 replaces this with the user's choice).

In `src/query/compare.rs`, both `aggregate_functions` calls in `compare_profiles` need a fifth arg:

```rust
let (counts_a, total_a) = aggregate_functions(
    a,
    args.filter.as_deref(),
    &args.filter_args,
    args.expand_inlines,
    &EventSource::Samples,
)?;
let (counts_b, total_b) = aggregate_functions(
    b,
    args.filter.as_deref(),
    &args.filter_args,
    args.expand_inlines,
    &EventSource::Samples,
)?;
```

Add the import:

```rust
use crate::query::event::EventSource;
```

* [ ] **Step 5: Update `top_groups.rs` similarly**

`top_groups` calls `aggregate_grouped`. It also needs the new `event` parameter. For now hold it on samples — adding `event` to `top_groups`'s public API is out of scope for this plan.

In `src/query/top_groups.rs`, find the `aggregate_grouped` call and pass `&EventSource::Samples`:

```rust
let (counts, total_samples): (HashMap<String, Counts>, u64) = aggregate_grouped(
    profile,
    args.filter.as_deref(),
    &args.filter_args,
    args.expand_inlines,
    &crate::query::event::EventSource::Samples,
    |func, module, file| match group_by {
        GroupBy::Function => Some(func.to_owned()),
        GroupBy::Module => Some(module.unwrap_or("<unknown>").to_owned()),
        GroupBy::File => Some(file.unwrap_or("<unknown>").to_owned()),
        GroupBy::Directory => directory_key(file?, depth),
    },
)?;
```

* [ ] **Step 6: Run tests**

Run: `cargo test --lib`
Expected: PASS — both new top_functions tests and all existing tests.

* [ ] **Step 7: Commit**

```bash
git add src/query/top_functions.rs src/query/event.rs src/query/compare.rs src/query/top_groups.rs
git commit -m "feat(top_functions): event= parameter for marker-backed counters"
```

---

## Task 5: Plumb `event` through `compare_profiles` (with ms suppression)

**Why:** The killer use case — "did this rewrite move cache-miss locality" — depends on this. Also exercises the time-shaped check (ms columns vanish when event != samples).

**Files:**
* Modify: `src/query/compare.rs`.
* Test: `src/query/compare.rs` (existing `mod tests`).

* [ ] **Step 1: Write the failing test**

Append in `src/query/compare.rs`'s `mod tests`:

```rust
#[test]
fn compare_profiles_with_marker_event() {
    // Build B by swapping the leaf so `hot`'s cache-miss marker now
    // lands on `cold`'s stack instead. Cycles distribution is
    // unchanged in this fixture; only cache-miss attribution moves.
    use crate::query::event::EventSource;
    let raw_a: RawProfile =
        serde_json::from_str(include_str!("../../tests/fixtures/two_events.json")).unwrap();
    let mut raw_b: RawProfile =
        serde_json::from_str(include_str!("../../tests/fixtures/two_events.json")).unwrap();
    // Repoint the first cache-miss marker from stack 0 (hot) → 1 (cold).
    raw_b.threads[0].markers.data[0] = Some(crate::profile::raw::RawMarkerData {
        cause: Some(crate::profile::raw::MarkerCause { stack: 1 }),
    });

    let a = Profile::from_raw(raw_a);
    let b = Profile::from_raw(raw_b);

    let out = compare_profiles(
        &a,
        &b,
        &Args {
            event: EventSource::Marker("cache-misses".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(out.event, "cache-misses");
    let cold = out.functions.iter().find(|r| r.function == "cold").unwrap();
    // Cold gained one cache-miss in B.
    assert_eq!(cold.delta_self_samples, 1);
    // Marker events are not time-shaped — ms columns must be None.
    assert!(cold.delta_self_ms.is_none(), "{cold:?}");
    assert!(cold.a_self_ms.is_none(), "{cold:?}");
}
```

* [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib query::compare::tests::compare_profiles_with_marker_event`
Expected: compile error — no `event` on `Args`, ms columns are `f64` not `Option<f64>`.

* [ ] **Step 3: Make ms columns optional**

In `src/query/compare.rs`, change `DiffEntry`:

```rust
    /// Per-side wall-time estimate: `samples * meta.interval_ms`. Pct
    /// columns shift when total profile time changes; ms columns don't —
    /// they answer "did this function take more or less time" directly.
    /// `None` when the chosen `event` is not time-shaped (i.e. anything
    /// other than the default samples track) — multiplying a marker's
    /// event count by the sampling interval has no useful meaning.
    /// Caveat for the time-shaped case: across N sampled threads,
    /// samples sum across threads, so the value is closer to summed-CPU-
    /// time than wall time when multiple threads are profiled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub a_self_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b_self_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub a_total_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b_total_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta_self_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta_total_ms: Option<f64>,
```

* [ ] **Step 4: Add `event` to `Args` + propagate to aggregation**

```rust
#[derive(Debug, Clone, Default)]
pub struct Args {
    // ... existing fields ...
    pub event: EventSource,
}
```

Replace the two `aggregate_functions` calls with `&args.event` (drop the `EventSource::Samples` placeholder added in Task 4).

Replace the ms construction block:

```rust
    let interval_a = a.meta().interval;
    let interval_b = b.meta().interval;
    let time_shaped = args.event.is_time_shaped();

    let mut rows: Vec<DiffEntry> = joined
        .into_iter()
        .map(|((function, module), (ca, cb))| {
            // ... pct calc unchanged ...
            let (a_self_ms, b_self_ms, a_total_ms, b_total_ms,
                 delta_self_ms, delta_total_ms) = if time_shaped {
                let a_self = ca.self_samples as f64 * interval_a;
                let b_self = cb.self_samples as f64 * interval_b;
                let a_total = ca.total_samples as f64 * interval_a;
                let b_total = cb.total_samples as f64 * interval_b;
                (Some(a_self), Some(b_self), Some(a_total), Some(b_total),
                 Some(b_self - a_self), Some(b_total - a_total))
            } else {
                (None, None, None, None, None, None)
            };
            DiffEntry {
                // ... existing fields ...
                a_self_ms, b_self_ms, a_total_ms, b_total_ms,
                delta_self_ms, delta_total_ms,
            }
        })
        .collect();
```

Update `sort_key` for the `DeltaMs` variant — when `delta_self_ms` is None, treat it as 0 so the sort doesn't crash on a non-time event:

```rust
fn sort_key(r: &DiffEntry, by: SortBy) -> f64 {
    match by {
        SortBy::Delta => r.delta_self_pct.abs() as f64,
        SortBy::DeltaMs => r.delta_self_ms.unwrap_or(0.0).abs(),
        SortBy::A => r.a_self_pct as f64,
        SortBy::B => r.b_self_pct as f64,
    }
}
```

Modify `Output` to include `event`:

```rust
#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub a_total_samples: u64,
    pub b_total_samples: u64,
    pub filter: Option<String>,
    pub sort_by: &'static str,
    pub event: String,
    pub functions: Vec<DiffEntry>,
}
```

And populate it: `event: args.event.label().to_owned()`.

* [ ] **Step 5: Update existing ms tests**

The existing tests (`delta_ms_falls_when_wall_time_falls_even_if_share_rises`, `sort_by_delta_ms_orders_by_absolute_ms_movement`) assert `delta_self_ms` as f64. Update each access to `.unwrap()` since those tests run with `event: EventSource::Samples` (the default), which is time-shaped.

Find every `delta_self_ms`, `a_self_ms`, etc. in `compare.rs::tests` and replace bare reads with `.unwrap()`. Example:

```rust
assert!(hot.delta_self_ms.unwrap() < 0.0, "{hot:?}");
assert!((hot.delta_self_ms.unwrap() + 30.0).abs() < 1e-9, "{hot:?}");
```

Same treatment for the `sort_by_delta_ms_orders_by_absolute_ms_movement` test:

```rust
assert!(out.functions[0].delta_self_ms.unwrap().abs()
    > out.functions[1].delta_self_ms.unwrap().abs());
```

* [ ] **Step 6: Run tests**

Run: `cargo test --lib query::compare`
Expected: PASS — including the new marker-event test.

* [ ] **Step 7: Run full suite**

Run: `cargo test --lib`
Expected: PASS.

* [ ] **Step 8: Commit**

```bash
git add src/query/compare.rs
git commit -m "feat(compare_profiles): event= parameter; null ms cols for non-time events"
```

---

## Task 6: Plumb `event` through `call_tree`

**Why:** Drill-down is the natural follow-on to "top cache-miss functions": who calls them. Without this the feature is half-shipped.

**Files:**
* Modify: `src/query/call_tree.rs`.
* Test: `src/query/call_tree.rs` (existing `mod tests`).

* [ ] **Step 1: Write the failing test**

```rust
#[test]
fn call_tree_with_marker_event() {
    use crate::query::event::EventSource;
    let raw: RawProfile =
        serde_json::from_str(include_str!("../../tests/fixtures/two_events.json")).unwrap();
    let profile = Profile::from_raw(raw);
    let tree = call_tree(
        &profile,
        &Args {
            event: EventSource::Marker("cache-misses".into()),
            min_pct: 0.0,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(tree.event, "cache-misses");
    // Two markers, one on each leaf — total event count = 2.
    assert_eq!(tree.total_samples, 2);
}
```

* [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib query::call_tree::tests::call_tree_with_marker_event`
Expected: compile error — `event` not on Args, `Output.event` missing.

* [ ] **Step 3: Wire `event` through `call_tree`**

In `src/query/call_tree.rs`:

Add:

```rust
use crate::query::event::{self, EventSource};
```

Modify `Args`:

```rust
pub struct Args {
    // ... existing fields ...
    pub event: EventSource,
}
```

Update `Default for Args` to include `event: EventSource::Samples` (the existing default).

Modify `Output`:

```rust
pub struct Output {
    pub thread: Option<String>,
    pub total_samples: u64,
    pub event: String,
    pub pruning: PruningKnobs,
    pub tree: Option<Node>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub did_you_mean: Option<DidYouMean>,
}
```

Pass `event` to the inner accumulator:

```rust
fn accumulate_with_root(
    profile: &Profile,
    handle: ThreadHandle,
    inverted: bool,
    expand_inlines: bool,
    event: &EventSource,
    // ... rest unchanged ...
) {
    // ... existing setup ...
    for stack_opt in event::stack_indices(profile, handle, event) {
        let Some(stack_idx) = stack_opt else { continue };
        // ... rest of body unchanged ...
    }
}
```

Update `call_tree_inner`'s call to `accumulate_with_root` to pass `&args.event`.

Populate the output:

```rust
Ok(Output {
    thread: None,
    total_samples,
    event: args.event.label().to_owned(),
    pruning: PruningKnobs { /* unchanged */ },
    tree,
    did_you_mean,
})
```

* [ ] **Step 4: Run tests**

Run: `cargo test --lib query::call_tree`
Expected: PASS.

* [ ] **Step 5: Run full suite**

Run: `cargo test --lib`
Expected: PASS.

* [ ] **Step 6: Commit**

```bash
git add src/query/call_tree.rs
git commit -m "feat(call_tree): event= parameter for marker-backed counters"
```

---

## Task 7: MCP wiring (`event` arg on three tools)

**Why:** Without this, the feature is unreachable from an LLM client.

**Files:**
* Modify: `src/tools/query.rs`.

* [ ] **Step 1: Add `event` to each Args struct**

For each of `TopFunctionsArgs`, `CallTreeArgs`, `CompareProfilesArgs` add:

```rust
    /// Event source: omit for the default samples track (cycles, in
    /// samply's perf recorder). Pass a marker name like `"cache-misses"`,
    /// `"branch-misses"`, or `"instructions"` to aggregate that hardware
    /// counter instead. The error message lists known events when the
    /// name doesn't match.
    #[serde(default)]
    pub event: Option<String>,
```

* [ ] **Step 2: Resolve and pass through in each tool method**

For each tool body, after the `get_session(...).await?` call, resolve the event and pass it through:

```rust
let event = crate::query::event::resolve(session.profile(), args.event.as_deref())?;
let q_args = ::Args {
    // ... existing fields ...
    event,
};
```

For `compare_profiles`, resolve against `session_a.profile()` (it's the baseline; we accept that an event valid in B but not A errors — symmetrical resolution would need both to know it, which is the strict semantic).

* [ ] **Step 3: Update tool descriptions**

In each `#[tool(...)]` attribute, append a sentence:

* `top_functions`: append " Pass `event=\"<name>\"` (e.g. `cache-misses`, `branch-misses`, `instructions`) to aggregate hardware-counter markers instead of the default samples track; pct columns then mean \"% of <event>\"."
* `compare_profiles`: append " The `event=` arg works the same way as on `top_functions`; the `*_ms` columns are omitted for non-time events because count × sampling-interval has no meaningful unit."
* `call_tree`: append " Pass `event=\"<name>\"` to build the tree from a marker-backed counter (cache-misses, branch-misses, instructions, …) instead of the default samples track."

* [ ] **Step 4: Run tests + clippy**

```bash
cargo test --lib
cargo clippy --tests --lib -- -D warnings
```

Expected: PASS.

* [ ] **Step 5: Commit**

```bash
git add src/tools/query.rs
git commit -m "feat(mcp): wire event= through top_functions, compare_profiles, call_tree"
```

---

## Task 8: End-to-end smoke check on the real fixture

**Why:** The full perf-counters.json the user collected from `simd` is not a unit-test fixture but is the canonical real-world input. Confirm the new code path is wired correctly end-to-end before declaring done.

**Files:**
* No code changes; runtime check only.

* [ ] **Step 1: Build the binary**

Run: `cargo build --release`
Expected: success.

* [ ] **Step 2: Probe each event via the binary's stdio MCP loop (or via a unit test that loads the file)**

Easiest: write a one-shot smoke test that exercises every event end-to-end:

```rust
#[test]
fn smoke_perf_counters_real_profile() {
    use crate::profile::raw::RawProfile;
    use crate::query::event::EventSource;
    use crate::query::top_functions::{top_functions, Args};
    let path = std::env::var("POLLARD_SMOKE_PROFILE").ok();
    let Some(path) = path else { return; };  // opt-in; skip when unset.
    let bytes = std::fs::read(&path).unwrap();
    let raw: RawProfile = serde_json::from_slice(&bytes).unwrap();
    let p = crate::profile::Profile::from_raw(raw);
    for ev in [
        EventSource::Samples,
        EventSource::Marker("cache-misses".into()),
        EventSource::Marker("branch-misses".into()),
        EventSource::Marker("instructions".into()),
    ] {
        let out = top_functions(
            &p,
            &Args { event: ev.clone(), limit: 5, ..Default::default() },
        ).unwrap();
        eprintln!("{}: total={} top_self={}", out.event, out.total_samples,
            out.functions.first().map(|f| f.self_samples).unwrap_or(0));
        assert!(out.total_samples > 0, "no samples for event {:?}", ev);
    }
}
```

Add to `src/query/top_functions.rs::tests`. Mark with `#[ignore]` so it doesn't run on CI without the env var:

```rust
#[test]
#[ignore]
fn smoke_perf_counters_real_profile() { /* ... */ }
```

Run:

```bash
POLLARD_SMOKE_PROFILE=/tmp/claude/perf-counters.json \
  cargo test --lib smoke_perf_counters_real_profile -- --ignored --nocapture
```

Expected: prints four lines with non-zero `total=` per event. The numbers should match the user's reported counts (samples ≈ 23272 cycles, plus the three marker totals).

* [ ] **Step 3: Commit**

```bash
git add src/query/top_functions.rs
git commit -m "test(top_functions): opt-in smoke test against real perf-counters profile"
```

---

## Task 9: README + tool-description sweep

**Why:** Document the new vocabulary so callers know to ask for it. Tool descriptions are the LLM's only docs.

**Files:**
* Modify: `README.md` if it lists tool surface (it does — line 22).

* [ ] **Step 1: Inspect README**

Run: `grep -n "compare_profiles\|top_functions\|call_tree" README.md`

* [ ] **Step 2: Add a one-line note under the tool surface**

Append after the tool list:

```markdown
The aggregating tools (`top_functions`, `call_tree`, `compare_profiles`)
accept an optional `event` argument; pass a marker name like
`cache-misses`, `branch-misses`, or `instructions` to aggregate that
hardware counter instead of the default samples track.
```

* [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs(readme): note event= argument on aggregating tools"
```

---

## Self-review

1. **Spec coverage:**
   * "Plumb `event` through `top_functions` + `compare_profiles` + `call_tree`" — Tasks 4, 5, 6.
   * "Default to samples track" — Task 2 `resolve` impl + `EventSource::default`.
   * "Strip ms columns when event != samples" — Task 5 step 3-4.
   * "Helpful error listing known events" — Task 2 `resolve` impl, asserted in test.
   * "MCP-level reach" — Task 7.
   * "Real-fixture validation" — Task 8.
   * "Docs" — Task 9 + tool-description sweep in Task 7 step 3.

2. **Placeholders:** none — every step has the exact code or command.

3. **Type consistency check:**
   * `EventSource` exposed under `crate::query::event` — referenced consistently in Tasks 4-7.
   * `Args.event: EventSource` — same field name on three Args structs.
   * `Output.event: String` — same shape on three Outputs.
   * `aggregate_functions` and `aggregate_grouped` both gain a fifth `event: &EventSource` param; all call sites updated (Task 4 step 4-5).
   * `DiffEntry.{a,b}_self_ms` and friends become `Option<f64>` — all existing tests updated to `.unwrap()` in Task 5 step 5.

---

## Plan revisions after code review

These patches override the relevant earlier task content. The original task structure is preserved for context but the implementation MUST honor these:

1. **`known_marker_names` filters to stack-bearing markers.** Real samply emits text-only markers like `"mmap"`. Listing them as event suggestions would mislead. In `event.rs::known_marker_names`, only enumerate marker indices `i` where `markers.data[i].as_ref().and_then(|d| d.cause.as_ref()).is_some()`. The `marker_name_exists` check already gates on the lookup succeeding — but should also require at least one occurrence with `cause.stack` populated.

2. **Tasks 4 and 5 ARE NOW ONE TASK** (already renamed above). Land top_functions + compare_profiles + top_groups (samples-pinned) in a single commit so each commit ships a coherent state.

3. **Keep `Option<f64>` for ms columns.** Reviewer suggested NaN/0.0 sentinels. Rejected — `0.0` collides with a real value, NaN doesn't round-trip through JSON cleanly. The `skip_serializing_if = "Option::is_none"` change IS a soft public-API break (fields disappear when event != samples); document this in the commit message.

4. **`sort_by="delta_ms"` with a non-time event must error**, not silently fall back. Add a check at the top of `compare_profiles`:

   ```rust
   if matches!(args.sort_by, SortBy::DeltaMs) && !args.event.is_time_shaped() {
       return Err(ToolError::Internal {
           message: format!(
               "sort_by=\"delta_ms\" is only valid for time-shaped events; \
                event {:?} has no millisecond interpretation. Try sort_by=\"delta\".",
               args.event.label(),
           ),
       });
   }
   ```

   No silent unwrap_or(0.0) in `sort_key` then — strip that fallback.

5. **API references in Task 2 are concrete, not "confirm by grep".** `Profile::threads()` returns `impl Iterator<Item = ThreadView<'_>>`. Get handles via `.map(|t| t.handle())`. `Profile::raw_thread(handle)` returns `&RawThread`. No new accessor needed.

6. **`stack_indices` may keep `Box<dyn Iterator>`.** The lifetime works because `raw_thread` returns `&'a RawThread` borrowed from `&'a Profile`. Verified.

7. **Fixture has ONE `tid` field**, integer form. Drop the trailing `"tid": "1"` line from the JSON in Task 3.

8. **Smoke test moves to `tests/smoke.rs`** (integration test) instead of unit test in `top_functions.rs`. Keep `#[ignore]` and `POLLARD_SMOKE_PROFILE` env-var gate.

9. **README and tool descriptions note that `top_groups` ignores `event` for now.** One-sentence caveat in each.

10. **`#[derive(Default)]` on `EventSource`** with `#[default]` attribute on the `Samples` variant — drop the manual `impl Default` in Task 4 step 3.
