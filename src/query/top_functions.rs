//! `top_functions` aggregation: flat top-N by self or total samples.

#![allow(dead_code)]

use crate::error::ToolError;
use crate::matching::FunctionMatcher;
use crate::profile::{Profile, ThreadHandle};
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortBy {
    #[default]
    SelfTime,
    TotalTime,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub thread: Option<String>,
    pub process: Option<String>,
    pub total_samples: u64,
    pub filter: Option<String>,
    pub sort_by: &'static str,
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

#[derive(Default)]
struct Counts {
    self_samples: u64,
    total_samples: u64,
}

pub fn top_functions(profile: &Profile, args: &Args) -> Result<Output, ToolError> {
    args.filter_args.validate_thread(profile)?;
    let matcher = match args.filter.as_deref() {
        Some(p) => Some(FunctionMatcher::new(p).map_err(|e| ToolError::Internal {
            message: e.to_string(),
        })?),
        None => None,
    };

    let mut counts: HashMap<(String, Option<String>), Counts> = HashMap::new();
    let mut total_samples: u64 = 0;

    for handle in args.filter_args.threads(profile) {
        accumulate_thread(
            profile,
            handle,
            args.sort_by,
            &matcher,
            &mut counts,
            &mut total_samples,
        );
    }

    // Build output
    let mut entries: Vec<((String, Option<String>), Counts)> = counts.into_iter().collect();
    entries.sort_by(|a, b| {
        let ka = match args.sort_by {
            SortBy::SelfTime => a.1.self_samples,
            SortBy::TotalTime => a.1.total_samples,
        };
        let kb = match args.sort_by {
            SortBy::SelfTime => b.1.self_samples,
            SortBy::TotalTime => b.1.total_samples,
        };
        kb.cmp(&ka)
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
        },
        functions,
    })
}

fn accumulate_thread(
    profile: &Profile,
    handle: ThreadHandle,
    _sort_by: SortBy,
    matcher: &Option<FunctionMatcher>,
    counts: &mut HashMap<(String, Option<String>), Counts>,
    total_samples: &mut u64,
) {
    let raw = profile.raw_thread(handle);
    for &stack_opt in &raw.samples.stack {
        let Some(stack_idx) = stack_opt else { continue };
        *total_samples += 1;
        let mut frames = profile.walk_stack(handle, stack_idx);
        let mut seen_in_stack: std::collections::HashSet<(String, Option<String>)> =
            Default::default();
        if let Some(leaf_frame_idx) = frames.next()
            && let Some(info) = profile.frame_info(handle, leaf_frame_idx)
            && matcher
                .as_ref()
                .is_none_or(|m| m.matches(info.function_name))
        {
            let key = (
                info.function_name.to_owned(),
                info.module_name.map(str::to_owned),
            );
            counts.entry(key.clone()).or_default().self_samples += 1;
            counts.entry(key.clone()).or_default().total_samples += 1;
            seen_in_stack.insert(key);
        }
        for frame_idx in frames {
            if let Some(info) = profile.frame_info(handle, frame_idx)
                && matcher
                    .as_ref()
                    .is_none_or(|m| m.matches(info.function_name))
            {
                let key = (
                    info.function_name.to_owned(),
                    info.module_name.map(str::to_owned),
                );
                if seen_in_stack.insert(key.clone()) {
                    counts.entry(key).or_default().total_samples += 1;
                }
            }
        }
    }
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
