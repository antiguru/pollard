//! Query MCP tools: top_functions, call_tree, stacks_containing.

use crate::error::ToolError;
use crate::query::filters::{Filter, ProcessFilter, ThreadFilter};
use crate::query::{call_tree, folded, stacks_containing, top_functions};
use crate::tools::PollardServer;
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
    /// Process filter: "pid:NNN" for a numeric pid, or a process name string.
    #[serde(default)]
    pub process: Option<String>,
    /// Optional time range [start_ms, end_ms].
    #[serde(default)]
    pub time_range: Option<[f64; 2]>,
}

fn parse_filter(args: &CommonFilterArgs) -> Filter {
    let thread = args.thread.as_deref().map(|t| {
        if let Some(rest) = t.strip_prefix("tid:")
            && let Ok(n) = rest.parse::<u64>()
        {
            return ThreadFilter::Tid(n);
        }
        ThreadFilter::Name(t.to_owned())
    });
    let process = args.process.as_deref().map(|p| {
        if let Some(rest) = p.strip_prefix("pid:")
            && let Ok(n) = rest.parse::<u64>()
        {
            return ProcessFilter::Pid(n);
        }
        ProcessFilter::Name(p.to_owned())
    });
    Filter {
        thread,
        process,
        time_range: args.time_range,
    }
}

// ---------------------------------------------------------------------------
// top_functions args
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub struct TopFunctionsArgs {
    /// Profile ID returned by load_profile.
    pub profile_id: String,
    /// Optional filter on the **demangled function name**. Literal
    /// case-sensitive substring by default — e.g. `"simd_cols_3rd"` matches,
    /// `"simd"` matches `"simd_cols_3rd"` but NOT `"SIMD"` or `"simdcols"`.
    /// Prefix with `re:` for a regex (`"re:simd.*cols"`). Not a topic /
    /// fuzzy / token search; for "anything related to X" use multiple narrow
    /// substring queries or a regex with alternation.
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
    #[serde(default)]
    pub root_function: Option<String>,
    /// Only include stacks that pass through this function.
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
}

// ---------------------------------------------------------------------------
// stacks_containing args
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub struct StacksContainingArgs {
    /// Profile ID returned by load_profile.
    pub profile_id: String,
    /// Function name / pattern to search for in each stack.
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
        description = "Top-N functions by self or total samples."
    )]
    pub async fn top_functions(
        &self,
        Parameters(args): Parameters<TopFunctionsArgs>,
    ) -> Result<Json<top_functions::Output>, ErrorData> {
        let session = get_session(self, &args.profile_id).await?;
        let q_args = top_functions::Args {
            filter: args.filter.clone(),
            limit: args.limit.unwrap_or(0),
            sort_by: match args.sort_by.as_deref() {
                Some("total") => top_functions::SortBy::TotalTime,
                Some("descendants") => top_functions::SortBy::Descendants,
                _ => top_functions::SortBy::SelfTime,
            },
            filter_args: parse_filter(&args.common),
            expand_inlines: args.expand_inlines.unwrap_or(false),
        };
        let result = top_functions::top_functions(session.profile(), &q_args)?;
        Ok(Json(result))
    }

    #[tool(
        name = "call_tree",
        description = "Hierarchical call tree, pruned for LLM consumption."
    )]
    pub async fn call_tree(
        &self,
        Parameters(args): Parameters<CallTreeArgs>,
    ) -> Result<Json<call_tree::Output>, ErrorData> {
        let session = get_session(self, &args.profile_id).await?;
        let defaults = call_tree::Args::default();
        let q_args = call_tree::Args {
            filter_args: parse_filter(&args.common),
            inverted: args.inverted.unwrap_or(false),
            root_function: args.root_function.clone(),
            paths_to: args.paths_to.clone(),
            min_pct: args.min_pct.unwrap_or(defaults.min_pct),
            min_samples: args.min_samples,
            max_depth: args.max_depth.unwrap_or(defaults.max_depth),
            max_breadth: args.max_breadth.unwrap_or(defaults.max_breadth),
            expand_inlines: args.expand_inlines.unwrap_or(defaults.expand_inlines),
        };
        let result = call_tree::call_tree(session.profile(), &q_args)?;
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
            filter_args: parse_filter(&args.common),
            function_filter: args.function_filter.clone(),
        };
        let folded = folded::folded_stacks(session.profile(), &q_args)?;
        Ok(Json(FoldedStacksOutput { folded }))
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
            filter_args: parse_filter(&args.common),
            function: args.function.clone(),
            limit: args.limit.unwrap_or(0),
        };
        let result = stacks_containing::stacks_containing(session.profile(), &q_args)?;
        Ok(Json(result))
    }
}
