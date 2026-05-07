//! `folded_stacks`: flamegraph-folded text export.
//!
//! Output is one line per unique stack:
//! ```text
//! root;child;...;leaf <samples>
//! ```
//! Compatible with inferno (`inferno-flamegraph`) and easy to diff with `comm`.

#![allow(dead_code)]

use crate::error::ToolError;
use crate::matching::optional_matcher;
use crate::profile::Profile;
use crate::query::event::EventSource;
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

/// Folded entries with their sample counts. Kept as a structured list
/// (rather than the joined string) so the budget trimmer can drop
/// individual lines by sample count and recompute the rendered text.
#[derive(Debug, Clone, Default)]
pub struct Folded {
    pub entries: Vec<FoldedEntry>,
    /// Sum of samples represented across all entries — denominator for
    /// any "% dropped" rollup the trimmer reports.
    pub total_samples: u64,
}

#[derive(Debug, Clone)]
pub struct FoldedEntry {
    pub stack: String,
    pub samples: u64,
}

impl Folded {
    /// Render the folded text in the canonical `root;...;leaf <samples>`
    /// shape, sorted by stack string for diffability with `comm`.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let mut sorted: Vec<&FoldedEntry> = self.entries.iter().collect();
        sorted.sort_by(|a, b| a.stack.cmp(&b.stack));
        for e in sorted {
            out.push_str(&e.stack);
            out.push(' ');
            out.push_str(&e.samples.to_string());
            out.push('\n');
        }
        out
    }
}

/// Backwards-compatible string rendering. Prefer
/// [`folded_stacks_structured`] when budget trimming is needed.
pub fn folded_stacks(profile: &Profile, args: &Args) -> Result<String, ToolError> {
    Ok(folded_stacks_structured(profile, args)?.render())
}

pub fn folded_stacks_structured(profile: &Profile, args: &Args) -> Result<Folded, ToolError> {
    args.filter_args.validate_process(profile)?;
    args.filter_args.validate_thread(profile)?;
    args.filter_args.validate_time_range(profile)?;

    let matcher = optional_matcher("function_filter", args.function_filter.as_deref())?;

    let mut counts: HashMap<String, u64> = HashMap::new();
    let mut total_samples: u64 = 0;

    for handle in args.filter_args.threads(profile) {
        for stack_opt in
            profile.stack_indices(handle, &EventSource::Samples, args.filter_args.time_range)
        {
            let Some(stack_idx) = stack_opt else { continue };
            // resolved_chain is root-to-leaf with view transforms applied —
            // exactly the orientation flamegraph-folded format expects.
            let mut any_match = matcher.is_none();
            let frames: Vec<String> = profile
                .resolved_chain(handle, stack_idx, false)
                .into_iter()
                .map(|f| {
                    if let Some(m) = matcher.as_ref()
                        && m.matches(&f.function)
                    {
                        any_match = true;
                    }
                    // Replace any `;` in the function name so the folded
                    // delimiter stays unambiguous.
                    f.function.replace(';', ":")
                })
                .collect();
            if !any_match {
                continue;
            }
            if frames.is_empty() {
                continue;
            }
            *counts.entry(frames.join(";")).or_default() += 1;
            total_samples += 1;
        }
    }

    let mut entries: Vec<FoldedEntry> = counts
        .into_iter()
        .map(|(stack, samples)| FoldedEntry { stack, samples })
        .collect();
    // Sort by stack string for deterministic rendering. The budget
    // trimmer below sorts by sample count when it needs to drop.
    entries.sort_by(|a, b| a.stack.cmp(&b.stack));
    Ok(Folded {
        entries,
        total_samples,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Profile, raw::RawProfile};

    fn fixture(path: &str) -> Profile {
        let raw: RawProfile = match path {
            "two_functions" => {
                serde_json::from_str(include_str!("../../tests/fixtures/two_functions.json"))
                    .unwrap()
            }
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
