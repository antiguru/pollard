//! Lifecycle MCP tools: load_profile, unload_profile, list_profiles, describe_profile.

use crate::error::ToolError;
use crate::query::describe::{ProfileDescription, describe};
use crate::tools::PollardServer;
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
    #[tool(description = "Load a Firefox-format profile and start symbolicating. Blocks until ready.")]
    pub async fn load_profile(
        &self,
        Parameters(args): Parameters<LoadProfileArgs>,
    ) -> Result<Json<LoadProfileResult>, rmcp::ErrorData> {
        let (id, evicted) = self.registry.load(&args.path, args.name.as_deref()).await?;
        let session = self.registry.get(&id).await.ok_or_else(|| {
            rmcp::ErrorData::internal_error("profile vanished after load", None)
        })?;
        let desc = describe(
            session.profile(),
            session.id(),
            session.name(),
            &session.path().display().to_string(),
            session.unsymbolicated_pct(),
        );
        let evicted = evicted
            .into_iter()
            .map(|e| EvictedRef {
                profile_id: e.profile_id,
                name: e.name,
                path: e.path.display().to_string(),
            })
            .collect();
        Ok(Json(LoadProfileResult { profile_id: id, description: desc, evicted }))
    }

    #[tool(description = "Free the memory held by a loaded profile.")]
    pub async fn unload_profile(
        &self,
        Parameters(args): Parameters<ProfileIdArgs>,
    ) -> Result<Json<UnloadResult>, rmcp::ErrorData> {
        Ok(Json(UnloadResult { freed: self.registry.unload(&args.profile_id).await }))
    }

    #[tool(description = "List currently loaded profiles, plus any that have been evicted but can still be re-loaded by path.")]
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

    #[tool(description = "Describe a loaded profile: processes, threads, sample counts.")]
    pub async fn describe_profile(
        &self,
        Parameters(args): Parameters<ProfileIdArgs>,
    ) -> Result<Json<ProfileDescription>, rmcp::ErrorData> {
        let session = self.registry.get_or_error(&args.profile_id).await?;
        Ok(Json(describe(
            session.profile(),
            session.id(),
            session.name(),
            &session.path().display().to_string(),
            session.unsymbolicated_pct(),
        )))
    }
}
