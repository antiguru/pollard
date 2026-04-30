# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
