# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.4](https://github.com/antiguru/pollard/compare/v0.0.3...v0.0.4) - 2026-04-30

### Fixed

- *(source_for_function)* guard None-file equality + add coverage for innermost selection / expand_inlines
- *(source_for_function)* attribute samples to same-file inline frame for closures

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
