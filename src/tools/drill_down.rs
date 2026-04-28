//! Drill-down MCP tools: source_for_function, asm_for_function.

use crate::error::ToolError;
use crate::query::{asm, source};
use crate::tools::PollardServer;
use rmcp::ErrorData;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

async fn session(
    server: &PollardServer,
    profile_id: &str,
) -> Result<std::sync::Arc<crate::session::ProfileSession>, ToolError> {
    server.registry.get_or_error(profile_id).await
}

#[derive(Deserialize, JsonSchema)]
pub struct SourceForFunctionArgs {
    pub profile_id: String,
    pub function: String,
    #[serde(default)]
    pub module: Option<String>,
    #[serde(default)]
    pub with_samples: Option<bool>,
    #[serde(default)]
    pub whole_file: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct AsmForFunctionArgs {
    pub profile_id: String,
    pub function: String,
    #[serde(default)]
    pub module: Option<String>,
    #[serde(default)]
    pub with_samples: Option<bool>,
}

#[tool_router(router = drill_down_router, vis = "pub(crate)")]
impl PollardServer {
    #[tool(
        name = "source_for_function",
        description = "Source listing with per-line sample counts."
    )]
    pub async fn source_for_function(
        &self,
        Parameters(args): Parameters<SourceForFunctionArgs>,
    ) -> Result<Json<source::SourceListing>, ErrorData> {
        let session = session(self, &args.profile_id).await?;
        let q_args = source::Args {
            function: args.function,
            module: args.module,
            with_samples: args.with_samples.unwrap_or(true),
            whole_file: args.whole_file.unwrap_or(false),
        };
        let result = source::source_for_function(session.profile(), &q_args)?;
        Ok(Json(result))
    }

    #[tool(
        name = "asm_for_function",
        description = "Disassembly with per-instruction sample counts."
    )]
    pub async fn asm_for_function(
        &self,
        Parameters(args): Parameters<AsmForFunctionArgs>,
    ) -> Result<Json<asm::AsmListing>, ErrorData> {
        let session = session(self, &args.profile_id).await?;
        let q_args = asm::Args {
            function: args.function,
            module: args.module,
            with_samples: args.with_samples.unwrap_or(true),
        };
        let result = asm::asm_for_function(session.profile(), &q_args).await?;
        Ok(Json(result))
    }
}
