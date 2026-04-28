//! `compare_profiles`: per-function delta between two profiles.
//!
//! Aligns by `(function, module)` and emits self/total sample counts and
//! percentages from each side plus their delta. The percentage delta is
//! the load-bearing column for benchmark comparisons — sample counts move
//! with profile duration, percentages are roughly normalized.

#![allow(dead_code)]

use crate::error::ToolError;
use crate::profile::Profile;
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortBy {
    /// `|delta_self_pct|` descending (default — surfaces what moved most).
    #[default]
    Delta,
    /// Profile A's self-pct descending.
    A,
    /// Profile B's self-pct descending.
    B,
}

const DEFAULT_LIMIT: usize = 30;

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub a_total_samples: u64,
    pub b_total_samples: u64,
    pub filter: Option<String>,
    pub sort_by: &'static str,
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
}

pub fn compare_profiles(a: &Profile, b: &Profile, args: &Args) -> Result<Output, ToolError> {
    let (counts_a, total_a) = aggregate_functions(
        a,
        args.filter.as_deref(),
        &args.filter_args,
        args.expand_inlines,
    )?;
    let (counts_b, total_b) = aggregate_functions(
        b,
        args.filter.as_deref(),
        &args.filter_args,
        args.expand_inlines,
    )?;

    // Outer-join on (function, module). Every key in either side gets a row;
    // missing-side counts default to zero so downstream pct math stays
    // well-defined.
    let mut joined: HashMap<(String, Option<String>), (Counts, Counts)> = HashMap::new();
    for (k, c) in counts_a {
        joined.entry(k).or_default().0 = c;
    }
    for (k, c) in counts_b {
        joined.entry(k).or_default().1 = c;
    }

    let denom_a = total_a.max(1) as f32;
    let denom_b = total_b.max(1) as f32;

    let mut rows: Vec<DiffEntry> = joined
        .into_iter()
        .map(|((function, module), (ca, cb))| {
            let a_self_pct = 100.0 * ca.self_samples as f32 / denom_a;
            let b_self_pct = 100.0 * cb.self_samples as f32 / denom_b;
            let a_total_pct = 100.0 * ca.total_samples as f32 / denom_a;
            let b_total_pct = 100.0 * cb.total_samples as f32 / denom_b;
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
            SortBy::A => "a",
            SortBy::B => "b",
        },
        functions: rows,
    })
}

fn sort_key(r: &DiffEntry, by: SortBy) -> f32 {
    match by {
        SortBy::Delta => r.delta_self_pct.abs(),
        SortBy::A => r.a_self_pct,
        SortBy::B => r.b_self_pct,
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
