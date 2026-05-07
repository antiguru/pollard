//! Query MCP tools: top_functions, call_tree, stacks_containing.

use crate::error::ToolError;
use crate::profile::raw::Pid;
use crate::query::filters::{Filter, ProcessFilter, ThreadFilter};
use crate::query::{call_tree, compare, folded, stacks_containing, top_functions, top_groups};
use crate::tools::PollardServer;
use crate::tools::budget::{DropOutcome, fit_to_budget, output_budget_bytes};
use rmcp::ErrorData;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Helper: look up a session by profile_id
// ---------------------------------------------------------------------------

async fn get_session(
    server: &PollardServer,
    profile_id: &str,
) -> Result<Arc<crate::session::ProfileSession>, ToolError> {
    server.registry.get_or_error(profile_id).await
}

// ---------------------------------------------------------------------------
// Common filter args / helper
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema, Default)]
pub struct CommonFilterArgs {
    /// Thread filter: "tid:NNN" for a numeric tid, or a thread name string.
    #[serde(default)]
    pub thread: Option<String>,
    /// Process filter: `"pid:NNN"` (or `"pid:NNN.M"` to pin to one of
    /// samply's `.N` sub-processes that share an OS pid), or a process
    /// name string matched against each thread's `processName`.
    #[serde(default)]
    pub process: Option<String>,
    /// Optional time range `[start_ms, end_ms]`, **relative to profile
    /// start** (i.e. the first recorded sample). To pull the matching
    /// reference frame out of `summary`, copy `time_range_ms` directly,
    /// or subtract `profile_start_ms` from any absolute timestamp first.
    #[serde(default)]
    pub time_range: Option<[f64; 2]>,
}

/// Parse the `pid:` payload. Accepts `"NNN"` (suffix `None`) or
/// `"NNN.M"` (suffix `Some(M)`). Returns `None` when the value isn't a
/// numeric pid; the caller is expected to treat that as a hard error
/// (the `pid:` prefix is an explicit opt-in to integer matching).
fn parse_pid(rest: &str) -> Option<Pid> {
    let mut parts = rest.splitn(2, '.');
    let value = parts.next()?.parse::<u64>().ok()?;
    let suffix = match parts.next() {
        Some(s) => Some(s.parse::<u32>().ok()?),
        None => None,
    };
    Some(Pid { value, suffix })
}

/// Map a user-supplied string against a fixed set of accepted values
/// for a string-enum argument. `None` returns `default`; an unknown
/// `Some(other)` returns [`ToolError::InvalidValue`] with the full
/// accepted list — matching the structured-suggestion shape of
/// `function_not_found.nearest_matches`. Unknown values were silently
/// falling through to the default before, masking caller typos.
fn parse_string_enum<T: Copy>(
    field: &'static str,
    raw: Option<&str>,
    default: T,
    table: &[(&'static str, T)],
) -> Result<T, ToolError> {
    let Some(value) = raw else {
        return Ok(default);
    };
    for (name, val) in table {
        if *name == value {
            return Ok(*val);
        }
    }
    Err(ToolError::InvalidValue {
        field: field.to_owned(),
        value: value.to_owned(),
        accepted: table.iter().map(|(n, _)| (*n).to_owned()).collect(),
        hint: None,
    })
}

/// Convert the wire-format filter args into a [`Filter`].
///
/// The `tid:` / `pid:` prefixes opt into integer matching and must be
/// followed by a well-formed numeric payload. A malformed prefix
/// (`tid:abc`, `pid:1.2.3`) used to silently fall back to a literal
/// name match — the resulting `thread_not_found` / `process_not_found`
/// then echoed the original string back, which made the diagnostic
/// confusing because the listed available threads/processes never
/// matched. Reject malformed prefixes up front instead.
pub(crate) fn parse_filter(args: &CommonFilterArgs) -> Result<Filter, ToolError> {
    let thread = args
        .thread
        .as_deref()
        .map(|t| {
            if let Some(rest) = t.strip_prefix("tid:") {
                let n = rest.parse::<u64>().map_err(|_| ToolError::InvalidValue {
                    field: "thread".to_owned(),
                    value: t.to_owned(),
                    accepted: vec!["tid:NNN".to_owned(), "<thread name>".to_owned()],
                    hint: None,
                })?;
                return Ok(ThreadFilter::Tid(n));
            }
            Ok(ThreadFilter::Name(t.to_owned()))
        })
        .transpose()?;
    let process = args
        .process
        .as_deref()
        .map(|p| {
            if let Some(rest) = p.strip_prefix("pid:") {
                let pid = parse_pid(rest).ok_or_else(|| ToolError::InvalidValue {
                    field: "process".to_owned(),
                    value: p.to_owned(),
                    accepted: vec![
                        "pid:NNN".to_owned(),
                        "pid:NNN.M".to_owned(),
                        "<process name>".to_owned(),
                    ],
                    hint: None,
                })?;
                return Ok(ProcessFilter::Pid(pid));
            }
            Ok(ProcessFilter::Name(p.to_owned()))
        })
        .transpose()?;
    Ok(Filter {
        thread,
        process,
        time_range: args.time_range,
    })
}

// ---------------------------------------------------------------------------
// top_functions args
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub struct TopFunctionsArgs {
    /// Profile ID returned by load_profile.
    pub profile_id: String,
    /// Optional filter on the **demangled function name**.
    /// Substring match by default; prefix with `re:` for a regex (use `re:(?i)foo` for case-insensitive).
    /// Case-sensitive — e.g. `"simd_cols_3rd"` matches, `"simd"` matches
    /// `"simd_cols_3rd"` but NOT `"SIMD"` or `"simdcols"`; a regex form is
    /// `"re:simd.*cols"`. Not a topic / fuzzy / token search; for "anything
    /// related to X" use multiple narrow substring queries or a regex with
    /// alternation.
    #[serde(default)]
    pub filter: Option<String>,
    /// Maximum number of results to return. Defaults to 30.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Sort order: "self" (default), "total", or "descendants"
    /// (`total - self`; surfaces wrappers/dispatchers that aren't themselves
    /// hot but call into hot code).
    #[serde(default)]
    pub sort_by: Option<String>,
    /// When true, expand each native frame into its DWARF inline chain so
    /// self-time attributes to the deepest inlined callee instead of the
    /// enclosing function (e.g. surfaces `core::iter::Sum::sum` instead of
    /// the bencher harness that inlined it). Off by default.
    #[serde(default)]
    pub expand_inlines: Option<bool>,
    /// Event source: omit (or empty string) for the default samples track —
    /// cycles, in samply's perf recorder. Pass a marker name like
    /// `"cache-misses"`, `"branch-misses"`, or `"instructions"` to
    /// aggregate that hardware counter instead. The error lists known
    /// events when the name doesn't match.
    #[serde(default)]
    pub event: Option<String>,
    #[serde(flatten)]
    pub common: CommonFilterArgs,
}

// ---------------------------------------------------------------------------
// call_tree args
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub struct CallTreeArgs {
    /// Profile ID returned by load_profile.
    pub profile_id: String,
    /// If true, build a bottom-up (callee-to-caller) tree.
    #[serde(default)]
    pub inverted: Option<bool>,
    /// Only show subtrees rooted at this function name / pattern.
    /// Substring match by default; prefix with `re:` for a regex (use `re:(?i)foo` for case-insensitive).
    #[serde(default)]
    pub root_function: Option<String>,
    /// Only include stacks that pass through this function.
    /// Substring match by default; prefix with `re:` for a regex (use `re:(?i)foo` for case-insensitive).
    #[serde(default)]
    pub paths_to: Option<String>,
    /// Minimum percentage threshold for including a node (default 1.0).
    #[serde(default)]
    pub min_pct: Option<f32>,
    /// Optional absolute-sample floor. Applied alongside `min_pct` —
    /// a node is pruned if either threshold rejects it. Useful for flat
    /// profiles where a percent threshold elides individually small but
    /// collectively large contributors.
    #[serde(default)]
    pub min_samples: Option<u64>,
    /// Maximum tree depth (default 8).
    #[serde(default)]
    pub max_depth: Option<u32>,
    /// Maximum child breadth per node (default 5).
    #[serde(default)]
    pub max_breadth: Option<u32>,
    /// When true, expand each native frame into its DWARF inline chain so
    /// heavily-inlined hot paths (e.g. stdlib calls inlined into a Rust
    /// benchmark) appear as a sequence of virtual call-tree nodes.
    /// Off by default to keep the historical tree shape.
    #[serde(default)]
    pub expand_inlines: Option<bool>,
    /// Event source: omit for the default samples track. Pass a marker
    /// name (`"cache-misses"`, `"branch-misses"`, `"instructions"`) to
    /// build the tree from a hardware-counter event instead.
    #[serde(default)]
    pub event: Option<String>,
    #[serde(flatten)]
    pub common: CommonFilterArgs,
}

// ---------------------------------------------------------------------------
// folded_stacks args
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub struct FoldedStacksArgs {
    /// Profile ID returned by load_profile.
    pub profile_id: String,
    /// Optional function-name filter; only stacks containing at least one
    /// matching frame are emitted (the full stack is preserved in the line).
    /// Substring match by default; prefix with `re:` for a regex (use `re:(?i)foo` for case-insensitive).
    #[serde(default)]
    pub function_filter: Option<String>,
    #[serde(flatten)]
    pub common: CommonFilterArgs,
}

#[derive(serde::Serialize, JsonSchema)]
pub struct FoldedStacksOutput {
    /// Folded text: one line per unique stack, formatted as
    /// `root;child;...;leaf <samples>`.
    pub folded: String,
    /// Set when the response was trimmed to fit
    /// `POLLARD_MAX_OUTPUT_BYTES`. The lowest-sample lines are dropped
    /// first; the remaining `folded` text is still sorted by stack
    /// string. See [`crate::tools::budget`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<crate::tools::budget::Truncated>,
}

// ---------------------------------------------------------------------------
// top_groups args
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub struct TopGroupsArgs {
    pub profile_id: String,
    /// Grouping axis: `"function"`, `"module"`, `"file"`, or
    /// `"directory"`. Defaults to `"function"` (which matches
    /// `top_functions` modulo the module disambiguation column).
    #[serde(default)]
    pub group_by: Option<String>,
    /// Optional filter on function names — same matcher as `top_functions`.
    /// Substring match by default; prefix with `re:` for a regex (use `re:(?i)foo` for case-insensitive). Filters
    /// frames *before* grouping, so a `group_by="module"` query with
    /// `filter="hot"` only counts frames whose function names match `hot`.
    #[serde(default)]
    pub filter: Option<String>,
    /// Maximum number of rows to return. Defaults to 30.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Sort order: `"self"` (default), `"total"`, or `"descendants"`.
    #[serde(default)]
    pub sort_by: Option<String>,
    /// Path-component depth for `group_by="directory"`. Omit for the
    /// full parent directory; `1` truncates to the first component
    /// (`/home/foo/bar.rs` → `/home`), `2` keeps two (`/home/foo`).
    #[serde(default)]
    pub directory_depth: Option<u32>,
    /// When true, expand each native frame into its DWARF inline chain
    /// before grouping — same semantics as `top_functions`.
    #[serde(default)]
    pub expand_inlines: Option<bool>,
    #[serde(flatten)]
    pub common: CommonFilterArgs,
}

// ---------------------------------------------------------------------------
// compare_profiles args
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub struct CompareProfilesArgs {
    /// Baseline profile ID (the "before" side of the diff).
    pub profile_id_a: String,
    /// Comparison profile ID (the "after" side of the diff).
    pub profile_id_b: String,
    /// Optional filter on function names — applied identically to both sides.
    /// Substring match by default; prefix with `re:` for a regex (use `re:(?i)foo` for case-insensitive).
    #[serde(default)]
    pub filter: Option<String>,
    /// Maximum number of rows to return. Defaults to 30.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Sort order: "delta" (default — `|b_self_pct - a_self_pct|`
    /// descending), "delta_ms" (`|delta_self_ms|` descending — robust to
    /// changes in total profile duration), "a" (A's self-pct), or "b"
    /// (B's self-pct).
    #[serde(default)]
    pub sort_by: Option<String>,
    /// Drop rows whose absolute self-pct delta is below this threshold.
    /// Useful for filtering out rounding noise on long-tail functions.
    #[serde(default)]
    pub min_delta_pct: Option<f32>,
    /// Forwarded to the per-profile aggregator on both sides.
    #[serde(default)]
    pub expand_inlines: Option<bool>,
    /// Join-key shape: "function_and_module" (default — joins on
    /// `(function, module)` after stripping cargo's 16-hex build hash) or
    /// "function" (drops module from the key, useful when the two
    /// profiles come from differently-named binaries).
    #[serde(default)]
    pub align_by: Option<String>,
    /// Event source: omit for the default samples track. Pass a marker
    /// name (`"cache-misses"`, `"branch-misses"`, `"instructions"`) to
    /// diff a hardware-counter event instead. Resolved against profile
    /// A; the `*_ms` columns are omitted from each row when the event
    /// is not time-shaped.
    #[serde(default)]
    pub event: Option<String>,
    #[serde(flatten)]
    pub common: CommonFilterArgs,
}

// ---------------------------------------------------------------------------
// stacks_containing args
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub struct StacksContainingArgs {
    /// Profile ID returned by load_profile.
    pub profile_id: String,
    /// Function name / pattern to search for in each stack.
    /// Substring match by default; prefix with `re:` for a regex (use `re:(?i)foo` for case-insensitive).
    pub function: String,
    /// Maximum number of distinct stacks to return. Defaults to 20.
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(flatten)]
    pub common: CommonFilterArgs,
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

#[tool_router(router = query_router, vis = "pub(crate)")]
impl PollardServer {
    #[tool(
        name = "top_functions",
        description = "Top-N functions by self or total samples. Pass `event=\"<name>\"` (e.g. `cache-misses`, `branch-misses`, `instructions`) to aggregate hardware-counter markers instead of the default samples track; pct columns then mean \"% of <event>\"."
    )]
    pub async fn top_functions(
        &self,
        Parameters(args): Parameters<TopFunctionsArgs>,
    ) -> Result<Json<top_functions::Output>, ErrorData> {
        let session = get_session(self, &args.profile_id).await?;
        let event = crate::query::event::resolve(session.profile(), args.event.as_deref())?;
        let q_args = top_functions::Args {
            filter: args.filter.clone(),
            limit: args.limit.unwrap_or(0),
            sort_by: parse_string_enum(
                "sort_by",
                args.sort_by.as_deref(),
                top_functions::SortBy::SelfTime,
                &[
                    ("self", top_functions::SortBy::SelfTime),
                    ("total", top_functions::SortBy::TotalTime),
                    ("descendants", top_functions::SortBy::Descendants),
                ],
            )?,
            filter_args: parse_filter(&args.common)?.compose_under_scope(session.view_scope())?,
            expand_inlines: args.expand_inlines.unwrap_or(false),
            event,
        };
        let mut result = top_functions::top_functions(session.profile(), &q_args)?;
        result.truncated = fit_to_budget(&mut result, output_budget_bytes(), |out| {
            // Rows are pre-sorted (descending by self/total/etc) so the
            // tail is always the lowest-priority — pop until budget fits.
            match out.functions.pop() {
                Some(row) => DropOutcome::Dropped(Some(row.self_pct)),
                None => DropOutcome::Exhausted,
            }
        });
        Ok(Json(result))
    }

    #[tool(
        name = "top_groups",
        description = "Top-N aggregation under a caller-chosen group key (function, module, file, or directory prefix). Same self/total accounting as `top_functions`; useful when you want hot directories or libraries rather than hot functions."
    )]
    pub async fn top_groups(
        &self,
        Parameters(args): Parameters<TopGroupsArgs>,
    ) -> Result<Json<top_groups::Output>, ErrorData> {
        let session = get_session(self, &args.profile_id).await?;
        let q_args = top_groups::Args {
            group_by: parse_string_enum(
                "group_by",
                args.group_by.as_deref(),
                top_groups::GroupBy::Function,
                &[
                    ("function", top_groups::GroupBy::Function),
                    ("module", top_groups::GroupBy::Module),
                    ("file", top_groups::GroupBy::File),
                    ("directory", top_groups::GroupBy::Directory),
                ],
            )?,
            filter: args.filter.clone(),
            limit: args.limit.unwrap_or(0),
            sort_by: parse_string_enum(
                "sort_by",
                args.sort_by.as_deref(),
                top_groups::SortBy::SelfTime,
                &[
                    ("self", top_groups::SortBy::SelfTime),
                    ("total", top_groups::SortBy::TotalTime),
                    ("descendants", top_groups::SortBy::Descendants),
                ],
            )?,
            filter_args: parse_filter(&args.common)?.compose_under_scope(session.view_scope())?,
            expand_inlines: args.expand_inlines.unwrap_or(false),
            directory_depth: args.directory_depth,
        };
        let mut result = top_groups::top_groups(session.profile(), &q_args)?;
        result.truncated = fit_to_budget(&mut result, output_budget_bytes(), |out| {
            match out.groups.pop() {
                Some(row) => DropOutcome::Dropped(Some(row.self_pct)),
                None => DropOutcome::Exhausted,
            }
        });
        Ok(Json(result))
    }

    #[tool(
        name = "call_tree",
        description = "Hierarchical call tree, pruned for LLM consumption. Pass `event=\"<name>\"` to build the tree from a marker-backed counter (cache-misses, branch-misses, instructions, …) instead of the default samples track. With `inverted=true` and no `process=` filter, multi-process recordings expose `cross_process: true` plus a `processes_in_tree: [{pid, name, samples, pct}]` summary so the caller can tell when one leaf's caller chain mixes time from different processes; narrow with `process=pid:<N>` to peel off a single contributor."
    )]
    pub async fn call_tree(
        &self,
        Parameters(args): Parameters<CallTreeArgs>,
    ) -> Result<Json<call_tree::Output>, ErrorData> {
        let session = get_session(self, &args.profile_id).await?;
        let event = crate::query::event::resolve(session.profile(), args.event.as_deref())?;
        let defaults = call_tree::Args::default();
        let q_args = call_tree::Args {
            filter_args: parse_filter(&args.common)?.compose_under_scope(session.view_scope())?,
            inverted: args.inverted.unwrap_or(false),
            root_function: args.root_function.clone(),
            paths_to: args.paths_to.clone(),
            min_pct: args.min_pct.unwrap_or(defaults.min_pct),
            min_samples: args.min_samples,
            max_depth: args.max_depth.unwrap_or(defaults.max_depth),
            max_breadth: args.max_breadth.unwrap_or(defaults.max_breadth),
            expand_inlines: args.expand_inlines.unwrap_or(defaults.expand_inlines),
            event,
        };
        let mut result = call_tree::call_tree(session.profile(), &q_args)?;
        result.truncated = fit_to_budget(&mut result, output_budget_bytes(), |out| {
            match call_tree::drop_smallest_leaf(&mut out.tree) {
                Some(pct) => DropOutcome::Dropped(Some(pct)),
                None => DropOutcome::Exhausted,
            }
        });
        Ok(Json(result))
    }

    #[tool(
        name = "folded_stacks",
        description = "Flamegraph-folded text export. One line per unique stack: `root;...;leaf <samples>`. Pipeable into inferno-flamegraph; diffable across profiles with `comm`."
    )]
    pub async fn folded_stacks(
        &self,
        Parameters(args): Parameters<FoldedStacksArgs>,
    ) -> Result<Json<FoldedStacksOutput>, ErrorData> {
        let session = get_session(self, &args.profile_id).await?;
        let q_args = folded::Args {
            filter_args: parse_filter(&args.common)?.compose_under_scope(session.view_scope())?,
            function_filter: args.function_filter.clone(),
        };
        let folded = folded::folded_stacks_structured(session.profile(), &q_args)?;
        let (rendered, truncated) = fit_folded_to_budget(folded, output_budget_bytes());
        Ok(Json(FoldedStacksOutput {
            folded: rendered,
            truncated,
        }))
    }

    #[tool(
        name = "compare_profiles",
        description = "Per-function delta between two loaded profiles, aligned by (function, module) by default. Cargo's 16-hex build-hash suffix on module names is stripped before keying, so two builds of the same binary align. Pass `align_by=\"function\"` to drop module from the key entirely (e.g. cross-binary comparisons). Each row reports both share-of-profile (`*_pct`) and wall-time-ish (`*_ms`, computed as `samples * meta.interval`) columns — `delta_self_ms` answers \"did this function take more or less time\" directly, while `delta_self_pct` is share-only and can mislead when total runtime changes. For fixed-workload programs (same input, same iteration count) `delta_self_ms` cleanly reads as \"got faster/slower\"; if A and B do different amounts of work, both columns mix workload-size and per-call-cost effects. The `event=` arg works the same way as on `top_functions` (e.g. `event=\"cache-misses\"` to diff cache-miss attribution); the `*_ms` columns are omitted from rows when the event is not time-shaped because count × sampling-interval has no meaningful unit there. Sorted by `|delta_self_pct|` descending by default — surfaces what moved most between A (before) and B (after)."
    )]
    pub async fn compare_profiles(
        &self,
        Parameters(args): Parameters<CompareProfilesArgs>,
    ) -> Result<Json<compare::Output>, ErrorData> {
        let session_a = get_session(self, &args.profile_id_a).await?;
        let session_b = get_session(self, &args.profile_id_b).await?;
        // Resolve the event against profile A. Profiles recorded together
        // share the same event set; rejecting on B-only events is the
        // strict and useful semantic when the two profiles disagree.
        let event = crate::query::event::resolve(session_a.profile(), args.event.as_deref())?;
        let q_args = compare::Args {
            filter: args.filter.clone(),
            limit: args.limit.unwrap_or(0),
            sort_by: parse_string_enum(
                "sort_by",
                args.sort_by.as_deref(),
                compare::SortBy::Delta,
                &[
                    ("delta", compare::SortBy::Delta),
                    ("delta_ms", compare::SortBy::DeltaMs),
                    ("a", compare::SortBy::A),
                    ("b", compare::SortBy::B),
                ],
            )?,
            min_delta_pct: args.min_delta_pct,
            // Per-call filter must compose with both sides' view scopes.
            // We only need to pass one effective filter to the aggregator
            // (it's applied symmetrically), but we validate against B's
            // scope so a per-call filter that conflicts with B doesn't
            // silently apply only to A.
            filter_args: {
                let parsed = parse_filter(&args.common)?;
                parsed.clone().compose_under_scope(session_b.view_scope())?;
                parsed.compose_under_scope(session_a.view_scope())?
            },
            expand_inlines: args.expand_inlines.unwrap_or(false),
            align_by: parse_string_enum(
                "align_by",
                args.align_by.as_deref(),
                compare::AlignBy::FunctionAndModule,
                &[
                    ("function_and_module", compare::AlignBy::FunctionAndModule),
                    ("function", compare::AlignBy::Function),
                ],
            )?,
            event,
        };
        let mut result =
            compare::compare_profiles(session_a.profile(), session_b.profile(), &q_args)?;
        result.truncated = fit_to_budget(&mut result, output_budget_bytes(), |out| {
            // Sorted by `|delta|` desc (or the chosen sort), so the
            // tail of the vec is the least interesting row.
            match out.functions.pop() {
                Some(row) => DropOutcome::Dropped(Some(row.delta_self_pct.abs())),
                None => DropOutcome::Exhausted,
            }
        });
        Ok(Json(result))
    }

    #[tool(
        name = "stacks_containing",
        description = "Distinct stacks that include a frame matching `function`."
    )]
    pub async fn stacks_containing(
        &self,
        Parameters(args): Parameters<StacksContainingArgs>,
    ) -> Result<Json<stacks_containing::Output>, ErrorData> {
        let session = get_session(self, &args.profile_id).await?;
        let q_args = stacks_containing::Args {
            filter_args: parse_filter(&args.common)?.compose_under_scope(session.view_scope())?,
            function: args.function.clone(),
            limit: args.limit.unwrap_or(0),
        };
        let mut result = stacks_containing::stacks_containing(session.profile(), &q_args)?;
        result.truncated = fit_to_budget(&mut result, output_budget_bytes(), |out| {
            match out.stacks.pop() {
                Some(stack) => {
                    out.stacks_returned = out.stacks.len();
                    DropOutcome::Dropped(Some(stack.pct))
                }
                None => DropOutcome::Exhausted,
            }
        });
        Ok(Json(result))
    }
}

/// Trim a [`folded::Folded`] result to fit `budget` bytes by dropping
/// the lowest-sample lines first. Specialized rather than going
/// through [`fit_to_budget`] because the rendered output is a single
/// string — re-rendering on every drop would be O(n²) on big
/// profiles. Computes per-line byte cost up front and walks the
/// pre-sorted entry list once.
fn fit_folded_to_budget(
    mut folded: folded::Folded,
    budget: usize,
) -> (String, Option<crate::tools::budget::Truncated>) {
    if budget == 0 {
        return (folded.render(), None);
    }
    // Approximate envelope cost of `{"folded":""}` plus any future
    // sibling fields. Slight overestimate is fine — the per-line cost
    // dominates.
    const ENVELOPE_OVERHEAD: usize = 64;
    let folded_budget = budget.saturating_sub(ENVELOPE_OVERHEAD);

    folded.entries.sort_by(|a, b| {
        b.samples
            .cmp(&a.samples)
            .then_with(|| a.stack.cmp(&b.stack))
    });
    let total = folded.total_samples.max(1) as f32;

    let mut keep = 0usize;
    let mut size = 0usize;
    for e in &folded.entries {
        let line = line_bytes(&e.stack, e.samples);
        // The string produced by `Folded::render` contains every kept
        // line — we don't escape, so the on-wire JSON-encoded `folded`
        // string is `line.len() + escapes`. Lines are demangled
        // function names which rarely contain `"` or `\`, so the
        // unescaped count is a tight estimate.
        if size + line > folded_budget && keep > 0 {
            break;
        }
        size += line;
        keep += 1;
    }

    if keep == folded.entries.len() {
        return (folded.render(), None);
    }

    let dropped = folded.entries.len() - keep;
    let dropped_pct: f32 = folded
        .entries
        .iter()
        .skip(keep)
        .map(|e| 100.0 * e.samples as f32 / total)
        .sum();
    folded.entries.truncate(keep);
    let rendered = folded.render();
    let final_bytes = rendered.len() + ENVELOPE_OVERHEAD;
    (
        rendered,
        Some(crate::tools::budget::Truncated {
            dropped,
            dropped_pct: Some(dropped_pct),
            budget_bytes: budget,
            final_bytes,
            still_over_budget: final_bytes > budget,
        }),
    )
}

fn line_bytes(stack: &str, samples: u64) -> usize {
    let digits = if samples == 0 {
        1
    } else {
        let mut d = 0usize;
        let mut n = samples;
        while n > 0 {
            d += 1;
            n /= 10;
        }
        d
    };
    stack.len() + 1 /* space */ + digits + 1 /* newline */
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_folded_drops_lowest_sample_lines_first() {
        // Three lines, total ~ 36 bytes of payload. Budget tight enough
        // to force at least one drop; the smallest-sample line goes.
        let folded = folded::Folded {
            entries: vec![
                folded::FoldedEntry {
                    stack: "aaaaaaaa".into(),
                    samples: 100,
                },
                folded::FoldedEntry {
                    stack: "bbbbbbbb".into(),
                    samples: 50,
                },
                folded::FoldedEntry {
                    stack: "cccccccc".into(),
                    samples: 1,
                },
            ],
            total_samples: 151,
        };
        // Envelope is ~64 bytes, line cost ~13 bytes each. Budget for
        // two lines ≈ 64 + 2*13 = 90.
        let (rendered, truncated) = fit_folded_to_budget(folded, 90);
        let truncated = truncated.expect("expected to truncate");
        assert_eq!(truncated.dropped, 1);
        assert!(!rendered.contains("cccccccc"));
        assert!(rendered.contains("aaaaaaaa"));
        assert!(rendered.contains("bbbbbbbb"));
    }

    #[test]
    fn fit_folded_returns_unchanged_under_budget() {
        let folded = folded::Folded {
            entries: vec![folded::FoldedEntry {
                stack: "x".into(),
                samples: 1,
            }],
            total_samples: 1,
        };
        let (rendered, truncated) = fit_folded_to_budget(folded, 10_000);
        assert_eq!(rendered, "x 1\n");
        assert!(truncated.is_none());
    }

    #[test]
    fn fit_folded_keeps_at_least_one_line_when_budget_tiny() {
        let folded = folded::Folded {
            entries: vec![
                folded::FoldedEntry {
                    stack: "looooong_stack_name".into(),
                    samples: 5,
                },
                folded::FoldedEntry {
                    stack: "another_one".into(),
                    samples: 3,
                },
            ],
            total_samples: 8,
        };
        // Budget so small no line fits — the trimmer keeps the largest
        // line anyway so the response isn't an empty payload.
        let (rendered, truncated) = fit_folded_to_budget(folded, 10);
        let truncated = truncated.expect("expected truncation");
        assert!(truncated.still_over_budget);
        assert!(rendered.contains("looooong_stack_name"));
        assert!(!rendered.contains("another_one"));
    }
}
