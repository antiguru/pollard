//! Hierarchical call tree, with pruning to keep output LLM-digestible.

#![allow(dead_code)]

use crate::error::{ProcessRef, ToolError};
use crate::matching::{
    DidYouMean, FunctionMatcher, auto_promote_match, matcher_to_string, narrowing_matcher,
    nearest_function_scored,
};
use crate::profile::raw::Pid;
use crate::profile::{Profile, ThreadHandle};
use crate::query::event::EventSource;
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
    /// Which per-sample event drives the tree. Default
    /// [`EventSource::Samples`] (cycles in samply); pass
    /// [`EventSource::Marker`] to build the tree from a hardware-counter
    /// event such as `cache-misses`.
    pub event: EventSource,
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
            event: EventSource::Samples,
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Output {
    pub thread: Option<String>,
    pub total_samples: u64,
    /// Echo of the resolved event source — `"samples"` or the marker
    /// name. Pct columns on the tree are percentages of this event.
    pub event: String,
    pub pruning: PruningKnobs,
    pub tree: Option<Node>,
    /// Set when `root_function` or `paths_to` didn't match exactly but the
    /// fuzzy ranker promoted a single high-confidence candidate. Surfaced
    /// so the caller can verify the substitution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub did_you_mean: Option<DidYouMean>,
    /// Set when a bare-name `process=` filter aggregated across more than
    /// one distinct pid. Lists the matched `(pid, name)` pairs so the
    /// caller can disambiguate via `pid:N` or `pid:N.M` syntax.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_processes: Option<Vec<ProcessRef>>,
    /// `true` when `inverted=true` and the tree aggregates leaves across
    /// more than one process because no `process=` filter was set.
    /// Only the inverted shape conflates callers across processes under
    /// the same leaf — a top-down tree groups under the outermost frame
    /// (typically per-process) and stays per-process by construction.
    /// Surfaced so the caller can decide whether the `memcpy`-style
    /// caller chain they're reading actually mixes time from two
    /// different processes via different code paths. Omitted (`null`)
    /// when the result is single-process or process-filtered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cross_process: Option<bool>,
    /// Per-process sample contribution to the tree, biggest first.
    /// Set together with [`Self::cross_process`]. Each entry's `pct` is
    /// share of `total_samples` (same denominator the tree uses), so
    /// a caller can re-run with `process=pid:<N>` to peel off a single
    /// contributor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processes_in_tree: Option<Vec<ProcessInTree>>,
    /// Set when the response was trimmed to fit
    /// `POLLARD_MAX_OUTPUT_BYTES`. The trimmer drops the lowest-pct
    /// leaf frame first and rolls it into its parent's `Omitted`
    /// summary. See [`crate::tools::budget`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<crate::tools::budget::Truncated>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ProcessInTree {
    /// `pid.suffix` when samply recorded a `.N` sub-process, otherwise
    /// the bare OS pid. Same wire form `process=pid:` accepts.
    pub pid: String,
    pub name: String,
    pub samples: u64,
    #[serde(serialize_with = "crate::serde_util::round1_pct")]
    pub pct: f32,
}

#[derive(Debug, Serialize, JsonSchema, Clone)]
pub struct PruningKnobs {
    #[serde(serialize_with = "crate::serde_util::round1_pct")]
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
    #[serde(serialize_with = "crate::serde_util::round1_pct")]
    pub self_pct: f32,
    pub total_samples: u64,
    #[serde(serialize_with = "crate::serde_util::round1_pct")]
    pub total_pct: f32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct OmittedSummary {
    pub count: u32,
    #[serde(serialize_with = "crate::serde_util::round1_pct")]
    pub combined_pct: f32,
    /// Names of up to [`TOP_OMITTED_CAP`] omitted children, biggest first,
    /// so the caller can decide whether widening `min_pct` / `max_breadth`
    /// is worth a second call.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub top_omitted: Vec<OmittedPreview>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct OmittedPreview {
    pub function: String,
    #[serde(serialize_with = "crate::serde_util::round1_pct")]
    pub pct: f32,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TruncatedSummary {
    /// Function name at the depth cutoff — the subtree below this frame was
    /// dropped because it would exceed `max_depth`.
    pub function: String,
    #[serde(serialize_with = "crate::serde_util::round1_pct")]
    pub deepest_descendant_pct: f32,
}

/// Maximum entries surfaced in [`OmittedSummary::top_omitted`].
const TOP_OMITTED_CAP: usize = 3;

#[derive(Default)]
struct AggNode {
    self_samples: u64,
    total_samples: u64,
    children: HashMap<(String, Option<String>), AggNode>,
}

pub fn call_tree(profile: &Profile, args: &Args) -> Result<Output, ToolError> {
    match call_tree_inner(profile, args, None) {
        Err(ToolError::FunctionNotFound {
            function: needle,
            nearest_matches: _,
        }) => {
            // Identify which arg threw so we know which one to substitute.
            // matcher_to_string() returns the un-prefixed pattern for
            // substring matchers, so equality works for the typical case.
            let mut promoted_args = args.clone();
            let target_field = if args.root_function.as_deref() == Some(needle.as_str()) {
                &mut promoted_args.root_function
            } else if args.paths_to.as_deref() == Some(needle.as_str()) {
                &mut promoted_args.paths_to
            } else {
                // Couldn't tie the error back to a specific arg (e.g. regex
                // matcher rendered with `re:` prefix). Re-raise.
                return Err(ToolError::FunctionNotFound {
                    function: needle,
                    nearest_matches: vec![],
                });
            };

            let matcher = FunctionMatcher::new(&needle).map_err(|e| ToolError::Internal {
                message: e.to_string(),
            })?;
            let scored = nearest_function_scored(profile, &matcher);
            let Some(resolved) = auto_promote_match(&scored).map(str::to_owned) else {
                return Err(ToolError::FunctionNotFound {
                    function: needle,
                    nearest_matches: crate::error::truncate_nearest_matches(
                        scored.into_iter().map(|(n, _)| n).collect(),
                    ),
                });
            };

            *target_field = Some(resolved.clone());
            let dym = DidYouMean { needle, resolved };
            call_tree_inner(profile, &promoted_args, Some(dym))
        }
        other => other,
    }
}

fn call_tree_inner(
    profile: &Profile,
    args: &Args,
    did_you_mean: Option<DidYouMean>,
) -> Result<Output, ToolError> {
    args.filter_args.validate_process(profile)?;
    args.filter_args.validate_thread(profile)?;
    args.filter_args.validate_time_range(profile)?;
    let root_matcher = narrowing_matcher("root_function", args.root_function.as_deref())?;
    let paths_to = narrowing_matcher("paths_to", args.paths_to.as_deref())?;

    let mut root = AggNode::default();
    let mut total_samples: u64 = 0;
    let mut root_match_seen = false;
    let mut paths_to_match_seen = false;
    // Track sample contribution per pid so we can flag inverted trees
    // that silently aggregate callers across processes (#68). Keyed by
    // the full `Pid` (incl. samply's `.N` sub-pid suffix) so a caller
    // can disambiguate via `process=pid:<N>` / `pid:<N.M>`.
    let mut per_pid: HashMap<Pid, (u64, String)> = HashMap::new();

    for handle in args.filter_args.threads(profile) {
        let view = profile.thread_view(handle);
        let pid = view.pid_full();
        let name = view.process_name().unwrap_or("").to_owned();
        let before = total_samples;
        accumulate_with_root(
            profile,
            handle,
            args.inverted,
            args.expand_inlines,
            &args.event,
            args.filter_args.time_range,
            &root_matcher,
            &paths_to,
            &mut root,
            &mut total_samples,
            &mut root_match_seen,
            &mut paths_to_match_seen,
        );
        let added = total_samples - before;
        if added > 0 {
            let entry = per_pid.entry(pid).or_insert_with(|| (0, name));
            entry.0 += added;
        }
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
            nearest_matches: crate::matching::nearest_matches_for_error(profile, m),
        });
    }
    if let Some(m) = &paths_to
        && !paths_to_match_seen
    {
        return Err(ToolError::FunctionNotFound {
            function: matcher_to_string(m),
            nearest_matches: crate::matching::nearest_matches_for_error(profile, m),
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

    let (cross_process, processes_in_tree) =
        if args.inverted && args.filter_args.process.is_none() && per_pid.len() > 1 {
            let total = total_samples.max(1) as f32;
            let mut entries: Vec<ProcessInTree> = per_pid
                .into_iter()
                .map(|(pid, (samples, name))| ProcessInTree {
                    pid: format_pid(&pid),
                    name,
                    samples,
                    pct: 100.0 * samples as f32 / total,
                })
                .collect();
            entries.sort_by(|a, b| b.samples.cmp(&a.samples).then_with(|| a.pid.cmp(&b.pid)));
            (Some(true), Some(entries))
        } else {
            (None, None)
        };

    Ok(Output {
        thread: None,
        total_samples,
        event: args.event.label().to_owned(),
        pruning: PruningKnobs {
            min_pct: args.min_pct,
            min_samples: args.min_samples,
            max_depth: args.max_depth,
            max_breadth: args.max_breadth,
        },
        tree,
        did_you_mean,
        matched_processes: args.filter_args.bare_name_multi_match(profile),
        cross_process,
        processes_in_tree,
        truncated: None,
    })
}

/// Render a [`Pid`] back to the wire form accepted by `process=pid:`.
/// Bare pid when no sub-process suffix is set; `pid.suffix` otherwise.
fn format_pid(pid: &Pid) -> String {
    match pid.suffix {
        Some(s) => format!("{}.{}", pid.value, s),
        None => pid.value.to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
fn accumulate_with_root(
    profile: &Profile,
    handle: ThreadHandle,
    inverted: bool,
    expand_inlines: bool,
    event: &EventSource,
    time_range: Option<[f64; 2]>,
    root_matcher: &Option<FunctionMatcher>,
    paths_to_matcher: &Option<FunctionMatcher>,
    root: &mut AggNode,
    total_samples: &mut u64,
    root_match_seen: &mut bool,
    paths_to_match_seen: &mut bool,
) {
    for stack_opt in profile.stack_indices(handle, event, time_range) {
        let Some(stack_idx) = stack_opt else { continue };
        // resolved_chain returns frames root-to-leaf with view transforms
        // (hide / rename / collapse) already applied — same orientation
        // accumulate_with_root expects when building the tree top-down.
        // Reverse once at the end if inverted is requested.
        let mut frames: Vec<(String, Option<String>)> = profile
            .resolved_chain(handle, stack_idx, expand_inlines)
            .into_iter()
            .map(|f| (f.function, f.module))
            .collect();
        if frames.is_empty() {
            continue;
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
                function,
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
    let mut top_omitted: Vec<OmittedPreview> = Vec::new();
    for (i, (key, child_agg)) in child_entries.iter().enumerate() {
        let child_pct = 100.0 * child_agg.total_samples as f32 / total;
        let breadth_cut = i as u32 >= args.max_breadth;
        let prune_cut = pruned(child_agg.total_samples, child_pct, args);
        let mut emitted = false;
        if !breadth_cut
            && !prune_cut
            && let Some(node) = build_node(
                child_agg,
                total_samples,
                key.0.clone(),
                key.1.clone(),
                args,
                depth + 1,
            )
        {
            children.push(node);
            emitted = true;
        }
        if !emitted {
            omitted_count += 1;
            omitted_samples += child_agg.total_samples;
            // child_entries is sorted by total_samples desc, so the first
            // omissions we observe are the heaviest — take the prefix.
            if top_omitted.len() < TOP_OMITTED_CAP {
                top_omitted.push(OmittedPreview {
                    function: key.0.clone(),
                    pct: child_pct,
                });
            }
        }
    }
    if omitted_count > 0 {
        children.push(Node::Omitted {
            omitted: OmittedSummary {
                count: omitted_count,
                combined_pct: 100.0 * omitted_samples as f32 / total,
                top_omitted,
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

/// Outcome of a single [`drop_smallest_leaf`] call.
///
/// `bytes_freed` is the net JSON-bytes saved by removing the leaf and
/// updating the sibling [`OmittedSummary`]. Reported back to the
/// budget trimmer so it can update its running estimate without
/// re-serializing the whole tree (which would push the trim loop into
/// `O(n²)` territory — see #101).
#[derive(Debug, Clone, Copy)]
pub struct DroppedLeaf {
    pub pct: f32,
    pub bytes_freed: usize,
}

/// Drop the lowest-`total_pct` Frame leaf from `tree`, rolling its
/// contribution into its parent's [`OmittedSummary`] so the response
/// stays internally consistent.
///
/// Returns a [`DroppedLeaf`] on success, or `None` when the tree has
/// no Frame children left to drop without losing the only visible
/// root. Used by [`crate::tools::budget::fit_to_budget`] to shrink a
/// serialized response one element at a time.
pub(crate) fn drop_smallest_leaf(tree: &mut Option<Node>) -> Option<DroppedLeaf> {
    let root = tree.as_mut()?;
    // If the root itself is the only Frame in the tree (no Frame
    // children), there's nothing to drop without losing the response
    // entirely. The trim loop reports this as "exhausted" so the
    // caller can re-query with a smaller scope.
    if let Node::Frame(f) = root
        && !f.children.iter().any(|c| matches!(c, Node::Frame(_)))
    {
        return None;
    }
    drop_smallest_leaf_inner(root)
}

fn drop_smallest_leaf_inner(node: &mut Node) -> Option<DroppedLeaf> {
    let frame = match node {
        Node::Frame(f) => f,
        _ => return None,
    };

    let mut smallest_leaf_child: Option<(usize, f32)> = None;
    let mut smallest_inner_child: Option<(usize, f32)> = None;
    for (i, child) in frame.children.iter().enumerate() {
        if let Node::Frame(c) = child {
            let has_frame_grandchildren = c.children.iter().any(|gc| matches!(gc, Node::Frame(_)));
            let pct = c.total_pct;
            if has_frame_grandchildren {
                if smallest_inner_child.is_none_or(|(_, p)| pct < p) {
                    smallest_inner_child = Some((i, pct));
                }
            } else if smallest_leaf_child.is_none_or(|(_, p)| pct < p) {
                smallest_leaf_child = Some((i, pct));
            }
        }
    }

    // Prefer dropping a leaf at this level over recursing — keeps the
    // average drop-cost low (single tree-walk per drop iteration).
    if let Some((i, pct)) = smallest_leaf_child {
        let removed = frame.children.remove(i);
        let removed_bytes = crate::serde_util::serialized_byte_count(&removed) + 1 /* trailing comma */;
        let omitted_delta = if let Node::Frame(c) = removed {
            roll_into_omitted(&mut frame.children, c.function, c.total_pct)
        } else {
            0
        };
        let bytes_freed = removed_bytes.saturating_sub(omitted_delta);
        return Some(DroppedLeaf { pct, bytes_freed });
    }

    if let Some((i, _)) = smallest_inner_child {
        return drop_smallest_leaf_inner(&mut frame.children[i]);
    }

    None
}

/// Append the dropped leaf's contribution to a sibling
/// [`OmittedSummary`] (creating one if absent) so the rendered tree's
/// pct totals stay truthful even after trimming. Returns the JSON
/// byte cost added by the update so the trimmer can subtract it from
/// the bytes the leaf removal freed.
fn roll_into_omitted(children: &mut Vec<Node>, function: String, pct: f32) -> usize {
    let preview = OmittedPreview { function, pct };
    let existing_idx = children
        .iter()
        .position(|c| matches!(c, Node::Omitted { .. }));
    if let Some(i) = existing_idx {
        let before = crate::serde_util::serialized_byte_count(&children[i]);
        if let Node::Omitted { omitted } = &mut children[i] {
            omitted.count += 1;
            omitted.combined_pct += pct;
            // top_omitted lists biggest contributors first. Successive
            // budget-driven drops at the same level pull progressively
            // *larger* leaves (we always drop the smallest remaining
            // first), so once the cap fills we have to displace the
            // smallest existing preview rather than discard the new
            // one — otherwise the list ends up showing the smallest
            // dropped frames, not the biggest.
            let changed;
            if omitted.top_omitted.len() < TOP_OMITTED_CAP {
                omitted.top_omitted.push(preview);
                changed = true;
            } else {
                // top_omitted is non-empty here (TOP_OMITTED_CAP > 0
                // and the cap-full branch implies at least one
                // entry), so min_by always returns Some — `expect`
                // documents that the fallback can't fire.
                let (min_idx, min_pct) = omitted
                    .top_omitted
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| {
                        a.pct
                            .partial_cmp(&b.pct)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .map(|(idx, p)| (idx, p.pct))
                    .expect("top_omitted is non-empty in cap-full branch");
                if pct > min_pct {
                    omitted.top_omitted[min_idx] = preview;
                    changed = true;
                } else {
                    changed = false;
                }
            }
            if changed {
                omitted.top_omitted.sort_by(|a, b| {
                    b.pct
                        .partial_cmp(&a.pct)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            }
        }
        let after = crate::serde_util::serialized_byte_count(&children[i]);
        return after.saturating_sub(before);
    }
    let new_node = Node::Omitted {
        omitted: OmittedSummary {
            count: 1,
            combined_pct: pct,
            top_omitted: vec![preview],
        },
    };
    let cost = crate::serde_util::serialized_byte_count(&new_node) + 1 /* comma */;
    children.push(new_node);
    cost
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
        assert!(
            !names.contains(&"cold".to_owned()),
            "cold not pruned: {names:?}"
        );
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
        t.inline_chains.resize_with(t.frame_table.length, Vec::new);
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
    fn auto_promotes_high_confidence_fuzzy_paths_to() {
        // Issue #21: when `Vec::push` doesn't literally appear but a single
        // demangled symbol scores high in the tokenizer tier, run the query
        // against that resolved name and surface the substitution via
        // `did_you_mean`.
        let mut raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/linear_chain.json")).unwrap();
        raw.threads[0].string_array[3] = "<alloc::vec::Vec<T,A>>::push".to_owned();
        let profile = Profile::from_raw(raw);

        let result = call_tree(
            &profile,
            &Args {
                paths_to: Some("Vec::push".into()),
                min_pct: 0.0,
                ..Default::default()
            },
        )
        .unwrap();

        let dym = result
            .did_you_mean
            .expect("auto-promote should have populated did_you_mean");
        assert_eq!(dym.needle, "Vec::push");
        assert_eq!(dym.resolved, "<alloc::vec::Vec<T,A>>::push");
    }

    #[test]
    fn auto_promotes_high_confidence_fuzzy_root_function() {
        // Same setup as the paths_to test, but pinning root_function — we
        // need to confirm both arg fields are eligible for promotion.
        let mut raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/linear_chain.json")).unwrap();
        raw.threads[0].string_array[0] = "<alloc::vec::Vec<T,A>>::push".to_owned();
        let profile = Profile::from_raw(raw);

        let result = call_tree(
            &profile,
            &Args {
                root_function: Some("Vec::push".into()),
                min_pct: 0.0,
                ..Default::default()
            },
        )
        .unwrap();

        let dym = result
            .did_you_mean
            .expect("auto-promote should have populated did_you_mean");
        assert_eq!(dym.resolved, "<alloc::vec::Vec<T,A>>::push");
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
    fn call_tree_with_marker_event() {
        // two_events.json: 2 cache-miss markers, one per leaf — the
        // tree built from `event=cache-misses` should account for
        // exactly 2 events.
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/two_events.json")).unwrap();
        let profile = Profile::from_raw(raw);
        let tree = call_tree(
            &profile,
            &Args {
                event: EventSource::Marker("cache-misses".into()),
                min_pct: 0.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(tree.event, "cache-misses");
        assert_eq!(tree.total_samples, 2);
    }

    #[test]
    fn omitted_marker_surfaces_top_child_names() {
        // two_functions.json: hot=90, cold=10 at top level. With max_breadth=1
        // we keep only `hot`, and the omitted marker should name `cold`.
        let p = fixture();
        let tree = call_tree(
            &p,
            &Args {
                min_pct: 0.0,
                max_breadth: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let root = tree.tree.expect("tree present");
        let Node::Frame(f) = root else {
            panic!("expected synthetic root frame");
        };
        let omitted = f
            .children
            .iter()
            .find_map(|c| match c {
                Node::Omitted { omitted } => Some(omitted),
                _ => None,
            })
            .expect("expected an Omitted marker");
        assert_eq!(omitted.count, 1);
        assert_eq!(omitted.top_omitted.len(), 1);
        assert_eq!(omitted.top_omitted[0].function, "cold");
        assert!((omitted.top_omitted[0].pct - 10.0).abs() < 0.01);
    }

    #[test]
    fn truncated_marker_includes_function_name() {
        // linear_chain a→b→c→d. With max_depth=1 the recursion hits depth=2
        // when entering `b`, so the truncated marker carries `function: "b"`.
        let raw: RawProfile =
            serde_json::from_str(include_str!("../../tests/fixtures/linear_chain.json")).unwrap();
        let profile = Profile::from_raw(raw);
        let tree = call_tree(
            &profile,
            &Args {
                min_pct: 0.0,
                max_depth: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let truncated = find_truncated(tree.tree.as_ref()).expect("expected a Truncated marker");
        assert_eq!(truncated.function, "b");
        assert!(truncated.deepest_descendant_pct > 0.0);
    }

    fn find_truncated(node: Option<&Node>) -> Option<&TruncatedSummary> {
        match node? {
            Node::Truncated { truncated } => Some(truncated),
            Node::Frame(f) => f.children.iter().find_map(|c| find_truncated(Some(c))),
            Node::Omitted { .. } => None,
        }
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

    /// Build a two-pid profile by re-parsing the two_functions fixture and
    /// rewriting the second copy's pid / processName. RawThread is
    /// `Deserialize`-only (no Clone), so we can't share a parsed thread
    /// between the two pids — re-parsing keeps the test self-contained.
    fn two_pid_profile() -> Profile {
        let json = include_str!("../../tests/fixtures/two_functions.json");
        let mut raw: RawProfile = serde_json::from_str(json).unwrap();
        let mut second: RawProfile = serde_json::from_str(json).unwrap();
        let mut clone = second.threads.remove(0);
        clone.pid = crate::profile::raw::Pid {
            value: 2,
            suffix: None,
        };
        clone.process_name = Some("Other".into());
        raw.threads.push(clone);
        Profile::from_raw(raw)
    }

    /// Inverted call_tree without a process filter that pulls samples from
    /// more than one pid must surface `cross_process: true` and a
    /// `processes_in_tree` breakdown so callers don't silently conflate
    /// `memcpy`-style caller chains across processes (#68).
    #[test]
    fn inverted_cross_process_signal() {
        let profile = two_pid_profile();

        let tree = call_tree(
            &profile,
            &Args {
                inverted: true,
                min_pct: 0.0,
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(tree.cross_process, Some(true));
        let entries = tree.processes_in_tree.expect("processes_in_tree set");
        assert_eq!(entries.len(), 2);
        let pids: Vec<_> = entries.iter().map(|p| p.pid.as_str()).collect();
        assert!(pids.contains(&"1") && pids.contains(&"2"), "{pids:?}");
        assert_eq!(entries.iter().map(|p| p.samples).sum::<u64>(), 200);
    }

    /// Top-down (non-inverted) trees stay silent — the synthetic ROOT
    /// already separates per-process callees, so there's no
    /// cross-process conflation to flag. Same fixture as the inverted
    /// case to keep the two tests contrastable.
    #[test]
    fn top_down_does_not_emit_cross_process_signal() {
        let profile = two_pid_profile();

        let tree = call_tree(
            &profile,
            &Args {
                inverted: false,
                min_pct: 0.0,
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(tree.cross_process, None);
        assert!(tree.processes_in_tree.is_none());
    }

    /// An explicit `process=pid:<N>` filter is the user telling us which
    /// pid they care about, so the cross-process signal should stay
    /// silent even for inverted trees.
    #[test]
    fn process_filter_suppresses_cross_process_signal() {
        let profile = two_pid_profile();

        let tree = call_tree(
            &profile,
            &Args {
                inverted: true,
                min_pct: 0.0,
                filter_args: Filter {
                    process: Some(crate::query::filters::ProcessFilter::Pid(
                        crate::profile::raw::Pid {
                            value: 1,
                            suffix: None,
                        },
                    )),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(tree.cross_process, None);
        assert!(tree.processes_in_tree.is_none());
    }

    /// Build a small tree with a known shape so we can drive the
    /// budget-trimmer and assert it picks the lowest-pct leaf and rolls
    /// the contribution into a sibling Omitted summary.
    fn synthetic_two_leaf_tree() -> Option<Node> {
        // root (10/100) -> [hot (8/80), cold (2/20)]
        Some(Node::Frame(FrameNode {
            function: "root".into(),
            module: None,
            chain: None,
            self_samples: 0,
            self_pct: 0.0,
            total_samples: 10,
            total_pct: 100.0,
            children: vec![
                Node::Frame(FrameNode {
                    function: "hot".into(),
                    module: None,
                    chain: None,
                    self_samples: 8,
                    self_pct: 80.0,
                    total_samples: 8,
                    total_pct: 80.0,
                    children: vec![],
                }),
                Node::Frame(FrameNode {
                    function: "cold".into(),
                    module: None,
                    chain: None,
                    self_samples: 2,
                    self_pct: 20.0,
                    total_samples: 2,
                    total_pct: 20.0,
                    children: vec![],
                }),
            ],
        }))
    }

    #[test]
    fn drop_smallest_leaf_picks_lowest_pct() {
        let mut tree = synthetic_two_leaf_tree();
        let dropped = drop_smallest_leaf(&mut tree).expect("dropped one leaf");
        assert!((dropped.pct - 20.0).abs() < 1e-3);
        let Some(Node::Frame(f)) = tree else {
            panic!("expected frame root after drop");
        };
        // Two children remain: hot (Frame), Omitted summarizing cold.
        assert_eq!(f.children.len(), 2);
        let omitted = f
            .children
            .iter()
            .find_map(|c| match c {
                Node::Omitted { omitted } => Some(omitted),
                _ => None,
            })
            .expect("rolled cold into Omitted");
        assert_eq!(omitted.count, 1);
        assert!((omitted.combined_pct - 20.0).abs() < 1e-3);
        assert_eq!(omitted.top_omitted[0].function, "cold");
    }

    #[test]
    fn drop_smallest_leaf_returns_none_at_solitary_root() {
        // A single Frame with no Frame children — nothing droppable.
        let mut tree = Some(Node::Frame(FrameNode {
            function: "lone".into(),
            module: None,
            chain: None,
            self_samples: 1,
            self_pct: 100.0,
            total_samples: 1,
            total_pct: 100.0,
            children: vec![],
        }));
        assert!(drop_smallest_leaf(&mut tree).is_none());
    }

    #[test]
    fn drop_smallest_leaf_prefers_leaf_sibling_at_current_level() {
        // root -> [a (inner: has a Frame grandchild), b (direct leaf)].
        // Both are siblings at the root level; the strategy prefers
        // dropping the leaf sibling (`b`) over descending into `a` —
        // see the longer rationale below the tree literal.
        let mut tree = Some(Node::Frame(FrameNode {
            function: "root".into(),
            module: None,
            chain: None,
            self_samples: 0,
            self_pct: 0.0,
            total_samples: 10,
            total_pct: 100.0,
            children: vec![
                Node::Frame(FrameNode {
                    function: "a".into(),
                    module: None,
                    chain: None,
                    self_samples: 0,
                    self_pct: 0.0,
                    total_samples: 4,
                    total_pct: 40.0,
                    children: vec![Node::Frame(FrameNode {
                        function: "tiny_leaf".into(),
                        module: None,
                        chain: None,
                        self_samples: 4,
                        self_pct: 40.0,
                        total_samples: 4,
                        total_pct: 40.0,
                        children: vec![],
                    })],
                }),
                Node::Frame(FrameNode {
                    function: "b".into(),
                    module: None,
                    chain: None,
                    self_samples: 6,
                    self_pct: 60.0,
                    total_samples: 6,
                    total_pct: 60.0,
                    children: vec![],
                }),
            ],
        }));
        // Expected pick: `b` is the smaller leaf at the root level
        // (60%) vs `a`'s only child `tiny_leaf` is reachable but a's
        // total is 40%. The trimmer prefers leaf children at the
        // current level when one is available — so `b` (60%) goes
        // first because both `a` (40, has frame grandchild) and `b`
        // (60, leaf) are siblings, and only `b` qualifies as a "leaf
        // child" candidate.
        let dropped = drop_smallest_leaf(&mut tree).expect("dropped one leaf");
        assert!((dropped.pct - 60.0).abs() < 1e-3);
    }

    /// When the root-level children are all *inner* frames (i.e. each
    /// has at least one Frame grandchild), `drop_smallest_leaf_inner`
    /// must descend rather than refuse — otherwise we'd never reach
    /// the leaves of a tall tree. Build a tree where the only
    /// droppable leaves live two levels down and verify the trimmer
    /// finds the smallest one.
    #[test]
    fn drop_smallest_leaf_recurses_when_no_root_level_leaves() {
        // root -> [a -> small_leaf (5%), b -> big_leaf (95%)].
        // No root-level Frame is a leaf (both `a` and `b` have a
        // Frame grandchild), so the trimmer must descend. Expected
        // pick: small_leaf at 5%.
        let inner = |fname: &str, total_pct: f32, leaf_name: &str, leaf_pct: f32| -> Node {
            Node::Frame(FrameNode {
                function: fname.into(),
                module: None,
                chain: None,
                self_samples: 0,
                self_pct: 0.0,
                total_samples: total_pct as u64,
                total_pct,
                children: vec![Node::Frame(FrameNode {
                    function: leaf_name.into(),
                    module: None,
                    chain: None,
                    self_samples: leaf_pct as u64,
                    self_pct: leaf_pct,
                    total_samples: leaf_pct as u64,
                    total_pct: leaf_pct,
                    children: vec![],
                })],
            })
        };
        let mut tree = Some(Node::Frame(FrameNode {
            function: "root".into(),
            module: None,
            chain: None,
            self_samples: 0,
            self_pct: 0.0,
            total_samples: 100,
            total_pct: 100.0,
            children: vec![
                inner("a", 5.0, "small_leaf", 5.0),
                inner("b", 95.0, "big_leaf", 95.0),
            ],
        }));
        let dropped = drop_smallest_leaf(&mut tree).expect("dropped a leaf");
        assert!(
            (dropped.pct - 5.0).abs() < 1e-3,
            "expected small_leaf (5%) dropped, got {}",
            dropped.pct
        );
    }

    /// `top_omitted` is documented as "biggest first", but successive
    /// budget-driven drops at the same level pull progressively
    /// larger leaves. Once the cap fills, the new (larger) preview
    /// must displace the smallest existing entry — otherwise the
    /// list ends up holding the smallest dropped frames.
    #[test]
    fn top_omitted_displaces_smallest_when_cap_full() {
        // Five tiny siblings at the root; cap is 3. Drop them all
        // (smallest first) and confirm `top_omitted` ends up with the
        // three *biggest* names, not the three smallest.
        let leaf = |name: &str, pct: f32| -> Node {
            Node::Frame(FrameNode {
                function: name.into(),
                module: None,
                chain: None,
                self_samples: pct as u64,
                self_pct: pct,
                total_samples: pct as u64,
                total_pct: pct,
                children: vec![],
            })
        };
        let mut tree = Some(Node::Frame(FrameNode {
            function: "root".into(),
            module: None,
            chain: None,
            self_samples: 0,
            self_pct: 0.0,
            total_samples: 100,
            total_pct: 100.0,
            children: vec![
                leaf("smallest_1", 1.0),
                leaf("small_2", 2.0),
                leaf("mid_3", 3.0),
                leaf("big_4", 4.0),
                leaf("biggest_5", 50.0), // sentinel: prevents solitary-root exhaustion
            ],
        }));
        // Drop four leaves — the first four come out smallest-first.
        for _ in 0..4 {
            drop_smallest_leaf(&mut tree).expect("can drop");
        }
        let Some(Node::Frame(root)) = tree else {
            panic!("expected root frame");
        };
        let omitted = root
            .children
            .iter()
            .find_map(|c| match c {
                Node::Omitted { omitted } => Some(omitted),
                _ => None,
            })
            .expect("Omitted summary present");
        assert_eq!(omitted.count, 4);
        let names: Vec<&str> = omitted
            .top_omitted
            .iter()
            .map(|p| p.function.as_str())
            .collect();
        assert_eq!(names.len(), TOP_OMITTED_CAP);
        // Biggest three of the dropped four should be retained:
        // {small_2, mid_3, big_4}. `smallest_1` should NOT appear.
        assert!(!names.contains(&"smallest_1"), "names: {names:?}");
        assert!(names.contains(&"big_4"), "names: {names:?}");
        assert!(names.contains(&"mid_3"), "names: {names:?}");
    }

    /// Successive drops at the same level should report progressive
    /// `bytes_freed`. The first drop creates an `Omitted` summary
    /// roughly the size of the removed leaf and may report ~0 net
    /// savings; the second drop merely bumps `count` and recovers the
    /// full leaf size. The trim loop relies on this making forward
    /// progress eventually.
    #[test]
    fn successive_drops_make_byte_progress() {
        // Five tiny siblings — first drop pays the Omitted overhead,
        // the rest amortize cleanly.
        let make_leaf = |name: &str, pct: f32, samples: u64| -> Node {
            Node::Frame(FrameNode {
                function: name.into(),
                module: None,
                chain: None,
                self_samples: samples,
                self_pct: pct,
                total_samples: samples,
                total_pct: pct,
                children: vec![],
            })
        };
        let mut tree = Some(Node::Frame(FrameNode {
            function: "root".into(),
            module: None,
            chain: None,
            self_samples: 0,
            self_pct: 0.0,
            total_samples: 100,
            total_pct: 100.0,
            children: vec![
                make_leaf("aaa", 5.0, 5),
                make_leaf("bbb", 6.0, 6),
                make_leaf("ccc", 7.0, 7),
                make_leaf("ddd", 8.0, 8),
                make_leaf("eee", 74.0, 74),
            ],
        }));

        let mut total_freed = 0usize;
        for _ in 0..3 {
            let dropped = drop_smallest_leaf(&mut tree).expect("can keep dropping");
            total_freed += dropped.bytes_freed;
        }
        assert!(
            total_freed > 0,
            "three successive drops must net positive bytes; got {total_freed}"
        );
    }
}
