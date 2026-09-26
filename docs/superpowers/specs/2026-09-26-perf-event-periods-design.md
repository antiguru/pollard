# Perf event periods from samply to pollard

## Problem

pollard counts every sample and every `Other event` marker as 1, because samply drops the perf event period when it imports `perf.data`.
In frequency mode (`perf record -F`), perf adjusts the period per sample, so equal counts do not mean equal event totals, and count shares are biased toward code that ran while the period was small.
The profile also does not name the event behind the samples track, so pollard can only call it `samples`.
This spec covers sub-project B: samply records periods and event metadata, and pollard weights by them.
It builds on sub-project A (`2026-09-25-shared-tables-format-design.md`), which makes pollard read samply's current output.

## Evidence

The findings below come from `samply import` of `perf record -e cycles,cache-misses,instructions,branch-misses` recordings made with `-F 999` and with `-c 100000`, on samply `da48ff40`.

* Every sample gets `weight` 1 (`samply/src/linux_shared/converter.rs`, `handle_main_event_sample`).
* `Other event` markers carry only `type` and `cause.stack`; their schema has no fields (`samply/src/shared/process_sample_data.rs`, `OtherEventMarker`).
* With `-F`, each sample record carries `PERIOD`; with `-c`, the sample type omits `PERIOD` and the attribute's `sample_period` gives the fixed value.
* With `-F cycles`, `threadCPUDelta` holds cycle counts written as nanoseconds, because the converter treats every period as time (`// TODO: Detect event type`).
* The Firefox Profiler's `ProfileMeta.extra` (`src/types/profile.ts`, `ExtraProfileInfoSection`) holds labeled sections for the profile info panel, and fxprof-processed-profile does not expose it.
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

## samply changes

The samply work splits into two pull requests against `mstange/samply`, so the maintainer can take the bug fix without the feature.

### Pull request 1: periods and event metadata

**fxprof-processed-profile** gains `Profile::add_extra_info_section(label, entries)`, written to `meta.extra` as the Firefox Profiler defines it.
Each entry has a label, a format (the marker field formats, e.g. `string` or `integer`), and a value.
The method is additive, so the crate's public API stays compatible.

**Event metadata.** `samply import` adds one `meta.extra` section labeled `Perf events`.
It lists one entry per perf event attribute: label is the event name, value is `"frequency 999 Hz"` or `"period 100000"` from the attribute's sampling policy.
The first entry is the main event, which becomes the samples track.
pollard reads this section to name the samples track and to find fixed periods; the Firefox Profiler shows it in the profile info panel.

**Marker period.** `OtherEventMarker` gains an integer field `period`.
The value is the sample record's `period` when present, and the attribute's fixed `sample_period` otherwise, so `-c` recordings carry a period too.
`EventInterpretation` gains `fixed_periods: Vec<Option<u64>>`, one per attribute, filled from each attribute's `SamplingPolicy::Period`.
Marker number fields are `f64`, which represents periods exactly up to 2^53.
This field is on by default: it is additive, and the Firefox Profiler shows it in the marker tooltip.

**Sample weight.** `samply import --weight-by-period` plumbs through `ProfileCreationProps` as `weight_by_period: bool`.
With the flag, `handle_main_event_sample` passes the main event's period, from the record or the fixed period, as the sample weight instead of 1.
Periods above `i32::MAX` saturate to `i32::MAX`, and samply prints one warning per import naming the event and the largest period seen.
Without the flag, output is unchanged.
`samply record` never passes the flag, so its output is unchanged.

### Pull request 2: CPU delta only for clock events

`handle_main_event_sample` converts `period` to `CpuDelta` only when the main event is the software `cpu-clock` or `task-clock` event, whose periods are nanoseconds.
For other main events without context-switch data, the CPU delta becomes 0 instead of an event count.
This changes the Firefox Profiler's CPU graph for such imports from wrong values to no CPU data, and the pull request says so.

## pollard changes

**Period per item.** `Profile::stack_indices` yields a weight alongside each stack index.
A sample's weight is `samples.weight[i]` when the column exists, and 1 otherwise.
A marker's weight is its `data.period` when present, then the fixed period of that event from the `Perf events` section, then 1.

**Weighted aggregation.** Aggregators that count samples (`top_functions`, `call_tree`, `stacks_containing`, `compare_profiles`, `source_for_function`, `asm_for_function`) add the weight instead of 1.
Totals and percentages therefore mean event counts, e.g. cache misses, rather than sample counts.
Outputs gain `weighted: bool`, true when any item in the selection had a weight other than 1.
Profiles without weights or periods produce exactly today's results.
`*_ms` columns in `compare_profiles` stay tied to sample counts, because an event count has no time unit.

**Event names.** `list_events` from sub-project A reads the `Perf events` section.
The samples entry keeps `name: "samples"` and gains `event: "cycles"` and `sampling: "frequency 999 Hz"` when the section exists.
Marker entries gain `sampling` from the same section.

**Docs.** The `profile-recording` skill drops the advice to prefer `-c`, once profiles carry periods, and says to pass `--weight-by-period` to `samply import`.

## Errors

* A marker `period` that is not a finite non-negative number is ignored, and the item weighs 1.
* A `Perf events` entry whose value matches neither form is ignored.

## Testing

* samply: a converter test feeds records with and without `period` and checks the marker `period` field, the fixed-period fallback, and the saturated weight with `--weight-by-period`.
* samply: the fixtures from sub-project A's `regenerate.sh` are re-imported with and without the flag; the default import differs from before only by the new marker field and the `meta.extra` section.
* samply: a test checks that `threadCPUDelta` is 0 for a cycles main event without context switches and equals the period for `cpu-clock`.
* pollard: unit tests cover weight lookup for samples, markers with `period`, markers with only a fixed period, and neither.
* pollard: aggregation tests use a fixture where two functions have equal counts but different periods, and check that shares follow periods and `weighted` is true.
* pollard: all existing tests pass unchanged, because their fixtures carry no weights or periods.

## Rollout

samply pull request 1 lands on branch `import-period`, and pull request 2 on branch `cpu-delta-clock-events`, both from `upstream/main` in the samply fork.
pollard's part lands on branch `period-weighting`, stacked on `shared-tables-format`.
pollard does not depend on the samply pull requests being merged: it reads the new fields when present and behaves as today otherwise.
