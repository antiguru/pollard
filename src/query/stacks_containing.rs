//! `stacks_containing`: distinct full stacks that include a frame matching `function`.

#![allow(dead_code)]

use crate::error::ToolError;
use crate::matching::FunctionMatcher;
use crate::profile::Profile;
use crate::query::filters::Filter;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct Args {
    pub filter_args: Filter,
    pub function: String,
    pub limit: usize,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub function_filter: String,
    pub matched_frame_samples: u64,
    pub matched_pct: f32,
    pub unique_stacks_total: usize,
    pub stacks_returned: usize,
    pub stacks: Vec<StackOutput>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct StackOutput {
    pub samples: u64,
    pub pct: f32,
    pub frames: Vec<FrameOutput>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FrameOutput {
    pub function: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub matched: bool,
}

const DEFAULT_LIMIT: usize = 20;

pub fn stacks_containing(profile: &Profile, args: &Args) -> Result<Output, ToolError> {
    args.filter_args.validate_thread(profile)?;
    let matcher = FunctionMatcher::new(&args.function)
        .map_err(|e| ToolError::Internal { message: e.to_string() })?;

    type StackKey = Vec<(String, Option<String>, bool)>;
    let mut counts: HashMap<StackKey, u64> = HashMap::new();
    let mut total_samples: u64 = 0;
    let mut matched_frame_samples: u64 = 0;

    for handle in args.filter_args.threads(profile) {
        let raw = profile.raw_thread(handle);
        for &stack_opt in &raw.samples.stack {
            let Some(stack_idx) = stack_opt else { continue };
            total_samples += 1;
            let mut frames: Vec<(String, Option<String>, bool)> = Vec::new();
            let mut any_match = false;
            for frame_idx in profile.walk_stack(handle, stack_idx) {
                if let Some(info) = profile.frame_info(handle, frame_idx) {
                    let m = matcher.matches(info.function_name);
                    any_match |= m;
                    frames.push((
                        info.function_name.to_owned(),
                        info.module_name.map(str::to_owned),
                        m,
                    ));
                }
            }
            // walk_stack is leaf-to-root; reverse to root-to-leaf
            frames.reverse();
            if any_match {
                matched_frame_samples += 1;
                *counts.entry(frames).or_default() += 1;
            }
        }
    }

    let mut entries: Vec<(StackKey, u64)> = counts.into_iter().collect();
    entries.sort_by(|a, b| {
        b.1.cmp(&a.1).then_with(|| {
            for (fa, fb) in a.0.iter().zip(b.0.iter()) {
                let cmp = fa.0.cmp(&fb.0);
                if cmp != std::cmp::Ordering::Equal {
                    return cmp;
                }
            }
            a.0.len().cmp(&b.0.len())
        })
    });

    let unique_stacks_total = entries.len();
    let limit = if args.limit == 0 { DEFAULT_LIMIT } else { args.limit };
    let total = total_samples.max(1) as f32;
    let stacks: Vec<StackOutput> = entries
        .into_iter()
        .take(limit)
        .map(|(frames, samples)| StackOutput {
            samples,
            pct: 100.0 * samples as f32 / total,
            frames: frames
                .into_iter()
                .map(|(function, module, matched)| FrameOutput { function, module, matched })
                .collect(),
        })
        .collect();

    Ok(Output {
        function_filter: args.function.clone(),
        matched_frame_samples,
        matched_pct: 100.0 * matched_frame_samples as f32 / total,
        unique_stacks_total,
        stacks_returned: stacks.len(),
        stacks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{raw::RawProfile, Profile};

    #[test]
    fn returns_distinct_stacks_with_matched_flag() {
        // Fixture has 3 distinct stacks. Two contain "alloc"; one does not.
        let raw: RawProfile = serde_json::from_str(include_str!(
            "../../tests/fixtures/stacks_containing.json"
        ))
        .unwrap();
        let profile = Profile::from_raw(raw);
        let result = stacks_containing(
            &profile,
            &Args { function: "alloc".into(), ..Default::default() },
        )
        .unwrap();
        assert_eq!(result.unique_stacks_total, 2);
        assert!(result.stacks.iter().all(|s| s.frames.iter().any(|f| f.matched)));
    }
}
