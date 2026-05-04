# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.5](https://github.com/antiguru/pollard/compare/v0.0.4...v0.0.5) - 2026-05-04

This release fixes a class of silent-failure bugs across the query tools where filter and enum arguments documented in the spec either did nothing or fell through to a default without any error.
Every gap closes with a structured `ToolError` that echoes the offending value and lists the accepted set, so the LLM can correct in one retry instead of guessing why a query came back empty.

### Fixed

- *(query)* wire process filter (name + sub-pid) ([#43](https://github.com/antiguru/pollard/pull/43)).
  `ProcessFilter::Name` was a TODO that always returned `false`, and `pid:` parsed only bare `u64`, so samply's `.N` sub-pid form fell through to the broken name path.
  Every `process=` value silently returned 0 samples.
  `Name` now matches each thread's `processName`; `Pid` widens to `raw::Pid` so a bare pid matches every sub-process sharing the OS pid and a suffixed pid pins to one.
  Unknown values now surface `process_not_found` with `available_processes` instead of an empty result.
- *(query)* wire `time_range` filter through sample iteration ([#48](https://github.com/antiguru/pollard/pull/48)).
  `time_range` was parsed into `Filter` and documented in the spec for `top_functions`, `call_tree`, `stacks_containing`, `folded_stacks`, and `compare_profiles`, but no aggregator consulted it — the unsliced full profile came back regardless.
  Per-sample gating now lives on `Profile::stack_indices` and uses `samples.absolute_times()` / `markers.start_time` to drop items outside the inclusive range.
  Fully-disjoint or inverted ranges return `out_of_bounds` via the new `validate_time_range`; partial overlap clamps silently per the design spec.
- *(query)* reject unknown sort_by/group_by/align_by values ([#49](https://github.com/antiguru/pollard/pull/49)).
  Five tool entry points used catch-all `_ =>` arms that silently fell back to the default on unrecognized string-enum values — a `sort_by="selfTime"` typo silently ranked by self-time anyway, `group_by="lib"` silently grouped by function.
  All five sites now route through a `parse_string_enum` helper that returns `ToolError::InvalidValue { field, value, accepted }` with the full accepted set.
- *(query)* reject malformed pid:/tid: prefix instead of name fallback ([#50](https://github.com/antiguru/pollard/pull/50)).
  `tid:` and `pid:` opt into integer matching, but the parser silently fell back to a literal name match when the rest didn't parse — `process="pid:abc"` then surfaced `process_not_found` listing every real process, none of which resembled the input.
  `parse_filter` is now fallible: malformed prefixes return `invalid_value` with the accepted syntax (`tid:NNN`, `pid:NNN`, `pid:NNN.M`, `<thread/process name>`).
  Bare-name matches without a prefix still work unchanged.

### Other

- enable `clippy::match_wildcard_for_single_variants` as defensive hygiene against future enum exhaustiveness mistakes
- drop redundant `#![allow(dead_code)]` from `src/profile/event_source.rs` and `src/query/event.rs` after the iterator refactor

## [0.0.4](https://github.com/antiguru/pollard/compare/v0.0.3...v0.0.4) - 2026-04-30

### Fixed

- *(source_for_function)* attribute closure-body samples to the hot line inside the closure instead of the closure call site ([#39](https://github.com/antiguru/pollard/pull/39)).
  Bencher-style `bencher.iter(|| { ... })` benchmarks previously reported nearly all samples on the `bencher.iter(...)` line, making per-line annotation useless for closure-bodied work.
  The matcher now walks the matched native frame's inline chain innermost-first and picks the first frame whose file equals the outer frame's file, falling back to the outer frame otherwise.
  Plain straight-line code and inlined helpers from other modules, stdlib, or deps fall through to existing behavior.
  `top_functions`, `call_tree`, and `resolved_name` are unaffected.

### Other

- *(release-plz)* enable publish + release for the workspace
- *(release-plz)* switch to crates.io trusted publishing
- *(build_tiny)* gate fixture regeneration on REGENERATE_FIXTURES=1
- *(event)* skip cleanly when POLLARD_SMOKE_PROFILE unset
- *(e2e)* lower perf_event_paranoid so samply can record on hosted ubuntu
- cargo fmt across remaining files to satisfy CI fmt check
- cargo fmt my new tests
- *(source_for_function)* clarify innermost-first ordering in attribution comment
- *(source_for_function)* label whole_file/expand_inlines args in regression test
- *(source_for_function)* regression guard for different-file inline fallback
