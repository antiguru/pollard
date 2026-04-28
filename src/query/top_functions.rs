//! `top_functions` aggregation: flat top-N by self or total samples.

#![allow(dead_code)]

use crate::error::ToolError;
use crate::matching::FunctionMatcher;
use crate::profile::Profile;
use crate::query::event::{self, EventSource};
use crate::query::filters::Filter;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct Args {
    pub filter: Option<String>,
    pub limit: usize,
    pub sort_by: SortBy,
    pub filter_args: Filter,
    /// When true, fan each native frame out into its DWARF inline chain
    /// (innermost-first when walking leaf-to-root), so self-time attributes
    /// to the deepest inlined callee instead of the enclosing function.
    pub expand_inlines: bool,
    /// Which per-sample event drives the aggregation. Default
    /// [`EventSource::Samples`] (samply puts cycles there); pass
    /// [`EventSource::Marker`] to drill into hardware-counter markers
    /// like `cache-misses`.
    pub event: EventSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortBy {
    #[default]
    SelfTime,
    TotalTime,
    /// `total_samples - self_samples`. Surfaces wrappers and dispatchers
    /// that aren't themselves hot but call into hot code; useful when the
    /// self-time list is dominated by leaf allocator/syscall frames.
    Descendants,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub thread: Option<String>,
    pub process: Option<String>,
    pub total_samples: u64,
    pub filter: Option<String>,
    pub sort_by: &'static str,
    /// Echo of the resolved event source — `"samples"` for the default
    /// track or the marker name (e.g. `"cache-misses"`). The pct
    /// columns are percentages of this event's total count.
    pub event: String,
    pub functions: Vec<FunctionEntry>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FunctionEntry {
    pub rank: usize,
    pub function: String,
    pub module: Option<String>,
    pub self_samples: u64,
    pub self_pct: f32,
    pub total_samples: u64,
    pub total_pct: f32,
}

const DEFAULT_LIMIT: usize = 30;

#[derive(Default, Clone)]
pub(crate) struct Counts {
    pub(crate) self_samples: u64,
    pub(crate) total_samples: u64,
}

/// Per-function `(self, total)` sample counts plus profile-wide total.
/// Shared between [`top_functions`] and [`crate::query::compare`] so cross-
/// profile diffs see the same aggregation rules.
pub(crate) fn aggregate_functions(
    profile: &Profile,
    filter: Option<&str>,
    filter_args: &Filter,
    expand_inlines: bool,
    event: &EventSource,
) -> Result<(HashMap<(String, Option<String>), Counts>, u64), ToolError> {
    aggregate_grouped(
        profile,
        filter,
        filter_args,
        expand_inlines,
        event,
        |f, m, _| Some((f.to_owned(), m.map(str::to_owned))),
    )
}

/// Aggregate self/total sample counts under a caller-provided key extractor.
/// Returning `None` from `key_fn` skips the frame — used by groupings (e.g.
/// `file`, `directory`) where some frames lack the underlying metadata.
///
/// The matcher in `filter` still applies to the function name regardless of
/// what's used as the grouping key, so callers can scope a group-by-module
/// query to only the functions they care about.
///
/// `event` selects which per-sample stream to walk: the default samples
/// track (cycles) or a name-filtered slice of the marker stream
/// (cache-misses, branch-misses, instructions, …). Both shapes go through
/// the same outer loop because [`event::stack_indices`] yields the same
/// `Option<usize>` shape as `samples.stack` directly.
pub(crate) fn aggregate_grouped<K, F>(
    profile: &Profile,
    filter: Option<&str>,
    filter_args: &Filter,
    expand_inlines: bool,
    event: &EventSource,
    mut key_fn: F,
) -> Result<(HashMap<K, Counts>, u64), ToolError>
where
    K: std::hash::Hash + Eq + Clone,
    F: FnMut(&str, Option<&str>, Option<&str>) -> Option<K>,
{
    filter_args.validate_thread(profile)?;
    let matcher = match filter {
        Some(p) => Some(FunctionMatcher::new(p).map_err(|e| ToolError::Internal {
            message: e.to_string(),
        })?),
        None => None,
    };

    let mut counts: HashMap<K, Counts> = HashMap::new();
    let mut total_samples: u64 = 0;

    for handle in filter_args.threads(profile) {
        for stack_opt in event::stack_indices(profile, handle, event) {
            let Some(stack_idx) = stack_opt else { continue };
            total_samples += 1;

            // Build the leaf-to-root chain. When expand_inlines is set, fan
            // each native frame out into its DWARF inline chain
            // innermost-first BEFORE the native name, so the deepest
            // inlined callee becomes the leaf and gets self-time.
            let mut entries: Vec<(String, Option<String>, Option<String>)> = Vec::new();
            for frame_idx in profile.walk_stack(handle, stack_idx) {
                let Some(info) = profile.frame_info(handle, frame_idx) else {
                    continue;
                };
                let module = info.module_name.map(str::to_owned);
                if expand_inlines {
                    for inl in profile.inline_chain(handle, frame_idx) {
                        entries.push((inl.function.clone(), module.clone(), inl.file.clone()));
                    }
                }
                entries.push((
                    info.function_name.to_owned(),
                    module,
                    info.file.map(str::to_owned),
                ));
            }

            let mut iter = entries.into_iter();
            let mut seen_in_stack: std::collections::HashSet<K> = Default::default();
            if let Some((func, module, file)) = iter.next()
                && matcher.as_ref().is_none_or(|m| m.matches(&func))
                && let Some(k) = key_fn(&func, module.as_deref(), file.as_deref())
            {
                let entry = counts.entry(k.clone()).or_default();
                entry.self_samples += 1;
                entry.total_samples += 1;
                seen_in_stack.insert(k);
            }
            for (func, module, file) in iter {
                if matcher.as_ref().is_none_or(|m| m.matches(&func))
                    && let Some(k) = key_fn(&func, module.as_deref(), file.as_deref())
                    && seen_in_stack.insert(k.clone())
                {
                    counts.entry(k).or_default().total_samples += 1;
                }
            }
        }
    }

    Ok((counts, total_samples))
}

pub fn top_functions(profile: &Profile, args: &Args) -> Result<Output, ToolError> {
    let (counts, total_samples) = aggregate_functions(
        profile,
        args.filter.as_deref(),
        &args.filter_args,
        args.expand_inlines,
        &args.event,
    )?;

    // Build output
    let mut entries: Vec<((String, Option<String>), Counts)> = counts.into_iter().collect();
    let key = |c: &Counts| match args.sort_by {
        SortBy::SelfTime => c.self_samples,
        SortBy::TotalTime => c.total_samples,
        SortBy::Descendants => c.total_samples.saturating_sub(c.self_samples),
    };
    entries.sort_by(|a, b| {
        key(&b.1)
            .cmp(&key(&a.1))
            .then_with(|| a.0.0.cmp(&b.0.0))
            .then_with(|| a.0.1.cmp(&b.0.1))
    });

    let limit = if args.limit == 0 {
        DEFAULT_LIMIT
    } else {
        args.limit
    };
    let total = total_samples.max(1) as f32;
    let functions: Vec<_> = entries
        .into_iter()
        .take(limit)
        .enumerate()
        .map(|(i, ((function, module), c))| FunctionEntry {
            rank: i + 1,
            function,
            module,
            self_samples: c.self_samples,
            self_pct: 100.0 * c.self_samples as f32 / total,
            total_samples: c.total_samples,
            total_pct: 100.0 * c.total_samples as f32 / total,
        })
        .collect();

    Ok(Output {
        thread: None,
        process: None,
        total_samples,
        filter: args.filter.clone(),
        sort_by: match args.sort_by {
            SortBy::SelfTime => "self",
            SortBy::TotalTime => "total",
            SortBy::Descendants => "descendants",
        },
        event: args.event.label().to_owned(),
        functions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Profile, raw::RawProfile};

    fn raw_with_two_functions() -> RawProfile {
        serde_json::from_str(include_str!("../../tests/fixtures/two_functions.json")).unwrap()
    }

    #[test]
    fn ranks_by_self_samples() {
        let profile = Profile::from_raw(raw_with_two_functions());
        let result = top_functions(&profile, &Args::default()).unwrap();
        assert_eq!(result.functions[0].function, "hot");
        assert_eq!(result.functions[0].self_samples, 90);
        assert_eq!(result.functions[1].function, "cold");
        assert_eq!(result.functions[1].self_samples, 10);
    }

    #[test]
    fn limit_truncates() {
        let profile = Profile::from_raw(raw_with_two_functions());
        let result = top_functions(
            &profile,
            &Args {
                limit: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(result.functions.len(), 1);
    }

    #[test]
    fn expand_inlines_attributes_self_to_innermost_inline() {
        // linear_chain.json: a → b → c → d, 100 samples on leaf `d`.
        // Inject one inline record on `d` so wholesym would have produced
        // [leaf_inline (innermost), d (outer)]. With expand_inlines, the
        // self-time should attribute to `leaf_inline`, not `d`.
        use crate::profile::raw::InlineFrame;
        let mut raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/linear_chain.json")).unwrap();
        let t = &mut raw.threads[0];
        t.inline_chains.resize_with(t.frame_table.length, Vec::new);
        t.inline_chains[3] = vec![InlineFrame {
            function: "leaf_inline".into(),
            file: None,
            line: None,
        }];
        let profile = Profile::from_raw(raw);

        let plain = top_functions(&profile, &Args::default()).unwrap();
        // Without expansion, `d` owns all 100 self samples.
        assert_eq!(plain.functions[0].function, "d");
        assert_eq!(plain.functions[0].self_samples, 100);
        assert!(!plain.functions.iter().any(|e| e.function == "leaf_inline"));

        let expanded = top_functions(
            &profile,
            &Args {
                expand_inlines: true,
                ..Default::default()
            },
        )
        .unwrap();
        let leaf = expanded
            .functions
            .iter()
            .find(|e| e.function == "leaf_inline")
            .expect("leaf_inline must appear");
        assert_eq!(leaf.self_samples, 100);
        // `d` becomes a non-self ancestor; its self_samples drop to 0.
        let d = expanded
            .functions
            .iter()
            .find(|e| e.function == "d")
            .expect("d must still appear in totals");
        assert_eq!(d.self_samples, 0);
        assert_eq!(d.total_samples, 100);
    }

    #[test]
    fn descendants_sort_pushes_leaf_to_bottom() {
        // linear_chain.json: a → b → c → d, all 100 samples land on leaf `d`.
        // self:      a=0   b=0   c=0   d=100
        // total:     a=100 b=100 c=100 d=100
        // descend.:  a=100 b=100 c=100 d=0
        // The leaf must rank last under descendants sort.
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/linear_chain.json")).unwrap();
        let profile = Profile::from_raw(raw);
        let result = top_functions(
            &profile,
            &Args {
                sort_by: SortBy::Descendants,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(result.sort_by, "descendants");
        assert_eq!(
            result.functions.last().map(|e| e.function.as_str()),
            Some("d"),
            "leaf must rank last under descendants sort, got {:?}",
            result
                .functions
                .iter()
                .map(|e| &e.function)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn aggregates_marker_event_by_name() {
        // two_events.json: 2 cache-miss markers — one on hot's stack,
        // one on cold's. With event=cache-misses each function should
        // own one self_sample and total=2.
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_events.json")).unwrap();
        let profile = Profile::from_raw(raw);
        let result = top_functions(
            &profile,
            &Args {
                event: EventSource::Marker("cache-misses".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(result.event, "cache-misses");
        assert_eq!(result.total_samples, 2);
        let hot = result
            .functions
            .iter()
            .find(|f| f.function == "hot")
            .unwrap();
        let cold = result
            .functions
            .iter()
            .find(|f| f.function == "cold")
            .unwrap();
        assert_eq!(hot.self_samples, 1);
        assert_eq!(cold.self_samples, 1);
    }

    #[test]
    fn defaults_to_samples_event() {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_events.json")).unwrap();
        let profile = Profile::from_raw(raw);
        let result = top_functions(&profile, &Args::default()).unwrap();
        assert_eq!(result.event, "samples");
        assert_eq!(result.total_samples, 4);
    }

    #[test]
    fn filter_substring_restricts() {
        let profile = Profile::from_raw(raw_with_two_functions());
        let result = top_functions(
            &profile,
            &Args {
                filter: Some("hot".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(result.functions.len(), 1);
        assert_eq!(result.functions[0].function, "hot");
    }
}
