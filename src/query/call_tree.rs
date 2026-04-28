//! Hierarchical call tree, with pruning to keep output LLM-digestible.

#![allow(dead_code)]

use crate::error::ToolError;
use crate::matching::{FunctionMatcher, matcher_to_string, nearest_function_names};
use crate::profile::{Profile, ThreadHandle};
use crate::query::filters::Filter;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Args {
    pub filter_args: Filter,
    pub inverted: bool,
    pub root_function: Option<String>,
    pub paths_to: Option<String>,
    pub min_pct: f32,
    /// Optional absolute-sample floor applied alongside [`Self::min_pct`].
    /// A node is pruned if *either* threshold rejects it. `None` means the
    /// percentage threshold alone decides.
    pub min_samples: Option<u64>,
    pub max_depth: u32,
    pub max_breadth: u32,
    /// When true, fan each native frame out into its DWARF inline chain
    /// (outer-to-inner) so heavily-inlined hot paths show as a sequence of
    /// virtual call-tree nodes instead of collapsing onto the enclosing
    /// function.
    pub expand_inlines: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            filter_args: Filter::default(),
            inverted: false,
            root_function: None,
            paths_to: None,
            min_pct: 1.0,
            min_samples: None,
            max_depth: 8,
            max_breadth: 5,
            expand_inlines: false,
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub thread: Option<String>,
    pub total_samples: u64,
    pub pruning: PruningKnobs,
    pub tree: Option<Node>,
}

#[derive(Debug, Serialize, JsonSchema, Clone)]
pub struct PruningKnobs {
    pub min_pct: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_samples: Option<u64>,
    pub max_depth: u32,
    pub max_breadth: u32,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum Node {
    Frame(FrameNode),
    Omitted { omitted: OmittedSummary },
    Truncated { truncated: TruncatedSummary },
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FrameNode {
    pub function: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain: Option<Vec<String>>,
    pub self_samples: u64,
    pub self_pct: f32,
    pub total_samples: u64,
    pub total_pct: f32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct OmittedSummary {
    pub count: u32,
    pub combined_pct: f32,
}

#[derive(Debug, Serialize, JsonSchema)]
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
    let root_matcher = args
        .root_function
        .as_deref()
        .map(FunctionMatcher::new)
        .transpose()
        .map_err(|e| ToolError::Internal {
            message: e.to_string(),
        })?;
    let paths_to = args
        .paths_to
        .as_deref()
        .map(FunctionMatcher::new)
        .transpose()
        .map_err(|e| ToolError::Internal {
            message: e.to_string(),
        })?;

    let mut root = AggNode::default();
    let mut total_samples: u64 = 0;
    let mut root_match_seen = false;
    let mut paths_to_match_seen = false;

    for handle in args.filter_args.threads(profile) {
        accumulate_with_root(
            profile,
            handle,
            args.inverted,
            args.expand_inlines,
            &root_matcher,
            &paths_to,
            &mut root,
            &mut total_samples,
            &mut root_match_seen,
            &mut paths_to_match_seen,
        );
    }

    // If the user pinned the tree to a specific function (root_function or
    // paths_to) and no stack matched, fall through with a `FunctionNotFound`
    // so the LLM gets nearest-name suggestions instead of a silent
    // `tree: null`. We attribute the miss to whichever matcher was set;
    // when both are set, root_function wins (it's the more restrictive cut).
    if let Some(m) = &root_matcher
        && !root_match_seen
    {
        return Err(ToolError::FunctionNotFound {
            function: matcher_to_string(m),
            nearest_matches: nearest_function_names(profile, m),
        });
    }
    if let Some(m) = &paths_to
        && !paths_to_match_seen
    {
        return Err(ToolError::FunctionNotFound {
            function: matcher_to_string(m),
            nearest_matches: nearest_function_names(profile, m),
        });
    }

    let mut tree = build_node(&root, total_samples, "ROOT".into(), None, args, 0);
    if let Some(Node::Frame(frame)) = tree.as_mut() {
        // Hoist single real root: if synthetic ROOT has exactly one Frame child,
        // replace ROOT with that child so it becomes the visible root.
        let single_real_child = matches!(frame.children.as_slice(), [Node::Frame(_)]);
        if frame.function == "ROOT" && single_real_child {
            if let Node::Frame(child) = frame.children.remove(0) {
                *frame = child;
            }
        } else if frame.function == "ROOT" && frame.children.len() > 1 {
            // Multiple real roots — keep synthetic but rename for clarity.
            frame.function = "<multiple roots>".to_owned();
        }
    }
    if let Some(node) = tree.as_mut() {
        compress_chains(node);
    }

    Ok(Output {
        thread: None,
        total_samples,
        pruning: PruningKnobs {
            min_pct: args.min_pct,
            min_samples: args.min_samples,
            max_depth: args.max_depth,
            max_breadth: args.max_breadth,
        },
        tree,
    })
}

#[allow(clippy::too_many_arguments)]
fn accumulate_with_root(
    profile: &Profile,
    handle: ThreadHandle,
    inverted: bool,
    expand_inlines: bool,
    root_matcher: &Option<FunctionMatcher>,
    paths_to_matcher: &Option<FunctionMatcher>,
    root: &mut AggNode,
    total_samples: &mut u64,
    root_match_seen: &mut bool,
    paths_to_match_seen: &mut bool,
) {
    let raw = profile.raw_thread(handle);
    for &stack_opt in &raw.samples.stack {
        let Some(stack_idx) = stack_opt else { continue };
        // Resolve every native frame and (when expand_inlines is set) fan
        // each one out into its DWARF inline chain. Build root-to-leaf,
        // reverse once at the end if inverted is requested.
        let native: Vec<usize> = profile.walk_stack(handle, stack_idx).collect();
        let mut frames: Vec<(String, Option<String>)> = Vec::with_capacity(native.len());
        // walk_stack iterates leaf-to-root; reverse to get root-to-leaf.
        for &fi in native.iter().rev() {
            let Some(info) = profile.frame_info(handle, fi) else {
                continue;
            };
            let module = info.module_name.map(str::to_owned);
            frames.push((info.function_name.to_owned(), module.clone()));
            if expand_inlines {
                // Inline chain is innermost-first; emit outer-to-inner so
                // the deepest inline ends up at the leaf side of the tree.
                for inl in profile.inline_chain(handle, fi).iter().rev() {
                    frames.push((inl.function.clone(), module.clone()));
                }
            }
        }
        if inverted {
            frames.reverse();
        }
        // If a paths_to matcher is set, skip stacks that don't contain a matching frame.
        if let Some(m) = paths_to_matcher {
            let hit = frames.iter().any(|(name, _)| m.matches(name));
            if !hit {
                continue;
            }
            *paths_to_match_seen = true;
        }
        // If a root matcher is set, find the frame that matches and trim the prefix.
        if let Some(m) = root_matcher {
            let pos = frames.iter().position(|(name, _)| m.matches(name));
            match pos {
                Some(p) => {
                    frames.drain(..p);
                    *root_match_seen = true;
                }
                None => continue, // skip this stack entirely
            };
        }
        *total_samples += 1;
        let mut node: &mut AggNode = root;
        let len = frames.len();
        for (i, (function, module)) in frames.iter().enumerate() {
            let key = (function.clone(), module.clone());
            node = node.children.entry(key).or_default();
            node.total_samples += 1;
            if i + 1 == len {
                node.self_samples += 1;
            }
        }
    }
}

/// True when either the percent threshold or the absolute-sample threshold
/// (when set) rejects a node. Both thresholds gate independently — the node
/// must clear *both* to be emitted.
fn pruned(samples: u64, pct: f32, args: &Args) -> bool {
    pct < args.min_pct || args.min_samples.is_some_and(|m| samples < m)
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
    if depth > 0 && pruned(agg.total_samples, total_pct, args) {
        return None;
    }
    if depth > args.max_depth {
        return Some(Node::Truncated {
            truncated: TruncatedSummary {
                deepest_descendant_pct: total_pct,
            },
        });
    }

    let mut child_entries: Vec<(&(String, Option<String>), &AggNode)> =
        agg.children.iter().collect();
    child_entries.sort_by(|a, b| {
        b.1.total_samples
            .cmp(&a.1.total_samples)
            .then_with(|| a.0.0.cmp(&b.0.0))
            .then_with(|| a.0.1.cmp(&b.0.1))
    });

    let mut children = Vec::new();
    let mut omitted_count: u32 = 0;
    let mut omitted_samples: u64 = 0;
    for (i, (key, child_agg)) in child_entries.iter().enumerate() {
        let mut emit = true;
        if i as u32 >= args.max_breadth {
            emit = false;
        }
        let child_pct = 100.0 * child_agg.total_samples as f32 / total;
        if pruned(child_agg.total_samples, child_pct, args) {
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
            omitted: OmittedSummary {
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
        chain: None,
        self_samples: agg.self_samples,
        self_pct: 100.0 * agg.self_samples as f32 / total,
        total_samples: agg.total_samples,
        total_pct,
        children,
    }))
}

fn compress_chains(node: &mut Node) {
    if let Node::Frame(frame) = node {
        loop {
            let only_real_child = matches!(frame.children.as_slice(), [Node::Frame(_)]);
            if !only_real_child {
                break;
            }
            let child = frame.children.remove(0);
            if let Node::Frame(child_frame) = child {
                let chain_entry = child_frame.function.clone();
                frame.chain.get_or_insert_with(Vec::new).push(chain_entry);
                frame.children = child_frame.children;
            }
        }
        for c in &mut frame.children {
            compress_chains(c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Profile, raw::RawProfile};

    fn fixture() -> Profile {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_functions.json")).unwrap();
        Profile::from_raw(raw)
    }

    #[test]
    fn builds_tree_with_two_top_level_functions() {
        let p = fixture();
        let tree = call_tree(
            &p,
            &Args {
                min_pct: 0.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(tree.tree.is_some());
    }

    #[test]
    fn min_samples_prunes_below_floor() {
        // two_functions.json: hot=90, cold=10. With min_samples=50, only `hot`
        // clears the floor; `cold` is pruned even though min_pct=0 would have
        // kept it.
        let p = fixture();
        let tree = call_tree(
            &p,
            &Args {
                min_pct: 0.0,
                min_samples: Some(50),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(tree.pruning.min_samples, Some(50));
        // Walk the tree; `cold` must not appear, `hot` must.
        let names = collect_frame_names(tree.tree.as_ref());
        assert!(names.contains(&"hot".to_owned()), "missing hot: {names:?}");
        assert!(!names.contains(&"cold".to_owned()), "cold not pruned: {names:?}");
    }

    fn collect_frame_names(node: Option<&Node>) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(Node::Frame(f)) = node {
            out.push(f.function.clone());
            // compress_chains may have collapsed a linear sub-chain into the
            // parent's `chain` field — pick those names up too.
            if let Some(chain) = &f.chain {
                out.extend(chain.iter().cloned());
            }
            for c in &f.children {
                out.extend(collect_frame_names(Some(c)));
            }
        }
        out
    }

    #[test]
    fn expand_inlines_fans_out_native_frame_into_chain() {
        // linear_chain.json: a → b → c → d, all 100 samples on leaf `d`.
        // We inject one inline record onto the leaf so wholesym lookups
        // would have produced [leaf_inline (innermost), d (outer)].
        // With expand_inlines=true the tree becomes a→b→c→d→leaf_inline
        // and `leaf_inline` gets the self_samples.
        use crate::profile::raw::InlineFrame;
        let mut raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/linear_chain.json")).unwrap();
        let t = &mut raw.threads[0];
        t.inline_chains
            .resize_with(t.frame_table.length, Vec::new);
        // frame_table.func = [0,1,2,3]; index 3 is `d` (the leaf).
        t.inline_chains[3] = vec![InlineFrame {
            function: "leaf_inline".into(),
            file: None,
            line: None,
        }];
        let profile = Profile::from_raw(raw);

        // Without expansion: deepest function in the tree is `d`.
        let plain = call_tree(
            &profile,
            &Args {
                min_pct: 0.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!frame_names_contain(&plain.tree, "leaf_inline"));

        // With expansion: `leaf_inline` appears as a child of `d`.
        let expanded = call_tree(
            &profile,
            &Args {
                min_pct: 0.0,
                expand_inlines: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            frame_names_contain(&expanded.tree, "leaf_inline"),
            "expected `leaf_inline` in expanded tree, got: {:?}",
            collect_frame_names(expanded.tree.as_ref())
        );
    }

    fn frame_names_contain(tree: &Option<Node>, target: &str) -> bool {
        collect_frame_names(tree.as_ref())
            .into_iter()
            .any(|n| n == target)
    }

    #[test]
    fn root_function_restricts_tree() {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_functions.json")).unwrap();
        let profile = Profile::from_raw(raw);
        let tree = call_tree(
            &profile,
            &Args {
                root_function: Some("hot".into()),
                min_pct: 0.0,
                ..Default::default()
            },
        )
        .unwrap();
        let root = tree.tree.expect("tree present");
        if let Node::Frame(f) = root {
            assert_eq!(f.function, "hot");
            // No chain because the only stack was [hot] with no children.
            assert!(f.chain.is_none() || f.chain.as_deref() == Some(&[][..]));
            assert_eq!(tree.total_samples, 90);
        } else {
            panic!("expected frame root");
        }
    }

    #[test]
    fn paths_to_keeps_only_matching_stacks() {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/paths_to.json")).unwrap();
        let profile = Profile::from_raw(raw);
        let tree = call_tree(
            &profile,
            &Args {
                paths_to: Some("lock_acquire".into()),
                min_pct: 0.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(tree.total_samples, 50);
    }

    #[test]
    fn single_root_hoisted() {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/linear_chain.json")).unwrap();
        let profile = Profile::from_raw(raw);
        let tree = call_tree(
            &profile,
            &Args {
                min_pct: 0.0,
                ..Default::default()
            },
        )
        .unwrap();
        if let Some(Node::Frame(f)) = tree.tree {
            assert_eq!(f.function, "a");
        } else {
            panic!("expected frame root");
        }
    }

    #[test]
    fn unknown_root_function_returns_function_not_found() {
        let p = fixture();
        let err = call_tree(
            &p,
            &Args {
                root_function: Some("definitely_not_in_profile".into()),
                min_pct: 0.0,
                ..Default::default()
            },
        )
        .unwrap_err();
        match err {
            ToolError::FunctionNotFound { function, .. } => {
                assert_eq!(function, "definitely_not_in_profile");
            }
            other => panic!("expected FunctionNotFound, got {other:?}"),
        }
    }

    #[test]
    fn unknown_paths_to_returns_function_not_found() {
        let p = fixture();
        let err = call_tree(
            &p,
            &Args {
                paths_to: Some("definitely_not_in_profile".into()),
                min_pct: 0.0,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::FunctionNotFound { .. }));
    }

    #[test]
    fn collapses_linear_chain() {
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/linear_chain.json")).unwrap();
        let profile = Profile::from_raw(raw);
        let tree = call_tree(
            &profile,
            &Args {
                min_pct: 0.0,
                ..Default::default()
            },
        )
        .unwrap();
        let root = tree.tree.unwrap();
        if let Node::Frame(f) = root {
            assert_eq!(f.function, "a");
            assert_eq!(
                f.chain.as_deref(),
                Some(&["b".to_owned(), "c".to_owned(), "d".to_owned()][..])
            );
        } else {
            panic!("expected frame root");
        }
    }
}
