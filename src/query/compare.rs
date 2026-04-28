//! `compare_profiles`: per-function delta between two profiles.
//!
//! Aligns by `(function, module)` by default and emits self/total sample
//! counts and percentages from each side plus their delta. The percentage
//! delta is the load-bearing column for benchmark comparisons — sample
//! counts move with profile duration, percentages are roughly normalized.
//!
//! Module strings are normalized via [`strip_cargo_hash`] before keying so
//! two builds of the same cargo binary (which embeds a fresh 16-hex hash
//! per build) align to one row per function. Callers that want to ignore
//! modules entirely (e.g. `bench-sse2` vs `bench-avx2`) can pass
//! `align_by = AlignBy::Function`.

#![allow(dead_code)]

use crate::error::ToolError;
use crate::profile::Profile;
use crate::query::event::EventSource;
use crate::query::filters::Filter;
use crate::query::top_functions::{Counts, aggregate_functions};
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct Args {
    /// Optional substring/regex filter applied to function names on both
    /// sides. See `top_functions` for the matcher syntax.
    pub filter: Option<String>,
    /// Limit on the number of rows returned. 0 → default ([`DEFAULT_LIMIT`]).
    pub limit: usize,
    pub sort_by: SortBy,
    /// Drop rows whose absolute self-pct delta is below this threshold.
    /// `None` keeps every aligned row.
    pub min_delta_pct: Option<f32>,
    /// Thread/process/time-range filter, applied to both sides.
    pub filter_args: Filter,
    /// Forwarded to the per-profile aggregator.
    pub expand_inlines: bool,
    /// Join-key shape. Default joins on `(function, module)`; `Function`
    /// drops the module so two binaries with different names but the same
    /// function set align.
    pub align_by: AlignBy,
    /// Which per-sample event drives the diff. Default
    /// [`EventSource::Samples`] (cycles in samply); pass
    /// [`EventSource::Marker`] to diff a hardware-counter event such as
    /// `cache-misses`. The `*_ms` columns are populated only for the
    /// time-shaped default — for marker events they are emitted as
    /// `null` because count × sampling-interval has no meaningful unit.
    pub event: EventSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortBy {
    /// `|delta_self_pct|` descending (default — surfaces what moved most
    /// in share-of-profile terms).
    #[default]
    Delta,
    /// `|delta_self_ms|` descending — surfaces functions whose absolute
    /// wall-time-ish contribution moved most. Robust to changes in total
    /// profile duration in a way that share-based sort isn't.
    DeltaMs,
    /// Profile A's self-pct descending.
    A,
    /// Profile B's self-pct descending.
    B,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlignBy {
    /// Join on `(function, module)` (default). Module strings are
    /// normalized to strip cargo's 16-hex-char build hash suffix so
    /// `simd_cols-1234567890abcdef` and `simd_cols-fedcba9876543210`
    /// align.
    #[default]
    FunctionAndModule,
    /// Join on function name only. Use when the two profiles come from
    /// differently-named binaries (e.g. `bench-sse2` vs `bench-avx2`)
    /// where module-level alignment is hopeless.
    Function,
}

/// Strip cargo's 16-hex-char build hash suffix from a module name.
/// Cargo embeds a hash like `-1234567890abcdef` in compiled binary names;
/// the hash differs across builds of the same source, which would otherwise
/// split each function into two unaligned rows in the diff.
///
/// Returns the input unchanged when no such suffix is present.
fn strip_cargo_hash(module: &str) -> &str {
    let bytes = module.as_bytes();
    if bytes.len() < 17 {
        return module;
    }
    let suffix_start = bytes.len() - 17;
    if bytes[suffix_start] != b'-' {
        return module;
    }
    let hex = &bytes[suffix_start + 1..];
    if hex
        .iter()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        // Refuse to produce an empty stem — guards against a hypothetical
        // module that is *only* a hash with no leading name.
        if suffix_start == 0 {
            return module;
        }
        &module[..suffix_start]
    } else {
        module
    }
}

const DEFAULT_LIMIT: usize = 30;

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub a_total_samples: u64,
    pub b_total_samples: u64,
    pub filter: Option<String>,
    pub sort_by: &'static str,
    /// Echo of the resolved event source — `"samples"` or the marker
    /// name (e.g. `"cache-misses"`). Lets the caller verify which
    /// counter the pct columns are percentages of.
    pub event: String,
    pub functions: Vec<DiffEntry>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DiffEntry {
    pub rank: usize,
    pub function: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    pub a_self_samples: u64,
    pub a_self_pct: f32,
    pub a_total_samples: u64,
    pub a_total_pct: f32,
    pub b_self_samples: u64,
    pub b_self_pct: f32,
    pub b_total_samples: u64,
    pub b_total_pct: f32,
    /// `b_self_pct - a_self_pct`. Positive = function got hotter in B.
    pub delta_self_pct: f32,
    /// `b_total_pct - a_total_pct`.
    pub delta_total_pct: f32,
    /// Raw sample-count deltas. Less normalized than the pct deltas — useful
    /// when both profiles have similar duration and the caller wants to know
    /// the absolute movement, not just the rebalance.
    pub delta_self_samples: i64,
    pub delta_total_samples: i64,
    /// Per-side wall-time estimate: `samples * meta.interval_ms`. Pct
    /// columns shift when total profile time changes; ms columns don't —
    /// they answer "did this function take more or less time" directly.
    /// `None` (and omitted from JSON output) when the chosen `event` is
    /// not time-shaped — multiplying a marker's event count by the
    /// sampling interval produces a meaningless unit.
    /// Caveat for the time-shaped case: across N sampled threads,
    /// samples sum across threads, so the value is closer to summed-
    /// CPU-time than wall time when multiple threads are profiled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub a_self_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b_self_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub a_total_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b_total_ms: Option<f64>,
    /// `b_self_ms - a_self_ms`. Positive = function spent more time in B.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta_self_ms: Option<f64>,
    /// `b_total_ms - a_total_ms`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta_total_ms: Option<f64>,
}

pub fn compare_profiles(a: &Profile, b: &Profile, args: &Args) -> Result<Output, ToolError> {
    // sort_by="delta_ms" is only defined for time-shaped events.
    // Refuse the combination loudly rather than silently sorting on a
    // null column.
    if matches!(args.sort_by, SortBy::DeltaMs) && !args.event.is_time_shaped() {
        return Err(ToolError::Internal {
            message: format!(
                "sort_by=\"delta_ms\" is only valid for time-shaped events; \
                 event {label:?} has no millisecond interpretation. Try sort_by=\"delta\".",
                label = args.event.label(),
            ),
        });
    }

    let (counts_a, total_a) = aggregate_functions(
        a,
        args.filter.as_deref(),
        &args.filter_args,
        args.expand_inlines,
        &args.event,
    )?;
    let (counts_b, total_b) = aggregate_functions(
        b,
        args.filter.as_deref(),
        &args.filter_args,
        args.expand_inlines,
        &args.event,
    )?;

    // Outer-join. Every key in either side gets a row; missing-side counts
    // default to zero so downstream pct math stays well-defined. Both sides
    // are re-keyed through `join_key` so the cargo-hash strip and the
    // optional drop-module mode apply symmetrically.
    let mut joined: HashMap<(String, Option<String>), (Counts, Counts)> = HashMap::new();
    for ((function, module), c) in counts_a {
        let key = join_key(function, module, args.align_by);
        let slot = &mut joined.entry(key).or_default().0;
        slot.self_samples += c.self_samples;
        slot.total_samples += c.total_samples;
    }
    for ((function, module), c) in counts_b {
        let key = join_key(function, module, args.align_by);
        let slot = &mut joined.entry(key).or_default().1;
        slot.self_samples += c.self_samples;
        slot.total_samples += c.total_samples;
    }

    let denom_a = total_a.max(1) as f32;
    let denom_b = total_b.max(1) as f32;
    let interval_a = a.meta().interval;
    let interval_b = b.meta().interval;
    let time_shaped = args.event.is_time_shaped();

    let mut rows: Vec<DiffEntry> = joined
        .into_iter()
        .map(|((function, module), (ca, cb))| {
            let a_self_pct = 100.0 * ca.self_samples as f32 / denom_a;
            let b_self_pct = 100.0 * cb.self_samples as f32 / denom_b;
            let a_total_pct = 100.0 * ca.total_samples as f32 / denom_a;
            let b_total_pct = 100.0 * cb.total_samples as f32 / denom_b;
            let (a_self_ms, b_self_ms, a_total_ms, b_total_ms, delta_self_ms, delta_total_ms) =
                if time_shaped {
                    let a_self = ca.self_samples as f64 * interval_a;
                    let b_self = cb.self_samples as f64 * interval_b;
                    let a_total = ca.total_samples as f64 * interval_a;
                    let b_total = cb.total_samples as f64 * interval_b;
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
                a_self_samples: ca.self_samples,
                a_self_pct,
                a_total_samples: ca.total_samples,
                a_total_pct,
                b_self_samples: cb.self_samples,
                b_self_pct,
                b_total_samples: cb.total_samples,
                b_total_pct,
                delta_self_pct: b_self_pct - a_self_pct,
                delta_total_pct: b_total_pct - a_total_pct,
                delta_self_samples: cb.self_samples as i64 - ca.self_samples as i64,
                delta_total_samples: cb.total_samples as i64 - ca.total_samples as i64,
                a_self_ms,
                b_self_ms,
                a_total_ms,
                b_total_ms,
                delta_self_ms,
                delta_total_ms,
            }
        })
        .collect();

    if let Some(threshold) = args.min_delta_pct {
        rows.retain(|r| r.delta_self_pct.abs() >= threshold);
    }

    rows.sort_by(|x, y| {
        let kx = sort_key(x, args.sort_by);
        let ky = sort_key(y, args.sort_by);
        ky.partial_cmp(&kx)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| x.function.cmp(&y.function))
            .then_with(|| x.module.cmp(&y.module))
    });

    let limit = if args.limit == 0 {
        DEFAULT_LIMIT
    } else {
        args.limit
    };
    rows.truncate(limit);
    for (i, r) in rows.iter_mut().enumerate() {
        r.rank = i + 1;
    }

    Ok(Output {
        a_total_samples: total_a,
        b_total_samples: total_b,
        filter: args.filter.clone(),
        sort_by: match args.sort_by {
            SortBy::Delta => "delta",
            SortBy::DeltaMs => "delta_ms",
            SortBy::A => "a",
            SortBy::B => "b",
        },
        event: args.event.label().to_owned(),
        functions: rows,
    })
}

fn sort_key(r: &DiffEntry, by: SortBy) -> f64 {
    match by {
        SortBy::Delta => r.delta_self_pct.abs() as f64,
        // The `compare_profiles` entry point rejects DeltaMs unless the
        // event is time-shaped, so `delta_self_ms` is guaranteed Some here.
        SortBy::DeltaMs => r
            .delta_self_ms
            .expect("DeltaMs sort gated on time-shaped event")
            .abs(),
        SortBy::A => r.a_self_pct as f64,
        SortBy::B => r.b_self_pct as f64,
    }
}

/// Re-key the per-side aggregator output for the outer-join according to
/// `align_by`. Always normalizes the module via [`strip_cargo_hash`] so
/// rebuilds of the same binary align even when keeping module in the key.
fn join_key(
    function: String,
    module: Option<String>,
    align_by: AlignBy,
) -> (String, Option<String>) {
    match align_by {
        AlignBy::Function => (function, None),
        AlignBy::FunctionAndModule => {
            let module = module.map(|m| strip_cargo_hash(&m).to_owned());
            (function, module)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Profile, raw::RawProfile};

    fn two_functions() -> Profile {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_functions.json")).unwrap();
        Profile::from_raw(raw)
    }

    #[test]
    fn identical_profiles_yield_zero_deltas() {
        // Comparing a profile to itself is the regression test — every row
        // must have zero delta on both axes.
        let p = two_functions();
        let out = compare_profiles(&p, &p, &Args::default()).unwrap();
        assert_eq!(out.a_total_samples, out.b_total_samples);
        assert!(!out.functions.is_empty());
        for row in &out.functions {
            assert_eq!(row.delta_self_pct, 0.0, "non-zero delta in {row:?}");
            assert_eq!(row.delta_total_pct, 0.0);
            assert_eq!(row.delta_self_samples, 0);
        }
    }

    #[test]
    fn function_only_in_b_appears_with_a_side_zero() {
        // Build B's `two_functions` profile; build A as the same profile but
        // with the leaf renamed so `hot` exists only in B.
        let mut raw_a: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_functions.json")).unwrap();
        // stringArray index 0 is "hot" — rename it so A no longer has it.
        raw_a.threads[0].string_array[0] = "hot_renamed_in_a".to_owned();
        let a = Profile::from_raw(raw_a);
        let b = two_functions();

        let out = compare_profiles(&a, &b, &Args::default()).unwrap();
        let row = out
            .functions
            .iter()
            .find(|r| r.function == "hot")
            .expect("hot should appear (only in B)");
        assert_eq!(row.a_self_samples, 0);
        assert!(row.b_self_samples > 0);
        assert!(row.delta_self_pct > 0.0);
    }

    #[test]
    fn min_delta_pct_filters_unmoved_rows() {
        // With identical profiles every row has delta=0; a >0 threshold
        // must drop them all.
        let p = two_functions();
        let out = compare_profiles(
            &p,
            &p,
            &Args {
                min_delta_pct: Some(0.1),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(out.functions.is_empty(), "{:?}", out.functions);
    }

    #[test]
    fn sort_by_delta_ms_orders_by_absolute_ms_movement() {
        // Same setup as `delta_ms_falls_when_wall_time_falls...`: A has
        // 100 samples (90 hot, 10 cold), B is truncated to 60 (60 hot, 0
        // cold). |delta_self_ms| is 30 for hot and 10 for cold, so hot
        // must rank first under DeltaMs.
        let a = two_functions();
        let mut raw_b: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_functions.json")).unwrap();
        let samples = &mut raw_b.threads[0].samples;
        samples.length = 60;
        samples.stack.truncate(60);
        samples.time_deltas.truncate(60);
        if let Some(w) = samples.weight.as_mut() {
            w.truncate(60);
        }
        let b = Profile::from_raw(raw_b);

        let out = compare_profiles(
            &a,
            &b,
            &Args {
                sort_by: SortBy::DeltaMs,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(out.sort_by, "delta_ms");
        assert_eq!(out.functions[0].function, "hot");
        assert!(
            out.functions[0].delta_self_ms.unwrap().abs()
                > out.functions[1].delta_self_ms.unwrap().abs()
        );
    }

    #[test]
    fn delta_ms_falls_when_wall_time_falls_even_if_share_rises() {
        // Fixture: A has 100 samples (90 hot, 10 cold), interval = 1ms.
        // Truncate B to the first 60 samples — all 60 are `hot` (the
        // fixture orders all hot samples first), so:
        //   hot:  share 90% → 100% (*rose*),    ms 90 → 60 (*fell* by 30)
        //   cold: share 10% → 0%   (fell),      ms 10 → 0  (fell by 10)
        // The hot row is the load-bearing case: pct says "got hotter",
        // ms correctly says "took less time". That's the column the user
        // is asking for.
        let a = two_functions();
        let mut raw_b: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_functions.json")).unwrap();
        let samples = &mut raw_b.threads[0].samples;
        samples.length = 60;
        samples.stack.truncate(60);
        samples.time_deltas.truncate(60);
        if let Some(w) = samples.weight.as_mut() {
            w.truncate(60);
        }
        let b = Profile::from_raw(raw_b);

        let out = compare_profiles(&a, &b, &Args::default()).unwrap();
        let hot = out
            .functions
            .iter()
            .find(|r| r.function == "hot")
            .expect("hot should appear");
        // pct rose...
        assert!(hot.delta_self_pct > 0.0, "{hot:?}");
        // ...but ms fell by exactly 30 (90 → 60 samples * 1ms interval).
        assert!(hot.delta_self_ms.unwrap() < 0.0, "{hot:?}");
        assert!((hot.delta_self_ms.unwrap() + 30.0).abs() < 1e-9, "{hot:?}");
    }

    #[test]
    fn strip_cargo_hash_strips_16_hex_suffix() {
        assert_eq!(strip_cargo_hash("simd_cols-1234567890abcdef"), "simd_cols");
        assert_eq!(strip_cargo_hash("foo-bar-fedcba9876543210"), "foo-bar");
    }

    #[test]
    fn strip_cargo_hash_leaves_non_hash_unchanged() {
        // Too short.
        assert_eq!(strip_cargo_hash("foo"), "foo");
        // 15 hex chars — not 16.
        assert_eq!(
            strip_cargo_hash("foo-123456789abcdef"),
            "foo-123456789abcdef"
        );
        // 17 hex chars — not 16.
        assert_eq!(
            strip_cargo_hash("foo-123456789abcdef01"),
            "foo-123456789abcdef01"
        );
        // Suffix has a non-hex char.
        assert_eq!(
            strip_cargo_hash("foo-1234567890abcdeg"),
            "foo-1234567890abcdeg"
        );
        // Uppercase hex (cargo emits lowercase) — leave alone to avoid
        // mangling user-visible names that happen to match the shape.
        assert_eq!(
            strip_cargo_hash("foo-1234567890ABCDEF"),
            "foo-1234567890ABCDEF"
        );
        // No leading dash before the hex run.
        assert_eq!(
            strip_cargo_hash("foo1234567890abcdef"),
            "foo1234567890abcdef"
        );
        // Empty stem — refuse to produce a bare hash.
        assert_eq!(strip_cargo_hash("-1234567890abcdef"), "-1234567890abcdef");
    }

    #[test]
    fn cargo_hash_suffixes_align_in_default_mode() {
        // Build A with module "bin-aaaaaaaaaaaaaaaa", B with the same
        // logical binary under "bin-bbbbbbbbbbbbbbbb". The default
        // `FunctionAndModule` mode strips both hashes, so each function
        // should appear as a single aligned row.
        let a = profile_with_module("bin-aaaaaaaaaaaaaaaa");
        let b = profile_with_module("bin-bbbbbbbbbbbbbbbb");

        let out = compare_profiles(&a, &b, &Args::default()).unwrap();
        assert_eq!(out.functions.len(), 2, "{:#?}", out.functions);
        for row in &out.functions {
            // Each function aligned: both sides have non-zero samples,
            // and the normalized module survives in the output.
            assert!(row.a_self_samples > 0 || row.a_total_samples > 0);
            assert!(row.b_self_samples > 0 || row.b_total_samples > 0);
            assert_eq!(row.module.as_deref(), Some("bin"));
        }
    }

    #[test]
    fn align_by_function_drops_module_from_key() {
        // Two profiles with completely different module names
        // ("alpha-binary" vs "beta-binary" — no cargo hash) but the same
        // function names. `align_by=Function` must collapse to one row
        // per function with the module field cleared.
        let a = profile_with_module("alpha-binary");
        let b = profile_with_module("beta-binary");

        let out = compare_profiles(
            &a,
            &b,
            &Args {
                align_by: AlignBy::Function,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(out.functions.len(), 2, "{:#?}", out.functions);
        for row in &out.functions {
            assert_eq!(row.module, None);
            assert!(row.a_self_samples > 0 || row.a_total_samples > 0);
            assert!(row.b_self_samples > 0 || row.b_total_samples > 0);
        }
    }

    #[test]
    fn compare_profiles_with_marker_event() {
        // A: cache-misses on hot stack (1 marker) + cold stack (1 marker).
        // B: same fixture but the first cache-miss marker is repointed
        // from the hot stack (idx 0) to the cold stack (idx 1). Cold
        // gains one cache-miss in B; hot loses one. Cycles distribution
        // is unchanged.
        use crate::profile::raw::{MarkerCause, RawMarkerData};
        let raw_a: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_events.json")).unwrap();
        let mut raw_b: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_events.json")).unwrap();
        raw_b.threads[0].markers.data[0] = Some(RawMarkerData {
            cause: Some(MarkerCause { stack: 1 }),
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
        assert_eq!(cold.delta_self_samples, 1, "{cold:?}");
        // Marker events are not time-shaped — ms columns must be None.
        assert!(cold.delta_self_ms.is_none(), "{cold:?}");
        assert!(cold.a_self_ms.is_none(), "{cold:?}");
    }

    #[test]
    fn delta_ms_sort_with_marker_event_errors() {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_events.json")).unwrap();
        let p = Profile::from_raw(raw);
        let err = compare_profiles(
            &p,
            &p,
            &Args {
                event: EventSource::Marker("cache-misses".into()),
                sort_by: SortBy::DeltaMs,
                ..Default::default()
            },
        )
        .unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("delta_ms"), "{msg}");
        assert!(msg.contains("cache-misses"), "{msg}");
    }

    /// Build a `two_functions` profile with a single library named
    /// `module_name` and wire every function in the thread to point at it,
    /// so `frame_info` reports `module_name` as the module for every
    /// frame. Used to exercise module-key normalization.
    fn profile_with_module(module_name: &str) -> Profile {
        use crate::profile::raw::RawLib;

        let mut raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_functions.json")).unwrap();

        let lib_idx = raw.libs.len();
        raw.libs.push(RawLib {
            name: Some(module_name.to_owned()),
            ..Default::default()
        });

        let thread = &mut raw.threads[0];
        // Resource table needs a row pointing at our new lib. Reuse an
        // existing string slot for the resource name to avoid disturbing
        // string indices the rest of the fixture references.
        let res_name_idx = 0;
        let res_idx = thread.resource_table.length as i32;
        thread.resource_table.length += 1;
        thread.resource_table.lib.push(Some(lib_idx));
        thread.resource_table.name.push(res_name_idx);
        thread.resource_table.host.push(None);
        thread.resource_table.type_.push(1);

        // Wire every function in the funcTable to that resource so each
        // frame resolves to the same module.
        for slot in &mut thread.func_table.resource {
            *slot = res_idx;
        }

        Profile::from_raw(raw)
    }

    #[test]
    fn sort_by_delta_puts_largest_movement_first() {
        // A has hot/cold with their original 90/10 split. B inverts it by
        // renaming to swap roles, so the deltas are ±large for both rows.
        let a = two_functions();
        let mut raw_b: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_functions.json")).unwrap();
        // Rename `cold` → `hot` and `hot` → `cold` to invert the split.
        raw_b.threads[0].string_array[0] = "cold".to_owned();
        raw_b.threads[0].string_array[1] = "hot".to_owned();
        let b = Profile::from_raw(raw_b);

        let out = compare_profiles(&a, &b, &Args::default()).unwrap();
        // Top row by |delta|: hot moved from 90% in A to 10% in B (or vice
        // versa for cold) — both rows tied at 80pp. Tie-breaker is lex on
        // function name, so `cold` lands first.
        assert!(!out.functions.is_empty());
        assert!(out.functions[0].delta_self_pct.abs() > 50.0);
    }
}
