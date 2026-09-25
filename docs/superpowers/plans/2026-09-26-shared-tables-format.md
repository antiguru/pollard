# Shared-tables profile format implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`* [ ]`) syntax for tracking.

**Goal:** Load samply's version 75 shared-table profiles (JSON and JSLB) alongside today's per-thread profiles, list the events a profile contains, and document the Linux perf workflow.

**Architecture:** `RawProfile` keeps its name but becomes the decoded model: one `SharedTables` per profile plus threads that hold only samples and markers.
Its `Deserialize` impl goes through a wire struct (`#[serde(try_from = "WireProfile")]`) that covers both layouts in one serde pass, so the ~40 existing `serde_json::from_str::<RawProfile>` test call sites keep compiling and plain JSON never builds a `serde_json::Value` tree.
A version gate in the `TryFrom` impl picks the per-thread decoder (missing version or 49 through 55) or the shared decoder (75).

**Tech Stack:** Rust 2024 (rust-version 1.95), serde, serde_json, flate2, json-slabs 0.2, wholesym, tokio, insta.

**Spec:** `docs/superpowers/specs/2026-09-25-shared-tables-format-design.md`

## Global constraints

* Supported processed-profile versions: missing, 49 through 55, and 75. Every other version returns `ToolError::UnsupportedProfileFormat`.
* Container detection uses magic bytes: gzip `1f 8b`, JSLB `DC DF 4A 53 4C 42 01 00`. Never the file extension.
* Plain JSON never goes through `serde_json::Value`. Only the JSLB path builds a `Value`.
* Function aggregation keeps keying on owned `(function, module)` strings.
* Symbolication keeps today's write granularity: name and file per func, line and inline chain per frame.
* Keep existing comments when moving code. Write new comments in the surrounding style.
* Run `cargo fmt` after editing Rust files and `cargo clippy --all-targets` before each commit. No new warnings.
* Commit messages use Conventional Commits (`feat:`, `fix:`, `refactor:`, `test:`, `docs:`) and end with the session's attribution lines.

## Review focus

* A legacy profile whose string array contains duplicates: marker lookup by name must still find every marker, so both decoders deduplicate strings through `Strings::intern_all` (Task 1 test `intern_all_deduplicates`, Task 2 test `duplicate_strings_merge`).
* A stack table whose prefix points forward or at itself: `walk_stack` must not loop forever, so the shared decoder rejects `prefixOffset > row` and validation rejects out-of-range prefixes (Task 4 test `prefix_offset_past_row_is_rejected`).
* A version 60 file, which has `shared` but pre-flag columns: the user must see `unsupported_profile_format`, not a serde "missing field flags" message (Task 4 test `version_60_reports_unsupported`).
* A `.json.gz` file that is really JSLB inside gzip, or a `.json` file that is gzipped: detection by magic bytes must load both (Task 5 test `gzipped_jslb_loads_by_magic`).
* An interval-end marker with a null `startTime`: loading must succeed and time filtering must use the end time (Task 3 test `interval_end_marker_uses_end_time`).

---

## File structure

* Create `src/profile/tables.rs`: `SharedTables`, `Strings`, the five table structs, inline-chain helpers, and `validate`.
* Create `src/profile/wire/mod.rs`: `WireProfile`, `WireProcess`, `WireThread`, `WireMarkerTable`, the version gate `TryFrom<WireProfile> for RawProfile`, and helpers shared by both decoders (`col`, `map_string`, `phase_times`, `decode_thread`).
* Create `src/profile/wire/legacy.rs`: per-thread wire tables (today's structs, moved verbatim) and the concatenating decoder.
* Create `src/profile/wire/v75.rs`: version 75 wire tables and the shared decoder.
* Create `src/profile/jslb.rs`: JSLB placeholder substitution into `serde_json::Value`.
* Modify `src/profile/raw.rs`: `RawProfile`, `RawThread`, `RawMarkerTable` become decoded types, `RawProcess` is new, and per-thread table structs move to `wire/legacy.rs`.
* Modify `src/profile/load.rs`: magic-byte detection, error mapping, version peek on the error path.
* Modify `src/profile/parsed.rs`, `src/profile/symbolicate.rs`, `src/query/asm.rs`, `src/query/event.rs`, `src/matching.rs`: read `SharedTables`.
* Modify the test modules in `src/query/{compare,call_tree,top_functions,source,summary}.rs` and `src/profile/{raw,symbolicate}.rs`: use `raw.shared` instead of per-thread tables.
* Modify `src/error.rs`: add `supported` to `UnsupportedProfileFormat`.
* Modify `src/query/describe.rs`, `src/query/summary.rs`: add `events`.
* Create `tests/fixtures/perf/` (three profiles plus `regenerate.sh`) and `tests/perf_formats.rs`.
* Modify `skills/profile-recording/SKILL.md`, `skills/pollard-doctor/SKILL.md`, `README.md`, `CHANGELOG.md`.

---

### Task 1: Shared tables module

**Files:**
* Create: `src/profile/tables.rs`
* Modify: `src/profile/mod.rs`

**Interfaces:**
* Consumes: `crate::profile::raw::{InlineFrame, RawLib}` (unchanged types).
* Produces:
  * `pub struct Strings` with `get(&self, usize) -> Option<&str>`, `len`, `is_empty`, `position(&self, &str) -> Option<usize>`, `intern(&mut self, &str) -> usize`, `intern_all(&mut self, &[String]) -> Vec<usize>`, `replace(&mut self, usize, &str)`.
  * `pub struct FrameTable { func: Vec<usize>, address: Vec<Option<u32>>, lib: Vec<Option<usize>>, line: Vec<Option<u32>>, column: Vec<Option<u32>>, native_symbol: Vec<Option<usize>>, inlined: Vec<bool> }` with `len`.
  * `pub struct FuncTable { name: Vec<usize>, is_js: Vec<bool>, relevant_for_js: Vec<bool>, resource: Vec<Option<usize>>, file_name: Vec<Option<usize>>, line_number: Vec<Option<u32>>, column_number: Vec<Option<u32>> }` with `len`.
  * `pub struct StackTable { frame: Vec<usize>, prefix: Vec<Option<usize>> }` with `len`.
  * `pub struct ResourceTable { name: Vec<usize>, host: Vec<Option<usize>>, type_: Vec<u8> }` with `len`.
  * `pub struct NativeSymbolTable { lib_index: Vec<usize>, address: Vec<Option<u32>>, name: Vec<usize>, function_size: Vec<Option<u64>> }` with `len`.
  * `pub struct SharedTables { strings, frames, funcs, stacks, resources, native_symbols, libs: Vec<RawLib>, inline_chains: Vec<Vec<InlineFrame>> }` with `inline_chain(&self, usize) -> &[InlineFrame]`, `set_inline_chain(&mut self, usize, Vec<InlineFrame>)`, and `validate_tables(&self) -> Result<(), String>`.
  * `pub(crate) fn out_of_range(column: &str, row: usize, value: usize, len: usize) -> String`.

* [ ] **Step 1: Write the failing tests**

Create `src/profile/tables.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_all_deduplicates() {
        let mut s = Strings::default();
        let map = s.intern_all(&["a".into(), "b".into(), "a".into()]);
        assert_eq!(map, vec![0, 1, 0]);
        assert_eq!(s.len(), 2);
        assert_eq!(s.position("a"), Some(0));
        let map2 = s.intern_all(&["b".into(), "c".into()]);
        assert_eq!(map2, vec![1, 2]);
    }

    #[test]
    fn replace_updates_index() {
        let mut s = Strings::default();
        s.intern("old");
        s.replace(0, "new");
        assert_eq!(s.get(0), Some("new"));
        assert_eq!(s.position("new"), Some(0));
        assert_eq!(s.position("old"), None);
    }

    #[test]
    fn set_inline_chain_grows_to_frame_count() {
        let mut t = SharedTables::default();
        t.frames.func = vec![0, 0, 0];
        t.set_inline_chain(
            2,
            vec![InlineFrame {
                function: "inner".into(),
                file: None,
                line: None,
            }],
        );
        assert_eq!(t.inline_chains.len(), 3);
        assert!(t.inline_chain(0).is_empty());
        assert_eq!(t.inline_chain(2)[0].function, "inner");
        assert!(t.inline_chain(99).is_empty());
    }

    fn one_frame_tables() -> SharedTables {
        let mut t = SharedTables::default();
        t.strings.intern("f");
        t.funcs = FuncTable {
            name: vec![0],
            is_js: vec![false],
            relevant_for_js: vec![false],
            resource: vec![None],
            file_name: vec![None],
            line_number: vec![None],
            column_number: vec![None],
        };
        t.frames = FrameTable {
            func: vec![0],
            address: vec![None],
            lib: vec![None],
            line: vec![None],
            column: vec![None],
            native_symbol: vec![None],
            inlined: vec![false],
        };
        t.stacks = StackTable {
            frame: vec![0],
            prefix: vec![None],
        };
        t
    }

    #[test]
    fn validate_accepts_consistent_tables() {
        assert_eq!(one_frame_tables().validate_tables(), Ok(()));
    }

    #[test]
    fn validate_rejects_out_of_range_func() {
        let mut t = one_frame_tables();
        t.frames.func[0] = 5;
        let err = t.validate_tables().unwrap_err();
        assert_eq!(err, "frameTable.func[0] = 5 is out of range (length 1)");
    }

    #[test]
    fn validate_rejects_out_of_range_prefix() {
        let mut t = one_frame_tables();
        t.stacks.prefix[0] = Some(3);
        let err = t.validate_tables().unwrap_err();
        assert_eq!(err, "stackTable.prefix[0] = 3 is out of range (length 1)");
    }

    #[test]
    fn validate_rejects_missing_lib() {
        let mut t = one_frame_tables();
        t.frames.lib[0] = Some(0);
        let err = t.validate_tables().unwrap_err();
        assert_eq!(err, "frameTable.lib[0] = 0 is out of range (length 0)");
    }
}
```

Add `pub mod tables;` to `src/profile/mod.rs` after `pub mod symbolicate;`.

* [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib profile::tables`
Expected: compile errors, `cannot find type Strings` and similar.

* [ ] **Step 3: Implement the module**

Put this above the test module in `src/profile/tables.rs`:

```rust
//! Profile-global tables shared by every thread.
//!
//! Both processed-profile layouts decode into these tables: the
//! per-thread layout (versions 49 through 55) by concatenating each
//! thread's tables, the shared layout (version 75) by mapping
//! `profile.shared` column by column. Nullable values are `Option`s,
//! so format sentinels and flag bits never leave the decoders.

#![allow(dead_code)]

use std::collections::HashMap;

use crate::profile::raw::{InlineFrame, RawLib};

/// Deduplicated string table with an interning index.
///
/// Decoders intern every file string, so each distinct string has
/// exactly one index. Marker lookup by name relies on that.
#[derive(Debug, Default, Clone)]
pub struct Strings {
    values: Vec<String>,
    index: HashMap<String, usize>,
}

impl Strings {
    pub fn get(&self, idx: usize) -> Option<&str> {
        self.values.get(idx).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn position(&self, s: &str) -> Option<usize> {
        self.index.get(s).copied()
    }

    pub fn intern(&mut self, s: &str) -> usize {
        if let Some(&idx) = self.index.get(s) {
            return idx;
        }
        let idx = self.values.len();
        self.values.push(s.to_owned());
        self.index.insert(s.to_owned(), idx);
        idx
    }

    /// Intern every entry of a file's string array and return the map
    /// from file index to table index.
    pub fn intern_all(&mut self, file_strings: &[String]) -> Vec<usize> {
        file_strings.iter().map(|s| self.intern(s)).collect()
    }

    /// Replace the string at `idx` in place. Tests use this to rename
    /// functions in a fixture without disturbing other indices.
    pub fn replace(&mut self, idx: usize, s: &str) {
        let old = std::mem::replace(&mut self.values[idx], s.to_owned());
        if self.index.get(&old) == Some(&idx) {
            self.index.remove(&old);
        }
        self.index.entry(s.to_owned()).or_insert(idx);
    }
}

#[derive(Debug, Default, Clone)]
pub struct FrameTable {
    pub func: Vec<usize>,
    /// Library-relative address, `None` for label frames.
    pub address: Vec<Option<u32>>,
    /// Index into [`SharedTables::libs`].
    pub lib: Vec<Option<usize>>,
    pub line: Vec<Option<u32>>,
    pub column: Vec<Option<u32>>,
    pub native_symbol: Vec<Option<usize>>,
    pub inlined: Vec<bool>,
}

impl FrameTable {
    pub fn len(&self) -> usize {
        self.func.len()
    }

    pub fn is_empty(&self) -> bool {
        self.func.is_empty()
    }
}

#[derive(Debug, Default, Clone)]
pub struct FuncTable {
    /// String index.
    pub name: Vec<usize>,
    pub is_js: Vec<bool>,
    pub relevant_for_js: Vec<bool>,
    pub resource: Vec<Option<usize>>,
    /// String index.
    pub file_name: Vec<Option<usize>>,
    pub line_number: Vec<Option<u32>>,
    pub column_number: Vec<Option<u32>>,
}

impl FuncTable {
    pub fn len(&self) -> usize {
        self.name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.name.is_empty()
    }
}

#[derive(Debug, Default, Clone)]
pub struct StackTable {
    pub frame: Vec<usize>,
    /// Absolute parent stack index, `None` for roots.
    pub prefix: Vec<Option<usize>>,
}

impl StackTable {
    pub fn len(&self) -> usize {
        self.frame.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frame.is_empty()
    }
}

#[derive(Debug, Default, Clone)]
pub struct ResourceTable {
    /// String index.
    pub name: Vec<usize>,
    /// String index.
    pub host: Vec<Option<usize>>,
    pub type_: Vec<u8>,
}

impl ResourceTable {
    pub fn len(&self) -> usize {
        self.name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.name.is_empty()
    }
}

#[derive(Debug, Default, Clone)]
pub struct NativeSymbolTable {
    /// Index into [`SharedTables::libs`].
    pub lib_index: Vec<usize>,
    pub address: Vec<Option<u32>>,
    /// String index.
    pub name: Vec<usize>,
    pub function_size: Vec<Option<u64>>,
}

impl NativeSymbolTable {
    pub fn len(&self) -> usize {
        self.name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.name.is_empty()
    }
}

/// All tables of one profile. Thread samples and markers index into
/// these.
#[derive(Debug, Default, Clone)]
pub struct SharedTables {
    pub strings: Strings,
    pub frames: FrameTable,
    pub funcs: FuncTable,
    pub stacks: StackTable,
    pub resources: ResourceTable,
    pub native_symbols: NativeSymbolTable,
    /// One list covering the root profile and every sub-process.
    pub libs: Vec<RawLib>,
    /// Per-frame inline-call chain (innermost-first), populated by
    /// [`crate::profile::symbolicate`]. Index parallel to `frames`;
    /// shorter than `frames` until symbolication resizes it.
    pub inline_chains: Vec<Vec<InlineFrame>>,
}

pub(crate) fn out_of_range(column: &str, row: usize, value: usize, len: usize) -> String {
    format!("{column}[{row}] = {value} is out of range (length {len})")
}

fn check(column: &str, values: &[usize], len: usize) -> Result<(), String> {
    for (row, &v) in values.iter().enumerate() {
        if v >= len {
            return Err(out_of_range(column, row, v, len));
        }
    }
    Ok(())
}

fn check_opt(column: &str, values: &[Option<usize>], len: usize) -> Result<(), String> {
    for (row, v) in values.iter().enumerate() {
        if let Some(v) = *v
            && v >= len
        {
            return Err(out_of_range(column, row, v, len));
        }
    }
    Ok(())
}

impl SharedTables {
    pub fn inline_chain(&self, frame_idx: usize) -> &[InlineFrame] {
        self.inline_chains
            .get(frame_idx)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn set_inline_chain(&mut self, frame_idx: usize, chain: Vec<InlineFrame>) {
        if self.inline_chains.len() < self.frames.len() {
            self.inline_chains.resize_with(self.frames.len(), Vec::new);
        }
        self.inline_chains[frame_idx] = chain;
    }

    /// Check every cross-reference between the tables. Decoders build
    /// each column row by row, so all columns of a table have equal
    /// length by construction.
    pub fn validate_tables(&self) -> Result<(), String> {
        let strings = self.strings.len();
        check("frameTable.func", &self.frames.func, self.funcs.len())?;
        check_opt("frameTable.lib", &self.frames.lib, self.libs.len())?;
        check_opt(
            "frameTable.nativeSymbol",
            &self.frames.native_symbol,
            self.native_symbols.len(),
        )?;
        check("funcTable.name", &self.funcs.name, strings)?;
        check_opt("funcTable.resource", &self.funcs.resource, self.resources.len())?;
        check_opt("funcTable.fileName", &self.funcs.file_name, strings)?;
        check("stackTable.frame", &self.stacks.frame, self.frames.len())?;
        check_opt("stackTable.prefix", &self.stacks.prefix, self.stacks.len())?;
        check("resourceTable.name", &self.resources.name, strings)?;
        check_opt("resourceTable.host", &self.resources.host, strings)?;
        check(
            "nativeSymbols.libIndex",
            &self.native_symbols.lib_index,
            self.libs.len(),
        )?;
        check("nativeSymbols.name", &self.native_symbols.name, strings)?;
        Ok(())
    }
}
```

* [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib profile::tables`
Expected: 7 tests pass.

* [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt
cargo clippy --all-targets
git add src/profile/tables.rs src/profile/mod.rs
git commit -m "feat: add profile-global shared tables"
```

---

### Task 2: Wire layer and per-thread decoder

This task adds the wire structs and the per-thread decoder as new code that nothing calls yet. Task 3 switches `RawProfile` over to it.

**Files:**
* Create: `src/profile/wire/mod.rs`, `src/profile/wire/legacy.rs`
* Modify: `src/profile/mod.rs`, `src/profile/raw.rs` (make `deserialize_id_as_u64` `pub(crate)`, add `RawMarkerData::type_`, add `RawMeta::preprocessed_profile_version`)

**Interfaces:**
* Consumes: everything from Task 1.
* Produces:
  * `pub struct WireProfile { meta: RawMeta, libs: Vec<RawLib>, shared: Option<v75::WireShared>, threads: Vec<WireThread>, processes: Vec<WireProcess> }` (the `shared` field is added in Task 4; in this task it is absent).
  * `pub struct DecodedProfile { meta: RawMeta, shared: SharedTables, threads: Vec<DecodedThread>, processes: Vec<Vec<DecodedThread>> }` as the decoder output until Task 3 renames it to `RawProfile`.
  * `pub struct DecodedThread { tid, pid, name, process_name, register_time, samples: RawSampleTable, markers: DecodedMarkerTable }`.
  * `pub struct DecodedMarkerTable { length: usize, data: Vec<Option<RawMarkerData>>, name: Vec<usize>, start_time: Vec<Option<f64>>, end_time: Vec<Option<f64>>, phase: Vec<u8>, category: Vec<usize> }`.
  * `pub(crate) fn is_supported_version(v: u32) -> bool` returns true for 49 through 55 and 75.
  * `pub(super) fn legacy::decode(w: WireProfile) -> Result<DecodedProfile, String>`.

Task 3 renames `DecodedProfile` to `RawProfile`, `DecodedThread` to `RawThread`, and `DecodedMarkerTable` to `RawMarkerTable`, and adds `RawProcess`. The `Decoded*` names exist only so this task builds next to the old types.

* [ ] **Step 1: Update `raw.rs` helpers**

In `src/profile/raw.rs`:
* Change `fn deserialize_id_as_u64` to `pub(crate) fn deserialize_id_as_u64`.
* Add to `RawMeta`:

```rust
    /// Processed-profile format version. Absent in hand-written test
    /// fixtures, which use the per-thread layout.
    #[serde(default)]
    pub preprocessed_profile_version: Option<u32>,
```

* Add to `RawMarkerData`, above `cause`:

```rust
    /// Marker schema name, e.g. `"Other event"` for samply's secondary
    /// perf events.
    #[serde(default, rename = "type")]
    pub type_: Option<String>,
```

Update the `RawMarkerData` doc comment to say that `type` and `cause.stack` are consumed.

* [ ] **Step 2: Write the failing decoder tests**

Create `src/profile/wire/legacy.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::super::WireProfile;
    use super::decode;

    /// Two threads whose tables both start at index 0, so every index
    /// in the second thread must move.
    const TWO_THREADS: &str = r#"{
        "meta": {"interval": 1.0, "startTime": 0.0, "preprocessedProfileVersion": 49},
        "libs": [{"name": "root"}],
        "threads": [
            {"tid": 1, "pid": 1, "registerTime": 0.0,
             "stringArray": ["a", "ev"],
             "frameTable": {"length": 1, "address": [16], "func": [0], "line": [null], "column": [null], "category": [0], "subcategory": [0], "nativeSymbol": [null]},
             "funcTable": {"length": 1, "name": [0], "isJS": [false], "relevantForJS": [false], "resource": [0], "fileName": [null], "lineNumber": [null], "columnNumber": [null]},
             "stackTable": {"length": 1, "frame": [0], "prefix": [null]},
             "resourceTable": {"length": 1, "lib": [0], "name": [0], "host": [null], "type": [1]},
             "samples": {"length": 1, "stack": [0], "time": [0.0]},
             "markers": {"length": 1, "data": [{"type": "Other event", "cause": {"stack": 0}}], "name": [1], "startTime": [0.5], "endTime": [null], "phase": [0], "category": [0]}},
            {"tid": 2, "pid": 1, "registerTime": 0.0,
             "stringArray": ["b", "a"],
             "frameTable": {"length": 2, "address": [-1, 32], "func": [0, 1], "line": [null, 7], "column": [null, null], "category": [0, 0], "subcategory": [0, 0], "nativeSymbol": [null, null]},
             "funcTable": {"length": 2, "name": [0, 1], "isJS": [false, false], "relevantForJS": [false, false], "resource": [-1, 0], "fileName": [null, 0], "lineNumber": [null, null], "columnNumber": [null, null]},
             "stackTable": {"length": 2, "frame": [0, 1], "prefix": [null, 0]},
             "resourceTable": {"length": 1, "lib": [0], "name": [1], "host": [null], "type": [1]},
             "samples": {"length": 2, "stack": [1, null], "time": [0.0, 1.0]}}
        ]
    }"#;

    fn decode_str(json: &str) -> super::super::DecodedProfile {
        let w: WireProfile = serde_json::from_str(json).unwrap();
        decode(w).unwrap()
    }

    #[test]
    fn second_thread_indices_are_offset() {
        let p = decode_str(TWO_THREADS);
        let t = &p.shared;
        assert_eq!(t.frames.len(), 3);
        assert_eq!(t.funcs.len(), 3);
        assert_eq!(t.stacks.len(), 3);
        // Thread 2's stack 1 is shared stack 2, whose prefix is thread 2's stack 0 (shared 1).
        assert_eq!(p.threads[1].samples.stack, vec![Some(2), None]);
        assert_eq!(t.stacks.prefix[2], Some(1));
        assert_eq!(t.stacks.frame[2], 2);
        assert_eq!(t.frames.func[2], 2);
        assert_eq!(t.frames.address[1], None);
        assert_eq!(t.frames.address[2], Some(32));
        assert_eq!(t.frames.line[2], Some(7));
    }

    #[test]
    fn duplicate_strings_merge() {
        let p = decode_str(TWO_THREADS);
        let t = &p.shared;
        // "a" appears in both threads and maps to one entry.
        assert_eq!(t.strings.len(), 3);
        assert_eq!(t.funcs.name[0], t.funcs.name[2]);
        assert_eq!(t.funcs.file_name[2], Some(t.strings.position("b").unwrap()));
        assert_eq!(p.threads[0].markers.name, vec![t.strings.position("ev").unwrap()]);
    }

    #[test]
    fn frame_lib_comes_from_resource() {
        let p = decode_str(TWO_THREADS);
        let t = &p.shared;
        assert_eq!(t.frames.lib, vec![Some(0), None, Some(0)]);
        assert_eq!(t.funcs.resource, vec![Some(0), None, Some(1)]);
    }

    #[test]
    fn marker_times_follow_phase() {
        let p = decode_str(TWO_THREADS);
        let m = &p.threads[0].markers;
        assert_eq!(m.start_time, vec![Some(0.5)]);
        assert_eq!(m.end_time, vec![None]);
        let data = m.data[0].as_ref().unwrap();
        assert_eq!(data.type_.as_deref(), Some("Other event"));
        assert_eq!(data.cause.as_ref().unwrap().stack, 0);
    }

    #[test]
    fn sub_process_libs_are_remapped() {
        let json = r#"{
            "meta": {"interval": 1.0, "startTime": 0.0},
            "libs": [{"name": "root"}],
            "threads": [],
            "processes": [{
                "libs": [{"name": "child"}],
                "threads": [
                    {"tid": 3, "pid": 2, "registerTime": 0.0,
                     "stringArray": ["c"],
                     "frameTable": {"length": 1, "address": [8], "func": [0], "line": [null], "column": [null], "category": [0], "subcategory": [0], "nativeSymbol": [0]},
                     "funcTable": {"length": 1, "name": [0], "isJS": [false], "relevantForJS": [false], "resource": [0], "fileName": [null], "lineNumber": [null], "columnNumber": [null]},
                     "stackTable": {"length": 1, "frame": [0], "prefix": [null]},
                     "resourceTable": {"length": 1, "lib": [0], "name": [0], "host": [null], "type": [1]},
                     "nativeSymbols": {"length": 1, "libIndex": [0], "address": [0], "name": [0], "functionSize": [null]},
                     "samples": {"length": 1, "stack": [0], "time": [0.0]}}
                ]
            }]
        }"#;
        let p = decode_str(json);
        let t = &p.shared;
        assert_eq!(t.libs.len(), 2);
        assert_eq!(t.libs[1].name.as_deref(), Some("child"));
        assert_eq!(t.frames.lib, vec![Some(1)]);
        assert_eq!(t.native_symbols.lib_index, vec![1]);
        assert_eq!(p.processes[0][0].tid, 3);
    }

    #[test]
    fn missing_string_array_is_reported() {
        let json = r#"{
            "meta": {"interval": 1.0, "startTime": 0.0},
            "threads": [{"tid": 1, "pid": 1, "registerTime": 0.0,
                         "samples": {"length": 0, "stack": []}}]
        }"#;
        let w: WireProfile = serde_json::from_str(json).unwrap();
        let err = decode(w).unwrap_err();
        assert_eq!(err, "threads[0]: missing stringArray");
    }
}
```

Create `src/profile/wire/mod.rs` containing only `mod legacy;`, and add `pub(crate) mod wire;` to `src/profile/mod.rs`.

* [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --lib profile::wire`
Expected: compile errors, `cannot find type WireProfile`.

* [ ] **Step 4: Implement the wire layer**

Replace `src/profile/wire/mod.rs` with:

```rust
//! Wire formats of the Firefox processed profile and the version gate
//! that picks a decoder.
//!
//! [`WireProfile`] deserializes both layouts in one serde pass: the
//! per-thread tables live on [`WireThread`], the shared tables on
//! [`WireProfile::shared`]. The decoders turn either into
//! [`SharedTables`] plus threads that keep only samples and markers.

#![allow(dead_code)]

mod legacy;

use serde::Deserialize;

use crate::profile::raw::{
    Pid, RawLib, RawMarkerData, RawMeta, RawSampleTable, deserialize_id_as_u64,
};
use crate::profile::tables::{SharedTables, out_of_range};

/// Versions the decoders understand. A missing version counts as the
/// per-thread layout, because the hand-written test fixtures carry none.
pub(crate) fn is_supported_version(v: u32) -> bool {
    matches!(v, 49..=55 | 75)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireProfile {
    pub meta: RawMeta,
    #[serde(default)]
    pub libs: Vec<RawLib>,
    #[serde(default)]
    pub threads: Vec<WireThread>,
    #[serde(default)]
    pub processes: Vec<WireProcess>,
}

/// A nested process of the per-thread layout. It carries its own
/// `libs` list, which its resource tables index into.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireProcess {
    #[serde(default)]
    pub libs: Vec<RawLib>,
    #[serde(default)]
    pub threads: Vec<WireThread>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireThread {
    #[serde(deserialize_with = "deserialize_id_as_u64")]
    pub tid: u64,
    pub pid: Pid,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub process_name: Option<String>,
    pub register_time: f64,
    pub samples: RawSampleTable,
    /// `default` so older fixtures without a markers field still parse.
    #[serde(default)]
    pub markers: WireMarkerTable,
    // Per-thread layout only. The shared layout keeps these in
    // `profile.shared`.
    #[serde(default)]
    pub string_array: Option<Vec<String>>,
    #[serde(default)]
    pub frame_table: Option<legacy::LegacyFrameTable>,
    #[serde(default)]
    pub func_table: Option<legacy::LegacyFuncTable>,
    #[serde(default)]
    pub stack_table: Option<legacy::LegacyStackTable>,
    #[serde(default)]
    pub resource_table: Option<legacy::LegacyResourceTable>,
    #[serde(default)]
    pub native_symbols: Option<legacy::LegacyNativeSymbols>,
}

/// Marker columns as stored. Times may be `null` where the phase makes
/// them meaningless (format version 68 and later).
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WireMarkerTable {
    pub length: usize,
    pub data: Vec<Option<RawMarkerData>>,
    pub name: Vec<usize>,
    pub start_time: Vec<Option<f64>>,
    pub end_time: Vec<Option<f64>>,
    pub phase: Vec<u8>,
    pub category: Vec<usize>,
}

#[derive(Debug)]
pub struct DecodedProfile {
    pub meta: RawMeta,
    pub shared: SharedTables,
    pub threads: Vec<DecodedThread>,
    pub processes: Vec<Vec<DecodedThread>>,
}

#[derive(Debug)]
pub struct DecodedThread {
    pub tid: u64,
    pub pid: Pid,
    pub name: Option<String>,
    pub process_name: Option<String>,
    pub register_time: f64,
    pub samples: RawSampleTable,
    pub markers: DecodedMarkerTable,
}

#[derive(Debug, Default)]
pub struct DecodedMarkerTable {
    pub length: usize,
    pub data: Vec<Option<RawMarkerData>>,
    /// String indices into [`SharedTables::strings`].
    pub name: Vec<usize>,
    pub start_time: Vec<Option<f64>>,
    pub end_time: Vec<Option<f64>>,
    pub phase: Vec<u8>,
    pub category: Vec<usize>,
}

/// Row `row` of `column`, or an error naming the column.
pub(super) fn col<'a, T>(column: &'a [T], row: usize, name: &str) -> Result<&'a T, String> {
    column
        .get(row)
        .ok_or_else(|| format!("{name} has no row {row}"))
}

/// Translate a file string index through the map returned by
/// [`crate::profile::tables::Strings::intern_all`].
pub(super) fn map_string(map: &[usize], idx: usize, column: &str, row: usize) -> Result<usize, String> {
    map.get(idx)
        .copied()
        .ok_or_else(|| out_of_range(column, row, idx, map.len()))
}

/// Drop the marker time the phase marks as meaningless: instant (0)
/// and interval-start (2) markers have no end, interval-end (3)
/// markers have no start.
pub(super) fn phase_times(phase: u8, start: Option<f64>, end: Option<f64>) -> (Option<f64>, Option<f64>) {
    match phase {
        0 | 2 => (start, None),
        3 => (None, end),
        _ => (start, end),
    }
}

/// Build a decoded thread. `string_map` translates the file's string
/// indices and `stack_base` moves the thread's stack indices into the
/// shared stack table.
pub(super) fn decode_thread(
    t: WireThread,
    string_map: &[usize],
    stack_base: usize,
) -> Result<DecodedThread, String> {
    let mut samples = t.samples;
    for stack in samples.stack.iter_mut().flatten() {
        *stack += stack_base;
    }

    let m = t.markers;
    let mut markers = DecodedMarkerTable {
        length: m.length,
        category: m.category,
        phase: m.phase,
        ..Default::default()
    };
    for (row, &name) in m.name.iter().enumerate() {
        markers
            .name
            .push(map_string(string_map, name, "markers.name", row)?);
    }
    for (row, data) in m.data.into_iter().enumerate() {
        markers.data.push(data.map(|mut d| {
            if let Some(cause) = d.cause.as_mut() {
                cause.stack += stack_base;
            }
            d
        }));
        let phase = markers.phase.get(row).copied().unwrap_or(0);
        let start = m.start_time.get(row).copied().flatten();
        let end = m.end_time.get(row).copied().flatten();
        let (start, end) = phase_times(phase, start, end);
        markers.start_time.push(start);
        markers.end_time.push(end);
    }

    Ok(DecodedThread {
        tid: t.tid,
        pid: t.pid,
        name: t.name,
        process_name: t.process_name,
        register_time: t.register_time,
        samples,
        markers,
    })
}
```

* [ ] **Step 5: Implement the per-thread decoder**

Put this above the test module in `src/profile/wire/legacy.rs`. The five `Legacy*` structs are today's `RawFrameTable`, `RawFuncTable`, `RawStackTable`, `RawResourceTable`, and `RawNativeSymbols` from `raw.rs`, moved with their comments and serde attributes and renamed. Task 3 deletes the originals.

```rust
//! Per-thread layout (versions 49 through 55).
//!
//! Each thread carries its own string array and tables. The decoder
//! concatenates them into [`SharedTables`], offsetting every
//! cross-reference, and merges nested processes' `libs` into one list.

use serde::Deserialize;

use super::{DecodedProfile, WireProfile, WireThread, col, decode_thread, map_string};
use crate::profile::tables::SharedTables;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyFrameTable {
    pub length: usize,
    pub address: Vec<i64>, // -1 for non-native
    pub func: Vec<usize>,
    pub line: Vec<Option<u32>>,
    pub column: Vec<Option<u32>>,
    pub category: Vec<Option<usize>>,
    pub subcategory: Vec<Option<usize>>,
    #[serde(default)]
    pub native_symbol: Vec<Option<usize>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyFuncTable {
    pub length: usize,
    pub name: Vec<usize>, // string-array index
    #[serde(rename = "isJS")]
    pub is_js: Vec<bool>,
    #[serde(rename = "relevantForJS")]
    pub relevant_for_js: Vec<bool>,
    pub resource: Vec<i32>,            // -1 if no resource
    pub file_name: Vec<Option<usize>>, // string-array index
    pub line_number: Vec<Option<u32>>,
    pub column_number: Vec<Option<u32>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyStackTable {
    pub length: usize,
    pub frame: Vec<usize>,
    #[serde(default)]
    pub category: Vec<usize>,
    #[serde(default)]
    pub subcategory: Vec<usize>,
    pub prefix: Vec<Option<usize>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyResourceTable {
    pub length: usize,
    pub lib: Vec<Option<usize>>,
    pub name: Vec<usize>, // string-array index
    pub host: Vec<Option<usize>>,
    #[serde(rename = "type")]
    pub type_: Vec<u8>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyNativeSymbols {
    pub length: usize,
    pub lib_index: Vec<usize>,
    pub address: Vec<i64>,
    pub name: Vec<usize>, // string-array index
    pub function_size: Vec<Option<u64>>,
}

pub(super) fn decode(w: WireProfile) -> Result<DecodedProfile, String> {
    let mut shared = SharedTables {
        libs: w.libs,
        ..Default::default()
    };
    let mut threads = Vec::with_capacity(w.threads.len());
    for (i, t) in w.threads.into_iter().enumerate() {
        threads.push(append_thread(&mut shared, t, 0).map_err(|e| format!("threads[{i}]: {e}"))?);
    }
    let mut processes = Vec::with_capacity(w.processes.len());
    for (pi, p) in w.processes.into_iter().enumerate() {
        // Each process's resource tables index its own `libs`, so its
        // libraries land after the ones merged so far.
        let lib_base = shared.libs.len();
        shared.libs.extend(p.libs);
        let mut process_threads = Vec::with_capacity(p.threads.len());
        for (i, t) in p.threads.into_iter().enumerate() {
            process_threads.push(
                append_thread(&mut shared, t, lib_base)
                    .map_err(|e| format!("processes[{pi}].threads[{i}]: {e}"))?,
            );
        }
        processes.push(process_threads);
    }
    Ok(DecodedProfile {
        meta: w.meta,
        shared,
        threads,
        processes,
    })
}

fn append_thread(
    shared: &mut SharedTables,
    mut t: WireThread,
    lib_base: usize,
) -> Result<super::DecodedThread, String> {
    let strings = t.string_array.take().ok_or("missing stringArray")?;
    let frames = t.frame_table.take().ok_or("missing frameTable")?;
    let funcs = t.func_table.take().ok_or("missing funcTable")?;
    let stacks = t.stack_table.take().ok_or("missing stackTable")?;
    let resources = t.resource_table.take().ok_or("missing resourceTable")?;
    let native_symbols = t.native_symbols.take();

    let smap = shared.strings.intern_all(&strings);
    let func_base = shared.funcs.len();
    let frame_base = shared.frames.len();
    let stack_base = shared.stacks.len();
    let resource_base = shared.resources.len();
    let native_base = shared.native_symbols.len();

    // Resource libs index this thread's process `libs`; keep them local
    // so frames can look them up before the offsets apply.
    let mut resource_libs = Vec::with_capacity(resources.length);
    for r in 0..resources.length {
        let name = *col(&resources.name, r, "resourceTable.name")?;
        shared
            .resources
            .name
            .push(map_string(&smap, name, "resourceTable.name", r)?);
        let host = *col(&resources.host, r, "resourceTable.host")?;
        shared.resources.host.push(
            host.map(|h| map_string(&smap, h, "resourceTable.host", r))
                .transpose()?,
        );
        shared
            .resources
            .type_
            .push(*col(&resources.type_, r, "resourceTable.type")?);
        resource_libs.push(col(&resources.lib, r, "resourceTable.lib")?.map(|l| l + lib_base));
    }

    for f in 0..funcs.length {
        let name = *col(&funcs.name, f, "funcTable.name")?;
        shared
            .funcs
            .name
            .push(map_string(&smap, name, "funcTable.name", f)?);
        shared
            .funcs
            .is_js
            .push(*col(&funcs.is_js, f, "funcTable.isJS")?);
        shared
            .funcs
            .relevant_for_js
            .push(*col(&funcs.relevant_for_js, f, "funcTable.relevantForJS")?);
        let resource = *col(&funcs.resource, f, "funcTable.resource")?;
        shared.funcs.resource.push(
            usize::try_from(resource)
                .ok()
                .map(|r| r + resource_base),
        );
        let file = *col(&funcs.file_name, f, "funcTable.fileName")?;
        shared.funcs.file_name.push(
            file.map(|s| map_string(&smap, s, "funcTable.fileName", f))
                .transpose()?,
        );
        shared
            .funcs
            .line_number
            .push(funcs.line_number.get(f).copied().flatten());
        shared
            .funcs
            .column_number
            .push(funcs.column_number.get(f).copied().flatten());
    }

    if let Some(ns) = native_symbols {
        for n in 0..ns.length {
            let lib = *col(&ns.lib_index, n, "nativeSymbols.libIndex")?;
            shared.native_symbols.lib_index.push(lib + lib_base);
            let address = *col(&ns.address, n, "nativeSymbols.address")?;
            shared
                .native_symbols
                .address
                .push(u32::try_from(address).ok());
            let name = *col(&ns.name, n, "nativeSymbols.name")?;
            shared
                .native_symbols
                .name
                .push(map_string(&smap, name, "nativeSymbols.name", n)?);
            shared
                .native_symbols
                .function_size
                .push(ns.function_size.get(n).copied().flatten());
        }
    }

    for fr in 0..frames.length {
        let func = *col(&frames.func, fr, "frameTable.func")?;
        shared.frames.func.push(func + func_base);
        let address = *col(&frames.address, fr, "frameTable.address")?;
        shared.frames.address.push(u32::try_from(address).ok());
        // The per-thread layout reaches a frame's library through
        // func, then resource, then resource.lib.
        let lib = funcs
            .resource
            .get(func)
            .and_then(|&r| usize::try_from(r).ok())
            .and_then(|r| resource_libs.get(r).copied().flatten());
        shared.frames.lib.push(lib);
        shared
            .frames
            .line
            .push(frames.line.get(fr).copied().flatten());
        shared
            .frames
            .column
            .push(frames.column.get(fr).copied().flatten());
        shared.frames.native_symbol.push(
            frames
                .native_symbol
                .get(fr)
                .copied()
                .flatten()
                .map(|n| n + native_base),
        );
        shared.frames.inlined.push(false);
    }

    for s in 0..stacks.length {
        let frame = *col(&stacks.frame, s, "stackTable.frame")?;
        shared.stacks.frame.push(frame + frame_base);
        let prefix = *col(&stacks.prefix, s, "stackTable.prefix")?;
        shared.stacks.prefix.push(prefix.map(|p| p + stack_base));
    }

    decode_thread(t, &smap, stack_base)
}
```

* [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --lib profile::wire`
Expected: 6 tests pass.

* [ ] **Step 7: Run the full suite**

Run: `cargo test`
Expected: all tests pass. Nothing calls the new code yet.

* [ ] **Step 8: Format, lint, commit**

```bash
cargo fmt
cargo clippy --all-targets
git add src/profile/wire src/profile/mod.rs src/profile/raw.rs
git commit -m "feat: decode per-thread profiles into shared tables"
```

---

### Task 3: Switch the model to shared tables

One commit, because every table consumer changes together. The `RawProfile` name and its `Deserialize` entry point stay, so tests that call `serde_json::from_str::<RawProfile>` keep compiling.

**Files:**
* Modify: `src/profile/raw.rs`, `src/profile/wire/mod.rs`, `src/profile/wire/legacy.rs`, `src/profile/parsed.rs`, `src/profile/symbolicate.rs`, `src/query/asm.rs`, `src/query/event.rs`, `src/matching.rs`, `src/session.rs` (only if it names moved types)
* Modify test modules: `src/query/compare.rs`, `src/query/call_tree.rs`, `src/query/top_functions.rs`, `src/query/source.rs`, `src/profile/raw.rs`

**Interfaces:**
* Consumes: Tasks 1 and 2.
* Produces:
  * `pub struct RawProfile { meta: RawMeta, shared: SharedTables, threads: Vec<RawThread>, processes: Vec<RawProcess> }`, deserialized through `WireProfile`.
  * `pub struct RawProcess { threads: Vec<RawThread> }`.
  * `pub struct RawThread { tid, pid, name, process_name, register_time, samples: RawSampleTable, markers: RawMarkerTable }`.
  * `pub struct RawMarkerTable` with `start_time: Vec<Option<f64>>` and `end_time: Vec<Option<f64>>`.
  * `Profile::shared(&self) -> &SharedTables`.

* [ ] **Step 1: Write the failing test for interval-end markers**

Add to the test module of `src/profile/parsed.rs`:

```rust
    #[test]
    fn interval_end_marker_uses_end_time() {
        let json = r#"{
            "meta": {"interval": 1.0, "startTime": 0.0},
            "threads": [{"tid": 1, "pid": 1, "registerTime": 0.0,
                "stringArray": ["f", "ev"],
                "frameTable": {"length": 1, "address": [-1], "func": [0], "line": [null], "column": [null], "category": [0], "subcategory": [0]},
                "funcTable": {"length": 1, "name": [0], "isJS": [false], "relevantForJS": [false], "resource": [-1], "fileName": [null], "lineNumber": [null], "columnNumber": [null]},
                "stackTable": {"length": 1, "frame": [0], "prefix": [null]},
                "resourceTable": {"length": 0, "lib": [], "name": [], "host": [], "type": []},
                "samples": {"length": 1, "stack": [0], "time": [0.0]},
                "markers": {"length": 1, "data": [{"type": "Other event", "cause": {"stack": 0}}],
                            "name": [1], "startTime": [null], "endTime": [5.0], "phase": [3], "category": [0]}}]
        }"#;
        let raw: RawProfile = serde_json::from_str(json).unwrap();
        let p = Profile::from_raw(raw);
        let handle = p.threads().next().unwrap().handle();
        let ev = EventSource::Marker("ev".into());
        let inside: Vec<_> = p.stack_indices(handle, &ev, Some([4.0, 6.0])).collect();
        assert_eq!(inside, vec![Some(0)]);
        let outside: Vec<_> = p.stack_indices(handle, &ev, Some([0.0, 1.0])).collect();
        assert!(outside.is_empty());
    }
```

Add `use crate::profile::raw::RawProfile;` and `use crate::profile::event_source::EventSource;` to the test module if it lacks them.

Run: `cargo test --lib interval_end_marker_uses_end_time`
Expected: FAIL. Today's `RawMarkerTable.start_time` is `Vec<f64>`, so the null makes serde fail and the `unwrap` panics.

* [ ] **Step 2: Replace the raw model**

In `src/profile/raw.rs`:
* Delete `RawFrameTable`, `RawFuncTable`, `RawStackTable`, `RawResourceTable`, and `RawNativeSymbols` (now in `wire/legacy.rs`).
* Replace `RawProfile`, `RawThread`, and `RawMarkerTable` with the following, keeping the existing doc comments on `RawThread.markers`, `RawThread.process_name`, and `RawMarkerTable`, and moving the `inline_chains` doc comment to `SharedTables::inline_chains` (already present from Task 1):

```rust
/// A decoded profile. Both on-disk layouts deserialize into this
/// through [`crate::profile::wire::WireProfile`], which also applies
/// the version gate.
#[derive(Debug, Deserialize)]
#[serde(try_from = "crate::profile::wire::WireProfile")]
pub struct RawProfile {
    pub meta: RawMeta,
    pub shared: SharedTables,
    pub threads: Vec<RawThread>,
    pub processes: Vec<RawProcess>,
}

/// Threads of a nested process in the per-thread layout. Its
/// libraries are merged into [`SharedTables::libs`].
#[derive(Debug)]
pub struct RawProcess {
    pub threads: Vec<RawThread>,
}

#[derive(Debug)]
pub struct RawThread {
    pub tid: u64,
    pub pid: Pid,
    pub name: Option<String>,
    /// (keep the existing process_name doc comment)
    pub process_name: Option<String>,
    pub register_time: f64,
    pub samples: RawSampleTable,
    /// (keep the existing markers doc comment)
    pub markers: RawMarkerTable,
}

/// (keep the existing RawMarkerTable doc comment, and add:)
/// Times are `None` where the marker phase makes them meaningless.
#[derive(Debug, Default)]
pub struct RawMarkerTable {
    pub length: usize,
    pub data: Vec<Option<RawMarkerData>>,
    /// String indices into [`SharedTables::strings`].
    pub name: Vec<usize>,
    pub start_time: Vec<Option<f64>>,
    pub end_time: Vec<Option<f64>>,
    pub phase: Vec<u8>,
    pub category: Vec<usize>,
}
```

Add `use crate::profile::tables::SharedTables;` at the top of `raw.rs`.

In `src/profile/wire/mod.rs` and `wire/legacy.rs`, delete `DecodedProfile`, `DecodedThread`, and `DecodedMarkerTable`, replace every use with `RawProfile`, `RawThread`, and `RawMarkerTable`, and wrap each process's thread list as `RawProcess { threads }` in `legacy::decode`. Then add the version gate to `wire/mod.rs`:

```rust
impl TryFrom<WireProfile> for RawProfile {
    type Error = String;

    fn try_from(w: WireProfile) -> Result<Self, String> {
        let profile = match w.meta.preprocessed_profile_version {
            None => legacy::decode(w)?,
            Some(v) if (49..=55).contains(&v) => legacy::decode(w)?,
            Some(v) => return Err(format!("unsupported processed-profile version {v}")),
        };
        validate(&profile)?;
        Ok(profile)
    }
}

/// Check every index a thread holds, after the table-level checks.
fn validate(p: &RawProfile) -> Result<(), String> {
    p.shared.validate_tables()?;
    let stacks = p.shared.stacks.len();
    let strings = p.shared.strings.len();
    let threads = p
        .threads
        .iter()
        .chain(p.processes.iter().flat_map(|pr| pr.threads.iter()));
    for t in threads {
        for (row, s) in t.samples.stack.iter().enumerate() {
            if let Some(s) = *s
                && s >= stacks
            {
                return Err(out_of_range("samples.stack", row, s, stacks));
            }
        }
        for (row, &n) in t.markers.name.iter().enumerate() {
            if n >= strings {
                return Err(out_of_range("markers.name", row, n, strings));
            }
        }
        for (row, d) in t.markers.data.iter().enumerate() {
            if let Some(c) = d.as_ref().and_then(|d| d.cause.as_ref())
                && c.stack >= stacks
            {
                return Err(out_of_range("markers.data.cause.stack", row, c.stack, stacks));
            }
        }
    }
    Ok(())
}
```

Task 4 adds the `Some(75)` arm.

* [ ] **Step 3: Port `parsed.rs`**

In `src/profile/parsed.rs`:
* `raw_thread` matches `Some(pi) => &self.raw.processes[pi].threads[handle.thread_idx]` (unchanged shape, `RawProcess` has `threads`).
* Add:

```rust
    /// The profile-global tables every thread indexes into.
    pub(crate) fn shared(&self) -> &crate::profile::tables::SharedTables {
        &self.raw.shared
    }
```

* `lib(idx)` returns `self.raw.shared.libs.get(idx)`. Update its doc comment to "Look up a library by its index in the merged global list."
* `all_libs()` returns `self.raw.shared.libs.iter()`. Keep the doc comment and change its second sentence to "Sub-process libraries are merged into the same list at load."
* `inline_chain` ignores the handle (keep the parameter) and returns `self.raw.shared.inline_chain(frame_idx)`.
* Replace the body of `frame_info` with:

```rust
        let t = &self.raw.shared;
        let func_idx = *t.frames.func.get(frame_idx)?;
        let func_name_idx = *t.funcs.name.get(func_idx)?;
        let function_name = t.strings.get(func_name_idx)?;
        let _ = handle;

        let lib = t
            .frames
            .lib
            .get(frame_idx)
            .copied()
            .flatten()
            .and_then(|li| self.lib(li));
        let module_name = lib.and_then(|l| l.name.as_deref());

        let file = t
            .funcs
            .file_name
            .get(func_idx)
            .copied()
            .flatten()
            .and_then(|si| t.strings.get(si));

        let line = t.frames.line.get(frame_idx).copied().flatten();
        let column = t.frames.column.get(frame_idx).copied().flatten();
        let address = t
            .frames
            .address
            .get(frame_idx)
            .copied()
            .flatten()
            .map(i64::from);

        Some(FrameInfo {
            function_name,
            module_name,
            file,
            line,
            column,
            address,
            lib,
        })
```

* Replace the body of `walk_stack` with:

```rust
        let _ = handle;
        let stacks = &self.raw.shared.stacks;
        let mut current = Some(stack_idx);
        std::iter::from_fn(move || {
            let s = current?;
            let frame = *stacks.frame.get(s)?;
            current = stacks.prefix.get(s).copied().flatten();
            Some(frame)
        })
```

* In `stack_indices`, the marker branch: replace `raw.string_array.iter().position(|s| s == name)` with `self.raw.shared.strings.position(name)`, update its comment from "once per thread" to "once per call", and replace the time lookup with:

```rust
                            // Interval-end markers carry only an end
                            // time; gate them by that.
                            let t = raw
                                .markers
                                .start_time
                                .get(i)
                                .copied()
                                .flatten()
                                .or_else(|| raw.markers.end_time.get(i).copied().flatten())?;
```

Update the `stack_indices` doc comment: markers gate by "`markers.start_time`, or `end_time` for interval-end markers".

* [ ] **Step 4: Port `symbolicate.rs`**

Replace `intern_string`, `symbolicate`, `symbolicate_threads`, and `symbolicate_thread` with one pass over `raw.shared`. Keep every existing comment inside the moved code, including the long comment on using the outer inline frame and the comments on outcome counting.

```rust
/// Symbolicate a `RawProfile` in-place.
///
/// Best-effort: any lib that wholesym cannot load is recorded with a
/// `LoadError` outcome and its frames are left as hex.
pub async fn symbolicate(
    raw: &mut RawProfile,
) -> Result<Vec<LibSymbolicationOutcome>, crate::error::ToolError> {
    // (keep the use_spotlight comment)
    let config = SymbolManagerConfig::new().use_spotlight(cfg!(target_os = "macos"));
    let symbol_manager = SymbolManager::with_config(config);

    let mut outcomes: HashMap<String, OutcomeAccum> = HashMap::new();
    symbolicate_tables(&symbol_manager, &mut raw.shared, &mut outcomes).await;

    let mut flat: Vec<LibSymbolicationOutcome> = outcomes
        .into_values()
        .map(OutcomeAccum::into_outcome)
        .collect();
    // (keep the stable-order comment)
    flat.sort_by(|a, b| {
        a.lib_name
            .cmp(&b.lib_name)
            .then_with(|| a.debug_id.cmp(&b.debug_id))
    });

    Ok(flat)
}

async fn symbolicate_tables(
    symbol_manager: &SymbolManager,
    t: &mut SharedTables,
    outcomes: &mut HashMap<String, OutcomeAccum>,
) {
    // Collect work: (frame_idx, func_idx, lib_idx, address)
    // We do this in a preliminary pass to avoid borrow conflicts.
    let mut work: Vec<(usize, usize, usize, u32)> = Vec::new();

    for frame_idx in 0..t.frames.len() {
        let Some(addr) = t.frames.address[frame_idx] else {
            continue; // non-native frame
        };
        let func_idx = t.frames.func[frame_idx];
        let name = t.strings.get(t.funcs.name[func_idx]).unwrap_or("");
        if !is_unsymbolicated(name) {
            continue; // already symbolicated
        }
        let Some(lib_idx) = t.frames.lib[frame_idx] else {
            continue;
        };
        work.push((frame_idx, func_idx, lib_idx, addr));
    }

    if work.is_empty() {
        return;
    }

    // Pre-size the parallel inline-chain table so per-frame writes below
    // can index directly. Empty Vec for frames without inline records.
    if t.inline_chains.len() < t.frames.len() {
        t.inline_chains.resize_with(t.frames.len(), Vec::new);
    }

    // Cache: lib_index → SymbolMap (or None if we failed to load it).
    // Library indices are profile-global, so each library loads once.
    let mut symbol_map_cache: HashMap<usize, Option<SymbolMap>> = HashMap::new();
    // (keep the rest of today's symbolicate_thread body: loading maps
    // for the needed libs, the outcome bookkeeping, and applying
    // results, with these substitutions:)
    //   libs.get(lib_idx)                 -> t.libs.get(lib_idx)
    //   intern_string(&mut thread.string_array, s) -> t.strings.intern(s)
    //   thread.func_table.name[func_idx]  -> t.funcs.name[func_idx]
    //   thread.func_table.file_name[..]   -> t.funcs.file_name[..]
    //   thread.frame_table.line[..]       -> t.frames.line[..]
    //   thread.inline_chains[frame_idx]   -> t.inline_chains[frame_idx]
}
```

Reads of `t.libs` next to writes of `t.strings`, `t.funcs`, `t.frames`, and `t.inline_chains` borrow disjoint fields of `t`, so the moved code compiles without restructuring.

Delete `intern_string`. Add `use crate::profile::tables::SharedTables;`, and drop `RawThread` from the imports if nothing else uses it.

* [ ] **Step 5: Port `asm.rs`, `event.rs`, and `matching.rs`**

In `src/query/asm.rs`, replace the lib resolution and native-symbol block inside the frame loop with:

```rust
                let shared = profile.shared();
                // The frame carries its library directly.
                let Some(lib_idx) = shared.frames.lib.get(frame_idx).copied().flatten() else {
                    continue;
                };

                // Try to get start/size from nativeSymbols.
                if native_loc.is_none()
                    && let Some(ns_idx) = shared.frames.native_symbol.get(frame_idx).copied().flatten()
                    && let Some(ns_addr) = shared.native_symbols.address.get(ns_idx).copied().flatten()
                {
                    let ns_size = shared
                        .native_symbols
                        .function_size
                        .get(ns_idx)
                        .copied()
                        .flatten()
                        .unwrap_or(0);
                    native_loc = Some((ns_addr, ns_size as u32, lib_idx));
                }
```

Keep `let raw = profile.raw_thread(handle);` for the `raw.samples.stack` loop.

In `src/query/event.rs`, `marker_lookup` and `known_marker_events` resolve names through `profile.shared().strings.get(str_idx)`. In `marker_lookup`, first compute `let Some(target_idx) = profile.shared().strings.position(target) else { return MarkerLookup::Unknown; };` and compare `str_idx == target_idx`.

In `src/matching.rs` `nearest_matches_for_error`, replace the per-thread loop with one pass over functions:

```rust
    let shared = profile.shared();
    for func_idx in 0..shared.funcs.len() {
        let Some(name) = shared.strings.get(shared.funcs.name[func_idx]) else {
            continue;
        };

        // Cheap dedup: several funcs can share a name. Heap holds at
        // most K+1 entries, so this scan is bounded.
        if heap.iter().any(|Reverse((_, n))| n == name) {
            continue;
        }
        // (rest of the loop body unchanged)
```

Replace uses of the `String` `name` with the `&str` `name` in the rest of the body (`name.to_owned()` where a `String` is pushed).

* [ ] **Step 6: Port the test modules**

Apply these rewrites:
* `raw.threads[0].string_array[i] = "X".to_owned();` becomes `raw.shared.strings.replace(i, "X");` in `call_tree.rs:891`, `call_tree.rs:917`, `compare.rs:423`, `compare.rs:712`, and `compare.rs:713`. The fixtures have one thread and no duplicate strings, so indices are unchanged.
* `t.inline_chains.resize_with(t.frame_table.length, Vec::new); t.inline_chains[N] = vec![...];` becomes `raw.shared.set_inline_chain(N, vec![...]);` in `top_functions.rs:290`, `call_tree.rs:778`, and `source.rs:497`, `:546`, `:613`, `:670`, `:741`. Drop the `let t = &mut raw.threads[0];` binding where it becomes unused.
* In `compare.rs` around line 676, replace the resource-table block with:

```rust
        let lib_idx = raw.shared.libs.len();
        raw.shared.libs.push(RawLib {
            name: Some(module_name.to_owned()),
            ..Default::default()
        });

        // Point every frame at the new lib so each resolves to the
        // same module.
        for slot in &mut raw.shared.frames.lib {
            *slot = Some(lib_idx);
        }

        Profile::from_raw(raw)
```

* In `src/profile/raw.rs` tests, replace assertions on `p.threads[0].string_array`, `frame_table`, or `markers.start_time` with the equivalents on `p.shared` and `Option` times. For example `assert_eq!(p.threads[0].markers.start_time, vec![1.0])` becomes `assert_eq!(p.threads[0].markers.start_time, vec![Some(1.0)])`.

Run `cargo build --all-targets 2>&1 | grep -E "^error" | head` and fix the remaining compile errors the same way. Every remaining error is a moved field access.

* [ ] **Step 7: Run the full suite**

Run: `cargo test`
Expected: all tests pass, including `interval_end_marker_uses_end_time`. Snapshot tests must not change. If an insta snapshot changes, stop and report the diff instead of accepting it, because this task must not change query output.

* [ ] **Step 8: Format, lint, commit**

```bash
cargo fmt
cargo clippy --all-targets
git add -u src
git commit -m "refactor: read profiles through profile-global shared tables

Threads keep only samples and markers. Symbolication runs once per
profile, and sub-process frames resolve their library from the merged
global list instead of the root libs."
```

---

### Task 4: Version 75 decoder and loader errors

**Files:**
* Create: `src/profile/wire/v75.rs`
* Modify: `src/profile/wire/mod.rs`, `src/profile/load.rs`, `src/error.rs`

**Interfaces:**
* Consumes: Tasks 1 to 3.
* Produces:
  * `pub struct WireShared` (deserialized from `profile.shared`) and `pub(super) fn v75::decode(w: WireProfile) -> Result<RawProfile, String>`.
  * `ToolError::UnsupportedProfileFormat { path, version: String, supported: String }`.
  * `pub(crate) fn load::decode_bytes(bytes: &[u8]) -> Result<RawProfile, LoadError>` and `pub const load::SUPPORTED_VERSIONS: &str = "49-55, 75"`.

* [ ] **Step 1: Write the failing decoder tests**

Create `src/profile/wire/v75.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use crate::profile::raw::RawProfile;

    /// Frame 0 is a label frame, frame 1 is native with a line, an
    /// inlined flag, and a native symbol. Func 1 has a resource, a
    /// source, and a line number.
    fn profile(prefix_offset: &str) -> String {
        format!(
            r#"{{
            "meta": {{"interval": 1.0, "startTime": 0.0, "preprocessedProfileVersion": 75}},
            "libs": [{{"name": "libx"}}],
            "shared": {{
                "stringArray": ["root", "leaf", "src.rs", "libx", "ev"],
                "frameTable": {{"length": 2, "flags": [4, 31], "func": [0, 1], "category": [0, 0], "subcategory": [0, 0],
                    "line": [0, 12], "column": [0, 0], "address": [0, 4096], "lib": [0, 0], "nativeSymbol": [0, 0],
                    "innerWindowID": [0, 0], "originalLocation": [0, 0]}},
                "funcTable": {{"length": 2, "flags": [0, 29], "name": [0, 1], "resource": [0, 0], "source": [0, 0],
                    "lineNumber": [0, 10], "columnNumber": [0, 0], "originalLocation": [0, 0]}},
                "stackTable": {{"length": 2, "prefixOffset": {prefix_offset}, "frame": [0, 1]}},
                "resourceTable": {{"length": 1, "name": [3], "host": [null], "type": [1]}},
                "nativeSymbols": {{"length": 1, "address": [4000], "functionSize": [-1], "libIndex": [0], "name": [1]}},
                "sources": {{"length": 1, "id": [null], "filename": [2], "startLine": [1], "startColumn": [1], "sourceMapURL": [null], "content": [null]}},
                "sourceLocationTable": {{"length": 0, "source": [], "line": [], "column": []}}
            }},
            "threads": [{{"tid": "7.1", "pid": "7.1", "registerTime": 0.0, "name": "t",
                "samples": {{"length": 2, "weightType": "samples", "stack": [1, null], "weight": [1, 1], "threadCPUDelta": [0, 0], "timeDeltas": [0.5, 1.0]}},
                "markers": {{"length": 2, "category": [0, 0],
                    "data": [{{"type": "Other event", "cause": {{"stack": 1}}}}, {{"type": "Other event"}}],
                    "endTime": [0, 7.0], "name": [4, 4], "phase": [0, 3], "startTime": [0.25, 0]}}}}]
        }}"#
        )
    }

    fn decode(prefix_offset: &str) -> Result<RawProfile, serde_json::Error> {
        serde_json::from_str(&profile(prefix_offset))
    }

    #[test]
    fn prefix_offset_becomes_absolute_prefix() {
        let p = decode("[0, 1]").unwrap();
        assert_eq!(p.shared.stacks.prefix, vec![None, Some(0)]);
    }

    #[test]
    fn prefix_offset_past_row_is_rejected() {
        let err = decode("[0, 2]").unwrap_err().to_string();
        assert!(err.contains("stackTable.prefixOffset[1] = 2 points before the table start"), "{err}");
    }

    #[test]
    fn frame_flags_gate_columns() {
        let p = decode("[0, 1]").unwrap();
        let f = &p.shared.frames;
        assert_eq!(f.address, vec![None, Some(4096)]);
        assert_eq!(f.lib, vec![None, Some(0)]);
        assert_eq!(f.line, vec![None, Some(12)]);
        assert_eq!(f.column, vec![None, None]);
        assert_eq!(f.native_symbol, vec![None, Some(0)]);
        assert_eq!(f.inlined, vec![false, true]);
    }

    #[test]
    fn func_flags_gate_columns() {
        let p = decode("[0, 1]").unwrap();
        let t = &p.shared;
        assert_eq!(t.funcs.is_js, vec![false, true]);
        assert_eq!(t.funcs.relevant_for_js, vec![false, false]);
        assert_eq!(t.funcs.resource, vec![None, Some(0)]);
        assert_eq!(t.funcs.file_name[0], None);
        assert_eq!(t.strings.get(t.funcs.file_name[1].unwrap()), Some("src.rs"));
        assert_eq!(t.funcs.line_number, vec![None, Some(10)]);
    }

    #[test]
    fn native_symbol_size_sentinel_is_none() {
        let p = decode("[0, 1]").unwrap();
        assert_eq!(p.shared.native_symbols.function_size, vec![None]);
        assert_eq!(p.shared.native_symbols.address, vec![Some(4000)]);
    }

    #[test]
    fn thread_ids_and_markers_decode() {
        let p = decode("[0, 1]").unwrap();
        let t = &p.threads[0];
        assert_eq!(t.tid, 7);
        assert_eq!(t.pid.suffix, Some(1));
        assert_eq!(t.samples.absolute_times(), vec![0.5, 1.5]);
        assert_eq!(t.markers.start_time, vec![Some(0.25), None]);
        assert_eq!(t.markers.end_time, vec![None, Some(7.0)]);
        let ev = p.shared.strings.position("ev").unwrap();
        assert_eq!(t.markers.name, vec![ev, ev]);
    }
}
```

Flag values used above: frame flags 4 is `HasCategory`, and 31 is `IsInlined | HasAddress | HasCategory | HasNativeSymbol | HasLine` (1 + 2 + 4 + 8 + 16), leaving `HasColumn` unset. Func flags 29 is `IsJS | HasResource | HasSource | HasLine` (1 + 4 + 8 + 16).

Add `mod v75;` to `src/profile/wire/mod.rs`.

* [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib profile::wire::v75`
Expected: FAIL with "unsupported processed-profile version 75".

* [ ] **Step 3: Implement the shared decoder**

Put this above the test module in `src/profile/wire/v75.rs`:

```rust
//! Shared layout, version 75.
//!
//! Column semantics follow the Firefox Profiler's
//! `docs-developer/CHANGELOG-formats.md`. Nullable columns hold 0 when
//! unset, and a flag bit says whether the value is meaningful.

use serde::Deserialize;

use super::{WireProfile, col, decode_thread, map_string};
use crate::profile::raw::RawProfile;
use crate::profile::tables::SharedTables;

// Frame flags (version 71).
const FRAME_IS_INLINED: u8 = 1 << 0;
const FRAME_HAS_ADDRESS: u8 = 1 << 1;
const FRAME_HAS_NATIVE_SYMBOL: u8 = 1 << 3;
const FRAME_HAS_LINE: u8 = 1 << 4;
const FRAME_HAS_COLUMN: u8 = 1 << 5;

// Func flags (version 75).
const FUNC_IS_JS: u8 = 1 << 0;
const FUNC_RELEVANT_FOR_JS: u8 = 1 << 1;
const FUNC_HAS_RESOURCE: u8 = 1 << 2;
const FUNC_HAS_SOURCE: u8 = 1 << 3;
const FUNC_HAS_LINE: u8 = 1 << 4;
const FUNC_HAS_COLUMN: u8 = 1 << 5;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireShared {
    pub string_array: Vec<String>,
    pub frame_table: FrameTable,
    pub func_table: FuncTable,
    pub stack_table: StackTable,
    pub resource_table: ResourceTable,
    #[serde(default)]
    pub native_symbols: Option<NativeSymbols>,
    #[serde(default)]
    pub sources: Option<Sources>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameTable {
    pub length: usize,
    pub flags: Vec<u8>,
    pub func: Vec<usize>,
    pub line: Vec<i64>,
    pub column: Vec<i64>,
    pub address: Vec<u32>,
    /// Version 70 wrote -1 for frames without a library, version 71
    /// and later write 0 and clear `HasAddress`.
    pub lib: Vec<i64>,
    pub native_symbol: Vec<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FuncTable {
    pub length: usize,
    pub flags: Vec<u8>,
    pub name: Vec<usize>,
    pub resource: Vec<i64>,
    pub source: Vec<i64>,
    pub line_number: Vec<i64>,
    pub column_number: Vec<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackTable {
    pub length: usize,
    pub frame: Vec<usize>,
    pub prefix_offset: Vec<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceTable {
    pub length: usize,
    pub name: Vec<usize>,
    pub host: Vec<Option<usize>>,
    #[serde(rename = "type")]
    pub type_: Vec<u8>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSymbols {
    pub length: usize,
    pub lib_index: Vec<usize>,
    pub address: Vec<i64>,
    pub name: Vec<usize>,
    /// -1 means unknown (version 74).
    pub function_size: Vec<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sources {
    pub length: usize,
    /// String index.
    pub filename: Vec<usize>,
}

/// `value` as an index when `bit` is set in `flags`.
fn gated_index(flags: u8, bit: u8, value: i64, column: &str, row: usize) -> Result<Option<usize>, String> {
    if flags & bit == 0 {
        return Ok(None);
    }
    usize::try_from(value)
        .map(Some)
        .map_err(|_| format!("{column}[{row}] = {value} is negative"))
}

/// `value` as a line or column number when `bit` is set in `flags`.
fn gated_u32(flags: u8, bit: u8, value: i64, column: &str, row: usize) -> Result<Option<u32>, String> {
    if flags & bit == 0 {
        return Ok(None);
    }
    u32::try_from(value)
        .map(Some)
        .map_err(|_| format!("{column}[{row}] = {value} does not fit a line number"))
}

pub(super) fn decode(mut w: WireProfile) -> Result<RawProfile, String> {
    let wire = w.shared.take().ok_or("version 75 profile has no shared section")?;
    if !w.processes.is_empty() {
        return Err("nested processes are not part of the shared layout".to_owned());
    }
    let mut shared = SharedTables {
        libs: w.libs,
        ..Default::default()
    };
    let smap = shared.strings.intern_all(&wire.string_array);

    let mut source_files = Vec::new();
    if let Some(src) = &wire.sources {
        for i in 0..src.length {
            let f = *col(&src.filename, i, "sources.filename")?;
            source_files.push(map_string(&smap, f, "sources.filename", i)?);
        }
    }

    let rt = &wire.resource_table;
    for r in 0..rt.length {
        let name = *col(&rt.name, r, "resourceTable.name")?;
        shared.resources.name.push(map_string(&smap, name, "resourceTable.name", r)?);
        let host = *col(&rt.host, r, "resourceTable.host")?;
        shared.resources.host.push(
            host.map(|h| map_string(&smap, h, "resourceTable.host", r))
                .transpose()?,
        );
        shared.resources.type_.push(*col(&rt.type_, r, "resourceTable.type")?);
    }

    if let Some(ns) = &wire.native_symbols {
        for n in 0..ns.length {
            shared
                .native_symbols
                .lib_index
                .push(*col(&ns.lib_index, n, "nativeSymbols.libIndex")?);
            let address = *col(&ns.address, n, "nativeSymbols.address")?;
            shared.native_symbols.address.push(u32::try_from(address).ok());
            let name = *col(&ns.name, n, "nativeSymbols.name")?;
            shared
                .native_symbols
                .name
                .push(map_string(&smap, name, "nativeSymbols.name", n)?);
            let size = *col(&ns.function_size, n, "nativeSymbols.functionSize")?;
            shared.native_symbols.function_size.push(u64::try_from(size).ok());
        }
    }

    let ft = &wire.func_table;
    for f in 0..ft.length {
        let flags = *col(&ft.flags, f, "funcTable.flags")?;
        let name = *col(&ft.name, f, "funcTable.name")?;
        shared.funcs.name.push(map_string(&smap, name, "funcTable.name", f)?);
        shared.funcs.is_js.push(flags & FUNC_IS_JS != 0);
        shared.funcs.relevant_for_js.push(flags & FUNC_RELEVANT_FOR_JS != 0);
        shared.funcs.resource.push(gated_index(
            flags,
            FUNC_HAS_RESOURCE,
            *col(&ft.resource, f, "funcTable.resource")?,
            "funcTable.resource",
            f,
        )?);
        let source = gated_index(
            flags,
            FUNC_HAS_SOURCE,
            *col(&ft.source, f, "funcTable.source")?,
            "funcTable.source",
            f,
        )?;
        let file_name = match source {
            None => None,
            Some(s) => Some(*col(&source_files, s, "sources")?),
        };
        shared.funcs.file_name.push(file_name);
        shared.funcs.line_number.push(gated_u32(
            flags,
            FUNC_HAS_LINE,
            *col(&ft.line_number, f, "funcTable.lineNumber")?,
            "funcTable.lineNumber",
            f,
        )?);
        shared.funcs.column_number.push(gated_u32(
            flags,
            FUNC_HAS_COLUMN,
            *col(&ft.column_number, f, "funcTable.columnNumber")?,
            "funcTable.columnNumber",
            f,
        )?);
    }

    let fr = &wire.frame_table;
    for i in 0..fr.length {
        let flags = *col(&fr.flags, i, "frameTable.flags")?;
        shared.frames.func.push(*col(&fr.func, i, "frameTable.func")?);
        // `HasAddress` gates both the address and the library, because
        // an address is only meaningful relative to its library.
        let has_address = flags & FRAME_HAS_ADDRESS != 0;
        let address = *col(&fr.address, i, "frameTable.address")?;
        shared.frames.address.push(has_address.then_some(address));
        shared.frames.lib.push(gated_index(
            flags,
            FRAME_HAS_ADDRESS,
            *col(&fr.lib, i, "frameTable.lib")?,
            "frameTable.lib",
            i,
        )?);
        shared.frames.native_symbol.push(gated_index(
            flags,
            FRAME_HAS_NATIVE_SYMBOL,
            *col(&fr.native_symbol, i, "frameTable.nativeSymbol")?,
            "frameTable.nativeSymbol",
            i,
        )?);
        shared.frames.line.push(gated_u32(
            flags,
            FRAME_HAS_LINE,
            *col(&fr.line, i, "frameTable.line")?,
            "frameTable.line",
            i,
        )?);
        shared.frames.column.push(gated_u32(
            flags,
            FRAME_HAS_COLUMN,
            *col(&fr.column, i, "frameTable.column")?,
            "frameTable.column",
            i,
        )?);
        shared.frames.inlined.push(flags & FRAME_IS_INLINED != 0);
    }

    let st = &wire.stack_table;
    for i in 0..st.length {
        shared.stacks.frame.push(*col(&st.frame, i, "stackTable.frame")?);
        // 0 marks a root; any other offset points back to the parent,
        // which the format guarantees comes first.
        let offset = *col(&st.prefix_offset, i, "stackTable.prefixOffset")?;
        let prefix = match offset {
            0 => None,
            k if k <= i => Some(i - k),
            k => {
                return Err(format!(
                    "stackTable.prefixOffset[{i}] = {k} points before the table start"
                ));
            }
        };
        shared.stacks.prefix.push(prefix);
    }

    let mut threads = Vec::with_capacity(w.threads.len());
    for (i, t) in w.threads.into_iter().enumerate() {
        threads.push(decode_thread(t, &smap, 0).map_err(|e| format!("threads[{i}]: {e}"))?);
    }

    Ok(RawProfile {
        meta: w.meta,
        shared,
        threads,
        processes: Vec::new(),
    })
}
```

In `src/profile/wire/mod.rs`:
* Add to `WireProfile`, after `libs`:

```rust
    /// Shared layout only.
    #[serde(default)]
    pub shared: Option<v75::WireShared>,
```

* In `legacy::decode`, return an error first thing when `w.shared.is_some()`: `"per-thread layout profile has a shared section"`.
* In the version gate, add `Some(75) => v75::decode(w)?,` before the catch-all arm.

* [ ] **Step 4: Run the decoder tests**

Run: `cargo test --lib profile::wire`
Expected: all pass.

* [ ] **Step 5: Write the failing loader tests**

Add to the test module in `src/profile/load.rs`:

```rust
    #[test]
    fn version_60_reports_unsupported() {
        // Version 60 has `shared` but pre-flag frame columns, so serde
        // fails before the version gate. The error path peeks the version.
        let json = r#"{"meta": {"interval": 1.0, "startTime": 0.0, "preprocessedProfileVersion": 60},
                       "shared": {"stringArray": [], "frameTable": {"length": 0, "address": []}}, "threads": []}"#;
        let err = decode_bytes(json.as_bytes()).unwrap_err();
        assert!(matches!(err, LoadError::UnsupportedVersion(60)), "{err:?}");
    }

    #[test]
    fn version_76_reports_unsupported() {
        let json = r#"{"meta": {"interval": 1.0, "startTime": 0.0, "preprocessedProfileVersion": 76}, "threads": []}"#;
        let err = decode_bytes(json.as_bytes()).unwrap_err();
        assert!(matches!(err, LoadError::UnsupportedVersion(76)), "{err:?}");
    }

    #[test]
    fn supported_versions_pass_the_gate() {
        for v in [49, 55] {
            let json = format!(
                r#"{{"meta": {{"interval": 1.0, "startTime": 0.0, "preprocessedProfileVersion": {v}}}, "threads": []}}"#
            );
            decode_bytes(json.as_bytes()).unwrap();
        }
    }

    #[test]
    fn schema_error_in_supported_version_is_not_a_profile() {
        let json = r#"{"meta": {"interval": 1.0, "startTime": 0.0, "preprocessedProfileVersion": 55},
                       "threads": [{"tid": 1}]}"#;
        let err = decode_bytes(json.as_bytes()).unwrap_err();
        assert!(matches!(err, LoadError::Json(_)), "{err:?}");
    }

    #[test]
    fn unsupported_version_maps_to_tool_error() {
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        f.write_all(br#"{"meta": {"interval": 1.0, "startTime": 0.0, "preprocessedProfileVersion": 74}, "threads": []}"#)
            .unwrap();
        let err = load_from_path(f.path()).unwrap_err();
        match err {
            ToolError::UnsupportedProfileFormat { version, supported, .. } => {
                assert_eq!(version, "74");
                assert_eq!(supported, SUPPORTED_VERSIONS);
            }
            other => panic!("unexpected error {other:?}"),
        }
    }
```

Run: `cargo test --lib profile::load`
Expected: compile errors, `decode_bytes` not found.

* [ ] **Step 6: Implement the loader**

In `src/error.rs`, change the variant to:

```rust
    UnsupportedProfileFormat {
        path: PathBuf,
        version: String,
        /// Versions pollard can read, e.g. `"49-55, 75"`.
        supported: String,
    },
```

and update its arm in `impl Display for ToolError` if that impl matches on variants.

Replace everything above the test module in `src/profile/load.rs` with:

```rust
//! Read a profile file into our raw types.
//!
//! The container is detected from magic bytes: gzip first, then JSLB,
//! and plain JSON otherwise.

#![allow(dead_code)]

use std::io::Read;
use std::path::Path;

use crate::error::ToolError;
use crate::profile::raw::RawProfile;
use crate::profile::wire::is_supported_version;

const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Processed-profile versions the decoders understand, for error
/// messages. Keep in sync with [`is_supported_version`].
pub const SUPPORTED_VERSIONS: &str = "49-55, 75";

#[derive(Debug)]
pub(crate) enum LoadError {
    Gzip(String),
    Jslb(String),
    Json(String),
    UnsupportedVersion(u32),
}

impl LoadError {
    fn into_tool_error(self, path: &Path) -> ToolError {
        let path = path.to_path_buf();
        match self {
            LoadError::UnsupportedVersion(v) => ToolError::UnsupportedProfileFormat {
                path,
                version: v.to_string(),
                supported: SUPPORTED_VERSIONS.to_owned(),
            },
            LoadError::Gzip(d) => ToolError::NotAProfile {
                path,
                details: format!("gzip: {d}"),
            },
            LoadError::Jslb(d) => ToolError::NotAProfile {
                path,
                details: format!("JSLB container: {d}"),
            },
            LoadError::Json(d) => ToolError::NotAProfile { path, details: d },
        }
    }
}

pub fn load_from_path(path: &Path) -> Result<RawProfile, ToolError> {
    let bytes = std::fs::read(path).map_err(|_| ToolError::FileNotFound {
        path: path.to_path_buf(),
    })?;
    decode_bytes(&bytes).map_err(|e| e.into_tool_error(path))
}

pub(crate) fn decode_bytes(bytes: &[u8]) -> Result<RawProfile, LoadError> {
    if bytes.starts_with(&GZIP_MAGIC) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes)
            .read_to_end(&mut out)
            .map_err(|e| LoadError::Gzip(e.to_string()))?;
        return decode_uncompressed(&out);
    }
    decode_uncompressed(bytes)
}

fn decode_uncompressed(bytes: &[u8]) -> Result<RawProfile, LoadError> {
    serde_json::from_slice(bytes).map_err(|e| {
        // Only the error path pays for a second parse. A file in an
        // unsupported version usually fails schema checks before the
        // version gate runs, so the user needs the version, not the
        // schema message.
        let version = serde_json::from_slice::<VersionPeek>(bytes)
            .ok()
            .and_then(|p| p.meta.preprocessed_profile_version);
        json_error(e, version)
    })
}

fn json_error(e: serde_json::Error, version: Option<u32>) -> LoadError {
    match version {
        Some(v) if !is_supported_version(v) => LoadError::UnsupportedVersion(v),
        _ => LoadError::Json(e.to_string()),
    }
}

#[derive(serde::Deserialize)]
struct VersionPeek {
    meta: VersionPeekMeta,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct VersionPeekMeta {
    #[serde(default)]
    preprocessed_profile_version: Option<u32>,
}
```

Remove the now unused `flate2::bufread::GzDecoder`, `File`, `BufReader`, and `OsStr` imports. The existing tests (`loads_uncompressed_json`, `loads_gzipped_json`, `missing_file_returns_file_not_found`) keep passing because detection no longer looks at the extension.

* [ ] **Step 7: Run the tests**

Run: `cargo test`
Expected: all pass.

* [ ] **Step 8: Format, lint, commit**

```bash
cargo fmt
cargo clippy --all-targets
git add src/profile/wire src/profile/load.rs src/error.rs
git commit -m "feat: decode version 75 shared-layout profiles

Gate decoding on preprocessedProfileVersion and report unsupported
versions with the supported set instead of a schema error."
```

---

### Task 5: JSLB container

**Files:**
* Create: `src/profile/jslb.rs`
* Modify: `Cargo.toml`, `src/profile/mod.rs`, `src/profile/load.rs`

**Interfaces:**
* Consumes: `load::decode_uncompressed`, `load::json_error` from Task 4.
* Produces: `pub(crate) fn jslb::to_value(bytes: &[u8]) -> Result<serde_json::Value, String>` and `pub(crate) const jslb::MAGIC: [u8; 8]`.

* [ ] **Step 1: Add the dependency**

Add `json-slabs = "0.2"` to `[dependencies]` in `Cargo.toml`, after `flate2`. Add `pub(crate) mod jslb;` to `src/profile/mod.rs`.

* [ ] **Step 2: Write the failing tests**

Create `src/profile/jslb.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use json_slabs::Builder;
    use serde_json::json;

    #[test]
    fn substitutes_every_numeric_slab_type() {
        let mut b = Builder::new();
        let i8s = b.add_slab_from_vec(vec![-1i8, 2]);
        let u8s = b.add_slab_from_vec(vec![255u8]);
        let i16s = b.add_slab_from_vec(vec![-300i16]);
        let u16s = b.add_slab_from_vec(vec![60000u16]);
        let i32s = b.add_slab_from_vec(vec![-70000i32]);
        let u32s = b.add_slab_from_vec(vec![4_000_000_000u32]);
        let f32s = b.add_slab_from_vec(vec![0.5f32]);
        let f64s = b.add_slab_from_vec(vec![1.25f64]);
        let i64s = b.add_slab_from_vec(vec![-5_000_000_000i64]);
        let u64s = b.add_slab_from_vec(vec![10_000_000_000u64]);
        let root = format!(
            r#"{{"a":{i8s:#},"b":{u8s:#},"c":{i16s:#},"d":{u16s:#},"e":{i32s:#},"f":{u32s:#},"g":{f32s:#},"h":{f64s:#},"i":{i64s:#},"j":{u64s:#}}}"#
        );
        let bytes = b.finish(root.as_bytes());
        let v = to_value(&bytes).unwrap();
        assert_eq!(
            v,
            json!({"a": [-1, 2], "b": [255], "c": [-300], "d": [60000], "e": [-70000],
                   "f": [4_000_000_000u64], "g": [0.5], "h": [1.25],
                   "i": [-5_000_000_000i64], "j": [10_000_000_000u64]})
        );
    }

    #[test]
    fn substitutes_nested_json_slabs() {
        let mut b = Builder::new();
        let inner_numbers = b.add_slab_from_vec(vec![1i32, 2, 3]);
        let inner = b.add_json_slab(format!(r#"{{"xs":{inner_numbers:#},"s":"hi"}}"#).into_bytes());
        let root = format!(r#"{{"threads":[{inner:#}],"keep":{{"$s":0,"other":1}}}}"#);
        let bytes = b.finish(root.as_bytes());
        let v = to_value(&bytes).unwrap();
        // An object with keys besides `$s` is not a placeholder.
        assert_eq!(
            v,
            json!({"threads": [{"xs": [1, 2, 3], "s": "hi"}], "keep": {"$s": 0, "other": 1}})
        );
    }

    #[test]
    fn out_of_range_placeholder_is_an_error() {
        let b = Builder::new();
        let bytes = b.finish(br#"{"x":{"$s":9}}"#);
        let err = to_value(&bytes).unwrap_err();
        assert!(err.contains("slab index 9 out of range"), "{err}");
    }
}
```

Add to the test module of `src/profile/load.rs`:

```rust
    #[test]
    fn gzipped_jslb_loads_by_magic() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        let mut b = json_slabs::Builder::new();
        let stacks = b.add_slab_from_vec(vec![0i32]);
        let root = format!(
            r#"{{"meta": {{"interval": 1.0, "startTime": 0.0}}, "threads": [{{"tid": 1, "pid": 1, "registerTime": 0.0,
                "stringArray": ["f"],
                "frameTable": {{"length": 1, "address": [-1], "func": [0], "line": [null], "column": [null], "category": [0], "subcategory": [0]}},
                "funcTable": {{"length": 1, "name": [0], "isJS": [false], "relevantForJS": [false], "resource": [-1], "fileName": [null], "lineNumber": [null], "columnNumber": [null]}},
                "stackTable": {{"length": 1, "frame": [0], "prefix": [null]}},
                "resourceTable": {{"length": 0, "lib": [], "name": [], "host": [], "type": []}},
                "samples": {{"length": 1, "stack": {stacks:#}, "time": [0.0]}}}}]}}"#
        );
        let jslb = b.finish(root.as_bytes());
        // A `.json` name must not matter: detection reads magic bytes.
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        let mut gz = GzEncoder::new(f.as_file_mut(), Compression::default());
        gz.write_all(&jslb).unwrap();
        gz.finish().unwrap();
        let p = load_from_path(f.path()).unwrap();
        assert_eq!(p.threads[0].samples.stack, vec![Some(0)]);
    }
```

* [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --lib jslb gzipped_jslb`
Expected: compile errors, `to_value` not found.

* [ ] **Step 4: Implement the reader**

Put this above the test module in `src/profile/jslb.rs`:

```rust
//! Decoder for the JSLB (JsonSlabs) container.
//!
//! A JSLB file holds a root JSON skeleton plus binary slabs, and the
//! skeleton refers to slabs with `{"$s": N}` placeholders. The
//! `json-slabs` crate parses the container but leaves substitution to
//! the consumer, so this module rebuilds the plain JSON value.

use json_slabs::{DecodeError, ParsedFile, SLAB_REF_KEY, SlabPlaceholder, SlabType};
use serde_json::{Map, Number, Value};

pub(crate) const MAGIC: [u8; 8] = json_slabs::MAGIC;

pub(crate) fn to_value(bytes: &[u8]) -> Result<Value, String> {
    let file = ParsedFile::parse(bytes).map_err(|e| e.to_string())?;
    let root: Value =
        serde_json::from_slice(file.root_json_bytes()).map_err(|e| format!("root slab: {e}"))?;
    substitute(&file, root)
}

fn substitute(file: &ParsedFile<'_>, value: Value) -> Result<Value, String> {
    match value {
        Value::Object(map) => {
            if let Some(idx) = placeholder_index(&map) {
                return slab_value(file, SlabPlaceholder(idx));
            }
            let mut out = Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k, substitute(file, v)?);
            }
            Ok(Value::Object(out))
        }
        Value::Array(items) => items
            .into_iter()
            .map(|v| substitute(file, v))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        other => Ok(other),
    }
}

/// A placeholder is an object whose only member is `$s`.
fn placeholder_index(map: &Map<String, Value>) -> Option<usize> {
    if map.len() != 1 {
        return None;
    }
    map.get(SLAB_REF_KEY)?
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
}

fn slab_value(file: &ParsedFile<'_>, p: SlabPlaceholder) -> Result<Value, String> {
    let err = |e: DecodeError| e.to_string();
    let slab_type = file.slab_at(p).map_err(err)?.slab_type;
    Ok(match slab_type {
        SlabType::Json => {
            let bytes = file.read_subjson_bytes(p).map_err(err)?;
            let v: Value = serde_json::from_slice(bytes)
                .map_err(|e| format!("slab {}: {e}", p.index()))?;
            return substitute(file, v);
        }
        SlabType::Int8 => ints(file.read::<i8>(p).map_err(err)?),
        SlabType::Uint8 => ints(file.read::<u8>(p).map_err(err)?),
        SlabType::Int16 => ints(file.read::<i16>(p).map_err(err)?),
        SlabType::Uint16 => ints(file.read::<u16>(p).map_err(err)?),
        SlabType::Int32 => ints(file.read::<i32>(p).map_err(err)?),
        SlabType::Uint32 => ints(file.read::<u32>(p).map_err(err)?),
        SlabType::Int64 => ints(file.read::<i64>(p).map_err(err)?),
        SlabType::Uint64 => ints(file.read::<u64>(p).map_err(err)?),
        SlabType::Float32 => floats(file.read::<f32>(p).map_err(err)?.into_iter().map(f64::from)),
        SlabType::Float64 => floats(file.read::<f64>(p).map_err(err)?.into_iter()),
    })
}

fn ints<T: Into<Number>>(values: Vec<T>) -> Value {
    Value::Array(values.into_iter().map(|v| Value::Number(v.into())).collect())
}

/// Non-finite floats have no JSON form and become `null`.
fn floats(values: impl Iterator<Item = f64>) -> Value {
    Value::Array(
        values
            .map(|v| Number::from_f64(v).map_or(Value::Null, Value::Number))
            .collect(),
    )
}
```

In `src/profile/load.rs`, add at the start of `decode_uncompressed`:

```rust
    if bytes.starts_with(&crate::profile::jslb::MAGIC) {
        let value = crate::profile::jslb::to_value(bytes).map_err(LoadError::Jslb)?;
        let version = value
            .pointer("/meta/preprocessedProfileVersion")
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok());
        return serde_json::from_value(value).map_err(|e| json_error(e, version));
    }
```

Update the module doc comment to add: "Only the JSLB path builds a `serde_json::Value`, because its skeleton is small and the columns live in binary slabs."

* [ ] **Step 5: Run the tests**

Run: `cargo test`
Expected: all pass.

* [ ] **Step 6: Format, lint, commit**

```bash
cargo fmt
cargo clippy --all-targets
git add Cargo.toml Cargo.lock src/profile/jslb.rs src/profile/mod.rs src/profile/load.rs
git commit -m "feat: load JSLB profile containers"
```

---

### Task 6: Real-recording fixtures and equivalence tests

**Files:**
* Create: `tests/fixtures/perf/regenerate.sh`, `tests/fixtures/perf/multi_v49.json.gz`, `tests/fixtures/perf/multi_v75.json.gz`, `tests/fixtures/perf/multi_v75.jslb.gz`, `tests/perf_formats.rs`

**Interfaces:**
* Consumes: `pollard::profile::load_from_path`, `pollard::profile::symbolicate::symbolicate`, `pollard::profile::Profile`, `pollard::query::top_functions::{Args, top_functions}`, `pollard::query::call_tree::{Args, call_tree}`, `pollard::query::event::EventSource`.
* Produces: three checked-in fixtures from one recording.

* [ ] **Step 1: Write the generator**

Create `tests/fixtures/perf/regenerate.sh` and make it executable (`chmod +x`):

```bash
#!/bin/bash
# Regenerate the multi-event perf fixtures from one recording.
#
# Needs perf with hardware counters, samply 0.13.1 (emits version 49),
# and a samply build from main (emits version 75):
#   SAMPLY_V49=/path/to/samply-0.13.1 SAMPLY_V75=/path/to/samply-main ./regenerate.sh
set -euo pipefail
cd "$(dirname "$0")"
: "${SAMPLY_V49:?set SAMPLY_V49 to a samply 0.13.1 binary}"
: "${SAMPLY_V75:?set SAMPLY_V75 to a samply main binary}"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

perf record -q -o "$work/multi.data" \
  -e cycles,cache-misses,instructions,branch-misses -F 999 -g \
  -- sh -c 'i=0; while [ $i -lt 300000 ]; do i=$((i+1)); done'

"$SAMPLY_V49" import "$work/multi.data" -s -o "$work/multi_v49.json.gz"
"$SAMPLY_V75" import "$work/multi.data" -s -o "$work/multi_v75.json.gz"
"$SAMPLY_V75" import "$work/multi.data" -s -o "$work/multi_v75.jslb.gz"

# Replace host-identifying strings with same-length placeholders, so
# JSLB slab offsets stay valid.
host=$(uname -n)
release=$(uname -r)
for f in multi_v49.json.gz multi_v75.json.gz multi_v75.jslb.gz; do
  python3 - "$work/$f" "$f" "$host" "$release" <<'EOF'
import gzip, sys
src, dst, host, release = sys.argv[1:]
data = gzip.open(src).read()
for secret in (host, release):
    data = data.replace(secret.encode(), b"x" * len(secret.encode()))
assert host.encode() not in data and release.encode() not in data
with gzip.open(dst, "wb", mtime=0) as out:
    out.write(data)
EOF
done
```

* [ ] **Step 2: Generate and inspect the fixtures**

Run with the two samply binaries (samply main at `~/dev/repos/samply` built with `cargo build -p samply`):

```bash
SAMPLY_V49=$HOME/.cargo/bin/samply \
SAMPLY_V75=$HOME/dev/repos/samply/target/debug/samply \
  tests/fixtures/perf/regenerate.sh
ls -la tests/fixtures/perf/
zcat tests/fixtures/perf/multi_v49.json.gz | grep -o '"product":"[^"]*"'
zcat tests/fixtures/perf/multi_v75.json.gz | grep -c "$HOME" || true
```

Expected: three files under 50 KB each. `product` shows `x` characters where the host name was. The `$HOME` count is 0; if it isn't, add `$HOME` to the scrub list in the script and rerun.

* [ ] **Step 3: Write the equivalence tests**

Create `tests/perf_formats.rs`:

```rust
//! One perf.data recording imported three ways must analyze the same.
//!
//! `multi_v75.json.gz` and `multi_v75.jslb.gz` come from the same
//! samply build, so every query result must match exactly.
//! `multi_v49.json.gz` comes from samply 0.13.1, which emits different
//! libraries and categories, so only per-event totals are compared.

use std::path::Path;

use pollard::profile::symbolicate::symbolicate;
use pollard::profile::{Profile, load_from_path};
use pollard::query::call_tree::{self, call_tree};
use pollard::query::event::EventSource;
use pollard::query::top_functions::{self, top_functions};

const EVENTS: [&str; 3] = ["cache-misses", "instructions", "branch-misses"];

async fn load(name: &str) -> Profile {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/perf")
        .join(name);
    let mut raw = load_from_path(&path).unwrap();
    symbolicate(&mut raw).await.unwrap();
    Profile::from_raw(raw)
}

fn event_sources() -> Vec<EventSource> {
    std::iter::once(EventSource::Samples)
        .chain(EVENTS.iter().map(|e| EventSource::Marker((*e).into())))
        .collect()
}

fn top(p: &Profile, event: EventSource) -> serde_json::Value {
    let out = top_functions(
        p,
        &top_functions::Args {
            event,
            limit: 50,
            ..Default::default()
        },
    )
    .unwrap();
    serde_json::to_value(&out.functions).unwrap()
}

fn total(p: &Profile, event: EventSource) -> u64 {
    top_functions(
        p,
        &top_functions::Args {
            event,
            ..Default::default()
        },
    )
    .unwrap()
    .total_samples
}

#[tokio::test]
async fn json_and_jslb_from_one_build_match() {
    let json = load("multi_v75.json.gz").await;
    let jslb = load("multi_v75.jslb.gz").await;
    for ev in event_sources() {
        assert_eq!(top(&json, ev.clone()), top(&jslb, ev.clone()), "top_functions {ev:?}");
    }
    let tree = |p: &Profile| {
        serde_json::to_value(call_tree(p, &call_tree::Args::default()).unwrap()).unwrap()
    };
    assert_eq!(tree(&json), tree(&jslb));
}

#[tokio::test]
async fn versions_49_and_75_agree_on_event_totals() {
    let v49 = load("multi_v49.json.gz").await;
    let v75 = load("multi_v75.json.gz").await;
    for ev in event_sources() {
        let (a, b) = (total(&v49, ev.clone()), total(&v75, ev.clone()));
        assert!(a > 0, "no {ev:?} in the version 49 fixture");
        assert_eq!(a, b, "total for {ev:?}");
    }
}
```

Before running, check `call_tree::Output` and the `top_functions` output rows derive `Serialize` and contain no field that differs between two loads of the same data, such as a profile ID. If `call_tree::Output` carries such a field, compare its `root` or node field instead and note which field in the test.

* [ ] **Step 4: Run the tests**

Run: `cargo test --test perf_formats`
Expected: both tests pass. If `versions_49_and_75_agree_on_event_totals` fails, print both totals per event, check them against `perf script -i <data> -F event | sort | uniq -c` from the recording, and report instead of loosening the assertion.

* [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt
cargo clippy --all-targets
git add tests/fixtures/perf tests/perf_formats.rs
git commit -m "test: compare one perf recording across profile formats"
```

---

### Task 7: Event discovery

**Files:**
* Modify: `src/query/event.rs`, `src/query/describe.rs`, `src/query/summary.rs`, snapshot files under `tests/snapshots/` if `describe` or `summary` are snapshotted.

**Interfaces:**
* Consumes: `Profile::shared()`, `RawMarkerData::type_`.
* Produces: `pub struct EventInfo { name: String, source: &'static str, count: u64, stackless: Option<bool> }` and `pub fn list_events(profile: &Profile) -> Vec<EventInfo>` in `src/query/event.rs`, plus `events: Vec<EventInfo>` on `ProfileDescription` and `summary::Output`.

* [ ] **Step 1: Write the failing test**

Add to the test module of `src/query/event.rs` (it already loads `two_events.json` in `fixture()`):

```rust
    #[test]
    fn list_events_reports_samples_and_other_event_markers() {
        let p = fixture();
        let events = list_events(&p);
        assert_eq!(events[0].name, "samples");
        assert_eq!(events[0].source, "samples");
        assert_eq!(events[0].stackless, None);
        let cm = events.iter().find(|e| e.name == "cache-misses").unwrap();
        assert_eq!(cm.source, "marker");
        assert_eq!(cm.count, 2);
        assert_eq!(cm.stackless, Some(false));
        // `mmap` markers are bookkeeping, not events.
        assert!(events.iter().all(|e| e.name != "mmap"));
    }
```

Run: `cargo test --lib list_events_reports`
Expected: FAIL to compile, `list_events` not found.

* [ ] **Step 2: Implement `list_events`**

Add to `src/query/event.rs`:

```rust
/// Marker schema samply uses for secondary perf events.
const OTHER_EVENT_TYPE: &str = "Other event";

/// One event a profile can aggregate by.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EventInfo {
    /// `"samples"` for the samples track, else the marker name, e.g.
    /// `"cache-misses"`. The samples track's real event name (such as
    /// cycles) is not recorded in the profile.
    pub name: String,
    /// `"samples"` or `"marker"`.
    pub source: &'static str,
    /// Samples or markers across all threads.
    pub count: u64,
    /// For markers: true when no marker of this name carries a stack,
    /// so it cannot be aggregated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stackless: Option<bool>,
}

/// The samples track first, then `Other event` markers by name.
pub fn list_events(profile: &Profile) -> Vec<EventInfo> {
    let mut samples = 0u64;
    let mut markers: std::collections::BTreeMap<usize, (u64, bool)> = Default::default();
    for thread in profile.threads() {
        let raw = thread.raw();
        samples += raw.samples.length as u64;
        for (i, &str_idx) in raw.markers.name.iter().enumerate() {
            let Some(data) = raw.markers.data.get(i).and_then(|d| d.as_ref()) else {
                continue;
            };
            if data.type_.as_deref() != Some(OTHER_EVENT_TYPE) {
                continue;
            }
            let entry = markers.entry(str_idx).or_default();
            entry.0 += 1;
            entry.1 |= data.cause.is_some();
        }
    }
    let strings = &profile.shared().strings;
    let mut marker_events: Vec<EventInfo> = markers
        .into_iter()
        .filter_map(|(idx, (count, has_stack))| {
            Some(EventInfo {
                name: strings.get(idx)?.to_owned(),
                source: "marker",
                count,
                stackless: Some(!has_stack),
            })
        })
        .collect();
    marker_events.sort_by(|a, b| a.name.cmp(&b.name));
    let mut events = vec![EventInfo {
        name: "samples".to_owned(),
        source: "samples",
        count: samples,
        stackless: None,
    }];
    events.extend(marker_events);
    events
}
```

Add `use schemars::JsonSchema;` and `use serde::Serialize;` to the imports.

Then make `known_marker_events` return the stack-bearing marker names from `list_events`:

```rust
fn known_marker_events(profile: &Profile) -> Vec<String> {
    list_events(profile)
        .into_iter()
        .filter(|e| e.stackless == Some(false))
        .map(|e| e.name)
        .collect()
}
```

Keep the existing doc comment on `known_marker_events`, and change "distinct marker names that have at least one stack-bearing entry" to "distinct `Other event` marker names that have at least one stack-bearing entry".

* [ ] **Step 3: Add `events` to describe and summary**

In `src/query/describe.rs`, add after `unsymbolicated_pct` in `ProfileDescription`:

```rust
    /// Events the profile can aggregate by: the samples track, then
    /// each perf event samply stored as markers. Pass a marker name as
    /// `event` to `top_functions`, `call_tree`, or `compare_profiles`.
    pub events: Vec<crate::query::event::EventInfo>,
```

and set `events: crate::query::event::list_events(profile),` where `describe` builds the struct. Do the same in `summary::Output` and `summary()`, with the doc comment "See [`crate::query::describe::ProfileDescription::events`]."

* [ ] **Step 4: Run the tests and review snapshots**

Run: `cargo test`
Expected: failures only in insta snapshots that include `describe` or `summary` output. Run `cargo insta review` (or `cargo insta accept` after reading each diff) and confirm every diff adds only an `events` array.

* [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt
cargo clippy --all-targets
git add -u src tests
git commit -m "feat: list available events in describe_profile and summary"
```

---

### Task 8: Documentation

**Files:**
* Modify: `skills/profile-recording/SKILL.md`, `skills/pollard-doctor/SKILL.md`, `README.md`, `CHANGELOG.md`

* [ ] **Step 1: Update the recording skill**

In `skills/profile-recording/SKILL.md`, next to the existing `samply import perf.data` instructions, add a section with this content, adapted to the file's heading and list style:

```markdown
## Recording other perf events

`samply record` samples cycles only. For other events, record with `perf` and import:

    perf record -e cycles,cache-misses,instructions -g -- <cmd>
    samply import perf.data --save-only -o /tmp/profile.json.gz

The first `-e` event becomes the samples track, which pollard queries by default.
Every other event becomes markers named after it; pass that name as `event`, e.g. `event="cache-misses"`.
`describe_profile` lists the events a profile contains.

Use a fixed period (`-c N`) instead of a frequency (`-F`) when comparing counts across events.
In frequency mode perf varies the period per sample, and pollard counts samples without weighting them by period.

samply saves `.jslb.gz` by default; pollard loads those too.
```

* [ ] **Step 2: Update the doctor skill**

In `skills/pollard-doctor/SKILL.md`, add an entry next to the other load errors:

```markdown
* `unsupported_profile_format`: the profile's `preprocessedProfileVersion` is one pollard cannot read.
  The error lists the supported versions.
  Upgrade pollard, or re-import the recording with a samply version that emits a supported format.
```

* [ ] **Step 3: Update README and CHANGELOG**

In `README.md`, where it describes accepted input, state that pollard reads Firefox processed profiles as `.json`, `.json.gz`, `.jslb`, or `.jslb.gz`, in format versions 49 through 55 and 75. In the `event` argument section, mention that `describe_profile` lists the events.

Add to the unreleased section of `CHANGELOG.md`, following its existing heading style:

```markdown
### Added

* Load samply's version 75 profiles, including the JSLB container (`.jslb`, `.jslb.gz`).
* `describe_profile` and `summary` list the events a profile contains.

### Fixed

* Frames in sub-process threads resolve their module from the sub-process's libraries.
```

* [ ] **Step 4: Commit**

```bash
git add skills README.md CHANGELOG.md
git commit -m "docs: document perf event recording and new profile formats"
```

---

