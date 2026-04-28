//! Hierarchical call tree, with pruning to keep output LLM-digestible.

#![allow(dead_code)]

use crate::error::ToolError;
use crate::matching::FunctionMatcher;
use crate::profile::{Profile, ThreadHandle};
use crate::query::filters::Filter;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Args {
    pub filter_args: Filter,
    pub inverted: bool,
    pub root_function: Option<String>,
    pub paths_to: Option<String>,
    pub min_pct: f32,
    pub max_depth: u32,
    pub max_breadth: u32,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            filter_args: Filter::default(),
            inverted: false,
            root_function: None,
            paths_to: None,
            min_pct: 1.0,
            max_depth: 8,
            max_breadth: 5,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Output {
    pub thread: Option<String>,
    pub total_samples: u64,
    pub pruning: PruningKnobs,
    pub tree: Option<Node>,
}

#[derive(Debug, Serialize, Clone)]
pub struct PruningKnobs {
    pub min_pct: f32,
    pub max_depth: u32,
    pub max_breadth: u32,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Node {
    Frame(FrameNode),
    Omitted {
        #[allow(non_snake_case)]
        _omitted: OmittedSummary,
    },
    Truncated {
        #[allow(non_snake_case)]
        _truncated: TruncatedSummary,
    },
}

#[derive(Debug, Serialize)]
pub struct FrameNode {
    pub function: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    pub self_samples: u64,
    pub self_pct: f32,
    pub total_samples: u64,
    pub total_pct: f32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
}

#[derive(Debug, Serialize)]
pub struct OmittedSummary {
    pub count: u32,
    pub combined_pct: f32,
}

#[derive(Debug, Serialize)]
pub struct TruncatedSummary {
    pub deepest_descendant_pct: f32,
}

#[derive(Default)]
struct AggNode {
    self_samples: u64,
    total_samples: u64,
    children: HashMap<(String, Option<String>), AggNode>,
}

pub fn call_tree(profile: &Profile, args: &Args) -> Result<Output, ToolError> {
    args.filter_args.validate_thread(profile)?;
    let _root_matcher = args
        .root_function
        .as_deref()
        .map(FunctionMatcher::new)
        .transpose()
        .map_err(|e| ToolError::Internal { message: e.to_string() })?;
    let _paths_to = args
        .paths_to
        .as_deref()
        .map(FunctionMatcher::new)
        .transpose()
        .map_err(|e| ToolError::Internal { message: e.to_string() })?;

    let mut root = AggNode::default();
    let mut total_samples: u64 = 0;

    for handle in args.filter_args.threads(profile) {
        accumulate(profile, handle, args.inverted, &mut root, &mut total_samples);
    }

    let tree = build_node(&root, total_samples, "ROOT".into(), None, args, 0);

    Ok(Output {
        thread: None,
        total_samples,
        pruning: PruningKnobs {
            min_pct: args.min_pct,
            max_depth: args.max_depth,
            max_breadth: args.max_breadth,
        },
        tree,
    })
}

fn accumulate(
    profile: &Profile,
    handle: ThreadHandle,
    inverted: bool,
    root: &mut AggNode,
    total_samples: &mut u64,
) {
    let raw = profile.raw_thread(handle);
    for &stack_opt in &raw.samples.stack {
        let Some(stack_idx) = stack_opt else { continue };
        *total_samples += 1;
        let mut frames: Vec<usize> = profile.walk_stack(handle, stack_idx).collect();
        if !inverted {
            frames.reverse();
        }
        let mut node: &mut AggNode = root;
        let len = frames.len();
        for (i, frame_idx) in frames.iter().enumerate() {
            let info = match profile.frame_info(handle, *frame_idx) {
                Some(fi) => fi,
                None => continue,
            };
            let key = (info.function_name.to_owned(), info.module_name.map(str::to_owned));
            node = node.children.entry(key).or_default();
            node.total_samples += 1;
            if i + 1 == len {
                node.self_samples += 1;
            }
        }
    }
}

fn build_node(
    agg: &AggNode,
    total_samples: u64,
    function: String,
    module: Option<String>,
    args: &Args,
    depth: u32,
) -> Option<Node> {
    let total = total_samples.max(1) as f32;
    let total_pct = 100.0 * agg.total_samples as f32 / total;
    if total_pct < args.min_pct && depth > 0 {
        return None;
    }
    if depth > args.max_depth {
        return Some(Node::Truncated {
            _truncated: TruncatedSummary { deepest_descendant_pct: total_pct },
        });
    }

    let mut child_entries: Vec<(&(String, Option<String>), &AggNode)> =
        agg.children.iter().collect();
    child_entries.sort_by(|a, b| {
        b.1.total_samples
            .cmp(&a.1.total_samples)
            .then_with(|| a.0 .0.cmp(&b.0 .0))
            .then_with(|| a.0 .1.cmp(&b.0 .1))
    });

    let mut children = Vec::new();
    let mut omitted_count: u32 = 0;
    let mut omitted_samples: u64 = 0;
    for (i, (key, child_agg)) in child_entries.iter().enumerate() {
        let mut emit = true;
        if i as u32 >= args.max_breadth {
            emit = false;
        }
        if 100.0 * child_agg.total_samples as f32 / total < args.min_pct {
            emit = false;
        }
        if emit {
            if let Some(node) = build_node(
                child_agg,
                total_samples,
                key.0.clone(),
                key.1.clone(),
                args,
                depth + 1,
            ) {
                children.push(node);
            } else {
                omitted_count += 1;
                omitted_samples += child_agg.total_samples;
            }
        } else {
            omitted_count += 1;
            omitted_samples += child_agg.total_samples;
        }
    }
    if omitted_count > 0 {
        children.push(Node::Omitted {
            _omitted: OmittedSummary {
                count: omitted_count,
                combined_pct: 100.0 * omitted_samples as f32 / total,
            },
        });
    }

    if depth == 0 && agg.children.is_empty() {
        return None;
    }

    Some(Node::Frame(FrameNode {
        function,
        module,
        self_samples: agg.self_samples,
        self_pct: 100.0 * agg.self_samples as f32 / total,
        total_samples: agg.total_samples,
        total_pct,
        children,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{raw::RawProfile, Profile};

    fn fixture() -> Profile {
        let raw: RawProfile = serde_json::from_str(include_str!(
            "../../tests/fixtures/two_functions.json"
        ))
        .unwrap();
        Profile::from_raw(raw)
    }

    #[test]
    fn builds_tree_with_two_top_level_functions() {
        let p = fixture();
        let tree = call_tree(&p, &Args { min_pct: 0.0, ..Default::default() }).unwrap();
        assert!(tree.tree.is_some());
    }
}
