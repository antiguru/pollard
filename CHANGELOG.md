# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- output budget: `top_functions`, `top_groups`, `compare_profiles`,
  `stacks_containing`, `folded_stacks`, and `call_tree` now trim their
  responses to fit `POLLARD_MAX_OUTPUT_BYTES` (default ~25 KB) so the
  caller harness's MCP token cap stops dropping payloads. Trimmed
  responses surface a `truncated: { dropped, dropped_pct, budget_bytes,
  final_bytes, still_over_budget }` field; rows are popped tail-first
  and `call_tree` rolls dropped leaves into the parent's `Omitted`
  summary.

### Changed

- `*_pct` columns serialize at one decimal and `*_ms` at two
  decimals — the LLM never needed full f32/f64 precision and the
  shorter rendering frees up budget for more rows.

## [0.0.8](https://github.com/antiguru/pollard/compare/v0.0.6...v0.0.8) - 2026-05-06

Fix release by synchronizing versions. The work below actually
landed here, not in the orphaned 0.0.7 tag.

### Other

- *(release-plz)* reorder steps so sync runs last
- *(release-plz)* restore HEAD to trigger SHA after manifest sync
- *(plugin-release)* allow manual dispatch against an existing tag
- use "./" relative-path source instead of bare "."

## [0.0.7](https://github.com/antiguru/pollard/compare/v0.0.6...v0.0.7) - 2026-05-06

Exists purely because of a broken release script, shouldn't exist. Do not use.

## [0.0.6](https://github.com/antiguru/pollard/compare/v0.0.5...v0.0.6) - 2026-05-06

### Added

- *(views)* scoped views — process / thread / time_range pre-filter on create_view
- *(summary)* hints field nudges callers toward create_view
- *(views)* keep_only_frames / keep_only_modules inverse-hide filter ([#89](https://github.com/antiguru/pollard/pull/89))
- *(views)* strip_type_params preset transform ([#88](https://github.com/antiguru/pollard/pull/88))
- *(views)* regex backreferences in rename replacements
- *(views)* rule hit counts on create_view and describe_view tool
- *(views)* stack views by composing transforms across the chain
- *(views)* collapse_recursion folds multi-function cycles up to length 8
- *(tools)* list_profiles reports view base id
- *(tools)* create_view MCP tool for derived profiles
- *(registry)* create_view derives a session from a base
- *(profile)* resolved_chain helper applies view transforms
- *(profile)* Arc-share raw tables and add transforms field
- *(profile)* introduce Transforms value type for views
- *(query)* compare_functions side-by-side asm diff ([#8](https://github.com/antiguru/pollard/pull/8))
- *(call_tree)* surface cross-process aggregation on inverted trees
- *(summary)* accept process / thread / time_range filter
- *(filter)* warn when bare-name process= matches multiple pids
- *(call_tree)* surface child names in pruning markers
- *(summary)* add top_processes and top_threads breakdown
- *(describe)* cap describe_profile output and drop idle entries

### Fixed

- rustfmt — missing newline before DescribeViewResult struct
- *(registry)* create_view caches identical views; harden tests
- *(error)* cap available_* lists, demangle nearest_matches, add module_not_found, route event/regex through invalid_value
- *(symbolicate)* count hex frames as unsymbolicated; surface per-lib outcomes
- *(query)* make time_range filter and summary.time_range_ms share a frame
- *(query)* reject empty function pattern for required/narrowing args

### Other

- *(release-plz)* use action outputs to find release PR branch
- *(changelog)* record plugin bundle, cookbook, summary hints, regex backrefs
- *(readme)* document plugin install path alongside MCP-direct install
- extract version via cargo metadata, not grep/sed
- skill description voice, doc cleanup, manifest version guard
- --version, --help, and unknown-arg handling
- prerequisites + pollard-doctor health-check skill
- sync plugin manifest versions in release-plz PR
- marketplace.json and tag-triggered release workflow
- bundle pollard as a Claude Code plugin with skills
- surface cookbook through view_presets MCP tool (may be reverted)
- *(views)* cookbook of canonical hide_modules / hide_frames regex sets
- *(transforms)* name the keep-only placeholder inline
- cargo fmt + fix clippy::shadow_unrelated in keep_only test
- *(view_stats)* cargo fmt
- *(matching)* cargo fmt
- *(view_stats)* drop unneeded allow(dead_code)
- *(view_stats)* cargo fmt
- *(spec)* tighten create_view cost and lifetime wording
- *(spec)* document profile views
- *(views)* end-to-end view re-attributes hidden leaf
- *(tools)* clarify create_view syntax and collapse_recursion guidance
- *(query)* folded_stacks consumes resolved_chain
- *(query)* stacks_containing consumes resolved_chain
- *(query)* call_tree consumes resolved_chain
- *(query)* top_functions consumes resolved_chain
- *(plan)* profile views implementation plan
- Merge pull request #58 from antiguru/measure-rss
- *(spec)* bring design doc back in sync with shipped surface
- *(query)* [**breaking**] unify percentage field naming on {kind}_pct
- Merge pull request #77 from antiguru/claude/fix-issue-68-oPTKI
- *(tools)* document `re:` regex prefix on every function-pattern arg
- *(call_tree)* apply cargo fmt
- Merge pull request #54 from antiguru/issue-52-marker-no-stack
- Merge pull request #53 from antiguru/issue-51-empty-pattern

### Fixed

- *(symbolicate)* don't enable macOS Spotlight on Linux ([#56](https://github.com/antiguru/pollard/issues/56)).
  `SymbolManagerConfig::use_spotlight(true)` was unconditional. On Linux that pushed wholesym into a macOS-shaped resolution path that ended in a `dyld_shared_cache_x86_64` read, so every Linux `.so` failed symbolication with a stderr line like `could not load symbols for "/usr/lib64/libc-2.28.so": ... /System/Library/dyld/dyld_shared_cache_x86_64 ... No such file or directory`. Profiles imported from `perf.data` showed up degraded (`top_functions`, `call_tree`, `source_for_function` lost names) and the stderr volume drowned other diagnostics. Spotlight is now gated behind `cfg!(target_os = "macos")`, leaving the macOS dSYM-bundle lookup intact and skipping it entirely on other targets.

### Changed

- **breaking** *(query)* unify percentage field naming on `{kind}_pct` ([#65](https://github.com/antiguru/pollard/issues/65)).
  `summary.dominant_thread.samples_pct`, `summary.top_processes[].samples_pct`, `summary.top_threads[].samples_pct`, and `source_for_function[].samples_pct` are renamed to `self_pct` so every percentage field across the surface uses the same `{kind}_pct` shape (`self_pct` / `total_pct`; `samples` reserved for cases where there's no self/total distinction at all). "Samples on this thread / process" *is* self time for that thread / process, and per-line source samples are self time for that line, so `self_pct` is the natural name and matches what `top_functions` already exposes. Wire-format break for callers that destructured the old name. The convention is documented next to `ProcessEntry::self_pct` and in the design doc.

- *(tools)* document the `re:(?i)` case-insensitive convention on every function-pattern arg ([#67](https://github.com/antiguru/pollard/issues/67)).
  Substring matching is case-sensitive (`memcpy` ≠ `MEMCPY`), which surprises during exploration. The regex form already supports the standard `(?i)` inline flag; the per-arg docs and the `src/matching.rs` module header now spell that out so callers don't have to discover it from the `regex` crate. The canonical one-liner is now `Substring match by default; prefix with `re:` for a regex (use `re:(?i)foo` for case-insensitive).` — same wording at every site so it stays grep-able alongside the #66 line. Default behavior is unchanged.

- *(tools)* document the `re:` regex prefix on every function-pattern arg ([#66](https://github.com/antiguru/pollard/issues/66)).
  The prefix that flips a function pattern from substring match to regex was discoverable through the skill description, not through per-arg docs — callers without the skill missed the feature, and callers with it had to re-find which args supported it. Every function-pattern arg now carries the same one-line "Substring match by default; prefix with `re:` for a regex." in its `description`: `top_functions.filter`, `top_groups.filter`, `compare_profiles.filter`, `call_tree.root_function`, `call_tree.paths_to`, `folded_stacks.function_filter`, `stacks_containing.function`, `source_for_function.function`, `asm_for_function.function`. The wording is identical at every site so it's grep-able.

### Added

- *(plugin)* bundle pollard as a Claude Code plugin with skills ([#92](https://github.com/antiguru/pollard/issues/92)).
  `.claude-plugin/plugin.json` registers the MCP server and `.claude-plugin/marketplace.json` makes the bundle installable via `/plugin marketplace add antiguru/pollard`. Three skills ship alongside: `profile-recording` (samply record / `samply import` for `perf.data` / `load_profile` workflow with troubleshooting for empty profiles, unsymbolicated frames, and `perf.data` recorded without `-g`), `view-presets` (cookbook regex sets for tracing-subscriber, tokio internals, and Rust stdlib glue, plus a stacked-view recipe), and `pollard-doctor` (dependency-ordered diagnostic for binary-not-on-PATH, MCP-server-not-reloaded, empty-profile, and unsymbolicated-frames failure modes with exact remediation per step). Skills surface as `/pollard:profile-recording`, `/pollard:view-presets`, and `/pollard:pollard-doctor` and solve the discoverability gap a plain MCP tool leaves: the cookbook would otherwise require the agent to think to look it up. The plugin bundle does not ship the binary — `cargo install pollard` is still a prerequisite. README documents both the plugin install path and the legacy `claude mcp add` server-only path.
- *(docs)* view-presets cookbook of canonical `hide_modules` / `hide_frames` regex sets ([#92](https://github.com/antiguru/pollard/issues/92)).
  `docs/superpowers/specs/2026-05-06-view-presets-cookbook.md` collects copy-paste regex sets for the three filters users keep re-deriving by hand: tracing-subscriber walls, tokio runtime internals, Rust stdlib glue. Ships as docs (and the mirrored `view-presets` skill) rather than a `presets=` argument on `create_view`, so curated content can drift with upstream crates without baking yesterday's noise filters into the binary. The skill mirrors the cookbook's regex blocks; the design spec records the convention that cookbook is the source of truth and reviewers reject one-sided regex changes.
- *(cli)* `--version`, `--help`, and unknown-arg handling.
  `pollard --version` and `pollard --help` previously hung on stdio waiting for an MCP client. A tiny hand-rolled arg parser in `src/main.rs` (no new dep) now prints and exits before tokio starts; unknown args fail with exit code 2 and a hint. The MCP server path is unchanged when no args are passed. `pollard-doctor`'s binary check uses `pollard --version` instead of `command -v pollard` so stale installs whose version doesn't match what the user expects are caught.
- *(query)* `summary` returns `hints` nudging callers toward `create_view` ([#93](https://github.com/antiguru/pollard/issues/93)).
  Three cheap heuristics fire on the summary response: dominant function appears recursively in stacks (suggests `collapse_recursion`), a single module covers >40% of stacks (suggests `hide_modules`), or top function names carry type parameters (suggests `strip_type_params`). The `hints` array is omitted when no heuristic fires, so a clean profile stays silent.
- *(views)* regex backreferences in `rename` replacements ([#87](https://github.com/antiguru/pollard/issues/87)).
  `rename` application now goes through a new `FunctionMatcher::replace` helper that delegates to `regex::Regex::replace`, so capture references (`$1`, `${name}`) interpolate in the replacement string. A single rule can fold a trait-vs-inherent monomorphisation pair like `re:<(.*) as .*::Schedule>::schedule => ${1}::schedule` where users previously had to enumerate one rule per concrete type.
- *(views)* scoped views — `process` / `thread` / `time_range` pre-filter on `create_view` ([#90](https://github.com/antiguru/pollard/issues/90)).
  Pinning a hot pid/tid or a benchmark window once at view creation is now possible: the scope is baked into the view's stored state and applied alongside any per-call `CommonFilterArgs` at query time. Per-call filters must be a sub-slice of the view's scope — equality (or, for `pid:`, the bare-vs-suffixed strict-narrowing case) for `process`/`thread`, and `[start, end] ⊆ [scope_start, scope_end]` for `time_range`. Conflicting per-call filters are rejected with `invalid_value`, so a misuse surfaces as one structured error rather than a silently-ignored argument. Stacked views inherit the parent scope and are only allowed to narrow it further, by the same sub-slice rule. The composed scope round-trips through `describe_view` under a new `scope` field (`thread`/`process`/`time_range`, omitted when nothing is set). View ids hash the scope alongside the transforms, so two views differing only in scope register as distinct.
- *(views)* `keep_only_frames` / `keep_only_modules` inverse-hide transforms on `create_view` ([#89](https://github.com/antiguru/pollard/issues/89)).
  Inverse of `hide_frames` / `hide_modules`: only frames whose function name (or module name) matches at least one pattern survive, and each maximal run of non-matching frames collapses into a single `<hidden>` placeholder frame. The two lists OR together — a frame is kept if it matches any `keep_only_*` rule. Applied before `hide_*`, so a frame matching both a `keep_only` and a `hide` rule is dropped (`hide` always wins). `describe_view`'s `transforms` view echoes the patterns, and `rule_stats` gains `keep_only_frames` / `keep_only_modules` entries whose `frames_matched` counts every kept frame and `0` is the same typo signal as for the other rule kinds. Composes through `extend_from` by appending lists (union), matching `hide_*`. The placeholder name is static for v1.
- *(views)* `strip_type_params` preset transform on `create_view` ([#88](https://github.com/antiguru/pollard/issues/88)).
  When `strip_type_params: true`, balanced `<…>` segments are removed from each frame's function name during transform application — `OrdValBatch<RowRowLayout<((Row, Row), Ts, i64)>>` collapses to `OrdValBatch`. The strip runs after `hide_*` and before `rename`, so user rules can target the normalized name without each rule encoding a bracket-tolerant pattern. Implemented as a hand-rolled depth counter (regex can't match balanced delimiters reliably); unmatched `>` outside any open bracket is preserved verbatim. `describe_view`'s `transforms` view echoes the flag, and `rule_stats` gains a `strip_type_params` entry whose `frames_matched` counts every frame whose name actually changed — `0` is the same typo signal as for the other rule kinds. Composes through `extend_from` as a logical OR, matching `collapse_recursion`.
- *(views)* per-rule hit counts on `create_view`, plus a new `describe_view` tool ([#85](https://github.com/antiguru/pollard/issues/85), [#86](https://github.com/antiguru/pollard/issues/86)).
  `create_view` now returns `rule_stats: [{rule_index, kind, pattern, frames_matched, samples_affected}]` and `total_base_samples`, computed by replaying every `hide_frames` / `hide_modules` / `rename` / `collapse_recursion` rule against the base's samples once at view-create time. A rule with `frames_matched: 0` is the typo signal — without it, mistyped patterns failed silently and only surfaced when a downstream tool produced unchanged output. `describe_view(profile_id)` is symmetric with `describe_profile` for views: returns the immediate parent's id, the full composed `transforms` shape, and the same per-rule counts so callers can re-fetch them later without re-creating the view. Stats are cached on the view's `ProfileSession`, so `describe_view` is a hash-map lookup.
- *(query)* `compare_functions(profile_id_a, function_a, function_b, profile_id_b?, ...)`: side-by-side asm diff with per-instruction sample counts on both sides ([#8](https://github.com/antiguru/pollard/issues/8)).
  Two `asm_for_function` calls plus an LCS pass over a normalized instruction key (registers collapsed to `R`, numeric immediates to `IMM`, mnemonic + operand shape preserved). The displayed asm text is unchanged; normalization is alignment-only — without it, register renames and differing displacements (`xmm0`/`xmm1`, `rdi+rcx*8-0x38` / `rdi+rcx*1-0x6000`) would split nominally-equal instructions onto separate rows and the per-row sample columns would no longer line up. `profile_id_b` defaults to `profile_id_a`, so the natural "why is `simd_rows_1st` faster than `simd_cols_1st`" workflow stays a one-profile call; pass a different `profile_id_b` for before/after refactor diffs across two recordings. Output rows carry `Option<offset|asm|samples>` per side so LCS gaps (instructions only on one side, e.g. an unrolled loop with more `addsd`s) remain visible at their original offsets. The two sides must agree on `arch`; a mismatch errors out rather than producing a meaningless alignment.

- *(query)* surface cross-process aggregation on inverted `call_tree` ([#68](https://github.com/antiguru/pollard/issues/68)).
  An inverted (`inverted=true`) `call_tree` without a `process=` filter pulls callers from every pid that hits a given leaf. Reasonable for "where is `memcpy` called from anywhere", but the same caller chain can mix time from two different processes via different code paths and the result was indistinguishable from a single-process tree. The response now carries `cross_process: true` and `processes_in_tree: [{pid, name, samples, pct}]` (biggest first; `pid` matches the `process=pid:` wire form) when the inverted aggregation crossed >1 distinct pid; single-process and process-filtered results stay silent. Top-down trees never emit the signal because their root structure already separates per-process callees.

- *(query)* `summary` accepts the standard `process` / `thread` / `time_range` filter ([#63](https://github.com/antiguru/pollard/issues/63)).
  Previously profile-wide only, so the natural follow-up after spotting a hot pid in `top_processes` was a hand-composed `top_functions` + `top_modules` + dominant-thread reasoning. The filter now flows through every sample-count surface in the response (`total_samples`, `time_range_ms`, `duration_ms`, `top_processes`, `top_threads`, `top_modules`, both top-functions lists, `dominant_thread`); recording-level fields (`interval_ms`, `sample_rate_hz`, `unsymbolicated_pct`, `profile_start_ms`) stay profile-wide because they describe the recording, not the slice. Default args reproduce the prior profile-wide behavior.

- *(query)* surface child names on `call_tree` pruning markers ([#61](https://github.com/antiguru/pollard/issues/61)).
  Omitted-children markers now carry `top_omitted` (up to 3 entries of `{function, pct}`, biggest first) and truncated-subtree markers carry the cutoff frame's `function`, so a caller can tell *what* was dropped and decide whether widening `min_pct` / `max_breadth` / `max_depth` is worth a second call.
- *(query)* warn when bare-name `process=` aggregates across multiple pids ([#62](https://github.com/antiguru/pollard/issues/62)).
  `top_functions`, `call_tree`, `top_groups`, `stacks_containing`, and `compare_profiles` now expose `matched_processes: [{pid, name}]` (per-side `a_/b_` for `compare_profiles`) when a bare-name filter resolves to more than one distinct pid, listing the pids it absorbed so the caller can disambiguate via `pid:N` / `pid:N.M`. Bare-name filters that resolve to a single pid stay silent.

### Fixed

- *(query)* align `summary.time_range_ms` with the `time_range` filter contract ([#64](https://github.com/antiguru/pollard/issues/64)).
  `summary.time_range_ms` reported absolute (boot-relative) timestamps while `time_range` filter args are documented as profile-relative; pasting one into the other returned no samples.
  `time_range_ms` is now offset by the first-sample anchor (matching the filter's reference frame), the new `summary.profile_start_ms` exposes that anchor so the absolute frame is one addition away, and `Profile::stack_indices` subtracts the anchor before gating so boot-relative samply timestamps no longer leak through.

### Other

- *(release)* tag-triggered `plugin-release.yml` workflow zips `.claude-plugin/`, `skills/`, README, and LICENSE-* into `pollard.skill` and attaches it to the GitHub release that release-plz already cuts; a verify step rejects the tag if Cargo.toml's version doesn't match `.claude-plugin/plugin.json` and `marketplace.json`.
- *(release)* `release-plz.yml` splits into release-pr → sync → release so every release-pr run is followed by a commit that mirrors `Cargo.toml`'s version into `.claude-plugin/plugin.json` and `.claude-plugin/marketplace.json`. release-plz force-pushes its release branch each run, so the sync commit is reapplied rather than persisting.
- *(ci)* new `plugin-manifests` job in `test.yml` asserts `Cargo.toml` version equals `.claude-plugin/plugin.json` and `marketplace.json`, catching manifest drift on regular PRs instead of waiting for the tag-time check.
- *(ci)* extract version via `cargo metadata --no-deps --format-version 1 | jq` instead of `grep`/`sed`, robust against Cargo.toml formatting changes (workspace inheritance, quoting, comments).

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
