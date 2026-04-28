//! `folded_stacks`: flamegraph-folded text export.
//!
//! Output is one line per unique stack:
//! ```text
//! root;child;...;leaf <samples>
//! ```
//! Compatible with inferno (`inferno-flamegraph`) and easy to diff with `comm`.

#![allow(dead_code)]

use crate::error::ToolError;
use crate::matching::FunctionMatcher;
use crate::profile::Profile;
use crate::query::filters::Filter;
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct Args {
    pub filter_args: Filter,
    /// Optional function-name filter; when set, only stacks that contain at
    /// least one matching frame are emitted (the full stack is still
    /// preserved in the line — we don't trim, just gate inclusion).
    pub function_filter: Option<String>,
}

pub fn folded_stacks(profile: &Profile, args: &Args) -> Result<String, ToolError> {
    args.filter_args.validate_thread(profile)?;

    let matcher = match args.function_filter.as_deref() {
        Some(p) => Some(FunctionMatcher::new(p).map_err(|e| ToolError::Internal {
            message: e.to_string(),
        })?),
        None => None,
    };

    let mut counts: HashMap<String, u64> = HashMap::new();

    for handle in args.filter_args.threads(profile) {
        let raw = profile.raw_thread(handle);
        for &stack_opt in &raw.samples.stack {
            let Some(stack_idx) = stack_opt else { continue };
            let mut frames: Vec<String> = Vec::new();
            let mut any_match = matcher.is_none();
            for frame_idx in profile.walk_stack(handle, stack_idx) {
                if let Some(info) = profile.frame_info(handle, frame_idx) {
                    if let Some(m) = matcher.as_ref()
                        && m.matches(info.function_name)
                    {
                        any_match = true;
                    }
                    // Replace any `;` in the function name so the folded
                    // delimiter stays unambiguous.
                    frames.push(info.function_name.replace(';', ":"));
                }
            }
            if !any_match {
                continue;
            }
            // walk_stack is leaf-to-root; folded format wants root-to-leaf.
            frames.reverse();
            if frames.is_empty() {
                continue;
            }
            *counts.entry(frames.join(";")).or_default() += 1;
        }
    }

    // Sort by stack string for determinism (also makes diffing trivial).
    let mut entries: Vec<(String, u64)> = counts.into_iter().collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut out = String::new();
    for (stack, n) in entries {
        out.push_str(&stack);
        out.push(' ');
        out.push_str(&n.to_string());
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Profile, raw::RawProfile};

    fn fixture(path: &str) -> Profile {
        let raw: RawProfile = match path {
            "two_functions" => serde_json::from_str(include_str!(
                "../../tests/fixtures/two_functions.json"
            ))
            .unwrap(),
            "linear_chain" => {
                serde_json::from_str(include_str!("../../tests/fixtures/linear_chain.json"))
                    .unwrap()
            }
            _ => panic!("unknown fixture"),
        };
        Profile::from_raw(raw)
    }

    #[test]
    fn linear_chain_emits_single_root_to_leaf_line() {
        // a → b → c → d, 100 samples on the leaf stack.
        let p = fixture("linear_chain");
        let text = folded_stacks(&p, &Args::default()).unwrap();
        assert_eq!(text, "a;b;c;d 100\n");
    }

    #[test]
    fn two_unique_stacks_emit_two_lines_sorted() {
        // hot=90 self, cold=10 self. Both single-frame stacks.
        let p = fixture("two_functions");
        let text = folded_stacks(&p, &Args::default()).unwrap();
        // Sorted by stack name: cold before hot.
        assert_eq!(text, "cold 10\nhot 90\n");
    }

    #[test]
    fn function_filter_gates_inclusion() {
        let p = fixture("two_functions");
        let text = folded_stacks(
            &p,
            &Args {
                function_filter: Some("hot".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(text, "hot 90\n");
    }
}
