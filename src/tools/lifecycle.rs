//! Lifecycle MCP tools: load_profile, unload_profile, list_profiles, describe_profile.

use crate::error::ToolError;
use crate::profile::symbolicate::problematic_outcomes;
use crate::query::describe::{DEFAULT_TOP_N, ProfileDescription, describe};
use crate::query::summary;
use crate::tools::PollardServer;
use crate::tools::query::{CommonFilterArgs, parse_filter};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Input / output shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub struct LoadProfileArgs {
    /// Absolute or relative path to a .json or .json.gz Firefox-format profile.
    pub path: PathBuf,
    /// Optional human-readable label. Defaults to the file basename.
    #[serde(default)]
    pub name: Option<String>,
    /// Cap on processes (and threads per process) returned in the
    /// description. 0-sample entries are always dropped first. Omit for
    /// the default; call `describe_profile` with a larger value to widen.
    #[serde(default)]
    pub top_n: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DescribeProfileArgs {
    pub profile_id: String,
    /// Cap on processes (and threads per process). 0-sample entries are
    /// always dropped first. Omit for the default.
    #[serde(default)]
    pub top_n: Option<usize>,
}

#[derive(Serialize, JsonSchema)]
pub struct LoadProfileResult {
    pub profile_id: String,
    pub description: ProfileDescription,
    /// Profiles that were evicted from the in-memory cache to make room for this
    /// load. Each entry retains the original path so the caller can re-load it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evicted: Vec<EvictedRef>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ProfileIdArgs {
    pub profile_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct SummaryArgs {
    pub profile_id: String,
    /// Optional filter context. When set, every sample-count surface
    /// in the response (`total_samples`, `time_range_ms`,
    /// `top_processes`, `top_threads`, `top_modules`, both top-functions
    /// lists, `dominant_thread`) is recomputed against the filter so the
    /// caller can re-run `summary` for a single pid/thread/time slice
    /// without composing `top_functions` + `top_modules` by hand.
    /// Recording-level fields (`interval_ms`, `sample_rate_hz`,
    /// `unsymbolicated_pct`, `profile_start_ms`) stay profile-wide
    /// because they describe the recording, not the slice.
    #[serde(flatten)]
    pub common: CommonFilterArgs,
}

#[derive(Serialize, JsonSchema)]
pub struct UnloadResult {
    pub freed: bool,
}

#[derive(Serialize, JsonSchema)]
pub struct ListResult {
    pub profiles: Vec<LoadedProfile>,
    /// Profiles that were evicted but are still tracked by path so they can be
    /// re-loaded. Empty when nothing has been evicted this session.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evicted: Vec<EvictedRef>,
}

#[derive(Serialize, JsonSchema)]
pub struct LoadedProfile {
    pub profile_id: String,
    pub name: String,
    pub path: String,
    /// When set, this is a derived view of the named profile id.
    /// Absent for profiles loaded from disk.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_profile_id: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct EvictedRef {
    pub profile_id: String,
    pub name: String,
    pub path: String,
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

/// Convert ToolError to rmcp's ErrorData so tool functions can use `?`.
impl From<ToolError> for rmcp::ErrorData {
    fn from(err: ToolError) -> Self {
        let json = serde_json::to_value(&err).unwrap_or(serde_json::Value::Null);
        rmcp::ErrorData::internal_error(err.to_string(), Some(json))
    }
}

#[tool_router(router = lifecycle_router, vis = "pub(crate)")]
impl PollardServer {
    #[tool(
        description = "Load a Firefox-format profile and start symbolicating. Blocks until ready."
    )]
    pub async fn load_profile(
        &self,
        Parameters(args): Parameters<LoadProfileArgs>,
    ) -> Result<Json<LoadProfileResult>, rmcp::ErrorData> {
        let (id, evicted) = self.registry.load(&args.path, args.name.as_deref()).await?;
        let session =
            self.registry.get(&id).await.ok_or_else(|| {
                rmcp::ErrorData::internal_error("profile vanished after load", None)
            })?;
        let mut desc = describe(
            session.profile(),
            session.id(),
            session.name(),
            &session.path().display().to_string(),
            session.unsymbolicated_pct(),
            args.top_n.unwrap_or(DEFAULT_TOP_N),
        );
        desc.lib_diagnostics = problematic_outcomes(session.lib_outcomes());
        let evicted = evicted
            .into_iter()
            .map(|e| EvictedRef {
                profile_id: e.profile_id,
                name: e.name,
                path: e.path.display().to_string(),
            })
            .collect();
        Ok(Json(LoadProfileResult {
            profile_id: id,
            description: desc,
            evicted,
        }))
    }

    #[tool(description = "Free the memory held by a loaded profile.")]
    pub async fn unload_profile(
        &self,
        Parameters(args): Parameters<ProfileIdArgs>,
    ) -> Result<Json<UnloadResult>, rmcp::ErrorData> {
        Ok(Json(UnloadResult {
            freed: self.registry.unload(&args.profile_id).await,
        }))
    }

    #[tool(
        description = "List currently loaded profiles, plus any that have been evicted but can still be re-loaded by path."
    )]
    pub async fn list_profiles(&self) -> Result<Json<ListResult>, rmcp::ErrorData> {
        let profiles = self
            .registry
            .list()
            .await
            .iter()
            .map(|s| LoadedProfile {
                profile_id: s.id().to_owned(),
                name: s.name().to_owned(),
                path: s.path().display().to_string(),
                base_profile_id: s.base_id().map(String::from),
            })
            .collect();
        let evicted = self
            .registry
            .list_evicted()
            .await
            .into_iter()
            .map(|e| EvictedRef {
                profile_id: e.profile_id,
                name: e.name,
                path: e.path.display().to_string(),
            })
            .collect();
        Ok(Json(ListResult { profiles, evicted }))
    }

    #[tool(
        description = "Describe a loaded profile: top processes and threads by sample count, with totals and omitted-entry counts. Use `top_n` to widen the per-call window."
    )]
    pub async fn describe_profile(
        &self,
        Parameters(args): Parameters<DescribeProfileArgs>,
    ) -> Result<Json<ProfileDescription>, rmcp::ErrorData> {
        let session = self.registry.get_or_error(&args.profile_id).await?;
        let mut desc = describe(
            session.profile(),
            session.id(),
            session.name(),
            &session.path().display().to_string(),
            session.unsymbolicated_pct(),
            args.top_n.unwrap_or(DEFAULT_TOP_N),
        );
        desc.lib_diagnostics = problematic_outcomes(session.lib_outcomes());
        Ok(Json(desc))
    }

    #[tool(
        name = "summary",
        description = "One-shot orientation: shape (duration, sample rate, time range, unsymbolicated bracket), dominant thread, top 5 modules, top 10 functions by self time, and top 10 by total time. Pass the standard process / thread / time_range filter args to re-scope every sample count to that slice — the response shape doesn't change. Use this first instead of chaining describe_profile + top_functions."
    )]
    pub async fn summary(
        &self,
        Parameters(args): Parameters<SummaryArgs>,
    ) -> Result<Json<summary::Output>, rmcp::ErrorData> {
        let session = self.registry.get_or_error(&args.profile_id).await?;
        let filter = parse_filter(&args.common)?;
        let mut result = summary::summary(
            session.profile(),
            session.id(),
            session.name(),
            &session.path().display().to_string(),
            session.unsymbolicated_pct(),
            filter,
        )?;
        result.lib_diagnostics = problematic_outcomes(session.lib_outcomes());
        Ok(Json(result))
    }
}
