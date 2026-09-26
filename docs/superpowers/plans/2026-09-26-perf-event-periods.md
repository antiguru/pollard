# Perf event periods implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`* [ ]`) syntax for tracking.

**Goal:** samply records each perf sample's period and names the perf events in `meta.extra`, and pollard weights its rankings by those periods when a profile carries them.

**Architecture:** samply pull request 1 adds `Profile::add_extra_info_section` to fxprof-processed-profile, a `Perf events` section and a marker `period` field to `samply import`, and an opt-in `--weight-by-period` flag whose weight logic lives in pure functions.
samply pull request 2, independent of the first, stops writing non-clock periods into `threadCPUDelta`.
pollard parses the `Perf events` section once per `Profile`, adds `Profile::weighted_stack_indices` next to the existing `stack_indices`, and moves every ranking tool onto weights, reporting `weighted` in each output.

**Tech Stack:** samply: Rust 2021 (rust-version 1.89), linux-perf-data 0.13, linux-perf-event-reader 0.10.2, fxprof-processed-profile 0.8.1, clap, insta. pollard: Rust 2024 (rust-version 1.95), serde, serde_json, schemars, tokio, insta.

**Spec:** `docs/superpowers/specs/2026-09-26-perf-event-periods-design.md` (in the pollard worktree `/home/moritz/dev/repos/pollard/.claude/worktrees/period-weighting`)

## Global constraints

* `Perf events` value grammar, verbatim from the spec, `N` a decimal integer: `event value = "frequency " N " Hz" | "period " N`, `weight value = "period" | "1"`.
* Section label `Perf events`, weight entry label `Sample weight`, marker field key `period`, CLI flag `--weight-by-period`, props field `weight_by_period: bool`.
* samply pull request 1 lives on branch `import-period` in `/home/moritz/dev/repos/samply/.claude/worktrees/import-period`, pull request 2 on branch `cpu-delta-clock-events` in `/home/moritz/dev/repos/samply/.claude/worktrees/cpu-delta-clock-events`. Both start at `upstream/main` (`da48ff40`) and neither depends on the other.
* pollard lives on branch `period-weighting` in `/home/moritz/dev/repos/pollard/.claude/worktrees/period-weighting`, stacked on `shared-tables-format`. It must not depend on samply being merged. Every pollard test uses checked-in fixtures.
* samply commands: `cargo fmt --all` (CI runs `cargo fmt --all --check`), `cargo clippy --all-targets`, `cargo test -p samply`, `cargo test -p fxprof-processed-profile`.
* pollard commands: `cargo fmt`, `cargo clippy --all-targets`, `cargo test`. No new warnings in either repo.
* samply commit subjects are short imperative sentences ending with a period, no Conventional Commits prefix. pollard commit subjects use Conventional Commits (`feat:`, `test:`, `docs:`).
* Every commit message ends with a blank line and exactly these two lines:
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`
  `Claude-Session: https://claude.ai/code/session_016aEc8uf7GZbHQHXmoX8o2k`
* Stage files by path. Never `git add -A`, never `git reset --hard`, never `git push --force` (use `--force-with-lease`). Do not push or open pull requests; the controller does that.
* Keep existing comments when moving code. New comments have no em-dashes and no structuring semicolons. A doc comment states the contract; reasoning goes inline at the decision point.
* No new `unsafe`. No `as` casts where a lossless conversion exists (use `u64::from`, `i32::try_from`, and so on). `u64 as f64` and `f64 as u64` are allowed because no lossless conversion exists.
* Repo markdown: `*` for new lists, headers capitalize only the first word and the word after a colon.
* Scratch files (scripts, imported profiles, baseline binaries) go in `S=/tmp/claude-1000/-home-moritz-dev-repos-samply/c426bfa0-d211-4428-9e82-e5b3db072f8b/scratchpad`, never in a repo.

## Review focus

* A `-c` recording, whose sample records carry no `PERIOD`: marker periods and flagged sample weights must come from the attribute's fixed period, not fall to 0 or 1 (Task 2 test `marker_period_prefers_the_record`, Task 3 test `sample_weight_falls_back_to_fixed_period`, Task 5 check on `multi_c.data`).
* A `-F cycles` recording with context switches imported with `--weight-by-period`: off-CPU samples must weigh 0 while on-CPU weights still sum to `perf script`'s periods (Task 3 test `off_cpu_samples_weigh_zero_with_period_weights`, Task 5 check on `swcs.data`).
* A profile whose `meta.extra` or a marker's `period` has an unexpected shape, for example a non-array `extra` or a string `period`: pollard must still load it and treat the value as absent (Task 9 tests `malformed_meta_extra_loads_empty` and `non_numeric_marker_period_is_ignored`).
* A `time_range` filter that selects none of an event's period-carrying markers: `weighted` must be false for that event, because it describes the selected markers (Task 10 test `marker_weighting_follows_the_time_range`).
* Comparing a period-weighted profile against an older unweighted one: `compare_profiles` must flag the mismatch in `note` and keep `*_ms` columns on sample counts (Task 11 tests `mixed_weighting_adds_note` and `weighted_profile_shares_follow_periods_and_ms_follows_counts`).

---

## File structure

samply pull request 1 (`import-period`):

* Create `fxprof-processed-profile/src/extra_info.rs`: `ExtraInfoEntry` (public) and `ExtraInfoSection` (crate-private), with their JSON writers.
* Modify `fxprof-processed-profile/src/lib.rs`, `fxprof-processed-profile/src/profile.rs`: register and export the module, store sections, write `meta.extra`.
* Modify `fxprof-processed-profile/tests/integration_tests/main.rs`: `meta.extra` tests.
* Modify `samply/src/linux_shared/event_interpretation.rs`: `fixed_periods` field, pure functions `fixed_periods`, `sampling_description`, `perf_events_section`, and unit tests.
* Modify `samply/src/linux_shared/mod.rs`: re-export `perf_events_section`.
* Modify `samply/src/shared/process_sample_data.rs`: `OtherEventMarker` gains `period`.
* Modify `samply/src/linux_shared/converter.rs`: marker period, sample weight, off-CPU weight, saturation warning, `add_extra_info_section`, pure functions `marker_period`, `sample_weight`, `off_cpu_sampling`, and the file's first unit tests.
* Modify `samply/src/shared/prop_types.rs`, `samply/src/cli.rs`: `weight_by_period` prop and `--weight-by-period` flag.
* Modify `samply/src/linux/profiler.rs`: hand-built `EventInterpretation` sets `fixed_periods: vec![None]`.
* Modify `samply/src/import/perf.rs`: write the `Perf events` section.
* Modify `README.md`: document `--weight-by-period`.

samply pull request 2 (`cpu-delta-clock-events`):

* Modify `samply/src/linux_shared/event_interpretation.rs`: `main_event_is_clock` field, pure function `is_clock_event`, unit test.
* Modify `samply/src/linux_shared/converter.rs`: period becomes CPU delta only for clock events.
* Modify `samply/src/linux/profiler.rs`: hand-built `EventInterpretation` sets `main_event_is_clock: false`.

pollard (`period-weighting`):

* Create `src/profile/perf_events.rs`: `PerfEvents`, `PerfEvent`, `Sampling`, section parsing, and the pure weight functions `event_count`, `sample_weight`, `marker_weight`.
* Modify `src/profile/raw.rs`: `RawMeta::extra`, `RawExtraSection`, `RawExtraEntry`, `RawMarkerData::period`, the `lenient` deserializer.
* Modify `src/profile/mod.rs`, `src/profile/parsed.rs`: register the module, parse once per `Profile`, `weighted_stack_indices`, `is_weighted`, `sample_weight`.
* Create `tests/fixtures/weighted_events.json`: equal sample and marker counts per function, different periods, one fixed-period event.
* Modify `src/query/top_functions.rs`, `src/query/top_groups.rs`, `src/query/compare.rs`: `Tally`/`Counts`, weighted aggregation, `weighted` outputs, compare note.
* Modify `src/query/call_tree.rs`, `src/query/stacks_containing.rs`, `src/query/folded.rs`, `src/tools/query.rs`: weighted accumulation and `weighted`.
* Modify `src/query/source.rs`, `src/query/asm.rs`, `src/query/compare_functions.rs`: per-sample weights and `weighted`.
* Modify `src/query/summary.rs`: weighted `top_modules`, `weighted` output.
* Modify `src/query/event.rs`: `event` and `sampling` in `list_events`.
* Modify `tests/snapshots/snapshot__top_functions_snapshot.snap`, `tests/snapshots/snapshot__summary_snapshot.snap` through insta only.
* Modify `tests/fixtures/perf/regenerate.sh`, add `tests/fixtures/perf/multi_period.json.gz`, modify `tests/perf_formats.rs` (optional Task 16).
* Modify `skills/profile-recording/SKILL.md`, `README.md`, `CHANGELOG.md`.

---

### Task 1: fxprof-processed-profile writes `meta.extra`

Repository: samply, worktree `/home/moritz/dev/repos/samply/.claude/worktrees/import-period`, branch `import-period`.

**Files:**
* Create: `fxprof-processed-profile/src/extra_info.rs`
* Modify: `fxprof-processed-profile/src/lib.rs:59-116` (module list and re-exports)
* Modify: `fxprof-processed-profile/src/profile.rs:211-232` (struct), `:248-269` (`Profile::new`), `:295-298` (after `set_os_name`), `:1580-1600` (`write_meta_json`)
* Test: `fxprof-processed-profile/tests/integration_tests/main.rs`

**Interfaces:**
* Consumes: `crate::writer::Writer` (`object`, `array`, `name`, `string_value`).
* Produces:
  * `pub struct ExtraInfoEntry` with `pub fn string(label: &str, value: &str) -> Self`, re-exported as `fxprof_processed_profile::ExtraInfoEntry`.
  * `pub fn Profile::add_extra_info_section(&mut self, label: &str, entries: Vec<ExtraInfoEntry>)`.
  * JSON: `meta.extra = [{"label", "entries": [{"label", "format": "string", "value"}]}]`, absent when no section was added.

* [ ] **Step 1: Build the baseline binary for later comparisons**

Run before touching any file, so Task 5 and Task 8 can diff against unmodified upstream output:

```bash
cd /home/moritz/dev/repos/samply/.claude/worktrees/import-period
git log -1 --format=%h   # expect da48ff40
git status --short        # expect no output
cargo build -p samply
cp target/debug/samply /tmp/claude-1000/-home-moritz-dev-repos-samply/c426bfa0-d211-4428-9e82-e5b3db072f8b/scratchpad/samply-base
```

* [ ] **Step 2: Write the failing tests**

Append to `fxprof-processed-profile/tests/integration_tests/main.rs`:

```rust
#[test]
fn extra_info_sections_are_written_to_meta_extra() {
    let mut profile = Profile::new(
        "test",
        ReferenceTimestamp::from_millis_since_unix_epoch(0.0),
        SamplingInterval::from_millis(1),
    );
    profile.add_extra_info_section(
        "Perf events",
        vec![
            ExtraInfoEntry::string("cycles", "frequency 999 Hz"),
            ExtraInfoEntry::string("Sample weight", "1"),
        ],
    );
    let json = profile_as_json_value(&profile);
    assert_eq!(
        json["meta"]["extra"],
        serde_json::json!([{
            "label": "Perf events",
            "entries": [
                {"label": "cycles", "format": "string", "value": "frequency 999 Hz"},
                {"label": "Sample weight", "format": "string", "value": "1"},
            ],
        }])
    );
}

#[test]
fn meta_extra_is_absent_without_sections() {
    let profile = Profile::new(
        "test",
        ReferenceTimestamp::from_millis_since_unix_epoch(0.0),
        SamplingInterval::from_millis(1),
    );
    assert!(profile_as_json_value(&profile)["meta"].get("extra").is_none());
}
```

Add `ExtraInfoEntry` to the `use fxprof_processed_profile::{...}` list at the top of the file.

* [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p fxprof-processed-profile --test integration_tests extra_info`
Expected: compile error, `unresolved import fxprof_processed_profile::ExtraInfoEntry`.

* [ ] **Step 4: Create the module**

Create `fxprof-processed-profile/src/extra_info.rs`:

```rust
use std::io::Write;

use crate::writer::Writer;

/// One labeled value in a section of the profile's `meta.extra`, see
/// [`Profile::add_extra_info_section`](crate::Profile::add_extra_info_section).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraInfoEntry {
    label: String,
    value: String,
}

impl ExtraInfoEntry {
    /// An entry whose value the Firefox Profiler shows as plain text.
    pub fn string(label: &str, value: &str) -> Self {
        Self {
            label: label.to_string(),
            value: value.to_string(),
        }
    }

    fn write_json<W: Write>(&self, w: &mut Writer<W>) -> std::io::Result<()> {
        w.object(|w| {
            w.name("label")?;
            w.string_value(&self.label)?;
            // The Firefox Profiler formats these values without a string
            // table, so string-index formats such as `unique-string` do not
            // work here. `string` carries the value itself.
            w.name("format")?;
            w.string_value("string")?;
            w.name("value")?;
            w.string_value(&self.value)
        })
    }
}

/// A labeled group of entries in `meta.extra`.
#[derive(Debug, Clone)]
pub(crate) struct ExtraInfoSection {
    pub(crate) label: String,
    pub(crate) entries: Vec<ExtraInfoEntry>,
}

impl ExtraInfoSection {
    pub(crate) fn write_json<W: Write>(&self, w: &mut Writer<W>) -> std::io::Result<()> {
        w.object(|w| {
            w.name("label")?;
            w.string_value(&self.label)?;
            w.name("entries")?;
            w.array(|w| {
                for entry in &self.entries {
                    entry.write_json(w)?;
                }
                Ok(())
            })
        })
    }
}
```

In `fxprof-processed-profile/src/lib.rs`, add `mod extra_info;` between `mod cpu_delta;` and `mod fast_hash_map;`, and `pub use extra_info::ExtraInfoEntry;` after `pub use cpu_delta::CpuDelta;`.

* [ ] **Step 5: Store and write sections in `Profile`**

In `fxprof-processed-profile/src/profile.rs`:

Add `use crate::extra_info::{ExtraInfoEntry, ExtraInfoSection};` to the `use crate::...` block (after `use crate::cpu_delta::CpuDelta;`).

Add a field at the end of `pub struct Profile` (after `used_tids`):

```rust
    extra_info_sections: Vec<ExtraInfoSection>,
```

Initialize it in `Profile::new` after `counters: Vec::new(),`:

```rust
            extra_info_sections: Vec::new(),
```

Add the method right after `set_os_name`:

```rust
    /// Add a labeled section to the profile's `meta.extra`, which the Firefox
    /// Profiler shows in its profile info panel. Sections are written in the
    /// order they were added.
    pub fn add_extra_info_section(&mut self, label: &str, entries: Vec<ExtraInfoEntry>) {
        self.extra_info_sections.push(ExtraInfoSection {
            label: label.to_string(),
            entries,
        });
    }
```

In `write_meta_json`, after the `initialSelectedThreads` block and before the closing `Ok(())`:

```rust
            if !self.extra_info_sections.is_empty() {
                w.name("extra")?;
                w.array(|w| {
                    for section in &self.extra_info_sections {
                        section.write_json(w)?;
                    }
                    Ok(())
                })?;
            }
```

* [ ] **Step 6: Run the tests**

Run: `cargo test -p fxprof-processed-profile`
Expected: all pass, including the two new tests. The existing insta snapshots are unchanged because they add no section.

* [ ] **Step 7: Format, lint, commit**

```bash
cd /home/moritz/dev/repos/samply/.claude/worktrees/import-period
cargo fmt --all
cargo clippy --all-targets
git add fxprof-processed-profile/src/extra_info.rs fxprof-processed-profile/src/lib.rs fxprof-processed-profile/src/profile.rs fxprof-processed-profile/tests/integration_tests/main.rs
git commit -m "$(cat <<'EOF'
Add Profile::add_extra_info_section for meta.extra.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_016aEc8uf7GZbHQHXmoX8o2k
EOF
)"
```

---

### Task 2: Fixed periods and the marker `period` field

Repository: samply, worktree `/home/moritz/dev/repos/samply/.claude/worktrees/import-period`.

**Files:**
* Modify: `samply/src/linux_shared/event_interpretation.rs:25-35` (struct), `:93-113` (end of `divine_from_attrs`), append functions and tests
* Modify: `samply/src/shared/process_sample_data.rs:175-193` (`OtherEventMarker`)
* Modify: `samply/src/linux_shared/converter.rs:62-113` (struct), `:196-222` (`Self { .. }` in `new`), `:578-590` (`handle_other_event_sample`), append `marker_period` and a test module at the end of the file
* Modify: `samply/src/linux/profiler.rs:337-345`

**Interfaces:**
* Consumes: `linux_perf_event_reader::SamplingPolicy` (`Period(NonZeroU64)`, `Frequency(u64)`, `NoSampling`).
* Produces:
  * `EventInterpretation::fixed_periods: Vec<Option<u64>>`, one per attribute.
  * `pub fn fixed_periods(policies: impl IntoIterator<Item = SamplingPolicy>) -> Vec<Option<u64>>` in `event_interpretation.rs`.
  * `pub struct OtherEventMarker { pub name: StringHandle, pub period: u64 }`, marker schema field `period` (integer).
  * `Converter::fixed_periods: Vec<Option<u64>>` (private field, Task 3 reads it).
  * `fn marker_period(record_period: Option<u64>, fixed_period: Option<u64>) -> u64` in `converter.rs`.

* [ ] **Step 1: Write the failing tests**

Append to `samply/src/linux_shared/event_interpretation.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::*;

    #[test]
    fn fixed_periods_come_from_period_attributes_only() {
        let policies = [
            SamplingPolicy::Frequency(999),
            SamplingPolicy::Period(NonZeroU64::new(10_000).unwrap()),
            SamplingPolicy::NoSampling,
        ];
        assert_eq!(fixed_periods(policies), vec![None, Some(10_000), None]);
    }
}
```

Append to the end of `samply/src/linux_shared/converter.rs` (after the `MmapMarker` impl):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_period_prefers_the_record() {
        assert_eq!(marker_period(Some(7), Some(10_000)), 7);
        assert_eq!(marker_period(None, Some(10_000)), 10_000);
        assert_eq!(marker_period(None, None), 0);
    }
}
```

* [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p samply fixed_periods marker_period`
Expected: compile errors, `cannot find function fixed_periods` and `cannot find function marker_period`.

* [ ] **Step 3: Derive fixed periods**

In `event_interpretation.rs`, add the field after `event_names` in `pub struct EventInterpretation`:

```rust
    /// The fixed sampling period of each attribute, in attribute order.
    /// `None` for frequency-based and non-sampling attributes.
    pub fixed_periods: Vec<Option<u64>>,
```

In `divine_from_attrs`, after the `event_names` binding:

```rust
        let fixed_periods = fixed_periods(attrs.iter().map(|attr_desc| attr_desc.attr.sampling_policy));
```

and add `fixed_periods,` after `event_names,` in the returned `Self { .. }`.

Add after the `impl EventInterpretation` block:

```rust
/// The fixed period of each attribute that samples every N events, in
/// order. Frequency-based attributes get `None`, because each of their
/// records carries its own period.
pub fn fixed_periods(policies: impl IntoIterator<Item = SamplingPolicy>) -> Vec<Option<u64>> {
    policies
        .into_iter()
        .map(|policy| match policy {
            SamplingPolicy::Period(period) => Some(period.get()),
            SamplingPolicy::Frequency(_) | SamplingPolicy::NoSampling => None,
        })
        .collect()
}
```

In `samply/src/linux/profiler.rs`, add `fixed_periods: vec![None],` after `event_names: vec!["cycles".to_string()],` in the hand-built `EventInterpretation`.

* [ ] **Step 4: Give `OtherEventMarker` a period**

Replace `OtherEventMarker` in `samply/src/shared/process_sample_data.rs` (the struct and its `impl Marker`) with:

```rust
#[derive(Debug, Clone)]
pub struct OtherEventMarker {
    pub name: StringHandle,
    /// Events this record stands for: the record's period, or the
    /// attribute's fixed period.
    pub period: u64,
}

impl Marker for OtherEventMarker {
    type FieldsType = f64;

    const UNIQUE_MARKER_TYPE_NAME: &'static str = "Other event";

    const DESCRIPTION: Option<&'static str> =
        Some("Emitted for any records in a perf.data file which don't map to a known event.");

    const FIELDS: Schema<Self::FieldsType> = Schema(MarkerField::integer("period", "Period"));

    fn name(&self, _profile: &mut Profile) -> StringHandle {
        self.name
    }

    fn field_values(&self) -> f64 {
        // Marker number fields are f64, which holds periods exactly up to 2^53.
        self.period as f64
    }
}
```

* [ ] **Step 5: Write the period in the converter**

In `samply/src/linux_shared/converter.rs`, add a field after `event_names: Vec<String>,` in `pub struct Converter`:

```rust
    /// Fixed sampling period per attribute, for records without `PERIOD`.
    fixed_periods: Vec<Option<u64>>,
```

In `Converter::new`, add `fixed_periods: interpretation.fixed_periods,` after `event_names: interpretation.event_names,`.

In `handle_other_event_sample`, replace

```rust
            let marker_handle =
                self.profile
                    .add_marker(thread_handle, timing, OtherEventMarker(name));
```

with

```rust
            let period = marker_period(
                e.period,
                self.fixed_periods.get(attr_index).copied().flatten(),
            );
            let marker_handle =
                self.profile
                    .add_marker(thread_handle, timing, OtherEventMarker { name, period });
```

Add above `fn process_off_cpu_sample_group`:

```rust
/// Period of a non-main event record: the record's own period, else the
/// attribute's fixed period, else 0.
fn marker_period(record_period: Option<u64>, fixed_period: Option<u64>) -> u64 {
    // perf requests `PERIOD` for every frequency-based attribute, so the
    // 0 case only occurs for unusual recordings. Marker fields cannot be
    // left out, and the JSON writer rejects NaN.
    record_period.or(fixed_period).unwrap_or(0)
}
```

* [ ] **Step 6: Run the tests**

Run: `cargo test -p samply`
Expected: all pass, including `fixed_periods_come_from_period_attributes_only` and `marker_period_prefers_the_record`.

* [ ] **Step 7: Format, lint, commit**

```bash
cd /home/moritz/dev/repos/samply/.claude/worktrees/import-period
cargo fmt --all
cargo clippy --all-targets
git add samply/src/linux_shared/event_interpretation.rs samply/src/shared/process_sample_data.rs samply/src/linux_shared/converter.rs samply/src/linux/profiler.rs
git commit -m "$(cat <<'EOF'
Record the period of each Other event marker.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_016aEc8uf7GZbHQHXmoX8o2k
EOF
)"
```

---

### Task 3: `samply import --weight-by-period`

Repository: samply, worktree `/home/moritz/dev/repos/samply/.claude/worktrees/import-period`.

**Files:**
* Modify: `samply/src/shared/prop_types.rs:70-104` (`ProfileCreationProps`)
* Modify: `samply/src/cli.rs:84-127` (`ImportArgs`), `:377-382` (`ImportArgs::profile_creation_props`), `:509-541` (`profile_creation_props_with_fallback_name`), `:588-621` (tests)
* Modify: `samply/src/linux_shared/event_interpretation.rs:28-29` (drop `#[allow(unused)]` on `main_event_name`)
* Modify: `samply/src/linux_shared/converter.rs` struct fields, `new` (`:158-162` off-CPU match, `Self { .. }`), `finish` (`:225-234`), `handle_main_event_sample` (`:309-382`), new pure functions and tests

**Interfaces:**
* Consumes: `EventInterpretation::{main_event_attr_index, main_event_name, fixed_periods, sampling_is_time_based}` (Task 2).
* Produces:
  * `ProfileCreationProps::weight_by_period: bool`, true only via `samply import --weight-by-period`.
  * `ImportArgs::weight_by_period: bool` (clap `--weight-by-period`).
  * `fn sample_weight(record_period: Option<u64>, fixed_period: Option<u64>, weight_by_period: bool) -> (i32, bool)` in `converter.rs`.
  * `fn off_cpu_sampling(sampling_is_time_based: Option<u64>, weight_by_period: bool) -> (u64, i32)` in `converter.rs`.

* [ ] **Step 1: Write the failing tests**

Add to the `mod tests` at the end of `samply/src/linux_shared/converter.rs`:

```rust
    #[test]
    fn sample_weight_uses_record_period() {
        assert_eq!(sample_weight(Some(1234), Some(10), true), (1234, false));
    }

    #[test]
    fn sample_weight_falls_back_to_fixed_period() {
        assert_eq!(sample_weight(None, Some(10_000), true), (10_000, false));
    }

    #[test]
    fn sample_weight_without_any_period_is_one() {
        assert_eq!(sample_weight(None, None, true), (1, false));
    }

    #[test]
    fn sample_weight_is_one_without_the_flag() {
        assert_eq!(sample_weight(Some(1234), Some(10), false), (1, false));
    }

    #[test]
    fn sample_weight_saturates_above_i32_max() {
        let max = u64::try_from(i32::MAX).unwrap();
        assert_eq!(sample_weight(Some(max), None, true), (i32::MAX, false));
        assert_eq!(sample_weight(Some(max + 1), None, true), (i32::MAX, true));
        assert_eq!(sample_weight(None, Some(u64::MAX), true), (i32::MAX, true));
    }

    #[test]
    fn off_cpu_samples_weigh_zero_with_period_weights() {
        assert_eq!(off_cpu_sampling(Some(1_001_001), false), (1_001_001, 1));
        assert_eq!(off_cpu_sampling(Some(1_001_001), true), (1_001_001, 0));
        assert_eq!(
            off_cpu_sampling(None, false),
            (DEFAULT_OFF_CPU_SAMPLING_INTERVAL_NS, 0)
        );
        assert_eq!(
            off_cpu_sampling(None, true),
            (DEFAULT_OFF_CPU_SAMPLING_INTERVAL_NS, 0)
        );
    }
```

Add to `mod test` in `samply/src/cli.rs`:

```rust
    #[test]
    fn verify_cli_import_weight_by_period() {
        let opt = Opt::parse_from(["samply", "import", "perf.data", "--weight-by-period"]);
        let Action::Import(import_args) = opt.action else {
            panic!("expected the import action");
        };
        assert!(import_args.profile_creation_props().weight_by_period);

        let opt = Opt::parse_from(["samply", "import", "perf.data"]);
        let Action::Import(import_args) = opt.action else {
            panic!("expected the import action");
        };
        assert!(!import_args.profile_creation_props().weight_by_period);
    }
```

* [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p samply sample_weight off_cpu_samples verify_cli_import`
Expected: compile errors for `sample_weight`, `off_cpu_sampling`, and the missing `weight_by_period` field.

* [ ] **Step 3: Add the prop and the flag**

In `samply/src/shared/prop_types.rs`, add after `should_emit_cswitch_markers` in `ProfileCreationProps`:

```rust
    /// Weight main-event samples by their perf event period instead of 1.
    /// Only `samply import` of perf.data files sets this.
    pub weight_by_period: bool,
```

In `samply/src/cli.rs`, add to `ImportArgs` after `time_range`:

```rust
    /// Weight each sample of the first perf event by its period, so sample
    /// totals count events such as cycles or cache misses instead of samples.
    /// Off-CPU samples get weight 0. The Firefox Profiler still labels the
    /// weighted totals as samples.
    #[arg(long)]
    pub weight_by_period: bool,
```

Replace `ImportArgs::profile_creation_props` with:

```rust
    pub fn profile_creation_props(&self) -> ProfileCreationProps {
        let filename = self.file.file_name().unwrap_or(self.file.as_os_str());
        let fallback_profile_name = filename.to_string_lossy().into();
        let mut props = self
            .profile_creation_args
            .profile_creation_props_with_fallback_name(fallback_profile_name);
        props.weight_by_period = self.weight_by_period;
        props
    }
```

In `profile_creation_props_with_fallback_name`, add after `should_emit_cswitch_markers: self.cswitch_markers,`:

```rust
            // Only `samply import` sets this, in `ImportArgs::profile_creation_props`.
            weight_by_period: false,
```

* [ ] **Step 4: Add the pure weight functions**

In `samply/src/linux_shared/converter.rs`, add above `fn marker_period`:

```rust
/// Weight of one main-event sample, and whether it saturated.
///
/// With `weight_by_period`, the weight is the record's period, else the
/// attribute's fixed period, capped at `i32::MAX`. Without the flag, or
/// when neither period is known, the weight is 1.
fn sample_weight(
    record_period: Option<u64>,
    fixed_period: Option<u64>,
    weight_by_period: bool,
) -> (i32, bool) {
    if !weight_by_period {
        return (1, false);
    }
    match record_period.or(fixed_period) {
        None => (1, false),
        Some(period) => match i32::try_from(period) {
            Ok(weight) => (weight, false),
            Err(_) => (i32::MAX, true),
        },
    }
}

/// Off-CPU sampling interval in nanoseconds and the weight of each off-CPU
/// sample.
fn off_cpu_sampling(sampling_is_time_based: Option<u64>, weight_by_period: bool) -> (u64, i32) {
    let (interval_ns, weight) = match sampling_is_time_based {
        Some(interval_ns) => (interval_ns, 1),
        None => (DEFAULT_OFF_CPU_SAMPLING_INTERVAL_NS, 0),
    };
    // Period weights count events of the main event, and off-CPU time
    // produces none, so a weight of 1 would mix units.
    if weight_by_period {
        (interval_ns, 0)
    } else {
        (interval_ns, weight)
    }
}
```

* [ ] **Step 5: Wire the weights into the converter**

In `samply/src/linux_shared/event_interpretation.rs`, delete the `#[allow(unused)]` line above `pub main_event_name: String,` (the converter reads it now).

In `pub struct Converter`, add after `fixed_periods`:

```rust
    /// Whether main-event samples weigh their period instead of 1.
    weight_by_period: bool,
    /// Name of the main event, for the saturation warning.
    main_event_name: String,
    /// Fixed period of the main event, for records without `PERIOD`.
    main_event_fixed_period: Option<u64>,
    /// Largest main-event period that did not fit a sample weight.
    largest_saturated_period: Option<u64>,
```

In `Converter::new`, replace

```rust
        let (off_cpu_sampling_interval_ns, off_cpu_weight_per_sample) =
            match &interpretation.sampling_is_time_based {
                Some(interval_ns) => (*interval_ns, 1),
                None => (DEFAULT_OFF_CPU_SAMPLING_INTERVAL_NS, 0),
            };
```

with

```rust
        let (off_cpu_sampling_interval_ns, off_cpu_weight_per_sample) = off_cpu_sampling(
            interpretation.sampling_is_time_based,
            profile_creation_props.weight_by_period,
        );
        let main_event_fixed_period = interpretation
            .fixed_periods
            .get(interpretation.main_event_attr_index)
            .copied()
            .flatten();
```

and add to `Self { .. }` after `fixed_periods: interpretation.fixed_periods,`:

```rust
            weight_by_period: profile_creation_props.weight_by_period,
            main_event_name: interpretation.main_event_name,
            main_event_fixed_period,
            largest_saturated_period: None,
```

In `finish`, insert at the top of the body:

```rust
        if let Some(period) = self.largest_saturated_period {
            eprintln!(
                "Warning: some {} samples have periods above {}, the largest sample weight, so their weights were capped. Largest period: {period}.",
                self.main_event_name,
                i32::MAX
            );
        }
```

In `handle_main_event_sample`, after the `let cpu_delta = ... ;` statement and before `let stack_index = ...`, insert:

```rust
        let (weight, saturated) = sample_weight(
            e.period,
            self.main_event_fixed_period,
            self.weight_by_period,
        );
        if saturated {
            let period = e.period.or(self.main_event_fixed_period).unwrap_or(0);
            self.largest_saturated_period = self.largest_saturated_period.max(Some(period));
        }
```

Then change the weight argument of all three `process.unresolved_samples.add_sample(...)` calls in `handle_main_event_sample` from `1,` to `weight,` (the thread sample after `cpu_delta,`, the per-CPU sample, and the combined-CPU sample after `CpuDelta::ZERO,`).

* [ ] **Step 6: Run the tests**

Run: `cargo test -p samply`
Expected: all pass, including the six converter tests and `verify_cli_import_weight_by_period`. `cargo run -p samply -- import --help` lists `--weight-by-period` with the doc text from Step 3.

* [ ] **Step 7: Format, lint, commit**

```bash
cd /home/moritz/dev/repos/samply/.claude/worktrees/import-period
cargo fmt --all
cargo clippy --all-targets
git add samply/src/shared/prop_types.rs samply/src/cli.rs samply/src/linux_shared/event_interpretation.rs samply/src/linux_shared/converter.rs
git commit -m "$(cat <<'EOF'
Add samply import --weight-by-period.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_016aEc8uf7GZbHQHXmoX8o2k
EOF
)"
```

---

### Task 4: `Perf events` section in `meta.extra`

Repository: samply, worktree `/home/moritz/dev/repos/samply/.claude/worktrees/import-period`.

**Files:**
* Modify: `samply/src/linux_shared/event_interpretation.rs` (new functions and tests)
* Modify: `samply/src/linux_shared/mod.rs:22` (re-export)
* Modify: `samply/src/linux_shared/converter.rs:9-14` (fxprof imports), add `Converter::add_extra_info_section` after `set_os_name` (`:240-242`)
* Modify: `samply/src/import/perf.rs:10` (imports), `:12-15` (linux_shared imports), `:107` (after `divine_from_attrs`), `:181-188` (after `Converter::new`)

**Interfaces:**
* Consumes: `ExtraInfoEntry::string` and `Profile::add_extra_info_section` (Task 1), `ProfileCreationProps::weight_by_period` (Task 3), `EventInterpretation::event_names`.
* Produces:
  * `pub fn sampling_description(policy: SamplingPolicy) -> String`.
  * `pub fn perf_events_section(event_names: &[String], policies: &[SamplingPolicy], weight_by_period: bool) -> Vec<(String, String)>`, re-exported from `crate::linux_shared`.
  * `pub fn Converter::add_extra_info_section(&mut self, label: &str, entries: Vec<(String, String)>)`.

* [ ] **Step 1: Write the failing test**

Add to `mod tests` in `event_interpretation.rs`:

```rust
    #[test]
    fn perf_events_section_lists_every_attribute_then_the_weight_mode() {
        let names = vec![
            "cycles:u".to_string(),
            "cache-misses".to_string(),
            "<unknown event 2>".to_string(),
        ];
        let policies = [
            SamplingPolicy::Frequency(999),
            SamplingPolicy::Period(NonZeroU64::new(10_000).unwrap()),
            SamplingPolicy::NoSampling,
        ];
        assert_eq!(
            perf_events_section(&names, &policies, false),
            vec![
                ("cycles:u".to_string(), "frequency 999 Hz".to_string()),
                ("cache-misses".to_string(), "period 10000".to_string()),
                ("<unknown event 2>".to_string(), "no sampling".to_string()),
                ("Sample weight".to_string(), "1".to_string()),
            ]
        );
        assert_eq!(
            perf_events_section(&names, &policies, true).last(),
            Some(&("Sample weight".to_string(), "period".to_string()))
        );
    }
```

* [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p samply perf_events_section`
Expected: compile error, `cannot find function perf_events_section`.

* [ ] **Step 3: Implement the pure functions**

Add to `event_interpretation.rs` after `fixed_periods`:

```rust
/// Value of an attribute's `Perf events` entry: `frequency N Hz` or
/// `period N`. Non-sampling attributes, which produce no samples, read
/// `no sampling`.
pub fn sampling_description(policy: SamplingPolicy) -> String {
    match policy {
        SamplingPolicy::Frequency(hz) => format!("frequency {hz} Hz"),
        SamplingPolicy::Period(period) => format!("period {period}"),
        SamplingPolicy::NoSampling => "no sampling".to_string(),
    }
}

/// Entries of the `Perf events` info section: one per attribute in
/// attribute order, labeled with the event name and valued with its
/// sampling, then `Sample weight`, which reads `period` with period weights
/// and `1` otherwise. The first entry is the main event.
pub fn perf_events_section(
    event_names: &[String],
    policies: &[SamplingPolicy],
    weight_by_period: bool,
) -> Vec<(String, String)> {
    let mut entries: Vec<(String, String)> = event_names
        .iter()
        .zip(policies)
        .map(|(name, policy)| (name.clone(), sampling_description(*policy)))
        .collect();
    let weight = if weight_by_period { "period" } else { "1" };
    entries.push(("Sample weight".to_string(), weight.to_string()));
    entries
}
```

In `samply/src/linux_shared/mod.rs`, change line 22 to:

```rust
pub use event_interpretation::{
    perf_events_section, EventInterpretation, KnownEvent, OffCpuIndicator,
};
```

* [ ] **Step 4: Write the section on import**

In `converter.rs`, add `ExtraInfoEntry` to the `use fxprof_processed_profile::{...}` list and add after `set_os_name`:

```rust
    /// Add a section of `(label, value)` string entries to the profile's
    /// `meta.extra`.
    pub fn add_extra_info_section(&mut self, label: &str, entries: Vec<(String, String)>) {
        let entries = entries
            .iter()
            .map(|(label, value)| ExtraInfoEntry::string(label, value))
            .collect();
        self.profile.add_extra_info_section(label, entries);
    }
```

In `samply/src/import/perf.rs`, change the imports to

```rust
use linux_perf_event_reader::{EventRecord, RecordType, SamplingPolicy};

use crate::linux_shared::{
    perf_events_section, ConvertRegs, ConvertRegsAarch64, ConvertRegsX86_64, Converter,
    EventInterpretation, KnownEvent, MmapRangeOrVec,
};
```

After `let interpretation = EventInterpretation::divine_from_attrs(attributes);` add:

```rust
    let sampling_policies: Vec<SamplingPolicy> = attributes
        .iter()
        .map(|attr_desc| attr_desc.attr.sampling_policy)
        .collect();
```

After the `Converter::<U>::new(...)` statement (before the Android `set_os_name` block) add:

```rust
    converter.add_extra_info_section(
        "Perf events",
        perf_events_section(
            &interpretation.event_names,
            &sampling_policies,
            profile_creation_props.weight_by_period,
        ),
    );
```

* [ ] **Step 5: Run the tests**

Run: `cargo test -p samply`
Expected: all pass.

* [ ] **Step 6: Format, lint, commit**

```bash
cd /home/moritz/dev/repos/samply/.claude/worktrees/import-period
cargo fmt --all
cargo clippy --all-targets
git add samply/src/linux_shared/event_interpretation.rs samply/src/linux_shared/mod.rs samply/src/linux_shared/converter.rs samply/src/import/perf.rs
git commit -m "$(cat <<'EOF'
Name perf events and their sampling in meta.extra.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_016aEc8uf7GZbHQHXmoX8o2k
EOF
)"
```

---

### Task 5: Check pull request 1 against real recordings

Repository: samply, worktree `/home/moritz/dev/repos/samply/.claude/worktrees/import-period`. No repository changes, no commit. Scripts and outputs go in the scratchpad `S`.

**Files:**
* Create (scratch only): `$S/check_periods.py`

**Interfaces:**
* Consumes: `$S/samply-base` (Task 1 Step 1), the Task 4 build, recordings `$S/multi_F.data` (`-F 999`, four events), `$S/multi_c.data` (`-c 10000`, cycles and cache-misses), `$S/swcs.data` (`-F cycles` with `--switch-events`).
* Produces: evidence that the default import differs from upstream only by the marker field and `meta.extra`, and that flagged weights match `perf script`.

* [ ] **Step 1: Build and import**

```bash
S=/tmp/claude-1000/-home-moritz-dev-repos-samply/c426bfa0-d211-4428-9e82-e5b3db072f8b/scratchpad
W=/home/moritz/dev/repos/samply/.claude/worktrees/import-period
cd "$W" && cargo build -p samply
for d in multi_F multi_c swcs; do
  "$S/samply-base" import "$S/$d.data" -s -o "$S/$d.base.json.gz"
  "$S/samply-base" import "$S/$d.data" -s -o "$S/$d.base2.json.gz"
  "$W/target/debug/samply" import "$S/$d.data" -s -o "$S/$d.new.json.gz"
  "$W/target/debug/samply" import "$S/$d.data" -s --weight-by-period -o "$S/$d.wbp.json.gz"
done
```

* [ ] **Step 2: Write the check script**

Create `$S/check_periods.py`:

```python
#!/usr/bin/env python3
"""Check samply's perf event periods against perf script.

Usage: check_periods.py NAME...  (NAME.data and its imports live in the scratchpad)
"""
import collections
import gzip
import json
import subprocess
import sys

S = "/tmp/claude-1000/-home-moritz-dev-repos-samply/c426bfa0-d211-4428-9e82-e5b3db072f8b/scratchpad"


def load(path):
    with gzip.open(path) as f:
        return json.load(f)


def tid_of(thread):
    return int(str(thread["tid"]).split(".")[0])


def strip_additions(profile):
    """Remove what pull request 1 adds to a default import."""
    profile["meta"].pop("extra", None)
    for schema in profile["meta"]["markerSchema"]:
        if schema["name"] == "Other event":
            schema["fields"] = [f for f in schema["fields"] if f["key"] != "period"]
    for thread in profile["threads"]:
        for data in thread["markers"]["data"]:
            if isinstance(data, dict) and data.get("type") == "Other event":
                data.pop("period", None)
    return profile


def perf_sums(name):
    """Per-tid period sums of the main event, and per-(tid, event) sums of the others."""
    data = f"{S}/{name}.data"
    evlist = subprocess.run(["perf", "evlist", "-i", data], capture_output=True, text=True, check=True)
    main_event = evlist.stdout.split()[0]
    script = subprocess.run(
        ["perf", "script", "-i", data, "-F", "tid,time,period,event", "--ns"],
        capture_output=True, text=True, check=True,
    )
    samples = collections.Counter()
    markers = collections.Counter()
    last_time = {}
    for line in script.stdout.splitlines():
        parts = line.split()
        if len(parts) < 4:
            continue
        tid, time, period, event = int(parts[0]), parts[1], int(parts[2]), parts[3].removesuffix(":")
        if event == main_event:
            # samply drops idle-thread samples and duplicate timestamps per thread.
            if tid == 0 or last_time.get(tid) == time:
                continue
            last_time[tid] = time
            samples[tid] += period
        else:
            markers[(tid, event)] += period
    return main_event, samples, markers


def profile_sums(profile):
    strings = profile["shared"]["stringArray"]
    samples = collections.Counter()
    markers = collections.Counter()
    zero_weight = 0
    for thread in profile["threads"]:
        weights = thread["samples"].get("weight") or [1] * thread["samples"]["length"]
        samples[tid_of(thread)] += sum(weights)
        zero_weight += sum(1 for w in weights if w == 0)
        table = thread["markers"]
        for i, data in enumerate(table["data"]):
            if isinstance(data, dict) and data.get("type") == "Other event":
                markers[(tid_of(thread), strings[table["name"][i]])] += data["period"]
    return samples, markers, zero_weight


def nonzero(counter):
    return {k: v for k, v in counter.items() if v}


def main():
    for name in sys.argv[1:]:
        base = load(f"{S}/{name}.base.json.gz")
        assert base == load(f"{S}/{name}.base2.json.gz"), f"{name}: baseline import is not deterministic"
        new = load(f"{S}/{name}.new.json.gz")
        wbp = load(f"{S}/{name}.wbp.json.gz")
        main_event, perf_samples, perf_markers = perf_sums(name)

        for profile, weight in ((new, "1"), (wbp, "period")):
            (section,) = profile["meta"]["extra"]
            assert section["label"] == "Perf events", section
            entries = section["entries"]
            assert entries[0]["label"] == main_event, entries
            assert entries[-1] == {"label": "Sample weight", "format": "string", "value": weight}, entries
            print(name, weight, [(e["label"], e["value"]) for e in entries])

        new_samples, new_markers, new_zero = profile_sums(new)
        wbp_samples, wbp_markers, wbp_zero = profile_sums(wbp)
        assert nonzero(wbp_samples) == nonzero(perf_samples), (name, wbp_samples, perf_samples)
        assert wbp_markers == perf_markers, (name, wbp_markers, perf_markers)
        assert new_markers == perf_markers, (name, new_markers, perf_markers)
        assert new_zero == 0, f"{name}: unflagged import has zero-weight samples"
        if name == "swcs":
            assert wbp_zero > 0, "swcs: expected off-CPU samples of weight 0"

        assert strip_additions(new) == base, f"{name}: default import changed beyond period and meta.extra"
        print(name, "ok")


main()
```

* [ ] **Step 3: Run the check**

Run: `python3 "$S/check_periods.py" multi_F multi_c swcs`
Expected: three `ok` lines. `multi_F` entries read `frequency 999 Hz`, `multi_c` entries read `period 10000`.
If the determinism assertion fires, the comparison against the baseline is meaningless: report that and compare only the sums. If the stripped comparison fails, find the first difference with `diff <(zcat "$S/multi_F.base.json.gz" | jq -S .) <(python3 -c 'import sys; sys.argv=["x"]; exec(open("'"$S"'/check_periods.py").read().split("def main")[0]); import json; print(json.dumps(strip_additions(load("'"$S"'/multi_F.new.json.gz")), sort_keys=True, indent=2))') | head -40` and fix the code before continuing.

---

### Task 6: Pull request 1 docs and description

Repository: samply, worktree `/home/moritz/dev/repos/samply/.claude/worktrees/import-period`.

**Files:**
* Modify: `README.md:107-109` (new subsection before `## Examples`)

**Interfaces:**
* Consumes: the `--weight-by-period` help text (Task 3).
* Produces: README section, pull request description text for the controller.

* [ ] **Step 1: Document the flag**

Insert before `## Examples` in `README.md`:

```markdown
### Weighting imported perf.data samples by period

`samply import perf.data` gives every sample of the first perf event a weight of 1.
With `perf record -F`, perf adjusts the sampling period per sample, so equal sample counts can stand for different event counts.
Pass `--weight-by-period` to weight each sample by its period instead, so totals count events such as cycles or cache misses.
The Firefox Profiler still labels these weighted totals as samples.
Every import names its perf events and their sampling in the profile info panel, and each marker of a non-main event records its period.
```

* [ ] **Step 2: Commit**

```bash
cd /home/moritz/dev/repos/samply/.claude/worktrees/import-period
git add README.md
git commit -m "$(cat <<'EOF'
Document samply import --weight-by-period.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_016aEc8uf7GZbHQHXmoX8o2k
EOF
)"
```

* [ ] **Step 3: Hand the pull request description to the controller**

Title: `Record perf event periods in samply import`

Body:

```markdown
`samply import` drops each perf sample's period, so a profile cannot say how many events a sample stands for. With `perf record -F`, perf adjusts the period per sample, and equal sample counts do not mean equal event counts.

* fxprof-processed-profile gains `Profile::add_extra_info_section`, written to `meta.extra`.
* Every import adds a `Perf events` section to `meta.extra` naming each event, its sampling (`frequency N Hz` or `period N`), and the sample weight mode.
* `Other event` markers gain a `period` field, from the record or the attribute's fixed period.
* `samply import --weight-by-period` weights main-event samples by their period and off-CPU samples by 0.

The marker `period` field changes the marker schema of every import, not only flagged ones. I can put it behind `--weight-by-period` if you prefer.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

https://claude.ai/code/session_016aEc8uf7GZbHQHXmoX8o2k
```

---

### Task 7: CPU delta only for clock events

Repository: samply. New worktree `/home/moritz/dev/repos/samply/.claude/worktrees/cpu-delta-clock-events` on new branch `cpu-delta-clock-events`.

**Files:**
* Modify: `samply/src/linux_shared/event_interpretation.rs:25-35` (struct), `:38-113` (`divine_from_attrs`), new function and test module
* Modify: `samply/src/linux_shared/converter.rs:79-81` (struct field), `Self { .. }` in `new`, `:309-320` (`cpu_delta` in `handle_main_event_sample`)
* Modify: `samply/src/linux/profiler.rs:337-345`

**Interfaces:**
* Consumes: `linux_perf_event_reader::{PerfEventType, SoftwareCounterType}` (already imported in `event_interpretation.rs`), `HardwareEventId`, `PmuTypeId` (tests only).
* Produces:
  * `EventInterpretation::main_event_is_clock: bool`.
  * `pub fn is_clock_event(type_: PerfEventType) -> bool`.

* [ ] **Step 1: Create the worktree**

```bash
git -C /home/moritz/dev/repos/samply fetch upstream
git -C /home/moritz/dev/repos/samply worktree add -b cpu-delta-clock-events /home/moritz/dev/repos/samply/.claude/worktrees/cpu-delta-clock-events upstream/main
git -C /home/moritz/dev/repos/samply/.claude/worktrees/cpu-delta-clock-events log -1 --format=%h
```

Expected: `da48ff40`, or a newer upstream commit. If newer, confirm `samply/src/linux_shared/converter.rs` still contains `// TODO: Detect event type` before continuing. All paths below are relative to this worktree.

* [ ] **Step 2: Write the failing test**

Append to `samply/src/linux_shared/event_interpretation.rs`:

```rust
#[cfg(test)]
mod tests {
    use linux_perf_event_reader::{HardwareEventId, PmuTypeId};

    use super::*;

    #[test]
    fn clock_events_are_cpu_clock_and_task_clock() {
        assert!(is_clock_event(PerfEventType::Software(
            SoftwareCounterType::CpuClock
        )));
        assert!(is_clock_event(PerfEventType::Software(
            SoftwareCounterType::TaskClock
        )));
        assert!(!is_clock_event(PerfEventType::Hardware(
            HardwareEventId::CpuCycles,
            PmuTypeId(0)
        )));
    }
}
```

* [ ] **Step 3: Run the test to verify it fails**

Run: `cd /home/moritz/dev/repos/samply/.claude/worktrees/cpu-delta-clock-events && cargo test -p samply clock_events`
Expected: compile error, `cannot find function is_clock_event`.

* [ ] **Step 4: Implement**

In `event_interpretation.rs`, add after `sampling_is_time_based` in `pub struct EventInterpretation`:

```rust
    /// Whether the main event is `cpu-clock` or `task-clock`, whose period
    /// is CPU time in nanoseconds.
    pub main_event_is_clock: bool,
```

In `divine_from_attrs`, after the `sampling_is_time_based` binding:

```rust
        let main_event_is_clock = is_clock_event(attrs[0].attr.type_);
```

and add `main_event_is_clock,` after `sampling_is_time_based,` in the returned `Self { .. }`.

Add after the `impl EventInterpretation` block:

```rust
/// Whether the event is the software `cpu-clock` or `task-clock` event,
/// whose period counts nanoseconds.
pub fn is_clock_event(type_: PerfEventType) -> bool {
    matches!(
        type_,
        PerfEventType::Software(SoftwareCounterType::CpuClock | SoftwareCounterType::TaskClock)
    )
}
```

In `samply/src/linux/profiler.rs`, add after `sampling_is_time_based: Some(interval_nanos),`:

```rust
        // The record path always has context-switch data, so it never
        // reads the period as CPU time.
        main_event_is_clock: false,
```

In `converter.rs`, add after `off_cpu_indicator: Option<OffCpuIndicator>,` in `pub struct Converter`:

```rust
    /// Whether the main event's period is CPU time.
    main_event_is_clock: bool,
```

and `main_event_is_clock: interpretation.main_event_is_clock,` before `off_cpu_indicator: interpretation.off_cpu_indicator,` in `Self { .. }`.

In `handle_main_event_sample`, replace

```rust
        } else if let Some(period) = e.period {
            // If the observed perf event is one of the clock time events, or cycles, then we should convert it to a CpuDelta.
            // TODO: Detect event type
            CpuDelta::from_nanos(period)
        } else {
```

with

```rust
        } else if let (true, Some(period)) = (self.main_event_is_clock, e.period) {
            // If the observed perf event is one of the clock time events, its
            // period is CPU time, so convert it to a CpuDelta. Other events,
            // such as cycles, count something other than time.
            CpuDelta::from_nanos(period)
        } else {
```

* [ ] **Step 5: Run the tests**

Run: `cargo test -p samply`
Expected: all pass.

* [ ] **Step 6: Format, lint, commit**

```bash
cd /home/moritz/dev/repos/samply/.claude/worktrees/cpu-delta-clock-events
cargo fmt --all
cargo clippy --all-targets
git add samply/src/linux_shared/event_interpretation.rs samply/src/linux_shared/converter.rs samply/src/linux/profiler.rs
git commit -m "$(cat <<'EOF'
Only use the main event's period as CPU delta for clock events.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_016aEc8uf7GZbHQHXmoX8o2k
EOF
)"
```

---

### Task 8: Check pull request 2 and describe it

Repository: samply, worktree `/home/moritz/dev/repos/samply/.claude/worktrees/cpu-delta-clock-events`. No repository changes. This branch adds no flag, so it needs no README change.

**Files:**
* Create (scratch only): `$S/cpu_deltas.py`

**Interfaces:**
* Consumes: `$S/samply-base` (Task 1 Step 1; if missing, build `upstream/main` and copy `target/debug/samply` there), recordings `$S/cyc_F.data` (`-F cycles`, no context switches) and `$S/swcs.data` (`-F cycles` with context switches).
* Produces: evidence that non-clock imports without context switches lose their CPU deltas and that context-switch imports keep theirs, plus the pull request description.

* [ ] **Step 1: Import with both builds**

```bash
S=/tmp/claude-1000/-home-moritz-dev-repos-samply/c426bfa0-d211-4428-9e82-e5b3db072f8b/scratchpad
W=/home/moritz/dev/repos/samply/.claude/worktrees/cpu-delta-clock-events
cd "$W" && cargo build -p samply
for d in cyc_F swcs; do
  "$S/samply-base" import "$S/$d.data" -s -o "$S/$d.pr2base.json.gz"
  "$W/target/debug/samply" import "$S/$d.data" -s -o "$S/$d.pr2.json.gz"
done
```

* [ ] **Step 2: Count nonzero CPU deltas**

Create `$S/cpu_deltas.py`:

```python
#!/usr/bin/env python3
"""Print the number of nonzero threadCPUDelta values in each profile."""
import gzip
import json
import sys

for path in sys.argv[1:]:
    with gzip.open(path) as f:
        profile = json.load(f)
    nonzero = sum(
        1
        for thread in profile["threads"]
        for delta in thread["samples"].get("threadCPUDelta") or []
        if delta
    )
    print(path, nonzero)
```

Run: `python3 "$S/cpu_deltas.py" "$S"/cyc_F.pr2base.json.gz "$S"/cyc_F.pr2.json.gz "$S"/swcs.pr2base.json.gz "$S"/swcs.pr2.json.gz`
Expected: `cyc_F.pr2base` nonzero, `cyc_F.pr2` 0, and `swcs.pr2base` equal to `swcs.pr2` (both nonzero). `cpu-clock` and `task-clock` cannot be recorded on this machine, so the unit test covers the clock branch.

* [ ] **Step 3: Hand the pull request description to the controller**

Title: `Only treat the main event's period as CPU time for clock events`

Body:

```markdown
Without context-switch records, `samply import` writes each main-event sample's period into `threadCPUDelta` as nanoseconds, whatever the event. For `perf record -e cycles`, this puts cycle counts into the CPU graph as if they were time.

The period now becomes the CPU delta only when the main event is `cpu-clock` or `task-clock`. Other main events get a CPU delta of 0.

This changes the Firefox Profiler's CPU graph for imports of non-clock events without context switches: it shows no CPU data instead of wrong values. Imports with context switches and `samply record` are unchanged.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

https://claude.ai/code/session_016aEc8uf7GZbHQHXmoX8o2k
```

---

### Task 9: Parse the `Perf events` section in pollard

Repository: pollard, worktree `/home/moritz/dev/repos/pollard/.claude/worktrees/period-weighting`, branch `period-weighting`.

**Files:**
* Create: `src/profile/perf_events.rs`
* Modify: `src/profile/raw.rs:9` (imports), `:113-124` (`RawMeta`), `:243-255` (`RawMarkerData`), new types and `lenient` fn, tests `:263-350`
* Modify: `src/profile/mod.rs:14` (register module)
* Modify: `src/profile/parsed.rs:15-26` (imports, struct), `:86-118` (`new_inner`), new accessor

**Interfaces:**
* Consumes: nothing from earlier pollard tasks.
* Produces:
  * `pub struct RawExtraSection { pub label: String, pub entries: Vec<RawExtraEntry> }`, `pub struct RawExtraEntry { pub label: String, pub format: String, pub value: serde_json::Value }`.
  * `RawMeta::extra: Vec<RawExtraSection>` and `RawMarkerData::period: Option<f64>`, both loading malformed values as empty.
  * `pub enum Sampling { Frequency(u64), Period(u64) }` with `parse(&str) -> Option<Self>` and `describe(&self) -> String`.
  * `pub struct PerfEvent { pub label: String, pub sampling: Option<Sampling> }`.
  * `pub struct PerfEvents { pub events: Vec<PerfEvent>, pub weight_by_period: bool }` with `from_extra(&[RawExtraSection]) -> Option<Self>`, `main_event() -> Option<&PerfEvent>`, `event(&str) -> Option<&PerfEvent>`, `fixed_period(&str) -> Option<u64>`.
  * `pub fn Profile::perf_events(&self) -> Option<&PerfEvents>`.

* [ ] **Step 1: Write the failing tests**

Create `src/profile/perf_events.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::raw::{RawExtraEntry, RawExtraSection};

    fn section(label: &str, entries: &[(&str, serde_json::Value)]) -> Vec<RawExtraSection> {
        vec![RawExtraSection {
            label: label.to_owned(),
            entries: entries
                .iter()
                .map(|(l, v)| RawExtraEntry {
                    label: (*l).to_owned(),
                    format: "string".to_owned(),
                    value: v.clone(),
                })
                .collect(),
        }]
    }

    #[test]
    fn parses_events_and_weight_mode() {
        let extra = section(
            "Perf events",
            &[
                ("cycles:u", "frequency 999 Hz".into()),
                ("cache-misses", "period 10000".into()),
                ("Sample weight", "period".into()),
            ],
        );
        let p = PerfEvents::from_extra(&extra).unwrap();
        assert!(p.weight_by_period);
        assert_eq!(p.events.len(), 2);
        assert_eq!(p.main_event().unwrap().label, "cycles:u");
        assert_eq!(p.main_event().unwrap().sampling, Some(Sampling::Frequency(999)));
        assert_eq!(p.fixed_period("cache-misses"), Some(10_000));
        assert_eq!(p.fixed_period("cycles:u"), None);
        assert_eq!(p.fixed_period("missing"), None);
    }

    #[test]
    fn sample_weight_one_is_not_period_weighting() {
        let extra = section("Perf events", &[("cycles", "period 5".into()), ("Sample weight", "1".into())]);
        assert!(!PerfEvents::from_extra(&extra).unwrap().weight_by_period);
    }

    #[test]
    fn invalid_values_are_ignored() {
        for bad in [
            serde_json::json!("frequency x Hz"),
            serde_json::json!("frequency 99"),
            serde_json::json!("period -5"),
            serde_json::json!("period +5"),
            serde_json::json!("period 1 "),
            serde_json::json!("Period 5"),
            serde_json::json!("period "),
            serde_json::json!("no sampling"),
            serde_json::json!(7),
        ] {
            let extra = section("Perf events", &[("ev", bad.clone()), ("Sample weight", "2".into())]);
            let p = PerfEvents::from_extra(&extra).unwrap();
            assert_eq!(p.events[0].label, "ev", "{bad}");
            assert_eq!(p.events[0].sampling, None, "{bad}");
            assert!(!p.weight_by_period, "{bad}");
        }
    }

    #[test]
    fn first_entry_with_a_label_wins() {
        let extra = section(
            "Perf events",
            &[("cycles", "period 5".into()), ("cycles", "period 9".into())],
        );
        assert_eq!(PerfEvents::from_extra(&extra).unwrap().fixed_period("cycles"), Some(5));
    }

    #[test]
    fn other_sections_are_not_perf_events() {
        let extra = section("Something else", &[("cycles", "period 5".into())]);
        assert!(PerfEvents::from_extra(&extra).is_none());
        assert!(PerfEvents::from_extra(&[]).is_none());
    }

    #[test]
    fn describe_round_trips_the_grammar() {
        for s in ["frequency 999 Hz", "period 10000", "period 0"] {
            assert_eq!(Sampling::parse(s).unwrap().describe(), s);
        }
    }
}
```

Add `pub mod perf_events;` to `src/profile/mod.rs` after `pub mod parsed;`.

Add to the `mod tests` in `src/profile/raw.rs`:

```rust
    #[test]
    fn meta_extra_sections_deserialize() {
        let json = MINIMAL.replace(
            r#""product": "test""#,
            r#""product": "test", "extra": [{"label": "Perf events", "entries": [{"label": "cycles", "format": "string", "value": "period 5"}]}]"#,
        );
        let p: RawProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(p.meta.extra[0].label, "Perf events");
        assert_eq!(p.meta.extra[0].entries[0].label, "cycles");
        assert_eq!(p.meta.extra[0].entries[0].value, serde_json::json!("period 5"));
    }

    #[test]
    fn malformed_meta_extra_loads_empty() {
        for extra in [r#"{"not": "an array"}"#, r#"[{"label": 3}]"#, "null", "7"] {
            let json = MINIMAL.replace(
                r#""product": "test""#,
                &format!(r#""product": "test", "extra": {extra}"#),
            );
            let p: RawProfile =
                serde_json::from_str(&json).unwrap_or_else(|e| panic!("{extra}: {e}"));
            assert!(p.meta.extra.is_empty(), "{extra}");
        }
    }

    #[test]
    fn non_numeric_marker_period_is_ignored() {
        let d: RawMarkerData =
            serde_json::from_str(r#"{"type": "Other event", "period": "x"}"#).unwrap();
        assert_eq!(d.period, None);
        let d: RawMarkerData =
            serde_json::from_str(r#"{"type": "Other event", "period": 25}"#).unwrap();
        assert_eq!(d.period, Some(25.0));
        let d: RawMarkerData = serde_json::from_str(r#"{"type": "Other event"}"#).unwrap();
        assert_eq!(d.period, None);
    }
```

* [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib profile::perf_events profile::raw`
Expected: compile errors, `cannot find type RawExtraSection` and similar.

* [ ] **Step 3: Add the raw types**

In `src/profile/raw.rs`, add the lenient deserializer after `deserialize_id_as_u64`:

```rust
/// Deserialize optional metadata leniently: a value of another shape
/// becomes `T::default()` instead of failing the whole profile load.
pub(crate) fn lenient<'de, D, T>(de: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = serde_json::Value::deserialize(de)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}
```

Add a field at the end of `RawMeta`:

```rust
    /// Labeled info sections (`meta.extra`). samply writes a `Perf events`
    /// section here, see [`crate::profile::perf_events`]. A value of an
    /// unexpected shape loads as empty.
    #[serde(default, deserialize_with = "lenient")]
    pub extra: Vec<RawExtraSection>,
```

Add after `RawMeta`:

```rust
/// One section of `meta.extra`, as the Firefox Profiler defines it.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct RawExtraSection {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub entries: Vec<RawExtraEntry>,
}

/// One labeled value of a [`RawExtraSection`]. samply writes only the
/// `string` format, whose value is a JSON string.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct RawExtraEntry {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub value: serde_json::Value,
}
```

Change the first sentence of the `RawMarkerData` doc comment from "Marker payload subset. Only `type` and `cause.stack` are consumed;" to "Marker payload subset. Only `type`, `cause.stack`, and `period` are consumed;" (keep the rest), and add a field after `cause`:

```rust
    /// Events this marker stands for. samply writes the perf record's
    /// period on `Other event` markers. A non-numeric value loads as `None`.
    #[serde(default, deserialize_with = "lenient")]
    pub period: Option<f64>,
```

* [ ] **Step 4: Implement the section parser**

Put this above the test module in `src/profile/perf_events.rs`:

```rust
//! The `Perf events` section that samply writes into `meta.extra`.
//!
//! samply lists one entry per perf event attribute, in attribute order,
//! then one `Sample weight` entry. The first event is the samples track.
//! Values follow this grammar, with `N` a decimal integer:
//!
//! ```text
//! event value  = "frequency " N " Hz" | "period " N
//! weight value = "period" | "1"
//! ```
//!
//! Values outside the grammar are ignored.

use crate::profile::raw::RawExtraSection;

/// Label of the section samply writes.
pub const SECTION_LABEL: &str = "Perf events";
/// Label of the entry that says how samples are weighted.
pub const SAMPLE_WEIGHT_LABEL: &str = "Sample weight";

/// How perf sampled one event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sampling {
    /// `perf record -F N`: perf adjusts the period to reach N samples per second.
    Frequency(u64),
    /// `perf record -c N`: one sample every N events.
    Period(u64),
}

impl Sampling {
    /// Parse an event value, or `None` when it does not match the grammar.
    pub fn parse(value: &str) -> Option<Self> {
        if let Some(rest) = value.strip_prefix("frequency ") {
            return parse_decimal(rest.strip_suffix(" Hz")?).map(Sampling::Frequency);
        }
        parse_decimal(value.strip_prefix("period ")?).map(Sampling::Period)
    }

    /// The event value in the grammar's form, e.g. `frequency 999 Hz`.
    pub fn describe(&self) -> String {
        match self {
            Sampling::Frequency(hz) => format!("frequency {hz} Hz"),
            Sampling::Period(n) => format!("period {n}"),
        }
    }
}

/// A non-empty run of ASCII digits. `u64::from_str` alone would accept `+5`.
fn parse_decimal(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// One perf event of the section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerfEvent {
    /// The event name as samply recorded it, e.g. `cycles:u`.
    pub label: String,
    /// `None` when the value does not match the grammar.
    pub sampling: Option<Sampling>,
}

/// The parsed `Perf events` section.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PerfEvents {
    /// Events in attribute order. The first is the samples track.
    pub events: Vec<PerfEvent>,
    /// True when `Sample weight` reads `period`.
    pub weight_by_period: bool,
}

impl PerfEvents {
    /// Parse the first `Perf events` section, or `None` when there is none.
    pub fn from_extra(sections: &[RawExtraSection]) -> Option<Self> {
        let section = sections.iter().find(|s| s.label == SECTION_LABEL)?;
        let mut out = PerfEvents::default();
        for entry in &section.entries {
            let value = entry.value.as_str();
            if entry.label == SAMPLE_WEIGHT_LABEL {
                out.weight_by_period = value == Some("period");
                continue;
            }
            out.events.push(PerfEvent {
                label: entry.label.clone(),
                sampling: value.and_then(Sampling::parse),
            });
        }
        Some(out)
    }

    /// The event behind the samples track.
    pub fn main_event(&self) -> Option<&PerfEvent> {
        self.events.first()
    }

    /// The first event with this label. Markers of equally named events
    /// are indistinguishable, so later duplicates never match.
    pub fn event(&self, label: &str) -> Option<&PerfEvent> {
        self.events.iter().find(|e| e.label == label)
    }

    /// The fixed period of the event with this label, when it was
    /// recorded with `perf record -c`.
    pub fn fixed_period(&self, label: &str) -> Option<u64> {
        match self.event(label)?.sampling? {
            Sampling::Period(n) => Some(n),
            Sampling::Frequency(_) => None,
        }
    }
}
```

* [ ] **Step 5: Parse once per `Profile`**

In `src/profile/parsed.rs`, add `use crate::profile::perf_events::{self, PerfEvents};` after `use crate::profile::event_source::EventSource;`. (`self` is used by Task 10.)

Add a field at the end of `pub struct Profile`:

```rust
    /// The `Perf events` section of `meta.extra`, parsed once.
    perf_events: Option<PerfEvents>,
```

In `new_inner`, before the final `Self { .. }`:

```rust
        let perf_events = PerfEvents::from_extra(&raw.meta.extra);
```

and add `perf_events,` to `Self { raw, transforms, threads }`.

Add after `pub fn meta(&self)`:

```rust
    /// The profile's `Perf events` section, when samply wrote one.
    pub fn perf_events(&self) -> Option<&PerfEvents> {
        self.perf_events.as_ref()
    }
```

Task 10 uses the `perf_events` module import. Until then, write the import as `use crate::profile::perf_events::PerfEvents;` if clippy flags the unused `self`, and switch to `{self, PerfEvents}` in Task 10.

* [ ] **Step 6: Run the tests**

Run: `cargo test`
Expected: all pass, including the six `perf_events` tests and three new `raw` tests.

* [ ] **Step 7: Format, lint, commit**

```bash
cd /home/moritz/dev/repos/pollard/.claude/worktrees/period-weighting
cargo fmt
cargo clippy --all-targets
git add src/profile/perf_events.rs src/profile/mod.rs src/profile/raw.rs src/profile/parsed.rs
git commit -m "$(cat <<'EOF'
feat: parse samply's Perf events section from meta.extra

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_016aEc8uf7GZbHQHXmoX8o2k
EOF
)"
```

---

### Task 10: Weight per sample and marker

Repository: pollard, worktree `/home/moritz/dev/repos/pollard/.claude/worktrees/period-weighting`.

**Files:**
* Modify: `src/profile/perf_events.rs` (weight functions and tests)
* Modify: `src/profile/parsed.rs:490-605` (`stack_indices` block), tests `:644-1121`
* Create: `tests/fixtures/weighted_events.json`

**Interfaces:**
* Consumes: `PerfEvents::{weight_by_period, fixed_period}`, `Profile::perf_events`, `RawMarkerData::period` (Task 9).
* Produces:
  * `pub fn event_count(v: f64) -> Option<u64>`, `pub fn sample_weight(weights: Option<&[f64]>, i: usize, weight_by_period: bool) -> u64`, `pub fn marker_weight(period: Option<f64>, fixed_period: Option<u64>) -> u64` in `crate::profile::perf_events`.
  * `pub fn Profile::weighted_stack_indices<'a>(&'a self, handle: ThreadHandle, source: &'a EventSource, time_range: Option<[f64; 2]>) -> Box<dyn Iterator<Item = (Option<usize>, u64)> + 'a>`.
  * `pub fn Profile::stack_indices` keeps its signature and drops the weights.
  * `pub fn Profile::is_weighted(&self, handles: impl IntoIterator<Item = ThreadHandle>, source: &EventSource, time_range: Option<[f64; 2]>) -> bool`.
  * `pub fn Profile::samples_weighted_by_period(&self) -> bool`.
  * `pub fn Profile::sample_weight(&self, handle: ThreadHandle, i: usize) -> u64`.
  * Fixture `tests/fixtures/weighted_events.json`: functions `hot` (frame 0, address 256, line 10) and `cold` (frame 1, address 512, line 20) in module `app`, file `/src/lib.rs`. Samples: 2 on `hot` weighing 100 each, 2 on `cold` weighing 300 each (total 800, `cold` 75%). `cache-misses` markers: 2 on `hot` with `period` 10, 2 on `cold` with `period` 30, at times 0, 1, 2, 3 (total 80, `cold` 75%). `instructions` markers without `period`: 1 on `hot`, 1 on `cold`, fixed `period 1000` (total 2000). `Sample weight: period`.

* [ ] **Step 1: Create the fixture**

Create `tests/fixtures/weighted_events.json`:

```json
{
  "meta": {
    "interval": 1.0,
    "startTime": 0.0,
    "product": "weighted_events",
    "extra": [
      {
        "label": "Perf events",
        "entries": [
          {"label": "cycles", "format": "string", "value": "frequency 1000 Hz"},
          {"label": "cache-misses", "format": "string", "value": "frequency 1000 Hz"},
          {"label": "instructions", "format": "string", "value": "period 1000"},
          {"label": "Sample weight", "format": "string", "value": "period"}
        ]
      }
    ]
  },
  "libs": [{"name": "app", "debugName": "app", "path": "/app", "arch": "x86_64"}],
  "threads": [
    {
      "name": "Main",
      "tid": 1,
      "pid": 1,
      "processName": "Main",
      "registerTime": 0.0,
      "stringArray": ["hot", "cold", "cache-misses", "instructions", "/src/lib.rs", "app"],
      "frameTable": {"length": 2, "address": [256, 512], "func": [0, 1], "category": [0, 0], "subcategory": [0, 0], "line": [10, 20], "column": [null, null], "nativeSymbol": [null, null]},
      "funcTable": {"length": 2, "name": [0, 1], "isJS": [false, false], "relevantForJS": [false, false], "resource": [0, 0], "fileName": [4, 4], "lineNumber": [null, null], "columnNumber": [null, null]},
      "stackTable": {"length": 2, "frame": [0, 1], "prefix": [null, null]},
      "resourceTable": {"length": 1, "lib": [0], "name": [5], "host": [null], "type": [1]},
      "samples": {"length": 4, "stack": [0, 0, 1, 1], "time": [0.0, 1.0, 2.0, 3.0], "weight": [100, 100, 300, 300], "weightType": "samples"},
      "markers": {
        "length": 6,
        "data": [
          {"type": "Other event", "cause": {"stack": 0}, "period": 10},
          {"type": "Other event", "cause": {"stack": 0}, "period": 10},
          {"type": "Other event", "cause": {"stack": 1}, "period": 30},
          {"type": "Other event", "cause": {"stack": 1}, "period": 30},
          {"type": "Other event", "cause": {"stack": 0}},
          {"type": "Other event", "cause": {"stack": 1}}
        ],
        "name": [2, 2, 2, 2, 3, 3],
        "startTime": [0.0, 1.0, 2.0, 3.0, 0.0, 3.0],
        "endTime": [null, null, null, null, null, null],
        "phase": [0, 0, 0, 0, 0, 0],
        "category": [0, 0, 0, 0, 0, 0]
      }
    }
  ]
}
```

* [ ] **Step 2: Write the failing tests**

Add to `mod tests` in `src/profile/perf_events.rs`:

```rust
    #[test]
    fn sample_weight_reads_the_weight_column_only_with_period_weights() {
        let w = [5.0, -1.0, f64::NAN, 2.5];
        assert_eq!(sample_weight(Some(&w), 0, true), 5);
        assert_eq!(sample_weight(Some(&w), 1, true), 0);
        assert_eq!(sample_weight(Some(&w), 2, true), 0);
        assert_eq!(sample_weight(Some(&w), 3, true), 2);
        assert_eq!(sample_weight(Some(&w), 0, false), 1);
        assert_eq!(sample_weight(None, 0, true), 1);
        assert_eq!(sample_weight(Some(&w), 9, true), 1);
    }

    #[test]
    fn marker_weight_prefers_period_then_fixed_period_then_one() {
        assert_eq!(marker_weight(Some(7.0), Some(100)), 7);
        assert_eq!(marker_weight(None, Some(100)), 100);
        assert_eq!(marker_weight(None, None), 1);
        assert_eq!(marker_weight(Some(-3.0), None), 1);
        assert_eq!(marker_weight(Some(f64::INFINITY), Some(100)), 100);
        assert_eq!(marker_weight(Some(0.0), Some(100)), 0);
    }
```

Add to `mod tests` in `src/profile/parsed.rs`:

```rust
    fn weighted_events_raw() -> RawProfile {
        serde_json::from_str(include_str!("../../tests/fixtures/weighted_events.json")).unwrap()
    }

    fn first_handle(p: &Profile) -> ThreadHandle {
        p.threads().next().unwrap().handle()
    }

    #[test]
    fn samples_weigh_their_period_when_the_profile_says_so() {
        let p = Profile::from_raw(weighted_events_raw());
        let h = first_handle(&p);
        let items: Vec<_> = p.weighted_stack_indices(h, &EventSource::Samples, None).collect();
        assert_eq!(
            items,
            vec![(Some(0), 100), (Some(0), 100), (Some(1), 300), (Some(1), 300)]
        );
        assert!(p.is_weighted([h], &EventSource::Samples, None));
        assert_eq!(p.sample_weight(h, 2), 300);
    }

    #[test]
    fn samples_weigh_one_without_sample_weight_period() {
        let mut raw = weighted_events_raw();
        raw.meta.extra[0].entries.last_mut().unwrap().value = serde_json::json!("1");
        let p = Profile::from_raw(raw);
        let h = first_handle(&p);
        let weights: Vec<u64> = p
            .weighted_stack_indices(h, &EventSource::Samples, None)
            .map(|(_, w)| w)
            .collect();
        assert_eq!(weights, vec![1, 1, 1, 1]);
        assert!(!p.is_weighted([h], &EventSource::Samples, None));
    }

    #[test]
    fn markers_weigh_their_period() {
        let p = Profile::from_raw(weighted_events_raw());
        let h = first_handle(&p);
        let ev = EventSource::Marker("cache-misses".into());
        let items: Vec<_> = p.weighted_stack_indices(h, &ev, None).collect();
        assert_eq!(items, vec![(Some(0), 10), (Some(0), 10), (Some(1), 30), (Some(1), 30)]);
        assert!(p.is_weighted([h], &ev, None));
    }

    #[test]
    fn markers_without_period_weigh_their_fixed_period() {
        let p = Profile::from_raw(weighted_events_raw());
        let h = first_handle(&p);
        let ev = EventSource::Marker("instructions".into());
        let items: Vec<_> = p.weighted_stack_indices(h, &ev, None).collect();
        assert_eq!(items, vec![(Some(0), 1000), (Some(1), 1000)]);
        assert!(p.is_weighted([h], &ev, None));
    }

    #[test]
    fn markers_without_any_period_weigh_one() {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_events.json")).unwrap();
        let p = Profile::from_raw(raw);
        let h = first_handle(&p);
        let ev = EventSource::Marker("cache-misses".into());
        let items: Vec<_> = p.weighted_stack_indices(h, &ev, None).collect();
        assert_eq!(items, vec![(Some(0), 1), (Some(1), 1)]);
        assert!(!p.is_weighted([h], &ev, None));
        assert!(!p.is_weighted([h], &EventSource::Samples, None));
    }

    #[test]
    fn marker_weighting_follows_the_time_range() {
        let p = Profile::from_raw(weighted_events_raw());
        let h = first_handle(&p);
        let ev = EventSource::Marker("cache-misses".into());
        assert!(p.is_weighted([h], &ev, Some([0.0, 1.0])));
        // No cache-misses marker falls in this range, and the event has
        // no fixed period, so nothing selected is weighted.
        assert!(!p.is_weighted([h], &ev, Some([10.0, 20.0])));
    }

    #[test]
    fn stack_indices_drops_the_weights() {
        let p = Profile::from_raw(weighted_events_raw());
        let h = first_handle(&p);
        let items: Vec<_> = p.stack_indices(h, &EventSource::Samples, None).collect();
        assert_eq!(items, vec![Some(0), Some(0), Some(1), Some(1)]);
    }
```

* [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --lib profile::`
Expected: compile errors, `cannot find function sample_weight` and `no method named weighted_stack_indices`.

* [ ] **Step 4: Implement the weight functions**

Add to `src/profile/perf_events.rs` after the `impl PerfEvents` block:

```rust
/// A period or weight as an event count, when it is a finite non-negative
/// number. Fractions are truncated.
pub fn event_count(v: f64) -> Option<u64> {
    // No lossless f64 to u64 conversion exists. `as` truncates and
    // saturates above u64::MAX.
    (v.is_finite() && v >= 0.0).then(|| v as u64)
}

/// Weight of sample `i`: its `samples.weight` entry when the profile is
/// weighted by period, else 1. A negative or non-finite entry weighs 0,
/// and a missing entry weighs 1.
pub fn sample_weight(weights: Option<&[f64]>, i: usize, weight_by_period: bool) -> u64 {
    if !weight_by_period {
        return 1;
    }
    match weights.and_then(|w| w.get(i)) {
        Some(&w) => event_count(w).unwrap_or(0),
        None => 1,
    }
}

/// Weight of a marker: its `period` when valid, else its event's fixed
/// period, else 1.
pub fn marker_weight(period: Option<f64>, fixed_period: Option<u64>) -> u64 {
    period.and_then(event_count).or(fixed_period).unwrap_or(1)
}
```

* [ ] **Step 5: Replace the `stack_indices` block**

Replace the whole `impl Profile { pub fn stack_indices ... }` block in `src/profile/parsed.rs` (the second `impl Profile`, lines 490-605) with:

```rust
impl Profile {
    /// Like [`Self::weighted_stack_indices`], without the weights. For
    /// callers that count items rather than events.
    pub fn stack_indices<'a>(
        &'a self,
        handle: ThreadHandle,
        source: &'a EventSource,
        time_range: Option<[f64; 2]>,
    ) -> Box<dyn Iterator<Item = Option<usize>> + 'a> {
        Box::new(
            self.weighted_stack_indices(handle, source, time_range)
                .map(|(stack, _)| stack),
        )
    }

    /// Iterate the stack-table indices that this thread contributes for
    /// the given event source, each with its weight. `Some(idx)` per
    /// sample/marker, `None` to skip (matching the existing
    /// `samples.stack: Vec<Option<usize>>` shape so callers can stay in
    /// their per-stack loop).
    ///
    /// `time_range`, when set, gates each yielded item by its per-sample
    /// timestamp ([`crate::profile::raw::RawSampleTable::absolute_times`]
    /// for [`EventSource::Samples`], `markers.start_time`, or `end_time`
    /// for interval-end markers, for [`EventSource::Marker`]). The range
    /// is interpreted relative to
    /// [`Self::start_time_ms`] — i.e. profile-zero — to match the
    /// public filter contract; raw sample timestamps are offset before
    /// the comparison so callers never have to know whether the
    /// profile uses boot-relative or zero-anchored timestamps. Items
    /// outside the inclusive range are dropped entirely. Pass `None`
    /// for the unfiltered behavior.
    ///
    /// A sample weighs its `samples.weight` entry when the `Perf events`
    /// section says `Sample weight: period`, else 1. A marker weighs its
    /// `data.period`, else its event's fixed period from the `Perf events`
    /// section, else 1. See [`crate::profile::perf_events`].
    pub fn weighted_stack_indices<'a>(
        &'a self,
        handle: ThreadHandle,
        source: &'a EventSource,
        time_range: Option<[f64; 2]>,
    ) -> Box<dyn Iterator<Item = (Option<usize>, u64)> + 'a> {
        let raw = self.raw_thread(handle);
        match source {
            EventSource::Samples => {
                let in_range = self.time_filter(time_range);
                // Materialize absolute times once per thread; samply
                // emits either `time` directly or `timeDeltas`, and
                // `absolute_times` unifies the two.
                let times = raw.samples.absolute_times();
                let weights = raw.samples.weight.as_deref();
                let by_period = self.samples_weighted_by_period();
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
                            Some((s, perf_events::sample_weight(weights, i, by_period)))
                        }),
                )
            }
            EventSource::Marker(name) => {
                // Markers without a `cause.stack` payload are yielded as
                // `None` so the caller's "skip None" branch handles them
                // uniformly with samples that have no stack.
                let fixed = self.perf_events().and_then(|p| p.fixed_period(name));
                Box::new(self.marker_rows(handle, name, time_range).map(move |i| {
                    let data = raw.markers.data.get(i).and_then(|d| d.as_ref());
                    let stack = data.and_then(|d| d.cause.as_ref()).map(|c| c.stack);
                    let period = data.and_then(|d| d.period);
                    (stack, perf_events::marker_weight(period, fixed))
                }))
            }
        }
    }

    /// Whether the weights [`Self::weighted_stack_indices`] yields for
    /// `source` over these threads come from perf event periods. For
    /// samples: the `Perf events` section says `Sample weight: period`. For
    /// markers: the event has a fixed period, or a marker selected by the
    /// threads and `time_range` carries a valid `period`. Independent of
    /// the weight values.
    pub fn is_weighted(
        &self,
        handles: impl IntoIterator<Item = ThreadHandle>,
        source: &EventSource,
        time_range: Option<[f64; 2]>,
    ) -> bool {
        match source {
            EventSource::Samples => self.samples_weighted_by_period(),
            EventSource::Marker(name) => {
                if self
                    .perf_events()
                    .and_then(|p| p.fixed_period(name))
                    .is_some()
                {
                    return true;
                }
                handles.into_iter().any(|h| {
                    let raw = self.raw_thread(h);
                    self.marker_rows(h, name, time_range).any(|i| {
                        raw.markers
                            .data
                            .get(i)
                            .and_then(|d| d.as_ref())
                            .and_then(|d| d.period)
                            .and_then(perf_events::event_count)
                            .is_some()
                    })
                })
            }
        }
    }

    /// True when the `Perf events` section says `Sample weight: period`.
    pub fn samples_weighted_by_period(&self) -> bool {
        self.perf_events().is_some_and(|p| p.weight_by_period)
    }

    /// Weight of sample `i` of this thread, see
    /// [`Self::weighted_stack_indices`].
    pub fn sample_weight(&self, handle: ThreadHandle, i: usize) -> u64 {
        let raw = self.raw_thread(handle);
        perf_events::sample_weight(
            raw.samples.weight.as_deref(),
            i,
            self.samples_weighted_by_period(),
        )
    }

    /// Row indices of this thread's markers named `name` that pass
    /// `time_range`, with the gating [`Self::weighted_stack_indices`]
    /// documents.
    fn marker_rows<'a>(
        &'a self,
        handle: ThreadHandle,
        name: &str,
        time_range: Option<[f64; 2]>,
    ) -> Box<dyn Iterator<Item = usize> + 'a> {
        let raw = self.raw_thread(handle);
        let in_range = self.time_filter(time_range);
        // Resolve the marker name to its string index
        // *once* per call.
        let Some(target) = self.raw.shared.strings.position(name) else {
            return Box::new(std::iter::empty());
        };
        Box::new(
            raw.markers
                .name
                .iter()
                .enumerate()
                .filter_map(move |(i, &n)| {
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
                    // the unstamped-sample branch). Interval-end
                    // markers carry only an end time; gate them
                    // by that. With no range, a marker with
                    // neither time still matches. The unfiltered
                    // path never drops a marker for lacking a
                    // timestamp.
                    if time_range.is_some() {
                        let t = raw
                            .markers
                            .start_time
                            .get(i)
                            .copied()
                            .flatten()
                            .or_else(|| raw.markers.end_time.get(i).copied().flatten())?;
                        if !in_range(t) {
                            return None;
                        }
                    }
                    Some(i)
                }),
        )
    }

    /// Inclusive `time_range` test for a raw timestamp, relative to
    /// [`Self::start_time_ms`]. Always true without a range.
    fn time_filter(&self, time_range: Option<[f64; 2]>) -> impl Fn(f64) -> bool + Copy {
        let start = self.start_time_ms();
        // Closure copies the range so each branch's iterator can move
        // it freely without borrowing `time_range` itself. Sample times
        // are offset by `start` so a [s, e] filter behaves the same way
        // regardless of whether the profile's clock is boot-relative
        // (samply) or already zero-anchored (synthetic fixtures).
        move |t: f64| match time_range {
            None => true,
            Some([s, e]) => {
                let rel = t - start;
                rel >= s && rel <= e
            }
        }
    }
}
```

The moved comments keep their original text, including the em-dashes they already had.

* [ ] **Step 6: Run the tests**

Run: `cargo test`
Expected: all pass, including every existing `stack_indices` test in `parsed.rs` and `query/event.rs`.

* [ ] **Step 7: Format, lint, commit**

```bash
cd /home/moritz/dev/repos/pollard/.claude/worktrees/period-weighting
cargo fmt
cargo clippy --all-targets
git add src/profile/perf_events.rs src/profile/parsed.rs tests/fixtures/weighted_events.json
git commit -m "$(cat <<'EOF'
feat: weight samples and markers by perf event period

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_016aEc8uf7GZbHQHXmoX8o2k
EOF
)"
```

---

### Task 11: Weighted `top_functions`, `top_groups`, and `compare_profiles`

Repository: pollard, worktree `/home/moritz/dev/repos/pollard/.claude/worktrees/period-weighting`.

**Files:**
* Modify: `src/query/top_functions.rs:42-64` (`Output`), `:80-84` (`Counts`), `:86-185` (aggregators), `:187-244` (`top_functions`), tests
* Modify: `src/query/top_groups.rs:58-74` (`Output`), `:88-161` (`top_groups`), tests
* Modify: `src/query/compare.rs:116-140` (`Output`), `:167-171` (delta doc), `:213-357` (`compare_profiles`), tests
* Modify: `tests/snapshots/snapshot__top_functions_snapshot.snap` (insta only)

**Interfaces:**
* Consumes: `Profile::weighted_stack_indices`, `Profile::is_weighted` (Task 10), fixture `weighted_events.json`.
* Produces:
  * `pub(crate) struct Tally { pub(crate) samples: u64, pub(crate) weight: u64 }` with `add(&mut self, weight: u64)` and `merge(&mut self, other: Tally)`.
  * `pub(crate) struct Counts { pub(crate) self_: Tally, pub(crate) total: Tally }` (replaces `self_samples`/`total_samples`).
  * `aggregate_functions` and `aggregate_grouped` return `(HashMap<K, Counts>, Tally)`.
  * `top_functions::Output::weighted: bool`, `top_groups::Output::weighted: bool`.
  * `compare::Output::{weighted_a: bool, weighted_b: bool, note: Option<String>}`.

* [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/query/top_functions.rs`:

```rust
    fn weighted_events() -> Profile {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/weighted_events.json"))
                .unwrap();
        Profile::from_raw(raw)
    }

    #[test]
    fn weighted_samples_rank_by_period() {
        let out = top_functions(&weighted_events(), &Args::default()).unwrap();
        assert!(out.weighted);
        assert_eq!(out.total_samples, 800);
        assert_eq!(out.functions[0].function, "cold");
        assert_eq!(out.functions[0].self_samples, 600);
        assert!((out.functions[0].self_pct - 75.0).abs() < 1e-4);
        assert_eq!(out.functions[1].function, "hot");
        assert_eq!(out.functions[1].self_samples, 200);
    }

    #[test]
    fn marker_periods_weight_marker_events() {
        let out = top_functions(
            &weighted_events(),
            &Args {
                event: EventSource::Marker("cache-misses".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(out.weighted);
        assert_eq!(out.total_samples, 80);
        assert_eq!(out.functions[0].function, "cold");
        assert!((out.functions[0].self_pct - 75.0).abs() < 1e-4);
    }

    #[test]
    fn fixed_period_weights_markers_without_period() {
        let out = top_functions(
            &weighted_events(),
            &Args {
                event: EventSource::Marker("instructions".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(out.weighted);
        assert_eq!(out.total_samples, 2000);
    }

    #[test]
    fn unweighted_profile_reports_weighted_false() {
        let out = top_functions(&Profile::from_raw(raw_with_two_functions()), &Args::default())
            .unwrap();
        assert!(!out.weighted);
        assert_eq!(out.total_samples, 100);
    }
```

Add to `mod tests` in `src/query/top_groups.rs`:

```rust
    #[test]
    fn weighted_groups_follow_periods() {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/weighted_events.json"))
                .unwrap();
        let p = Profile::from_raw(raw);
        let out = top_groups(
            &p,
            &Args {
                group_by: GroupBy::Function,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(out.weighted);
        assert_eq!(out.total_samples, 800);
        assert_eq!(out.groups[0].key, "cold");
        assert_eq!(out.groups[0].self_samples, 600);
        assert!((out.groups[0].self_pct - 75.0).abs() < 1e-4);
    }
```

Add to `mod tests` in `src/query/compare.rs`:

```rust
    fn weighted_events_raw() -> RawProfile {
        serde_json::from_str(include_str!("../../tests/fixtures/weighted_events.json")).unwrap()
    }

    fn row<'a>(out: &'a Output, function: &str) -> &'a DiffEntry {
        out.functions
            .iter()
            .find(|r| r.function == function)
            .unwrap_or_else(|| panic!("no row for {function}"))
    }

    #[test]
    fn weighted_profile_shares_follow_periods_and_ms_follows_counts() {
        let p = Profile::from_raw(weighted_events_raw());
        let out = compare_profiles(&p, &p, &Args::default()).unwrap();
        assert!(out.weighted_a && out.weighted_b);
        assert!(out.note.is_none());
        assert_eq!(out.a_total_samples, 800);
        let (hot, cold) = (row(&out, "hot"), row(&out, "cold"));
        assert_eq!(hot.a_self_samples, 200);
        assert_eq!(cold.a_self_samples, 600);
        assert!((hot.a_self_pct - 25.0).abs() < 1e-4);
        assert!((cold.a_self_pct - 75.0).abs() < 1e-4);
        // `*_ms` counts samples: two samples each at a 1 ms interval.
        assert_eq!(hot.a_self_ms, Some(2.0));
        assert_eq!(cold.a_self_ms, Some(2.0));
    }

    #[test]
    fn mixed_weighting_adds_note() {
        let a = Profile::from_raw(weighted_events_raw());
        let mut raw_b = weighted_events_raw();
        raw_b.meta.extra.clear();
        let b = Profile::from_raw(raw_b);
        let out = compare_profiles(&a, &b, &Args::default()).unwrap();
        assert!(out.weighted_a);
        assert!(!out.weighted_b);
        let note = out.note.clone().expect("note when only one side is weighted");
        assert!(note.contains("not directly comparable"), "{note}");
        assert!(note.contains("profile A"), "{note}");
        assert_eq!(row(&out, "hot").b_self_samples, 2);
        assert_eq!(row(&out, "cold").a_self_samples, 600);
    }

    #[test]
    fn unweighted_profiles_have_no_note() {
        let p = two_functions();
        let out = compare_profiles(&p, &p, &Args::default()).unwrap();
        assert!(!out.weighted_a && !out.weighted_b);
        assert!(out.note.is_none());
    }
```

* [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib query::top_functions query::top_groups query::compare`
Expected: compile errors, `no field weighted`, `no field weighted_a`, `no field note`.

* [ ] **Step 3: Replace `Counts` and the aggregators**

In `src/query/top_functions.rs`, replace

```rust
#[derive(Default, Clone)]
pub(crate) struct Counts {
    pub(crate) self_samples: u64,
    pub(crate) total_samples: u64,
}
```

with

```rust
/// Items counted and their summed weight. Without period information every
/// weight is 1, so `weight == samples`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tally {
    pub(crate) samples: u64,
    pub(crate) weight: u64,
}

impl Tally {
    pub(crate) fn add(&mut self, weight: u64) {
        self.samples += 1;
        self.weight = self.weight.saturating_add(weight);
    }

    pub(crate) fn merge(&mut self, other: Tally) {
        self.samples += other.samples;
        self.weight = self.weight.saturating_add(other.weight);
    }
}

/// Per-key self and total tallies.
#[derive(Default, Clone)]
pub(crate) struct Counts {
    pub(crate) self_: Tally,
    pub(crate) total: Tally,
}
```

Change the doc of `aggregate_functions` from "Per-function `(self, total)` sample counts plus profile-wide total." to "Per-function `(self, total)` tallies plus the profile-wide tally.", and its return type to `Result<(HashMap<(String, Option<String>), Counts>, Tally), ToolError>`.

In `aggregate_grouped`, change the doc's first line from "Aggregate self/total sample counts under a caller-provided key extractor." to "Aggregate self/total tallies under a caller-provided key extractor. Each item adds its weight from [`Profile::weighted_stack_indices`]." (keep the rest), the return type to `Result<(HashMap<K, Counts>, Tally), ToolError>`, and the body from `let mut counts` to the end with:

```rust
    let mut counts: HashMap<K, Counts> = HashMap::new();
    let mut total = Tally::default();

    for handle in filter_args.threads(profile) {
        for (stack_opt, weight) in
            profile.weighted_stack_indices(handle, event, filter_args.time_range)
        {
            let Some(stack_idx) = stack_opt else { continue };
            total.add(weight);

            // resolved_chain returns root-to-leaf with view transforms
            // (hide / rename / collapse) and optional inline expansion
            // already applied. Reverse to leaf-to-root so the existing
            // self/total accounting — which credits self-time to the
            // first iterated frame — keeps working unchanged.
            let mut chain = profile.resolved_chain(handle, stack_idx, expand_inlines);
            chain.reverse();

            let mut iter = chain.into_iter();
            let mut seen_in_stack: std::collections::HashSet<K> = Default::default();
            if let Some(frame) = iter.next()
                && matcher.as_ref().is_none_or(|m| m.matches(&frame.function))
                && let Some(k) = key_fn(
                    &frame.function,
                    frame.module.as_deref(),
                    frame.file.as_deref(),
                )
            {
                let entry = counts.entry(k.clone()).or_default();
                entry.self_.add(weight);
                entry.total.add(weight);
                seen_in_stack.insert(k);
            }
            for frame in iter {
                if matcher.as_ref().is_none_or(|m| m.matches(&frame.function))
                    && let Some(k) = key_fn(
                        &frame.function,
                        frame.module.as_deref(),
                        frame.file.as_deref(),
                    )
                    && seen_in_stack.insert(k.clone())
                {
                    counts.entry(k).or_default().total.add(weight);
                }
            }
        }
    }

    Ok((counts, total))
}
```

* [ ] **Step 4: Update `top_functions`**

Add to `top_functions::Output` after `event`:

```rust
    /// True when counts and percentages sum perf event periods, so they
    /// count events such as cycles or cache misses rather than samples.
    /// Without period information every item weighs 1 and this is false.
    pub weighted: bool,
```

In `fn top_functions`, rename the binding `let (counts, total_samples) =` to `let (counts, total) =`, and change:

```rust
    let key = |c: &Counts| match args.sort_by {
        SortBy::SelfTime => c.self_.weight,
        SortBy::TotalTime => c.total.weight,
        SortBy::Descendants => c.total.weight.saturating_sub(c.self_.weight),
    };
```

```rust
    let total_f = total.weight.max(1) as f32;
```

In the `.map(...)` building `FunctionEntry`, use `c.self_.weight`, `c.total.weight`, and `total_f`:

```rust
        .map(|(i, ((function, module), c))| FunctionEntry {
            rank: i + 1,
            function,
            module,
            self_samples: c.self_.weight,
            self_pct: 100.0 * c.self_.weight as f32 / total_f,
            total_samples: c.total.weight,
            total_pct: 100.0 * c.total.weight as f32 / total_f,
        })
```

In the returned `Output`, set `total_samples: total.weight,` and add after `event`:

```rust
        weighted: profile.is_weighted(
            args.filter_args.threads(profile),
            &args.event,
            args.filter_args.time_range,
        ),
```

* [ ] **Step 5: Update `top_groups`**

Change the import to `use crate::query::top_functions::{Counts, Tally, aggregate_grouped};`. Add to `top_groups::Output` after `sort_by`:

```rust
    /// True when counts and percentages sum perf event periods rather than
    /// counting samples. See `top_functions`.
    pub weighted: bool,
```

In `fn top_groups`, change the binding to `let (counts, total): (HashMap<String, Counts>, Tally) = aggregate_grouped(`, and:

```rust
    let sort_key = |c: &Counts| match args.sort_by {
        SortBy::SelfTime => c.self_.weight,
        SortBy::TotalTime => c.total.weight,
        SortBy::Descendants => c.total.weight.saturating_sub(c.self_.weight),
    };
```

```rust
    let total_f = total.weight.max(1) as f32;
```

```rust
        .map(|(i, (key, c))| GroupEntry {
            rank: i + 1,
            key,
            self_samples: c.self_.weight,
            self_pct: 100.0 * c.self_.weight as f32 / total_f,
            total_samples: c.total.weight,
            total_pct: 100.0 * c.total.weight as f32 / total_f,
        })
```

In the returned `Output`, set `total_samples: total.weight,` and add after `sort_by: ...,`:

```rust
        weighted: profile.is_weighted(
            args.filter_args.threads(profile),
            &event,
            args.filter_args.time_range,
        ),
```

* [ ] **Step 6: Update `compare_profiles`**

Add to `compare::Output` after `event`:

```rust
    /// True when profile A's counts and percentages sum perf event periods
    /// rather than counting samples. See `top_functions`.
    pub weighted_a: bool,
    /// Same as [`Self::weighted_a`] for profile B.
    pub weighted_b: bool,
    /// Set when exactly one side is weighted: that side's percentages are
    /// event shares and the other's are sample shares.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
```

In `DiffEntry`, change the comment above `delta_self_samples` from

```rust
    /// Raw sample-count deltas. Less normalized than the pct deltas — useful
```

to

```rust
    /// Raw count deltas, event counts when `weighted_*` is set. Less normalized than the pct deltas — useful
```

(keep the following two comment lines).

In `fn compare_profiles`, replace the two join loops with:

```rust
    for ((function, module), c) in counts_a {
        let key = join_key(function, module, args.align_by);
        let slot = &mut joined.entry(key).or_default().0;
        slot.self_.merge(c.self_);
        slot.total.merge(c.total);
    }
    for ((function, module), c) in counts_b {
        let key = join_key(function, module, args.align_by);
        let slot = &mut joined.entry(key).or_default().1;
        slot.self_.merge(c.self_);
        slot.total.merge(c.total);
    }
```

Change the denominators to `let denom_a = total_a.weight.max(1) as f32;` and `let denom_b = total_b.weight.max(1) as f32;`, and replace the row `.map(...)` closure body with:

```rust
        .map(|((function, module), (ca, cb))| {
            let a_self_pct = 100.0 * ca.self_.weight as f32 / denom_a;
            let b_self_pct = 100.0 * cb.self_.weight as f32 / denom_b;
            let a_total_pct = 100.0 * ca.total.weight as f32 / denom_a;
            let b_total_pct = 100.0 * cb.total.weight as f32 / denom_b;
            let (a_self_ms, b_self_ms, a_total_ms, b_total_ms, delta_self_ms, delta_total_ms) =
                if time_shaped {
                    // An event count has no time unit, so the ms columns
                    // stay on sample counts even for weighted profiles.
                    let a_self = ca.self_.samples as f64 * interval_a;
                    let b_self = cb.self_.samples as f64 * interval_b;
                    let a_total = ca.total.samples as f64 * interval_a;
                    let b_total = cb.total.samples as f64 * interval_b;
                    (
                        Some(a_self),
                        Some(b_self),
                        Some(a_total),
                        Some(b_total),
                        Some(b_self - a_self),
                        Some(b_total - a_total),
                    )
                } else {
                    (None, None, None, None, None, None)
                };
            DiffEntry {
                rank: 0,
                function,
                module,
                a_self_samples: ca.self_.weight,
                a_self_pct,
                a_total_samples: ca.total.weight,
                a_total_pct,
                b_self_samples: cb.self_.weight,
                b_self_pct,
                b_total_samples: cb.total.weight,
                b_total_pct,
                delta_self_pct: b_self_pct - a_self_pct,
                delta_total_pct: b_total_pct - a_total_pct,
                delta_self_samples: cb.self_.weight as i64 - ca.self_.weight as i64,
                delta_total_samples: cb.total.weight as i64 - ca.total.weight as i64,
                a_self_ms,
                b_self_ms,
                a_total_ms,
                b_total_ms,
                delta_self_ms,
                delta_total_ms,
            }
        })
```

Before `Ok(Output { .. })`, add:

```rust
    let weighted_a = a.is_weighted(
        args.filter_args.threads(a),
        &args.event,
        args.filter_args.time_range,
    );
    let weighted_b = b.is_weighted(
        args.filter_args.threads(b),
        &args.event,
        args.filter_args.time_range,
    );
```

In `Output { .. }`, set `a_total_samples: total_a.weight,`, `b_total_samples: total_b.weight,`, and add after `event`:

```rust
        weighted_a,
        weighted_b,
        note: weighting_note(weighted_a, weighted_b),
```

Add after `fn sort_key`:

```rust
/// The mismatch note for [`Output::note`], `None` when both sides agree.
fn weighting_note(weighted_a: bool, weighted_b: bool) -> Option<String> {
    let (weighted, unweighted) = match (weighted_a, weighted_b) {
        (true, false) => ("A", "B"),
        (false, true) => ("B", "A"),
        _ => return None,
    };
    Some(format!(
        "profile {weighted}'s percentages are event shares weighted by perf event period, \
         profile {unweighted}'s are sample shares, so they are not directly comparable"
    ))
}
```

* [ ] **Step 7: Run the tests and accept the snapshot**

Run: `cargo test`
Expected: the new tests pass. `snapshot::top_functions_snapshot` fails because the output gains `weighted`.
Run: `INSTA_UPDATE=always cargo test --test snapshot`, then `git diff tests/snapshots`.
Expected: the only change is one added line `"weighted": false,` in `snapshot__top_functions_snapshot.snap`. If anything else changed, stop and investigate. Run `cargo test` again: all pass.

* [ ] **Step 8: Format, lint, commit**

```bash
cd /home/moritz/dev/repos/pollard/.claude/worktrees/period-weighting
cargo fmt
cargo clippy --all-targets
git add src/query/top_functions.rs src/query/top_groups.rs src/query/compare.rs tests/snapshots/snapshot__top_functions_snapshot.snap
git commit -m "$(cat <<'EOF'
feat: weight top_functions, top_groups, and compare_profiles by period

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_016aEc8uf7GZbHQHXmoX8o2k
EOF
)"
```

---

### Task 12: Weighted `call_tree`, `stacks_containing`, and `folded_stacks`

Repository: pollard, worktree `/home/moritz/dev/repos/pollard/.claude/worktrees/period-weighting`.

**Files:**
* Modify: `src/query/call_tree.rs:60-104` (`Output`), `:233-357` (`call_tree_inner`), `:366-428` (`accumulate_with_root`), tests
* Modify: `src/query/stacks_containing.rs:21-39` (`Output`), `:60-143`, tests
* Modify: `src/query/folded.rs:27-41` (`Folded`), `:67-122`, tests
* Modify: `src/tools/query.rs:278-289` (`FoldedStacksOutput`), `:565-582` (`folded_stacks` tool), `:700-794` (`fit_folded_to_budget`), tests `:813-883`

**Interfaces:**
* Consumes: `Profile::weighted_stack_indices`, `Profile::is_weighted` (Task 10).
* Produces: `call_tree::Output::weighted`, `stacks_containing::Output::weighted`, `folded::Folded::weighted`, `FoldedStacksOutput::weighted` (all `bool`). Existing `u64` count fields keep their names and hold weights.

* [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/query/call_tree.rs`:

```rust
    fn find_frame<'a>(node: &'a Node, name: &str) -> Option<&'a FrameNode> {
        match node {
            Node::Frame(f) if f.function == name => Some(f),
            Node::Frame(f) => f.children.iter().find_map(|c| find_frame(c, name)),
            _ => None,
        }
    }

    #[test]
    fn weighted_tree_follows_periods() {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/weighted_events.json"))
                .unwrap();
        let p = Profile::from_raw(raw);
        let out = call_tree(
            &p,
            &Args {
                min_pct: 0.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(out.weighted);
        assert_eq!(out.total_samples, 800);
        let tree = out.tree.as_ref().unwrap();
        let cold = find_frame(tree, "cold").unwrap();
        assert_eq!(cold.total_samples, 600);
        assert!((cold.total_pct - 75.0).abs() < 1e-4);
    }
```

Add to `mod tests` in `src/query/stacks_containing.rs`:

```rust
    #[test]
    fn weighted_stacks_follow_periods() {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/weighted_events.json"))
                .unwrap();
        let profile = Profile::from_raw(raw);
        let out = stacks_containing(
            &profile,
            &Args {
                function: "cold".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(out.weighted);
        assert_eq!(out.matched_frame_samples, 600);
        assert_eq!(out.stacks[0].samples, 600);
        assert!((out.matched_pct - 75.0).abs() < 1e-4);
    }
```

In `src/query/folded.rs` tests, add an arm to the `fixture` match:

```rust
            "weighted_events" => {
                serde_json::from_str(include_str!("../../tests/fixtures/weighted_events.json"))
                    .unwrap()
            }
```

and the test:

```rust
    #[test]
    fn weighted_lines_sum_periods() {
        let p = fixture("weighted_events");
        let folded = folded_stacks_structured(&p, &Args::default()).unwrap();
        assert!(folded.weighted);
        assert_eq!(folded.total_samples, 800);
        assert_eq!(folded.render(), "cold 600\nhot 200\n");
    }

    #[test]
    fn unweighted_folded_reports_weighted_false() {
        let p = fixture("two_functions");
        assert!(!folded_stacks_structured(&p, &Args::default()).unwrap().weighted);
    }
```

* [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib query::call_tree query::stacks_containing query::folded`
Expected: compile errors, `no field weighted`.

* [ ] **Step 3: Update `call_tree`**

Add to `call_tree::Output` after `event`:

```rust
    /// True when counts and percentages sum perf event periods rather than
    /// counting samples. See `top_functions`.
    pub weighted: bool,
```

In `accumulate_with_root`, change the loop header to

```rust
    for (stack_opt, weight) in profile.weighted_stack_indices(handle, event, time_range) {
```

and replace the accumulation at the end of the loop body with:

```rust
        *total_samples += weight;
        let mut node: &mut AggNode = root;
        let len = frames.len();
        for (i, (function, module)) in frames.iter().enumerate() {
            let key = (function.clone(), module.clone());
            node = node.children.entry(key).or_default();
            node.total_samples += weight;
            if i + 1 == len {
                node.self_samples += weight;
            }
        }
```

In `call_tree_inner`, add to the returned `Output` after `event: ...,`:

```rust
        weighted: profile.is_weighted(
            args.filter_args.threads(profile),
            &args.event,
            args.filter_args.time_range,
        ),
```

* [ ] **Step 4: Update `stacks_containing`**

Add to `stacks_containing::Output` after `matched_pct`:

```rust
    /// True when `matched_frame_samples` and the per-stack `samples` sum
    /// perf event periods rather than counting samples. See `top_functions`.
    pub weighted: bool,
```

Change the loop to

```rust
        for (stack_opt, weight) in profile.weighted_stack_indices(
            handle,
            &EventSource::Samples,
            args.filter_args.time_range,
        ) {
            let Some(stack_idx) = stack_opt else { continue };
            total_samples += weight;
```

and inside `if any_match { ... }`:

```rust
            if any_match {
                matched_frame_samples += weight;
                *counts.entry(frames).or_default() += weight;
            }
```

In the returned `Output`, add after `matched_pct: ...,`:

```rust
        weighted: profile.is_weighted(
            args.filter_args.threads(profile),
            &EventSource::Samples,
            args.filter_args.time_range,
        ),
```

* [ ] **Step 5: Update `folded_stacks`**

Add to `pub struct Folded` after `total_samples`:

```rust
    /// True when the per-line counts sum perf event periods rather than
    /// counting samples.
    pub weighted: bool,
```

Change the loop in `folded_stacks_structured` to

```rust
        for (stack_opt, weight) in profile.weighted_stack_indices(
            handle,
            &EventSource::Samples,
            args.filter_args.time_range,
        ) {
```

and its last two lines to

```rust
            *counts.entry(frames.join(";")).or_default() += weight;
            total_samples += weight;
```

Change the returned value to

```rust
    Ok(Folded {
        entries,
        total_samples,
        weighted: profile.is_weighted(
            args.filter_args.threads(profile),
            &EventSource::Samples,
            args.filter_args.time_range,
        ),
    })
```

* [ ] **Step 6: Carry `weighted` through the tool**

In `src/tools/query.rs`, add to `FoldedStacksOutput` after `folded`:

```rust
    /// True when the counts in `folded` sum perf event periods rather than
    /// counting samples. The folded text has no slot for this.
    pub weighted: bool,
```

In the `folded_stacks` tool method, replace

```rust
        let folded = folded::folded_stacks_structured(session.profile(), &q_args)?;
        let budget = resolve_budget(output_budget_bytes(), args.max_output_bytes);
        let (rendered, truncated) = fit_folded_to_budget(folded, budget);
        Ok(Json(FoldedStacksOutput {
            folded: rendered,
            truncated,
        }))
```

with

```rust
        let folded = folded::folded_stacks_structured(session.profile(), &q_args)?;
        let weighted = folded.weighted;
        let budget = resolve_budget(output_budget_bytes(), args.max_output_bytes);
        let (rendered, truncated) = fit_folded_to_budget(folded, budget);
        Ok(Json(FoldedStacksOutput {
            folded: rendered,
            weighted,
            truncated,
        }))
```

In `fit_folded_to_budget`, add `weighted: folded.weighted,` after `folded: String::new(),` in `envelope_with_truncated` and after `folded: rendered.clone(),` in `final_response`.

In the three test literals `folded::Folded { entries: ..., total_samples: ... }` in the same file, add `weighted: false,` after `total_samples`.

* [ ] **Step 7: Run the tests**

Run: `cargo test`
Expected: all pass. No snapshot covers these outputs.

* [ ] **Step 8: Format, lint, commit**

```bash
cd /home/moritz/dev/repos/pollard/.claude/worktrees/period-weighting
cargo fmt
cargo clippy --all-targets
git add src/query/call_tree.rs src/query/stacks_containing.rs src/query/folded.rs src/tools/query.rs
git commit -m "$(cat <<'EOF'
feat: weight call_tree, stacks_containing, and folded_stacks by period

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_016aEc8uf7GZbHQHXmoX8o2k
EOF
)"
```

---

### Task 13: Weighted `source_for_function`, `asm_for_function`, and `compare_functions`

Repository: pollard, worktree `/home/moritz/dev/repos/pollard/.claude/worktrees/period-weighting`.

**Files:**
* Modify: `src/query/source.rs:61-77` (`SourceListing`), `:155-189` (`record_match`), `:190-~300` (`attribute` loop), `:469-478` (`build_listing` result), tests
* Modify: `src/query/asm.rs:41-54` (`AsmListing`), `:116-181` (`resolve_function` loop), `:566-574`, tests
* Modify: `src/query/compare_functions.rs:33-48` (`Output`), `:125-134`

**Interfaces:**
* Consumes: `Profile::sample_weight`, `Profile::samples_weighted_by_period` (Task 10).
* Produces: `SourceListing::weighted`, `AsmListing::weighted`, `compare_functions::Output::{weighted_a, weighted_b}` (all `bool`). `samples` fields hold weights. Both tools keep iterating `samples.stack` directly, so their event and time-range behavior does not change.

* [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/query/source.rs`:

```rust
    #[test]
    fn weighted_samples_attribute_periods_to_lines() {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/weighted_events.json"))
                .unwrap();
        let profile = Profile::from_raw(raw);
        let content: String = (1..=25).map(|i| format!("line {i}\n")).collect();
        let listing = build_listing(
            &profile,
            "cold",
            None,
            ResolvedSource {
                file: "/src/lib.rs".to_owned(),
                language: None,
                content,
            },
            true,
            true,
            false,
        )
        .unwrap();
        assert!(listing.weighted);
        assert_eq!(listing.total_function_samples, 600);
        let line = listing.lines.iter().find(|l| l.line == 20).unwrap();
        assert_eq!(line.samples, 600);
    }
```

Add to `mod tests` in `src/query/asm.rs`:

```rust
    #[test]
    fn weighted_samples_attribute_periods_to_addresses() {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/weighted_events.json"))
                .unwrap();
        let profile = Profile::from_raw(raw);
        let matcher = FunctionMatcher::new("cold").unwrap();
        match resolve_function(&profile, &matcher, None) {
            ResolveResult::Single(loc) => {
                assert_eq!(loc.frame_counts.get(&512), Some(&600));
            }
            other => panic!("expected Single, got {other:?}"),
        }
    }
```

* [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib query::source query::asm`
Expected: `source` fails to compile (`no field weighted`); after that is fixed, the `asm` test fails with `left: Some(2), right: Some(600)`.

* [ ] **Step 3: Weight `source_for_function`**

Add to `SourceListing` after `total_function_samples`:

```rust
    /// True when `total_function_samples` and each line's `samples` sum
    /// perf event periods rather than counting samples.
    pub weighted: bool,
```

Add a last parameter `weight: u64,` to `record_match` (after `total: &mut u64,`), and change its final block to:

```rust
    if let Some(line) = frame_line {
        *samples_per_line.entry(line).or_default() += weight;
        *total += weight;
    }
```

In `attribute`, change the sample loop header to

```rust
        for (sample_idx, &stack_opt) in raw.samples.stack.iter().enumerate() {
            let Some(stack_idx) = stack_opt else { continue };
            let weight = profile.sample_weight(handle, sample_idx);
```

and pass `weight,` as the new last argument at both `record_match(...)` call sites (after `&mut total,`).

In `build_listing`, add `weighted: profile.samples_weighted_by_period(),` after `total_function_samples: total,` in `SourceListing { .. }`.

* [ ] **Step 4: Weight `asm_for_function`**

Add to `AsmListing` after `instructions`:

```rust
    /// True when each instruction's `samples` sums perf event periods
    /// rather than counting samples.
    pub weighted: bool,
```

In `resolve_function`, change the sample loop header to

```rust
        for (sample_idx, &stack_opt) in raw.samples.stack.iter().enumerate() {
            let Some(stack_idx) = stack_opt else { continue };
            let weight = profile.sample_weight(handle, sample_idx);
```

and the last statement of the frame loop to

```rust
                *frame_counts.entry(rel_addr).or_default() += weight;
```

The `matched_pairs` count stays `+= 1`: it ranks ambiguity candidates by occurrences.

In `asm_for_function_inner`, add `weighted: profile.samples_weighted_by_period(),` after `instructions,` in `AsmListing { .. }`.

* [ ] **Step 5: Report both sides in `compare_functions`**

Add to `compare_functions::Output` after `total_samples_b`:

```rust
    /// True when side A's `samples` sum perf event periods rather than
    /// counting samples.
    pub weighted_a: bool,
    /// Same as [`Self::weighted_a`] for side B.
    pub weighted_b: bool,
```

In `compare_functions`, add after `total_samples_b,` in `Output { .. }`:

```rust
        weighted_a: listing_a.weighted,
        weighted_b: listing_b.weighted,
```

Move these two lines above `function_a: listing_a.function,` if the borrow checker reports a use after partial move (both fields are `bool`, so either order compiles).

* [ ] **Step 6: Run the tests**

Run: `cargo test`
Expected: all pass.

* [ ] **Step 7: Format, lint, commit**

```bash
cd /home/moritz/dev/repos/pollard/.claude/worktrees/period-weighting
cargo fmt
cargo clippy --all-targets
git add src/query/source.rs src/query/asm.rs src/query/compare_functions.rs
git commit -m "$(cat <<'EOF'
feat: weight source, asm, and compare_functions by period

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_016aEc8uf7GZbHQHXmoX8o2k
EOF
)"
```

---

### Task 14: Weighted `summary` rankings

Repository: pollard, worktree `/home/moritz/dev/repos/pollard/.claude/worktrees/period-weighting`.

**Files:**
* Modify: `src/query/summary.rs:30-94` (`Output`), `:207-228` (result), `:519-558` (`compute_top_modules`), tests
* Modify: `tests/snapshots/snapshot__summary_snapshot.snap` (insta only)

**Interfaces:**
* Consumes: `top_functions::Output::weighted` (Task 11), `Profile::weighted_stack_indices` (Task 10).
* Produces: `summary::Output::weighted: bool`. `top_modules`, `top_self_functions`, and `top_total_functions` rank by weight. `total_samples`, `dominant_thread`, `top_processes`, and `top_threads` keep counting samples. `function_recurs_in_any_stack` keeps calling the unweighted `stack_indices`.

* [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/query/summary.rs`:

```rust
    #[test]
    fn weighted_rankings_follow_periods_and_counts_stay_raw() {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/weighted_events.json"))
                .unwrap();
        let profile = Profile::from_raw(raw);
        let s = summary(&profile, "id", "name", "/tmp/p.json", 0.0, Filter::default()).unwrap();
        assert!(s.weighted);
        assert_eq!(s.total_samples, 4);
        assert_eq!(s.top_threads[0].samples, 4);
        assert_eq!(s.top_self_functions[0].function, "cold");
        assert_eq!(s.top_self_functions[0].self_samples, 600);
        assert_eq!(s.top_modules[0].module, "app");
        assert_eq!(s.top_modules[0].total_samples, 800);
    }
```

* [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib query::summary`
Expected: compile error, `no field weighted`.

* [ ] **Step 3: Implement**

Add to `summary::Output` after `events`:

```rust
    /// True when `top_modules`, `top_self_functions`, and
    /// `top_total_functions` sum perf event periods rather than counting
    /// samples. `total_samples` and the per-thread and per-process counts
    /// always count samples.
    pub weighted: bool,
```

In `fn summary`, add after `events: crate::query::event::list_events(profile),`:

```rust
        weighted: by_self.weighted,
```

In `compute_top_modules`, replace the loop with:

```rust
    for handle in filter.threads(profile) {
        for (stack_opt, weight) in
            profile.weighted_stack_indices(handle, &EventSource::Samples, filter.time_range)
        {
            let Some(stack_idx) = stack_opt else { continue };
            total_samples += weight;
            // Each sample adds its weight to every module appearing at
            // least once on its stack, the same semantics as
            // `total_samples` in `top_functions`.
            let mut seen: HashSet<String> = HashSet::new();
            for frame_idx in profile.walk_stack(handle, stack_idx) {
                let Some(info) = profile.frame_info(handle, frame_idx) else {
                    continue;
                };
                let Some(module) = info.module_name else {
                    continue;
                };
                if seen.insert(module.to_owned()) {
                    *counts.entry(module.to_owned()).or_default() += weight;
                }
            }
        }
    }
```

The rewritten comment replaces "Each sample contributes 1 to every module appearing at least once on its stack — same semantics as `total_samples` in `top_functions`." with the same statement for weights.

* [ ] **Step 4: Run the tests and accept the snapshot**

Run: `cargo test`
Expected: the new test passes. `snapshot::summary_snapshot` fails because the output gains `weighted`.
Run: `INSTA_UPDATE=always cargo test --test snapshot`, then `git diff tests/snapshots`.
Expected: the only change is one added line `"weighted": false,` in `snapshot__summary_snapshot.snap`. Run `cargo test` again: all pass.

* [ ] **Step 5: Format, lint, commit**

```bash
cd /home/moritz/dev/repos/pollard/.claude/worktrees/period-weighting
cargo fmt
cargo clippy --all-targets
git add src/query/summary.rs tests/snapshots/snapshot__summary_snapshot.snap
git commit -m "$(cat <<'EOF'
feat: weight summary rankings by period

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_016aEc8uf7GZbHQHXmoX8o2k
EOF
)"
```

---

### Task 15: Event names and sampling in `list_events`

Repository: pollard, worktree `/home/moritz/dev/repos/pollard/.claude/worktrees/period-weighting`.

**Files:**
* Modify: `src/query/event.rs:105-120` (`EventInfo`), `:122-162` (`list_events`), tests

**Interfaces:**
* Consumes: `Profile::perf_events`, `PerfEvents::{main_event, event}`, `Sampling::describe` (Task 9).
* Produces: `EventInfo::event: Option<String>` (samples entry only) and `EventInfo::sampling: Option<String>`, both omitted from JSON when `None`. `describe_profile` and `summary` show them through `events`.

* [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/query/event.rs`:

```rust
    #[test]
    fn list_events_reads_the_perf_events_section() {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/weighted_events.json"))
                .unwrap();
        let p = Profile::from_raw(raw);
        let events = list_events(&p);
        assert_eq!(events[0].name, "samples");
        assert_eq!(events[0].event.as_deref(), Some("cycles"));
        assert_eq!(events[0].sampling.as_deref(), Some("frequency 1000 Hz"));
        let cm = events.iter().find(|e| e.name == "cache-misses").unwrap();
        assert_eq!(cm.event, None);
        assert_eq!(cm.sampling.as_deref(), Some("frequency 1000 Hz"));
        assert_eq!(cm.count, 4);
        let ins = events.iter().find(|e| e.name == "instructions").unwrap();
        assert_eq!(ins.sampling.as_deref(), Some("period 1000"));
    }

    #[test]
    fn list_events_without_section_omits_event_and_sampling() {
        let events = list_events(&fixture());
        assert_eq!(events[0].event, None);
        assert_eq!(events[0].sampling, None);
        let json = serde_json::to_value(&events[0]).unwrap();
        assert!(json.get("event").is_none(), "{json}");
        assert!(json.get("sampling").is_none(), "{json}");
    }
```

* [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib query::event`
Expected: compile error, `no field event on EventInfo`.

* [ ] **Step 3: Implement**

In `EventInfo`, change the doc of `name` from

```rust
    /// `"samples"` for the samples track, else the marker name, e.g.
    /// `"cache-misses"`. The samples track's real event name (such as
    /// cycles) is not recorded in the profile.
```

to

```rust
    /// `"samples"` for the samples track, else the marker name, e.g.
    /// `"cache-misses"`. The samples track's real event name (such as
    /// cycles) is in `event` when the profile records it.
```

and add after `stackless`:

```rust
    /// For the samples track: the main perf event's name as samply
    /// recorded it, e.g. `"cycles:u"`. Absent without samply's
    /// `Perf events` section.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    /// How perf sampled this event, `"frequency N Hz"` or `"period N"`.
    /// Absent when the profile does not say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling: Option<String>,
```

In `list_events`, replace everything from `let strings = &profile.shared().strings;` to the end of the function with:

```rust
    let strings = &profile.shared().strings;
    let perf = profile.perf_events();
    // Equally named events share the first entry's sampling.
    let sampling_of = |label: &str| {
        perf.and_then(|p| p.event(label))
            .and_then(|e| e.sampling)
            .map(|s| s.describe())
    };
    let mut marker_events: Vec<EventInfo> = markers
        .into_iter()
        .filter_map(|(idx, (count, has_stack))| {
            let name = strings.get(idx)?.to_owned();
            Some(EventInfo {
                sampling: sampling_of(&name),
                name,
                source: "marker",
                count,
                stackless: Some(!has_stack),
                event: None,
            })
        })
        .collect();
    marker_events.sort_by(|a, b| a.name.cmp(&b.name));
    let main = perf.and_then(|p| p.main_event());
    let mut events = vec![EventInfo {
        name: "samples".to_owned(),
        source: "samples",
        count: samples,
        stackless: None,
        event: main.map(|e| e.label.clone()),
        sampling: main.and_then(|e| e.sampling).map(|s| s.describe()),
    }];
    events.extend(marker_events);
    events
}
```

* [ ] **Step 4: Run the tests**

Run: `cargo test`
Expected: all pass. `snapshot::describe_snapshot` and `snapshot::summary_snapshot` stay unchanged because `tiny.json.gz` has no `Perf events` section.

* [ ] **Step 5: Format, lint, commit**

```bash
cd /home/moritz/dev/repos/pollard/.claude/worktrees/period-weighting
cargo fmt
cargo clippy --all-targets
git add src/query/event.rs
git commit -m "$(cat <<'EOF'
feat: name the main perf event and each event's sampling in list_events

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_016aEc8uf7GZbHQHXmoX8o2k
EOF
)"
```

---

### Task 16: Real recording with periods (optional)

Repository: pollard, worktree `/home/moritz/dev/repos/pollard/.claude/worktrees/period-weighting`. Needs perf with hardware counters, samply 0.13.1 at `~/.cargo/bin/samply`, the upstream baseline `$S/samply-base` (Task 1 Step 1), and the pull request 1 build. Skip this task if any of them is unavailable. The rest of the plan does not depend on it.

**Files:**
* Modify: `tests/fixtures/perf/regenerate.sh`
* Modify (regenerated): `tests/fixtures/perf/multi_v49.json.gz`, `multi_v75.json.gz`, `multi_v75.jslb.gz`
* Create: `tests/fixtures/perf/multi_period.json.gz`
* Modify: `tests/perf_formats.rs`

**Interfaces:**
* Consumes: `list_events` (`event`, `sampling`, `count`), `top_functions::Output::{weighted, total_samples}`.
* Produces: `multi_period.json.gz`, a flagged import of the same recording as the other three fixtures.

* [ ] **Step 1: Extend the regeneration script**

In `tests/fixtures/perf/regenerate.sh`, change the header comment and the checks to:

```bash
# Regenerate the multi-event perf fixtures from one recording.
#
# Needs perf with hardware counters, samply 0.13.1 (emits version 49),
# a samply build from main without period support (emits version 75),
# and a samply build with `--weight-by-period` (samply pull request
# "Record perf event periods in samply import", branch import-period):
#   SAMPLY_V49=/path/to/samply-0.13.1 SAMPLY_V75=/path/to/samply-main \
#   SAMPLY_PERIOD=/path/to/samply-import-period ./regenerate.sh
set -euo pipefail
cd "$(dirname "$0")"
: "${SAMPLY_V49:?set SAMPLY_V49 to a samply 0.13.1 binary}"
: "${SAMPLY_V75:?set SAMPLY_V75 to a samply main binary}"
: "${SAMPLY_PERIOD:?set SAMPLY_PERIOD to a samply binary with --weight-by-period}"
```

After the `"$SAMPLY_V75" import ... multi_v75.jslb.gz` line add:

```bash
"$SAMPLY_PERIOD" import "$work/multi.data" -s --weight-by-period -o "$work/multi_period.json.gz"
```

and change the scrub loop header to

```bash
for f in multi_v49.json.gz multi_v75.json.gz multi_v75.jslb.gz multi_period.json.gz; do
```

* [ ] **Step 2: Regenerate**

```bash
cd /home/moritz/dev/repos/samply/.claude/worktrees/import-period && cargo build -p samply
cd /home/moritz/dev/repos/pollard/.claude/worktrees/period-weighting
SAMPLY_V49="$HOME/.cargo/bin/samply" \
SAMPLY_V75=/tmp/claude-1000/-home-moritz-dev-repos-samply/c426bfa0-d211-4428-9e82-e5b3db072f8b/scratchpad/samply-base \
SAMPLY_PERIOD=/home/moritz/dev/repos/samply/.claude/worktrees/import-period/target/debug/samply \
  tests/fixtures/perf/regenerate.sh
cargo test --test perf_formats
```

Expected: the two existing `perf_formats` tests pass on the regenerated fixtures.

* [ ] **Step 3: Write the test**

In `tests/perf_formats.rs`, extend the module doc with:

```rust
//! `multi_period.json.gz` is a `--weight-by-period` import of the same
//! recording, so its item counts match the others and its weights exceed them.
```

and append:

```rust
#[tokio::test]
async fn period_fixture_is_weighted_by_period() {
    let period = load("multi_period.json.gz").await;
    let v75 = load("multi_v75.json.gz").await;

    let events = pollard::query::event::list_events(&period);
    assert_eq!(events[0].event.as_deref(), Some("cycles"));
    for e in &events {
        assert_eq!(e.sampling.as_deref(), Some("frequency 999 Hz"), "{e:?}");
    }
    let plain = pollard::query::event::list_events(&v75);
    for (a, b) in events.iter().zip(plain.iter()) {
        assert_eq!((&a.name, a.count), (&b.name, b.count));
    }

    for ev in event_sources() {
        let out = top_functions(
            &period,
            &top_functions::Args {
                event: ev.clone(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(out.weighted, "{ev:?}");
        assert!(
            out.total_samples > total(&v75, ev.clone()),
            "period weights must exceed counts for {ev:?}"
        );
    }
}
```

* [ ] **Step 4: Run the tests**

Run: `cargo test --test perf_formats`
Expected: three tests pass.

* [ ] **Step 5: Commit**

```bash
cd /home/moritz/dev/repos/pollard/.claude/worktrees/period-weighting
git add tests/fixtures/perf/regenerate.sh tests/fixtures/perf/multi_v49.json.gz tests/fixtures/perf/multi_v75.json.gz tests/fixtures/perf/multi_v75.jslb.gz tests/fixtures/perf/multi_period.json.gz tests/perf_formats.rs
git commit -m "$(cat <<'EOF'
test: check a period-weighted perf recording end to end

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_016aEc8uf7GZbHQHXmoX8o2k
EOF
)"
```

---

### Task 17: pollard docs

Repository: pollard, worktree `/home/moritz/dev/repos/pollard/.claude/worktrees/period-weighting`.

**Files:**
* Modify: `skills/profile-recording/SKILL.md:128-144` (`## Recording other perf events`)
* Modify: `README.md:24-30` (event paragraph)
* Modify: `CHANGELOG.md:8-20` (`## [Unreleased]`)

**Interfaces:**
* Consumes: behavior from Tasks 9 through 15.
* Produces: user-facing docs.

* [ ] **Step 1: Update the recording skill**

In `skills/profile-recording/SKILL.md`, change the import line of the `## Recording other perf events` code block to

```sh
samply import perf.data --weight-by-period --save-only -o /tmp/profile.json.gz
```

and replace the two lines

```markdown
Use a fixed period (`-c N`) instead of a frequency (`-F`) when comparing counts across events.
In frequency mode perf varies the period per sample, and pollard counts samples without weighting them by period.
```

with

```markdown
Pass `--weight-by-period` to `samply import` so the samples track counts events instead of samples.
samply also records each marker's period, so pollard weights every event by its period and reports `weighted: true` in tool outputs.
This makes frequency mode (`-F`) and fixed periods (`-c N`) equally usable.
`--weight-by-period` needs a samply build that includes it; samply 0.13.1 does not.
Without it, pollard counts samples of the first event, which in frequency mode biases shares toward code that ran while the period was small.
`describe_profile` names the first event and how each event was sampled.
```

* [ ] **Step 2: Update the README**

In `README.md`, after the line "`describe_profile` lists the events a profile contains." add:

```markdown
When samply recorded perf event periods (`samply import --weight-by-period`), the query and drill-down tools add periods instead of counting samples, so their counts and percentages mean events such as cycles or cache misses.
Their outputs report `weighted: true` in that case, and `compare_profiles` adds a `note` when only one side is weighted.
```

* [ ] **Step 3: Update the changelog**

In `CHANGELOG.md`, append to the `### Added` list under `## [Unreleased]`:

```markdown
* Weight `top_functions`, `top_groups`, `call_tree`, `stacks_containing`, `folded_stacks`, `compare_profiles`, `compare_functions`, `source_for_function`, `asm_for_function`, and the `summary` rankings by perf event period when samply recorded periods. Outputs report `weighted`.
* `describe_profile` and `summary` name the main perf event and each event's sampling mode.
```

* [ ] **Step 4: Commit**

```bash
cd /home/moritz/dev/repos/pollard/.claude/worktrees/period-weighting
git add skills/profile-recording/SKILL.md README.md CHANGELOG.md
git commit -m "$(cat <<'EOF'
docs: document period weighting and samply import --weight-by-period

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_016aEc8uf7GZbHQHXmoX8o2k
EOF
)"
```

---

## Self-review

* Spec coverage: `add_extra_info_section` (Task 1), `Perf events` section and grammar (Task 4), marker `period` with fixed fallback (Task 2), `--weight-by-period` with saturation warning and off-CPU weight 0 (Task 3), `sample_weight` and `fixed_periods` unit tests (Tasks 2, 3), re-import checks against `perf script` (Task 5), clock-only CPU delta with `main_event_is_clock` test and `-F cycles` check (Tasks 7, 8), pollard `RawMeta::extra` and `perf_events()` (Task 9), `weighted_stack_indices` with `stack_indices` wrapper and weight lookup (Task 10), weighted tools and `weighted` flags (Tasks 11 to 14), `Counts { samples, weight }` with `_ms` on samples (Task 11), `list_events` `event` and `sampling` (Task 15), docs (Tasks 6, 17). `view_stats`, `describe_profile`, per-thread counts, and `function_recurs_in_any_stack` stay on unweighted counts because no task changes them.
* Placeholders: none. Every code step shows the code.
* Type consistency: `Tally`/`Counts::{self_, total}` (Task 11) are the only accumulator names used later. `weighted_stack_indices`, `is_weighted`, `sample_weight`, `samples_weighted_by_period` (Task 10) match their uses in Tasks 11 to 14. `PerfEvents::{main_event, event, fixed_period}` and `Sampling::describe` (Task 9) match Task 15.
* Review focus: each of the five lines has a named test in its owning task.

## Spec discrepancies

* The spec grammar has no value for non-sampling attributes (for example group members recorded with `:S`), yet asks for one entry per attribute. Task 4 writes `no sampling`, which pollard ignores as outside the grammar.
* The spec does not say what a marker's `period` is when neither the record nor the attribute has one. Marker number fields cannot be omitted and the JSON writer rejects NaN, so Task 2 writes 0. pollard then weighs such a marker 0 under the spec's rules.
* The spec's error rules say an invalid marker `period` makes the item weigh 1, while its lookup rule says period, then fixed period, then 1. Task 10 follows the lookup chain, so an invalid period falls through to the fixed period when one exists.
* The spec gives each weighted tool a single `weighted: bool`, but `compare_functions` has two sides. Task 13 reports `weighted_a` and `weighted_b`, like `compare_profiles`.
* "All existing tests pass unchanged" holds for assertions only: three `Folded` literals in `src/tools/query.rs` tests gain `weighted: false` (Task 12), and the `top_functions` and `summary` insta snapshots gain `"weighted": false` (Tasks 11, 14).
