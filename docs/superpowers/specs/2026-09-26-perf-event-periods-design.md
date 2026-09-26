# Perf event periods from samply to pollard

## Problem

pollard counts every sample and every `Other event` marker as 1, because samply drops the perf event period when it imports `perf.data`.
In frequency mode (`perf record -F`), perf adjusts the period per sample, so equal counts do not mean equal event totals, and count shares are biased toward code that ran while the period was small.
The profile also does not name the event behind the samples track, so pollard can only call it `samples`.
This spec covers sub-project B: samply records periods and event metadata, and pollard weights by them.
It builds on sub-project A (`2026-09-25-shared-tables-format-design.md`), which makes pollard read samply's current output.

## Evidence

The findings below come from `samply import` of `perf record -e cycles,cache-misses,instructions,branch-misses` recordings made with `-F 999` and with `-c 100000`, on samply `da48ff40`.

* Every main-event sample gets `weight` 1 at all three `add_sample` sites in `handle_main_event_sample` (`samply/src/linux_shared/converter.rs`): the thread, the per-CPU thread, and the combined CPU thread.
* Off-CPU samples get weight 1 whenever `sampling_is_time_based` is set, which is true for every `-F` recording regardless of event (`converter.rs`, `off_cpu_weight_per_sample`; `event_interpretation.rs`).
* `Other event` markers carry only `type` and `cause.stack`; their schema has no fields (`samply/src/shared/process_sample_data.rs`, `OtherEventMarker`).
* With `-F`, each sample record carries `PERIOD`; with `-c`, the sample type omits `PERIOD` and the attribute's `sample_period` gives the fixed value (`perf evlist -v`).
* With `-F cycles`, `threadCPUDelta` holds cycle counts written as nanoseconds, because the converter treats every period as time (`// TODO: Detect event type`).
* The Firefox Profiler's `ProfileMeta.extra` (`src/types/profile.ts`, `ExtraProfileInfoSection`) holds labeled sections for the profile info panel. The profile upgraders never touch it, and fxprof-processed-profile does not expose it.
* The format's `samples.weight` is a JSON number array, while fxprof-processed-profile takes `weight: i32` in `Thread::add_sample`.

## Goals

* samply writes each `Other event` marker's period.
* samply names every perf event and its sampling mode in `meta.extra`.
* `samply import --weight-by-period` writes the main event's period as the sample weight.
* samply stops writing non-clock periods into `threadCPUDelta`.
* pollard weights shares by period when the profile carries periods, and reports the main event's name.

## Non-goals

* Event selection for `samply record` (`-e`, `-c`): `perf record` already covers it, and `samply import` converts the result.
* Widening fxprof-processed-profile's `weight` type from `i32`.
* A new `WeightType` variant; the Firefox Profiler accepts only `samples`, `tracing-ms`, and `bytes`.
  With `--weight-by-period`, the Firefox Profiler therefore labels weighted totals as samples.
* Making `sampling_is_time_based` event-aware. `-F cycles` keeps its time-based profile interval.
* Recovering periods of duplicate-timestamp samples, which the converter already drops.

## samply changes

The samply work splits into two pull requests against `mstange/samply`, so the maintainer can take the bug fix without the feature.

### Pull request 1: periods and event metadata

**fxprof-processed-profile** gains `Profile::add_extra_info_section(label, entries)`, written in `write_meta_json` to `meta.extra` as the Firefox Profiler defines it.
Each entry has a label, a format, and a value, and samply uses only the `string` format.
The Firefox Profiler formats these values without a string table, so string-index formats such as `unique-string` must not be used.
The method is additive, so the crate's public API stays compatible.

**Event metadata.** `samply import` adds one `meta.extra` section labeled `Perf events`, with one `string` entry per perf event attribute in attribute order.
The label is the event name exactly as `EventInterpretation::event_names` holds it, which includes placeholders like `<unknown event 2>`.
The value follows this grammar, with `N` a decimal integer:

```
value = "frequency " N " Hz" | "period " N
```

The first entry is the main event, which becomes the samples track.
pollard reads this section to name the samples track and to find fixed periods, and the Firefox Profiler shows it in the profile info panel.

**Marker period.** `OtherEventMarker` gains an integer field `period`.
The value is the sample record's `period` when present, and the attribute's fixed `sample_period` otherwise, so `-c` recordings carry a period too.
`EventInterpretation` gains `fixed_periods: Vec<Option<u64>>`, one per attribute, filled from each attribute's `SamplingPolicy::Period`.
`samply record` builds its `EventInterpretation` by hand in `linux/profiler.rs` and sets `fixed_periods: vec![None]`.
Marker number fields are `f64`, which represents periods exactly up to 2^53, and the writer prints whole values without a fraction.
This field is on by default because it is additive, and the Firefox Profiler shows it in the marker tooltip.
It changes every import's marker schema, so the pull request description says so and offers to put it behind the flag.

**Sample weight.** `samply import --weight-by-period` plumbs through `ProfileCreationProps` as `weight_by_period: bool`.
With the flag, all three `add_sample` calls in `handle_main_event_sample` pass the main event's period as the sample weight instead of 1, from the record or the fixed period.
Periods above `i32::MAX` saturate to `i32::MAX`, and samply prints one warning per import naming the event and the largest period seen.
The flag also sets the off-CPU weight per sample to 0, because off-CPU time produces no events of the main event and a weight of 1 would mix units with event counts.
`modify_last_sample`, whose `+=` could overflow, is only called from the macOS recorder and not on the import path.
Without the flag, output is unchanged apart from the marker field and `meta.extra`.
`samply record` never passes the flag.

The weight decision lives in one pure function so it can be unit tested without building a `Converter`:

```rust
/// Weight of one main-event sample, and whether it saturated.
fn sample_weight(record_period: Option<u64>, fixed_period: Option<u64>, weight_by_period: bool) -> (i32, bool)
```

The derivation of `fixed_periods` from the attributes' sampling policies is a second pure function with its own unit tests.

### Pull request 2: CPU delta only for clock events

`EventInterpretation` gains `main_event_is_clock: bool`, true when the main attribute is the software `cpu-clock` or `task-clock` event, as `divine_from_attrs` already matches for `sampling_is_time_based`.
`handle_main_event_sample` converts the period to `CpuDelta` only when `main_event_is_clock` is true, and uses the fixed period when the record has none, so `-c cpu-clock` imports gain CPU deltas.
For other main events without context-switch data, the CPU delta becomes 0 instead of an event count.
This changes the Firefox Profiler's CPU graph for such imports from wrong values to no CPU data, and the pull request says so.
The pull request covers the CPU delta only and leaves the profile interval alone.

## pollard changes

**Weight per item.** `Profile` gains `weighted_stack_indices`, which yields `(Option<usize>, u64)` pairs of stack index and weight.
The existing `stack_indices` becomes a wrapper that drops the weight, so callers move over one at a time.
A sample's weight is `samples.weight[i]` when the column exists, and 1 otherwise.
A marker's weight is its `data.period` when present, then the fixed period of that event from the `Perf events` section, then 1.
`RawMarkerData` gains `period: Option<f64>`.

**Weighted tools.** These tools add weights instead of counting items: `top_functions`, `call_tree`, `stacks_containing`, `folded_stacks`, `top_groups`, `compare_profiles`, `source_for_function`, and `asm_for_function`.
Their totals and percentages therefore mean event counts, e.g. cache misses, rather than sample counts.
`source_for_function` and `asm_for_function` iterate `samples.stack` directly and keep doing so, reading `samples.weight[i]` for each sample, so their event and time-range behavior does not change.
`summary`, `describe_profile`, `view_stats`, and `list_events` keep reporting raw sample and marker counts, because they describe the recording rather than rank code.

**Counts with weights.** `top_functions`' per-function `Counts` become `{ samples: u64, weight: u64 }` for self and total.
`*_samples` and `*_pct` report `weight`, and `compare_profiles`' `*_ms` columns keep using `samples` times the interval, because an event count has no time unit.

**`weighted` flag.** Outputs of the weighted tools gain `weighted: bool`.
It is true when the selected event source carries weight information on at least one selected thread: a `samples.weight` column for the samples track, or a marker `period` or fixed period for a marker event.
It does not depend on the weight values, so a `-c 1` recording is still weighted.
`compare_profiles` reports `weighted_a` and `weighted_b`, and its shares stay comparable when only one side is weighted, because each side's percentages are relative to its own total.

**Event names.** `list_events` from sub-project A reads the `Perf events` section.
The samples entry keeps `name: "samples"` and gains `event: "cycles"` and `sampling: "frequency 999 Hz"` when the section exists.
Marker entries gain `sampling` from the entry with the same label.
When several entries share a label, the first one wins, because markers of equally named events are indistinguishable anyway.

**Docs.** The `profile-recording` skill drops the advice to prefer `-c`, and says to pass `--weight-by-period` to `samply import`.

## Errors

* A marker `period` that is not a finite non-negative number is ignored, and the item weighs 1.
* A `Perf events` value that does not match the grammar is ignored.
* A `samples.weight` entry that is negative or not finite weighs 0.

## Testing

* samply: unit tests for `sample_weight` cover a record period, a fixed period only, neither, the flag off, and saturation above `i32::MAX`.
* samply: unit tests for the `fixed_periods` derivation cover frequency and period attributes.
* samply: the fixtures from sub-project A's `regenerate.sh` are re-imported with and without the flag.
  The default import differs from before only by the marker field and the `meta.extra` section, and the flagged import's sample weights equal `perf script -F period` for the main event.
* samply: a `-F cycles` recording with context switches, imported with the flag, has off-CPU samples of weight 0.
* samply: a unit test for `main_event_is_clock` covers `cpu-clock`, `task-clock`, and `cycles`, and a `-c cpu-clock` import has CPU deltas equal to the period.
* pollard: unit tests cover weight lookup for samples, markers with `period`, markers with only a fixed period, and neither.
* pollard: aggregation tests use a fixture where two functions have equal counts but different periods, and check that shares follow periods, `weighted` is true, and `compare_profiles` `_ms` columns follow counts.
* pollard: all existing tests pass unchanged, because their fixtures carry no weights or periods.

## Rollout

samply pull request 1 lands on branch `import-period`, and pull request 2 on branch `cpu-delta-clock-events`, both from `upstream/main` in the samply fork.
pollard's part lands on branch `period-weighting`, stacked on `shared-tables-format`.
pollard does not depend on the samply pull requests being merged: it reads the new fields when present and behaves as today otherwise.
